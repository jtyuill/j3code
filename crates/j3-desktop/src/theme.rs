//! Appearance (spec §8.9): the rune-kit [`Kit`] from `rune_theme_opencode` for the system
//! appearance, with the user's interface and code font sizes applied; [`Metrics`] (zoom,
//! font sizes, rem, text measure) and the status colours the sidebar and feed use.
//!
//! Zoom scales the whole window (`app::view` draws the root under `El::scale(zoom)`), so
//! views lay out in unzoomed points. Font sizes scale the kit's text styles: the kit's own
//! sizes are the defaults (interface 16, code 12), and a larger setting scales every style
//! in proportion. Font families go through `Tx::set_font_families` ([`apply_families`]).

use std::{cell::RefCell, rc::Rc};

use rune::{Color, TextStyle, Tx};
use rune_kit::Kit;

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

/// The kit for `dark`, with `ui`'s font sizes, for the blend mode of the window building the
/// view. Cached until the inputs change, so a view can call it every frame.
pub fn kit(dark: bool, ui: &UiSettings) -> Rc<Kit> {
    type Key = (bool, bool, u32, u32);
    thread_local! {
        static CACHE: RefCell<Option<(Key, Rc<Kit>)>> = const { RefCell::new(None) };
    }
    let key = (
        dark,
        rune::theme::blends_srgb(),
        ui.interface_font_size().to_bits(),
        ui.code_font_size().to_bits(),
    );
    CACHE.with_borrow_mut(|cache| {
        if let Some((cached, kit)) = cache.as_ref()
            && *cached == key
        {
            return kit.clone();
        }
        let mut kit = rune_theme_opencode::kit(dark).clone();
        scale_type(
            &mut kit,
            ui.interface_font_size() / DEFAULT_INTERFACE_FONT_SIZE,
            ui.code_font_size() / DEFAULT_CODE_FONT_SIZE,
        );
        let kit = Rc::new(kit);
        *cache = Some((key, kit.clone()));
        kit
    })
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
        assert_eq!(k.ty.body.size, rune_theme_opencode::kit(true).ty.body.size * 1.25);
        assert_eq!(k.ty.mono.size, rune_theme_opencode::kit(true).ty.mono.size * 1.5);
    }

    #[test]
    fn chosen_fonts_lead_the_family_lists() {
        let mut ui = UiSettings::default();
        assert_eq!(families(&ui), (FAMILIES.0.to_owned(), FAMILIES.1.to_owned()));
        ui.mono_font = Some("JetBrains Mono".into());
        assert_eq!(families(&ui).1, format!("\"JetBrains Mono\", {}", FAMILIES.1));
    }

    #[test]
    fn the_measure_is_in_rems_of_the_interface_size() {
        let m = Metrics::new(&UiSettings::default());
        assert_eq!(m.measure, 38.0 * 16.0);
        assert_eq!(m.row(), 40.0);
    }
}
