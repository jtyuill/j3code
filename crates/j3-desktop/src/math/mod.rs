//! TeX math in chat markdown. `$…$` and `$$…$$` (and the `\(…\)` and
//! `\[…\]` models write, rewritten to them) are typeset with RaTeX and drawn
//! as SVGs rasterized at their exact size.
//!
//! Rune's markdown has no math, but it draws an image that stands as a block
//! of its own. [`prepare`] rewrites a message's markdown so each display
//! formula that is a paragraph by itself becomes such an image,
//! `![<tex>](j3-math:<index>)`, and [`Typesetter`] draws the formula an image
//! names. A markdown view uses them as
//!
//! ```ignore
//! let prepared = math::prepare(&text);
//! let formulas = prepared.formulas;
//! let typesetter = typesetter.clone(); // Rc<RefCell<math::Typesetter>>, kept by the view
//! Markdown::new(prepared.source).images(move |src| {
//!     let formula = formulas.get(math::parse_src(src)?)?;
//!     typesetter.borrow_mut().image(&images, formula, ink, font_size, 1.0)
//! })
//! ```
//!
//! Markdown draws an image a point per pixel, up to the column's width, so a
//! formula drawn there rasterizes at scale 1 to keep its size. A formula in a
//! line of text stays its `$…$` source: markdown has no inline images.
//!
//! Formulas are validated as they are found, so TeX RaTeX rejects (and prose
//! such as `$5 and $10`) stays literal text, and typeset through a bounded
//! cache: a formula is laid out once per color, never per frame.

mod delimiters;

use std::{
    collections::{HashMap, VecDeque},
    hash::{DefaultHasher, Hash, Hasher},
    sync::Arc,
};

use ratex_layout::{LayoutBox, LayoutOptions, layout, to_display_list, validate_layout};
use ratex_svg::{SvgColorSyntax, SvgOptions, render_to_svg_with_color_syntax};
use ratex_types::{color::Color, math_style::MathStyle};
use rune::{Image, ImageCache, color::srgb_encode};

use delimiters::{
    CodeBlocks, after_quote_markers, columns, is_list_item, list_marker_len, normalize_delimiters,
};

/// Math is set larger than the surrounding text, as KaTeX does: TeX's glyphs
/// have a small x-height next to interface fonts.
const MATH_SCALE: f32 = 1.21;

/// SVG user units per em. The SVG is a vector rasterized at the size it is
/// drawn, so this only sets the coordinate scale; RaTeX's default keeps its
/// stroke widths in proportion.
const SVG_EM: f64 = 40.;

/// Typeset formulas kept before the oldest are dropped.
const CAPACITY: usize = 1024;

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
    fn new(tex: &str, display: bool) -> Option<Self> {
        if tex.trim().is_empty() {
            return None;
        }
        layout_box(tex, display, Color::BLACK)?;
        Some(Self {
            tex: tex.to_owned(),
            display,
        })
    }

    /// Hash of `tex` and `display`: the formula's half of the cache key.
    fn key(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }
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
        let fence = columns(&rest[..rest.len() - body.len()]) < 4
            && run >= 2
            && !body[run..].contains('$');
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

/// A typeset formula: its SVG and its box, in em.
struct Typeset {
    svg: Arc<[u8]>,
    width: f32,
    height: f32,
    depth: f32,
    /// The last raster drawn: its size in points, its scale, and the handle
    /// that keeps it decoded while the formula is drawn.
    raster: Option<(f32, f32, Image)>,
}

impl Typeset {
    fn new(formula: &Formula, color: Color) -> Option<Self> {
        let list = to_display_list(&layout_box(&formula.tex, formula.display, color)?);
        let options = SvgOptions {
            font_size: SVG_EM,
            padding: 0.,
            embed_glyphs: true,
            ..SvgOptions::default()
        };
        let svg = render_to_svg_with_color_syntax(&list, &options, SvgColorSyntax::Rgb);
        Some(Self {
            svg: svg.into_bytes().into(),
            width: list.width as f32,
            height: list.height as f32,
            depth: list.depth as f32,
            raster: None,
        })
    }

