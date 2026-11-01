//! `orchestration.dispatchCommand` and `orchestration.launchThread`
//! (`C/orchestrationV2.ts`, `OV2` below; ios-surface.md §3).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::chat_attachment::ChatAttachment;
use super::double_option;
use super::ids::*;
use super::model::ModelSelection;
use super::orchestration::{
    Actor, CreationSource, IdleCompaction, MessageContext, ProviderApprovalDecision,
    ProviderInteractionMode, ProviderUserInputAnswers, RuntimeMode, SourcePlanRef,
    ThreadProjection, UserInputAttachments, WorkspaceStrategy,
};

// ---------------------------------------------------------------------------
// Required commands (§3.4)
// ---------------------------------------------------------------------------

/// `deliveryIntent` of `message.dispatch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeliveryIntent {
    Auto,
    Steer,
    Restart,
}

/// `dispatchMode` of `message.dispatch` (`OV2:3149-3160`), by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DispatchMode {
    #[serde(rename = "defer_start", rename_all = "camelCase")]
    DeferStart {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_strategy: Option<WorkspaceStrategy>,
    },
    #[serde(rename = "steer_active", rename_all = "camelCase")]
    SteerActive { target_run_id: RunId },
    #[serde(rename = "restart_active", rename_all = "camelCase")]
    RestartActive { target_run_id: RunId },
    #[serde(rename = "queue_after_active")]
    QueueAfterActive,
    #[serde(rename = "start_immediately")]
    StartImmediately,
}

/// D1 `message.dispatch` (`OV2:3121-3161`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageDispatchCommand {
    /// `OrchestrationV2Notification`; opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_message_id: Option<GuestMessageId>,
    pub created_by: Actor,
    pub creation_source: CreationSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_task_id: Option<ScheduledTaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_thread_id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_compaction: Option<IdleCompaction>,
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub message_id: MessageId,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<MessageContext>,
    pub attachments: Vec<ChatAttachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_seed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_selection: Option<ModelSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_plan_ref: Option<SourcePlanRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_continuation_of_run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_limit_continuation_of_run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_continuation_of_run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_limit_recovery_request_id: Option<CommandId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_intent: Option<DeliveryIntent>,
    /// `{parentRunId, generation, taskIds}`; opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_completion: Option<Value>,
    pub dispatch_mode: DispatchMode,
}

/// D2 `run.interrupt` (`OV2:3201-3208`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunInterruptCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub run_id: RunId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_queue: Option<bool>,
}

/// D3 `thread.stop` (`OV2:2878-2888`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStopCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_delegated_tasks: Option<bool>,
}

/// `{type, commandId, threadId}`: D4 `thread.archive`, D5 `thread.unarchive`,
/// D15 `queue.resume`, `thread.unpin`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
}

/// D6 `thread.delete` (`OV2:2937-2943`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadDeleteCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only_if_unused: Option<bool>,
}

/// D7 `thread.metadata.update` (`OV2:3020-3035`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadMetadataUpdateCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regenerate_title: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub branch: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub worktree_path: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub expected_worktree_path: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub expected_branch: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_empty: Option<bool>,
    /// `OrchestrationV2LimitRecoveryUpdate | null`; opaque.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub limit_recovery: Option<Option<Value>>,
    /// `ThreadLinkedPullRequest | null`; opaque.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub linked_pull_request: Option<Option<Value>>,
}

/// D8 `thread.runtime-mode.set` (`OV2:3096-3101`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadRuntimeModeSetCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub runtime_mode: RuntimeMode,
}

/// D9 `thread.interaction-mode.set` (`OV2:3102-3107`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadInteractionModeSetCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub interaction_mode: ProviderInteractionMode,
}

/// D10 `runtime-request.respond` (`OV2:3250-3259`): a `decision` (+ `feedback`)
/// for approvals, or `answers` (+ `attachmentsByQuestionId`) for questions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRequestRespondCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub request_id: RuntimeRequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<ProviderApprovalDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
    /// Question id → `string | string[]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<ProviderUserInputAnswers>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments_by_question_id: Option<UserInputAttachments>,
}

/// D11 `thread.user-input.dismiss` (`OV2:3260-3265`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputDismissCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub request_id: RuntimeRequestId,
}

