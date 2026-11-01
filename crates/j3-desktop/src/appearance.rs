//! Applies the font and zoom preferences to the theme. Zoom scales the base
//! font size, which GPUI Kit's root uses as the rem size, so everything sized
//! in rems (text, spacing, controls) scales with it.

use gpui_kit::component::Theme;
use gpui_kit::{App, Entity, Global, Pixels, SharedString};

use crate::preferences::Preferences;

/// The theme's fonts before any preference touched them.
struct DefaultFonts {
    family: SharedString,
    size: Pixels,
    mono_family: SharedString,
    mono_size: Pixels,
}

impl Global for DefaultFonts {}

/// Sets the theme's fonts and sizes from the UI preferences. Call it again
/// after the theme reloads (an appearance change), which may reset the fonts.
pub fn apply(preferences: &Entity<Preferences>, cx: &mut App) {
    let ui = preferences.read(cx).ui();
    let zoom = ui.zoom();
    let (ui_font, mono_font) = (ui.ui_font.clone(), ui.mono_font.clone());
    if !cx.has_global::<DefaultFonts>() {
        let theme = Theme::global(cx);
        let defaults = DefaultFonts {
            family: theme.font_family.clone(),
            size: theme.font_size,
            mono_family: theme.mono_font_family.clone(),
            mono_size: theme.mono_font_size,
        };
        cx.set_global(defaults);
    }
    let defaults = cx.global::<DefaultFonts>();
    let family = ui_font.map_or(defaults.family.clone(), SharedString::from);
    let mono_family = mono_font.map_or(defaults.mono_family.clone(), SharedString::from);
    let (size, mono_size) = (defaults.size * zoom, defaults.mono_size * zoom);

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
