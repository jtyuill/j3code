//! TeX math in chat markdown, drawn with Rune. [`j3_text::math`] finds and
//! typesets formulas as SVGs; here they are rasterized at their exact size.
//!
//! Rune's markdown has no math, but it draws an image that stands as a block
//! of its own: [`j3_text::math::prepare`] makes each display formula that is
//! a paragraph by itself such an image, and [`Typesetter`] draws the formula
//! an image names. Markdown draws an image a point per pixel, up to the
//! column's width, so a formula drawn there rasterizes at scale 1 to keep its
//! size.
//!
//! A formula in a line of text is drawn in the line by [`view`], which draws
//! a top-level paragraph holding one itself; elsewhere it reads as Unicode.
//! [`view::doc`] is the whole of it for a message.
//!
//! Formulas are typeset through a bounded cache: a formula is laid out once
//! per color, never per frame.

pub mod view;

use std::{
    collections::{HashMap, VecDeque},
    hash::{DefaultHasher, Hash, Hasher},
    sync::Arc,
};

use j3_text::math::{Formula, SCALE};
use rune::{Image, ImageCache, color::srgb_encode};

/// Typeset formulas kept before the oldest are dropped.
const CAPACITY: usize = 1024;

/// A typeset formula: its SVG and its box, in em.
struct Typeset {
    svg: Arc<[u8]>,
    width: f32,
    height: f32,
    depth: f32,
    /// The last raster drawn: its size in points, its scale, and the handle
    /// that keeps it decoded while the formula is drawn.
    raster: Option<(f32, f32, Image)>,
    /// The same for the padded raster a line of text draws ([`view`]).
    inline_raster: Option<(f32, f32, Image)>,
}

impl Typeset {
    fn new(formula: &Formula, color: [f32; 4]) -> Option<Self> {
        let svg = formula.svg(color)?;
        Some(Self {
            svg: svg.svg.into_bytes().into(),
            width: svg.width,
            height: svg.height,
            depth: svg.depth,
            raster: None,
            inline_raster: None,
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

/// Formula hash and packed RGBA color.
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
        let typeset = self.typeset(formula, color)?;
        Some(typeset.image(cache, font_size * SCALE, scale))
    }

    /// `formula` laid out in `color`, unless it already is, dropping the
    /// oldest entries past [`CAPACITY`]; `None` when RaTeX cannot typeset it.
    fn typeset(&mut self, formula: &Formula, color: rune::Color) -> Option<&mut Typeset> {
        let color = srgb(color);
        let key = (key(formula), pack(color));
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
        self.entries.get_mut(&key)?.1.as_mut()
    }
}

/// Hash of `formula`'s TeX and style: its half of the cache key.
fn key(formula: &Formula) -> u64 {
    let mut hasher = DefaultHasher::new();
    formula.hash(&mut hasher);
    hasher.finish()
}

/// `color`, linear, as sRGB.
fn srgb(color: rune::Color) -> [f32; 4] {
    [
        srgb_encode(color.r),
        srgb_encode(color.g),
        srgb_encode(color.b),
        color.a,
    ]
}

/// An sRGB `color` as 8-bit RGBA.
fn pack(color: [f32; 4]) -> u32 {
    u32::from_be_bytes(color.map(|value| (value.clamp(0., 1.) * 255.).round() as u8))
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