    /// The formula rasterized with `font` points per em at `scale` pixels per
    /// point: fitting its larger side, so it rasterizes at its own size.
    fn image(&mut self, cache: &ImageCache, font: f32, scale: f32) -> Image {
        let size = font * self.width.max(self.height + self.depth);
        if let Some((drawn, drawn_scale, image)) = &self.raster
            && *drawn == size
            && *drawn_scale == scale
        {
            return image.clone();
        }
        let image = cache.svg(self.svg.clone(), size, scale);
        self.raster = Some((size, scale, image.clone()));
        image
    }
}

/// Formula key and packed RGBA color.
type Key = (u64, u32);

/// Typeset formulas by formula and color, with each entry's formula to tell
/// hash collisions apart. A failed typeset is kept too, so it is not retried
/// every frame.
#[derive(Default)]
pub struct Typesetter {
    entries: HashMap<Key, (Formula, Option<Typeset>)>,
    order: VecDeque<Key>,
}

impl Typesetter {
    /// `formula` drawn in `color` for text of `font_size` points, rasterized
    /// through `cache` at `scale` pixels per point; `None` when RaTeX cannot
    /// typeset it. The formula is laid out once per color and its raster
    /// kept while it is drawn at the same size.
    pub fn image(
        &mut self,
        cache: &ImageCache,
        formula: &Formula,
        color: rune::Color,
        font_size: f32,
        scale: f32,
    ) -> Option<Image> {
        let color = ratex_color(color);
        let key = (formula.key(), pack(color));
        let cached = self
            .entries
            .get(&key)
            .is_some_and(|(cached, _)| cached == formula);
        if !cached {
            let typeset = Typeset::new(formula, color);
            if self
                .entries
                .insert(key, (formula.clone(), typeset))
                .is_none()
            {
                self.order.push_back(key);
            }
            while self.order.len() > CAPACITY {
                if let Some(old) = self.order.pop_front() {
                    self.entries.remove(&old);
                }
            }
        }
        let typeset = self.entries.get_mut(&key)?.1.as_mut()?;
        Some(typeset.image(cache, font_size * MATH_SCALE, scale))
    }
}

/// `color`, linear, as RaTeX's sRGB.
fn ratex_color(color: rune::Color) -> Color {
    Color::new(
        srgb_encode(color.r),
        srgb_encode(color.g),
        srgb_encode(color.b),
        color.a,
    )
}

/// `color` as 8-bit RGBA.
fn pack(color: Color) -> u32 {
    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
    u32::from_be_bytes([
        channel(color.r),
        channel(color.g),
        channel(color.b),
        channel(color.a),
    ])
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
        assert_eq!(
            prepared.source,
            "Sum:\n![\\\\sum\\_k k](j3-math:0)\nafter"
        );
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
    fn typesetting_rasterizes_at_the_formula_size() {
        let cache = ImageCache::new(Arc::new(|| {}));
        let mut typesetter = Typesetter::default();
        let formula = display("x + y + z");
        let image = typesetter
            .image(&cache, &formula, rune::Color::BLACK, 16., 1.)
            .unwrap();
        let [width, height] = image.wait().unwrap();
        assert!(width > height);
        let again = typesetter
            .image(&cache, &formula, rune::Color::BLACK, 16., 1.)
            .unwrap();
        assert_eq!(again.id(), image.id());
        let white = typesetter
            .image(&cache, &formula, rune::Color::WHITE, 16., 1.)
            .unwrap();
        assert_ne!(white.id(), image.id());
        assert!(
            typesetter
                .image(&cache, &display(r"\frac{a"), rune::Color::BLACK, 16., 1.)
                .is_none()
        );
    }
}
