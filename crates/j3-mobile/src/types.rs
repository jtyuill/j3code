//! The records and enums Swift sees.
//!
//! Protocol types that cross the FFI unchanged are declared with
//! `#[uniffi::remote]`: the declarations repeat the `j3_protocol::v2` items
//! field for field, so a protocol change fails this crate's build. Types
//! that need a different shape (maps, `serde_json::Value`, names that clash
//! with Swift or Foundation, the flattened `Item`) are mirrored here with
//! `From` conversions.

use std::collections::HashMap;

use j3_client::{entities, methods};

pub use j3_client::{
    entities::{
        Answer, Attachment, BackgroundJob, Delivery, Goal, GoalState, ItemStatus, JobState,
        MessageRole, MessageSummary, ModelRef, NoticeLevel, Project, Question, QuestionOption,
        QueueEntry, Request, RequestKind, RequestStatus, RequestSummary, SearchMatch, Settled,
        ThinkingLevel, ThreadMode, ThreadStatus, TodoPhase, TodoStatus, TodoTask, ToolDetail,
        ToolImage, ToolOutput, Triage, Turn, TurnError, TurnErrorKind, TurnStatus, TurnSummary,
        WebResult, WokenBy,
    },
    envelope::ErrorCode,
    methods::{
        BrowseEntry, BrowseResult, BtwRecord, BtwStatus, BtwTurn, CatalogCommand, CatalogModel,
        CatalogSkill, CatalogStatus, EntriesResult, Entry, EntryKind,
    },
};

/// RFC 3339 UTC timestamp, e.g. `2026-11-09T11:18:00.000Z`.
type Time = String;

// ---------------------------------------------------------------------------
// Protocol types crossing unchanged
// ---------------------------------------------------------------------------

#[uniffi::remote(Record)]
pub struct ModelRef {
    pub provider: String,
    pub id: String,
    pub thinking: Option<ThinkingLevel>,
}

#[uniffi::remote(Enum)]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

#[uniffi::remote(Record)]
pub struct Attachment {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size: u64,
}

#[uniffi::remote(Record)]
pub struct Project {
    pub id: String,
    pub title: String,
    pub path: String,
    pub default_model: Option<ModelRef>,
    pub created_at: Time,
    pub updated_at: Time,
}

#[uniffi::remote(Enum)]
pub enum ThreadStatus {
    Idle,
    Starting,
    Running,
    Waiting,
    Stopping,
    Failed,
}

#[uniffi::remote(Enum)]
pub enum ThreadMode {
    Default,
    Plan,
}

#[uniffi::remote(Record)]
pub struct MessageSummary {
    pub role: MessageRole,
    pub text: String,
    pub at: Time,
}

#[uniffi::remote(Enum)]
pub enum MessageRole {
    User,
    Assistant,
}

#[uniffi::remote(Record)]
pub struct Triage {
    pub visited_at: Option<Time>,
    pub settled: Option<Settled>,
    pub settled_at: Option<Time>,
    pub unsettled_at: Option<Time>,
    pub snoozed_until: Option<Time>,
    pub snoozed_at: Option<Time>,
    pub last_wake_at: Option<Time>,
    pub pinned_at: Option<Time>,
    pub pin_order: Option<String>,
    pub active_order: Option<String>,
    pub auto_settle_off_at: Option<Time>,
    pub bumped_at: Time,
}

#[uniffi::remote(Enum)]
pub enum Settled {
    Settled,
    Active,
}

#[uniffi::remote(Record)]
pub struct Turn {
    pub id: String,
    pub thread_id: String,
    pub ordinal: u32,
    pub status: TurnStatus,
    pub model: ModelRef,
    pub requested_at: Time,
    pub started_at: Option<Time>,
    pub ended_at: Option<Time>,
    pub error: Option<TurnError>,
    pub woken_by: Option<WokenBy>,
}

#[uniffi::remote(Enum)]
pub enum TurnStatus {
    Running,
    Waiting,
    Completed,
    Failed,
    Stopped,
}