/// `{type, commandId, threadId, runId}`: D12 `queued-run.cancel`, D16
/// `queued-run.start`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedRunCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub run_id: RunId,
}

/// D13 `queued-run.edit` (`OV2:3240-3249`). Absent `attachments` keeps the
/// existing ones; present replaces all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedRunEditCommand {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<MessageContext>,
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub run_id: RunId,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<ChatAttachment>>,
}

/// D14 `queued-message.promote-to-steer` (`OV2:3209-3215`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromoteToSteerCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub queued_run_id: RunId,
    pub target_run_id: RunId,
}

/// D17 `thread.visit` (`OV2:3005-3014`). `searchQuery` at most 200 chars.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadVisitCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub visited_at: IsoDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_query: Option<String>,
}

// ---------------------------------------------------------------------------
// No-op commands (§3.5)
// ---------------------------------------------------------------------------

/// `thread.settle` (`OV2:2945-2950`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSettleCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<IsoDateTime>,
}

/// `reason` of `thread.unsettle` / `thread.unsnooze`: only `"user"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum UserReason {
    #[default]
    #[serde(rename = "user")]
    User,
}

/// `thread.unsettle` (`OV2:2959-2964`) and `thread.unsnooze` (`OV2:2971-2976`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadUserReasonCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub reason: UserReason,
}

/// `thread.snooze` (`OV2:2965-2970`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSnoozeCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub snoozed_until: IsoDateTime,
}

/// `thread.auto-settle.set` (`OV2:2977-2982`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadAutoSettleSetCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub enabled: bool,
}

/// `thread.pin` (`OV2:2983-2988`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadPinCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_key: Option<String>,
}

/// `thread.pin.reorder` / `thread.active.reorder` (`OV2:2994-3004`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadReorderCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub order_key: String,
}

open_union! {
    /// `OrchestrationV2Command` (`OV2:2877-3347`), the `dispatchCommand`
    /// payload, by `type`. The 17 required and 9 no-op commands j3 accepts
    /// are typed; every other type (the 29 cut ones, future ones) decodes to
    /// `Unknown` and must be rejected with [`OrchestrationV2DispatchCommandError`].
    pub enum OrchestrationV2Command {
        tag = "type";
        newtype {
            "message.dispatch" => MessageDispatch(MessageDispatchCommand),
            "run.interrupt" => RunInterrupt(RunInterruptCommand),
            "thread.stop" => ThreadStop(ThreadStopCommand),
            "thread.archive" => ThreadArchive(ThreadCommand),
            "thread.unarchive" => ThreadUnarchive(ThreadCommand),
            "thread.delete" => ThreadDelete(ThreadDeleteCommand),
            "thread.metadata.update" => ThreadMetadataUpdate(ThreadMetadataUpdateCommand),
            "thread.runtime-mode.set" => ThreadRuntimeModeSet(ThreadRuntimeModeSetCommand),
            "thread.interaction-mode.set" => ThreadInteractionModeSet(ThreadInteractionModeSetCommand),
            "runtime-request.respond" => RuntimeRequestRespond(RuntimeRequestRespondCommand),
            "thread.user-input.dismiss" => UserInputDismiss(UserInputDismissCommand),
            "queued-run.cancel" => QueuedRunCancel(QueuedRunCommand),
            "queued-run.edit" => QueuedRunEdit(QueuedRunEditCommand),
            "queued-message.promote-to-steer" => QueuedMessagePromoteToSteer(PromoteToSteerCommand),
            "queue.resume" => QueueResume(ThreadCommand),
            "queued-run.start" => QueuedRunStart(QueuedRunCommand),
            "thread.visit" => ThreadVisit(ThreadVisitCommand),
            "thread.settle" => ThreadSettle(ThreadSettleCommand),
            "thread.unsettle" => ThreadUnsettle(ThreadUserReasonCommand),
            "thread.snooze" => ThreadSnooze(ThreadSnoozeCommand),
            "thread.unsnooze" => ThreadUnsnooze(ThreadUserReasonCommand),
            "thread.auto-settle.set" => ThreadAutoSettleSet(ThreadAutoSettleSetCommand),
            "thread.pin" => ThreadPin(ThreadPinCommand),
            "thread.unpin" => ThreadUnpin(ThreadCommand),
            "thread.pin.reorder" => ThreadPinReorder(ThreadReorderCommand),
            "thread.active.reorder" => ThreadActiveReorder(ThreadReorderCommand),
        }
        unit {}
    }
}

