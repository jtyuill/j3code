//! Feed text preparation from `j3-text`, as the desktop does it. Snapshots
//! carry raw text; Swift calls these per message and caches by item id and
//! text.

pub use j3_text::math::Formula;
use j3_text::{
    math::{self, inline},
    message::{self, SpanKind},
};

use crate::types::CatalogModel;

/// A formula RaTeX accepted.
#[uniffi::remote(Record)]
pub struct Formula {
    /// The TeX, without its delimiters.
    pub tex: String,
    /// Set in display style.
    pub display: bool,
}

/// A message's markdown with its display formulas taken out.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PreparedMath {
    /// The markdown, each display formula that is a paragraph by itself
    /// replaced by an image `![<tex>](j3-math:<index>)` naming `formulas`.
    pub source: String,
    pub formulas: Vec<Formula>,
}

/// Rewrites `markdown` so each display formula that is a paragraph by
/// itself becomes an image, for a markdown view that draws images as blocks.
#[uniffi::export]
pub fn prepare_math(markdown: String) -> PreparedMath {
    let prepared = math::prepare(&markdown);
    PreparedMath {
        source: prepared.source,
        formulas: prepared.formulas,
    }
}

/// A part of a message from `split_inline_math`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum MathPart {
    /// Markdown, its formulas written as their Unicode approximation.
    Markdown { markdown: String },
    /// A top-level paragraph holding formulas: its markdown with formula
    /// `i` replaced by the private-use character U+E000 + `i`.
    Paragraph {
        source: String,
        formulas: Vec<Formula>,
    },
}

/// Cuts `markdown` into the top-level paragraphs that hold inline formulas
/// (to be drawn with each formula set in its line) and the markdown around
/// them.
#[uniffi::export]
pub fn split_inline_math(markdown: String) -> Vec<MathPart> {
    inline::split(&markdown)
        .into_iter()
        .map(|part| match part {
            inline::Part::Markdown(markdown) => MathPart::Markdown { markdown },
            inline::Part::Paragraph { source, formulas } => {
                MathPart::Paragraph { source, formulas }
            }
        })
        .collect()
}

/// A typeset formula: an SVG with its glyphs embedded and its box in em.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FormulaSvg {
    pub svg: String,
    pub width: f32,
    /// Above the baseline.
    pub height: f32,
    /// Below the baseline.
    pub depth: f32,
}

/// Typesets `formula` in an sRGB color (components 0 to 1); `None` when
/// RaTeX cannot lay it out.
#[uniffi::export]
pub fn typeset_formula(
    formula: Formula,
    red: f32,
    green: f32,
    blue: f32,
    alpha: f32,
) -> Option<FormulaSvg> {
    let svg = formula.svg([red, green, blue, alpha])?;
    Some(FormulaSvg {
        svg: svg.svg,
        width: svg.width,
        height: svg.height,
        depth: svg.depth,
    })
}

/// A typeset formula as pixels, for an image in a line of text.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FormulaBitmap {
    pub width: u32,
    pub height: u32,
    /// Premultiplied RGBA, row by row.
    pub rgba: Vec<u8>,
    /// Pixels from the top to the baseline.
    pub ascent: f32,
}

/// Typesets `formula` in an sRGB color and rasterizes it next to text of
/// `font_size` pixels (math is set `j3_text::math::SCALE` larger, as on the
/// desktop); `None` when RaTeX cannot lay it out.
#[uniffi::export]
pub fn rasterize_formula(
    formula: Formula,
    red: f32,
    green: f32,
    blue: f32,
    alpha: f32,
    font_size: f32,
) -> Option<FormulaBitmap> {
    use resvg::{tiny_skia, usvg};

    let svg = formula.svg([red, green, blue, alpha])?;
    let tree = usvg::Tree::from_str(&svg.svg, &usvg::Options::default()).ok()?;
    let em = font_size * math::SCALE;
    let size = tree.size();
    let scale = em * svg.width / size.width();
    let width = (size.width() * scale).ceil().max(1.) as u32;
    let height = (size.height() * scale).ceil().max(1.) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Some(FormulaBitmap {
        width,
        height,
        rgba: pixmap.take(),
        ascent: svg.height * em,
    })
}

/// A sent user message as displayed: `^model` mentions collapsed to chips
/// and omp's magic keywords marked.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct RichText {
    pub text: String,
    /// Sorted by start and never overlapping.
    pub spans: Vec<RichSpan>,
}

