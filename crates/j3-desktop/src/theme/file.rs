//! A theme file (`docs/themes.md`): JSON naming an appearance and, optionally, the three
//! colours the interface is derived from, the status colours, single tokens of the kit's
//! palette, the code colours and the terminal's 16 ANSI colours. [`Theme::parse`] checks
//! everything and says what is wrong in words a person or an agent can act on;
//! [`Theme::kit`] builds the rune-kit [`Kit`].

use std::collections::BTreeMap;

use rune::Color;
use rune_kit::{
    Kit, Palette, PaletteSeed, StateColors,
    syntax::{Syntax, SyntaxTheme},
};
use serde::Deserialize;

/// A theme file as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    name: String,
    appearance: Appearance,
    background: Option<Hex>,
    foreground: Option<Hex>,
    accent: Option<Hex>,
    #[serde(default)]
    status: BTreeMap<String, Hex>,
    #[serde(default)]
    tokens: BTreeMap<String, Hex>,
    #[serde(default)]
    syntax: BTreeMap<String, Hex>,
    terminal: Option<Terminal>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Terminal {
    ansi: Vec<Hex>,
}

#[derive(Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Appearance {
    Light,
    Dark,
}

/// `#rgb`, `#rrggbb` or `#rrggbbaa`.
#[derive(Deserialize, Clone, Copy)]
#[serde(try_from = "String")]
struct Hex(Color);

impl TryFrom<String> for Hex {
    type Error = String;

    fn try_from(s: String) -> Result<Self, String> {
        rune::color::hex(&s)
            .map(Hex)
            .ok_or_else(|| format!("`{s}` is not a colour; write #rgb, #rrggbb or #rrggbbaa"))
    }
}

type Token = fn(&mut Palette) -> &mut Color;
type Role = fn(&mut Palette) -> &mut StateColors;

macro_rules! tokens {
    ($($name:ident),* $(,)?) => {
        &[$((stringify!($name), |p| &mut p.$name)),*]
    };
}

/// The tokens `tokens` may set: the colour fields of rune-kit's [`Palette`], by name, then
/// the four parts of each status role.
const TOKENS: &[(&str, Token)] = &{
    const PLAIN: &[(&str, Token)] = tokens![
        page,
        bg_base,
        bg_deep,
        layer_01,
        layer_02,
        layer_03,
        layer_04,
        bg_inverse,
        bg_contrast,
        bg_button_neutral,
        bg_icon_button_contrast,
        bg_accent,
        submit_highlight,
        submit_hover,
        submit_fg,
        submit_icon,
        text_base,
        text_muted,
        text_faint,
        text_inverse,
        text_contrast,
        text_accent,
        icon_base,
        icon_muted,
        icon_faint,
        icon_inverse,
        icon_contrast,
        icon_accent,
        border_muted,
        border_base,
        border_strong,
        border_focus,
        hover,
        pressed,
        contrast_hover,
        contrast_pressed,
        scrim,
        depth_top,
        depth_bottom,
        white,
        black,
        knob,
        knob_on,
        surface_base,
        surface_weak,
        surface_hover,
        surface_raised,
        surface_raised_base,
        surface_raised_hover,
        surface_float,
        surface_disabled,
        border_weak,
        border_hover,
        border_active,
        border_disabled,
        text_strong,
        text_weak,
        text_invert_strong,
        icon_weaker,
        std_border,
        std_icon,
        std_icon_invert,
        std_icon_strong,
        std_text,
    ];
    const STATES: [(&str, Token); 16] = [
        ("success_bg", |p| &mut p.success.bg),
        ("success_fg", |p| &mut p.success.fg),
        ("success_border", |p| &mut p.success.border),
        ("success_icon", |p| &mut p.success.icon),
        ("warning_bg", |p| &mut p.warning.bg),
        ("warning_fg", |p| &mut p.warning.fg),
        ("warning_border", |p| &mut p.warning.border),
        ("warning_icon", |p| &mut p.warning.icon),
        ("danger_bg", |p| &mut p.danger.bg),
        ("danger_fg", |p| &mut p.danger.fg),
        ("danger_border", |p| &mut p.danger.border),
        ("danger_icon", |p| &mut p.danger.icon),
        ("info_bg", |p| &mut p.info.bg),
        ("info_fg", |p| &mut p.info.fg),
        ("info_border", |p| &mut p.info.border),
        ("info_icon", |p| &mut p.info.icon),
    ];
    let mut all = [PLAIN[0]; PLAIN.len() + STATES.len()];
    let mut i = 0;
    while i < all.len() {
        all[i] = if i < PLAIN.len() {
            PLAIN[i]
        } else {
            STATES[i - PLAIN.len()]
        };
        i += 1;
    }
    all
};

