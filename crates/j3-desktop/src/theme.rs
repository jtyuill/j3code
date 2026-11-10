//! Appearance (spec §8.9): the rune-kit [`Kit`] of the chosen theme ([`library`]: Light or
//! Dark by the system appearance, or one theme by name; `docs/themes.md`), with the user's
//! interface and code font sizes applied; the theme's code and terminal colours;
//! [`Metrics`] (zoom, font sizes, rem, text measure) and the status colours the sidebar and
//! feed use.
//!
//! Zoom scales the whole window (`app::view` draws the root under `El::scale(zoom)`), so
//! views lay out in unzoomed points. Font sizes scale the kit's text styles: the kit's own
//! sizes are the defaults (interface 16, code 12), and a larger setting scales every style
//! in proportion. Font families go through `Tx::set_font_families` ([`apply_families`]).

pub mod file;
pub mod library;

use std::{
    cell::RefCell,
    hash::{DefaultHasher, Hash, Hasher},
    rc::Rc,
};

use rune::{Color, TextStyle, Tx};
use rune_kit::{Kit, MarkdownStyle, syntax::SyntaxTheme};

use crate::ui_settings::{DEFAULT_CODE_FONT_SIZE, DEFAULT_INTERFACE_FONT_SIZE, UiSettings};

/// `AppSpec::fonts`: Inter and IBM Plex Mono.
pub const FONTS: &[&[u8]] = rune_theme_opencode::FONTS;
/// `AppSpec::families`: (sans, mono).
pub const FAMILIES: (&str, &str) = rune_theme_opencode::FAMILIES;

/// Sizes every view lays out with, in unzoomed points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// The window's zoom (1 = 100%); already applied to the whole window.
    pub zoom: f32,
    /// The interface font size the user chose (16 by default): the old app's `rem`.
    pub rem: f32,
    /// Monospace size for code, diffs and tool output.
    pub code: f32,
    /// The terminal's font size.
    pub terminal: f32,
    /// Widest conversation text and composer, in points (`textMeasure` rem).
    pub measure: f32,
}

impl Metrics {
    pub fn new(ui: &UiSettings) -> Self {
        let rem = ui.interface_font_size();
        Self {
            zoom: ui.zoom(),
            rem,
            code: ui.code_font_size(),
            terminal: ui.terminal_font_size(),
            measure: ui.measure() * rem,
        }
    }

    /// `n` rem in points.
    pub fn rems(&self, n: f32) -> f32 {
        n * self.rem
    }

    /// The panes' header row (2.5 rem), which is also the macOS titlebar band.
    pub fn row(&self) -> f32 {
        self.rems(2.5)
    }
}

/// The status colours of spec §4 and §6 (sidebar pills, herdr glyphs, tool rows).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Status {
    pub info: Color,
    pub warning: Color,
    pub danger: Color,
    pub success: Color,
    pub muted: Color,
}

impl Status {
    pub fn of(k: &Kit) -> Self {
        Self {
            info: k.c.info.icon,
            warning: k.c.warning.icon,
            danger: k.c.danger.icon,
            success: k.c.success.icon,
            muted: k.c.text_muted,
        }
    }
}

/// The hover fill of a feed row's header: half the theme's hover, since the feed's rows sit
/// close together and are passed over constantly. (`Kit::alpha` sets a colour's opacity; the
/// theme's hover is translucent already, so its own opacity is what is halved.)
pub fn row_hover(k: &Kit) -> Color {
    k.alpha(k.c.hover, k.c.hover.a * 0.5)
}

/// The theme on screen: what [`kit`] built last, and what it was built from.
struct Shown {
    generation: u64,
    /// The theme asked for by name (`J3_THEME`, else the setting).
    choice: Option<String>,
    /// `choice` names a theme, so the system appearance doesn't matter.
    fixed: bool,
    srgb: bool,
    sizes: (u32, u32),
    kit: Rc<Kit>,
    syntax: SyntaxTheme,
    ansi: [Color; 16],
    stamp: u64,
}

