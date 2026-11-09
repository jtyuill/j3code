//! The herdr workflow of the thread list: every thread is an agent with a
//! state glyph, grouped into spaces whose header rolls up the most urgent
//! state. It reads only thread status; it has no triage state of its own.

use std::cmp::Reverse;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::EnvThread;
use crate::entities::Thread;
use crate::inbox::{self, Status, parse_time};

/// An agent's state, most urgent first by [`AgentStatus::priority`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentStatus {
    /// The agent is on a turn.
    Working,
    /// The agent needs the user: an open request or a failed turn.
    Blocked,
    /// The agent finished a turn the user has not seen.
    Done,
    /// Nothing new.
    Idle,
    /// The thread has no turn yet.
    Unknown,
}

impl AgentStatus {
    /// Rollup priority: Blocked 4, Done 3, Working 2, Idle 1, Unknown 0.
    pub fn priority(self) -> u8 {
        match self {
            Self::Blocked => 4,
            Self::Done => 3,
            Self::Working => 2,
            Self::Idle => 1,
            Self::Unknown => 0,
        }
    }

    /// The state glyph in the chosen indicator style.
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

    pub fn label(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Idle => "idle",
            Self::Unknown => "unknown",
        }
    }

    /// The key that filters the agent list to this state; Unknown has none.
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

/// How state glyphs are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StatusIndicators {
    /// Filled and hollow dots, coloured by state.
    #[default]
    Dots,
    /// A distinct symbol per state.
    Symbols,
}

/// How the agent list is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentOrder {
    /// Grouped by space.
    #[default]
    Spaces,
    /// Most urgent first.
    Attention,
}

/// The agent state of a thread, from its Inbox status: Input, Failed and
/// Limited are Blocked; Working is Working; Ready is Unknown with no turn
/// yet, Done when unread, else Idle.
pub fn agent_status(thread: &Thread) -> AgentStatus {
    match inbox::status(thread) {
        Status::Input | Status::Failed | Status::Limited => AgentStatus::Blocked,
        Status::Working => AgentStatus::Working,
        Status::Ready if thread.latest_turn.is_none() => AgentStatus::Unknown,
        Status::Ready if inbox::is_unread(thread) => AgentStatus::Done,
        Status::Ready => AgentStatus::Idle,
    }
}

/// The most urgent of `statuses`; Unknown when empty.
pub fn rollup(statuses: impl IntoIterator<Item = AgentStatus>) -> AgentStatus {
    statuses
        .into_iter()
        .max_by_key(|status| status.priority())
        .unwrap_or(AgentStatus::Unknown)
}

/// When the agent's state last changed: the latest of the last user
/// message, the latest turn's request, start and end, and the open request's
/// opening; else the thread's creation. Visits don't count.
pub fn state_changed_at(thread: &Thread) -> Option<OffsetDateTime> {
    let turn = thread.latest_turn.as_ref();
    [
        thread.latest_user_message_at.as_deref(),
        turn.map(|turn| turn.requested_at.as_str()),
        turn.and_then(|turn| turn.started_at.as_deref()),
        turn.and_then(|turn| turn.ended_at.as_deref()),
        thread
            .pending_request
            .as_ref()
            .map(|request| request.opened_at.as_str()),
    ]
    .into_iter()
    .filter_map(parse_time)
    .max()
    .or_else(|| parse_time(Some(&thread.created_at)))
}