impl OrchestrationV2Command {
    /// `commandId`; for `Unknown`, read from the raw body if it is a string.
    pub fn command_id(&self) -> Option<&str> {
        Some(match self {
            Self::MessageDispatch(c) => &c.command_id,
            Self::RunInterrupt(c) => &c.command_id,
            Self::ThreadStop(c) => &c.command_id,
            Self::ThreadArchive(c)
            | Self::ThreadUnarchive(c)
            | Self::QueueResume(c)
            | Self::ThreadUnpin(c) => &c.command_id,
            Self::ThreadDelete(c) => &c.command_id,
            Self::ThreadMetadataUpdate(c) => &c.command_id,
            Self::ThreadRuntimeModeSet(c) => &c.command_id,
            Self::ThreadInteractionModeSet(c) => &c.command_id,
            Self::RuntimeRequestRespond(c) => &c.command_id,
            Self::UserInputDismiss(c) => &c.command_id,
            Self::QueuedRunCancel(c) | Self::QueuedRunStart(c) => &c.command_id,
            Self::QueuedRunEdit(c) => &c.command_id,
            Self::QueuedMessagePromoteToSteer(c) => &c.command_id,
            Self::ThreadVisit(c) => &c.command_id,
            Self::ThreadSettle(c) => &c.command_id,
            Self::ThreadUnsettle(c) | Self::ThreadUnsnooze(c) => &c.command_id,
            Self::ThreadSnooze(c) => &c.command_id,
            Self::ThreadAutoSettleSet(c) => &c.command_id,
            Self::ThreadPin(c) => &c.command_id,
            Self::ThreadPinReorder(c) | Self::ThreadActiveReorder(c) => &c.command_id,
            Self::Unknown(value) => return value.get("commandId").and_then(Value::as_str),
        })
    }

    /// `threadId` (every typed command has one); for `Unknown`, read from the
    /// raw body if it is a string.
    pub fn thread_id(&self) -> Option<&str> {
        Some(match self {
            Self::MessageDispatch(c) => &c.thread_id,
            Self::RunInterrupt(c) => &c.thread_id,
            Self::ThreadStop(c) => &c.thread_id,
            Self::ThreadArchive(c)
            | Self::ThreadUnarchive(c)
            | Self::QueueResume(c)
            | Self::ThreadUnpin(c) => &c.thread_id,
            Self::ThreadDelete(c) => &c.thread_id,
            Self::ThreadMetadataUpdate(c) => &c.thread_id,
            Self::ThreadRuntimeModeSet(c) => &c.thread_id,
            Self::ThreadInteractionModeSet(c) => &c.thread_id,
            Self::RuntimeRequestRespond(c) => &c.thread_id,
            Self::UserInputDismiss(c) => &c.thread_id,
            Self::QueuedRunCancel(c) | Self::QueuedRunStart(c) => &c.thread_id,
            Self::QueuedRunEdit(c) => &c.thread_id,
            Self::QueuedMessagePromoteToSteer(c) => &c.thread_id,
            Self::ThreadVisit(c) => &c.thread_id,
            Self::ThreadSettle(c) => &c.thread_id,
            Self::ThreadUnsettle(c) | Self::ThreadUnsnooze(c) => &c.thread_id,
            Self::ThreadSnooze(c) => &c.thread_id,
            Self::ThreadAutoSettleSet(c) => &c.thread_id,
            Self::ThreadPin(c) => &c.thread_id,
            Self::ThreadPinReorder(c) | Self::ThreadActiveReorder(c) => &c.thread_id,
            Self::Unknown(value) => return value.get("threadId").and_then(Value::as_str),
        })
    }

    /// True for the §3.5 triage commands that only need to succeed.
    pub fn is_no_op(&self) -> bool {
        matches!(
            self,
            Self::ThreadSettle(_)
                | Self::ThreadUnsettle(_)
                | Self::ThreadSnooze(_)
                | Self::ThreadUnsnooze(_)
                | Self::ThreadAutoSettleSet(_)
                | Self::ThreadPin(_)
                | Self::ThreadUnpin(_)
                | Self::ThreadPinReorder(_)
                | Self::ThreadActiveReorder(_)
        )
    }
}

