//! TeX math in chat markdown. `$…$` and `$$…$$` (and the `\(…\)` and
//! `\[…\]` models write, rewritten by [`normalize_delimiters`]) are typeset
//! with RaTeX and drawn as SVGs rasterized at their exact size.
//!
//! The markdown parser hands math to two plugins: [`InlineMath`] for
//! formulas in a line of text, [`DisplayMath`] for `$$` blocks. They validate
//! at parse time, so TeX RaTeX rejects (and prose such as `$5 and $10`) stays
//! literal text, and typeset at render time through a bounded cache: a
//! formula is laid out once per color, never per frame.

use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
    hash::{DefaultHasher, Hash, Hasher},
    sync::{Arc, LazyLock},
};

use gpui_kit::component::text::{
    InlineElement, InlineRenderContext, MarkdownExtensions, MarkdownNode, MarkdownParseContext,
    MarkdownPlugin, TextView, markdown_ast::Node,
};
use gpui_kit::{
    App, ElementId, Global, Hsla, Img, InteractiveElement as _, IntoElement, ParentElement as _,
    Pixels, Rgba, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, img,
    size,
};
use ratex_layout::{LayoutBox, LayoutOptions, layout, to_display_list, validate_layout};
use ratex_svg::{SvgColorSyntax, SvgOptions, render_to_svg_with_color_syntax};
use ratex_types::{color::Color, math_style::MathStyle};

use crate::svg_image;

/// Math is set larger than the surrounding text, as KaTeX does: TeX's glyphs
/// have a small x-height next to interface fonts.
const MATH_SCALE: f32 = 1.21;

/// SVG user units per em. The SVG is a vector drawn at its element's size,
/// so this only sets the coordinate scale; RaTeX's default keeps its stroke
/// widths in proportion.
const SVG_EM: f64 = 40.;

/// Typeset formulas kept before the oldest are dropped.
const CAPACITY: usize = 1024;

/// Identifies the math plugins' parser configuration, so a TextView keeps
/// its parsed document while the registry is handed to it every frame.
const PARSER_REVISION: u64 = 0x6d61_7468;

const INLINE: &str = "math-inline";
const DISPLAY: &str = "math-display";

/// The math plugins, built once.
static EXTENSIONS: LazyLock<MarkdownExtensions> = LazyLock::new(|| {
    MarkdownExtensions::default()
        .plugin(InlineMath)
        .plugin(DisplayMath)
        .parser_revision(PARSER_REVISION)
});

/// `view` with TeX math typeset when `enabled`; otherwise `view` as is.
///
/// The registry is a clone of one shared value, so its revision is the same
/// every frame and the view's state neither re-parses nor re-registers.
pub fn text_view(view: TextView, enabled: bool) -> TextView {
    if enabled {
        view.markdown_extensions(EXTENSIONS.clone())
    } else {
        view
    }
}

/// The markdown a message's `text` is shown as: with the `\(…\)` and `\[…\]`
/// delimiters rewritten when math is `enabled`.
pub fn markdown_source(text: &str, enabled: bool) -> Cow<'_, str> {
    if enabled {
        normalize_delimiters(text)
    } else {
        Cow::Borrowed(text)
    }
}

/// A formula RaTeX accepted, as a markdown node's data.
struct Formula {
    tex: Arc<str>,
    display: bool,
    /// Hash of `tex` and `display`: the formula's half of the cache key.
    key: u64,
}

