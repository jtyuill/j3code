//! The desktop's UI preferences, stored as the `"ui"` object of
//! `desktop.json`. Every field has a serde default, so files written before a
//! field existed keep loading.

use j3_client::herdr::{AgentOrder, StatusIndicators};
use serde::{Deserialize, Serialize};

use crate::speech::models::SpeechModel;

/// Which thread workflow the sidebar follows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Workflow {
    /// j3's triage workflow (`docs/triage.md`): Pinned, Active, then the
    /// Working, Snoozed and Settled shelves.
    #[default]
    // Files written before the rename stored the old name: read as Inbox, saved as "inbox".
    #[serde(alias = "t3")] // deny-list: allow
    Inbox,
    /// herdr's agent workflow: Spaces with rolled-up attention, then every
    /// agent with its state glyph.
    Herdr,
}

/// A bottom-anchored shelf of the Inbox sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Shelf {
    Working,
    Snoozed,
    Settled,
}

/// Default width of the conversation text and composer, in rem.
pub const DEFAULT_TEXT_MEASURE: f32 = 38.;
/// Narrowest and widest text measure the setting accepts, in rem.
pub const TEXT_MEASURE_MIN: f32 = 28.;
pub const TEXT_MEASURE_MAX: f32 = 64.;

/// Interface font size in px at 100% zoom: the rem size, so text, spacing
/// and controls all follow it.
pub const DEFAULT_INTERFACE_FONT_SIZE: f32 = 16.;
pub const INTERFACE_FONT_SIZE_MIN: f32 = 12.;
pub const INTERFACE_FONT_SIZE_MAX: f32 = 20.;
/// Code font size in px at 100% zoom: tool output and the file preview.
pub const DEFAULT_CODE_FONT_SIZE: f32 = 12.;
pub const CODE_FONT_SIZE_MIN: f32 = 10.;
pub const CODE_FONT_SIZE_MAX: f32 = 18.;
/// Terminal font size in px at 100% zoom.
pub const DEFAULT_TERMINAL_FONT_SIZE: f32 = 14.;
pub const TERMINAL_FONT_SIZE_MIN: f32 = 8.;
pub const TERMINAL_FONT_SIZE_MAX: f32 = 20.;

/// The zoom levels the zoom commands step through; 1 is 100%.
pub const ZOOM_LEVELS: [f32; 9] = [0.75, 0.8, 0.9, 1., 1.1, 1.25, 1.5, 1.75, 2.];

/// The `"ui"` object of `desktop.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UiSettings {
    pub workflow: Workflow,
    /// Running threads leave Active for the Working shelf. Off, Active keeps
    /// its own (rearrangeable) order.
    pub working_shelf: bool,
    /// herdr's `ui.status_indicators`.
    pub status_indicators: StatusIndicators,
    /// herdr's agent panel sort.
    pub agent_order: AgentOrder,
    /// In-app toasts when a thread needs attention or finishes.
    pub toasts: bool,
    /// Sounds for the same events.
    pub sounds: bool,
    /// Typeset TeX math (`$…$`, `$$…$$`, `\(…\)`, `\[…\]`) in messages.
    pub math_rendering: bool,
    /// Inbox shelves the user expanded; every shelf starts collapsed.
    pub expanded_shelves: Vec<Shelf>,
    /// Widest the conversation text and composer get, in rem. Narrower
    /// columns are easier to read.
    pub text_measure: f32,
    /// Interface font family; `None` is the system font.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_font: Option<String>,
    /// Monospace (code, tool output) font family; `None` is the platform's
    /// default monospace font.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mono_font: Option<String>,
    /// Interface font size in px at 100% zoom.
    pub interface_font_size: f32,
    /// Code font size in px at 100% zoom.
    pub code_font_size: f32,
    /// Terminal font size in px at 100% zoom.
    pub terminal_font_size: f32,
    /// Scale of the whole interface; 1 is 100%.
    pub zoom: f32,
    /// Starred models (slugs such as `anthropic/claude-opus-5-5`), pinned
    /// to the top of the model picker in starring order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub favorite_models: Vec<String>,
    /// omp provider ids (`anthropic`, `opencode-go`) whose session, weekly
    /// and monthly usage the composer shows beside the model.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub usage_inline: Vec<String>,
    /// The dictation model's key (`small.en`, `large-v3-turbo`, …); `None`
    /// is the one recommended for this machine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dictation_model: Option<String>,
    /// What this computer is called in j3; `None` is inferred from the machine (`device`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            workflow: Workflow::default(),
            working_shelf: true,
            status_indicators: StatusIndicators::default(),
            agent_order: AgentOrder::default(),
            toasts: true,
            sounds: true,
            math_rendering: true,
            expanded_shelves: Vec::new(),
            text_measure: DEFAULT_TEXT_MEASURE,
            ui_font: None,
            mono_font: None,
            interface_font_size: DEFAULT_INTERFACE_FONT_SIZE,
            code_font_size: DEFAULT_CODE_FONT_SIZE,
            terminal_font_size: DEFAULT_TERMINAL_FONT_SIZE,
            favorite_models: Vec::new(),
            usage_inline: Vec::new(),
            dictation_model: None,
            device_name: None,
            zoom: 1.,
        }
    }
}

