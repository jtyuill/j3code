//! Orchestration V2 entities, projection, shell and stream items
//! (`C/orchestrationV2.ts`, abbreviated `OV2` below; policy enums from
//! `C/providerPolicy.ts`, message context from `C/composerContext.ts`).
//!
//! Rust names drop the `OrchestrationV2` prefix. Entities j3 never emits
//! (subagents, checkpoints, context handoffs/transfers) stay opaque JSON.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::chat_attachment::ChatAttachment;
use super::double_option;
use super::ids::*;
use super::model::ModelSelection;
use super::project::ProjectShell;
use super::{One, True};

// ---------------------------------------------------------------------------
// Shared enums
// ---------------------------------------------------------------------------

/// `OrchestrationV2Actor` (`OV2:88`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Actor {
    User,
    Agent,
    System,
}

/// `OrchestrationV2CreationSource` (`OV2:91-97`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CreationSource {
    Web,
    Mobile,
    Mcp,
    Provider,
    Server,
}

/// `OrchestrationV2ThreadHistoryOrigin` (`OV2:100`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ThreadHistoryOrigin {
    #[serde(rename = "native")]
    Native,
    #[serde(rename = "v1_import")]
    V1Import,
}

/// `RuntimeMode` (`C/providerPolicy.ts:25-31`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeMode {
    ApprovalRequired,
    AutoAcceptEdits,
    Auto,
    FullAccess,
}

/// `ProviderInteractionMode` (`C/providerPolicy.ts:34`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderInteractionMode {
    Default,
    Plan,
}

/// `ProviderApprovalDecision` (`C/providerPolicy.ts:47-53`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderApprovalDecision {
    Accept,
    AcceptForSession,
    AcceptAlways,
    Decline,
    Cancel,
}

/// `ProviderUserInputAnswers`: `Record<string, unknown>`; answers are a
/// string or a string array per question id.
pub type ProviderUserInputAnswers = BTreeMap<String, Value>;

/// `UserInputAttachments`: images/files per question id.
pub type UserInputAttachments = BTreeMap<String, Vec<ChatAttachment>>;

/// `OrchestrationV2NativeRefStrength` (`OV2:142`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NativeRefStrength {
    Strong,
    Weak,
    None,
}

/// `OrchestrationV2ProviderRef` (`OV2:145-151`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRef {
    pub driver: ProviderDriverKind,
    pub native_id: Option<String>,
    pub strength: NativeRefStrength,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordinal: Option<u64>,
}

/// `OrchestrationMessageContext` (`C/composerContext.ts:284-304`). Records are
/// forward-compatible; store and echo them as-is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageContext {
    pub version: One,
    /// `GuestMessageAttribution`; opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest: Option<Value>,
    pub records: Vec<Value>,
}

/// `OrchestrationV2IdleCompaction` (`OV2:1277`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IdleCompaction {
    BeforeCacheExpires,
    OnReturn,
}

// ---------------------------------------------------------------------------
// Thread
// ---------------------------------------------------------------------------

/// `relationshipToParent` of [`AppThreadLineage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThreadRelationship {
    Fork,
    Subagent,
}

/// `OrchestrationV2AppThreadLineage` (`OV2:153-158`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppThreadLineage {
    pub parent_thread_id: Option<ThreadId>,
    pub relationship_to_parent: Option<ThreadRelationship>,
    pub root_thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_chat: Option<bool>,
}

impl AppThreadLineage {
    /// A top-level thread: no parent, its own root.
    pub fn root(thread_id: impl Into<ThreadId>) -> Self {
        Self {
            parent_thread_id: None,
            relationship_to_parent: None,
            root_thread_id: thread_id.into(),
            side_chat: None,
        }
    }
}

/// `forkedFrom` of `OrchestrationV2AppThread` (`OV2:466-475`), by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ThreadForkSource {
    #[serde(rename = "run", rename_all = "camelCase")]
    Run { thread_id: ThreadId, run_id: RunId },
    #[serde(rename = "node", rename_all = "camelCase")]
    Node { node_id: NodeId },
    #[serde(rename = "provider_thread", rename_all = "camelCase")]
    ProviderThread {
        provider_thread_id: ProviderThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_turn_id: Option<ProviderTurnId>,
    },
}

/// `OrchestrationV2DelegatedFrom` (`OV2:132-136`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegatedFrom {
    pub environment_id: EnvironmentId,
    pub thread_id: ThreadId,
    pub title: String,
}

/// `settledOverride` literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettledOverride {
    Settled,
    Active,
}

/// `titleRegeneration` of `OrchestrationV2AppThread` / thread shell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleRegeneration {
    pub request_id: CommandId,
    pub started_at: IsoDateTime,
}

/// `rollbackFailure` of `OrchestrationV2AppThread`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RollbackFailure {
    pub request_id: CommandId,
    pub message: String,
}

/// `OrchestrationV2AppThread` (`OV2:441-512`). `title` must be non-blank.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppThread {
    pub created_by: Actor,
    pub creation_source: CreationSource,
    pub id: ThreadId,
    pub project_id: ProjectId,
    pub title: String,
    pub provider_instance_id: ProviderInstanceId,
    pub model_selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    /// `ThreadLinkedPullRequest | null`; opaque.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub linked_pull_request: Option<Option<Value>>,
    /// `ThreadPullRequestLink[]`; opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_requests: Option<Vec<Value>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub branch_pull_request: Option<Option<Value>>,
    pub active_provider_thread_id: Option<ProviderThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_origin: Option<ThreadHistoryOrigin>,
    /// `OrchestrationV2LinkOrigin`; opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_origin: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_from: Option<DelegatedFrom>,
    pub lineage: AppThreadLineage,
    pub forked_from: Option<ThreadForkSource>,
    pub created_at: IsoDateTime,
    pub updated_at: IsoDateTime,
    pub archived_at: Option<IsoDateTime>,
    /// Decoding default `null`.
    #[serde(default)]
    pub settled_override: Option<SettledOverride>,
    /// Decoding default `null`.
    #[serde(default)]
    pub settled_at: Option<IsoDateTime>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub unsettled_at: Option<Option<IsoDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub snoozed_until: Option<Option<IsoDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub snoozed_at: Option<Option<IsoDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub last_snooze_wake_at: Option<Option<IsoDateTime>>,
    /// `OrchestrationV2LimitRecovery | null`; opaque.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub limit_recovery: Option<Option<Value>>,
    /// `ThreadGuestShare | null`; opaque.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub guest_share: Option<Option<Value>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub pinned_at: Option<Option<IsoDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub auto_settle_disabled_at: Option<Option<IsoDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub pin_order_key: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub active_order_key: Option<Option<String>>,
    /// Decoding default `null`.
    #[serde(default)]
    pub last_visited_at: Option<IsoDateTime>,
    /// `OrchestrationV2ThreadOpens | null` (lenient); opaque.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub opens: Option<Option<Value>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub title_regeneration: Option<Option<TitleRegeneration>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_request_id: Option<CommandId>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub rollback_failure: Option<Option<RollbackFailure>>,
    pub deleted_at: Option<IsoDateTime>,
}

