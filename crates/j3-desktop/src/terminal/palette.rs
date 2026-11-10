//! The terminal's colours: the theme's 16 ANSI colours, xterm's 6×6×6 cube and grey ramp,
//! then foreground, background and cursor from the kit; what a program set with
//! OSC 4/10/11/12 wins. Cell colours (`vte` named, indexed or true colour) resolve here.

use alacritty_terminal::{
    term::color::Colors,
    vte::ansi::{Color as AnsiColor, NamedColor, Rgb},
};
use rune::{
    Color,
    color::{srgb_decode, srgb_encode},
};
use rune_kit::Kit;

/// The colours a frame of the terminal draws with.
#[derive(Clone, Copy, Debug, PartialEq, Hash)]
pub struct Palette {
    ansi: [Color; 16],
    pub foreground: Color,
    pub background: Color,
    pub cursor: Color,
    /// `DimForeground`: faint text.
    pub dim: Color,
    pub selection: Color,
}

impl Palette {
    /// The palette of `k`'s theme.
    pub fn new(k: &Kit) -> Self {
        Self {
            ansi: crate::theme::ansi(k),
            foreground: k.c.text_base,
            background: k.c.bg_base,
            cursor: k.c.text_base,
            dim: k.c.text_muted,
            selection: k.selection(),
        }
    }

    /// Palette entry `index`: 0–15 the ANSI colours, 16–255 xterm's cube and grey ramp, 256
    /// the foreground, 257 the background, 258 the cursor.
    pub fn entry(&self, index: usize) -> Color {
        match index {
            0..=15 => self.ansi[index],
            16..=231 => {
                let n = index - 16;
                let level = |v: usize| if v == 0 { 0 } else { 55 + v as u8 * 40 };
                from_rgb(Rgb {
                    r: level(n / 36),
                    g: level(n / 6 % 6),
                    b: level(n % 6),
                })
            }
            232..=255 => {
                let grey = 8 + (index - 232) as u8 * 10;
                from_rgb(Rgb {
                    r: grey,
                    g: grey,
                    b: grey,
                })
            }
            257 => self.background,
            258 => self.cursor,
            _ => self.foreground,
        }
    }

    /// Entry `index` as the program set it (`colors`), else from the palette.
    pub fn color(&self, colors: &Colors, index: usize) -> Color {
        match colors[index] {
            Some(rgb) => from_rgb(rgb),
            None => self.entry(index),
        }
    }

    /// A cell's colour.
    pub fn resolve(&self, colors: &Colors, color: AnsiColor) -> Color {
        match color {
            AnsiColor::Spec(rgb) => from_rgb(rgb),
            AnsiColor::Indexed(index) => self.color(colors, usize::from(index)),
            AnsiColor::Named(named) => match named {
                NamedColor::BrightForeground => self.color(colors, NamedColor::Foreground as usize),
                NamedColor::DimForeground => self.dim,
                named if (named as usize) >= NamedColor::DimBlack as usize => {
                    let base = named as usize - NamedColor::DimBlack as usize;
                    dimmed(self.color(colors, base))
                }
                named => self.color(colors, named as usize),
            },
        }
    }
}

/// `color` faded as faint (SGR 2) text draws.
pub fn dimmed(color: Color) -> Color {
    color.alpha(color.a * 0.7)
}

/// An sRGB byte triple as a (linear) Rune colour.
pub fn from_rgb(rgb: Rgb) -> Color {
    let channel = |v: u8| srgb_decode(f32::from(v) / 255.0);
    Color {
        r: channel(rgb.r),
        g: channel(rgb.g),
        b: channel(rgb.b),
        a: 1.0,
    }
}

/// A Rune colour as the sRGB bytes an OSC colour report carries.
pub fn to_rgb(color: Color) -> Rgb {
    let channel = |v: f32| (srgb_encode(v.clamp(0.0, 1.0)) * 255.0).round() as u8;
    Rgb {
        r: channel(color.r),
        g: channel(color.g),
        b: channel(color.b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette() -> Palette {
        Palette::new(rune_theme_opencode::kit(true))
    }

    #[test]
    fn colours_round_trip_through_srgb_bytes() {
        for rgb in [
            Rgb { r: 1, g: 2, b: 3 },
            Rgb {
                r: 128,
                g: 200,
                b: 255,
            },
        ] {
            assert_eq!(to_rgb(from_rgb(rgb)), rgb);
        }
    }

    #[test]
    fn the_xterm_cube_and_grey_ramp() {
        let p = palette();
        let rgb = |index| to_rgb(p.entry(index));
        assert_eq!(rgb(16), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(
            rgb(231),
            Rgb {
                r: 255,
                g: 255,
                b: 255
            }
        );
        assert_eq!(rgb(196), Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(rgb(21), Rgb { r: 0, g: 0, b: 255 });
        assert_eq!(rgb(232), Rgb { r: 8, g: 8, b: 8 });
        assert_eq!(
            rgb(255),
            Rgb {
                r: 238,
                g: 238,
                b: 238
            }
        );
    }

    #[test]
    fn named_colours_follow_the_kit_and_the_program() {
        let p = palette();
        let k = rune_theme_opencode::kit(true);
        let mut colors = Colors::default();
        assert_eq!(
            p.resolve(&colors, AnsiColor::Named(NamedColor::Foreground)),
            k.c.text_base
        );
        assert_eq!(
            p.resolve(&colors, AnsiColor::Named(NamedColor::Background)),
            k.c.bg_base
        );
        assert_eq!(
            p.resolve(&colors, AnsiColor::Named(NamedColor::BrightRed)),
            p.entry(9)
        );
        assert_eq!(
            p.resolve(&colors, AnsiColor::Named(NamedColor::DimRed)),
            dimmed(p.entry(1))
        );
        // OSC 4 replaced red.
        colors[1] = Some(Rgb { r: 1, g: 2, b: 3 });
        assert_eq!(
            to_rgb(p.resolve(&colors, AnsiColor::Named(NamedColor::Red))),
            Rgb { r: 1, g: 2, b: 3 }
        );
        assert_eq!(
            to_rgb(p.resolve(&colors, AnsiColor::Spec(Rgb { r: 9, g: 8, b: 7 }))),
            Rgb { r: 9, g: 8, b: 7 }
        );
    }
}
