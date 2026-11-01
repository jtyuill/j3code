//! t3's thread triage (the settle workflow) on [`AppThread`]: settle,
//! unsettle, snooze, unsnooze, the auto-settle opt-out, pin, unpin, pin and
//! active reorder, mark-unread, visit, the un-settle / un-snooze a sent
//! message causes, and the automatic settlement decision.
//!
//! Sources (t3code @ 8760d063): `apps/server/src/orchestration-v2/
//! Orchestrator.ts` (`dispatchThreadVisit` 2760-2818, `dispatchThreadMutation`
//! 3119-3530, `thread.auto-settle` 12875-12903, `dispatchMessage` 5353-5445)
//! and `ThreadSettlementService.ts` (96-245). Every mutation takes the clock
//! as `now_ms` so callers and tests decide what "now" is.
//!
//! [`AppThread`]: j3_protocol::t3::orchestration::AppThread

use j3_protocol::t3::{
    iso_from_unix_ms,
    orchestration::{RuntimeRequestStatus, SettledOverride, ShellThreadStatus, ThreadShell},
    unix_ms_from_iso,
};

use super::{Reject, ThreadModel};
use crate::engine::projection::Kind;

/// A user message this recent with no run after it is about to start a run
/// (`QUEUED_TURN_START_GRACE_MS`, ThreadSettlementService.ts:45).
pub const QUEUED_TURN_START_GRACE_MS: i64 = 2 * 60 * 1_000;
/// One day in milliseconds.
pub const DAY_MS: i64 = 24 * 60 * 60 * 1_000;

/// The wall clock in epoch milliseconds.
pub fn now_ms() -> i64 {
    i64::try_from(
        time::OffsetDateTime::now_utc()
            .unix_timestamp_nanos()
            .div_euclid(1_000_000),
    )
    .unwrap_or(i64::MAX)
}

fn is_set<T>(field: &Option<Option<T>>) -> bool {
    matches!(field, Some(Some(_)))
}

fn time_ms(field: &Option<Option<String>>) -> Option<i64> {
    field.as_ref()?.as_deref().and_then(unix_ms_from_iso)
}

fn latest_ms(values: impl IntoIterator<Item = Option<i64>>) -> Option<i64> {
    values.into_iter().flatten().max()
}

/// `TrimmedNonEmptyString` order keys.
fn order_key(key: &str) -> Result<String, Reject> {
    let key = key.trim();
    if key.is_empty() {
        return Err(Reject::new("The order key can't be empty."));
    }
    Ok(key.to_owned())
}

/// `threadHasQueuedTurnStart` (ThreadSettlementService.ts:96-119): a user
/// message sent within the grace window that no run has picked up yet.
/// The window is bounded on both sides against clock skew.
pub fn has_queued_turn_start(shell: &ThreadShell, now_ms: i64) -> bool {
    let Some(message_ms) = shell
        .latest_user_message_at
        .as_deref()
        .and_then(unix_ms_from_iso)
    else {
        return false;
    };
    if shell.status == ShellThreadStatus::Failed
        || (now_ms - message_ms).abs() > QUEUED_TURN_START_GRACE_MS
    {
        return false;
    }
    if shell.latest_run_id.is_none() {
        return true;
    }
    [
        &shell.latest_run_requested_at,
        &shell.latest_run_started_at,
        &shell.latest_run_completed_at,
    ]
    .into_iter()
    .all(|at| time_ms(at).is_none_or(|at| at < message_ms))
}

/// `isSnoozed` (ThreadSettlementService.ts:162-186), the server mirror of the
/// clients' `effectiveSnoozed`: the wake time is ahead and the thread has not
/// raised a hand (pending request, guest message, failure after snoozing).
pub fn is_snoozed(shell: &ThreadShell, now_ms: i64) -> bool {
    let Some(until_ms) = time_ms(&shell.snoozed_until) else {
        return false;
    };
    if until_ms <= now_ms
        || shell.pending_runtime_request.is_some()
        || shell.pending_guest_message_count.unwrap_or(0) > 0
    {
        return false;
    }
    let snoozed_ms = time_ms(&shell.snoozed_at);
    let completed_ms = time_ms(&shell.latest_run_completed_at);
    let woke_on_error = shell.status == ShellThreadStatus::Failed
        && match (snoozed_ms, completed_ms) {
            (None, _) => true,
            (Some(snoozed), Some(completed)) => completed > snoozed,
            (Some(_), None) => false,
        };
    !woke_on_error
}