// ---------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------

/// `OrchestrationV2RunStatus` (`OV2:585-596`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Preparing,
    Queued,
    Starting,
    Running,
    Waiting,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
    RolledBack,
}

impl RunStatus {
    /// `preparing|starting|running|waiting`: the run is the thread's active run.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Preparing | Self::Starting | Self::Running | Self::Waiting
        )
    }
}

/// `OrchestrationV2ThreadLaunchWorkspaceStrategy` (`OV2:649-665`), by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WorkspaceStrategy {
    #[serde(rename = "root", rename_all = "camelCase")]
    Root {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
    #[serde(rename = "existing_worktree", rename_all = "camelCase")]
    ExistingWorktree {
        worktree_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
    #[serde(rename = "worktree", rename_all = "camelCase")]
    Worktree {
        base_ref: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_from_origin: Option<bool>,
    },
}

/// `sourcePlanRef` of runs and `message.dispatch`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcePlanRef {
    pub thread_id: ThreadId,
    pub plan_id: PlanId,
}

/// `OrchestrationV2Run` (`OV2:671-703`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: RunId,
    pub thread_id: ThreadId,
    /// PositiveInt.
    pub ordinal: u64,
    pub provider_instance_id: ProviderInstanceId,
    pub model_selection: ModelSelection,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub user_message_id: MessageId,
    pub root_node_id: Option<NodeId>,
    pub active_attempt_id: Option<RunAttemptId>,
    pub status: RunStatus,
    /// `PositiveInt | null`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub queue_position: Option<Option<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_held: Option<bool>,
    pub requested_at: IsoDateTime,
    pub started_at: Option<IsoDateTime>,
    pub completed_at: Option<IsoDateTime>,
    pub checkpoint_id: Option<CheckpointId>,
    pub context_handoff_id: Option<ContextHandoffId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_continuation_of_run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_task_id: Option<NodeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_started_at: Option<IsoDateTime>,
    /// `OrchestrationV2RestartCancelledBackgroundWork[]`; opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_cancelled_background_work: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_plan_ref: Option<SourcePlanRef>,
    /// `OrchestrationV2DelegatedCompletionCohort`; opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_completion: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_preparation: Option<WorkspaceStrategy>,
}

/// `reason` of [`RunAttempt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunAttemptReason {
    Initial,
    SteeringRestart,
    Retry,
    ProviderRecovery,
}

/// `status` of [`RunAttempt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunAttemptStatus {
    Pending,
    Running,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
    Superseded,
}

/// `OrchestrationV2RunAttempt` (`OV2:730-752`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAttempt {
    pub id: RunAttemptId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_thread_id: Option<String>,
    pub run_id: RunId,
    /// PositiveInt.
    pub attempt_ordinal: u64,
    pub root_node_id: NodeId,
    pub provider_instance_id: ProviderInstanceId,
    pub provider_thread_id: ProviderThreadId,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub reason: RunAttemptReason,
    pub status: RunAttemptStatus,
    pub started_at: Option<IsoDateTime>,
    pub completed_at: Option<IsoDateTime>,
}

/// `kind` of [`ExecutionNode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionNodeKind {
    RootTurn,
    AssistantMessage,
    Reasoning,
    Plan,
    TodoList,
    ToolCall,
    ApprovalRequest,
    UserInputRequest,
    Subagent,
    Hook,
    System,
}

/// `status` of [`ExecutionNode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionNodeStatus {
    Idle,
    Pending,
    Running,
    Waiting,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
    RolledBack,
}

/// `OrchestrationV2ExecutionNode` (`OV2:755-794`). j3 sends no nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionNode {
    pub id: NodeId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub parent_node_id: Option<NodeId>,
    pub root_node_id: NodeId,
    pub kind: ExecutionNodeKind,
    pub status: ExecutionNodeStatus,
    pub counts_for_run: bool,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub native_item_ref: Option<ProviderRef>,
    pub runtime_request_id: Option<RuntimeRequestId>,
    pub checkpoint_scope_id: Option<CheckpointScopeId>,
    pub started_at: Option<IsoDateTime>,
    pub completed_at: Option<IsoDateTime>,
}

// ---------------------------------------------------------------------------
// Provider entities
// ---------------------------------------------------------------------------

/// `OrchestrationV2SessionCapabilities` (`OV2:259-265`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCapabilities {
    pub supports_multiple_provider_threads_per_session: bool,
    pub supports_model_switch_in_session: bool,
    pub supports_provider_switching_via_handoff: bool,
    pub supports_runtime_mode_switch_in_session: bool,
    pub pending_requests_survive_restart: bool,
}

/// `OrchestrationV2ThreadCapabilities` (`OV2:268-276`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadCapabilities {
    pub can_create_empty_thread: bool,
    pub can_read_thread_snapshot: bool,
    pub can_rollback_thread: bool,
    pub can_fork_thread: bool,
    pub can_fork_from_turn: bool,
    pub can_fork_from_subagent_thread: bool,
    pub exposes_native_thread_id: bool,
}