impl UiSettings {
    pub fn is_expanded(&self, shelf: Shelf) -> bool {
        self.expanded_shelves.contains(&shelf)
    }

    /// The dictation model picked in Settings; `None` (also for a key this
    /// build does not know) is the one recommended for this machine.
    pub fn dictation_model(&self) -> Option<SpeechModel> {
        self.dictation_model
            .as_deref()
            .and_then(SpeechModel::from_key)
    }

    pub fn set_expanded(&mut self, shelf: Shelf, expanded: bool) {
        self.expanded_shelves.retain(|s| *s != shelf);
        if expanded {
            self.expanded_shelves.push(shelf);
        }
    }

    /// The text measure, kept inside the range the setting offers (a file
    /// edited by hand could hold anything).
    pub fn measure(&self) -> f32 {
        self.text_measure.clamp(TEXT_MEASURE_MIN, TEXT_MEASURE_MAX)
    }

    /// The interface font size, kept inside the range the setting offers.
    pub fn interface_font_size(&self) -> f32 {
        font_size(
            self.interface_font_size,
            DEFAULT_INTERFACE_FONT_SIZE,
            INTERFACE_FONT_SIZE_MIN,
            INTERFACE_FONT_SIZE_MAX,
        )
    }

    /// The code font size, kept inside the range the setting offers.
    pub fn code_font_size(&self) -> f32 {
        font_size(
            self.code_font_size,
            DEFAULT_CODE_FONT_SIZE,
            CODE_FONT_SIZE_MIN,
            CODE_FONT_SIZE_MAX,
        )
    }

    /// The terminal font size, kept inside the range the setting offers.
    pub fn terminal_font_size(&self) -> f32 {
        font_size(
            self.terminal_font_size,
            DEFAULT_TERMINAL_FONT_SIZE,
            TERMINAL_FONT_SIZE_MIN,
            TERMINAL_FONT_SIZE_MAX,
        )
    }

    /// The zoom, kept inside the levels the setting offers.
    pub fn zoom(&self) -> f32 {
        self.zoom
            .clamp(ZOOM_LEVELS[0], ZOOM_LEVELS[ZOOM_LEVELS.len() - 1])
    }

    /// The zoom one level in (`steps` > 0) or out (< 0) from the current
    /// one; a zoom between levels moves to the nearest level that way.
    pub fn zoom_step(&self, steps: i32) -> f32 {
        let zoom = self.zoom();
        let next = if steps > 0 {
            ZOOM_LEVELS
                .iter()
                .copied()
                .find(|&level| level > zoom + 1e-3)
        } else {
            ZOOM_LEVELS
                .iter()
                .rev()
                .copied()
                .find(|&level| level < zoom - 1e-3)
        };
        next.unwrap_or(zoom)
    }

    pub fn is_favorite(&self, model: &str) -> bool {
        self.favorite_models.iter().any(|m| m == model)
    }

    /// Stars `model` (added last) or unstars it.
    pub fn toggle_favorite(&mut self, model: &str) {
        if self.is_favorite(model) {
            self.favorite_models.retain(|m| m != model);
        } else {
            self.favorite_models.push(model.to_owned());
        }
    }

    /// Whether the composer shows `provider`'s usage windows inline.
    pub fn shows_usage(&self, provider: &str) -> bool {
        self.usage_inline.iter().any(|p| p == provider)
    }

    pub fn set_shows_usage(&mut self, provider: &str, show: bool) {
        self.usage_inline.retain(|p| p != provider);
        if show {
            self.usage_inline.push(provider.to_owned());
        }
    }
}

