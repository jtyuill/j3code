//! Per-thread state for T3 mode: the status ladder, unread, the Working
//! shelf test, and the snooze rules.
//!
//! Ports of t3's `resolveSidebarThreadStatus`, `hasUnseenCompletion`,
//! `hasUnseenFailure`, `shouldRecedeSidebarThread`
//! (`apps/web/src/components/Sidebar.logic.ts:766-953`), `isThreadWorking`
//! (`packages/client-runtime/src/state/threadInbox.ts:17-31`),
//! `resolveThreadWorkingStartedAt` / `formatWorkingDurationLabel`
//! (`models.ts:373-396`), `threadSettled.ts:42-172,326-335`,
//! `resolveThreadListV2WokeAt` / `nextSnoozeWakeAfter`
//! (`apps/mobile/src/features/threads/threadListV2.ts:158-199`) and
//! `resolveSettledThreadTimestamp` (`threadSort.ts:26-47`).

use j3_protocol::t3::orchestration::{
    ProviderFailureClass, ProviderInteractionMode, SettledOverride, ThreadRelationship, ThreadShell,
};
use time::OffsetDateTime;

use super::view::{self, Status, View, flat, instant, ms, now_ms, opt_ms};

/// How long a sent message counts as a queued run start before a run adopts
/// it (`QUEUED_TURN_START_GRACE_MS`, threadSettled.ts:39).
pub const QUEUED_TURN_START_GRACE: time::Duration = time::Duration::minutes(2);
const GRACE_MS: i64 = 2 * 60 * 1_000;
const HOUR_MS: u64 = 60 * 60 * 1_000;
const DAY_MS: u64 = 24 * HOUR_MS;

/// A thread's state on its row, most urgent first
/// (`SidebarThreadStatus`, Sidebar.logic.ts:884-892).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThreadStatus {
    /// A pending approval (j3 never raises one; omp runs in yolo mode).
    Approval,
    /// A pending `ask` question.
    Input,
    /// A guest message waits for the owner (j3 has no guests).
    Request,
    /// Running but blocked on another thread, or idle while background work
    /// it waits on runs.
    Waiting,
    Working,
    Failed,
    /// Stopped by a usage limit.
    Limited,
    Ready,
}

impl ThreadStatus {
    /// The row's status label, as t3's mobile list shows it
    /// (`thread-list-v2-items.tsx:91-140,699-701`): `Ready` reads "Done"
    /// only while its completion is unread.
    pub fn label(self, unread: bool) -> Option<&'static str> {
        Some(match self {
            Self::Approval => "Approval",
            Self::Input => "Input",
            Self::Request => "Request",
            Self::Working => "Working",
            Self::Waiting => "Waiting",
            Self::Failed => "Failed",
            Self::Limited => "Limited",
            Self::Ready if unread => "Done",
            Self::Ready => return None,
        })
    }
}

/// `resolveSidebarThreadStatus` (Sidebar.logic.ts:926-953).
pub fn thread_status(thread: &ThreadShell) -> ThreadStatus {
    status_of(&View::of(thread))
}

pub(crate) fn status_of(view: &View<'_>) -> ThreadStatus {
    if view.has_pending_approvals {
        return ThreadStatus::Approval;
    }
    if view.has_pending_user_input {
        return ThreadStatus::Input;
    }
    if view.pending_guest_message_count > 0 {
        return ThreadStatus::Request;
    }
    let Some(runtime) = view.runtime else {
        return ThreadStatus::Ready;
    };
    if runtime.status == Status::Running && view.shell.blocked_on_thread_wait == Some(true) {
        return ThreadStatus::Waiting;
    }
    if view::is_active(runtime.status) {
        return ThreadStatus::Working;
    }
    match runtime.status {
        Status::Idle => ThreadStatus::Waiting,
        Status::Failed if runtime.last_error_class == Some(ProviderFailureClass::UsageLimit) => {
            ThreadStatus::Limited
        }
        Status::Failed => ThreadStatus::Failed,
        _ => ThreadStatus::Ready,
    }
}

/// The latest run completed after the last visit (`hasUnseenCompletion`,
/// Sidebar.logic.ts:766-775). A thread never visited reads as seen; the
/// server stamps a visit when the first message is sent.
pub fn has_unseen_completion(thread: &ThreadShell) -> bool {
    unseen_completion(&View::of(thread))
}

pub(crate) fn unseen_completion(view: &View<'_>) -> bool {
    let Some(completed_at) = view
        .latest_run
        .and_then(|run| run.completed_at)
        .filter(|at| !at.is_empty())
    else {
        return false;
    };
    let Some(completed_at) = ms(completed_at) else {
        return false;
    };
    let Some(last_visited_at) = view.last_visited_at().flatten().filter(|at| !at.is_empty()) else {
        return false;
    };
    ms(last_visited_at).is_none_or(|last_visited_at| completed_at > last_visited_at)
}

/// The latest run failed after the last visit; unlike completions, a
/// never-visited thread's failure is unseen (`hasUnseenFailure`,
/// Sidebar.logic.ts:777-785).
pub fn has_unseen_failure(thread: &ThreadShell) -> bool {
    let view = View::of(thread);
    let Some(run) = view.latest_run.filter(|run| run.status == Status::Failed) else {
        return false;
    };
    let Some(last_visited_at) = view.last_visited_at().flatten().filter(|at| !at.is_empty()) else {
        return true;
    };
    match (opt_ms(run.completed_at), ms(last_visited_at)) {
        (Some(completed_at), Some(last_visited_at)) => completed_at > last_visited_at,
        _ => true,
    }
}