thread_local! {
    static SHOWN: RefCell<Option<Shown>> = const { RefCell::new(None) };
}

/// The kit of `ui`'s theme (Light or Dark by `system_dark` when it names none), with `ui`'s
/// font sizes, for the blend mode of the window building the view. Cached until the inputs
/// change, so a view can call it every frame. A view holding only a kit passes its `dark`.
pub fn kit(system_dark: bool, ui: &UiSettings) -> Rc<Kit> {
    let choice = library::forced().or(ui.theme.as_deref());
    let generation = library::generation();
    let srgb = rune::theme::blends_srgb();
    let sizes = (
        ui.interface_font_size().to_bits(),
        ui.code_font_size().to_bits(),
    );
    SHOWN.with_borrow_mut(|shown| {
        if let Some(s) = shown.as_ref()
            && s.generation == generation
            && s.choice.as_deref() == choice
            && s.srgb == srgb
            && s.sizes == sizes
            && (s.fixed || s.kit.dark == system_dark)
        {
            return s.kit.clone();
        }
        let chosen = choice.and_then(library::get);
        let fixed = chosen.is_some();
        let entry = chosen.unwrap_or_else(|| builtin(system_dark));
        let mut kit = entry.theme.kit();
        scale_type(
            &mut kit,
            ui.interface_font_size() / DEFAULT_INTERFACE_FONT_SIZE,
            ui.code_font_size() / DEFAULT_CODE_FONT_SIZE,
        );
        let syntax = entry.theme.syntax(&kit);
        let ansi = entry.theme.ansi.unwrap_or_else(|| builtin_ansi(kit.dark));
        let stamp = stamp_of(&kit, &syntax);
        let kit = Rc::new(kit);
        *shown = Some(Shown {
            generation,
            choice: choice.map(str::to_owned),
            fixed,
            srgb,
            sizes,
            kit: kit.clone(),
            syntax,
            ansi,
            stamp,
        });
        kit
    })
}

/// The built-in Light or Dark theme.
fn builtin(dark: bool) -> std::sync::Arc<library::Entry> {
    library::get(if dark { library::DARK } else { library::LIGHT })
        .expect("Light and Dark are built in")
}

fn builtin_ansi(dark: bool) -> [Color; 16] {
    builtin(dark)
        .theme
        .ansi
        .expect("Light and Dark name their terminal colours")
}

fn stamp_of(k: &Kit, syntax: &SyntaxTheme) -> u64 {
    let mut hasher = DefaultHasher::new();
    (k.dark, &k.c, syntax).hash(&mut hasher);
    hasher.finish()
}

/// `f` over the theme on screen when `k` is its kit ([`kit`]'s), else `None` (a kit built
/// some other way, as tests do).
fn shown<R>(k: &Kit, f: impl FnOnce(&Shown) -> R) -> Option<R> {
    SHOWN.with_borrow(|shown| shown.as_ref().filter(|s| std::ptr::eq(&*s.kit, k)).map(f))
}

/// The code colours of `k`'s theme.
pub fn syntax(k: &Kit) -> SyntaxTheme {
    shown(k, |s| s.syntax).unwrap_or_else(|| SyntaxTheme::from_kit(k))
}

/// The terminal's 16 ANSI colours (normal then bright) in `k`'s theme.
pub fn ansi(k: &Kit) -> [Color; 16] {
    shown(k, |s| s.ansi).unwrap_or_else(|| builtin_ansi(k.dark))
}

/// A number that changes when `k`'s colours do: the part of a `memo` version that follows
/// the theme.
pub fn stamp(k: &Kit) -> u64 {
    shown(k, |s| s.stamp).unwrap_or_else(|| stamp_of(k, &SyntaxTheme::from_kit(k)))
}

/// How markdown looks in `k`'s theme: the kit's tokens, with the theme's code colours.
pub fn markdown(k: &Kit) -> MarkdownStyle {
    MarkdownStyle::from_kit(k).syntax_theme(syntax(k))
}

