//! omp "magic keywords" ([`j3_text::keywords`]) drawn with their gradient:
//! the editor advances its phase every [`SHIMMER_FRAME`].

use std::time::Duration;

use j3_text::keywords::{Keyword, gradient};
use rune::Color;
use rune::color::srgb_decode;

/// Interval between shimmer repaints while the editor is focused.
pub const SHIMMER_FRAME: Duration = Duration::from_millis(70);

/// [`gradient`] as opaque colours in linear light.
pub fn colors(keyword: &Keyword, phase: f32) -> Vec<Color> {
    gradient(keyword, phase)
        .into_iter()
        .map(|[r, g, b]| Color {
            r: srgb_decode(r),
            g: srgb_decode(g),
            b: srgb_decode(b),
            a: 1.0,
        })
        .collect()
}
