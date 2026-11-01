//! Thread workflows for the sidebar, as pure functions over shell rows.
//!
//! - **T3 mode** ports t3code's settle workflow (t3code @ 8760d063): the
//!   status ladder and unread marks, snooze rules and menus, pin/active
//!   order keys, the Pinned → Active → Working → Snoozed → Settled
//!   sections and drag & drop planning.
//! - **herdr mode** ports herdr's agent states (herdr v0.9.5): Working /
//!   Blocked / Done / Idle / Unknown, attention priority, space rollup,
//!   ordering and notifications.
//!
//! Every list helper takes [`EnvThread`]s so lists merge across
//! environments. Instants are UTC [`time::OffsetDateTime`]s; local calendar
//! math (snooze menus) takes a [`TimeZone`], usually `TimeZone::system()`.

mod herdr;
mod order;
mod sections;
mod snooze;
mod status;
mod view;

#[cfg(test)]
mod fixtures;

use j3_protocol::t3::orchestration::ThreadShell;
pub use jiff::tz::TimeZone;

pub use herdr::{
    AgentNotification, AgentOrder, AgentStatus, StatusIndicators, agent_notification, agent_status,
    attention_priority, rollup, sort_agents_by_attention, sort_agents_by_space, state_changed_at,
};
pub use order::{OrderAssignment, order_key_between, plan_reorder, spread_order_keys};
pub use sections::{
    DropInput, DropPlan, DropSection, DropTarget, DropVerb, ListItem, ListMarker, T3Layout,
    T3Section, drop_target, drop_verb, forward_target, plan_drop, sort_active,
    sort_active_by_return, sort_pinned, sort_settled, sort_snoozed, sort_working, t3_layout,
    t3_section,
};
pub use snooze::{
    CustomSnooze, DEFAULT_SNOOZE_PRESETS, SnoozeDay, SnoozePreset, SnoozePresetSetting, SnoozeUnit,
    pick_snooze_preset, pick_snooze_preset_with, resolve_custom_snooze, resolve_snooze_presets,
    resolve_snooze_presets_with,
};
pub use status::{
    QUEUED_TURN_START_GRACE, ThreadStatus, can_snooze, effective_snoozed, format_working_duration,
    has_queued_turn_start, has_unseen_completion, has_unseen_failure, is_listed, is_working,
    next_snooze_wake_after, raised_hand_while_snoozed, settled_timestamp, should_recede,
    snooze_wake_label, thread_status, woke_at, woke_unseen_at, working_started_at,
};

/// A thread with the key of the environment (connection) it lives in.
/// Thread ids are unique only within an environment.
#[derive(Debug, Clone, Copy)]
pub struct EnvThread<'a> {
    pub environment: &'a str,
    pub thread: &'a ThreadShell,
}
