//! Chat markdown as an element: rune-kit's `Markdown` with math typeset (spec §6.2, §6.5).
//!
//! Display formulas are images `Markdown` draws ([`j3_text::math::prepare`]). Formulas inside
//! a line are drawn in the line: a top-level paragraph that holds one is drawn here as a rich
//! text styled the way `Markdown` styles a paragraph, each formula a typeset image set in its
//! line with [`El::inline_icon`] before a word joiner standing in its place. Elsewhere (lists,
//! quotes, tables) `Markdown` can only draw text, so a formula there reads as its Unicode
//! approximation (see [`j3_text::math::inline`]).
//!
//! Markdown images (`![](…)`) come from the thread's server through `J3::image`. A part whose
//! images are still arriving draws again when they are ready: [`Doc::wait`] collects the
//! decodes it waits on, for the feed's poll.

use std::{
    cell::RefCell,
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    rc::Rc,
};

use j3_text::math::{Formula, SCALE, inline, padded, parse_src, prepare};
use rune::{Color, El, Image, InlineIcon, Link, LinkPaint, Span, TextStyle, col, el, memo, rich};
use rune_kit::{
    Kit, Markdown,
    markdown::{Block, Inline, Style, parse_cached},
};

use super::{Typeset, Typesetter};
use crate::{app::J3, environment::ImageRef, images::ImageLoad};

/// Inter's ascent and descent per em: Rune centres an inline image on the letters of the word
/// after it, so a formula is padded to put its baseline on the text's.
const ASCENT: f32 = 0.969;
const DESCENT: f32 = 0.242;

/// rune-kit's paragraph line height (`MarkdownStyle::from_kit`).
const LINE_HEIGHT: f32 = 22.4;

/// What a message's markdown is drawn with.
#[derive(Clone, Copy)]
pub struct Look<'a> {
    pub k: &'a Kit,
    /// The text colour (muted for reasoning).
    pub ink: Color,
    /// Device pixels per point: formulas rasterize at it.
    pub scale: f32,
    /// The text column's width in points.
    pub width: f32,
    /// The text is still arriving.
    pub streaming: bool,
    /// Typeset math (the `math_rendering` preference); off, the markdown draws as written.
    pub math: bool,
}

/// One message's markdown, cut into parts and with its images resolved.
pub struct Doc {
    parts: Vec<Built>,
    /// Hash of every image's state, for the row's memo version.
    pub images: u64,
    /// Images still decoding; the view draws again once one is ready.
    pub wait: Vec<Image>,
}

enum Built {
    Markdown {
        source: String,
        images: Rc<HashMap<String, (Image, f32)>>,
    },
    Paragraph {
        source: String,
        formulas: Vec<Formula>,
    },
}

/// The parts of `text` and the images they draw, read through `s` for a thread of project `project_id` on
/// environment `env`.
pub fn doc(s: &J3, env: &str, project_id: &str, text: &str, look: Look) -> Doc {
    let parts = parts(text, look.math);
    let mut doc = Doc {
        parts: Vec::with_capacity(parts.len()),
        images: 0,
        wait: Vec::new(),
    };
    let mut hasher = DefaultHasher::new();
    for part in parts.iter() {
        match part {
            Part::Markdown { source, formulas } => {
                let mut images = HashMap::new();
                for src in image_sources(&parse_cached(source)) {
                    let image = match parse_src(&src) {
                        Some(index) => formulas.get(index).and_then(|formula| {
                            let image = s.math.borrow_mut().image(
                                s.images.cache(),
                                formula,
                                look.ink,
                                look.k.ty.body.size,
                                look.scale,
                            )?;
                            Some((image, look.scale))
                        }),
                        None => match s.image(env, &ImageRef::markdown(&src, project_id)) {
                            ImageLoad::Loaded(image) => {
                                let density = s.images.density(&image);
                                Some((image, density))
                            }
                            ImageLoad::Loading => {
                                1u8.hash(&mut hasher);
                                None
                            }
                            ImageLoad::Failed => {
                                2u8.hash(&mut hasher);
                                None
                            }
                        },
                    };
                    if let Some((image, density)) = image {
                        let ready = image.is_ready();
                        (image.id(), ready).hash(&mut hasher);
                        if !ready {
                            doc.wait.push(image.clone());
                        }
                        images.insert(src, (image, density));
                    }
                }
                doc.parts.push(Built::Markdown {
                    source: source.clone(),
                    images: Rc::new(images),
                });
            }
            Part::Paragraph { source, formulas } => doc.parts.push(Built::Paragraph {
                source: source.clone(),
                formulas: formulas.clone(),
            }),
        }
    }
    doc.images = hasher.finish();
    doc
}

