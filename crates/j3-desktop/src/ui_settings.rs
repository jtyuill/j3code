//! The desktop's UI preferences, stored as the `"ui"` object of
//! `desktop.json`. Every field has a serde default, so files written before a
//! field existed keep loading.

use j3_client::workflow::{AgentOrder, StatusIndicators};
use serde::{Deserialize, Serialize};

/// Which thread workflow the sidebar follows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Workflow {
    /// t3code's settle workflow: Pinned, Active, then the Working, Snoozed
    /// and Settled shelves.
    #[default]
    T3,
    /// herdr's agent workflow: Spaces with rolled-up attention, then every
    /// agent with its state glyph.
    Herdr,
}

/// A bottom-anchored shelf of the T3 sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Shelf {
    Working,
    Snoozed,
    Settled,
}

/// The `"ui"` object of `desktop.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UiSettings {
    pub workflow: Workflow,
    /// t3's `sidebarWorkingShelfEnabled`: running threads leave Active for
    /// the Working shelf. Off, Active keeps its own (rearrangeable) order.
    pub working_shelf: bool,
    /// herdr's `ui.status_indicators`.
    pub status_indicators: StatusIndicators,
    /// herdr's agent panel sort.
    pub agent_order: AgentOrder,
    /// In-app toasts when a thread needs attention or finishes.
    pub toasts: bool,
    /// Sounds for the same events.
    pub sounds: bool,
    /// T3 shelves the user expanded; like t3 web, every shelf starts
    /// collapsed.
    pub expanded_shelves: Vec<Shelf>,
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
            expanded_shelves: Vec::new(),
        }
    }
}

impl UiSettings {
    pub fn is_expanded(&self, shelf: Shelf) -> bool {
        self.expanded_shelves.contains(&shelf)
    }

    pub fn set_expanded(&mut self, shelf: Shelf, expanded: bool) {
        self.expanded_shelves.retain(|s| *s != shelf);
        if expanded {
            self.expanded_shelves.push(shelf);
        }
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
        assert_eq!(ui.workflow, Workflow::T3);
        assert_eq!(ui.status_indicators, StatusIndicators::Dots);
        assert_eq!(ui.agent_order, AgentOrder::Spaces);
        assert!(ui.toasts && ui.sounds && ui.working_shelf);
        assert!(!ui.is_expanded(Shelf::Settled));

        let ui: UiSettings =
            serde_json::from_value(json!({"workflow": "herdr", "sounds": false})).unwrap();
        assert_eq!(ui.workflow, Workflow::Herdr);
        assert!(!ui.sounds);
        assert!(ui.toasts);
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
            expanded_shelves: Vec::new(),
        };
        ui.set_expanded(Shelf::Settled, true);
        ui.set_expanded(Shelf::Working, true);
        ui.set_expanded(Shelf::Working, false);
        let value = serde_json::to_value(&ui).unwrap();
        assert_eq!(
            value,
            json!({"workflow": "herdr", "workingShelf": false, "statusIndicators": "symbols",
                "agentOrder": "attention", "toasts": false, "sounds": true,
                "expandedShelves": ["settled"]})
        );
        assert_eq!(serde_json::from_value::<UiSettings>(value).unwrap(), ui);
        assert!(ui.is_expanded(Shelf::Settled));
        assert!(!ui.is_expanded(Shelf::Working));
    }
}