#[uniffi::remote(Record)]
pub struct TurnError {
    pub message: String,
    pub kind: TurnErrorKind,
}

#[uniffi::remote(Enum)]
pub enum TurnErrorKind {
    UsageLimit,
    Provider,
    Omp,
    Crashed,
    Refused,
}

#[uniffi::remote(Enum)]
pub enum WokenBy {
    BackgroundJob,
}

#[uniffi::remote(Record)]
pub struct TurnSummary {
    pub id: String,
    pub status: TurnStatus,
    pub requested_at: Time,
    pub started_at: Option<Time>,
    pub ended_at: Option<Time>,
    pub error_kind: Option<TurnErrorKind>,
}

#[uniffi::remote(Enum)]
pub enum ItemStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// How a user message reached omp.
#[uniffi::remote(Enum)]
pub enum Delivery {
    Prompt,
    Steer,
}

#[uniffi::remote(Record)]
pub struct ToolOutput {
    pub text: String,
    pub truncated: bool,
    pub images: Vec<ToolImage>,
    pub exit_code: Option<i64>,
    pub background: Option<BackgroundJob>,
}

#[uniffi::remote(Record)]
pub struct ToolImage {
    pub attachment_id: String,
    pub mime_type: String,
}

#[uniffi::remote(Record)]
pub struct BackgroundJob {
    pub job_id: String,
    pub state: JobState,
}

#[uniffi::remote(Enum)]
pub enum JobState {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[uniffi::remote(Enum)]
pub enum ToolDetail {
    Command {
        command: String,
    },
    FileChange {
        path: String,
        diff: Option<String>,
        additions: Option<u64>,
        deletions: Option<u64>,
        content: Option<String>,
    },
    Search {
        pattern: String,
        matches: Option<Vec<SearchMatch>>,
    },
    WebSearch {
        query: String,
        results: Option<Vec<WebResult>>,
    },
    Unknown,
}

#[uniffi::remote(Record)]
pub struct SearchMatch {
    pub path: String,
    pub line: Option<u64>,
    pub text: Option<String>,
}

#[uniffi::remote(Record)]
pub struct WebResult {
    pub title: String,
    pub url: String,
    pub snippet: Option<String>,
}

#[uniffi::remote(Record)]
pub struct TodoPhase {
    pub name: String,
    pub tasks: Vec<TodoTask>,
}

#[uniffi::remote(Record)]
pub struct TodoTask {
    pub text: String,
    pub status: TodoStatus,
}

#[uniffi::remote(Enum)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Abandoned,
    Blocked,
}

#[uniffi::remote(Record)]
pub struct Goal {
    pub objective: String,
    pub state: GoalState,
    pub tokens_used: Option<u64>,
    pub budget: Option<u64>,
}

#[uniffi::remote(Enum)]
pub enum GoalState {
    Active,
    Paused,
    BudgetLimited,
    Complete,
    Dropped,
}

#[uniffi::remote(Enum)]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

#[uniffi::remote(Record)]
pub struct Request {
    pub id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub kind: RequestKind,
    pub status: RequestStatus,
    pub title: Option<String>,
    pub questions: Vec<Question>,
    pub answers: Option<Vec<Answer>>,
    pub opened_at: Time,
    pub expires_at: Option<Time>,
    pub closed_at: Option<Time>,
}

#[uniffi::remote(Enum)]
pub enum RequestKind {
    Ask,
    Select,
    Confirm,
    Input,
    Editor,
}

#[uniffi::remote(Enum)]
pub enum RequestStatus {
    Open,
    Answered,
    Dismissed,
    Expired,
}

#[uniffi::remote(Record)]
pub struct Question {
    pub id: String,
    pub text: String,
    pub header: Option<String>,
    pub options: Vec<QuestionOption>,
    pub multiple: bool,
    pub allow_other: bool,
    pub default: Option<String>,
}

#[uniffi::remote(Record)]
pub struct QuestionOption {
    pub label: String,
    pub description: Option<String>,
}

