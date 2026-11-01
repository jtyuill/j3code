//! herdr mode: herdr's agent states, attention priority, rollup, ordering
//! and notifications, read off j3 thread shells (herdr v0.9.5:
//! `src/app/api_helpers.rs:1-9`, `src/workspace/aggregate.rs:50-75`,
//! `src/app/agent_view.rs:46-83`, `src/client/shell.rs:178-237`,
//! `src/app/actions.rs:22-93`).
//!
//! herdr detects states by reading terminal screens; j3 knows them from the
//! shell. Blocked is a pending question or a failed run (herdr has no
//! failure state); Done is t3's unread completion; Unknown is a thread that
//! never ran.

use std::cmp::Reverse;

use j3_protocol::t3::orchestration::ThreadShell;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::{
    EnvThread,
    status::{ThreadStatus, status_of, unseen_completion},
    view::{View, flat, instant, ms},
};

/// An agent's state (herdr `AgentStatus`, `src/app/api_helpers.rs:97-107`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentStatus {
    /// A run is active.
    Working,
    /// Waits on the user: a pending question, or the run failed.
    Blocked,
    /// Finished and not yet seen.
    Done,
    /// Finished and seen.
    Idle,
    /// Never ran.
    Unknown,
}

/// herdr's `status_indicators` setting (`src/config/model.rs:112-125`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StatusIndicators {
    /// `●` `○` `·`
    #[default]
    Dots,
    /// `◐` `×` `✓` `○` `·`
    Symbols,
}

/// herdr's agent panel sort (`AgentPanelSort`, `src/app/agent_view.rs:46-83`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentOrder {
    /// By space (project), then recency.
    #[default]
    Spaces,
    /// By attention priority, then the latest state change.
    Attention,
}

impl AgentStatus {
    /// Blocked 4, Done 3, Working 2, Idle 1, Unknown 0
    /// (`tab_attention_priority`, `src/app/api_helpers.rs:1-9`).
    pub fn priority(self) -> u8 {
        match self {
            Self::Blocked => 4,
            Self::Done => 3,
            Self::Working => 2,
            Self::Idle => 1,
            Self::Unknown => 0,
        }
    }

    /// The row glyph (`status_icon`, `src/client/shell.rs:178-197`).
    pub fn glyph(self, indicators: StatusIndicators) -> &'static str {
        match (indicators, self) {
            (StatusIndicators::Dots, Self::Working | Self::Blocked | Self::Done) => "●",
            (StatusIndicators::Symbols, Self::Working) => "◐",
            (StatusIndicators::Symbols, Self::Blocked) => "×",
            (StatusIndicators::Symbols, Self::Done) => "✓",
            (_, Self::Idle) => "○",
            (_, Self::Unknown) => "·",
        }
    }

    /// `working`, `blocked`, `done`, `idle`, `unknown`
    /// (`status_text`, `src/client/shell.rs:214-223`).
    pub fn label(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Idle => "idle",
            Self::Unknown => "unknown",
        }
    }

    /// The goto picker's single-key filter: `b`, `w`, `i`, `d`; Unknown has
    /// none (keyboard.mdx "The Goto picker").
    pub fn filter_key(self) -> Option<char> {
        match self {
            Self::Blocked => Some('b'),
            Self::Working => Some('w'),
            Self::Idle => Some('i'),
            Self::Done => Some('d'),
            Self::Unknown => None,
        }
    }
}

/// The thread's agent state.
pub fn agent_status(thread: &ThreadShell) -> AgentStatus {
    let view = View::of(thread);
    match status_of(&view) {
        ThreadStatus::Approval
        | ThreadStatus::Input
        | ThreadStatus::Request
        | ThreadStatus::Failed
        | ThreadStatus::Limited => AgentStatus::Blocked,
        ThreadStatus::Working | ThreadStatus::Waiting => AgentStatus::Working,
        ThreadStatus::Ready if view.runtime.is_none() => AgentStatus::Unknown,
        ThreadStatus::Ready if unseen_completion(&view) => AgentStatus::Done,
        ThreadStatus::Ready => AgentStatus::Idle,
    }
}