impl Doc {
    /// The markdown as a column of its parts, spaced as `Markdown` spaces blocks. `key`
    /// names the parts' memos; `on_link` opens a link's destination.
    pub fn view(&self, s: &J3, key: &str, look: Look) -> El {
        let k = look.k;
        let ink = (
            look.ink.r.to_bits(),
            look.ink.g.to_bits(),
            look.ink.b.to_bits(),
        );
        let mut column = col().w_full().selection_group(true);
        let mut previous: Option<Rc<Vec<Block>>> = None;
        for (index, part) in self.parts.iter().enumerate() {
            let (source, blocks) = match part {
                Built::Markdown { source, .. } | Built::Paragraph { source, .. } => {
                    (source, parse_cached(source))
                }
            };
            let top = match (previous.as_deref().and_then(|b| b.last()), blocks.first()) {
                (Some(before), Some(next)) => gap(before, next),
                _ => 0.0,
            };
            let sizes = (
                k.ty.body.size.to_bits(),
                look.scale.to_bits(),
                look.width.to_bits(),
            );
            let version = (
                source,
                look.streaming,
                k.dark,
                ink,
                sizes,
                self.images,
            );
            let part_el = memo(format!("{key}-part-{index}"), version, || match part {
                Built::Markdown { source, images } => markdown(source, images.clone(), look),
                Built::Paragraph { source, formulas } => paragraph(s, source, formulas, look),
            });
            column = column.child(el().w_full().mt(top).child(part_el));
            if !blocks.is_empty() {
                previous = Some(blocks);
            }
        }
        column
    }
}

/// A message's parts before their images are resolved.
enum Part {
    Markdown {
        source: String,
        formulas: Rc<Vec<Formula>>,
    },
    Paragraph {
        source: String,
        formulas: Vec<Formula>,
    },
}

/// Split texts by `(text hash, math on)`.
type PartsCache = HashMap<(u64, bool), Rc<Vec<Part>>>;

/// `text` prepared and split, cached by text: a mounted message is drawn every commit, and a
/// streaming one grows, so the newest few hundred texts are kept.
fn parts(text: &str, math: bool) -> Rc<Vec<Part>> {
    const KEEP: usize = 256;
    thread_local! {
        static CACHE: RefCell<[PartsCache; 2]> = RefCell::default();
    }
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    let key = (hasher.finish(), math);
    CACHE.with_borrow_mut(|[current, previous]| {
        if let Some(parts) = current.get(&key) {
            return parts.clone();
        }
        let parts = previous
            .remove(&key)
            .unwrap_or_else(|| Rc::new(split(text, math)));
        if current.len() >= KEEP {
            *previous = std::mem::take(current);
        }
        current.insert(key, parts.clone());
        parts
    })
}

fn split(text: &str, math: bool) -> Vec<Part> {
    if !math {
        return vec![Part::Markdown {
            source: text.to_owned(),
            formulas: Rc::default(),
        }];
    }
    let prepared = prepare(text);
    let formulas = Rc::new(prepared.formulas);
    inline::split(&prepared.source)
        .into_iter()
        .map(|part| match part {
            inline::Part::Markdown(source) => Part::Markdown {
                source,
                formulas: formulas.clone(),
            },
            inline::Part::Paragraph { source, formulas } => Part::Paragraph { source, formulas },
        })
        .collect()
}

/// Every image source in `blocks`, in order.
fn image_sources(blocks: &[Block]) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack: Vec<&[Block]> = vec![blocks];
    while let Some(blocks) = stack.pop() {
        for block in blocks {
            match block {
                Block::Image { src, .. } => out.push(src.clone()),
                Block::Quote(inner) => stack.push(inner),
                Block::List { items, .. } => stack.extend(items.iter().map(Vec::as_slice)),
                _ => {}
            }
        }
    }
    out
}

/// The space `Markdown` puts between `before` and `next`: their margins, collapsed.
fn gap(before: &Block, next: &Block) -> f32 {
    let margins = |b: &Block| -> (f32, f32) {
        match b {
            Block::Para(_) => (0.0, 12.0),
            Block::Heading(1, _) => (28.0, 12.0),
            Block::Heading(2, _) => (24.0, 10.0),
            Block::Heading(..) => (20.0, 8.0),
            Block::Code(_) | Block::Diff(_) | Block::Mermaid(_) => (12.0, 24.0),
            Block::List { .. } => (8.0, 12.0),
            Block::Quote(_) => (16.0, 16.0),
            Block::Rule => (32.0, 32.0),
            Block::Table(_) => (12.0, 16.0),
            Block::Image { .. } => (24.0, 24.0),
        }
    };
    let ((_, below), (above, _)) = (margins(before), margins(next));
    match (before, next) {
        (_, Block::Heading(..) | Block::Rule) => above.max(below),
        (Block::Heading(..), _) => below,
        _ => above.max(below),
    }
}