#[uniffi::remote(Record)]
pub struct Answer {
    pub question_id: String,
    pub values: Vec<String>,
    pub other: Option<String>,
}

#[uniffi::remote(Record)]
pub struct RequestSummary {
    pub id: String,
    pub kind: RequestKind,
    pub title: Option<String>,
    pub opened_at: Time,
}

#[uniffi::remote(Record)]
pub struct QueueEntry {
    pub id: String,
    pub thread_id: String,
    pub position: u32,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub model: ModelRef,
    pub held: bool,
    pub created_at: Time,
}

#[uniffi::remote(Enum)]
pub enum ErrorCode {
    BadRequest,
    UnknownMethod,
    NotFound,
    Rejected,
    Conflict,
    TooLarge,
    Unavailable,
    Cancelled,
    Internal,
    Other,
}

#[uniffi::remote(Record)]
pub struct CatalogModel {
    pub provider: String,
    pub id: String,
    pub name: String,
    pub thinking: Option<Vec<ThinkingLevel>>,
    pub default: bool,
}

#[uniffi::remote(Record)]
pub struct CatalogSkill {
    pub name: String,
    pub description: String,
}

#[uniffi::remote(Record)]
pub struct CatalogCommand {
    pub name: String,
    pub description: String,
    pub hint: Option<String>,
}

#[uniffi::remote(Enum)]
pub enum CatalogStatus {
    Ready,
    Warning,
    Error,
}

#[uniffi::remote(Record)]
pub struct BrowseResult {
    pub parent_path: String,
    pub entries: Vec<BrowseEntry>,
}

#[uniffi::remote(Record)]
pub struct BrowseEntry {
    pub name: String,
    pub path: String,
}

#[uniffi::remote(Record)]
pub struct EntriesResult {
    pub entries: Vec<Entry>,
    pub truncated: bool,
}

#[uniffi::remote(Record)]
pub struct Entry {
    pub path: String,
    pub kind: EntryKind,
    pub ignored: Option<bool>,
}

#[uniffi::remote(Enum)]
pub enum EntryKind {
    File,
    Dir,
}

/// A side-question topic (`/btw`): the question and its follow-ups, oldest
/// first.
#[uniffi::remote(Record)]
pub struct BtwRecord {
    pub id: String,
    pub turns: Vec<BtwTurn>,
}

#[uniffi::remote(Record)]
pub struct BtwTurn {
    pub question: String,
    /// Markdown; grows while `status` is `Running`.
    pub answer: String,
    pub status: BtwStatus,
    pub error: Option<String>,
}

#[uniffi::remote(Enum)]
pub enum BtwStatus {
    Running,
    Complete,
    Cancelled,
    Error,
}

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

/// The connection's lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum ConnectionStatus {
    /// The first attempt is under way.
    Connecting,
    /// The socket is open and the catalog arrived; `HostSnapshot::synced`
    /// says whether the shell has caught up.
    Connected,
    /// The last attempt failed; the next one is `attempt + 1`.
    Reconnecting { attempt: u32, error: String },
    /// The server speaks another protocol version.
    Incompatible { message: String },
    /// `Host::close` was called.
    Closed,
}

impl From<&j3_client::Status> for ConnectionStatus {
    fn from(status: &j3_client::Status) -> Self {
        match status {
            j3_client::Status::Connecting => Self::Connecting,
            j3_client::Status::Connected => Self::Connected,
            j3_client::Status::Reconnecting { attempt, error } => Self::Reconnecting {
                attempt: *attempt,
                error: error.clone(),
            },
            j3_client::Status::Incompatible { message } => Self::Incompatible {
                message: message.clone(),
            },
            j3_client::Status::Closed => Self::Closed,
        }
    }
}

/// `GET /api/v1/info`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ServerInfo {
    /// Stable for the life of the server's database; key cached state by it.
    pub server_id: String,
    pub name: String,
    pub protocol: u32,
    pub version: String,
    /// Optional method groups the server serves (`terminal`, `git`, …).
    pub capabilities: Vec<String>,
    pub platform: String,
    pub home: String,
}