/// `OrchestrationV2TurnCapabilities` (`OV2:279-290`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnCapabilities {
    pub exposes_native_turn_id: bool,
    pub emits_turn_started: bool,
    pub emits_turn_completed: bool,
    pub supports_interrupt: bool,
    pub supports_active_steering: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_steering_interrupts_tools: Option<bool>,
    pub supports_steering_by_interrupt_restart: bool,
    pub supports_queued_messages: bool,
    pub terminal_status_quality: NativeRefStrength,
}

/// `OrchestrationV2StreamingCapabilities` (`OV2:293-299`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamingCapabilities {
    pub streams_assistant_text: bool,
    pub streams_reasoning: bool,
    pub streams_tool_output: bool,
    pub streams_plan_text: bool,
    pub emits_message_completed: bool,
}

/// `OrchestrationV2ToolCapabilities` (`OV2:302-309`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCapabilities {
    pub exposes_tool_item_ids: bool,
    pub emits_tool_started: bool,
    pub emits_tool_completed: bool,
    pub emits_tool_output: bool,
    pub supports_mcp_tools: bool,
    pub supports_dynamic_tool_callbacks: bool,
}

/// `OrchestrationV2ApprovalCapabilities` (`OV2:312-320`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalCapabilities {
    pub supports_command_approval: bool,
    pub supports_file_read_approval: bool,
    pub supports_file_change_approval: bool,
    pub supports_apply_patch_approval: bool,
    pub approvals_have_native_request_ids: bool,
    pub approval_callbacks_are_live_only: bool,
    pub approvals_can_originate_from_subagents: bool,
}

/// `OrchestrationV2PlanningCapabilities` (`OV2:323-329`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanningCapabilities {
    pub emits_plan_updated: bool,
    pub emits_todo_list: bool,
    pub emits_proposed_plan: bool,
    pub supports_structured_questions: bool,
    pub plan_deltas_have_item_ids: bool,
}

/// `OrchestrationV2SubagentCapabilities` (`OV2:332-339`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentCapabilities {
    pub supports_subagents: bool,
    pub exposes_subagent_thread_ids: bool,
    pub emits_subagent_lifecycle: bool,
    pub can_wait_for_subagents: bool,
    pub can_close_subagents: bool,
    pub can_fork_subagent_thread: bool,
}

/// `OrchestrationV2ContextCapabilities` (`OV2:342-351`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextCapabilities {
    pub accepts_system_context: bool,
    pub accepts_developer_context: bool,
    pub accepts_synthetic_user_context: bool,
    pub can_generate_summaries: bool,
    pub can_consume_handoff_summaries: bool,
    pub supports_delta_handoff: bool,
    pub supports_full_thread_handoff: bool,
    /// `PositiveInt | null`.
    pub max_recommended_handoff_chars: Option<u64>,
}

/// `OrchestrationV2CheckpointCapabilities` (`OV2:354-360`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointCapabilities {
    pub app_can_checkpoint_filesystem: bool,
    pub supports_nested_checkpoint_scopes: bool,
    pub provider_can_rollback_conversation: bool,
    pub provider_rollback_returns_snapshot: bool,
    pub provider_can_read_conversation_snapshot: bool,
}

/// `enforcement` of [`RuntimePolicyCapabilities`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimePolicyEnforcement {
    Native,
    ClientBoundary,
}

/// `OrchestrationV2RuntimePolicyCapabilities` (`OV2:364-366`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimePolicyCapabilities {
    pub enforcement: RuntimePolicyEnforcement,
}

impl Default for RuntimePolicyCapabilities {
    /// The contract's decoding default: `{enforcement: "client-boundary"}`.
    fn default() -> Self {
        Self {
            enforcement: RuntimePolicyEnforcement::ClientBoundary,
        }
    }
}

/// `OrchestrationV2IdentityCapabilities` (`OV2:370-375`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityCapabilities {
    pub native_thread_ids: NativeRefStrength,
    pub native_turn_ids: NativeRefStrength,
    pub native_item_ids: NativeRefStrength,
    pub native_request_ids: NativeRefStrength,
}

/// `OrchestrationV2ProviderCapabilities` (`OV2:378-394`). Every nested boolean
/// is required on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapabilities {
    pub sessions: SessionCapabilities,
    pub threads: ThreadCapabilities,
    pub turns: TurnCapabilities,
    pub streaming: StreamingCapabilities,
    pub tools: ToolCapabilities,
    pub approvals: ApprovalCapabilities,
    pub planning: PlanningCapabilities,
    pub subagents: SubagentCapabilities,
    pub context: ContextCapabilities,
    pub checkpointing: CheckpointCapabilities,
    pub identity: IdentityCapabilities,
    /// Decoding default `{enforcement: "client-boundary"}`.
    #[serde(default)]
    pub runtime_policy: RuntimePolicyCapabilities,
}