/// `source` through rune-kit's `Markdown`, its images (display formulas among them) from
/// `images`, each with the device pixels per point it draws at for its natural size.
fn markdown(source: &str, images: Rc<HashMap<String, (Image, f32)>>, look: Look) -> El {
    let mut style = rune_kit::MarkdownStyle::from_kit(look.k);
    if look.ink != look.k.c.text_base {
        style = style.text(look.ink);
    }
    let width = look.width;
    let densities = images.clone();
    Markdown::new(source)
        .style(style)
        .streaming(look.streaming)
        .selectable(true)
        .images(move |src| images.get(src).map(|(image, _)| image.clone()))
        .image_frame(move |picture, frame| {
            let density = densities.get(frame.src).map_or(1.0, |(_, d)| *d);
            if parse_src(frame.src).is_some() {
                return display_formula(frame.alt, frame.image, density, width).unwrap_or(picture);
            }
            natural_size(frame.alt, frame.image, density).unwrap_or(picture)
        })
        .on_link_tx(|_: &mut J3, url, tx| {
            if let Err(e) = tx.open_url(url) {
                tracing::debug!("open {url}: {e:?}");
            }
        })
        .build(look.k)
}

/// A display formula rasterized at `density` pixels per point: drawn at its size in points,
/// centred in the column, scrolling sideways when wider than `width`.
fn display_formula(tex: &str, image: &Image, density: f32, width: f32) -> Option<El> {
    let [w, h] = image.size()?;
    let (w, h) = (w as f32 / density, h as f32 / density);
    let formula = el()
        .role("image")
        .label(tex.to_owned())
        .size(w, h)
        .pixels(image.pixels());
    Some(if w <= width {
        rune::row().w_full().justify_center().child(formula)
    } else {
        rune::row().w_full().scroll_x().child(formula)
    })
}

/// A markdown picture rasterized at `density` pixels per point, drawn at its natural size
/// in points up to the column's width, as `Markdown` means to draw one.
fn natural_size(alt: &str, image: &Image, density: f32) -> Option<El> {
    let [w, h] = image.size()?;
    let picture = crate::images::fitted(image, w as f32 / density, h as f32 / density, 4.0);
    let label = if alt.is_empty() { "Image preview" } else { alt };
    Some(crate::viewer::opens(
        picture.role("image").label(label),
        image,
        density,
        label,
    ))
}

/// A top-level paragraph holding formulas: `source` (markdown, each formula a placeholder)
/// as `Markdown` draws a paragraph, each formula typeset in its line.
fn paragraph(s: &J3, source: &str, formulas: &[Formula], look: Look) -> El {
    let blocks = parse_cached(source);
    let [Block::Para(inl)] = blocks.as_slice() else {
        return markdown(
            &inline::unicode_source(source, formulas),
            Rc::default(),
            look,
        );
    };
    let k = look.k;
    let body = TextStyle {
        line_height: LINE_HEIGHT,
        ..k.ty.body
    };
    // Each placeholder becomes a word joiner (as long in UTF-8), so spans keep their bytes.
    let mut text = String::with_capacity(inl.text.len());
    let mut marks = Vec::new();
    for (at, c) in inl.text.char_indices() {
        match inline::placeholder_index(c) {
            Some(index) => {
                marks.push((at, index));
                text.push('\u{2060}');
            }
            None => text.push(c),
        }
    }
    let mut e = rich(text, spans(inl, look, body.size))
        .font(body)
        .color(look.ink)
        .exact_lines()
        .w_full()
        .selectable(true)
        .selection_color(k.selection());
    for &(a, b, style) in &inl.spans {
        match style {
            Style::Strike => e = e.strike(a, b, None),
            Style::CodePad => e = e.uncopied(a..b),
            _ => {}
        }
    }
    let mut typesetter = s.math.borrow_mut();
    for (at, index) in marks {
        e = e.uncopied(at..at + '\u{2060}'.len_utf8());
        let Some(formula) = formulas.get(index) else {
            continue;
        };
        if let Some((pixels, side)) =
            typesetter.inline(s.images.cache(), formula, look.ink, body.size, look.scale)
        {
            e = e.inline_icon(at, InlineIcon::image(pixels).size(side).gap(0.0));
        }
    }
    drop(typesetter);
    if !inl.links.is_empty() {
        let hover = LinkPaint::new().underline(k.c.text_accent);
        for (a, b, url) in &inl.links {
            e = e.link(Link::new(*a..*b, url.clone()).hover(hover));
        }
        e = e.on_link(|_: &mut J3, url: &str, tx: &mut rune::Tx| {
            if let Err(err) = tx.open_url(url) {
                tracing::debug!("open {url}: {err:?}");
            }
        });
    }
    e
}