/// A styled stretch of `RichText::text`, in UTF-16 code units (`NSRange`).
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct RichSpan {
    pub location: u32,
    pub length: u32,
    pub kind: RichSpanKind,
}

#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum RichSpanKind {
    /// A magic keyword, drawn with a gradient from `hue_from` to `hue_to`
    /// degrees.
    Keyword {
        word: String,
        hue_from: f32,
        hue_to: f32,
    },
    /// A model chip.
    Model,
}

/// The displayed form of a user message. A `^provider/id` mention of one of
/// `models` becomes a chip with the model's name; others stay literal.
#[uniffi::export]
pub fn rich_user_text(text: String, models: Vec<CatalogModel>) -> RichText {
    let rich = message::rich(&text, |selector| {
        models
            .iter()
            .find(|model| {
                selector
                    .strip_prefix(model.provider.as_str())
                    .and_then(|rest| rest.strip_prefix('/'))
                    == Some(model.id.as_str())
            })
            .map(|model| model.name.clone())
    });
    let utf16 = |bytes: &str| bytes.encode_utf16().count() as u32;
    let mut spans = Vec::with_capacity(rich.spans.len());
    let mut offset = 0;
    let mut location = 0;
    for span in &rich.spans {
        location += utf16(&rich.text[offset..span.range.start]);
        let length = utf16(&rich.text[span.range.clone()]);
        spans.push(RichSpan {
            location,
            length,
            kind: match span.kind {
                SpanKind::Keyword(keyword) => RichSpanKind::Keyword {
                    word: keyword.word.to_owned(),
                    hue_from: keyword.hue_from,
                    hue_to: keyword.hue_to,
                },
                SpanKind::Model => RichSpanKind::Model,
            },
        });
        location += length;
        offset = span.range.end;
    }
    RichText {
        text: rich.text,
        spans,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(provider: &str, id: &str, name: &str) -> CatalogModel {
        CatalogModel {
            provider: provider.into(),
            id: id.into(),
            name: name.into(),
            thinking: None,
            default: false,
        }
    }

    #[test]
    fn rich_spans_count_utf16_units() {
        // "é" is one UTF-16 unit and two UTF-8 bytes; "𝑥" is two and four.
        let rich = rich_user_text(
            "é𝑥 ^a/b ultrathink".into(),
            vec![model("a", "b", "Bee"), model("a", "bc", "BC")],
        );
        assert_eq!(rich.spans.len(), 2, "{rich:?}");
        let units: Vec<u16> = rich.text.encode_utf16().collect();
        let text = |span: &RichSpan| {
            String::from_utf16(
                &units[span.location as usize..(span.location + span.length) as usize],
            )
            .unwrap()
        };
        assert_eq!(rich.spans[0].kind, RichSpanKind::Model);
        assert!(text(&rich.spans[0]).ends_with("Bee"), "{rich:?}");
        assert!(matches!(&rich.spans[1].kind,
            RichSpanKind::Keyword { word, .. } if word == "ultrathink"));
        assert_eq!(text(&rich.spans[1]), "ultrathink");
    }

    #[test]
    fn math_is_prepared_and_typeset() {
        let prepared = prepare_math("Area:\n\n$$\\pi r^2$$\n".into());
        assert_eq!(prepared.formulas.len(), 1, "{prepared:?}");
        assert!(prepared.source.contains("j3-math:0"), "{prepared:?}");
        let svg = typeset_formula(prepared.formulas[0].clone(), 0., 0., 0., 1.).unwrap();
        assert!(svg.svg.starts_with("<svg"), "{}", &svg.svg[..40]);
        assert!(svg.width > 0.);
        let bitmap = rasterize_formula(prepared.formulas[0].clone(), 0., 0., 0., 1., 34.).unwrap();
        assert_eq!(
            bitmap.rgba.len(),
            (bitmap.width * bitmap.height * 4) as usize
        );
        assert!(bitmap.ascent > 0. && bitmap.ascent <= bitmap.height as f32 + 1.);
        assert!(bitmap.rgba.chunks(4).any(|pixel| pixel[3] > 0), "ink drawn");

        let parts = split_inline_math("Say $x^2$ here.\n".into());
        assert!(
            parts.iter().any(
                |part| matches!(part, MathPart::Paragraph { formulas, .. } if formulas.len() == 1)
            ),
            "{parts:?}"
        );
    }
}
