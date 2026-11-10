//! Params and results of every v2 method (`docs/protocol/wire.md` §5).
//!
//! State-changing methods take `commandId` (wire.md §3.4) and, unless their
//! result type says otherwise, answer [`Sequenced`].

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::entities::{Answer, ModelRef, Project, ThinkingLevel, Thread, ThreadMode, Time};

pub const SERVER_WATCH: &str = "server.watch";
pub const SERVER_REFRESH: &str = "server.refresh";
pub const SERVER_USAGE: &str = "server.usage";
pub const SHELL_WATCH: &str = "shell.watch";
pub const PROJECTS_CREATE: &str = "projects.create";
pub const PROJECTS_UPDATE: &str = "projects.update";
pub const PROJECTS_DELETE: &str = "projects.delete";
pub const PROJECTS_BROWSE: &str = "projects.browse";
pub const PROJECTS_LIST_ENTRIES: &str = "projects.listEntries";
pub const PROJECTS_SEARCH_ENTRIES: &str = "projects.searchEntries";
pub const PROJECTS_READ_FILE: &str = "projects.readFile";
pub const PROJECTS_IMAGE_URL: &str = "projects.imageUrl";
pub const THREADS_CREATE: &str = "threads.create";
pub const THREADS_WATCH: &str = "threads.watch";
pub const THREADS_GET: &str = "threads.get";
pub const THREADS_RENAME: &str = "threads.rename";
pub const THREADS_ARCHIVE: &str = "threads.archive";
pub const THREADS_UNARCHIVE: &str = "threads.unarchive";
pub const THREADS_DELETE: &str = "threads.delete";
pub const THREADS_SET_MODE: &str = "threads.setMode";
pub const TURNS_SEND: &str = "turns.send";
pub const TURNS_STOP: &str = "turns.stop";
pub const REQUESTS_ANSWER: &str = "requests.answer";
pub const REQUESTS_DISMISS: &str = "requests.dismiss";
pub const QUEUE_EDIT: &str = "queue.edit";
pub const QUEUE_CANCEL: &str = "queue.cancel";
pub const QUEUE_STEER: &str = "queue.steer";
pub const QUEUE_START: &str = "queue.start";
pub const QUEUE_RESUME: &str = "queue.resume";
pub const TRIAGE_VISIT: &str = "triage.visit";
pub const TRIAGE_MARK_UNREAD: &str = "triage.markUnread";
pub const TRIAGE_SETTLE: &str = "triage.settle";
pub const TRIAGE_UNSETTLE: &str = "triage.unsettle";
pub const TRIAGE_SNOOZE: &str = "triage.snooze";
pub const TRIAGE_WAKE: &str = "triage.wake";
pub const TRIAGE_PIN: &str = "triage.pin";
pub const TRIAGE_UNPIN: &str = "triage.unpin";
pub const TRIAGE_ORDER_PINNED: &str = "triage.orderPinned";
pub const TRIAGE_ORDER_ACTIVE: &str = "triage.orderActive";
pub const TRIAGE_SET_AUTO_SETTLE: &str = "triage.setAutoSettle";
pub const GOALS_CREATE: &str = "goals.create";
pub const GOALS_PAUSE: &str = "goals.pause";
pub const GOALS_RESUME: &str = "goals.resume";
pub const GOALS_DROP: &str = "goals.drop";
pub const SUBAGENTS_STOP: &str = "subagents.stop";
pub const SUBAGENTS_TRANSCRIPT: &str = "subagents.transcript";
pub const BTW_ASK: &str = "btw.ask";
pub const GIT_STATUS: &str = "git.status";
pub const GIT_PULL: &str = "git.pull";
pub const GIT_RUN: &str = "git.run";
pub const TERMINAL_ATTACH: &str = "terminal.attach";
pub const TERMINAL_WRITE: &str = "terminal.write";
pub const TERMINAL_RESIZE: &str = "terminal.resize";
pub const TERMINAL_CLOSE: &str = "terminal.close";
pub const ATTACHMENTS_DELETE: &str = "attachments.delete";