impl From<&j3_client::Info> for ServerInfo {
    fn from(info: &j3_client::Info) -> Self {
        Self {
            server_id: info.server_id.clone(),
            name: info.name.clone(),
            protocol: info.protocol,
            version: info.version.clone(),
            capabilities: info.capabilities.clone(),
            platform: info.platform.clone(),
            home: info.home.clone(),
        }
    }
}

/// Models, skills and slash commands omp offers.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Catalog {
    pub models: Vec<CatalogModel>,
    pub skills: Vec<CatalogSkill>,
    pub commands: Vec<CatalogCommand>,
    /// Slash commands per project id, for projects asked about by
    /// `Host::refresh_catalog`.
    pub project_commands: HashMap<String, Vec<CatalogCommand>>,
    pub status: CatalogStatus,
    pub message: Option<String>,
}

impl From<&methods::Catalog> for Catalog {
    fn from(catalog: &methods::Catalog) -> Self {
        Self {
            models: catalog.models.clone(),
            skills: catalog.skills.clone(),
            commands: catalog.commands.clone(),
            project_commands: catalog
                .project_commands
                .iter()
                .map(|(id, commands)| (id.clone(), commands.clone()))
                .collect(),
            status: catalog.status,
            message: catalog.message.clone(),
        }
    }
}

/// Everything `HostListener` receives: status, server identity, catalog and
/// shell.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct HostSnapshot {
    pub status: ConnectionStatus,
    /// `None` until the first connection.
    pub info: Option<ServerInfo>,
    /// `None` until the first connection.
    pub catalog: Option<Catalog>,
    /// The shell caught up with the server on a live socket; false while
    /// (re)subscribing, when `projects` and `threads` may be stale.
    pub synced: bool,
    /// The highest shell sequence applied.
    pub sequence: u64,
    /// By id.
    pub projects: Vec<Project>,
    /// Every thread that is not deleted, by id; archived ones included
    /// (filter on `archived_at`).
    pub threads: Vec<ThreadRow>,
}

impl From<&j3_client::ConnectionState> for HostSnapshot {
    fn from(state: &j3_client::ConnectionState) -> Self {
        Self {
            status: (&state.status).into(),
            info: state.info.as_ref().map(Into::into),
            catalog: state.catalog.as_ref().map(Into::into),
            synced: state.shell.synced,
            sequence: state.shell.sequence,
            projects: state.shell.projects.values().cloned().collect(),
            threads: state.shell.threads.values().map(Into::into).collect(),
        }
    }
}

// ---------------------------------------------------------------------------
// Threads
// ---------------------------------------------------------------------------

/// One thread list row (wire.md §4.2). Not called `Thread`, which would
/// shadow `Foundation.Thread` in Swift.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ThreadRow {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub created_at: Time,
    pub updated_at: Time,
    pub archived_at: Option<Time>,
    pub status: ThreadStatus,
    pub model: ModelRef,
    pub mode: ThreadMode,
    pub active_turn_id: Option<String>,
    pub pending_request: Option<RequestSummary>,
    pub queued: u32,
    pub last_message: Option<MessageSummary>,
    pub latest_turn: Option<TurnSummary>,
    pub latest_user_message_at: Option<Time>,
    pub goal: Option<Goal>,
    pub triage: Triage,
}

impl From<&entities::Thread> for ThreadRow {
    fn from(thread: &entities::Thread) -> Self {
        let entities::Thread {
            id,
            project_id,
            title,
            created_at,
            updated_at,
            archived_at,
            status,
            model,
            mode,
            active_turn_id,
            pending_request,
            queued,
            last_message,
            latest_turn,
            latest_user_message_at,
            goal,
            triage,
        } = thread.clone();
        Self {
            id,
            project_id,
            title,
            created_at,
            updated_at,
            archived_at,
            status,
            model,
            mode,
            active_turn_id,
            pending_request,
            queued,
            last_message,
            latest_turn,
            latest_user_message_at,
            goal,
            triage,
        }
    }
}

