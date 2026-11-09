//! Per-thread status, unread, snooze and Woke derivations
//! (`docs/triage.md` §3, §4, §7.2–7.5, with §12.1).

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::entities::{Settled, Thread, ThreadStatus, TurnErrorKind, TurnStatus};

/// A wire timestamp; absent, empty or unparseable reads as `None`.
pub(crate) fn parse_time(value: Option<&str>) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value?, &Rfc3339).ok()
}

/// The status of a thread row, first match wins (triage §3.2 after §12.1(8)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    /// omp is waiting on an open request (ask/select/confirm/input/editor).
    Input,
    /// A turn is starting, running, waiting or stopping.
    Working,
    /// The latest turn failed on a usage limit.
    Limited,
    /// The latest turn failed.
    Failed,
    /// Nothing to do: idle after a completed or stopped turn, or no turn yet.
    Ready,
}

impl Status {
    /// The row's status word: "Input", "Working", "Limited", "Failed", and for
    /// Ready "Done" when unread, else none.
    pub fn label(self, unread: bool) -> Option<&'static str> {
        match self {
            Self::Input => Some("Input"),
            Self::Working => Some("Working"),
            Self::Limited => Some("Limited"),
            Self::Failed => Some("Failed"),
            Self::Ready => unread.then_some("Done"),
        }
    }
}

fn is_active(status: ThreadStatus) -> bool {
    matches!(
        status,
        ThreadStatus::Starting
            | ThreadStatus::Running
            | ThreadStatus::Waiting
            | ThreadStatus::Stopping
    )
}

/// The thread's status: an open request reads Input; a starting, running,
/// waiting or stopping thread reads Working; a failed one reads Limited when
/// the latest turn hit a usage limit, else Failed; anything else is Ready.
pub fn status(thread: &Thread) -> Status {
    if thread.pending_request.is_some() {
        Status::Input
    } else if is_active(thread.status) {
        Status::Working
    } else if thread.status == ThreadStatus::Failed {
        let limited = thread
            .latest_turn
            .as_ref()
            .is_some_and(|turn| turn.error_kind == Some(TurnErrorKind::UsageLimit));
        if limited {
            Status::Limited
        } else {
            Status::Failed
        }
    } else {
        Status::Ready
    }
}

/// The latest turn's `ended_at` when that turn is completed, failed or stopped.
pub fn completed_at(thread: &Thread) -> Option<OffsetDateTime> {
    let turn = thread.latest_turn.as_ref()?;
    match turn.status {
        TurnStatus::Completed | TurnStatus::Failed | TurnStatus::Stopped => {
            parse_time(turn.ended_at.as_deref())
        }
        TurnStatus::Running | TurnStatus::Waiting => None,
    }
}

/// The thread finished a turn the user has not seen (triage §4.1): the latest
/// turn has a completion time `C`, the thread carries a non-empty
/// `visited_at`, and that visit is unparseable or earlier than `C`. A thread
/// never visited is never unread.
pub fn is_unread(thread: &Thread) -> bool {
    let Some(completed) = completed_at(thread) else {
        return false;
    };
    match thread.triage.visited_at.as_deref() {
        None | Some("") => false,
        visited => parse_time(visited).is_none_or(|visited| completed > visited),
    }
}

/// The thread is working (triage §5.2): no open request and a turn starting,
/// running, waiting or stopping.
pub fn is_working(thread: &Thread) -> bool {
    thread.pending_request.is_none() && is_active(thread.status)
}

/// When the current stretch of work started (triage §3.3): for a Working
/// thread the latest turn's start, else its request time; otherwise none.
pub fn working_since(thread: &Thread) -> Option<OffsetDateTime> {
    if status(thread) != Status::Working {
        return None;
    }
    let turn = thread.latest_turn.as_ref()?;
    parse_time(turn.started_at.as_deref()).or_else(|| parse_time(Some(&turn.requested_at)))
}

/// The working timer: under a minute `"59s"`, under an hour `"2m"`, else
/// `"1h 5m"`. Parts round down; negative durations read as zero.
pub fn format_working(elapsed: time::Duration) -> String {
    let seconds = elapsed.whole_seconds().max(0);
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h {}m", seconds / 3600, seconds % 3600 / 60)
    }
}