impl ProviderCapabilities {
    /// `PiProviderCapabilitiesV2` (`packages/provider-pi/src/server/adapter.ts:91-182`),
    /// value for value. omp is Pi's fork and j3 reports its sessions as Pi's.
    pub fn omp() -> Self {
        Self {
            sessions: SessionCapabilities {
                supports_multiple_provider_threads_per_session: false,
                supports_model_switch_in_session: true,
                supports_provider_switching_via_handoff: true,
                supports_runtime_mode_switch_in_session: false,
                pending_requests_survive_restart: false,
            },
            threads: ThreadCapabilities {
                can_create_empty_thread: true,
                can_read_thread_snapshot: true,
                can_rollback_thread: true,
                can_fork_thread: true,
                can_fork_from_turn: true,
                can_fork_from_subagent_thread: false,
                exposes_native_thread_id: true,
            },
            turns: TurnCapabilities {
                exposes_native_turn_id: false,
                emits_turn_started: true,
                emits_turn_completed: true,
                supports_interrupt: true,
                supports_active_steering: true,
                active_steering_interrupts_tools: None,
                supports_steering_by_interrupt_restart: false,
                supports_queued_messages: true,
                terminal_status_quality: NativeRefStrength::Strong,
            },
            streaming: StreamingCapabilities {
                streams_assistant_text: true,
                streams_reasoning: true,
                streams_tool_output: true,
                streams_plan_text: false,
                emits_message_completed: true,
            },
            tools: ToolCapabilities {
                exposes_tool_item_ids: true,
                emits_tool_started: true,
                emits_tool_completed: true,
                emits_tool_output: true,
                supports_mcp_tools: true,
                supports_dynamic_tool_callbacks: false,
            },
            approvals: ApprovalCapabilities {
                supports_command_approval: true,
                supports_file_read_approval: false,
                supports_file_change_approval: true,
                supports_apply_patch_approval: false,
                approvals_have_native_request_ids: true,
                approval_callbacks_are_live_only: true,
                approvals_can_originate_from_subagents: false,
            },
            planning: PlanningCapabilities {
                emits_plan_updated: false,
                emits_todo_list: false,
                emits_proposed_plan: false,
                supports_structured_questions: true,
                plan_deltas_have_item_ids: false,
            },
            subagents: SubagentCapabilities {
                supports_subagents: true,
                exposes_subagent_thread_ids: false,
                emits_subagent_lifecycle: true,
                can_wait_for_subagents: false,
                can_close_subagents: false,
                can_fork_subagent_thread: false,
            },
            context: ContextCapabilities {
                accepts_system_context: false,
                accepts_developer_context: false,
                accepts_synthetic_user_context: true,
                can_generate_summaries: false,
                can_consume_handoff_summaries: true,
                supports_delta_handoff: true,
                supports_full_thread_handoff: true,
                max_recommended_handoff_chars: None,
            },
            checkpointing: CheckpointCapabilities {
                app_can_checkpoint_filesystem: true,
                supports_nested_checkpoint_scopes: false,
                provider_can_rollback_conversation: true,
                provider_rollback_returns_snapshot: true,
                provider_can_read_conversation_snapshot: true,
            },
            identity: IdentityCapabilities {
                native_thread_ids: NativeRefStrength::Strong,
                native_turn_ids: NativeRefStrength::Weak,
                native_item_ids: NativeRefStrength::Strong,
                native_request_ids: NativeRefStrength::Strong,
            },
            runtime_policy: RuntimePolicyCapabilities {
                enforcement: RuntimePolicyEnforcement::ClientBoundary,
            },
        }
    }
}

/// `status` of [`ProviderSession`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderSessionStatus {
    Starting,
    Ready,
    Running,
    Waiting,
    Stopped,
    Error,
}

/// `OrchestrationV2ProviderSession` (`OV2:941-952`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSession {
    pub id: ProviderSessionId,
    pub driver: ProviderDriverKind,
    pub provider_instance_id: ProviderInstanceId,
    pub status: ProviderSessionStatus,
    pub cwd: String,
    pub model: Option<String>,
    pub capabilities: ProviderCapabilities,
    pub created_at: IsoDateTime,
    pub updated_at: IsoDateTime,
    pub last_error: Option<String>,
}

/// `status` of [`ProviderThread`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderThreadStatus {
    NotLoaded,
    Idle,
    Active,
    Archived,
    Closed,
    Error,
}

/// `forkedFrom` of [`ProviderThread`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderThreadForkedFrom {
    pub provider_thread_id: ProviderThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_turn_id: Option<ProviderTurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_id: Option<CheckpointId>,
}

/// `OrchestrationV2ProviderThread` (`OV2:1069-1103`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderThread {
    pub id: ProviderThreadId,
    pub driver: ProviderDriverKind,
    pub provider_instance_id: ProviderInstanceId,
    pub provider_session_id: Option<ProviderSessionId>,
    pub app_thread_id: Option<ThreadId>,
    pub owner_node_id: Option<NodeId>,
    pub native_thread_ref: Option<ProviderRef>,
    pub native_conversation_head_ref: Option<ProviderRef>,
    pub status: ProviderThreadStatus,
    /// `PositiveInt | null`.
    pub first_run_ordinal: Option<u64>,
    /// `PositiveInt | null`.
    pub last_run_ordinal: Option<u64>,
    pub handoff_ids: Vec<ContextHandoffId>,
    pub forked_from: Option<ProviderThreadForkedFrom>,
    /// `OrchestrationV2PendingBackgroundTask[]`, decoding default `[]`; opaque.
    #[serde(default)]
    pub pending_background_tasks: Vec<Value>,
    /// `ThreadTokenUsageSnapshot | null`, decoding default `null`; opaque.
    #[serde(default)]
    pub context_usage: Option<Value>,
    /// `OrchestrationV2ProviderThreadNativeMetadata | null`, decoding default `null`; opaque.
    #[serde(default)]
    pub native_metadata: Option<Value>,
    /// `OrchestrationV2ProviderGoal | null`, decoding default `null`; opaque.
    #[serde(default)]
    pub goal: Option<Value>,
    pub created_at: IsoDateTime,
    pub updated_at: IsoDateTime,
}

/// `status` of [`ProviderTurn`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderTurnStatus {
    Pending,
    Running,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
}

/// `OrchestrationV2ProviderTurn` (`OV2:1163-1182`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTurn {
    pub id: ProviderTurnId,
    pub provider_thread_id: ProviderThreadId,
    pub node_id: NodeId,
    pub run_attempt_id: Option<RunAttemptId>,
    pub native_turn_ref: Option<ProviderRef>,
    /// PositiveInt.
    pub ordinal: u64,
    pub status: ProviderTurnStatus,
    pub started_at: Option<IsoDateTime>,
    pub completed_at: Option<IsoDateTime>,
    /// `OrchestrationV2ProviderTurnTokenUsage`; opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<Value>,
    /// `TurnTokenUsage`; opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_token_usage: Option<Value>,
}

// ---------------------------------------------------------------------------
// Runtime requests, messages, plans
// ---------------------------------------------------------------------------

