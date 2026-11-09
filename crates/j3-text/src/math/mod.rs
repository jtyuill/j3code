//! TeX math in chat markdown. `$…$` and `$$…$$` (and the `\(…\)` and
//! `\[…\]` models write, rewritten to them) are typeset with RaTeX as SVGs.
//!
//! [`prepare`] rewrites a message's markdown so each display formula that is
//! a paragraph by itself becomes an image, `![<tex>](j3-math:<index>)`, for a
//! markdown view that draws images as blocks of their own. [`inline`] finds
//! formulas inside a line of text and cuts a message into the top-level
//! paragraphs that hold them; elsewhere a formula reads as its Unicode
//! approximation ([`unicode`]). [`Formula::svg`] typesets one.
//!
//! Formulas are validated as they are found, so TeX RaTeX rejects (and prose
//! such as `$5 and $10`) stays literal text.

mod delimiters;
pub mod inline;
pub mod unicode;

use ratex_layout::{LayoutBox, LayoutOptions, layout, to_display_list, validate_layout};
use ratex_svg::{SvgColorSyntax, SvgOptions, render_to_svg_with_color_syntax};
use ratex_types::{color::Color, math_style::MathStyle};

use delimiters::{
    CodeBlocks, after_quote_markers, columns, is_list_item, list_marker_len, normalize_delimiters,
};

/// Math is set this much larger than the surrounding text, as KaTeX does:
/// TeX's glyphs have a small x-height next to interface fonts.
pub const SCALE: f32 = 1.21;

/// SVG user units per em. The SVG is a vector rasterized at the size it is
/// drawn, so this only sets the coordinate scale; RaTeX's default keeps its
/// stroke widths in proportion.
const SVG_EM: f64 = 40.;

/// The scheme of the image sources [`prepare`] writes for formulas.
const SCHEME: &str = "j3-math:";

/// A message's markdown ready for a markdown view, from [`prepare`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prepared {
    /// The markdown, with each display formula that is a paragraph by itself
    /// replaced by an image whose source is [`image_src`] of its index in
    /// `formulas`.
    pub source: String,
    /// The display formulas the images in `source` name.
    pub formulas: Vec<Formula>,
}

/// A formula RaTeX accepted.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Formula {
    /// The TeX, without its delimiters.
    pub tex: String,
    /// Set in display style.
    pub display: bool,
}

impl Formula {
    /// `tex` when RaTeX parses and lays it out within its resource limits.
    pub fn new(tex: &str, display: bool) -> Option<Self> {
        if tex.trim().is_empty() {
            return None;
        }
        layout_box(tex, display, Color::BLACK)?;
        Some(Self {
            tex: tex.to_owned(),
            display,
        })
    }

    /// The formula typeset in `color` (sRGB red, green, blue and alpha, 0 to
    /// 1); `None` when RaTeX cannot lay it out.
    pub fn svg(&self, color: [f32; 4]) -> Option<Svg> {
        let [r, g, b, a] = color;
        let list = to_display_list(&layout_box(
            &self.tex,
            self.display,
            Color::new(r, g, b, a),
        )?);
        let options = SvgOptions {
            font_size: SVG_EM,
            padding: 0.,
            embed_glyphs: true,
            ..SvgOptions::default()
        };
        Some(Svg {
            svg: render_to_svg_with_color_syntax(&list, &options, SvgColorSyntax::Rgb),
            width: list.width as f32,
            height: list.height as f32,
            depth: list.depth as f32,
        })
    }
}

/// A typeset formula: its SVG, glyphs embedded, and its box in em.
#[derive(Clone, Debug, PartialEq)]
pub struct Svg {
    pub svg: String,
    pub width: f32,
    /// Above the baseline.
    pub height: f32,
    /// Below the baseline.
    pub depth: f32,
}

/// Lays `tex` out, or `None` when it does not parse or is too costly to draw.
fn layout_box(tex: &str, display: bool, color: Color) -> Option<LayoutBox> {
    let ast = ratex_parser::parse(tex).ok()?;
    let style = if display {
        MathStyle::Display
    } else {
        MathStyle::Text
    };
    let options = LayoutOptions::default().with_style(style).with_color(color);
    let lbox = layout(&ast, &options);
    validate_layout(&lbox).ok()?;
    Some(lbox)
}