impl Formula {
    /// `tex` when RaTeX parses and lays it out within its resource limits.
    fn new(tex: &str, display: bool) -> Option<Self> {
        if tex.trim().is_empty() {
            return None;
        }
        layout_box(tex, display, Color::BLACK)?;
        let mut hasher = DefaultHasher::new();
        (tex, display).hash(&mut hasher);
        Some(Self {
            tex: tex.into(),
            display,
            key: hasher.finish(),
        })
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

/// A node holding `formula` whose text, for selection and copying, is the
/// TeX `source` as authored.
fn formula_node(name: &'static str, formula: Formula, source: &str) -> MarkdownNode {
    let source = SharedString::from(source.to_owned());
    MarkdownNode::new(name, formula)
        .text(source.clone())
        .markdown(source)
}

/// `$…$` in a line of text.
struct InlineMath;

impl MarkdownPlugin for InlineMath {
    fn name(&self) -> &str {
        INLINE
    }

    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::InlineMath(math) = node else {
            return None;
        };
        let source = cx.node_source(node)?;
        let after = cx.source().get(node.position()?.end.offset..)?;
        let formula = inline_formula(source, &math.value, after)?;
        Some(formula_node(INLINE, formula, source))
    }

    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &InlineRenderContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<InlineElement> {
        let formula = node.data::<Formula>()?;
        let typeset = typeset(formula, context.text_style().color, cx)?;
        let font = context.font_size() * MATH_SCALE;
        Some(InlineElement::new(typeset.image(font)).with_baseline(font * typeset.height))
    }
}

/// A `$$` block, or a paragraph that is one `$$…$$` formula: centered,
/// scrolling sideways when wider than the column.
struct DisplayMath;

impl MarkdownPlugin for DisplayMath {
    fn is_block(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        DISPLAY
    }

    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let tex = match node {
            Node::Math(math) => &math.value,
            Node::Paragraph(paragraph) => match paragraph.children.as_slice() {
                [child @ Node::InlineMath(math)]
                    if cx
                        .node_source(child)
                        .is_some_and(|source| source.starts_with("$$")) =>
                {
                    &math.value
                }
                _ => return None,
            },
            _ => return None,
        };
        let formula = Formula::new(tex, true)?;
        Some(formula_node(DISPLAY, formula, cx.node_source(node)?))
    }

    fn render(&self, node: &MarkdownNode, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let style = window.text_style();
        let typeset = node
            .data::<Formula>()
            .and_then(|formula| Some((formula.key, typeset(formula, style.color, cx)?)));
        let Some((key, typeset)) = typeset else {
            return div().child(node.as_text().to_owned()).into_any_element();
        };
        let font = style.font_size.to_pixels(window.rem_size()) * MATH_SCALE;
        let start = node.source_range().map_or(0, |range| range.start as u64);
        div()
            .id(ElementId::NamedInteger(DISPLAY.into(), key ^ start))
            .w_full()
            .overflow_x_scroll()
            .restrict_scroll_to_axis()
            .child(
                div()
                    .flex()
                    .min_w_full()
                    .justify_center()
                    .child(typeset.image(font)),
            )
            .into_any_element()
    }
}

/// A typeset formula: its SVG and its box, in em.
struct Typeset {
    svg: Arc<[u8]>,
    /// Identifies `svg`'s content for the raster cache.
    key: u64,
    width: f32,
    height: f32,
    depth: f32,
}

impl Typeset {
    fn new(formula: &Formula, color: Rgba, key: u64) -> Option<Self> {
        let color = Color::new(color.r, color.g, color.b, color.a);
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
            key,
            width: list.width as f32,
            height: list.height as f32,
            depth: list.depth as f32,
        })
    }

    /// The formula drawn with `font` pixels per em.
    fn image(&self, font: Pixels) -> Img {
        let size = size(font * self.width, font * (self.height + self.depth));
        img(svg_image::exact(self.key, self.svg.clone(), size))
            .flex_none()
            .w(size.width)
            .h(size.height)
    }
}

/// Formula key and packed RGBA color.
type Key = (u64, u32);

/// Typeset formulas by formula and color, with each entry's TeX to tell
/// hash collisions apart. A failed typeset is kept too, so it is not retried
/// every frame.
#[derive(Default)]
struct Typesets {
    entries: HashMap<Key, (Arc<str>, Option<Arc<Typeset>>)>,
    order: VecDeque<Key>,
}

impl Global for Typesets {}