/// The thread belongs on the Working shelf: its runtime is active or idle
/// on background work, nobody waits on the user, and it is not a finished
/// plan turn with a plan to act on (`isThreadWorking`, threadInbox.ts:17-31).
pub fn is_working(thread: &ThreadShell) -> bool {
    working(&View::of(thread))
}

pub(crate) fn working(view: &View<'_>) -> bool {
    if view.has_pending_approvals || view.has_pending_user_input {
        return false;
    }
    let Some(runtime) = view
        .runtime
        .filter(|runtime| view::is_active(runtime.status) || runtime.status == Status::Idle)
    else {
        return false;
    };
    let run_settled = view.latest_run.is_some_and(|run| {
        !view::is_active(run.status) && runtime.active_run_id != Some(run.run_id)
    });
    !(view.shell.interaction_mode == ProviderInteractionMode::Plan
        && view.shell.has_actionable_proposed_plan
        && run_settled)
}

/// When the current stretch of work started, for the row's elapsed time
/// (`resolveThreadWorkingStartedAt`, models.ts:373-388).
pub fn working_started_at(thread: &ThreadShell) -> Option<OffsetDateTime> {
    let view = View::of(thread);
    let runtime = view.runtime;
    if let Some(activity_started_at) = runtime.and_then(|runtime| runtime.activity_started_at) {
        return activity_started_at.and_then(instant);
    }
    let run = view.latest_run?;
    if run.completed_at.is_none()
        && runtime.and_then(|runtime| runtime.active_run_id) == Some(run.run_id)
    {
        return run
            .started_at
            .and_then(instant)
            .or_else(|| run.requested_at.and_then(instant));
    }
    None
}

/// `12s`, `4m`, `1h 5m` (`formatWorkingDurationLabel`, models.ts:390-396).
pub fn format_working_duration(elapsed: time::Duration) -> String {
    let seconds = elapsed.whole_seconds().max(0);
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    format!("{}h {}m", minutes / 60, minutes % 60)
}

/// Whether the row is drawn quietly (`shouldRecedeSidebarThread`,
/// Sidebar.logic.ts:894-914).
pub fn should_recede(
    status: ThreadStatus,
    unread: bool,
    woke: bool,
    active: bool,
    selected: bool,
) -> bool {
    if active || selected || matches!(status, ThreadStatus::Input | ThreadStatus::Request) {
        return false;
    }
    match status {
        ThreadStatus::Working | ThreadStatus::Waiting => true,
        ThreadStatus::Ready | ThreadStatus::Approval => !unread && !woke,
        _ => false,
    }
}

/// The thread gets a list row: not a subagent or side chat
/// (`isListedThreadLineage`, `packages/contracts/src/orchestrationV2.ts:161-163`),
/// not archived, not deleted.
pub fn is_listed(thread: &ThreadShell) -> bool {
    thread.lineage.relationship_to_parent != Some(ThreadRelationship::Subagent)
        && thread.lineage.side_chat != Some(true)
        && thread.archived_at.is_none()
        && thread.deleted_at.is_none()
}

/// A sent message no run has adopted yet, within the grace window
/// (`hasQueuedTurnStart`, threadSettled.ts:42-69). Settling or snoozing
/// then would hide work about to start.
pub fn has_queued_turn_start(thread: &ThreadShell, now: OffsetDateTime) -> bool {
    queued_turn_start(&View::of(thread), now_ms(now))
}

fn queued_turn_start(view: &View<'_>, now: i64) -> bool {
    if view.runtime.is_some_and(|runtime| {
        matches!(
            runtime.status,
            Status::Preparing | Status::Queued | Status::Starting
        )
    }) {
        return true;
    }
    let Some(message_at) = view.shell.latest_user_message_at.as_deref() else {
        return false;
    };
    let Some(message_at) = ms(message_at) else {
        return false;
    };
    // Bounded on both sides: a sender's clock ahead of this one must not
    // hold the queued state for the whole skew.
    if (now - message_at).abs() > GRACE_MS {
        return false;
    }
    let Some(run) = view.latest_run else {
        return true;
    };
    [run.requested_at, run.started_at, run.completed_at]
        .into_iter()
        .all(|at| at.is_none_or(|at| ms(at).is_some_and(|at| at < message_at)))
}

/// The failure that can end a snooze: the failed run's completion, else the
/// runtime's last update; `None` unless the runtime failed
/// (`failureEdgeAt`, threadSettled.ts:99-109).
fn failure_edge_at<'a>(view: &View<'a>) -> Option<&'a str> {
    let runtime = view
        .runtime
        .filter(|runtime| runtime.status == Status::Failed)?;
    if let Some(completed_at) = view
        .latest_run
        .filter(|run| run.status == Status::Failed)
        .and_then(|run| run.completed_at)
    {
        return Some(completed_at);
    }
    Some(runtime.updated_at)
}

fn failed_after_snooze(failed_at: &str, snoozed_at: Option<&str>) -> bool {
    match snoozed_at {
        None => true,
        Some(snoozed_at) => matches!((ms(failed_at), ms(snoozed_at)), (Some(f), Some(s)) if f > s),
    }
}

/// Only a request for the user or a fresh failure ends a snooze early;
/// completed runs never do (`threadRaisedHandWhileSnoozed`,
/// threadSettled.ts:81-97).
pub fn raised_hand_while_snoozed(thread: &ThreadShell) -> bool {
    raised_hand(&View::of(thread))
}

