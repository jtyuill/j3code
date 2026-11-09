//! The Inbox workflow of the thread list (`docs/triage.md`, with its §12.1
//! decisions): per-thread status, unread and Woke markers, sections and their
//! order, order keys, drag and drop plans, and snooze presets.
//!
//! Everything here is a pure function of the thread rows, the clock and the
//! caller's settings, so the desktop and the phone derive the same list.

mod order;
mod sections;
mod snooze;
mod status;

pub use jiff::tz::TimeZone;
pub use order::{OrderAssignment, is_valid_order, order_between, plan_reorder, spread_orders};
pub use sections::{
    DropInput, DropPlan, DropSection, DropTarget, DropVerb, Layout, ListItem, ListMarker, Section,
    drop_target, drop_verb, forward_target, is_listed, layout, plan_drop, section,
};
pub use snooze::{
    CustomSnooze, DEFAULT_SNOOZE_PRESETS, SnoozeDay, SnoozePreset, SnoozePresetSetting, SnoozeUnit,
    custom_snooze, pick_snooze_preset, pick_snooze_preset_with, snooze_presets,
    snooze_presets_with,
};
pub use status::{
    Status, can_settle, can_snooze, completed_at, format_working, is_snoozed, is_unread,
    is_working, raised_hand, recedes, settled_time, snooze_countdown, status, wake_at, woke_at,
    working_since,
};

pub(crate) use status::parse_time;

/// Thread rows and times for the workflow tests.
#[cfg(test)]
pub(crate) mod fixtures {
    use serde_json::{Value, json};
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;

    use crate::entities::Thread;

    /// The tests' "now": 2026-11-09 12:00 UTC.
    pub fn base() -> OffsetDateTime {
        OffsetDateTime::parse("2026-11-09T12:00:00Z", &Rfc3339).unwrap()
    }

    /// `base()` moved by `minutes`.
    pub fn at(minutes: i64) -> OffsetDateTime {
        base() + time::Duration::minutes(minutes)
    }

    /// `at(minutes)` as a wire timestamp.
    pub fn min(minutes: i64) -> String {
        at(minutes).format(&Rfc3339).unwrap()
    }

    /// A minimal idle row created at `min(-1000)`, with `patch` merged over it
    /// (objects merge recursively, `null` removes a field).
    pub fn thread(id: &str, patch: Value) -> Thread {
        let mut row = json!({
            "id": id,
            "projectId": "p",
            "title": id,
            "createdAt": min(-1000),
            "updatedAt": min(-1000),
            "status": "idle",
            "model": {"provider": "anthropic", "id": "m"},
            "mode": "default",
            "queued": 0,
            "triage": {"bumpedAt": min(-1000)},
        });
        merge(&mut row, patch);
        serde_json::from_value(row).unwrap()
    }

    /// A latest turn with `status`, requested at `requested` and ended at
    /// `ended` (minutes from `base()`).
    pub fn turn(status: &str, requested: i64, ended: Option<i64>) -> Value {
        let mut turn = json!({"id": "t", "status": status, "requestedAt": min(requested)});
        if let Some(ended) = ended {
            turn["endedAt"] = json!(min(ended));
        }
        turn
    }

    /// A pending request opened at `opened` (minutes from `base()`).
    pub fn request(opened: i64) -> Value {
        json!({"id": "r", "kind": "ask", "openedAt": min(opened)})
    }

    fn merge(target: &mut Value, patch: Value) {
        match (target, patch) {
            (Value::Object(target), Value::Object(patch)) => {
                for (key, value) in patch {
                    if value.is_null() {
                        target.remove(&key);
                    } else {
                        merge(target.entry(key).or_insert(Value::Null), value);
                    }
                }
            }
            (target, patch) => *target = patch,
        }
    }
}