/// `kind` of [`RuntimeRequest`]: `ProviderRequestKind` ∪
/// `dynamic_tool_call|user_input|auth_refresh` (`OV2:1205-1208`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RuntimeRequestKind {
    #[serde(rename = "command")]
    Command,
    #[serde(rename = "file-read")]
    FileRead,
    #[serde(rename = "file-change")]
    FileChange,
    #[serde(rename = "mcp-elicitation")]
    McpElicitation,
    #[serde(rename = "permission")]
    Permission,
    #[serde(rename = "dynamic_tool_call")]
    DynamicToolCall,
    #[serde(rename = "user_input")]
    UserInput,
    #[serde(rename = "auth_refresh")]
    AuthRefresh,
}

/// `status` of [`RuntimeRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeRequestStatus {
    Pending,
    Resolved,
    Expired,
    Cancelled,
}

/// `responseCapability` of [`RuntimeRequest`], by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ResponseCapability {
    #[serde(rename = "live", rename_all = "camelCase")]
    Live {
        provider_session_id: ProviderSessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        supports_feedback: Option<bool>,
    },
    #[serde(rename = "message")]
    Message,
    #[serde(rename = "not_resumable")]
    NotResumable { reason: String },
}

/// `OrchestrationV2RuntimeRequest` (`OV2:1200-1224`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRequest {
    pub id: RuntimeRequestId,
    pub node_id: NodeId,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub native_request_ref: Option<ProviderRef>,
    pub kind: RuntimeRequestKind,
    pub status: RuntimeRequestStatus,
    pub response_capability: ResponseCapability,
    pub created_at: IsoDateTime,
    pub resolved_at: Option<IsoDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<ProviderApprovalDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<ProviderUserInputAnswers>,
}

/// `role` of [`ConversationMessage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

/// `OrchestrationV2ConversationMessage` (`OV2:1283-1312`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMessage {
    /// `OrchestrationV2Notification`; opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification: Option<Value>,
    pub created_by: Actor,
    pub creation_source: CreationSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_task_id: Option<ScheduledTaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_thread_id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_compaction: Option<IdleCompaction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_script: Option<bool>,
    pub id: MessageId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: Option<NodeId>,
    pub role: MessageRole,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<MessageContext>,
    pub attachments: Vec<ChatAttachment>,
    pub streaming: bool,
    pub created_at: IsoDateTime,
    pub updated_at: IsoDateTime,
    /// `{parentRunId, generation, taskIds}`; opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_completion: Option<Value>,
}

/// `status` of [`PlanStep`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanStepStatus {
    Pending,
    Running,
    Completed,
}

/// `OrchestrationV2PlanStep` (`OV2:1315-1321`). `id`/`text` non-blank.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    pub id: String,
    pub text: String,
    pub status: PlanStepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_anchor_at: Option<IsoDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// An option of [`UserInputQuestion`]. `label`/`description` non-blank.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputQuestionOption {
    pub label: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

/// `OrchestrationV2UserInputQuestion` (`OV2:1324-1340`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<UserInputQuestionOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_select: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_custom_answer: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

/// `status` of [`PlanArtifact`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanArtifactStatus {
    Draft,
    Active,
    Completed,
    Superseded,
}

/// Body of [`PlanArtifact`], by `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum PlanArtifactKind {
    #[serde(rename = "proposed_plan")]
    ProposedPlan { markdown: String },
    #[serde(rename = "todo_list")]
    TodoList {
        steps: Vec<PlanStep>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        explanation: Option<String>,
    },
}

/// `OrchestrationV2PlanArtifact` (`OV2:1343-1364`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanArtifact {
    pub id: PlanId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: NodeId,
    pub status: PlanArtifactStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_in_turn_item: Option<True>,
    #[serde(flatten)]
    pub kind: PlanArtifactKind,
}

// ---------------------------------------------------------------------------
// Turn items
// ---------------------------------------------------------------------------

/// `OrchestrationV2TurnItemStatus` (`OV2:1420-1429`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TurnItemStatus {
    Idle,
    Pending,
    Running,
    Waiting,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

/// `OrchestrationV2FileChangeDetail` (`OV2:1433-1439`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChangeDetail {
    pub operation: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// `OrchestrationV2ProviderFailureClass` (`OV2:1442-1449`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderFailureClass {
    UsageLimit,
    ProviderError,
    TransportError,
    PermissionError,
    ValidationError,
    Unknown,
}

/// `OrchestrationV2ProviderFailure` (`OV2:1458-1464`). `message` non-blank,
/// at most 4096 chars; `code` non-blank, at most 128 chars.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderFailure {
    pub class: ProviderFailureClass,
    pub message: String,
    pub code: Option<String>,
    pub retryable: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub reset_at: Option<Option<IsoDateTime>>,
}

/// `OrchestrationV2ProviderRetry` (`OV2:1467-1471`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRetry {
    /// PositiveInt.
    pub attempt: u64,
    /// `PositiveInt | null`.
    pub max_attempts: Option<u64>,
    pub retry_delay_ms: Option<u64>,
}

/// `OrchestrationV2UserMessageInputIntent` (`OV2:1474-1479`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserMessageInputIntent {
    TurnStart,
    QueuedTurn,
    Steer,
    PromotedQueuedToSteer,
}

/// `deferredWork` of the turn item base (wire-only cursor; j3 omits it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeferredWork {
    pub cursor: String,
    /// PositiveInt.
    pub item_count: u64,
    pub last_ordinal: u64,
}

/// `ToolActivitySurface` (`C/providerRuntime.ts:80`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolActivitySurface {
    Browser,
    Computer,
}

/// `OrchestrationV2FileSearchResult` (`OV2:1512-1517`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSearchResult {
    pub file_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

/// `OrchestrationV2WebSearchResult` (`OV2:1520-1524`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSearchResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

/// `UserInputAttachmentAnswerPayload` (`C/providerPolicy.ts:79-84`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputAnswerPayload {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_text_by_id: Option<BTreeMap<String, String>>,
    pub answers: ProviderUserInputAnswers,
    pub attachments_by_question_id: UserInputAttachments,
}

/// `responseMode` of `user_input_request`: only `"message"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UserInputResponseMode {
    #[serde(rename = "message")]
    Message,
}