/// `Markdown`'s paragraph spans for `inl` (its `Renderer::spans`): links in the accent with
/// an underline, inline code as a tinted monospace chip, strong and emphasis.
fn spans(inl: &Inline, look: Look, base: f32) -> Vec<Span> {
    let k = look.k;
    let mut out = Vec::with_capacity(inl.spans.len());
    for &(a, b, style) in &inl.spans {
        let mut span = Span::new(a, b);
        match style {
            Style::Link => {
                span.color = Some(k.c.text_accent);
                span.underline = Some(k.alpha(k.c.text_accent, 0.4));
            }
            Style::Code => {
                span.mono = true;
                span.weight = Some(400.0);
                span.size = Some(base * 0.9);
                span = span.bg(k.alpha(k.c.text_base, 0.08), 3.5);
            }
            Style::Strong => span.weight = Some(670.0),
            Style::Em => span.italic = true,
            Style::Strike | Style::CodePad => continue,
        }
        out.push(span);
    }
    out
}

impl Typesetter {
    /// `formula` for a line of text `font_size` points tall: its raster (padded so its
    /// baseline sits on the text's when centred on the letters) and the side in points of the
    /// square it fits, which is the room it takes in the line. `None` when RaTeX cannot
    /// typeset it.
    pub fn inline(
        &mut self,
        cache: &rune::ImageCache,
        formula: &Formula,
        color: Color,
        font_size: f32,
        scale: f32,
    ) -> Option<(rune::Pixels, f32)> {
        let typeset = self.typeset(formula, color)?;
        Some(typeset.inline(cache, font_size, scale))
    }
}

impl Typeset {
    fn inline(
        &mut self,
        cache: &rune::ImageCache,
        font_size: f32,
        scale: f32,
    ) -> (rune::Pixels, f32) {
        let em = font_size * SCALE;
        // The letters' centre above the baseline, in the formula's em.
        let centre = (ASCENT - DESCENT) / 2.0 * font_size / em;
        let lift = self.height - self.depth - 2.0 * centre;
        let (top, bottom) = if lift > 0.0 {
            (0.0, lift)
        } else {
            (-lift, 0.0)
        };
        let tall = self.height + self.depth + top + bottom;
        let side = em * self.width.max(tall);
        if let Some((drawn, drawn_scale, image)) = &self.inline_raster
            && *drawn == side
            && *drawn_scale == scale
        {
            return (image.pixels().clone(), side);
        }
        let svg = padded(&self.svg, self.width, self.height + self.depth, top, bottom);
        let image = cache.svg(svg, side, scale);
        let pixels = image.pixels().clone();
        self.inline_raster = Some((side, scale, image));
        (pixels, side)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_parts_are_spaced_as_markdown_spaces_blocks() {
        let para = Block::Para(Inline::default());
        let heading = Block::Heading(2, Inline::default());
        let list = Block::List {
            start: None,
            items: Vec::new(),
        };
        assert_eq!(gap(&para, &para), 12.0);
        assert_eq!(gap(&para, &heading), 24.0);
        assert_eq!(gap(&heading, &para), 10.0);
        assert_eq!(gap(&para, &list), 12.0);
    }

    #[test]
    fn an_inline_formula_takes_its_width_in_the_line() {
        let cache = rune::ImageCache::new(std::sync::Arc::new(|| {}));
        let mut typesetter = Typesetter::default();
        let wide = Formula {
            tex: "a + b + c".into(),
            display: false,
        };
        let (_, side) = typesetter
            .inline(&cache, &wide, Color::BLACK, 14.0, 2.0)
            .unwrap();
        assert!(side > 14.0 * 2.0, "{side}");
        let unknown = Formula {
            tex: r"\frac{a".into(),
            display: false,
        };
        assert!(
            typesetter
                .inline(&cache, &unknown, Color::BLACK, 14.0, 2.0)
                .is_none()
        );
    }
}