/// Every method name; a name outside this list is `unknown_method`.
pub const METHODS: &[&str] = &[
    SERVER_WATCH,
    SERVER_REFRESH,
    SERVER_USAGE,
    SHELL_WATCH,
    PROJECTS_CREATE,
    PROJECTS_UPDATE,
    PROJECTS_DELETE,
    PROJECTS_BROWSE,
    PROJECTS_LIST_ENTRIES,
    PROJECTS_SEARCH_ENTRIES,
    PROJECTS_READ_FILE,
    PROJECTS_IMAGE_URL,
    THREADS_CREATE,
    THREADS_WATCH,
    THREADS_GET,
    THREADS_RENAME,
    THREADS_ARCHIVE,
    THREADS_UNARCHIVE,
    THREADS_DELETE,
    THREADS_SET_MODE,
    TURNS_SEND,
    TURNS_STOP,
    REQUESTS_ANSWER,
    REQUESTS_DISMISS,
    QUEUE_EDIT,
    QUEUE_CANCEL,
    QUEUE_STEER,
    QUEUE_START,
    QUEUE_RESUME,
    TRIAGE_VISIT,
    TRIAGE_MARK_UNREAD,
    TRIAGE_SETTLE,
    TRIAGE_UNSETTLE,
    TRIAGE_SNOOZE,
    TRIAGE_WAKE,
    TRIAGE_PIN,
    TRIAGE_UNPIN,
    TRIAGE_ORDER_PINNED,
    TRIAGE_ORDER_ACTIVE,
    TRIAGE_SET_AUTO_SETTLE,
    GOALS_CREATE,
    GOALS_PAUSE,
    GOALS_RESUME,
    GOALS_DROP,
    SUBAGENTS_STOP,
    SUBAGENTS_TRANSCRIPT,
    BTW_ASK,
    GIT_STATUS,
    GIT_PULL,
    GIT_RUN,
    TERMINAL_ATTACH,
    TERMINAL_WRITE,
    TERMINAL_RESIZE,
    TERMINAL_CLOSE,
    ATTACHMENTS_DELETE,
];

/// A present field, `null` included, as `Some`; for "`null` clears" params.
fn present<'de, D, T>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(de).map(Some)
}

// ---- shared ----

/// The result of most state-changing methods: the sequence of the commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sequenced {
    pub sequence: u64,
}

/// `{}`: params of parameterless methods and results without content.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Empty {}

/// Params of the methods that name only a thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadParams {
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

/// Params of the stream methods that resume from a sequence (wire.md §6.2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<u64>,
}

// ---- server ----

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh: Option<bool>,
}