/// Whether the row is drawn quietly (triage §3.4). A selected row and an
/// Input row never recede. Otherwise Working recedes, Ready recedes unless
/// unread or woke, and Failed and Limited never do.
pub fn recedes(status: Status, unread: bool, woke: bool, selected: bool) -> bool {
    if selected {
        return false;
    }
    match status {
        Status::Input | Status::Failed | Status::Limited => false,
        Status::Working => true,
        Status::Ready => !(unread || woke),
    }
}

/// Snoozing is offered (triage §12.1(1)): no open request, nothing queued and
/// no turn starting.
pub fn can_snooze(thread: &Thread) -> bool {
    thread.pending_request.is_none()
        && thread.queued == 0
        && thread.status != ThreadStatus::Starting
}

/// Settling is offered (triage §12.1(3)): the thread is not starting,
/// running, waiting or stopping, nothing is queued and no request is open.
pub fn can_settle(thread: &Thread) -> bool {
    !is_active(thread.status) && thread.queued == 0 && thread.pending_request.is_none()
}

/// When the failed latest turn failed: its `ended_at`, else its request time.
fn failure_time(thread: &Thread) -> Option<OffsetDateTime> {
    let turn = thread.latest_turn.as_ref()?;
    parse_time(turn.ended_at.as_deref()).or_else(|| parse_time(Some(&turn.requested_at)))
}

/// "Failed after snoozing" (triage §12.1(2)): the thread failed, and either no
/// snooze time is set or the failure came after it.
fn failed_after_snoozing(thread: &Thread) -> bool {
    if thread.status != ThreadStatus::Failed {
        return false;
    }
    match parse_time(thread.triage.snoozed_at.as_deref()) {
        None => true,
        Some(snoozed) => failure_time(thread).is_some_and(|failed| failed > snoozed),
    }
}

/// A snoozed thread raises its hand (triage §7.2): it has an open request or
/// it failed after snoozing. Completions, new turns and ongoing work never do.
pub fn raised_hand(thread: &Thread) -> bool {
    thread.pending_request.is_some() || failed_after_snoozing(thread)
}

/// The thread is effectively snoozed (triage §12.1(2)): `snoozed_until`
/// parses, is after `now`, and the thread has not raised its hand. A
/// malformed wake time never hides a thread.
pub fn is_snoozed(thread: &Thread, now: OffsetDateTime) -> bool {
    parse_time(thread.triage.snoozed_until.as_deref()).is_some_and(|until| until > now)
        && !raised_hand(thread)
}

/// When the snooze ended (triage §7.4), only while `snoozed_until` parses:
/// the failure time if the thread failed after snoozing; else, with an open
/// request, the request's `opened_at` (else `snoozed_at`); else
/// `snoozed_until` once it is at or before `now`; else none.
pub fn wake_at(thread: &Thread, now: OffsetDateTime) -> Option<OffsetDateTime> {
    let until = parse_time(thread.triage.snoozed_until.as_deref())?;
    if failed_after_snoozing(thread)
        && let Some(failed) = failure_time(thread)
    {
        return Some(failed);
    }
    if let Some(request) = &thread.pending_request
        && let Some(opened) = parse_time(Some(&request.opened_at))
            .or_else(|| parse_time(thread.triage.snoozed_at.as_deref()))
    {
        return Some(opened);
    }
    (until <= now).then_some(until)
}

/// The Woke marker (triage §7.4): the thread is not settled, it has a wake
/// time `W`, and `visited_at` is absent, unparseable or earlier than `W`.
/// Returns `W`; visiting at or after it clears the marker.
pub fn woke_at(thread: &Thread, now: OffsetDateTime) -> Option<OffsetDateTime> {
    if thread.triage.settled == Some(Settled::Settled) {
        return None;
    }
    let wake = wake_at(thread, now)?;
    parse_time(thread.triage.visited_at.as_deref())
        .is_none_or(|visited| visited < wake)
        .then_some(wake)
}

/// The time a thread counts as settled for the Settled order (triage §5.3):
/// `settled_at`, else the latest of `latest_user_message_at` and the latest
/// turn's request, start and end, else `triage.bumped_at`.
pub fn settled_time(thread: &Thread) -> Option<OffsetDateTime> {
    parse_time(thread.triage.settled_at.as_deref())
        .or_else(|| last_activity(thread))
        .or_else(|| parse_time(Some(&thread.triage.bumped_at)))
}