/// `formula` typeset in `color`, from the cache when it was before.
fn typeset(formula: &Formula, color: Hsla, cx: &mut App) -> Option<Arc<Typeset>> {
    let color = Rgba::from(color);
    let key = (formula.key, pack(color));
    if let Some((tex, typeset)) = cx
        .try_global::<Typesets>()
        .and_then(|typesets| typesets.entries.get(&key))
        && *tex == formula.tex
    {
        return typeset.clone();
    }

    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    let typeset = Typeset::new(formula, color, hasher.finish()).map(Arc::new);
    let typesets = cx.default_global::<Typesets>();
    if typesets
        .entries
        .insert(key, (formula.tex.clone(), typeset.clone()))
        .is_none()
    {
        typesets.order.push_back(key);
    }
    while typesets.order.len() > CAPACITY {
        if let Some(old) = typesets.order.pop_front() {
            typesets.entries.remove(&old);
        }
    }
    typeset
}

/// `color` as 8-bit RGBA.
fn pack(color: Rgba) -> u32 {
    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
    u32::from_be_bytes([
        channel(color.r),
        channel(color.g),
        channel(color.b),
        channel(color.a),
    ])
}

/// Rewrites the `\(…\)` and `\[…\]` math delimiters models write, which
/// markdown reads as escaped brackets, to `$…$` and `$$…$$`.
///
/// Code spans, fenced and indented code blocks are left alone, and so is an
/// escaped backslash (`\\(` is a backslash and a parenthesis). Space just
/// inside `\(…\)` is dropped, so the formula passes the dollar rule. A `\[`
/// that starts a line (after indentation or `>` markers) becomes a `$$`
/// block, its fences on lines of their own. Text without `\(` or `\[` is
/// returned as is.
pub fn normalize_delimiters(text: &str) -> Cow<'_, str> {
    if !text.contains("\\(") && !text.contains("\\[") {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 16);
    let mut blocks = CodeBlocks::new();
    let mut display = None;
    let mut prose: Option<usize> = None;
    let mut pos = 0;
    for line in text.split_inclusive('\n') {
        if blocks.is_verbatim(line) {
            if let Some(start) = prose.take() {
                convert(&text[start..pos], &mut out, &mut display);
            }
            out.push_str(line);
        } else if prose.is_none() {
            prose = Some(pos);
        }
        pos += line.len();
    }
    if let Some(start) = prose {
        convert(&text[start..], &mut out, &mut display);
    }
    if out == text {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(out)
    }
}

/// An open `\[`: inside a line, or a block whose lines start with the
/// opening line's prefix (indentation and `>` markers).
#[derive(Clone, Copy)]
enum Bracket<'a> {
    Inline,
    Block(&'a str),
}

