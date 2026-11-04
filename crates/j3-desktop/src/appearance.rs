//! Applies the font and zoom preferences to the theme. The interface font
//! size, times the zoom, is the theme's base font size, which GPUI Kit's root
//! uses as the rem size, so everything sized in rems (text, spacing,
//! controls) follows it. The code font size, times the zoom, is the theme's
//! mono size (tool output, the file preview); the terminal's is kept in
//! [`TerminalFontSize`].

use gpui_kit::component::Theme;
use gpui_kit::{App, Entity, Global, Pixels, Rems, SharedString, font, px, rems};

use crate::preferences::Preferences;

/// The theme's font families before any preference touched them.
struct DefaultFonts {
    family: SharedString,
    mono_family: SharedString,
}

impl Global for DefaultFonts {}

/// The terminal's font size, zoom applied.
pub struct TerminalFontSize(pub Pixels);

impl Global for TerminalFontSize {}

/// The terminal's font size, zoom applied.
pub fn terminal_font_size(cx: &App) -> Pixels {
    cx.try_global::<TerminalFontSize>()
        .map_or(px(crate::ui_settings::DEFAULT_TERMINAL_FONT_SIZE), |size| {
            size.0
        })
}

/// Sets the theme's fonts and sizes from the UI preferences. Call it again
/// after the theme reloads (an appearance change), which may reset the fonts.
pub fn apply(preferences: &Entity<Preferences>, cx: &mut App) {
    let ui = preferences.read(cx).ui();
    let zoom = ui.zoom();
    let (ui_font, mono_font) = (ui.ui_font.clone(), ui.mono_font.clone());
    let size = px(ui.interface_font_size() * zoom);
    let mono_size = px(ui.code_font_size() * zoom);
    let terminal_size = px(ui.terminal_font_size() * zoom);
    if !cx.has_global::<DefaultFonts>() {
        let theme = Theme::global(cx);
        let defaults = DefaultFonts {
            family: theme.font_family.clone(),
            mono_family: theme.mono_font_family.clone(),
        };
        cx.set_global(defaults);
    }
    let defaults = cx.global::<DefaultFonts>();
    let family = ui_font.map_or(defaults.family.clone(), SharedString::from);
    let mono_family = mono_font.map_or(defaults.mono_family.clone(), SharedString::from);
    if cx.try_global::<TerminalFontSize>().map(|size| size.0) != Some(terminal_size) {
        cx.set_global(TerminalFontSize(terminal_size));
    }

    let theme = Theme::global(cx);
    if theme.font_family == family
        && theme.mono_font_family == mono_family
        && theme.font_size == size
        && theme.mono_font_size == mono_size
    {
        return;
    }
    Theme::update(cx, |theme| {
        theme.font_family = family;
        theme.mono_font_family = mono_family;
        theme.font_size = size;
        theme.mono_font_size = mono_size;
    });
}

/// How far to move text in `family` at `size` rems down so its baseline
/// meets that of `reference` text (family, size in rems) when both are
/// centred in one row. GPUI centres a line's glyphs on their ascent plus
/// descent, so in a row smaller text otherwise sits above the larger text's
/// baseline.
pub fn baseline_shift(
    cx: &App,
    (family, size): (&SharedString, f32),
    reference: (&SharedString, f32),
) -> Rems {
    // The baseline's distance below the line's centre, per rem of font size.
    let below_centre = |family: &SharedString| {
        let text = cx.text_system();
        let id = text.resolve_font(&font(family.clone()));
        let unit = px(100.);
        // The metrics' descent is negative: it is below the baseline.
        f32::from(text.ascent(id, unit) + text.descent(id, unit)) / 2. / f32::from(unit)
    };
    rems(below_centre(reference.0) * reference.1 - below_centre(family) * size)
}