fn raised_hand(view: &View<'_>) -> bool {
    if view.has_pending_approvals || view.has_pending_user_input {
        return true;
    }
    if view.has_pending_runtime_request() || view.pending_guest_message_count > 0 {
        return true;
    }
    failure_edge_at(view).is_some_and(|failed_at| failed_after_snooze(failed_at, view.snoozed_at()))
}

/// Snoozing is allowed: nothing waits on the user and no run is about to
/// start, mirroring the server's rejections (`canSnooze`,
/// threadSettled.ts:111-135).
pub fn can_snooze(thread: &ThreadShell, now: OffsetDateTime) -> bool {
    let view = View::of(thread);
    !(view.has_pending_approvals
        || view.has_pending_user_input
        || view.has_pending_runtime_request()
        || view.pending_guest_message_count > 0
        || queued_turn_start(&view, now_ms(now)))
}

/// The snooze still hides the thread: its timer has not run out and the
/// thread has not raised a hand (`effectiveSnoozed`, threadSettled.ts:137-146).
/// Malformed wake times never hide a thread.
pub fn effective_snoozed(thread: &ThreadShell, now: OffsetDateTime) -> bool {
    snoozed(&View::of(thread), now_ms(now))
}

pub(crate) fn snoozed(view: &View<'_>, now: i64) -> bool {
    let Some(wake_at) = opt_ms(view.snoozed_until()) else {
        return false;
    };
    wake_at > now && !raised_hand(view)
}

/// When a snoozed thread woke: the hand-raise (failure, else the runtime's
/// last update) for an early wake, else the timer once it has passed
/// (`threadWokeAt`, threadSettled.ts:148-172).
pub fn woke_at(thread: &ThreadShell, now: OffsetDateTime) -> Option<OffsetDateTime> {
    woke(&View::of(thread), now_ms(now)).and_then(instant)
}

fn woke<'a>(view: &View<'a>, now: i64) -> Option<&'a str> {
    let snoozed_until = view.snoozed_until()?;
    let wake_at = ms(snoozed_until)?;
    if raised_hand(view) {
        // An old failure must not replace the time of an approval or input wake.
        if let Some(failed_at) = failure_edge_at(view)
            && failed_after_snooze(failed_at, view.snoozed_at())
        {
            return Some(failed_at);
        }
        return view
            .runtime
            .map(|runtime| runtime.updated_at)
            .or(view.snoozed_at());
    }
    (wake_at <= now).then_some(snoozed_until)
}

/// The wake the user has not seen yet: woke after the last visit, not
/// settled, and the server tracks visits (`resolveThreadListV2WokeAt`,
/// threadListV2.ts:187-199).
pub fn woke_unseen_at(thread: &ThreadShell, now: OffsetDateTime) -> Option<OffsetDateTime> {
    let view = View::of(thread);
    let last_visited_at = view.last_visited_at()?;
    if thread.settled_override == Some(SettledOverride::Settled) {
        return None;
    }
    let woke_at = woke(&view, now_ms(now))?;
    let woke_ms = ms(woke_at)?;
    match opt_ms(last_visited_at) {
        Some(last_visited_at) if last_visited_at >= woke_ms => None,
        _ => instant(woke_at),
    }
}

/// When the thread settled: `settledAt`, else its latest activity, else
/// `updatedAt` (`resolveSettledThreadTimestamp`, threadSort.ts:26-47). Also
/// the time a settled row shows.
pub fn settled_timestamp(thread: &ThreadShell) -> Option<OffsetDateTime> {
    settled_ms(&View::of(thread))
        .and_then(|ms| OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).ok())
}

pub(crate) fn settled_ms(view: &View<'_>) -> Option<i64> {
    if let Some(settled_at) = view.shell.settled_at.as_deref().and_then(ms) {
        return Some(settled_at);
    }
    let run = view.latest_run;
    let latest = [
        view.shell.latest_user_message_at.as_deref(),
        run.and_then(|run| run.requested_at),
        run.and_then(|run| run.started_at),
        run.and_then(|run| run.completed_at),
    ]
    .into_iter()
    .filter_map(opt_ms)
    .max();
    latest.or_else(|| ms(&view.shell.updated_at))
}

/// Time left on a snooze: `now`, `45m`, `3h`, `2d`
/// (`snoozeWakeLabel`, threadSettled.ts:326-335).
pub fn snooze_wake_label(snoozed_until: OffsetDateTime, now: OffsetDateTime) -> String {
    let Ok(remaining) = u64::try_from(now_ms(snoozed_until) - now_ms(now)) else {
        return "now".to_owned();
    };
    if remaining == 0 {
        "now".to_owned()
    } else if remaining < HOUR_MS {
        format!("{}m", remaining.div_ceil(60_000).max(1))
    } else if remaining < DAY_MS {
        format!("{}h", remaining.div_ceil(HOUR_MS))
    } else {
        format!("{}d", remaining.div_ceil(DAY_MS))
    }
}