/// The roles `status` may set.
const ROLES: [(&str, Role); 4] = [
    ("success", |p| &mut p.success),
    ("warning", |p| &mut p.warning),
    ("danger", |p| &mut p.danger),
    ("info", |p| &mut p.info),
];

/// The kinds of code token `syntax` may colour.
const SYNTAX: [(&str, Syntax); 11] = [
    ("keyword", Syntax::Keyword),
    ("string", Syntax::String),
    ("number", Syntax::Number),
    ("comment", Syntax::Comment),
    ("type", Syntax::Type),
    ("function", Syntax::Function),
    ("property", Syntax::Property),
    ("punctuation", Syntax::Punctuation),
    ("inserted", Syntax::Inserted),
    ("deleted", Syntax::Deleted),
    ("parameter", Syntax::Parameter),
];

/// A checked theme.
#[derive(Clone, Debug)]
pub struct Theme {
    pub name: String,
    pub dark: bool,
    /// Background, foreground and accent the palette is derived from; `None` starts from
    /// j3's own light or dark palette.
    seed: Option<PaletteSeed>,
    /// Indices into [`ROLES`], [`TOKENS`] and [`SYNTAX`].
    status: Vec<(usize, Color)>,
    tokens: Vec<(usize, Color)>,
    syntax: Vec<(usize, Color)>,
    /// The terminal's ANSI colours, normal then bright.
    pub ansi: Option<[Color; 16]>,
}

/// The index of `key` in `names`, or which names `section` takes.
fn index<'a>(
    section: &str,
    key: &str,
    mut names: impl Iterator<Item = &'a str> + Clone,
) -> Result<usize, String> {
    names.clone().position(|name| name == key).ok_or_else(|| {
        let mut all = names.next().unwrap_or_default().to_owned();
        for name in names {
            all.push_str(", ");
            all.push_str(name);
        }
        format!("`{section}` has no `{key}`; it takes: {all}")
    })
}

fn indexed<'a>(
    section: &str,
    map: BTreeMap<String, Hex>,
    names: impl Iterator<Item = &'a str> + Clone,
) -> Result<Vec<(usize, Color)>, String> {
    map.into_iter()
        .map(|(key, Hex(color))| Ok((index(section, &key, names.clone())?, color)))
        .collect()
}

impl Theme {
    /// Reads and checks a theme file's text.
    pub fn parse(json: &str) -> Result<Self, String> {
        let file: File = serde_json::from_str(json).map_err(|e| e.to_string())?;
        if file.name.trim().is_empty() {
            return Err("`name` is empty".into());
        }
        let dark = file.appearance == Appearance::Dark;
        let seed = match (file.background, file.foreground, file.accent) {
            (None, None, None) => None,
            (Some(Hex(bg)), Some(Hex(fg)), Some(Hex(accent))) => {
                if bg.a < 1.0 || fg.a < 1.0 || accent.a < 1.0 {
                    return Err("`background`, `foreground` and `accent` must be opaque".into());
                }
                if (fg.luminance() > bg.luminance()) != dark {
                    return Err(format!(
                        "`appearance` is {} but `foreground` is {} than `background`",
                        if dark { "dark" } else { "light" },
                        if dark { "darker" } else { "lighter" },
                    ));
                }
                Some(PaletteSeed { bg, fg, accent })
            }
            (bg, fg, accent) => {
                let missing: Vec<&str> = [
                    ("background", bg.is_none()),
                    ("foreground", fg.is_none()),
                    ("accent", accent.is_none()),
                ]
                .into_iter()
                .filter_map(|(name, missing)| missing.then_some(name))
                .collect();
                return Err(format!(
                    "`background`, `foreground` and `accent` go together (all three or none); missing: {}",
                    missing.join(", ")
                ));
            }
        };
        let ansi = match file.terminal {
            None => None,
            Some(terminal) => {
                let count = terminal.ansi.len();
                let colors: Vec<Color> = terminal.ansi.into_iter().map(|Hex(c)| c).collect();
                Some(<[Color; 16]>::try_from(colors).map_err(|_| {
                    format!("`terminal.ansi` takes 16 colours (black, red, green, yellow, blue, magenta, cyan, white, then their bright versions); it has {count}")
                })?)
            }
        };
        Ok(Self {
            name: file.name.trim().to_owned(),
            dark,
            seed,
            status: indexed("status", file.status, ROLES.iter().map(|r| r.0))?,
            tokens: indexed("tokens", file.tokens, TOKENS.iter().map(|t| t.0))?,
            syntax: indexed("syntax", file.syntax, SYNTAX.iter().map(|s| s.0))?,
            ansi,
        })
    }