/// Models, skills and slash commands omp offers (wire.md §5.1.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub models: Vec<CatalogModel>,
    pub skills: Vec<CatalogSkill>,
    pub commands: Vec<CatalogCommand>,
    /// Slash commands per project id, for projects asked about by `server.refresh`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub project_commands: BTreeMap<String, Vec<CatalogCommand>>,
    pub status: CatalogStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModel {
    pub provider: String,
    pub id: String,
    pub name: String,
    /// Supported thinking levels; absent for models without reasoning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<Vec<ThinkingLevel>>,
    #[serde(default)]
    pub default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogSkill {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogCommand {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogStatus {
    /// omp answered.
    Ready,
    /// omp answered with problems; `message` says which.
    Warning,
    /// omp could not be queried.
    Error,
}

/// Provider usage limits, from `omp usage --json` (wire.md §5.1.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub generated_at: Time,
    pub accounts: Vec<UsageAccount>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageAccount {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<AccountInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<Time>,
    pub limits: Vec<UsageLimit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimit {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<UsageWindow>,
    pub used: f64,
    pub limit: f64,
    pub unit: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<Time>,
}

// ---- projects ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateProjectParams {
    pub title: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<ModelRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProjectParams {
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Absent keeps the default model; `null` clears it.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub default_model: Option<Option<ModelRef>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

/// Result of `projects.create` and `projects.update`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectResult {
    pub project: Project,
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteProjectParams {
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowseParams {
    pub partial_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowseResult {
    pub parent_path: String,
    pub entries: Vec<BrowseEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowseEntry {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Dir,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListEntriesParams {
    pub project_id: String,
    /// Directory relative to the project root; absent is the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
}

/// Result of `projects.listEntries` and `projects.searchEntries`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntriesResult {
    pub entries: Vec<Entry>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// Relative to the project root.
    pub path: String,
    pub kind: EntryKind,
    /// Matched by gitignore; `projects.listEntries` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignored: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchEntriesParams {
    pub project_id: String,
    pub query: String,
    /// At most 200.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<EntryKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images_only: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadFileParams {
    pub project_id: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_missing: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadFileResult {
    pub path: String,
    pub text: String,
    pub size: u64,
    pub truncated: bool,
    pub exists: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageUrlParams {
    pub project_id: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageUrlResult {
    pub url: String,
    pub expires_at: Time,
}

// ---- threads ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateThreadParams {
    pub project_id: String,
    pub model: ModelRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ThreadMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub text: String,
    /// Attachment ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadResult {
    pub thread: Box<Thread>,
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchThreadParams {
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameThreadParams {
    pub thread_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteThreadParams {
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only_if_unused: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetModeParams {
    pub thread_id: String,
    pub mode: ThreadMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

// ---- turns ----

/// How `turns.send` treats a thread with an active turn (wire.md §5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    Auto,
    Queue,
    Steer,
    Restart,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendParams {
    pub thread_id: String,
    pub text: String,
    /// Attachment ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    pub delivery: Delivery,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

/// `turnId` when a turn started (or steered), `queueEntryId` when queued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendResult {
    pub sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_entry_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopParams {
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_queue: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

// ---- requests ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnswerParams {
    pub thread_id: String,
    pub request_id: String,
    pub answers: Vec<Answer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DismissParams {
    pub thread_id: String,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

// ---- queue ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueEditParams {
    pub thread_id: String,
    pub entry_id: String,
    pub text: String,
    /// Attachment ids; absent keeps the entry's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

/// Params of `queue.cancel`, `queue.steer` and `queue.start`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueEntryParams {
    pub thread_id: String,
    pub entry_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

// ---- triage ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisitParams {
    pub thread_id: String,
    pub visited_at: Time,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettleParams {
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<Time>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnoozeParams {
    pub thread_id: String,
    pub until: Time,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinParams {
    pub thread_id: String,
    /// Order key; absent pins first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

/// Params of `triage.orderPinned` and `triage.orderActive`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderParams {
    pub thread_id: String,
    pub order: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoSettleParams {
    pub thread_id: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

// ---- goals and subagents ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateGoalParams {
    pub thread_id: String,
    pub objective: String,
    /// Tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopSubagentParams {
    pub thread_id: String,
    pub subagent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptParams {
    pub thread_id: String,
    pub subagent_id: String,
    pub from_byte: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptResult {
    pub next_byte: u64,
    /// The file is shorter than `fromByte`: start over from these entries.
    pub reset: bool,
    pub more: bool,
    pub entries: Vec<TranscriptEntry>,
}

/// One entry of a subagent transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TranscriptEntry {
    User {
        text: String,
    },
    Assistant {
        text: String,
    },
    Thinking {
        text: String,
    },
    ToolCall {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        intent: Option<String>,
        input: Value,
    },
    ToolResult {
        name: String,
        text: String,
        failed: bool,
    },
    Error {
        text: String,
    },
}

// ---- btw ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BtwAskParams {
    pub thread_id: String,
    pub question: String,
    /// The topic this question follows up on; absent starts a new topic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_id: Option<String>,
}

/// Items of the `btw.ask` stream (wire.md §5.13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BtwItem {
    /// The whole topic: first, and on every status change.
    Record { record: BtwRecord },
    /// Text appended to the answer of the topic's last turn.
    #[serde(rename_all = "camelCase")]
    Delta { record_id: String, append: String },
}

/// A side-question topic: the question and its follow-ups, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BtwRecord {
    pub id: String,
    pub turns: Vec<BtwTurn>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BtwTurn {
    pub question: String,
    /// Markdown; grows by `delta` items while `running`.
    pub answer: String,
    pub status: BtwStatus,
    /// Why the turn failed (`status` `error`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BtwStatus {
    Running,
    Complete,
    Cancelled,
    Error,
}

// ---- git ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectParams {
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

/// A project directory's git state (wire.md §5.10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatus {
    /// False when the project is not in a git repository; the rest is then empty.
    pub is_repo: bool,
    /// The current branch; absent on a detached HEAD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The branch is the remote's default branch.
    pub on_default_branch: bool,
    /// The repository has a remote to push to.
    pub has_remote: bool,
    /// The remote is on GitHub, so pull requests work.
    pub github: bool,
    pub has_upstream: bool,
    pub ahead: u64,
    pub behind: u64,
    /// Uncommitted changes exist (tracked or untracked).
    pub dirty: bool,
    pub files: Vec<GitFile>,
    pub additions: u64,
    pub deletions: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<PullRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFile {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
}

/// The branch's newest pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequest {
    pub url: String,
    pub state: PullRequestState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestState {
    Open,
    Closed,
    Merged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullResult {
    pub status: PullStatus,
    pub branch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullStatus {
    Pulled,
    UpToDate,
}

/// What `git.run` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitAction {
    Commit,
    Push,
    Pr,
    CommitPush,
    CommitPushPr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitRunParams {
    pub project_id: String,
    pub action: GitAction,
    /// Commit message; absent lets omp write one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

/// A step of `git.run`; also the `data.phase` of its failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitPhase {
    Branch,
    Commit,
    Push,
    Pr,
}

/// Items of the `git.run` stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GitRunItem {
    Phase { phase: GitPhase },
    Finished { result: GitRunResult },
}

/// The outcome of `git.run`, shown as a success toast.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitRunResult {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The toast's button.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<GitNext>,
}

/// A follow-up offered after `git.run`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GitNext {
    /// Open the pull request at `url`.
    OpenPr { label: String, url: String },
    /// Run `git.run` with `action`.
    Run { label: String, action: GitAction },
}

// ---- terminal ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachParams {
    pub thread_id: String,
    pub terminal_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cols: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u16>,
    /// Extra environment for the shell.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Start a new shell if the old one exited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<bool>,
}

/// Items of the `terminal.attach` stream: `snapshot` first, then live events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalItem {
    Snapshot(TerminalSnapshot),
    Output {
        data: String,
    },
    #[serde(rename_all = "camelCase")]
    Exited {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_signal: Option<i64>,
    },
    /// The shell failed; the stream ends after it.
    Failed {
        message: String,
    },
    Cleared,
    /// The terminal was closed; the stream ends after it.
    Closed,
    /// A new shell replaced the old one: reset the screen to this snapshot.
    Restarted(TerminalSnapshot),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSnapshot {
    /// Output so far, to replay into an empty screen.
    pub history: String,
    pub state: TerminalState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_signal: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalState {
    Running,
    Exited,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalWriteParams {
    pub thread_id: String,
    pub terminal_id: String,
    /// 1–65,536 characters.
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalResizeParams {
    pub thread_id: String,
    pub terminal_id: String,
    /// 1–1000.
    pub cols: u16,
    /// 1–500.
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalCloseParams {
    pub thread_id: String,
    /// Absent closes all of the thread's terminals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_history: Option<bool>,
}

// ---- attachments ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteAttachmentParams {
    pub attachment_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}