/// `svg`, a formula `width` by `height` em from [`Formula::svg`], with `top`
/// and `bottom` em of room added.
pub fn padded(svg: &[u8], width: f32, height: f32, top: f32, bottom: f32) -> Vec<u8> {
    let body = svg
        .iter()
        .position(|&b| b == b'>')
        .map_or(svg, |end| &svg[end + 1..]);
    let unit = SVG_EM as f32;
    let (w, h) = (width * unit, (height + top + bottom) * unit);
    let mut out = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 {} {w} {h}" width="{w}pt" height="{h}pt">"#,
        -top * unit,
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

/// The formula in an inline math node: `source` is the node as authored
/// (`$…$` or `$$…$$`), `after` the text following it.
///
/// A single-dollar formula follows Pandoc's rule: no space just inside
/// either `$`, and no digit right after the closing one, so prices stay
/// prose. A `$$` formula inside a line is set in display style.
fn inline_formula(source: &str, tex: &str, after: &str) -> Option<Formula> {
    let display = source.starts_with("$$");
    if !display {
        let inner = source.strip_prefix('$')?.strip_suffix('$')?;
        if inner.starts_with(char::is_whitespace)
            || inner.ends_with(char::is_whitespace)
            || after.starts_with(|c: char| c.is_ascii_digit())
        {
            return None;
        }
    }
    Formula::new(tex, display)
}

/// `markdown` for a markdown view: the `\(…\)` and `\[…\]` delimiters
/// rewritten, and each display formula that is a paragraph by itself (a `$$`
/// block, or a paragraph that is one `$$…$$`) replaced by an image naming it.
/// The image's alt text is the TeX, on one line. Formulas in a line of text,
/// code, and TeX RaTeX rejects are left as they are.
pub fn prepare(markdown: &str) -> Prepared {
    let text = normalize_delimiters(markdown);
    let mut prepared = Prepared {
        source: String::with_capacity(text.len()),
        formulas: Vec::new(),
    };
    let mut blocks = CodeBlocks::new();
    let mut open: Option<Open> = None;
    let mut pos = 0;
    for line in text.split_inclusive('\n') {
        let verbatim = blocks.is_verbatim(line);
        let end = pos + line.len();
        if let Some(Open::Math(start, fence)) = open {
            if closes_math(line, fence) {
                prepared.push_paragraph(&text[start..end]);
                open = None;
            }
            pos = end;
            continue;
        }
        let kind = (!verbatim).then(|| Line::new(line));
        if let Some(Open::Paragraph(start, first, last)) = open
            && kind.is_none_or(|kind| kind.math_fence.is_some() || !kind.continues(first, last))
        {
            prepared.push_paragraph(&text[start..pos]);
            open = None;
        }
        open = match kind {
            Some(Line {
                math_fence: Some(fence),
                ..
            }) => Some(Open::Math(pos, fence)),
            Some(kind) => Some(match open {
                Some(Open::Paragraph(start, first, _)) => Open::Paragraph(start, first, kind),
                _ => Open::Paragraph(pos, kind, kind),
            }),
            None => {
                prepared.source.push_str(line);
                None
            }
        };
        pos = end;
    }
    match open {
        Some(Open::Paragraph(start, ..)) => prepared.push_paragraph(&text[start..]),
        // A `$$` block not closed yet, as while it streams in, stays text.
        Some(Open::Math(start, _)) => prepared.source.push_str(&text[start..]),
        None => {}
    }
    prepared
}

/// The block [`prepare`] is reading.
#[derive(Clone, Copy)]
enum Open {
    /// A paragraph: where it starts, its first line and its last.
    Paragraph(usize, Line, Line),
    /// A `$$` block: where it starts, and its opening fence's length.
    Math(usize, usize),
}

/// Whether `line` closes a `$$` block whose opening fence is `fence` long.
fn closes_math(line: &str, fence: usize) -> bool {
    let body = after_quote_markers(line).trim_start_matches([' ', '\t']);
    let run = body.bytes().take_while(|&b| b == b'$').count();
    run >= fence && body[run..].trim().is_empty()
}

impl Prepared {
    /// Appends `chunk`, a paragraph's lines, as an image when it is a display
    /// formula and as is otherwise.
    fn push_paragraph(&mut self, chunk: &str) {
        let Some((prefix, formula)) = display_paragraph(chunk) else {
            self.source.push_str(chunk);
            return;
        };
        let ending = &chunk[chunk.trim_end_matches(['\r', '\n']).len()..];
        self.source.push_str(prefix);
        self.source.push_str("![");
        push_alt(&mut self.source, &formula.tex);
        self.source.push_str("](");
        self.source.push_str(&image_src(self.formulas.len()));
        self.source.push(')');
        self.source.push_str(ending);
        self.formulas.push(formula);
    }
}

/// A prose line's place among paragraphs.
#[derive(Clone, Copy)]
struct Line {
    /// Its blockquote depth.
    depth: usize,
    /// It starts a block: a list item or a heading.
    starts_block: bool,
    /// It is a heading, which no line continues.
    heading: bool,
    /// It opens a `$$` block whose fence is this long: two or more `$`, no
    /// `$` after them, indented less than four columns.
    math_fence: Option<usize>,
}

impl Line {
    fn new(line: &str) -> Self {
        let rest = after_quote_markers(line);
        let depth = line[..line.len() - rest.len()].matches('>').count();
        let body = rest.trim_start_matches([' ', '\t']);
        let heading = is_heading(body);
        let run = body.bytes().take_while(|&b| b == b'$').count();
        let fence =
            columns(&rest[..rest.len() - body.len()]) < 4 && run >= 2 && !body[run..].contains('$');
        Self {
            depth,
            starts_block: heading || is_list_item(body),
            heading,
            math_fence: fence.then_some(run),
        }
    }

    /// Whether this line goes on with the paragraph that starts with `first`
    /// and so far ends with `last`.
    fn continues(self, first: Line, last: Line) -> bool {
        self.depth == first.depth && !self.starts_block && !last.heading
    }
}

/// Whether `body` is an ATX heading: one to six `#` and a space or nothing.
fn is_heading(body: &str) -> bool {
    let hashes = body.bytes().take_while(|&b| b == b'#').count();
    let rest = &body[hashes..];
    (1..=6).contains(&hashes) && (rest.is_empty() || rest.starts_with([' ', '\t', '\n', '\r']))
}

/// The display formula `chunk`, a paragraph's lines, is when it is one
/// `$$…$$` and nothing else, with the first line's quote markers, indentation
/// and list marker to keep before the image.
fn display_paragraph(chunk: &str) -> Option<(&str, Formula)> {
    let mut content = String::with_capacity(chunk.len());
    let mut prefix = "";
    for (n, line) in chunk.lines().enumerate() {
        let mut body = after_quote_markers(line).trim_start_matches([' ', '\t']);
        if n == 0 {
            if let Some(marker) = list_marker_len(body) {
                body = body[marker..].trim_start_matches([' ', '\t']);
            }
            prefix = &line[..line.len() - body.len()];
        } else {
            content.push('\n');
        }
        content.push_str(body.trim_end());
    }
    let tex = content.strip_prefix("$$")?.strip_suffix("$$")?;
    if tex.contains("$$") {
        return None;
    }
    let formula = inline_formula(&content, tex.trim(), "")?;
    Some((prefix, formula))
}

/// `tex` as an image's alt text: on one line, its punctuation escaped so
/// markdown reads it back as written.
fn push_alt(out: &mut String, tex: &str) {
    for (n, word) in tex.split_whitespace().enumerate() {
        if n > 0 {
            out.push(' ');
        }
        for c in word.chars() {
            if c.is_ascii_punctuation() {
                out.push('\\');
            }
            out.push(c);
        }
    }
}

/// The image source [`prepare`] writes for formula `index`.
pub fn image_src(index: usize) -> String {
    format!("{SCHEME}{index}")
}

/// The formula index an image source [`image_src`] wrote names.
pub fn parse_src(src: &str) -> Option<usize> {
    src.strip_prefix(SCHEME)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(tex: &str) -> Formula {
        Formula {
            tex: tex.to_owned(),
            display: true,
        }
    }

    #[test]
    fn single_dollars_follow_the_dollar_rule() {
        // `$5 and $10`: the parser's formula is `$5 and $`, then `10`.
        assert!(inline_formula("$5 and $", "5 and ", "10").is_none());
        assert!(inline_formula("$ x$", " x", "").is_none());
        assert!(inline_formula("$x$", "x", "5").is_none());
        let formula = inline_formula("$x$", "x", " more").unwrap();
        assert!(!formula.display);
        assert!(inline_formula("$$x$$", "x", "").unwrap().display);
    }

    #[test]
    fn tex_ratex_rejects_stays_text() {
        assert!(Formula::new(r"\notacommand{x}", false).is_none());
        assert!(Formula::new(r"\frac{a", true).is_none());
        assert!(Formula::new("  ", false).is_none());
        assert!(Formula::new(r"\frac{a}{b}", true).is_some());
    }

    #[test]
    fn a_display_paragraph_becomes_an_image() {
        let prepared = prepare("Sum:\n\n$$\\frac{a}{b}$$\n\nafter");
        assert_eq!(
            prepared.source,
            "Sum:\n\n![\\\\frac\\{a\\}\\{b\\}](j3-math:0)\n\nafter"
        );
        assert_eq!(prepared.formulas, [display(r"\frac{a}{b}")]);
    }

    #[test]
    fn a_display_block_becomes_an_image() {
        let prepared = prepare("Sum:\n\\[ \\sum_k k \\]\nafter");
        assert_eq!(prepared.source, "Sum:\n![\\\\sum\\_k k](j3-math:0)\nafter");
        assert_eq!(prepared.formulas, [display(r"\sum_k k")]);

        let prepared = prepare("$$\nx +\n  y\n$$\n\n$$z$$\n");
        assert_eq!(
            prepared.source,
            "![x \\+ y](j3-math:0)\n\n![z](j3-math:1)\n"
        );
        assert_eq!(prepared.formulas, [display("x +\ny"), display("z")]);
    }

    #[test]
    fn display_formulas_keep_their_containers() {
        assert_eq!(prepare("> \\[ x \\]").source, "> ![x](j3-math:0)");
        assert_eq!(
            prepare("- $$x$$\n- $$y$$").source,
            "- ![x](j3-math:0)\n- ![y](j3-math:1)"
        );
        assert_eq!(prepare("# Title\n$$x$$").source, "# Title\n![x](j3-math:0)");
    }

    #[test]
    fn inline_formulas_stay_text() {
        let prepared = prepare("so $x^2$ and $$y$$ here");
        assert_eq!(prepared.source, "so $x^2$ and $$y$$ here");
        assert!(prepared.formulas.is_empty());
        assert_eq!(prepare(r"so \(x^2\).").source, "so $x^2$.");
        // Text after the formula keeps the paragraph prose.
        assert_eq!(prepare("$$x$$\nmore").source, "$$x$$\nmore");
    }

    #[test]
    fn code_and_rejected_tex_stay_as_they_are() {
        let fenced = "```\n$$x$$\n```\n";
        assert_eq!(prepare(fenced).source, fenced);
        let indented = "text\n\n    $$x$$\n";
        assert_eq!(prepare(indented).source, indented);
        let rejected = "$$\\frac{a$$";
        assert_eq!(prepare(rejected).source, rejected);
        assert!(prepare(rejected).formulas.is_empty());
    }

    #[test]
    fn image_sources_round_trip() {
        assert_eq!(parse_src(&image_src(3)), Some(3));
        assert_eq!(parse_src("j3-math:x"), None);
        assert_eq!(parse_src("https://example.com/a.png"), None);
    }

    #[test]
    fn a_formula_typesets_to_an_svg_in_its_color() {
        let svg = display("x + y + z").svg([1., 0., 0., 1.]).unwrap();
        assert!(svg.svg.starts_with("<svg"));
        assert!(svg.svg.contains(r#"fill="rgb(255,0,0)""#));
        assert!(svg.width > svg.height + svg.depth);
        assert!(display(r"\frac{a").svg([0., 0., 0., 1.]).is_none());
    }

    #[test]
    fn padding_moves_the_formula_down_in_its_box() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 40 20" width="40pt" height="20pt"><path d="M0 0"/></svg>"#;
        let out = String::from_utf8(padded(svg, 1.0, 0.5, 0.25, 0.0)).unwrap();
        assert_eq!(
            out,
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 -10 40 30" width="40pt" height="30pt"><path d="M0 0"/></svg>"#
        );
    }
}