/// `user_message` turn item (`OV2:1589-1604`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMessageItem {
    pub created_by: Actor,
    pub creation_source: CreationSource,
    pub message_id: MessageId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_task_id: Option<ScheduledTaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_thread_id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_compaction: Option<IdleCompaction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_script: Option<bool>,
    pub input_intent: UserMessageInputIntent,
    pub text: String,
    /// Deduped-body marker; j3 never sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_omitted: Option<True>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<MessageContext>,
    pub attachments: Vec<ChatAttachment>,
}

/// `assistant_message` turn item (`OV2:1605-1613`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessageItem {
    pub message_id: MessageId,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_omitted: Option<True>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<ChatAttachment>>,
    pub streaming: bool,
}

/// `reasoning` turn item (`OV2:1614-1619`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReasoningItem {
    pub text: String,
    pub streaming: bool,
}

/// `todo_list` turn item (`OV2:1627-1633`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoListItem {
    pub plan_id: PlanId,
    pub steps: Vec<PlanStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
}

/// `user_input_request` turn item (`OV2:1634-1641`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputRequestItem {
    pub request_id: RuntimeRequestId,
    pub questions: Vec<UserInputQuestion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_answer: Option<UserInputAnswerPayload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_mode: Option<UserInputResponseMode>,
}

/// `file_change` turn item (`OV2:1642-1652`). `fileName` non-blank.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChangeItem {
    pub file_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_str: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_str: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_str: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changes: Option<Vec<FileChangeDetail>>,
}

/// `command_execution` turn item (`OV2:1653-1664`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecutionItem {
    pub input: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_omitted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_indicates_failure: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<bool>,
}

/// `file_search` turn item (`OV2:1665-1670`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileSearchItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub results: Option<Vec<FileSearchResult>>,
}

/// `web_search` turn item (`OV2:1671-1676`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebSearchItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patterns: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub results: Option<Vec<WebSearchResult>>,
}

/// `run_interrupt_request` turn item (`OV2:1698-1704`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunInterruptRequestItem {
    pub message: String,
    /// Set when the provider did not confirm the stop in time (offers Force stop).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unresponsive_at: Option<IsoDateTime>,
}

/// `run_interrupt_result` / `system_notice` turn items (`OV2:1705-1714`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageItem {
    pub message: String,
}

/// `error` turn item (`OV2:1715-1720`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorItem {
    pub failure: ProviderFailure,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<ProviderRetry>,
}

/// `compaction` turn item (`OV2:1721-1728`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionItem {
    pub driver: Option<ProviderDriverKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_token_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_token_count: Option<u64>,
}

/// `dynamic_tool` turn item (`OV2:1795-1804`). `input` is any JSON and the
/// key is required.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DynamicToolItem {
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewed_image_path: Option<String>,
    pub input: Value,
    /// `Schema.optional(Schema.Unknown)`: absent, `null` and values round-trip.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub output: Option<Option<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_omitted: Option<bool>,
}

open_union! {
    /// The `type`-specific body of a [`TurnItem`] (`OV2:1583-1805`). The 15
    /// kinds j3 emits are typed; every other kind (`notification`,
    /// `proposed_plan`, `approval_request`, `checkpoint`, `handoff`, `fork`,
    /// `thread_created`, `secret_request`, `link_request`, `subagent`, future
    /// kinds) is `Unknown` holding the non-base keys, `type` included.
    pub enum TurnItemKind {
        tag = "type";
        newtype {
            "user_message" => UserMessage(UserMessageItem),
            "assistant_message" => AssistantMessage(AssistantMessageItem),
            "reasoning" => Reasoning(ReasoningItem),
            "todo_list" => TodoList(TodoListItem),
            "user_input_request" => UserInputRequest(UserInputRequestItem),
            "file_change" => FileChange(FileChangeItem),
            "command_execution" => CommandExecution(CommandExecutionItem),
            "file_search" => FileSearch(FileSearchItem),
            "web_search" => WebSearch(WebSearchItem),
            "dynamic_tool" => DynamicTool(DynamicToolItem),
            "run_interrupt_request" => RunInterruptRequest(RunInterruptRequestItem),
            "run_interrupt_result" => RunInterruptResult(MessageItem),
            "system_notice" => SystemNotice(MessageItem),
            "error" => Error(ErrorItem),
            "compaction" => Compaction(CompactionItem),
        }
        unit {}
    }
}

/// `OrchestrationV2TurnItem` (`OV2:1483-1805`): base fields plus a
/// `type`-tagged body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred_work: Option<DeferredWork>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_non_execution_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_surface: Option<ToolActivitySurface>,
    /// `ToolActivityIcon`; opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_icon: Option<Value>,
    /// `ToolActivitySource`; opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_source: Option<Value>,
    pub id: TurnItemId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: Option<NodeId>,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub native_item_ref: Option<ProviderRef>,
    pub parent_item_id: Option<TurnItemId>,
    /// Thread-wide order key; assign once and never change.
    pub ordinal: u64,
    pub status: TurnItemStatus,
    pub title: Option<String>,
    pub started_at: Option<IsoDateTime>,
    pub completed_at: Option<IsoDateTime>,
    pub updated_at: IsoDateTime,
    #[serde(flatten)]
    pub kind: TurnItemKind,
}

/// `visibility` of [`ProjectedTurnItem`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TurnItemVisibility {
    Local,
    Inherited,
    Synthetic,
}

/// `OrchestrationV2ProjectedTurnItem` (`OV2:1831-1837`), one feed row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectedTurnItem {
    pub position: u64,
    pub visibility: TurnItemVisibility,
    pub source_thread_id: ThreadId,
    pub source_item_id: TurnItemId,
    pub item: TurnItem,
}

// ---------------------------------------------------------------------------
// Domain events
// ---------------------------------------------------------------------------