/// A font size clamped to `min..=max`; a non-number (a hand-edited file)
/// is the default.
fn font_size(value: f32, default: f32, min: f32, max: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        default
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_fields_take_their_defaults() {
        let ui: UiSettings = serde_json::from_value(json!({})).unwrap();
        assert_eq!(ui, UiSettings::default());
        assert_eq!(ui.workflow, Workflow::Inbox);
        assert_eq!(ui.status_indicators, StatusIndicators::Dots);
        assert_eq!(ui.agent_order, AgentOrder::Spaces);
        assert!(ui.toasts && ui.sounds && ui.working_shelf && ui.math_rendering);
        assert_eq!(ui.text_measure, DEFAULT_TEXT_MEASURE);
        assert_eq!((ui.zoom, ui.ui_font.as_deref()), (1., None));
        assert_eq!(
            (
                ui.interface_font_size,
                ui.code_font_size,
                ui.terminal_font_size
            ),
            (
                DEFAULT_INTERFACE_FONT_SIZE,
                DEFAULT_CODE_FONT_SIZE,
                DEFAULT_TERMINAL_FONT_SIZE
            )
        );
        assert!(!ui.is_expanded(Shelf::Settled));

        let ui: UiSettings =
            serde_json::from_value(json!({"workflow": "herdr", "sounds": false})).unwrap();
        assert_eq!(ui.workflow, Workflow::Herdr);
        assert!(!ui.sounds);
        assert!(ui.toasts);
    }

    #[test]
    fn the_measure_stays_in_range() {
        let mut ui = UiSettings {
            text_measure: 10.,
            ..Default::default()
        };
        assert_eq!(ui.measure(), TEXT_MEASURE_MIN);
        ui.text_measure = 200.;
        assert_eq!(ui.measure(), TEXT_MEASURE_MAX);
        ui.text_measure = 42.;
        assert_eq!(ui.measure(), 42.);
    }

    #[test]
    fn font_sizes_stay_in_range() {
        let ui = UiSettings {
            interface_font_size: 4.,
            code_font_size: 40.,
            terminal_font_size: 11.,
            ..UiSettings::default()
        };
        assert_eq!(ui.interface_font_size(), INTERFACE_FONT_SIZE_MIN);
        assert_eq!(ui.code_font_size(), CODE_FONT_SIZE_MAX);
        assert_eq!(ui.terminal_font_size(), 11.);
    }

    #[test]
    fn zoom_steps_through_the_levels_and_stops_at_the_ends() {
        let mut ui = UiSettings::default();
        assert_eq!(ui.zoom_step(1), 1.1);
        assert_eq!(ui.zoom_step(-1), 0.9);
        // Between levels: the nearest level in that direction.
        ui.zoom = 1.2;
        assert_eq!(ui.zoom_step(1), 1.25);
        assert_eq!(ui.zoom_step(-1), 1.1);
        ui.zoom = 2.;
        assert_eq!(ui.zoom_step(1), 2.);
        ui.zoom = 9.;
        assert_eq!(ui.zoom(), 2.);
        ui.zoom = 0.75;
        assert_eq!(ui.zoom_step(-1), 0.75);
    }

    #[test]
    fn round_trips_in_camel_case() {
        let mut ui = UiSettings {
            workflow: Workflow::Herdr,
            working_shelf: false,
            status_indicators: StatusIndicators::Symbols,
            agent_order: AgentOrder::Attention,
            toasts: false,
            sounds: true,
            math_rendering: false,
            expanded_shelves: Vec::new(),
            text_measure: 42.,
            ui_font: Some("Inter".into()),
            mono_font: None,
            interface_font_size: 15.,
            code_font_size: 13.,
            terminal_font_size: 12.,
            favorite_models: Vec::new(),
            usage_inline: Vec::new(),
            dictation_model: Some("small.en".into()),
            device_name: Some("revolver".into()),
            zoom: 1.25,
        };
        ui.set_expanded(Shelf::Settled, true);
        ui.set_expanded(Shelf::Working, true);
        ui.set_expanded(Shelf::Working, false);
        ui.toggle_favorite("a/one");
        ui.set_shows_usage("anthropic", true);
        ui.set_shows_usage("opencode-go", true);
        ui.set_shows_usage("opencode-go", false);
        ui.toggle_favorite("b/two");
        ui.toggle_favorite("b/two");
        let value = serde_json::to_value(&ui).unwrap();
        assert_eq!(
            value,
            json!({"workflow": "herdr", "workingShelf": false, "statusIndicators": "symbols",
                "agentOrder": "attention", "toasts": false, "sounds": true, "mathRendering": false,
                "expandedShelves": ["settled"], "textMeasure": 42.0, "uiFont": "Inter",
                "interfaceFontSize": 15.0, "codeFontSize": 13.0, "terminalFontSize": 12.0,
                "favoriteModels": ["a/one"], "usageInline": ["anthropic"],
                "dictationModel": "small.en", "deviceName": "revolver", "zoom": 1.25})
        );
        assert_eq!(serde_json::from_value::<UiSettings>(value).unwrap(), ui);
        assert!(ui.is_expanded(Shelf::Settled));
        assert!(!ui.is_expanded(Shelf::Working));
        assert!(ui.is_favorite("a/one") && !ui.is_favorite("b/two"));
        assert!(ui.shows_usage("anthropic") && !ui.shows_usage("opencode-go"));
    }
}
