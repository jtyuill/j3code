//! Payload and success shapes of the orchestration RPC methods
//! (`ORCHESTRATION_V2_WS_METHODS`, `C/orchestrationV2.ts` = `OV2`). Command
//! dispatch and launch live in [`super::commands`]; non-orchestration
//! methods live with their contracts module ([`super::server`],
//! [`super::project`], [`super::filesystem`], [`super::assets`],
//! [`super::background`]).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::True;
use super::ids::*;
use super::orchestration::{ThreadProjection, ThreadShell, TurnItem};
use super::project::ProjectShell;
use super::thread_search::{OrchestrationSearchThreadsInput, OrchestrationSearchThreadsResult};

/// `{}`: payload of `orchestration.getArchivedShellSnapshot` (and any other
/// `Schema.Struct({})` input).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EmptyInput {}

/// M4 payload: `OrchestrationV2SubscribeShellInput` (`OV2:3649-3662`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeShellInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_completion_marker: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omit_idle_subagents: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omit_older_settled: Option<bool>,
}

/// M5 payload: `OrchestrationV2SubscribeThreadInput` (`OV2:3747-3757`). Every
/// `accept*` flag is an opt-in j3 ignores.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeThreadInput {
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_completion_marker: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept_bounded_snapshot: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept_deferred_work: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept_compact_turn_items: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept_deduped_bodies: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept_compact_ids: Option<bool>,
}

/// M8 payload: `OrchestrationV2GetThreadProjectionInput` (`OV2:3643-3645`).
/// Success: [`ThreadProjection`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetThreadProjectionInput {
    pub thread_id: ThreadId,
}

/// M9 success: `OrchestrationV2ArchivedShellSnapshot` (`OV2:3598-3603`).
/// `threads` holds the archived threads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchivedShellSnapshot {
    /// PositiveInt.
    pub schema_version: u64,
    pub snapshot_sequence: u64,
    pub projects: Vec<ProjectShell>,
    pub threads: Vec<ThreadShell>,
}

/// M16 payload: `OrchestrationV2GetTurnItemInput` (`OV2:4034-4039`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTurnItemInput {
    pub thread_id: ThreadId,
    pub item_id: TurnItemId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

/// M16 success: `OrchestrationV2GetTurnItemResult` (`OV2:4041-4043`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetTurnItemResult {
    pub item: Option<TurnItem>,
}

/// M17 payload: `OrchestrationV2SearchThreadsInput` (`OV2:3693-3702`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchThreadsInput {
    #[serde(flatten)]
    pub base: OrchestrationSearchThreadsInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub older_settled_before: Option<IsoDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_ids: Option<Vec<ProjectId>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub titles_only: Option<bool>,
}

/// M17 success: `OrchestrationV2SearchThreadsResult` (`OV2:3705-3709`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchThreadsResult {
    #[serde(flatten)]
    pub base: OrchestrationSearchThreadsResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threads: Option<Vec<ThreadShell>>,
}

/// M18 payload: `OrchestrationV2ForceStopThreadInput` (`OV2:3951-3955`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForceStopThreadInput {
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
}

/// `outcome` of [`ForceStopThreadResult`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForceStopOutcome {
    Killed,
    Released,
    Releasing,
    Shared,
}

/// M18 success: `OrchestrationV2ForceStopThreadResult` (`OV2:3958-3967`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForceStopThreadResult {
    pub outcome: ForceStopOutcome,
    pub shared_with_thread_ids: Vec<ThreadId>,
}

/// `reason` of [`OrchestrationV2ForceStopThreadError`]: only `"superseded"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ForceStopErrorReason {
    #[serde(rename = "superseded")]
    Superseded,
}

json_literal! {
    pub struct ForceStopThreadErrorTag = str("OrchestrationV2ForceStopThreadError");
}

/// `OrchestrationV2ForceStopThreadError` (`OV2:3970-3984`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ForceStopThreadError {
    #[serde(rename = "_tag")]
    pub tag: ForceStopThreadErrorTag,
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<ForceStopErrorReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<Value>,
}

impl OrchestrationV2ForceStopThreadError {
    pub fn new(thread_id: impl Into<ThreadId>) -> Self {
        Self {
            tag: ForceStopThreadErrorTag,
            thread_id: thread_id.into(),
            reason: None,
            cause: None,
        }
    }
}

/// H7 success: `OrchestrationV2ThreadBoundedSnapshot` (`OV2:3777-3793`), for
/// `GET /api/orchestration/threads/:id/bounded` if it is ever implemented.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadBoundedSnapshot {
    pub snapshot_sequence: u64,
    pub projection: ThreadProjection,
    pub history_cursor: Option<String>,
    pub has_more_history: bool,
    pub latest_local_turn_ordinal: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_items_omit_local_visible: Option<True>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_table: Option<True>,
}