open_union! {
    /// `type` + `payload` of a [`DomainEvent`] (`OV2:1851-1994`). The 20 types
    /// j3 emits are typed; every other type is `Unknown` holding `type`,
    /// `payload` and any other non-base keys.
    pub enum DomainEventBody {
        tag = "type", content = "payload";
        newtype {
            "thread.created" => ThreadCreated(AppThread),
            "thread.archived" => ThreadArchived(AppThread),
            "thread.unarchived" => ThreadUnarchived(AppThread),
            "thread.deleted" => ThreadDeleted(AppThread),
            "thread.metadata-updated" => ThreadMetadataUpdated(AppThread),
            "thread.runtime-mode-updated" => ThreadRuntimeModeUpdated(AppThread),
            "thread.interaction-mode-updated" => ThreadInteractionModeUpdated(AppThread),
            "thread.model-selection-updated" => ThreadModelSelectionUpdated(AppThread),
            "run.created" => RunCreated(Run),
            "run.updated" => RunUpdated(Run),
            "run-attempt.created" => RunAttemptCreated(RunAttempt),
            "run-attempt.updated" => RunAttemptUpdated(RunAttempt),
            "provider-session.attached" => ProviderSessionAttached(ProviderSession),
            "provider-session.updated" => ProviderSessionUpdated(ProviderSession),
            "provider-thread.updated" => ProviderThreadUpdated(ProviderThread),
            "provider-turn.updated" => ProviderTurnUpdated(ProviderTurn),
            "runtime-request.updated" => RuntimeRequestUpdated(RuntimeRequest),
            "message.updated" => MessageUpdated(ConversationMessage),
            "turn-item.updated" => TurnItemUpdated(TurnItem),
            "plan.updated" => PlanUpdated(PlanArtifact),
        }
        unit {}
    }
}

/// `OrchestrationV2DomainEvent` (`OV2:1840-1994`): event base plus a typed
/// body. Each event replaces one entity by id; there are no deltas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainEvent {
    pub id: EventId,
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<ProviderDriverKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_instance_id: Option<ProviderInstanceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_event_id: Option<RawEventId>,
    pub occurred_at: IsoDateTime,
    #[serde(flatten)]
    pub body: DomainEventBody,
}

// ---------------------------------------------------------------------------
// Projection
// ---------------------------------------------------------------------------

/// `OrchestrationV2ThreadProjection` (`OV2:1997-2022`). Every array is
/// required. `visibleTurnItems` is the feed and must be filled in snapshots.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadProjection {
    pub thread: AppThread,
    pub runs: Vec<Run>,
    pub attempts: Vec<RunAttempt>,
    pub nodes: Vec<ExecutionNode>,
    /// `OrchestrationV2Subagent[]`; opaque (j3 sends `[]`).
    pub subagents: Vec<Value>,
    pub provider_sessions: Vec<ProviderSession>,
    pub provider_threads: Vec<ProviderThread>,
    pub provider_turns: Vec<ProviderTurn>,
    pub runtime_requests: Vec<RuntimeRequest>,
    pub messages: Vec<ConversationMessage>,
    pub plans: Vec<PlanArtifact>,
    pub turn_items: Vec<TurnItem>,
    /// `OrchestrationV2CheckpointScope[]`; opaque (j3 sends `[]`).
    pub checkpoint_scopes: Vec<Value>,
    /// `OrchestrationV2Checkpoint[]`; opaque (j3 sends `[]`).
    pub checkpoints: Vec<Value>,
    /// `OrchestrationV2ContextHandoff[]`; opaque (j3 sends `[]`).
    pub context_handoffs: Vec<Value>,
    /// `OrchestrationV2ContextTransfer[]`; opaque (j3 sends `[]`).
    pub context_transfers: Vec<Value>,
    pub visible_turn_items: Vec<ProjectedTurnItem>,
    pub updated_at: IsoDateTime,
}

impl ThreadProjection {
    /// A projection holding only `thread`, every array empty.
    pub fn empty(thread: AppThread, updated_at: IsoDateTime) -> Self {
        Self {
            thread,
            runs: Vec::new(),
            attempts: Vec::new(),
            nodes: Vec::new(),
            subagents: Vec::new(),
            provider_sessions: Vec::new(),
            provider_threads: Vec::new(),
            provider_turns: Vec::new(),
            runtime_requests: Vec::new(),
            messages: Vec::new(),
            plans: Vec::new(),
            turn_items: Vec::new(),
            checkpoint_scopes: Vec::new(),
            checkpoints: Vec::new(),
            context_handoffs: Vec::new(),
            context_transfers: Vec::new(),
            visible_turn_items: Vec::new(),
            updated_at,
        }
    }
}

// ---------------------------------------------------------------------------
// Shell
// ---------------------------------------------------------------------------

/// `OrchestrationV2ShellThreadStatus` (`OV2:2033-2036`): `idle` ∪ run status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellThreadStatus {
    Idle,
    Preparing,
    Queued,
    Starting,
    Running,
    Waiting,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
    RolledBack,
}

impl From<RunStatus> for ShellThreadStatus {
    fn from(status: RunStatus) -> Self {
        match status {
            RunStatus::Preparing => Self::Preparing,
            RunStatus::Queued => Self::Queued,
            RunStatus::Starting => Self::Starting,
            RunStatus::Running => Self::Running,
            RunStatus::Waiting => Self::Waiting,
            RunStatus::Completed => Self::Completed,
            RunStatus::Interrupted => Self::Interrupted,
            RunStatus::Failed => Self::Failed,
            RunStatus::Cancelled => Self::Cancelled,
            RunStatus::RolledBack => Self::RolledBack,
        }
    }
}

/// `activityRunStatus` of [`ThreadShell`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivityRunStatus {
    Preparing,
    Starting,
    Running,
    Waiting,
}

/// `OrchestrationV2PendingRuntimeRequestSummary` (`OV2:2039-2045`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRuntimeRequestSummary {
    pub id: RuntimeRequestId,
    pub kind: RuntimeRequestKind,
    pub created_at: IsoDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking: Option<bool>,
}

/// `OrchestrationV2LatestVisibleMessageSummary` (`OV2:2049-2054`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatestVisibleMessageSummary {
    pub id: MessageId,
    pub role: MessageRole,
    pub text: String,
    pub updated_at: IsoDateTime,
}

