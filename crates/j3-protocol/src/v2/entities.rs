//! The entities of protocol v2 (`docs/protocol/wire.md` §4).
//!
//! Timestamps are RFC 3339 UTC strings with millisecond precision. Optional
//! fields are omitted when absent.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// RFC 3339 UTC timestamp, e.g. `2026-11-09T11:18:00.000Z`.
pub type Time = String;

/// A model and its thinking level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRef {
    pub provider: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingLevel>,
}

/// omp's thinking levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

/// An uploaded or tool-produced file; bytes are fetched through the asset
/// route (wire.md §8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size: u64,
}

/// wire.md §4.1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub title: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<ModelRef>,
    pub created_at: Time,
    pub updated_at: Time,
}

/// wire.md §4.2: one thread list row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub created_at: Time,
    pub updated_at: Time,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<Time>,
    pub status: ThreadStatus,
    pub model: ModelRef,
    pub mode: ThreadMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_request: Option<RequestSummary>,
    pub queued: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message: Option<MessageSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_turn: Option<TurnSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_user_message_at: Option<Time>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<Goal>,
    pub triage: Triage,
}

/// wire.md §4.2.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadStatus {
    Idle,
    Starting,
    Running,
    Waiting,
    Stopping,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadMode {
    #[default]
    Default,
    Plan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageSummary {
    pub role: MessageRole,
    /// At most [`MessageSummary::MAX_TEXT`] characters.
    pub text: String,
    pub at: Time,
}

impl MessageSummary {
    pub const MAX_TEXT: usize = 300;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
}

/// wire.md §4.2.2; semantics in `docs/triage.md`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Triage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visited_at: Option<Time>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled: Option<Settled>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<Time>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsettled_at: Option<Time>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed_until: Option<Time>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed_at: Option<Time>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_wake_at: Option<Time>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_at: Option<Time>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_order: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_order: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_settle_off_at: Option<Time>,
    pub bumped_at: Time,
}

/// The user's explicit settle choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Settled {
    Settled,
    Active,
}

/// wire.md §4.3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub id: String,
    pub thread_id: String,
    pub ordinal: u32,
    pub status: TurnStatus,
    pub model: ModelRef,
    pub requested_at: Time,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<Time>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<Time>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TurnError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub woken_by: Option<WokenBy>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Running,
    Waiting,
    Completed,
    Failed,
    Stopped,
}

impl TurnStatus {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Running | Self::Waiting)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnError {
    pub message: String,
    pub kind: TurnErrorKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnErrorKind {
    UsageLimit,
    Provider,
    Omp,
    Crashed,
    Refused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WokenBy {
    BackgroundJob,
}

/// The thread row's `latestTurn`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSummary {
    pub id: String,
    pub status: TurnStatus,
    pub requested_at: Time,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<Time>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<Time>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<TurnErrorKind>,
}

impl From<&Turn> for TurnSummary {
    fn from(turn: &Turn) -> Self {
        Self {
            id: turn.id.clone(),
            status: turn.status,
            requested_at: turn.requested_at.clone(),
            started_at: turn.started_at.clone(),
            ended_at: turn.ended_at.clone(),
            error_kind: turn.error.as_ref().map(|error| error.kind),
        }
    }
}

/// wire.md §4.4: one feed row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub ordinal: u64,
    pub status: ItemStatus,
    pub started_at: Time,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<Time>,
    #[serde(flatten)]
    pub kind: ItemKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl ItemStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }
}

/// The kind of an [`Item`] and its fields. A kind this build does not know
/// decodes to [`ItemKind::Unknown`], so an older client keeps working with a
/// newer server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ItemKind {
    User(UserItem),
    Text(TextItem),
    Thinking(TextItem),
    Tool(ToolItem),
    Todo(TodoItem),
    Subagent(SubagentItem),
    Goal(Goal),
    Compaction(CompactionItem),
    Notice(NoticeItem),
    Stop(StopItem),
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserItem {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    pub delivery: Delivery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    Prompt,
    Steer,
}

/// Assistant answer (`text`) or thinking (`thinking`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextItem {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolItem {
    pub call_id: String,
    pub name: String,
    /// omp's one-line intent (`i` argument).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    pub input: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<ToolOutput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<ToolDetail>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolOutput {
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ToolImage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<BackgroundJob>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolImage {
    pub attachment_id: String,
    pub mime_type: String,
}

/// A tool call omp moved to the background; its final output replaces
/// `text` when the job ends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundJob {
    pub job_id: String,
    pub state: JobState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// Structured view of well-known tools (wire.md §4.4 `ToolDetail`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolDetail {
    Command {
        command: String,
    },
    FileChange {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        additions: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deletions: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
    },
    Search {
        pattern: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        matches: Option<Vec<SearchMatch>>,
    },
    WebSearch {
        query: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        results: Option<Vec<WebResult>>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchMatch {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebResult {
    pub title: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub phases: Vec<TodoPhase>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoPhase {
    pub name: String,
    pub tasks: Vec<TodoTask>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoTask {
    pub text: String,
    pub status: TodoStatus,
}

/// omp's todo statuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Abandoned,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentItem {
    pub subagent_id: String,
    pub agent: String,
    /// omp's short description of the task (e.g. "Answer PONG").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// omp's live one-line progress; not kept across replays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
}

/// omp goal mode: the `goal` item kind and the thread row's `goal`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Goal {
    pub objective: String,
    pub state: GoalState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<u64>,
}

/// omp's goal statuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GoalState {
    Active,
    Paused,
    BudgetLimited,
    Complete,
    Dropped,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_before: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeItem {
    pub level: NoticeLevel,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

/// A user stop of the turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopItem {
    pub requested_at: Time,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<Time>,
}

/// wire.md §4.5: a question omp is waiting on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub kind: RequestKind,
    pub status: RequestStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub questions: Vec<Question>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<Vec<Answer>>,
    pub opened_at: Time,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Time>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<Time>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    Ask,
    Select,
    Confirm,
    Input,
    Editor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus {
    Open,
    Answered,
    Dismissed,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<QuestionOption>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multiple: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_other: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Answer {
    pub question_id: String,
    pub values: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other: Option<String>,
}

/// The thread row's `pendingRequest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestSummary {
    pub id: String,
    pub kind: RequestKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub opened_at: Time,
}

/// wire.md §4.6: a prompt waiting for the active turn to end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueEntry {
    pub id: String,
    pub thread_id: String,
    pub position: u32,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    pub model: ModelRef,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
    pub created_at: Time,
}