/// Rewrites the delimiters in `chunk`, consecutive non-blank prose lines
/// (code spans don't cross a blank line), onto `out`. `display` carries an
/// open `\[` block from one chunk to the next.
fn convert<'a>(chunk: &'a str, out: &mut String, display: &mut Option<Bracket<'a>>) {
    let bytes = chunk.as_bytes();
    // An open `\(`: where its `$` is in `out`, and where it is in `chunk`.
    let mut inline: Option<(usize, usize)> = None;
    let (mut i, mut copied, mut line_start) = (0, 0, 0);
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                i += 1;
                line_start = i;
            }
            b'`' => {
                let run = bytes[i..].iter().take_while(|&&b| b == b'`').count();
                let end = closing_run(&chunk[i + run..], run).map_or(i + run, |end| i + run + end);
                if let Some(newline) = chunk[i..end].rfind('\n') {
                    line_start = i + newline + 1;
                }
                i = end;
            }
            b'\\' if i + 1 < bytes.len() => {
                let next = bytes[i + 1];
                if !matches!(next, b'(' | b')' | b'[' | b']') {
                    // An escaped character, `\\` included, is never a delimiter.
                    i += if next.is_ascii_punctuation() { 2 } else { 1 };
                    continue;
                }
                out.push_str(&chunk[copied..i]);
                let start = i;
                i += 2;
                match next {
                    b'(' => {
                        inline = Some((out.len(), start));
                        out.push('$');
                        i = skip_blanks(bytes, i);
                    }
                    b')' => match inline.take() {
                        Some((open, source)) => {
                            trim_blanks(out, open + 1);
                            if out.len() == open + 1 {
                                // `\(\)`: `$$` would open display math.
                                out.truncate(open);
                                out.push_str(&chunk[source..i]);
                            } else {
                                out.push('$');
                            }
                        }
                        None => out.push_str("\\)"),
                    },
                    b'[' => {
                        let prefix = &chunk[line_start..start];
                        if is_line_prefix(prefix) {
                            *display = Some(Bracket::Block(prefix));
                            out.push_str("$$");
                            i = skip_blanks(bytes, i);
                            if !at_line_end(bytes, i) {
                                out.push('\n');
                                out.push_str(prefix);
                            }
                        } else {
                            *display = Some(Bracket::Inline);
                            out.push_str("$$");
                        }
                    }
                    _ => match display.take() {
                        Some(Bracket::Block(prefix)) => {
                            if !is_line_prefix(&chunk[line_start..start]) {
                                trim_blanks(out, 0);
                                out.push('\n');
                                out.push_str(prefix);
                            }
                            out.push_str("$$");
                            i = skip_blanks(bytes, i);
                            if !at_line_end(bytes, i) {
                                out.push('\n');
                                out.push_str(prefix);
                            }
                        }
                        Some(Bracket::Inline) => out.push_str("$$"),
                        None => out.push_str("\\]"),
                    },
                }
                copied = i;
            }
            _ => i += 1,
        }
    }
    out.push_str(&chunk[copied..]);
}