/// `OrchestrationV2ThreadShell` (`OV2:2058-2135`), one thread-list row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadShell {
    pub created_by: Actor,
    pub creation_source: CreationSource,
    pub id: ThreadId,
    pub project_id: ProjectId,
    pub title: String,
    pub provider_instance_id: ProviderInstanceId,
    pub model_selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub linked_pull_request: Option<Option<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_requests: Option<Vec<Value>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub branch_pull_request: Option<Option<Value>>,
    pub lineage: AppThreadLineage,
    pub forked_from: Option<ThreadForkSource>,
    pub active_provider_thread_id: Option<ProviderThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_origin: Option<ThreadHistoryOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_origin: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_from: Option<DelegatedFrom>,
    pub latest_run_id: Option<RunId>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub latest_run_requested_at: Option<Option<IsoDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub latest_run_started_at: Option<Option<IsoDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub latest_run_completed_at: Option<Option<IsoDateTime>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_run_stop_requested: Option<bool>,
    pub active_run_id: Option<RunId>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub activity_run_started_at: Option<Option<IsoDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub activity_run_status: Option<Option<ActivityRunStatus>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_on_thread_wait: Option<bool>,
    pub status: ShellThreadStatus,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub last_error: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub last_error_class: Option<Option<ProviderFailureClass>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub last_error_code: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub usage_limit_reset_at: Option<Option<IsoDateTime>>,
    pub pending_runtime_request: Option<PendingRuntimeRequestSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_guest_message_count: Option<u64>,
    pub latest_visible_message: Option<LatestVisibleMessageSummary>,
    pub latest_user_message_at: Option<IsoDateTime>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub latest_user_authored_message_at: Option<Option<IsoDateTime>>,
    pub has_actionable_proposed_plan: bool,
    /// `OrchestrationV2PendingBackgroundTask[]`, decoding default `[]`; opaque.
    #[serde(default)]
    pub pending_background_tasks: Vec<Value>,
    /// Decoding default `[]`.
    #[serde(default)]
    pub provider_instance_history: Vec<ProviderInstanceId>,
    /// `OrchestrationV2ProviderGoal | null`; opaque.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub goal: Option<Option<Value>>,
    pub item_count: u64,
    pub visible_item_count: u64,
    pub created_at: IsoDateTime,
    pub updated_at: IsoDateTime,
    pub archived_at: Option<IsoDateTime>,
    pub settled_override: Option<SettledOverride>,
    pub settled_at: Option<IsoDateTime>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub unsettled_at: Option<Option<IsoDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub snoozed_until: Option<Option<IsoDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub snoozed_at: Option<Option<IsoDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub limit_recovery: Option<Option<Value>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub pinned_at: Option<Option<IsoDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub auto_settle_disabled_at: Option<Option<IsoDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub pin_order_key: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub active_order_key: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub last_visited_at: Option<Option<IsoDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub opens: Option<Option<Value>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub title_regeneration: Option<Option<TitleRegeneration>>,
    pub deleted_at: Option<IsoDateTime>,
}

/// `OrchestrationV2ShellSnapshot` (`OV2:2199-2211`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellSnapshot {
    /// PositiveInt.
    pub schema_version: u64,
    pub snapshot_sequence: u64,
    pub threads: Vec<ThreadShell>,
    pub archived_threads: Vec<ThreadShell>,
    pub projects: Vec<ProjectShell>,
    /// `OrchestrationV2OlderSettledThreads`; opaque (only with `omitOlderSettled`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub older_settled: Option<Value>,
}

/// `location` of shell thread items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShellLocation {
    Active,
    Archive,
}

/// Shell `snapshot` item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellSnapshotItem {
    pub snapshot: ShellSnapshot,
    /// Never sent by j3: its presence changes how the client merges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_repository_identity_roots: Option<Vec<String>>,
}

/// Shell `project.updated` item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShellProjectUpdated {
    pub sequence: u64,
    pub project: ProjectShell,
}

/// Shell `project.removed` item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellProjectRemoved {
    pub sequence: u64,
    pub project_id: ProjectId,
}

/// Shell `thread.updated` item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShellThreadUpdated {
    pub sequence: u64,
    pub location: ShellLocation,
    pub thread: ThreadShell,
}

/// Shell `thread.removed` item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellThreadRemoved {
    pub sequence: u64,
    pub location: ShellLocation,
    pub thread_id: ThreadId,
}

open_union! {
    /// `OrchestrationV2ShellStreamItem` (`OV2:2214-2245`), by `kind`.
    pub enum ShellStreamItem {
        tag = "kind";
        newtype {
            "snapshot" => Snapshot(ShellSnapshotItem),
            "project.updated" => ProjectUpdated(ShellProjectUpdated),
            "project.removed" => ProjectRemoved(ShellProjectRemoved),
            "thread.updated" => ThreadUpdated(ShellThreadUpdated),
            "thread.removed" => ThreadRemoved(ShellThreadRemoved),
        }
        unit {
            "synchronized" => Synchronized,
        }
    }
}

// ---------------------------------------------------------------------------
// Thread stream
// ---------------------------------------------------------------------------

/// Thread `snapshot` item (`OV2:3864-3873`). j3 omits every optional key, so
/// the client treats the projection as the complete timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSnapshotItem {
    pub snapshot_sequence: u64,
    pub projection: ThreadProjection,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub history_cursor: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_more_history: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub latest_local_turn_ordinal: Option<Option<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_items_omit_local_visible: Option<True>,
    /// Marks an id-table encoded projection; j3 never sends or decodes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_table: Option<True>,
}

/// Thread `event` item (`OV2:3874-3878`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadEventItem {
    pub sequence: u64,
    pub event: DomainEvent,
}

open_union! {
    /// `OrchestrationV2ThreadStreamItem` (`OV2:3858-3881`), by `kind`.
    pub enum ThreadStreamItem {
        tag = "kind";
        newtype {
            "snapshot" => Snapshot(ThreadSnapshotItem),
            "event" => Event(ThreadEventItem),
        }
        unit {
            "synchronized" => Synchronized,
        }
    }
}