/// Where a `ThreadSnapshot` stands.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum ThreadSync {
    /// Not caught up: before the first snapshot, or resubscribing after a
    /// reconnect (the last state stays in place meanwhile).
    Loading,
    /// Caught up with the server on a live socket.
    Synced,
    /// The thread does not exist or was deleted; the subscription ended.
    Missing,
    /// The subscription failed and is being retried.
    Error { message: String },
}

impl From<&j3_client::ThreadSync> for ThreadSync {
    fn from(sync: &j3_client::ThreadSync) -> Self {
        match sync {
            j3_client::ThreadSync::Loading => Self::Loading,
            j3_client::ThreadSync::Synced => Self::Synced,
            j3_client::ThreadSync::Missing => Self::Missing,
            j3_client::ThreadSync::Error { message } => Self::Error {
                message: message.clone(),
            },
        }
    }
}

/// One thread with everything its feed shows; what `ThreadListener`
/// receives.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ThreadSnapshot {
    pub thread_id: String,
    pub sync: ThreadSync,
    /// The highest sequence applied.
    pub sequence: u64,
    /// `None` until the first snapshot.
    pub thread: Option<ThreadRow>,
    /// By `ordinal`.
    pub turns: Vec<Turn>,
    /// By `ordinal`.
    pub items: Vec<Item>,
    /// Open requests and those of the latest turn, in arrival order.
    pub requests: Vec<Request>,
    /// By `position`.
    pub queue: Vec<QueueEntry>,
}

impl From<&j3_client::ThreadView> for ThreadSnapshot {
    fn from(view: &j3_client::ThreadView) -> Self {
        Self {
            thread_id: view.thread_id.clone(),
            sync: (&view.sync).into(),
            sequence: view.sequence,
            thread: view.thread.as_ref().map(Into::into),
            turns: view.turns.clone(),
            items: view.items.iter().map(Into::into).collect(),
            requests: view.requests.clone(),
            queue: view.queue.clone(),
        }
    }
}

/// One feed row (wire.md §4.4): the common fields and its `kind`.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct Item {
    pub id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub ordinal: u64,
    pub status: ItemStatus,
    pub started_at: Time,
    pub ended_at: Option<Time>,
    pub kind: ItemKind,
}

/// The kind of an `Item` and its fields. Text is the raw markdown omp
/// wrote; prepare it with `prepare_math` / `split_inline_math` and
/// `rich_user_text`.
#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum ItemKind {
    User {
        text: String,
        attachments: Vec<Attachment>,
        delivery: Delivery,
    },
    /// Assistant answer.
    Text {
        text: String,
    },
    Thinking {
        text: String,
    },
    Tool {
        call_id: String,
        name: String,
        /// omp's one-line intent (`i` argument).
        intent: Option<String>,
        /// The tool's arguments as JSON text; their shape is the tool's.
        input_json: String,
        output: Option<ToolOutput>,
        /// A structured view of well-known tools.
        detail: Option<ToolDetail>,
    },
    Todo {
        phases: Vec<TodoPhase>,
    },
    Subagent {
        subagent_id: String,
        agent: String,
        /// omp's short description of the task.
        description: Option<String>,
        prompt: String,
        model: Option<String>,
        /// omp's live one-line progress; not kept across replays.
        progress: Option<String>,
        result: Option<String>,
    },
    Goal {
        goal: Goal,
    },
    Compaction {
        summary: Option<String>,
        tokens_before: Option<u64>,
    },
    Notice {
        level: NoticeLevel,
        text: String,
    },
    /// A user stop of the turn.
    Stop {
        requested_at: Time,
        stopped_at: Option<Time>,
    },
    /// A kind this build does not know (a newer server).
    Unknown,
}

impl From<&entities::Item> for Item {
    fn from(item: &entities::Item) -> Self {
        Self {
            id: item.id.clone(),
            thread_id: item.thread_id.clone(),
            turn_id: item.turn_id.clone(),
            ordinal: item.ordinal,
            status: item.status,
            started_at: item.started_at.clone(),
            ended_at: item.ended_at.clone(),
            kind: (&item.kind).into(),
        }
    }
}