    /// The kit, for the blend mode of the window building the view. Type, radii, motion and
    /// glyphs are j3's own in every theme.
    pub fn kit(&self) -> Kit {
        let base = rune_theme_opencode::kit(self.dark);
        let mut kit = match self.seed {
            None => base.clone(),
            Some(seed) => Kit {
                glyph: base.glyph,
                ty: base.ty,
                radius: base.radius,
                motion: base.motion,
                invalid_text: base.invalid_text,
                ..Kit::from_palette(seed)
            },
        };
        let surface = kit.c.bg_base;
        for &(role, color) in &self.status {
            *ROLES[role].1(&mut kit.c) = StateColors {
                bg: surface.mix(color, 0.12),
                fg: color,
                border: surface.mix(color, 0.35),
                icon: color,
            };
        }
        for &(token, color) in &self.tokens {
            *TOKENS[token].1(&mut kit.c) = color;
        }
        kit.blend_over = Some(kit.c.bg_base);
        kit
    }

    /// The code colours over `kit` (this theme's): rune-kit's hues, then the theme's own.
    pub fn syntax(&self, kit: &Kit) -> SyntaxTheme {
        self.syntax
            .iter()
            .fold(SyntaxTheme::from_kit(kit), |theme, &(kind, color)| {
                theme.with(SYNTAX[kind].1, color)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEEDED: &str = r##"{
        "name": "Seeded", "appearance": "dark",
        "background": "#101020", "foreground": "#e0e0ff", "accent": "#ff8800",
        "status": {"danger": "#ff0000"},
        "tokens": {"text_muted": "#808090", "info_icon": "#00ffff"},
        "syntax": {"keyword": "#ff00ff"},
        "terminal": {"ansi": ["#000","#111","#222","#333","#444","#555","#666","#777",
                              "#888","#999","#aaa","#bbb","#ccc","#ddd","#eee","#fff"]}
    }"##;

    fn hex(s: &str) -> Color {
        rune::color::hex(s).unwrap()
    }

    #[test]
    fn a_seeded_theme_derives_its_palette_and_applies_its_overrides() {
        let theme = Theme::parse(SEEDED).unwrap();
        let kit = theme.kit();
        assert!(kit.dark && theme.dark);
        assert_eq!(kit.c.bg_base, hex("#101020"));
        assert_eq!(kit.c.text_base, hex("#e0e0ff"));
        assert_eq!(kit.c.bg_accent, hex("#ff8800"));
        assert_eq!(kit.c.text_muted, hex("#808090"));
        assert_eq!(kit.c.danger.icon, hex("#ff0000"));
        assert_ne!(kit.c.danger.bg, kit.c.danger.icon);
        assert_eq!(kit.c.info.icon, hex("#00ffff"));
        // j3's type in every theme.
        assert_eq!(kit.ty.ui, rune_theme_opencode::kit(true).ty.ui);
        let syntax = theme.syntax(&kit);
        assert_eq!(syntax.color(Syntax::Keyword), hex("#ff00ff"));
        assert_eq!(
            syntax.color(Syntax::String),
            SyntaxTheme::from_kit(&kit).color(Syntax::String)
        );
        assert_eq!(theme.ansi.unwrap()[15], hex("#fff"));
    }

    #[test]
    fn an_unseeded_theme_starts_from_j3s_palette() {
        let theme = Theme::parse(
            r##"{"name": "Mine", "appearance": "light", "tokens": {"bg_base": "#fafafa"}}"##,
        )
        .unwrap();
        let (kit, base) = (theme.kit(), rune_theme_opencode::kit(false));
        assert_eq!(kit.c.bg_base, hex("#fafafa"));
        assert_eq!(kit.blend_over, Some(hex("#fafafa")));
        assert_eq!(kit.c.text_base, base.c.text_base);
    }

    #[test]
    fn mistakes_are_named() {
        let error = |json: &str| Theme::parse(json).unwrap_err();
        assert!(
            error(r##"{"name": "x", "appearance": "dark", "tokens": {"text_mute": "#fff"}}"##)
                .starts_with("`tokens` has no `text_mute`; it takes: page, bg_base,")
        );
        assert!(
            error(r##"{"name": "x", "appearance": "dark", "syntax": {"keyword": "purple"}}"##)
                .contains("`purple` is not a colour")
        );
        assert!(
            error(r##"{"name": "x", "appearance": "dark", "background": "#000"}"##)
                .ends_with("missing: foreground, accent")
        );
        assert!(
            error(
                r##"{"name": "x", "appearance": "light", "background": "#000",
                    "foreground": "#fff", "accent": "#f00"}"##
            )
            .contains("`appearance` is light but `foreground` is lighter")
        );
        assert!(
            error(r##"{"name": "x", "appearance": "dark", "terminal": {"ansi": ["#000"]}}"##)
                .contains("it has 1")
        );
        assert!(
            error(r##"{"name": "x", "appearance": "dark", "colours": {}}"##).contains("colours")
        );
        assert!(error(r##"{"name": "x"}"##).contains("appearance"));
    }
}