/// Most urgent first, then most recently changed, then environment, then
/// thread id.
pub fn sort_agents_by_attention(threads: &mut [EnvThread<'_>]) {
    threads.sort_by_cached_key(|&row| {
        (
            Reverse(agent_status(row.thread).priority()),
            Reverse(state_changed_at(row.thread)),
            row.environment,
            row.thread.id.as_str(),
        )
    });
}

/// By `space_rank`, then most recently changed, then environment, then
/// thread id.
pub fn sort_agents_by_space(
    threads: &mut [EnvThread<'_>],
    space_rank: impl Fn(&EnvThread<'_>) -> usize,
) {
    threads.sort_by_cached_key(|row| {
        let row = *row;
        (
            space_rank(&row),
            Reverse(state_changed_at(row.thread)),
            row.environment,
            row.thread.id.as_str(),
        )
    });
}

/// A desktop notification for an agent's state change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentNotification {
    /// The agent became blocked on the user.
    NeedsAttention,
    /// The agent stopped working while the user was not watching it.
    Finished,
}

/// The notification for a change from `previous` to `next` (Done counts as
/// Idle): becoming Blocked needs attention, even while `watching`; going from
/// Working or Blocked to Idle while not watching is Finished; anything else
/// notifies nothing.
pub fn agent_notification(
    previous: AgentStatus,
    next: AgentStatus,
    watching: bool,
) -> Option<AgentNotification> {
    let settle = |status| {
        if status == AgentStatus::Done {
            AgentStatus::Idle
        } else {
            status
        }
    };
    let (previous, next) = (settle(previous), settle(next));
    if previous == next {
        return None;
    }
    match (previous, next) {
        (_, AgentStatus::Blocked) => Some(AgentNotification::NeedsAttention),
        (AgentStatus::Working | AgentStatus::Blocked, AgentStatus::Idle) if !watching => {
            Some(AgentNotification::Finished)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::inbox::fixtures::{at, min, request, thread, turn};

    #[test]
    fn statuses_from_rows() {
        let mut limited = turn("failed", -10, Some(-5));
        limited["errorKind"] = json!("usage_limit");
        let cases = [
            (
                thread(
                    "a",
                    json!({"status": "waiting", "pendingRequest": request(-1)}),
                ),
                AgentStatus::Blocked,
            ),
            (
                thread(
                    "a",
                    json!({"status": "failed", "latestTurn": turn("failed", -10, Some(-5))}),
                ),
                AgentStatus::Blocked,
            ),
            (
                thread("a", json!({"status": "failed", "latestTurn": limited})),
                AgentStatus::Blocked,
            ),
            (
                thread(
                    "a",
                    json!({"status": "running", "latestTurn": turn("running", -1, None)}),
                ),
                AgentStatus::Working,
            ),
            (thread("a", json!({})), AgentStatus::Unknown),
            (
                thread(
                    "a",
                    json!({"latestTurn": turn("completed", -10, Some(-5)), "triage": {"visitedAt": min(-6)}}),
                ),
                AgentStatus::Done,
            ),
            (
                thread(
                    "a",
                    json!({"latestTurn": turn("completed", -10, Some(-5)), "triage": {"visitedAt": min(-1)}}),
                ),
                AgentStatus::Idle,
            ),
            (
                thread("a", json!({"latestTurn": turn("stopped", -10, Some(-5))})),
                AgentStatus::Idle,
            ),
        ];
        for (row, expected) in cases {
            assert_eq!(agent_status(&row), expected, "{row:?}");
        }
    }

    #[test]
    fn glyphs_labels_and_filter_keys() {
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
        let keys = [Blocked, Working, Idle, Done, Unknown].map(AgentStatus::filter_key);
        assert_eq!(keys, [Some('b'), Some('w'), Some('i'), Some('d'), None]);
    }

    #[test]
    fn settings_serialize_lowercase_with_defaults() {
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
    fn rollup_takes_the_most_urgent() {
        use AgentStatus::*;
        assert_eq!(rollup([]), Unknown);
        assert_eq!(rollup([Idle, Working, Done]), Done);
        assert_eq!(rollup([Done, Blocked, Idle]), Blocked);
        assert_eq!(rollup([Unknown, Idle]), Idle);
        assert_eq!(rollup([Working, Unknown]), Working);
    }

    #[test]
    fn state_changes_follow_turns_and_requests_but_not_visits() {
        let created = thread("a", json!({"triage": {"visitedAt": min(5)}}));
        assert_eq!(state_changed_at(&created), Some(at(-1000)));
        let mut started = turn("running", -10, None);
        started["startedAt"] = json!(min(-9));
        let running = thread(
            "a",
            json!({"status": "running", "latestUserMessageAt": min(-11), "latestTurn": started}),
        );
        assert_eq!(state_changed_at(&running), Some(at(-9)));
        let asking = thread(
            "a",
            json!({"status": "waiting", "latestTurn": turn("waiting", -10, None), "pendingRequest": request(-2)}),
        );
        assert_eq!(state_changed_at(&asking), Some(at(-2)));
        let ended = thread("a", json!({"latestTurn": turn("completed", -10, Some(-1))}));
        assert_eq!(state_changed_at(&ended), Some(at(-1)));
    }

    fn ids(rows: &[EnvThread<'_>]) -> Vec<String> {
        rows.iter()
            .map(|row| format!("{}/{}", row.environment, row.thread.id))
            .collect()
    }

    #[test]
    fn sorts_by_attention() {
        let threads = [
            thread(
                "idle",
                json!({"latestTurn": turn("completed", -10, Some(-1))}),
            ),
            thread("blocked-old", json!({"pendingRequest": request(-30)})),
            thread("blocked-new", json!({"pendingRequest": request(-3)})),
            thread(
                "working",
                json!({"status": "running", "latestTurn": turn("running", -2, None)}),
            ),
            thread("unknown", json!({})),
        ];
        let tie = thread("blocked-new", json!({"pendingRequest": request(-3)}));
        let mut rows: Vec<EnvThread<'_>> = threads
            .iter()
            .map(|thread| EnvThread {
                environment: "e2",
                thread,
            })
            .collect();
        rows.push(EnvThread {
            environment: "e1",
            thread: &tie,
        });
        sort_agents_by_attention(&mut rows);
        assert_eq!(
            ids(&rows),
            [
                "e1/blocked-new",
                "e2/blocked-new",
                "e2/blocked-old",
                "e2/working",
                "e2/idle",
                "e2/unknown"
            ]
        );
    }

    #[test]
    fn sorts_by_space() {
        let threads = [
            thread(
                "a",
                json!({"projectId": "two", "latestTurn": turn("completed", -10, Some(-1))}),
            ),
            thread(
                "b",
                json!({"projectId": "one", "latestTurn": turn("completed", -10, Some(-5))}),
            ),
            thread(
                "c",
                json!({"projectId": "one", "latestTurn": turn("completed", -10, Some(-2))}),
            ),
            thread(
                "d",
                json!({"projectId": "two", "latestTurn": turn("completed", -10, Some(-1))}),
            ),
        ];
        let mut rows: Vec<EnvThread<'_>> = threads
            .iter()
            .map(|thread| EnvThread {
                environment: "e",
                thread,
            })
            .collect();
        sort_agents_by_space(&mut rows, |row| {
            if row.thread.project_id == "one" { 0 } else { 1 }
        });
        assert_eq!(ids(&rows), ["e/c", "e/b", "e/a", "e/d"]);
    }

    #[test]
    fn notifications() {
        use AgentNotification::*;
        use AgentStatus::*;
        assert_eq!(agent_notification(Working, Working, false), None);
        assert_eq!(
            agent_notification(Done, Idle, false),
            None,
            "Done counts as Idle"
        );
        assert_eq!(
            agent_notification(Working, Blocked, false),
            Some(NeedsAttention)
        );
        assert_eq!(
            agent_notification(Idle, Blocked, true),
            Some(NeedsAttention),
            "even while watching"
        );
        assert_eq!(agent_notification(Working, Done, false), Some(Finished));
        assert_eq!(agent_notification(Blocked, Idle, false), Some(Finished));
        assert_eq!(agent_notification(Working, Idle, true), None, "watching");
        assert_eq!(agent_notification(Idle, Working, false), None);
        assert_eq!(agent_notification(Unknown, Idle, false), None);
        assert_eq!(agent_notification(Blocked, Working, false), None);
    }
}