/// `isAutoSettlementCandidate` (ThreadSettlementService.ts:140-159).
pub fn is_auto_settlement_candidate(shell: &ThreadShell, now_ms: i64) -> bool {
    if shell.archived_at.is_some() || shell.settled_override.is_some() {
        return false;
    }
    if is_set(&shell.pinned_at) || is_set(&shell.auto_settle_disabled_at) {
        return false;
    }
    if shell.pending_runtime_request.is_some()
        || shell.pending_guest_message_count.unwrap_or(0) > 0
        || is_set(&shell.activity_run_status)
        || !shell.pending_background_tasks.is_empty()
    {
        return false;
    }
    !has_queued_turn_start(shell, now_ms) && !is_snoozed(shell, now_ms)
}

/// `resolveAutoSettlementAt` (ThreadSettlementService.ts:190-245) without
/// pull requests: when the thread went quiet, if that is more than
/// `after_ms` ago and it may be settled automatically.
pub fn auto_settlement_at(
    shell: &ThreadShell,
    last_snooze_wake_at: Option<&str>,
    now_ms: i64,
    after_ms: i64,
) -> Option<i64> {
    if !is_auto_settlement_candidate(shell, now_ms) {
        return None;
    }
    let activity_ms = latest_ms([
        shell
            .latest_user_message_at
            .as_deref()
            .and_then(unix_ms_from_iso),
        time_ms(&shell.latest_run_requested_at),
        time_ms(&shell.latest_run_started_at),
        time_ms(&shell.latest_run_completed_at),
    ])
    .or_else(|| unix_ms_from_iso(&shell.created_at))?;
    let timer_wake_ms = time_ms(&shell.snoozed_until).filter(|until| *until <= now_ms);
    let inactive_ms = latest_ms([
        Some(activity_ms),
        last_snooze_wake_at.and_then(unix_ms_from_iso),
        timer_wake_ms,
    ])
    .unwrap_or(activity_ms);
    (inactive_ms < now_ms - after_ms).then_some(inactive_ms)
}

impl ThreadModel {
    /// Marks the thread changed with a triage event; `updated_at` moves only
    /// when t3 moves it.
    fn touch_triage(&mut self, event: &'static str, updated_at: Option<String>) {
        if let Some(at) = updated_at {
            self.proj.thread.updated_at = at;
        }
        let id = self.proj.thread.id.clone();
        self.proj.touch(Kind::Thread, &id, event);
    }

    fn require_unarchived(&self) -> Result<(), Reject> {
        if self.proj.thread.archived_at.is_some() {
            return Err(Reject::new("This thread is archived."));
        }
        Ok(())
    }

    fn has_pending_request(&self) -> bool {
        self.proj
            .requests
            .iter()
            .any(|request| request.status == RuntimeRequestStatus::Pending)
    }

    /// `thread.settle`. Refused while a question waits, a run is active or a
    /// message is queued (j3 has no automatic queued messages and every
    /// request blocks). Settling clears pin, order keys and snooze.
    pub fn settle(&mut self, now_ms: i64, settled_at: Option<&str>) -> Result<(), Reject> {
        self.require_unarchived()?;
        let settled_at = settled_at
            .map(|at| {
                unix_ms_from_iso(at)
                    .map(iso_from_unix_ms)
                    .ok_or_else(|| Reject::new("The settle time is not a valid timestamp."))
            })
            .transpose()?;
        if self.has_pending_request() {
            return Err(Reject::new(
                "This thread is waiting on an approval or question. Answer it before settling.",
            ));
        }
        if self.proj.active_run().is_some() {
            return Err(Reject::new(
                "This thread is still running. Stop it before settling.",
            ));
        }
        if !self.proj.queued_runs().is_empty() {
            return Err(Reject::new(
                "This thread has a queued message. Send it or remove it from the queue before settling.",
            ));
        }
        let now = iso_from_unix_ms(now_ms);
        let thread = &mut self.proj.thread;
        let already_settled = thread.settled_override == Some(SettledOverride::Settled)
            && thread.settled_at.is_some()
            && !is_set(&thread.pinned_at);
        let snoozed = is_set(&thread.snoozed_until) || is_set(&thread.snoozed_at);
        if !already_settled {
            thread.settled_at = Some(settled_at.unwrap_or_else(|| now.clone()));
        }
        thread.settled_override = Some(SettledOverride::Settled);
        thread.unsettled_at = Some(None);
        thread.pinned_at = Some(None);
        thread.pin_order_key = Some(None);
        thread.active_order_key = Some(None);
        thread.snoozed_until = Some(None);
        thread.snoozed_at = Some(None);
        let bump = !already_settled || snoozed;
        self.touch_triage("thread.settled", bump.then_some(now));
        Ok(())
    }