/// The earliest `snoozedUntil` strictly after `now`: when a snoozed row
/// next wakes, so the list re-renders exactly then
/// (`nextSnoozeWakeAfter`, threadListV2.ts:157-169).
pub fn next_snooze_wake_after<'a>(
    threads: impl IntoIterator<Item = &'a ThreadShell>,
    now: OffsetDateTime,
) -> Option<OffsetDateTime> {
    let now = now_ms(now);
    threads
        .into_iter()
        .filter_map(|thread| flat(&thread.snoozed_until).and_then(ms))
        .filter(|wake_at| *wake_at > now)
        .min()
        .and_then(|ms| OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).ok())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use time::macros::datetime;

    use super::*;
    use crate::workflow::fixtures::{UNDEFINED, request, shell};

    fn at(iso: &str) -> OffsetDateTime {
        instant(iso).expect("valid timestamp")
    }

    // --- resolveSidebarThreadStatus (Sidebar.logic.test.ts:259-349) ---

    /// `runtime: { status: "running", activeRunId: null }`.
    fn running() -> serde_json::Value {
        json!({ "latestRunId": "run-1", "status": "running" })
    }

    fn patched(base: serde_json::Value, extra: serde_json::Value) -> ThreadShell {
        let mut base = base;
        for (key, value) in extra.as_object().unwrap() {
            base[key] = value.clone();
        }
        shell("thread-1", base)
    }

    /// Sidebar.logic.test.ts:271-290.
    #[test]
    fn awaiting_input_beats_a_running_runtime_below_approval() {
        let input = patched(
            running(),
            json!({ "pendingRuntimeRequest": request("user_input"), "blockedOnThreadWait": true }),
        );
        assert_eq!(thread_status(&input), ThreadStatus::Input);
        let approval = patched(
            running(),
            json!({ "pendingRuntimeRequest": request("command"), "blockedOnThreadWait": true }),
        );
        assert_eq!(thread_status(&approval), ThreadStatus::Approval);
    }

    /// Sidebar.logic.test.ts:291-302.
    #[test]
    fn a_waiting_guest_message_is_a_request_below_the_agents_own() {
        let guest = patched(running(), json!({ "pendingGuestMessageCount": 1 }));
        assert_eq!(thread_status(&guest), ThreadStatus::Request);
        let input = patched(
            json!({}),
            json!({ "pendingGuestMessageCount": 1, "pendingRuntimeRequest": request("user_input") }),
        );
        assert_eq!(thread_status(&input), ThreadStatus::Input);
    }

    /// Sidebar.logic.test.ts:304-327.
    #[test]
    fn usage_limit_stops_stay_limited_until_the_thread_recovers() {
        let limited = json!({ "latestRunId": "run-1", "status": "failed", "lastError": "Plan limit reached", "lastErrorClass": "usage_limit" });
        assert_eq!(
            thread_status(&patched(limited.clone(), json!({}))),
            ThreadStatus::Limited
        );
        assert_eq!(
            thread_status(&patched(limited.clone(), json!({ "status": "running" }))),
            ThreadStatus::Working
        );
        assert_eq!(
            thread_status(&patched(limited, json!({ "status": "completed" }))),
            ThreadStatus::Ready
        );
        assert!(!should_recede(
            ThreadStatus::Limited,
            false,
            false,
            false,
            false
        ));
    }

    /// Sidebar.logic.test.ts:329-348.
    #[test]
    fn failed_only_while_the_latest_run_failed() {
        let failed = json!({ "latestRunId": "run-1", "status": "failed", "lastError": "boom" });
        assert_eq!(
            thread_status(&patched(failed, json!({}))),
            ThreadStatus::Failed
        );
        let completed =
            json!({ "latestRunId": "run-1", "status": "completed", "lastError": "persisted" });
        assert_eq!(
            thread_status(&patched(completed, json!({}))),
            ThreadStatus::Ready
        );
        let idle = json!({ "latestRunId": "run-1", "status": "idle", "lastError": "persisted" });
        assert_eq!(
            thread_status(&patched(idle, json!({}))),
            ThreadStatus::Waiting
        );
    }

    /// Sidebar.logic.test.ts:454-467 / threadListV2.test.ts:103-125: a
    /// decoded thread wait reads Waiting and resumes Working when it ends.
    #[test]
    fn a_thread_wait_reads_waiting_and_resumes_working() {
        let raw =
            json!({ "status": "running", "activeRunId": "run-wait", "latestRunId": "run-wait" });
        assert_eq!(
            thread_status(&patched(raw.clone(), json!({}))),
            ThreadStatus::Working
        );
        let blocked = patched(raw.clone(), json!({ "blockedOnThreadWait": true }));
        assert_eq!(thread_status(&blocked), ThreadStatus::Waiting);
        let blocked_input = patched(
            raw.clone(),
            json!({ "blockedOnThreadWait": true, "pendingRuntimeRequest": request("user_input") }),
        );
        assert_eq!(thread_status(&blocked_input), ThreadStatus::Input);
        let resumed = patched(raw, json!({ "blockedOnThreadWait": false }));
        assert_eq!(thread_status(&resumed), ThreadStatus::Working);
    }

    /// Sidebar.logic.test.ts:990-1039 / threadListV2.test.ts:139-161: an
    /// unseen completion with a background roster.
    #[test]
    fn unseen_completion_with_background_work() {
        for (task, status, receded) in [
            (json!({ "kind": "command" }), ThreadStatus::Ready, false),
            (json!({ "kind": "monitor" }), ThreadStatus::Ready, false),
            (
                json!({ "kind": "command", "awaited": true }),
                ThreadStatus::Waiting,
                true,
            ),
        ] {
            let mut task = task;
            task["taskId"] = json!("background-work");
            let thread = shell(
                "thread-1",
                json!({
                    "latestRunId": "run-background-completion",
                    "status": "completed",
                    "latestRunCompletedAt": "2026-06-20T01:00:00.000Z",
                    "lastVisitedAt": "2026-06-20T00:59:00.000Z",
                    "pendingBackgroundTasks": [task],
                }),
            );
            let unread = has_unseen_completion(&thread);
            assert!(unread);
            assert_eq!(thread_status(&thread), status);
            assert_eq!(should_recede(status, unread, false, false, false), receded);
            assert_eq!(is_working(&thread), receded);
        }
    }

    /// Sidebar.logic.test.ts:510-590: unread needs a visit marker; failures
    /// are unseen until visited after them.
    #[test]
    fn unread_completions_and_failures() {
        let completed = |last_visited_at: serde_json::Value| {
            shell(
                "thread-1",
                json!({
                    "latestRunId": "turn-1",
                    "status": "completed",
                    "latestRunRequestedAt": "2026-03-09T10:00:00.000Z",
                    "latestRunStartedAt": "2026-03-09T10:00:00.000Z",
                    "latestRunCompletedAt": "2026-03-09T10:05:00.000Z",
                    "lastVisitedAt": last_visited_at,
                }),
            )
        };
        // :510 — no visit marker, no completed state.
        assert!(!has_unseen_completion(&completed(json!(UNDEFINED))));
        assert!(!has_unseen_completion(&completed(json!(null))));
        // :526 — visited before the completion.
        assert!(has_unseen_completion(&completed(json!(
            "2026-03-09T10:04:00.000Z"
        ))));
        assert!(!has_unseen_completion(&completed(json!(
            "2026-03-09T10:05:00.000Z"
        ))));

        let failed = |last_visited_at: serde_json::Value| {
            shell(
                "thread-1",
                json!({
                    "latestRunId": "turn-1",
                    "status": "failed",
                    "latestRunCompletedAt": "2026-03-09T10:05:00.000Z",
                    "lastVisitedAt": last_visited_at,
                }),
            )
        };
        // :544 — visited before the failure.
        assert!(has_unseen_failure(&failed(json!(
            "2026-03-09T10:04:00.000Z"
        ))));
        // :577 — seen.
        assert!(!has_unseen_failure(&failed(json!(
            "2026-03-09T10:06:00.000Z"
        ))));
        // Sidebar.logic.ts:777 — never opened.
        assert!(has_unseen_failure(&failed(json!(null))));
        assert!(!has_unseen_failure(&completed(json!(null))));
    }

    /// Sidebar.logic.test.ts:1070-1092.
    #[test]
    fn the_working_shelf_takes_running_threads_and_background_waits_only() {
        let run = json!({
            "latestRunId": "turn-1",
            "latestRunRequestedAt": "2026-03-09T10:00:00.000Z",
            "latestRunStartedAt": "2026-03-09T10:00:00.000Z",
            "latestRunCompletedAt": "2026-03-09T10:05:00.000Z",
        });
        let task = json!([{ "taskId": "bg-1", "description": "Watch build", "kind": "monitor", "awaited": true }]);
        let thread = |extra: serde_json::Value| patched(run.clone(), extra);

        assert!(is_working(&thread(json!({ "status": "running" }))));
        let waiting = json!({ "status": "completed", "pendingBackgroundTasks": task });
        assert!(is_working(&thread(waiting.clone())));
        assert!(!is_working(&thread(json!({ "status": "completed" }))));
        assert!(!is_working(&thread(
            json!({ "status": "running", "pendingRuntimeRequest": request("command") })
        )));
        assert!(!is_working(&thread(
            json!({ "status": "running", "pendingRuntimeRequest": request("user_input") })
        )));
        assert!(!is_working(&thread(
            json!({ "status": "failed", "lastError": "boom", "pendingBackgroundTasks": task })
        )));
        // :1084 — a ready plan stays in the inbox while background work runs.
        let mut plan = waiting;
        plan["interactionMode"] = json!("plan");
        plan["hasActionableProposedPlan"] = json!(true);
        assert!(!is_working(&thread(plan)));
    }

    /// Sidebar.logic.test.ts:386-434.
    #[test]
    fn working_started_at_does_not_invent_a_start() {
        // :396 — the newest run completed.
        let completed = shell(
            "thread-1",
            json!({
                "latestRunId": "turn-1",
                "activeRunId": "turn-1",
                "status": "running",
                "latestRunStartedAt": "2026-03-09T10:00:00.000Z",
                "latestRunCompletedAt": "2026-03-09T10:05:00.000Z",
                "updatedAt": "2026-03-09T10:02:00.000Z",
            }),
        );
        assert_eq!(working_started_at(&completed), None);
        // :405 — a newer queued or cancelled run shares the activity timer.
        for (status, completed_at) in [
            ("queued", json!(null)),
            ("cancelled", json!("2026-03-09T10:05:00.000Z")),
        ] {
            let thread = shell(
                "thread-1",
                json!({
                    "latestRunId": "newer-run",
                    "activeRunId": "turn-1",
                    "status": status,
                    "latestRunStartedAt": null,
                    "latestRunCompletedAt": completed_at,
                    "activityRunStartedAt": "2026-03-09T10:00:00.000Z",
                    "activityRunStatus": "running",
                    "updatedAt": "2026-03-09T10:30:00.000Z",
                }),
            );
            assert_eq!(
                working_started_at(&thread),
                Some(at("2026-03-09T10:00:00.000Z"))
            );
        }
        // models.ts:384 — the active run's start, else its request.
        let active = shell(
            "thread-1",
            json!({
                "latestRunId": "turn-1",
                "activeRunId": "turn-1",
                "status": "running",
                "latestRunRequestedAt": "2026-03-09T09:59:00.000Z",
                "latestRunStartedAt": null,
                "latestRunCompletedAt": null,
            }),
        );
        assert_eq!(
            working_started_at(&active),
            Some(at("2026-03-09T09:59:00.000Z"))
        );
        assert_eq!(format_working_duration(time::Duration::seconds(59)), "59s");
        assert_eq!(format_working_duration(time::Duration::seconds(125)), "2m");
        assert_eq!(
            format_working_duration(time::Duration::seconds(3_900)),
            "1h 5m"
        );
    }

    // --- threadSnoozed.test.ts ---

    const NOW: &str = "2026-04-10T12:00:00.000Z";
    const SNOOZED_AT: &str = "2026-04-10T09:00:00.000Z";
    const FUTURE_WAKE: &str = "2026-04-11T09:00:00.000Z";
    const PAST_WAKE: &str = "2026-04-10T10:00:00.000Z";

    /// `makeShell` (threadSnoozed.test.ts:28-65), in OV2 shell terms: a
    /// session status becomes the shell status (`error` is `failed`), with
    /// the runtime's `updatedAt` at 11:00.
    fn snooze_shell(
        snoozed_until: Option<&str>,
        status: Option<&str>,
        pending: Option<&str>,
        turn_completed_at: Option<&str>,
    ) -> ThreadShell {
        let mut row = json!({
            "snoozedUntil": snoozed_until,
            "snoozedAt": snoozed_until.map(|_| SNOOZED_AT),
            "updatedAt": "2026-04-10T11:00:00.000Z",
        });
        if let Some(status) = status {
            row["activeProviderThreadId"] = json!("provider-thread");
            row["status"] = json!(match status {
                "error" => "failed",
                "ready" => "idle",
                other => other,
            });
        }
        if let Some(kind) = pending {
            row["pendingRuntimeRequest"] = request(kind);
        }
        if let Some(completed_at) = turn_completed_at {
            row["latestRunId"] = json!("turn-1");
            row["status"] = json!(match status {
                Some("error") => "failed",
                _ => "completed",
            });
            row["latestRunRequestedAt"] = json!(SNOOZED_AT);
            row["latestRunCompletedAt"] = json!(completed_at);
        }
        shell("thread-1", row)
    }

    #[test]
    fn never_hides_on_malformed_wake_data() {
        // threadSnoozed.test.ts:74-76
        let thread = snooze_shell(Some("not-a-date"), None, None, None);
        assert!(!effective_snoozed(&thread, at(NOW)));
    }

    #[test]
    fn wakes_early_when_blocked_on_the_user() {
        // threadSnoozed.test.ts:78-89
        for kind in ["command", "user_input"] {
            let thread = snooze_shell(Some(FUTURE_WAKE), None, Some(kind), None);
            assert!(!effective_snoozed(&thread, at(NOW)));
        }
    }

    #[test]
    fn uses_the_wake_event_time_despite_newer_metadata() {
        // threadSnoozed.test.ts:91-133
        for (completed_at, pending, woke) in [
            ("2026-04-10T08:00:00.000Z", None, None),
            (
                "2026-04-10T10:00:00.000Z",
                None,
                Some("2026-04-10T10:00:00.000Z"),
            ),
            (
                "2026-04-10T08:00:00.000Z",
                Some("command"),
                Some("2026-04-10T11:00:00.000Z"),
            ),
            (
                "2026-04-10T08:00:00.000Z",
                Some("user_input"),
                Some("2026-04-10T11:00:00.000Z"),
            ),
        ] {
            let mut row = json!({
                "snoozedUntil": FUTURE_WAKE,
                "snoozedAt": SNOOZED_AT,
                "status": "failed",
                "updatedAt": "2026-04-10T11:00:00.000Z",
                "latestRunId": "run-1",
                "latestRunRequestedAt": "2026-04-10T07:00:00.000Z",
                "latestRunCompletedAt": completed_at,
            });
            if let Some(kind) = pending {
                row["pendingRuntimeRequest"] = request(kind);
            }
            let thread = shell("thread-1", row);
            assert_eq!(effective_snoozed(&thread, at(NOW)), woke.is_none());
            assert_eq!(woke_at(&thread, at(NOW)), woke.map(at));
        }
    }

    #[test]
    fn stays_snoozed_while_the_agent_keeps_working() {
        // threadSnoozed.test.ts:135-141
        let thread = snooze_shell(Some(FUTURE_WAKE), Some("running"), None, None);
        assert!(effective_snoozed(&thread, at(NOW)));
    }

    #[test]
    fn stays_snoozed_when_a_run_requested_after_the_snooze_completes() {
        // threadSnoozed.test.ts:143-156
        let thread = shell(
            "thread-1",
            json!({
                "snoozedUntil": FUTURE_WAKE,
                "snoozedAt": SNOOZED_AT,
                "latestRunId": "run-1",
                "status": "completed",
                "latestRunRequestedAt": "2026-04-10T09:00:00.001Z",
                "latestRunStartedAt": "2026-04-10T09:15:00.000Z",
                "latestRunCompletedAt": "2026-04-10T10:30:00.000Z",
            }),
        );
        assert!(effective_snoozed(&thread, at(NOW)));
        assert_eq!(woke_at(&thread, at(NOW)), None);
    }

    #[test]
    fn raised_hand_for_approvals_input_and_failures() {
        // threadSnoozed.test.ts:159-173
        for (status, pending) in [
            (None, Some("command")),
            (None, Some("user_input")),
            (Some("error"), None),
        ] {
            let thread = snooze_shell(Some(FUTURE_WAKE), status, pending, None);
            assert!(raised_hand_while_snoozed(&thread));
        }
    }

    #[test]
    fn can_snooze_refuses_blocked_on_you_work() {
        // threadSnoozed.test.ts:176-194 (j3 has no subagent requests)
        for kind in ["command", "user_input"] {
            let thread = snooze_shell(None, None, Some(kind), None);
            assert!(!can_snooze(&thread, at(NOW)));
        }
    }

    #[test]
    fn can_snooze_refuses_a_queued_turn_start() {
        // threadSnoozed.test.ts:196-210
        let queued = shell(
            "thread-1",
            json!({ "latestUserMessageAt": "2026-04-10T11:59:30.000Z" }),
        );
        assert!(!can_snooze(&queued, at(NOW)));
        let old = shell(
            "thread-1",
            json!({ "latestUserMessageAt": "2026-04-10T11:00:00.000Z" }),
        );
        assert!(can_snooze(&old, at(NOW)));
    }

    #[test]
    fn queued_state_clears_when_a_run_adopts_the_message() {
        // threadSnoozed.test.ts:213-244 (j3 has no legacy session; a failed
        // runtime is covered by the run that failed)
        let message_at = "2026-04-10T11:59:00.000Z";
        let adopted = shell(
            "thread-1",
            json!({
                "latestUserMessageAt": message_at,
                "latestRunId": "turn-adopted",
                "status": "running",
                "latestRunRequestedAt": message_at,
                "latestRunStartedAt": null,
                "latestRunCompletedAt": null,
            }),
        );
        assert!(!has_queued_turn_start(&adopted, at(NOW)));
        let pending = shell("thread-1", json!({ "latestUserMessageAt": message_at }));
        assert!(has_queued_turn_start(&pending, at(NOW)));
        let skewed = shell(
            "thread-1",
            json!({ "latestUserMessageAt": "2026-04-10T12:03:00.000Z" }),
        );
        assert!(!has_queued_turn_start(&skewed, at(NOW)));
        let starting = shell(
            "thread-1",
            json!({ "latestRunId": "run-1", "status": "starting" }),
        );
        assert!(has_queued_turn_start(&starting, at(NOW)));
    }

    #[test]
    fn reports_the_timer_wake_after_an_already_requested_run_completes() {
        // threadSnoozed.test.ts:248-256
        let plain = snooze_shell(Some(PAST_WAKE), None, None, None);
        assert_eq!(woke_at(&plain, at(NOW)), Some(at(PAST_WAKE)));
        let completed = snooze_shell(
            Some(PAST_WAKE),
            None,
            None,
            Some("2026-04-10T09:30:00.000Z"),
        );
        assert!(!effective_snoozed(&completed, at(NOW)));
        assert_eq!(woke_at(&completed, at(NOW)), Some(at(PAST_WAKE)));
    }

    #[test]
    fn reports_hand_raise_activity_instead_of_a_completion() {
        // threadSnoozed.test.ts:258-270
        for (status, pending) in [
            ("error", None),
            ("ready", Some("command")),
            ("ready", Some("user_input")),
        ] {
            let thread = snooze_shell(
                Some(FUTURE_WAKE),
                Some(status),
                pending,
                Some("2026-04-10T10:30:00.000Z"),
            );
            assert!(!effective_snoozed(&thread, at(NOW)));
            let expected = if status == "error" {
                // OV2 reads the failed run's completion as the failure edge
                // (threadSettled.ts:99-109); t3's legacy-session fixture has
                // only the runtime's update time.
                "2026-04-10T10:30:00.000Z"
            } else {
                "2026-04-10T11:00:00.000Z"
            };
            assert_eq!(woke_at(&thread, at(NOW)), Some(at(expected)));
        }
    }

    #[test]
    fn keeps_the_early_wake_after_the_scheduled_time_passes() {
        // threadSnoozed.test.ts:272-283
        let thread = shell(
            "thread-1",
            json!({
                "snoozedUntil": PAST_WAKE,
                "snoozedAt": SNOOZED_AT,
                "status": "failed",
                "updatedAt": "2026-04-10T11:00:00.000Z",
                "latestRunId": "run-1",
                "latestRunRequestedAt": "2026-04-10T09:15:00.000Z",
                "latestRunCompletedAt": "2026-04-10T09:30:00.000Z",
            }),
        );
        assert_eq!(
            woke_at(&thread, at(NOW)),
            Some(at("2026-04-10T09:30:00.000Z"))
        );
    }

    // --- thread list Woke marker (threadListV2.test.ts:1403-1470) ---

    fn woke_thread(last_visited_at: serde_json::Value) -> ThreadShell {
        shell(
            "woke",
            json!({
                "latestUserMessageAt": "2026-06-01T22:30:00.000Z",
                "snoozedAt": "2026-06-01T23:00:00.000Z",
                "snoozedUntil": "2026-06-01T23:59:00.000Z",
                "lastVisitedAt": last_visited_at,
            }),
        )
    }

    const LIST_NOW: &str = "2026-06-02T00:00:00.000Z";

    #[test]
    fn keeps_a_timer_wake_until_acknowledged_at_its_timestamp() {
        // threadListV2.test.ts:1414-1428
        let woke = Some(at("2026-06-01T23:59:00.000Z"));
        assert_eq!(
            woke_unseen_at(&woke_thread(json!(null)), at(LIST_NOW)),
            woke
        );
        assert_eq!(
            woke_unseen_at(
                &woke_thread(json!("2026-06-01T23:30:00.000Z")),
                at(LIST_NOW)
            ),
            woke
        );
        assert_eq!(
            woke_unseen_at(
                &woke_thread(json!("2026-06-01T23:59:00.000Z")),
                at(LIST_NOW)
            ),
            None
        );
        assert_eq!(
            woke_unseen_at(&woke_thread(json!("invalid")), at(LIST_NOW)),
            woke
        );
    }

    #[test]
    fn woke_stays_hidden_while_snoozed_settled_or_untracked() {
        // threadListV2.test.ts:1430-1439
        let thread = woke_thread(json!(null));
        assert_eq!(
            woke_unseen_at(&thread, at("2026-06-01T23:58:00.000Z")),
            None
        );
        let mut settled = thread.clone();
        settled.settled_override = Some(SettledOverride::Settled);
        assert_eq!(woke_unseen_at(&settled, at(LIST_NOW)), None);
        assert_eq!(
            woke_unseen_at(&woke_thread(json!(UNDEFINED)), at(LIST_NOW)),
            None
        );
        let mut awake = thread;
        awake.snoozed_until = Some(None);
        assert_eq!(woke_unseen_at(&awake, at(LIST_NOW)), None);
    }

    #[test]
    fn keeps_an_early_failure_wake_acknowledged_after_the_scheduled_time() {
        // threadListV2.test.ts:1441-1470
        let completed_at = "2026-06-01T23:58:00.000Z";
        let failed = |last_visited_at: serde_json::Value| {
            shell(
                "woke",
                json!({
                    "latestUserMessageAt": "2026-06-01T22:30:00.000Z",
                    "snoozedAt": "2026-06-01T23:00:00.000Z",
                    "snoozedUntil": "2026-06-02T00:01:00.000Z",
                    "lastVisitedAt": last_visited_at,
                    "status": "failed",
                    "updatedAt": completed_at,
                    "latestRunId": "woke-run",
                    "latestRunRequestedAt": "2026-06-01T23:57:00.000Z",
                    "latestRunStartedAt": "2026-06-01T23:57:00.000Z",
                    "latestRunCompletedAt": completed_at,
                }),
            )
        };
        assert_eq!(
            woke_unseen_at(&failed(json!(null)), at(LIST_NOW)),
            Some(at(completed_at))
        );
        assert_eq!(
            woke_unseen_at(&failed(json!(completed_at)), at("2026-06-02T00:02:00.000Z")),
            None
        );
    }

    // --- resolveSettledThreadTimestamp (threadSort.test.ts:29-59) ---

    #[test]
    fn settled_timestamp_prefers_the_stamp_then_activity_then_updated_at() {
        let stamped = shell(
            "thread-1",
            json!({
                "settledAt": "2026-03-09T10:00:00.000Z",
                "latestUserMessageAt": "2026-03-09T11:00:00.000Z",
                "updatedAt": "2026-03-09T12:00:00.000Z",
            }),
        );
        assert_eq!(
            settled_timestamp(&stamped),
            Some(at("2026-03-09T10:00:00.000Z"))
        );
        let malformed = shell(
            "thread-1",
            json!({
                "settledAt": "invalid",
                "latestUserMessageAt": "2026-03-09T11:00:00.000Z",
                "updatedAt": "2026-03-09T12:00:00.000Z",
            }),
        );
        assert_eq!(
            settled_timestamp(&malformed),
            Some(at("2026-03-09T11:00:00.000Z"))
        );
        let bare = shell(
            "thread-1",
            json!({ "updatedAt": "2026-03-09T12:00:00.000Z" }),
        );
        assert_eq!(
            settled_timestamp(&bare),
            Some(at("2026-03-09T12:00:00.000Z"))
        );
    }

    #[test]
    fn snooze_wake_labels_and_next_wake() {
        // threadSettled.ts:326-335, threadListV2.ts:157-169
        let now = datetime!(2026-04-10 12:00 UTC);
        assert_eq!(snooze_wake_label(now, now), "now");
        assert_eq!(
            snooze_wake_label(now + time::Duration::seconds(20), now),
            "1m"
        );
        assert_eq!(
            snooze_wake_label(now + time::Duration::minutes(45), now),
            "45m"
        );
        assert_eq!(
            snooze_wake_label(now + time::Duration::minutes(61), now),
            "2h"
        );
        assert_eq!(
            snooze_wake_label(now + time::Duration::hours(30), now),
            "2d"
        );
        let threads = [
            shell("a", json!({ "snoozedUntil": "2026-04-10T11:00:00.000Z" })),
            shell("b", json!({ "snoozedUntil": "2026-04-10T15:00:00.000Z" })),
            shell("c", json!({ "snoozedUntil": "2026-04-10T13:00:00.000Z" })),
            shell("d", json!({})),
        ];
        assert_eq!(
            next_snooze_wake_after(&threads, now),
            Some(datetime!(2026-04-10 13:00 UTC))
        );
    }

    #[test]
    fn labels_follow_the_mobile_list() {
        assert_eq!(ThreadStatus::Ready.label(false), None);
        assert_eq!(ThreadStatus::Ready.label(true), Some("Done"));
        assert_eq!(ThreadStatus::Input.label(false), Some("Input"));
        assert_eq!(ThreadStatus::Failed.label(true), Some("Failed"));
    }
}