/// [`AgentStatus::priority`] of the thread.
pub fn attention_priority(thread: &ThreadShell) -> u8 {
    agent_status(thread).priority()
}

/// A space's state: its most urgent agent, Unknown when empty
/// (`Workspace::aggregate_state`, `src/workspace/aggregate.rs:61-75`).
pub fn rollup(statuses: impl IntoIterator<Item = AgentStatus>) -> AgentStatus {
    statuses
        .into_iter()
        .max_by_key(|status| status.priority())
        .unwrap_or(AgentStatus::Unknown)
}

/// When the thread's state last changed, for herdr's
/// `last_agent_state_change_seq`: the latest of the message that started
/// work, the run's request, start and completion, the current activity's
/// start and a pending question; else its creation. Visits do not count.
pub fn state_changed_at(thread: &ThreadShell) -> Option<OffsetDateTime> {
    state_changed_iso(thread).and_then(instant)
}

fn state_changed_iso(thread: &ThreadShell) -> Option<&str> {
    [
        thread.latest_user_message_at.as_deref(),
        flat(&thread.latest_run_requested_at),
        flat(&thread.latest_run_started_at),
        flat(&thread.latest_run_completed_at),
        flat(&thread.activity_run_started_at),
        thread
            .pending_runtime_request
            .as_ref()
            .map(|request| request.created_at.as_str()),
    ]
    .into_iter()
    .flatten()
    .filter_map(|at| ms(at).map(|ms| (ms, at)))
    .max_by_key(|(ms, _)| *ms)
    .map(|(_, at)| at)
    .or_else(|| ms(&thread.created_at).map(|_| thread.created_at.as_str()))
}

fn changed_ms(thread: &ThreadShell) -> i64 {
    state_changed_iso(thread).and_then(ms).unwrap_or(0)
}