    /// `thread.unsettle`: back to active (`settledOverride: "active"`).
    pub fn unsettle(&mut self, now_ms: i64) -> Result<(), Reject> {
        self.require_unarchived()?;
        let now = iso_from_unix_ms(now_ms);
        let thread = &mut self.proj.thread;
        let already_active = thread.settled_override == Some(SettledOverride::Active);
        thread.settled_override = Some(SettledOverride::Active);
        thread.settled_at = None;
        thread.unsettled_at = Some(if already_active {
            thread.unsettled_at.clone().flatten()
        } else {
            Some(now.clone())
        });
        self.touch_triage("thread.unsettled", (!already_active).then_some(now));
        Ok(())
    }

    /// `thread.snooze` until `until`, which must be ahead of `now_ms`.
    /// Refused while a question waits or a message is queued.
    pub fn snooze(&mut self, now_ms: i64, until: &str) -> Result<(), Reject> {
        self.require_unarchived()?;
        let until_ms = unix_ms_from_iso(until)
            .filter(|until| *until > now_ms)
            .ok_or_else(|| Reject::new("The snooze time is not in the future."))?;
        if self.has_pending_request() {
            return Err(Reject::new(
                "This thread is waiting on an approval or question and can't be snoozed.",
            ));
        }
        if !self.proj.queued_runs().is_empty() {
            return Err(Reject::new(
                "This thread has a queued message and can't be snoozed.",
            ));
        }
        let now = iso_from_unix_ms(now_ms);
        let thread = &mut self.proj.thread;
        thread.snoozed_until = Some(Some(iso_from_unix_ms(until_ms)));
        thread.snoozed_at = Some(Some(now.clone()));
        self.touch_triage("thread.snoozed", Some(now));
        Ok(())
    }

    /// `thread.unsnooze`: wake now (`lastSnoozeWakeAt` restarts the
    /// auto-settle timer).
    pub fn unsnooze(&mut self, now_ms: i64) -> Result<(), Reject> {
        self.require_unarchived()?;
        let now = iso_from_unix_ms(now_ms);
        let thread = &mut self.proj.thread;
        let already_awake = !is_set(&thread.snoozed_until);
        thread.snoozed_until = Some(None);
        thread.snoozed_at = Some(None);
        if !already_awake {
            thread.last_snooze_wake_at = Some(Some(now.clone()));
        }
        self.touch_triage("thread.unsnoozed", (!already_awake).then_some(now));
        Ok(())
    }

    /// `thread.auto-settle.set`: the per-thread opt-out of automatic
    /// settlement (`autoSettleDisabledAt`).
    pub fn set_auto_settle(&mut self, now_ms: i64, enabled: bool) -> Result<(), Reject> {
        self.require_unarchived()?;
        let now = iso_from_unix_ms(now_ms);
        let thread = &mut self.proj.thread;
        let unchanged = enabled != is_set(&thread.auto_settle_disabled_at);
        thread.auto_settle_disabled_at = Some(if enabled {
            None
        } else {
            Some(
                thread
                    .auto_settle_disabled_at
                    .clone()
                    .flatten()
                    .unwrap_or_else(|| now.clone()),
            )
        });
        self.touch_triage("thread.auto-settle-set", (!unchanged).then_some(now));
        Ok(())
    }

    /// `thread.pin`, optionally with its first order key. Pinning promotes a
    /// settled thread to active and wakes a snoozed one.
    pub fn pin(&mut self, now_ms: i64, key: Option<&str>) -> Result<(), Reject> {
        self.require_unarchived()?;
        let key = key.map(order_key).transpose()?;
        let now = iso_from_unix_ms(now_ms);
        let thread = &mut self.proj.thread;
        let already_pinned = is_set(&thread.pinned_at);
        let settled = thread.settled_override == Some(SettledOverride::Settled);
        let snoozed = is_set(&thread.snoozed_until);
        if !already_pinned {
            thread.pinned_at = Some(Some(now.clone()));
            if let Some(key) = key {
                thread.pin_order_key = Some(Some(key));
            }
        }
        if settled {
            thread.settled_override = Some(SettledOverride::Active);
            thread.settled_at = None;
        }
        thread.snoozed_until = Some(None);
        thread.snoozed_at = Some(None);
        if snoozed {
            thread.last_snooze_wake_at = Some(Some(now.clone()));
        }
        let bump = !already_pinned || settled || snoozed;
        self.touch_triage("thread.pinned", bump.then_some(now));
        Ok(())
    }