/// The end of the first run of exactly `len` backticks in `text`.
fn closing_run(text: &str, len: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let start = i;
            while i < bytes.len() && bytes[i] == b'`' {
                i += 1;
            }
            if i - start == len {
                return Some(i);
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Whether a line's text before some point is only indentation and `>`
/// markers.
fn is_line_prefix(text: &str) -> bool {
    text.bytes().all(|b| matches!(b, b' ' | b'\t' | b'>'))
}

fn skip_blanks(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
        i += 1;
    }
    i
}

fn at_line_end(bytes: &[u8], i: usize) -> bool {
    matches!(bytes.get(i), None | Some(b'\n' | b'\r'))
}

/// Drops spaces and tabs from the end of `out`, not before `floor`.
fn trim_blanks(out: &mut String, floor: usize) {
    let keep = floor + out[floor..].trim_end_matches([' ', '\t']).len();
    out.truncate(keep);
}

/// Tells code lines from prose, one line after another.
struct CodeBlocks {
    /// An open fence: its character and length.
    fence: Option<(u8, usize)>,
    /// The previous line was indented code.
    indented: bool,
    /// The previous line was blank (or there was none).
    after_blank: bool,
    /// Inside a list, where indented lines continue an item.
    in_list: bool,
}

impl CodeBlocks {
    fn new() -> Self {
        Self {
            fence: None,
            indented: false,
            after_blank: true,
            in_list: false,
        }
    }

    /// Whether `line` is copied unchanged: blank, a fence or inside one, or
    /// indented code.
    fn is_verbatim(&mut self, line: &str) -> bool {
        let rest = after_quote_markers(line);
        let body = rest.trim_start_matches([' ', '\t']);
        let indent = columns(&rest[..rest.len() - body.len()]);
        let may_fence = indent < 4 || self.in_list;
        if let Some((fence, len)) = self.fence {
            let run = body.bytes().take_while(|&b| b == fence).count();
            if may_fence && run >= len && body[run..].trim().is_empty() {
                self.fence = None;
            }
            return true;
        }
        if body.trim().is_empty() {
            self.after_blank = true;
            return true;
        }
        if may_fence && let Some(fence) = opening_fence(body) {
            self.fence = Some(fence);
            self.indented = false;
            self.after_blank = false;
            return true;
        }
        let code = indent >= 4 && !self.in_list && (self.after_blank || self.indented);
        if !code {
            if indent < 4 && is_list_item(body) {
                self.in_list = true;
            } else if indent == 0 && self.after_blank {
                self.in_list = false;
            }
        }
        self.indented = code;
        self.after_blank = false;
        code
    }
}

/// `line` after its blockquote markers.
fn after_quote_markers(mut line: &str) -> &str {
    while let Some(rest) = line.trim_start_matches([' ', '\t']).strip_prefix('>') {
        line = rest.strip_prefix(' ').unwrap_or(rest);
    }
    line
}

/// The width of leading whitespace, tabs stopping every four columns.
fn columns(indent: &str) -> usize {
    indent.bytes().fold(0, |column, b| match b {
        b'\t' => column + 4 - column % 4,
        _ => column + 1,
    })
}

/// The fence `body` opens: three or more backticks (with no backtick after)
/// or tildes.
fn opening_fence(body: &str) -> Option<(u8, usize)> {
    let fence = *body.as_bytes().first()?;
    if !matches!(fence, b'`' | b'~') {
        return None;
    }
    let len = body.bytes().take_while(|&b| b == fence).count();
    (len >= 3 && (fence == b'~' || !body[len..].contains('`'))).then_some((fence, len))
}

/// Whether `body` starts with a bullet or ordered list marker.
fn is_list_item(body: &str) -> bool {
    let rest = match body.strip_prefix(['-', '*', '+']) {
        Some(rest) => rest,
        None => {
            let digits = body.bytes().take_while(u8::is_ascii_digit).count();
            if !(1..=9).contains(&digits) {
                return false;
            }
            match body[digits..].strip_prefix(['.', ')']) {
                Some(rest) => rest,
                None => return false,
            }
        }
    };
    rest.is_empty() || rest.starts_with([' ', '\t', '\n', '\r'])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalized(text: &str) -> String {
        normalize_delimiters(text).into_owned()
    }

    #[test]
    fn inline_delimiters_become_dollars() {
        assert_eq!(normalized(r"so \(x^2\) and \( y \)."), "so $x^2$ and $y$.");
        // Mid-line `\[` is display math inside the line.
        assert_eq!(normalized(r"then \[x\] here"), "then $$x$$ here");
        // Empty: `$$` would open display math.
        assert_eq!(normalized(r"a \(\) b"), r"a \(\) b");
    }

    #[test]
    fn a_line_starting_with_bracket_becomes_a_block() {
        assert_eq!(
            normalized("Sum:\n\\[ \\sum_k k \\]\nafter"),
            "Sum:\n$$\n\\sum_k k\n$$\nafter"
        );
        assert_eq!(normalized("\\[\nx\n\\]\n"), "$$\nx\n$$\n");
        assert_eq!(normalized("> \\[ x \\]"), "> $$\n> x\n> $$");
    }

    #[test]
    fn code_is_left_alone() {
        assert_eq!(normalized(r"`\(x\)` and \(y\)"), r"`\(x\)` and $y$");
        assert_eq!(
            normalized("```\n\\(x\\)\n```\n\\(y\\)"),
            "```\n\\(x\\)\n```\n$y$"
        );
        assert!(matches!(
            normalize_delimiters("text\n\n    \\(x\\)\n"),
            Cow::Borrowed(_)
        ));
        // Indented lines in a list continue its item.
        assert_eq!(normalized("- item\n\n    \\(x\\)"), "- item\n\n    $x$");
    }

    #[test]
    fn an_escaped_backslash_is_not_a_delimiter() {
        assert!(matches!(
            normalize_delimiters(r"a \\(x\\) b"),
            Cow::Borrowed(_)
        ));
        // TeX's `\\` line break inside a formula stays.
        assert_eq!(normalized(r"\(a \\ b\)"), r"$a \\ b$");
    }

    #[test]
    fn text_without_delimiters_is_borrowed() {
        assert!(matches!(
            normalize_delimiters("plain $x$ and (y)"),
            Cow::Borrowed(_)
        ));
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
}