impl From<&entities::ItemKind> for ItemKind {
    fn from(kind: &entities::ItemKind) -> Self {
        use entities::ItemKind as K;
        match kind.clone() {
            K::User(user) => Self::User {
                text: user.text,
                attachments: user.attachments,
                delivery: user.delivery,
            },
            K::Text(body) => Self::Text { text: body.text },
            K::Thinking(body) => Self::Thinking { text: body.text },
            K::Tool(tool) => Self::Tool {
                call_id: tool.call_id,
                name: tool.name,
                intent: tool.intent,
                input_json: tool.input.to_string(),
                output: tool.output,
                detail: tool.detail,
            },
            K::Todo(todo) => Self::Todo {
                phases: todo.phases,
            },
            K::Subagent(subagent) => Self::Subagent {
                subagent_id: subagent.subagent_id,
                agent: subagent.agent,
                description: subagent.description,
                prompt: subagent.prompt,
                model: subagent.model,
                progress: subagent.progress,
                result: subagent.result,
            },
            K::Goal(goal) => Self::Goal { goal },
            K::Compaction(compaction) => Self::Compaction {
                summary: compaction.summary,
                tokens_before: compaction.tokens_before,
            },
            K::Notice(notice) => Self::Notice {
                level: notice.level,
                text: notice.text,
            },
            K::Stop(stop) => Self::Stop {
                requested_at: stop.requested_at,
                stopped_at: stop.stopped_at,
            },
            K::Unknown => Self::Unknown,
        }
    }
}

// ---------------------------------------------------------------------------
// Command params and results
// ---------------------------------------------------------------------------

/// How `Host::send` treats a thread with an active turn (wire.md §5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SendDelivery {
    /// Start a turn when idle, otherwise queue.
    Auto,
    Queue,
    Steer,
    Restart,
}

impl From<SendDelivery> for methods::Delivery {
    fn from(delivery: SendDelivery) -> Self {
        match delivery {
            SendDelivery::Auto => Self::Auto,
            SendDelivery::Queue => Self::Queue,
            SendDelivery::Steer => Self::Steer,
            SendDelivery::Restart => Self::Restart,
        }
    }
}

/// `turnId` when a turn started (or steered), `queueEntryId` when queued.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SendOutcome {
    pub sequence: u64,
    pub turn_id: Option<String>,
    pub queue_entry_id: Option<String>,
}

impl From<methods::SendResult> for SendOutcome {
    fn from(result: methods::SendResult) -> Self {
        Self {
            sequence: result.sequence,
            turn_id: result.turn_id,
            queue_entry_id: result.queue_entry_id,
        }
    }
}

/// A page of a subagent's transcript.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct Transcript {
    /// Pass as `from_byte` to continue.
    pub next_byte: u64,
    /// The file is shorter than `from_byte`: start over from these entries.
    pub reset: bool,
    pub more: bool,
    pub entries: Vec<TranscriptEntry>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
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
        intent: Option<String>,
        /// The tool's arguments as JSON text.
        input_json: String,
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

impl From<methods::TranscriptResult> for Transcript {
    fn from(result: methods::TranscriptResult) -> Self {
        Self {
            next_byte: result.next_byte,
            reset: result.reset,
            more: result.more,
            entries: result.entries.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<methods::TranscriptEntry> for TranscriptEntry {
    fn from(entry: methods::TranscriptEntry) -> Self {
        use methods::TranscriptEntry as E;
        match entry {
            E::User { text } => Self::User { text },
            E::Assistant { text } => Self::Assistant { text },
            E::Thinking { text } => Self::Thinking { text },
            E::ToolCall {
                name,
                intent,
                input,
            } => Self::ToolCall {
                name,
                intent,
                input_json: input.to_string(),
            },
            E::ToolResult { name, text, failed } => Self::ToolResult { name, text, failed },
            E::Error { text } => Self::Error { text },
        }
    }
}