    /// `thread.unpin`.
    pub fn unpin(&mut self, now_ms: i64) -> Result<(), Reject> {
        self.require_unarchived()?;
        let thread = &mut self.proj.thread;
        let already_unpinned = !is_set(&thread.pinned_at);
        thread.pinned_at = Some(None);
        thread.pin_order_key = Some(None);
        self.touch_triage(
            "thread.unpinned",
            (!already_unpinned).then(|| iso_from_unix_ms(now_ms)),
        );
        Ok(())
    }

    /// `thread.pin.reorder`; refused unless pinned, so a raced reorder can't
    /// restore a cleared pin.
    pub fn reorder_pinned(&mut self, now_ms: i64, key: &str) -> Result<(), Reject> {
        self.require_unarchived()?;
        let key = order_key(key)?;
        if !is_set(&self.proj.thread.pinned_at) {
            return Err(Reject::new(
                "This thread is not pinned, so it can't be reordered.",
            ));
        }
        let thread = &mut self.proj.thread;
        let changed = thread.pin_order_key.as_ref().and_then(Option::as_ref) != Some(&key);
        thread.pin_order_key = Some(Some(key));
        self.touch_triage(
            "thread.pin-reordered",
            changed.then(|| iso_from_unix_ms(now_ms)),
        );
        Ok(())
    }

    /// `thread.active.reorder`; refused for pinned or settled threads. Never
    /// moves `updatedAt`.
    pub fn reorder_active(&mut self, key: &str) -> Result<(), Reject> {
        self.require_unarchived()?;
        let key = order_key(key)?;
        let thread = &self.proj.thread;
        if is_set(&thread.pinned_at) || thread.settled_override == Some(SettledOverride::Settled) {
            return Err(Reject::new(
                "This thread is not active, so it can't be reordered.",
            ));
        }
        self.proj.thread.active_order_key = Some(Some(key));
        self.touch_triage("thread.active-reordered", None);
        Ok(())
    }

    /// `thread.mark-unread`: rewinds the read watermark to just before the
    /// latest run completed. A snooze whose wake already passed is retired
    /// first, so the wake does not fire again.
    pub fn mark_unread(&mut self, now_ms: i64) -> Result<(), Reject> {
        let completed_ms = self
            .proj
            .latest_run()
            .and_then(|run| run.completed_at.as_deref())
            .and_then(unix_ms_from_iso)
            .ok_or_else(|| Reject::new("This thread has no finished run to mark unread."))?;
        let thread = &mut self.proj.thread;
        thread.last_visited_at = Some(iso_from_unix_ms(completed_ms - 1));
        if time_ms(&thread.snoozed_until).is_some_and(|until| until <= now_ms) {
            thread.last_snooze_wake_at = thread.snoozed_until.take();
            thread.snoozed_until = Some(None);
            thread.snoozed_at = Some(None);
        }
        self.touch_triage("thread.marked-unread", None);
        Ok(())
    }

    /// `thread.visit`: the read watermark only moves forward. Never moves
    /// `updatedAt`.
    pub fn visit(&mut self, visited_at: &str) -> Result<(), Reject> {
        let visited_ms = unix_ms_from_iso(visited_at)
            .ok_or_else(|| Reject::new("The visit time is not a valid timestamp."))?;
        let thread = &mut self.proj.thread;
        let moves_forward = thread
            .last_visited_at
            .as_deref()
            .and_then(unix_ms_from_iso)
            .is_none_or(|last| visited_ms > last);
        if moves_forward {
            thread.last_visited_at = Some(iso_from_unix_ms(visited_ms));
        }
        self.touch_triage("thread.visited", None);
        Ok(())
    }