/// `OrchestrationV2DispatchCommandResult` (`OV2:3638-3640`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchCommandResult {
    pub sequence: u64,
}

// ---------------------------------------------------------------------------
// launchThread (§3.3)
// ---------------------------------------------------------------------------

/// `initialMessage` of [`ThreadLaunchInput`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadLaunchInitialMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<MessageId>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<MessageContext>,
    pub attachments: Vec<ChatAttachment>,
}

/// `OrchestrationV2ThreadLaunchInput` (`OV2:3606-3626`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadLaunchInput {
    pub command_id: CommandId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creation_source: Option<CreationSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reuse_existing_thread: Option<bool>,
    pub project_id: ProjectId,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generate_title: Option<bool>,
    pub model_selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
    pub workspace_strategy: WorkspaceStrategy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_message: Option<ThreadLaunchInitialMessage>,
}

/// `OrchestrationV2ThreadLaunchResult` (`OV2:3629-3635`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadLaunchResult {
    pub thread_id: ThreadId,
    pub projection: ThreadProjection,
    /// True when this replays an already-launched `commandId`.
    pub resumed: bool,
    /// The initial message's commit sequence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
}

// ---------------------------------------------------------------------------
// Typed errors
// ---------------------------------------------------------------------------

json_literal! {
    pub struct DispatchCommandErrorTag = str("OrchestrationV2DispatchCommandError");
}

/// `OrchestrationV2DispatchCommandError` (`OV2:3884-3893`). Keep `message`
/// human-readable and free of transport words ("connection", "timeout"): the
/// mobile outbox retries those forever.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2DispatchCommandError {
    #[serde(rename = "_tag")]
    pub tag: DispatchCommandErrorTag,
    pub command_id: CommandId,
    pub command_type: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// `Schema.Defect()`; j3 omits it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<Value>,
}

impl OrchestrationV2DispatchCommandError {
    pub fn new(
        command_id: impl Into<CommandId>,
        command_type: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            tag: DispatchCommandErrorTag,
            command_id: command_id.into(),
            command_type: command_type.into(),
            message: message.into(),
            detail: None,
            cause: None,
        }
    }
}

json_literal! {
    pub struct ThreadLaunchErrorTag = str("OrchestrationV2ThreadLaunchError");
}

/// `OrchestrationV2ThreadLaunchError` (`OV2:3912-3920`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2ThreadLaunchError {
    #[serde(rename = "_tag")]
    pub tag: ThreadLaunchErrorTag,
    pub command_id: CommandId,
    pub project_id: ProjectId,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<Value>,
}

impl OrchestrationV2ThreadLaunchError {
    pub fn new(
        command_id: impl Into<CommandId>,
        project_id: impl Into<ProjectId>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            tag: ThreadLaunchErrorTag,
            command_id: command_id.into(),
            project_id: project_id.into(),
            message: message.into(),
            cause: None,
        }
    }
}

json_literal! {
    pub struct GetThreadProjectionErrorTag = str("OrchestrationV2GetThreadProjectionError");
}

/// `OrchestrationV2GetThreadProjectionError` (`OV2:3895-3902`). Also the
/// typed failure for `subscribeThread` on an unknown thread.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2GetThreadProjectionError {
    #[serde(rename = "_tag")]
    pub tag: GetThreadProjectionErrorTag,
    pub thread_id: ThreadId,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<Value>,
}

impl OrchestrationV2GetThreadProjectionError {
    pub fn new(thread_id: impl Into<ThreadId>, message: impl Into<String>) -> Self {
        Self {
            tag: GetThreadProjectionErrorTag,
            thread_id: thread_id.into(),
            message: message.into(),
            cause: None,
        }
    }
}

json_literal! {
    pub struct GetShellSnapshotErrorTag = str("OrchestrationV2GetShellSnapshotError");
}

/// `OrchestrationV2GetShellSnapshotError` (`OV2:3904-3910`). Also the typed
/// failure for `subscribeShell`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationV2GetShellSnapshotError {
    #[serde(rename = "_tag")]
    pub tag: GetShellSnapshotErrorTag,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<Value>,
}

impl OrchestrationV2GetShellSnapshotError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            tag: GetShellSnapshotErrorTag,
            message: message.into(),
            cause: None,
        }
    }
}