/// Attention priority descending, then the latest state change
/// (`apply_agent_view` with `AgentPanelSort::Priority`,
/// `src/app/agent_view.rs:69-82`); ties by environment, then thread id.
pub fn sort_agents_by_attention(threads: &mut [EnvThread<'_>]) {
    threads.sort_by_cached_key(|entry| {
        (
            Reverse(attention_priority(entry.thread)),
            Reverse(changed_ms(entry.thread)),
            entry.environment,
            entry.thread.id.as_str(),
        )
    });
}

/// By space (`space_rank`, the project's position in the Spaces list), then
/// most recent state change first; ties by environment, then thread id.
pub fn sort_agents_by_space(
    threads: &mut [EnvThread<'_>],
    space_rank: impl Fn(&EnvThread<'_>) -> usize,
) {
    threads.sort_by_cached_key(|entry| {
        (
            space_rank(entry),
            Reverse(changed_ms(entry.thread)),
            entry.environment,
            entry.thread.id.as_str(),
        )
    });
}

/// A toast or sound worth raising for a state change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentNotification {
    /// "«title» needs attention" (herdr `ToastKind::NeedsAttention`,
    /// `Sound::Request`).
    NeedsAttention,
    /// "«title» finished" (herdr `ToastKind::Finished`, `Sound::Done`).
    Finished,
}

/// The notification for a change from `previous` to `next`
/// (`notification_toast_for_state_change` /
/// `notification_sound_for_state_change`, `src/app/actions.rs:22-93`):
/// becoming Blocked always notifies, even while `watching` the thread; a
/// run finishing (Working or Blocked to Done or Idle) notifies only when
/// the user is not watching it.
pub fn agent_notification(
    previous: AgentStatus,
    next: AgentStatus,
    watching: bool,
) -> Option<AgentNotification> {
    /// herdr's detector state: Done and Idle are both `Idle`.
    fn raw(status: AgentStatus) -> AgentStatus {
        match status {
            AgentStatus::Done => AgentStatus::Idle,
            other => other,
        }
    }
    let (previous, next) = (raw(previous), raw(next));
    if previous == next {
        return None;
    }
    match next {
        AgentStatus::Blocked => Some(AgentNotification::NeedsAttention),
        AgentStatus::Idle
            if !watching && matches!(previous, AgentStatus::Working | AgentStatus::Blocked) =>
        {
            Some(AgentNotification::Finished)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::workflow::fixtures::{request, shell};

    fn running() -> ThreadShell {
        shell(
            "running",
            json!({ "latestRunId": "run-1", "activeRunId": "run-1", "status": "running" }),
        )
    }

    fn completed(id: &str, last_visited_at: &str) -> ThreadShell {
        shell(
            id,
            json!({
                "latestRunId": "run-1",
                "status": "completed",
                "latestRunCompletedAt": "2026-03-09T10:05:00.000Z",
                "lastVisitedAt": last_visited_at,
            }),
        )
    }

    #[test]
    fn statuses_from_shells() {
        assert_eq!(agent_status(&running()), AgentStatus::Working);
        let asking = shell(
            "asking",
            json!({ "latestRunId": "run-1", "status": "running", "pendingRuntimeRequest": request("user_input") }),
        );
        assert_eq!(agent_status(&asking), AgentStatus::Blocked);
        let failed = shell(
            "failed",
            json!({ "latestRunId": "run-1", "status": "failed" }),
        );
        assert_eq!(agent_status(&failed), AgentStatus::Blocked);
        assert_eq!(
            agent_status(&completed("done", "2026-03-09T10:00:00.000Z")),
            AgentStatus::Done
        );
        assert_eq!(
            agent_status(&completed("idle", "2026-03-09T10:06:00.000Z")),
            AgentStatus::Idle
        );
        assert_eq!(agent_status(&shell("new", json!({}))), AgentStatus::Unknown);
        // Idle on awaited background work is still working.
        let waiting = shell(
            "waiting",
            json!({ "latestRunId": "run-1", "status": "completed", "pendingBackgroundTasks": [{ "taskId": "t", "kind": "subagent" }] }),
        );
        assert_eq!(agent_status(&waiting), AgentStatus::Working);
    }

    #[test]
    fn priorities_follow_tab_attention_priority() {
        // src/app/api_helpers.rs:1-9, src/client/shell.rs:203-212
        let statuses = [
            AgentStatus::Blocked,
            AgentStatus::Done,
            AgentStatus::Working,
            AgentStatus::Idle,
            AgentStatus::Unknown,
        ];
        assert_eq!(statuses.map(AgentStatus::priority), [4, 3, 2, 1, 0]);
        assert_eq!(attention_priority(&running()), 2);
    }

    #[test]
    fn rollup_takes_the_most_urgent_agent() {
        // src/workspace/aggregate.rs:97-157
        assert_eq!(rollup([]), AgentStatus::Unknown);
        assert_eq!(rollup([AgentStatus::Unknown]), AgentStatus::Unknown);
        assert_eq!(
            rollup([AgentStatus::Idle, AgentStatus::Working]),
            AgentStatus::Working
        );
        assert_eq!(
            rollup([AgentStatus::Done, AgentStatus::Working]),
            AgentStatus::Done
        );
        assert_eq!(
            rollup([AgentStatus::Done, AgentStatus::Blocked, AgentStatus::Idle]),
            AgentStatus::Blocked
        );
    }

    #[test]
    fn glyphs_and_labels() {
        // src/client/shell.rs:178-223
        use AgentStatus::*;
        let all = [Working, Blocked, Done, Idle, Unknown];
        assert_eq!(
            all.map(|s| s.glyph(StatusIndicators::Dots)),
            ["●", "●", "●", "○", "·"]
        );
        assert_eq!(
            all.map(|s| s.glyph(StatusIndicators::Symbols)),
            ["◐", "×", "✓", "○", "·"]
        );
        assert_eq!(
            all.map(AgentStatus::label),
            ["working", "blocked", "done", "idle", "unknown"]
        );
        assert_eq!(
            all.map(AgentStatus::filter_key),
            [Some('w'), Some('b'), Some('d'), Some('i'), None]
        );
    }

    #[test]
    fn settings_serialize_lowercase_with_herdr_defaults() {
        assert_eq!(StatusIndicators::default(), StatusIndicators::Dots);
        assert_eq!(AgentOrder::default(), AgentOrder::Spaces);
        assert_eq!(
            serde_json::to_value(StatusIndicators::Symbols).unwrap(),
            json!("symbols")
        );
        assert_eq!(
            serde_json::from_value::<AgentOrder>(json!("attention")).unwrap(),
            AgentOrder::Attention
        );
    }

    #[test]
    fn attention_order_puts_blocked_then_done_first_then_recency() {
        // src/app/agent_view.rs:69-82
        let blocked = shell(
            "blocked",
            json!({ "latestRunId": "run-1", "status": "failed", "latestRunCompletedAt": "2026-03-09T09:00:00.000Z" }),
        );
        let done = completed("done", "2026-03-09T10:00:00.000Z");
        let idle_old = completed("idle-old", "2026-03-09T11:00:00.000Z");
        let idle_new = shell(
            "idle-new",
            json!({ "latestRunId": "run-1", "status": "completed", "latestRunCompletedAt": "2026-03-09T10:30:00.000Z", "lastVisitedAt": "2026-03-09T11:00:00.000Z" }),
        );
        let working = running();
        let mut entries: Vec<EnvThread<'_>> = [&idle_old, &working, &idle_new, &done, &blocked]
            .into_iter()
            .map(|thread| EnvThread {
                environment: "env",
                thread,
            })
            .collect();
        sort_agents_by_attention(&mut entries);
        let ids: Vec<&str> = entries.iter().map(|e| e.thread.id.as_str()).collect();
        assert_eq!(ids, ["blocked", "done", "running", "idle-new", "idle-old"]);
    }

    #[test]
    fn space_order_groups_by_space_then_recency() {
        let a_old = completed("a-old", "2026-03-09T11:00:00.000Z");
        let mut a_new = completed("a-new", "2026-03-09T11:00:00.000Z");
        a_new.latest_user_message_at = Some("2026-03-09T12:00:00.000Z".into());
        let mut b = running();
        b.project_id = "project-b".into();
        let mut entries: Vec<EnvThread<'_>> = [&b, &a_old, &a_new]
            .into_iter()
            .map(|thread| EnvThread {
                environment: "env",
                thread,
            })
            .collect();
        sort_agents_by_space(&mut entries, |entry| {
            usize::from(entry.thread.project_id == "project-b")
        });
        let ids: Vec<&str> = entries.iter().map(|e| e.thread.id.as_str()).collect();
        assert_eq!(ids, ["a-new", "a-old", "running"]);
        assert_eq!(
            state_changed_at(&a_new),
            Some(time::macros::datetime!(2026-03-09 12:00 UTC))
        );
    }

    #[test]
    fn notifications_follow_herdr_rules() {
        // src/app/actions.rs:3454-3476
        use AgentNotification::*;
        use AgentStatus::*;
        assert_eq!(
            agent_notification(Working, Blocked, true),
            Some(NeedsAttention)
        );
        assert_eq!(agent_notification(Working, Done, false), Some(Finished));
        assert_eq!(agent_notification(Working, Idle, true), None);
        assert_eq!(agent_notification(Unknown, Idle, false), None);
        assert_eq!(agent_notification(Blocked, Done, false), Some(Finished));
        assert_eq!(agent_notification(Blocked, Blocked, false), None);
        assert_eq!(agent_notification(Done, Idle, false), None);
        assert_eq!(agent_notification(Idle, Working, false), None);
    }
}