    /// What an accepted user message does to triage state
    /// (`dispatchMessage`): the first one sets the read watermark (just
    /// before now, so a completion in the same millisecond is unread), and
    /// any one un-settles and wakes the thread. j3 has no automatic wakes.
    pub fn on_user_message(&mut self, now_ms: i64) {
        let now = iso_from_unix_ms(now_ms);
        if self.proj.thread.last_visited_at.is_none() {
            self.proj.thread.last_visited_at = Some(iso_from_unix_ms(now_ms - 1));
            self.touch_triage("thread.metadata-updated", None);
        }
        let thread = &mut self.proj.thread;
        if let Some(previous) = thread.settled_override {
            thread.settled_override = None;
            thread.settled_at = None;
            thread.unsettled_at = Some(match previous {
                SettledOverride::Active => thread.unsettled_at.clone().flatten(),
                SettledOverride::Settled => Some(now.clone()),
            });
            self.touch_triage("thread.unsettled", Some(now.clone()));
        }
        let thread = &mut self.proj.thread;
        if is_set(&thread.snoozed_until) {
            thread.snoozed_until = Some(None);
            thread.snoozed_at = Some(None);
            self.touch_triage("thread.unsnoozed", Some(now));
        }
    }

    /// When this thread should be settled automatically, if it is due:
    /// inactive for longer than `after_ms` as of `now_ms`.
    pub fn auto_settlement_at(&self, now_ms: i64, after_ms: i64) -> Option<i64> {
        auto_settlement_at(
            &self.proj.shell(),
            self.proj
                .thread
                .last_snooze_wake_at
                .as_ref()
                .and_then(Option::as_deref),
            now_ms,
            after_ms,
        )
    }

    /// `thread.auto-settle`: settles at `settled_ms` unless the thread
    /// changed after `snapshot_at` (the sweep's view) or has an explicit
    /// settled override, so user actions win over the sweep.
    pub fn auto_settle(
        &mut self,
        now_ms: i64,
        snapshot_at: &str,
        settled_ms: i64,
    ) -> Result<(), Reject> {
        let changed = match (
            unix_ms_from_iso(&self.proj.updated_at()),
            unix_ms_from_iso(snapshot_at),
        ) {
            (Some(updated), Some(snapshot)) => updated > snapshot,
            _ => true,
        };
        if self.proj.thread.settled_override.is_some() || changed {
            return Err(Reject::new(
                "This thread changed before automatic settlement.",
            ));
        }
        self.settle(now_ms, Some(&iso_from_unix_ms(settled_ms)))
    }
}

#[cfg(test)]
mod tests {
    use j3_protocol::t3::orchestration::{
        Actor, CreationSource, PendingRuntimeRequestSummary, ProviderInteractionMode, RuntimeMode,
        RuntimeRequestKind,
    };
    use serde_json::json;

    use super::*;
    use crate::{
        engine::model::{NewThread, UserInput},
        omp::omp_rpc::{Event, RpcNotification},
    };

    const HOUR_MS: i64 = 60 * 60 * 1_000;

    fn model() -> ThreadModel {
        ThreadModel::create(NewThread {
            id: "T".into(),
            project_id: "P".into(),
            title: "Triage".into(),
            model_selection: serde_json::from_value(json!({"instanceId": "omp", "model": "a/b"}))
                .unwrap(),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: ProviderInteractionMode::Default,
            created_by: Actor::User,
            creation_source: CreationSource::Mobile,
            cwd: "/tmp".into(),
        })
    }

    /// A thread whose one run has completed; returns it with the run's
    /// completion time.
    fn finished() -> (ThreadModel, i64) {
        let mut model = model();
        let selection = model.proj.thread.model_selection.clone();
        model.start_run(&UserInput {
            message_id: "m1".into(),
            text: "hi".into(),
            attachments: Vec::new(),
            context: None,
            model_selection: selection,
            created_by: Actor::User,
            creation_source: CreationSource::Mobile,
        });
        let agent_start = RpcNotification::from_value(json!({"type": "agent_start"})).unwrap();
        model.apply(&Event::Notification(agent_start));
        model.complete_without_agent();
        let completed = model
            .proj
            .latest_run()
            .and_then(|run| run.completed_at.as_deref())
            .and_then(unix_ms_from_iso)
            .unwrap();
        (model, completed)
    }

    #[test]
    fn queued_turn_start_grace() {
        let (model, _) = finished();
        let mut shell = model.proj.shell();
        let now = unix_ms_from_iso(&shell.created_at).unwrap() + 10 * HOUR_MS;
        shell.latest_user_message_at = Some(iso_from_unix_ms(now - 1_000));
        // The latest run started before the message: it is still queued.
        assert!(has_queued_turn_start(&shell, now));
        shell.latest_run_id = None;
        assert!(has_queued_turn_start(&shell, now));
        // Outside the grace window on either side.
        assert!(!has_queued_turn_start(
            &shell,
            now + QUEUED_TURN_START_GRACE_MS
        ));
        assert!(!has_queued_turn_start(
            &shell,
            now - QUEUED_TURN_START_GRACE_MS - 2_000
        ));
        shell.latest_run_id = Some("run".into());
        shell.latest_run_requested_at = Some(Some(iso_from_unix_ms(now)));
        assert!(!has_queued_turn_start(&shell, now));
        shell.latest_run_requested_at = Some(None);
        shell.status = ShellThreadStatus::Failed;
        assert!(!has_queued_turn_start(&shell, now));
    }