/// Scales every text style of `k`: sans styles by `ui`, monospace by `mono`.
fn scale_type(k: &mut Kit, ui: f32, mono: f32) {
    let scale = |style: &mut TextStyle, by: f32| {
        style.size *= by;
        style.line_height *= by;
    };
    let t = &mut k.ty;
    for style in [
        &mut t.ui,
        &mut t.ui_medium,
        &mut t.ui_compact,
        &mut t.small,
        &mut t.small_medium,
        &mut t.body,
        &mut t.body_medium,
        &mut t.text_small,
        &mut t.text_small_medium,
        &mut t.text_xsmall,
        &mut t.text_xsmall_medium,
    ] {
        scale(style, ui);
    }
    scale(&mut t.mono, mono);
}

/// The (sans, mono) family lists for `ui`'s font choices, falling back to the theme's.
pub fn families(ui: &UiSettings) -> (String, String) {
    let list = |chosen: &Option<String>, default: &str| match chosen {
        Some(font) if !font.trim().is_empty() => format!("\"{}\", {default}", font.trim()),
        _ => default.to_owned(),
    };
    (
        list(&ui.ui_font, FAMILIES.0),
        list(&ui.mono_font, FAMILIES.1),
    )
}

/// Puts `ui`'s font families behind every window's sans and mono slots.
pub fn apply_families(ui: &UiSettings, tx: &mut Tx) {
    let (sans, mono) = families(ui);
    tx.set_font_families(sans, mono);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_theme_ignores_the_system_appearance() {
        let system = UiSettings::default();
        let (light, dark) = (kit(false, &system), kit(true, &system));
        assert!(!light.dark && dark.dark);
        assert_eq!(dark.c.bg_base, rune_theme_opencode::kit(true).c.bg_base);
        let dark_stamp = stamp(&dark);

        let tokyo = UiSettings {
            theme: Some("tokyo-night".into()),
            ..UiSettings::default()
        };
        let k = kit(false, &tokyo);
        assert!(k.dark);
        assert_eq!(k.c.bg_base, rune::color::hex("#1a1b26").unwrap());
        // The same kit whichever appearance a caller passes, so its colours are found.
        assert!(Rc::ptr_eq(&k, &kit(true, &tokyo)));
        assert_eq!(ansi(&k)[1], rune::color::hex("#f7768e").unwrap());
        assert_eq!(
            syntax(&k).color(rune_kit::syntax::Syntax::Keyword),
            rune::color::hex("#bb9af7").unwrap()
        );
        assert_ne!(stamp(&k), dark_stamp);

        // A theme that is gone: the system's.
        let gone = UiSettings {
            theme: Some("no-such-theme".into()),
            ..UiSettings::default()
        };
        assert!(!kit(false, &gone).dark && kit(true, &gone).dark);
    }

    #[test]
    fn default_sizes_keep_the_kits_type() {
        let ui = UiSettings::default();
        let k = kit(true, &ui);
        assert_eq!(k.ty.ui.size, rune_theme_opencode::kit(true).ty.ui.size);
        let larger = UiSettings {
            interface_font_size: 20.0,
            code_font_size: 18.0,
            ..UiSettings::default()
        };
        let k = kit(true, &larger);
        assert_eq!(
            k.ty.body.size,
            rune_theme_opencode::kit(true).ty.body.size * 1.25
        );
        assert_eq!(
            k.ty.mono.size,
            rune_theme_opencode::kit(true).ty.mono.size * 1.5
        );
    }

    #[test]
    fn chosen_fonts_lead_the_family_lists() {
        let mut ui = UiSettings::default();
        assert_eq!(
            families(&ui),
            (FAMILIES.0.to_owned(), FAMILIES.1.to_owned())
        );
        ui.mono_font = Some("JetBrains Mono".into());
        assert_eq!(
            families(&ui).1,
            format!("\"JetBrains Mono\", {}", FAMILIES.1)
        );
    }

    #[test]
    fn the_measure_is_in_rems_of_the_interface_size() {
        let m = Metrics::new(&UiSettings::default());
        assert_eq!(m.measure, 38.0 * 16.0);
        assert_eq!(m.row(), 40.0);
    }
}