/// The latest of `latest_user_message_at` and the latest turn's request,
/// start and end.
fn last_activity(thread: &Thread) -> Option<OffsetDateTime> {
    let turn = thread.latest_turn.as_ref();
    [
        thread.latest_user_message_at.as_deref(),
        turn.map(|turn| turn.requested_at.as_str()),
        turn.and_then(|turn| turn.started_at.as_deref()),
        turn.and_then(|turn| turn.ended_at.as_deref()),
    ]
    .into_iter()
    .filter_map(parse_time)
    .max()
}

/// The countdown on a snoozed row (triage §7.5): "now" when due, else the
/// time left rounded up: `"{m}m"` (at least 1) under an hour, `"{h}h"` under
/// a day, else `"{d}d"`.
pub fn snooze_countdown(until: OffsetDateTime, now: OffsetDateTime) -> String {
    const MINUTE: i128 = 60_000_000_000;
    const HOUR: i128 = 60 * MINUTE;
    const DAY: i128 = 24 * HOUR;
    let left = (until - now).whole_nanoseconds();
    let up = |unit: i128| (left + unit - 1) / unit;
    if left <= 0 {
        "now".to_owned()
    } else if left < HOUR {
        format!("{}m", up(MINUTE).max(1))
    } else if left < DAY {
        format!("{}h", up(HOUR))
    } else {
        format!("{}d", up(DAY))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use time::Duration;

    use super::*;
    use crate::inbox::fixtures::{at, base, min, request, thread, turn};

    #[test]
    fn an_open_request_reads_input_even_while_running() {
        let row = thread(
            "a",
            json!({"status": "running", "pendingRequest": request(-1)}),
        );
        assert_eq!(status(&row), Status::Input);
        let waiting = thread(
            "a",
            json!({"status": "waiting", "pendingRequest": request(-1)}),
        );
        assert_eq!(status(&waiting), Status::Input);
        assert!(!is_working(&waiting));
    }

    #[test]
    fn every_active_thread_status_reads_working() {
        for value in ["starting", "running", "waiting", "stopping"] {
            let row = thread(
                "a",
                json!({"status": value, "latestTurn": turn("running", -5, None)}),
            );
            assert_eq!(status(&row), Status::Working, "{value}");
            assert!(is_working(&row), "{value}");
        }
        assert!(!is_working(&thread("a", json!({"status": "failed"}))));
        assert!(!is_working(&thread("a", json!({}))));
    }

    #[test]
    fn usage_limit_stops_stay_limited_until_the_thread_recovers() {
        let mut failed = turn("failed", -10, Some(-5));
        failed["errorKind"] = json!("usage_limit");
        let limited = thread("a", json!({"status": "failed", "latestTurn": failed}));
        assert_eq!(status(&limited), Status::Limited);
        assert!(!recedes(Status::Limited, false, false, false));

        let running = thread(
            "a",
            json!({"status": "running", "latestTurn": turn("running", -1, None)}),
        );
        assert_eq!(status(&running), Status::Working);
        let done = thread("a", json!({"latestTurn": turn("completed", -1, Some(0))}));
        assert_eq!(status(&done), Status::Ready);
    }

    #[test]
    fn failed_only_while_the_latest_turn_failed() {
        let mut failed = turn("failed", -10, Some(-5));
        failed["errorKind"] = json!("provider");
        let row = thread("a", json!({"status": "failed", "latestTurn": failed}));
        assert_eq!(status(&row), Status::Failed);

        let mut completed = turn("completed", -10, Some(-5));
        completed["errorKind"] = json!("provider");
        let recovered = thread("a", json!({"latestTurn": completed}));
        assert_eq!(status(&recovered), Status::Ready);
    }

    #[test]
    fn working_since_does_not_invent_a_start() {
        let done = thread("a", json!({"latestTurn": turn("completed", -10, Some(-5))}));
        assert_eq!(working_since(&done), None);

        let mut started = turn("running", -10, None);
        started["startedAt"] = json!(min(-9));
        let running = thread("a", json!({"status": "running", "latestTurn": started}));
        assert_eq!(working_since(&running), Some(at(-9)));

        let starting = thread(
            "a",
            json!({"status": "starting", "latestTurn": turn("running", -3, None)}),
        );
        assert_eq!(working_since(&starting), Some(at(-3)));

        let input = thread(
            "a",
            json!({"status": "waiting", "pendingRequest": request(-1), "latestTurn": turn("waiting", -3, None)}),
        );
        assert_eq!(working_since(&input), None);
    }

    #[test]
    fn working_durations() {
        assert_eq!(format_working(Duration::seconds(59)), "59s");
        assert_eq!(format_working(Duration::seconds(125)), "2m");
        assert_eq!(format_working(Duration::seconds(3900)), "1h 5m");
        assert_eq!(format_working(Duration::seconds(-30)), "0s");
        assert_eq!(format_working(Duration::milliseconds(59_999)), "59s");
    }

    #[test]
    fn labels_follow_the_status_and_unread() {
        assert_eq!(Status::Ready.label(false), None);
        assert_eq!(Status::Ready.label(true), Some("Done"));
        assert_eq!(Status::Input.label(false), Some("Input"));
        assert_eq!(Status::Working.label(true), Some("Working"));
        assert_eq!(Status::Limited.label(false), Some("Limited"));
        assert_eq!(Status::Failed.label(false), Some("Failed"));
        assert_eq!(Status::Failed.label(true), Some("Failed"));
    }

    #[test]
    fn receding_follows_status_unread_and_woke() {
        assert!(recedes(Status::Working, true, true, false));
        assert!(!recedes(Status::Working, false, false, true));
        assert!(!recedes(Status::Input, false, false, false));
        assert!(recedes(Status::Ready, false, false, false));
        assert!(!recedes(Status::Ready, true, false, false));
        assert!(!recedes(Status::Ready, false, true, false));
        assert!(!recedes(Status::Failed, false, false, false));
        assert!(!recedes(Status::Limited, false, false, false));
    }

    #[test]
    fn unread_completions() {
        let completed = || turn("completed", -10, Some(-5));
        let never = thread("a", json!({"latestTurn": completed()}));
        assert!(!is_unread(&never), "never visited");
        let empty = thread(
            "a",
            json!({"latestTurn": completed(), "triage": {"visitedAt": ""}}),
        );
        assert!(!is_unread(&empty), "empty visit marker");

        let before = thread(
            "a",
            json!({"latestTurn": completed(), "triage": {"visitedAt": min(-6)}}),
        );
        assert!(is_unread(&before));
        let same = thread(
            "a",
            json!({"latestTurn": completed(), "triage": {"visitedAt": min(-5)}}),
        );
        assert!(!is_unread(&same), "visited at the completion's millisecond");
        let bad = thread(
            "a",
            json!({"latestTurn": completed(), "triage": {"visitedAt": "soon"}}),
        );
        assert!(is_unread(&bad), "unparseable visit");

        let running = thread(
            "a",
            json!({"status": "running", "latestTurn": turn("running", -1, None), "triage": {"visitedAt": min(-6)}}),
        );
        assert!(!is_unread(&running), "no completion yet");
        assert_eq!(completed_at(&running), None);

        let stopped = thread("a", json!({"latestTurn": turn("stopped", -10, Some(-5))}));
        assert_eq!(completed_at(&stopped), Some(at(-5)));
    }

    #[test]
    fn settled_timestamp_prefers_the_stamp_then_activity_then_bumped_at() {
        let stamped = thread(
            "a",
            json!({"latestTurn": turn("completed", -10, Some(-5)), "triage": {"settledAt": min(-20)}}),
        );
        assert_eq!(settled_time(&stamped), Some(at(-20)));

        let malformed = thread(
            "a",
            json!({"latestUserMessageAt": min(-11), "latestTurn": turn("completed", -10, Some(-5)),
                   "triage": {"settledAt": "never"}}),
        );
        assert_eq!(settled_time(&malformed), Some(at(-5)));

        let message = thread(
            "a",
            json!({"latestUserMessageAt": min(-3), "latestTurn": turn("completed", -10, Some(-5))}),
        );
        assert_eq!(settled_time(&message), Some(at(-3)));

        let bare = thread("a", json!({"triage": {"bumpedAt": min(-30)}}));
        assert_eq!(settled_time(&bare), Some(at(-30)));
    }

    #[test]
    fn can_snooze_refuses_open_requests_queued_messages_and_starting_turns() {
        assert!(can_snooze(&thread("a", json!({}))));
        assert!(can_snooze(&thread("a", json!({"status": "running"}))));
        assert!(can_snooze(&thread("a", json!({"status": "failed"}))));
        assert!(!can_snooze(&thread(
            "a",
            json!({"pendingRequest": request(-1)})
        )));
        assert!(!can_snooze(&thread(
            "a",
            json!({"status": "running", "queued": 1})
        )));
        assert!(!can_snooze(&thread("a", json!({"status": "starting"}))));
    }

    #[test]
    fn settle_is_offered_only_where_the_server_accepts_it() {
        assert!(can_settle(&thread("a", json!({}))));
        assert!(can_settle(&thread("a", json!({"status": "failed"}))));
        for value in ["starting", "running", "waiting", "stopping"] {
            assert!(
                !can_settle(&thread("a", json!({"status": value}))),
                "{value}"
            );
        }
        assert!(!can_settle(&thread("a", json!({"queued": 2}))));
        assert!(!can_settle(&thread(
            "a",
            json!({"pendingRequest": request(-1)})
        )));
    }

    #[test]
    fn never_hides_on_malformed_wake_data() {
        let row = thread(
            "a",
            json!({"triage": {"snoozedUntil": "tomorrow", "snoozedAt": min(-5)}}),
        );
        assert!(!is_snoozed(&row, base()));
        assert_eq!(wake_at(&row, base()), None);
        let empty = thread("a", json!({"triage": {"snoozedUntil": ""}}));
        assert!(!is_snoozed(&empty, base()));
    }

    #[test]
    fn snoozed_until_the_timer_passes() {
        let row = thread(
            "a",
            json!({"triage": {"snoozedUntil": min(60), "snoozedAt": min(-5)}}),
        );
        assert!(is_snoozed(&row, base()));
        assert!(!is_snoozed(&row, at(60)));
        assert_eq!(wake_at(&row, at(59)), None);
        assert_eq!(wake_at(&row, at(61)), Some(at(60)));
    }

    #[test]
    fn wakes_early_when_blocked_on_the_user() {
        let row = thread(
            "a",
            json!({"status": "waiting", "pendingRequest": request(-2),
                   "triage": {"snoozedUntil": min(60), "snoozedAt": min(-5)}}),
        );
        assert!(raised_hand(&row));
        assert!(!is_snoozed(&row, base()));
        assert_eq!(
            wake_at(&row, base()),
            Some(at(-2)),
            "the request's open time"
        );
    }

    #[test]
    fn stays_snoozed_while_the_agent_keeps_working() {
        let row = thread(
            "a",
            json!({"status": "running", "latestTurn": turn("running", -1, None),
                   "triage": {"snoozedUntil": min(60), "snoozedAt": min(-5)}}),
        );
        assert!(!raised_hand(&row));
        assert!(is_snoozed(&row, base()));
    }

    #[test]
    fn stays_snoozed_when_a_turn_requested_after_the_snooze_completes() {
        let row = thread(
            "a",
            json!({"latestTurn": turn("completed", -4, Some(-1)),
                   "triage": {"snoozedUntil": min(60), "snoozedAt": min(-5)}}),
        );
        assert!(is_snoozed(&row, base()));
        assert_eq!(wake_at(&row, base()), None);
    }

    #[test]
    fn raised_hand_for_input_and_failures_after_the_snooze() {
        let snooze = json!({"snoozedUntil": min(60), "snoozedAt": min(-5)});
        let after = thread(
            "a",
            json!({"status": "failed", "latestTurn": turn("failed", -4, Some(-1)), "triage": snooze}),
        );
        assert!(raised_hand(&after));
        assert!(!is_snoozed(&after, base()));

        let before = thread(
            "a",
            json!({"status": "failed", "latestTurn": turn("failed", -9, Some(-6)), "triage": snooze}),
        );
        assert!(!raised_hand(&before), "an old failure does not wake");
        assert!(is_snoozed(&before, base()));

        let unstamped = thread(
            "a",
            json!({"status": "failed", "latestTurn": turn("failed", -9, Some(-6)), "triage": {"snoozedUntil": min(60)}}),
        );
        assert!(
            raised_hand(&unstamped),
            "any failure wakes without snoozedAt"
        );

        let no_end = thread(
            "a",
            json!({"status": "failed", "latestTurn": turn("failed", -2, None), "triage": snooze}),
        );
        assert!(
            raised_hand(&no_end),
            "the request time stands in for the end"
        );

        let input = thread("a", json!({"pendingRequest": request(-1)}));
        assert!(raised_hand(&input));
        assert!(!raised_hand(&thread("a", json!({}))));
    }

    #[test]
    fn reports_the_failure_time_for_a_failure_wake() {
        let row = thread(
            "a",
            json!({"status": "failed", "latestTurn": turn("failed", -4, Some(-1)),
                   "triage": {"snoozedUntil": min(60), "snoozedAt": min(-5)}}),
        );
        assert_eq!(wake_at(&row, base()), Some(at(-1)));
    }

    #[test]
    fn keeps_the_early_wake_after_the_scheduled_time_passes() {
        let row = thread(
            "a",
            json!({"status": "failed", "latestTurn": turn("failed", -4, Some(-1)),
                   "triage": {"snoozedUntil": min(60), "snoozedAt": min(-5)}}),
        );
        assert_eq!(wake_at(&row, at(120)), Some(at(-1)));
        let visited = thread(
            "a",
            json!({"status": "failed", "latestTurn": turn("failed", -4, Some(-1)),
                   "triage": {"snoozedUntil": min(60), "snoozedAt": min(-5), "visitedAt": min(0)}}),
        );
        assert_eq!(
            woke_at(&visited, at(120)),
            None,
            "acknowledged before the timer"
        );
    }

    #[test]
    fn reports_the_timer_wake_after_a_turn_requested_before_the_snooze_completes() {
        let row = thread(
            "a",
            json!({"latestTurn": turn("completed", -10, Some(30)),
                   "triage": {"snoozedUntil": min(60), "snoozedAt": min(-5)}}),
        );
        assert_eq!(wake_at(&row, at(90)), Some(at(60)));
    }

    #[test]
    fn keeps_a_timer_wake_until_visited_at_its_time() {
        let row = |visited: serde_json::Value| {
            thread(
                "a",
                json!({"triage": {"snoozedUntil": min(60), "snoozedAt": min(-5), "visitedAt": visited}}),
            )
        };
        assert_eq!(woke_at(&row(json!(min(30))), at(90)), Some(at(60)));
        assert_eq!(woke_at(&row(json!("garbage")), at(90)), Some(at(60)));
        assert_eq!(
            woke_at(&row(serde_json::Value::Null), at(90)),
            Some(at(60)),
            "no visit yet"
        );
        assert_eq!(
            woke_at(&row(json!(min(60))), at(90)),
            None,
            "visited at the wake"
        );
        assert_eq!(woke_at(&row(json!(min(70))), at(90)), None);
    }

    #[test]
    fn woke_stays_hidden_while_snoozed_settled_or_unsnoozed() {
        let snoozed = thread(
            "a",
            json!({"triage": {"snoozedUntil": min(60), "visitedAt": min(-30)}}),
        );
        assert_eq!(woke_at(&snoozed, base()), None);
        let settled = thread(
            "a",
            json!({"triage": {"snoozedUntil": min(60), "visitedAt": min(-30), "settled": "settled"}}),
        );
        assert_eq!(woke_at(&settled, at(90)), None);
        let cleared = thread("a", json!({"triage": {"visitedAt": min(-30)}}));
        assert_eq!(woke_at(&cleared, at(90)), None);
        let unsettled = thread(
            "a",
            json!({"triage": {"snoozedUntil": min(60), "visitedAt": min(-30), "settled": "active"}}),
        );
        assert_eq!(woke_at(&unsettled, at(90)), Some(at(60)));
    }

    #[test]
    fn snooze_countdown_labels() {
        let now = base();
        let after = |seconds: i64| snooze_countdown(now + Duration::seconds(seconds), now);
        assert_eq!(after(0), "now");
        assert_eq!(after(-10), "now");
        assert_eq!(after(20), "1m");
        assert_eq!(after(45 * 60), "45m");
        assert_eq!(after(61 * 60), "2h");
        assert_eq!(after(30 * 3600), "2d");
        assert_eq!(after(24 * 3600), "1d");
    }
}
