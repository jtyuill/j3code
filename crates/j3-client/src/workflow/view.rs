//! The client-side reading of a shell row, ported from t3's
//! `presentThreadShell` (`packages/client-runtime/src/state/models.ts:223-350`):
//! the runtime summary, the latest run, and the pending-request flags every
//! derivation reads.

use j3_protocol::t3::{
    self,
    orchestration::{
        ActivityRunStatus, ProviderFailureClass, RuntimeRequestKind, ShellThreadStatus, ThreadShell,
    },
};
use serde_json::Value;
use time::OffsetDateTime;

pub(crate) type Status = ShellThreadStatus;

/// `Date.parse` for wire timestamps: epoch milliseconds, `None` where JS
/// would get `NaN`.
pub(crate) fn ms(iso: &str) -> Option<i64> {
    t3::unix_ms_from_iso(iso)
}

pub(crate) fn opt_ms(iso: Option<&str>) -> Option<i64> {
    iso.and_then(ms)
}

pub(crate) fn now_ms(now: OffsetDateTime) -> i64 {
    i64::try_from(now.unix_timestamp_nanos().div_euclid(1_000_000)).unwrap_or(i64::MAX)
}

pub(crate) fn instant(iso: &str) -> Option<OffsetDateTime> {
    let ms = ms(iso)?;
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).ok()
}

/// A `T | null | undefined` field as `T | null`.
pub(crate) fn flat(value: &Option<Option<String>>) -> Option<&str> {
    value.as_ref().and_then(|value| value.as_deref())
}

/// `preparing | queued | starting | running | waiting`
/// (`threadRunStatusIsActive`, models.ts:127-135).
pub(crate) fn is_active(status: Status) -> bool {
    matches!(
        status,
        Status::Preparing | Status::Queued | Status::Starting | Status::Running | Status::Waiting
    )
}

/// `terminalRunStatus` (models.ts:209-217).
fn is_terminal(status: Status) -> bool {
    matches!(
        status,
        Status::Completed
            | Status::Interrupted
            | Status::Failed
            | Status::Cancelled
            | Status::RolledBack
    )
}

/// `backgroundWorkHoldsCompletion`
/// (`packages/shared/src/orchestrationV2PendingBackgroundWork.ts:89-108`):
/// commands and monitors hold only when awaited; subagents and background
/// tasks always do.
pub(crate) fn background_work_holds_completion(tasks: &[Value]) -> bool {
    tasks.iter().any(|task| {
        task.get("awaited").and_then(Value::as_bool) == Some(true)
            || matches!(
                task.get("kind").and_then(Value::as_str),
                Some("subagent" | "background_task")
            )
    })
}

/// `ThreadRuntimeSummary`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Runtime<'a> {
    pub status: Status,
    pub active_run_id: Option<&'a str>,
    /// Outer `None` is `undefined` (an older server), inner `None` is `null`.
    pub activity_started_at: Option<Option<&'a str>>,
    pub last_error_class: Option<ProviderFailureClass>,
    pub updated_at: &'a str,
}

/// `ThreadRunSummary`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Run<'a> {
    pub run_id: &'a str,
    pub status: Status,
    pub requested_at: Option<&'a str>,
    pub started_at: Option<&'a str>,
    pub completed_at: Option<&'a str>,
}

/// The parts of t3's `EnvironmentThreadShell` the workflow reads.
#[derive(Debug, Clone, Copy)]
pub(crate) struct View<'a> {
    pub shell: &'a ThreadShell,
    pub runtime: Option<Runtime<'a>>,
    pub latest_run: Option<Run<'a>>,
    pub has_pending_approvals: bool,
    pub has_pending_user_input: bool,
    pub pending_guest_message_count: u64,
}

impl<'a> View<'a> {
    pub fn of(shell: &'a ThreadShell) -> Self {
        let latest_run = shell.latest_run_id.as_deref().map(|run_id| Run {
            run_id,
            status: if shell.status == Status::Idle {
                Status::Completed
            } else {
                shell.status
            },
            requested_at: flat(&shell.latest_run_requested_at),
            started_at: flat(&shell.latest_run_started_at),
            completed_at: match &shell.latest_run_completed_at {
                None if shell.status == Status::Idle || is_terminal(shell.status) => {
                    Some(shell.updated_at.as_str())
                }
                None => None,
                Some(completed_at) => completed_at.as_deref(),
            },
        });
        let request = shell.pending_runtime_request.as_ref().map(|r| r.kind);
        Self {
            shell,
            runtime: runtime(shell),
            latest_run,
            has_pending_approvals: request.is_some_and(|kind| {
                kind != RuntimeRequestKind::UserInput && kind != RuntimeRequestKind::AuthRefresh
            }),
            has_pending_user_input: request == Some(RuntimeRequestKind::UserInput),
            pending_guest_message_count: shell.pending_guest_message_count.unwrap_or(0),
        }
    }

    pub fn has_pending_runtime_request(&self) -> bool {
        self.shell.pending_runtime_request.is_some()
    }

    pub fn snoozed_until(&self) -> Option<&'a str> {
        flat(&self.shell.snoozed_until)
    }

    pub fn snoozed_at(&self) -> Option<&'a str> {
        flat(&self.shell.snoozed_at)
    }

    /// Outer `None`: the server predates visit tracking.
    pub fn last_visited_at(&self) -> Option<Option<&'a str>> {
        self.shell.last_visited_at.as_ref().map(Option::as_deref)
    }
}

/// `shellRuntime` (models.ts:223-246).
fn runtime(shell: &ThreadShell) -> Option<Runtime<'_>> {
    let park_at_idle = background_work_holds_completion(&shell.pending_background_tasks)
        && shell.status != Status::Failed;
    if shell.latest_run_id.is_none() && shell.active_provider_thread_id.is_none() && !park_at_idle {
        return None;
    }
    let activity = shell
        .activity_run_status
        .flatten()
        .map(|status| match status {
            ActivityRunStatus::Preparing => Status::Preparing,
            ActivityRunStatus::Starting => Status::Starting,
            ActivityRunStatus::Running => Status::Running,
            ActivityRunStatus::Waiting => Status::Waiting,
        });
    Some(Runtime {
        status: if park_at_idle {
            Status::Idle
        } else {
            activity.unwrap_or(shell.status)
        },
        active_run_id: shell.active_run_id.as_deref(),
        activity_started_at: shell.activity_run_started_at.as_ref().map(Option::as_deref),
        last_error_class: shell.last_error_class.flatten(),
        updated_at: &shell.updated_at,
    })
}