    #[test]
    fn snooze_mirror_wakes_on_requests_and_failures() {
        let (model, completed) = finished();
        let mut shell = model.proj.shell();
        let now = completed + HOUR_MS;
        shell.snoozed_until = Some(Some(iso_from_unix_ms(now + HOUR_MS)));
        shell.snoozed_at = Some(Some(iso_from_unix_ms(completed + 1)));
        assert!(is_snoozed(&shell, now));
        assert!(!is_snoozed(&shell, now + HOUR_MS), "the timer expired");
        // A failure before snoozing does not wake it; one after does.
        shell.status = ShellThreadStatus::Failed;
        assert!(is_snoozed(&shell, now));
        shell.snoozed_at = Some(Some(iso_from_unix_ms(completed - 1)));
        assert!(!is_snoozed(&shell, now));
        shell.snoozed_at = Some(None);
        assert!(!is_snoozed(&shell, now));
        shell.status = ShellThreadStatus::Completed;
        shell.pending_runtime_request = Some(PendingRuntimeRequestSummary {
            id: "rq".into(),
            kind: RuntimeRequestKind::UserInput,
            created_at: iso_from_unix_ms(now),
            blocking: Some(true),
        });
        assert!(!is_snoozed(&shell, now));
    }

    #[test]
    fn auto_settlement_waits_for_inactivity_and_snooze_wakes() {
        let (mut model, completed) = finished();
        let after = 3 * DAY_MS;
        assert_eq!(model.auto_settlement_at(completed + after, after), None);
        assert_eq!(
            model.auto_settlement_at(completed + after + 1, after),
            Some(completed)
        );
        // An expired snooze timer counts as activity at its wake time.
        let wake = completed + DAY_MS;
        model.proj.thread.snoozed_until = Some(Some(iso_from_unix_ms(wake)));
        assert_eq!(model.auto_settlement_at(completed + after + 1, after), None);
        assert_eq!(
            model.auto_settlement_at(wake + after + 1, after),
            Some(wake)
        );
        // So does the last explicit wake.
        model.proj.thread.snoozed_until = Some(None);
        model.proj.thread.last_snooze_wake_at = Some(Some(iso_from_unix_ms(wake + 5)));
        assert_eq!(model.auto_settlement_at(wake + after + 1, after), None);
        assert_eq!(
            model.auto_settlement_at(wake + after + 6, after),
            Some(wake + 5)
        );
        // Opted out, pinned or explicitly overridden threads never settle.
        let late = completed + 30 * DAY_MS;
        model.set_auto_settle(late, false).unwrap();
        assert_eq!(model.auto_settlement_at(late, after), None);
        model.set_auto_settle(late, true).unwrap();
        model.pin(late, None).unwrap();
        assert_eq!(model.auto_settlement_at(late, after), None);
        model.unpin(late).unwrap();
        model.unsettle(late).unwrap();
        assert_eq!(model.auto_settlement_at(late + after + 1, after), None);
    }

    #[test]
    fn auto_settle_yields_to_changes_after_the_snapshot() {
        let (mut model, completed) = finished();
        let now = completed + 4 * DAY_MS;
        let snapshot = model.proj.updated_at();
        let stale = iso_from_unix_ms(unix_ms_from_iso(&snapshot).unwrap() - 1);
        assert_eq!(
            model.auto_settle(now, &stale, completed),
            Err(Reject::new(
                "This thread changed before automatic settlement."
            ))
        );
        model.auto_settle(now, &snapshot, completed).unwrap();
        assert_eq!(
            model.proj.thread.settled_override,
            Some(SettledOverride::Settled)
        );
        assert_eq!(
            model.proj.thread.settled_at,
            Some(iso_from_unix_ms(completed))
        );
        // Settled threads keep their override against the sweep.
        let snapshot = model.proj.updated_at();
        assert!(model.auto_settle(now, &snapshot, completed).is_err());
    }
}
