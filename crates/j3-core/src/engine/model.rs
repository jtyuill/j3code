//! One thread's state machine: thread commands (runs, queue, steer, stop,
//! answers) and the omp event → projection mapping (ios-surface §5).
//!
//! Everything here is synchronous and free of I/O: inputs are commands and
//! omp [`Event`]s, outputs are projection changes (drained by the actor into a
//! hub commit) plus [`Effect`]s the actor must perform.

use std::collections::{BTreeMap, HashMap};

use j3_protocol::t3::{
    self, True,
    chat_attachment::ChatAttachment,
    model::{ModelSelection, ProviderOptionSelection, ProviderOptionSelectionValue},
    orchestration::{
        Actor, AppThread, AppThreadLineage, AssistantMessageItem, CommandExecutionItem,
        CompactionItem, ConversationMessage, CreationSource, DynamicToolItem, ErrorItem,
        FileChangeItem, FileSearchItem, FileSearchResult, MessageContext, MessageItem, MessageRole,
        NativeRefStrength, PlanArtifact, PlanArtifactKind, PlanArtifactStatus, PlanStep,
        PlanStepStatus, ProviderCapabilities, ProviderFailure, ProviderFailureClass,
        ProviderInteractionMode, ProviderRef, ProviderRetry, ProviderSession,
        ProviderSessionStatus, ProviderThread, ProviderThreadStatus, ProviderTurn,
        ProviderTurnStatus, ProviderUserInputAnswers, ReasoningItem, ResponseCapability, Run,
        RunAttempt, RunAttemptReason, RunAttemptStatus, RunInterruptRequestItem, RunStatus,
        RuntimeMode, RuntimeRequest, RuntimeRequestKind, RuntimeRequestStatus, TodoListItem,
        TurnItem, TurnItemKind, TurnItemStatus, UserInputAnswerPayload, UserInputQuestion,
        UserInputQuestionOption, UserInputRequestItem, UserMessageInputIntent, UserMessageItem,
        WebSearchItem, WebSearchResult,
    },
};
use serde_json::{Map, Value, json};

use super::projection::{DRIVER, Kind, PROVIDER_INSTANCE, Projection};
use crate::omp::omp_rpc::{
    AgentMessage, AnswersUiResponse, AskAnswer, AskQuestion, AssistantContent,
    AssistantMessageEvent, CancelUiResponse, ConfirmUiResponse, Event, ExtensionUiRequest,
    ExtensionUiResponse, LitTrue, PromptError, PromptStatus, RpcAgentEvent, RpcNotification,
    SessionState, ThinkingLevel, TodoPhase, TodoStatus, ToolCall, ValueUiResponse,
};

mod agents;
pub mod triage;

/// Option id of the thinking descriptor j3 advertises.
pub const THINKING_OPTION: &str = "thinking";
/// omp's ask tool; its row is represented by the `user_input_request` item.
const ASK_TOOL: &str = "ask";
const TODO_TOOL: &str = "todo";
const MAX_FAILURE_MESSAGE: usize = 4096;

/// Work the actor must do after an input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Re-read `get_state` (todos, model) and feed it to [`ThreadModel::apply_state`].
    RefreshState,
    /// The active run ended; the next queued run may start.
    RunSettled,
    /// Expire runtime request `request_id` after `after_ms` unless answered.
    ExpireAfter { request_id: String, after_ms: u64 },
    /// Read the transcript of finished subagent `subagent_id` and feed it to
    /// [`ThreadModel::apply_subagent_transcript`].
    FetchSubagentResult { subagent_id: String },
    /// Store images a tool returned, under the attachment ids its item's
    /// output names.
    StoreToolImages(Vec<ToolImage>),
}

/// An image block of a tool result, to keep as an attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolImage {
    pub attachment_id: String,
    pub mime_type: String,
    /// Base64, as omp sent it.
    pub data: String,
}

/// Most images kept from one tool result (`MAX_TOOL_OUTPUT_IMAGES`).
const MAX_TOOL_OUTPUT_IMAGES: usize = 8;

/// The raster image blocks (`{type:"image", data, mimeType}`) of a tool
/// result, each with a new attachment id; at most
/// [`MAX_TOOL_OUTPUT_IMAGES`], none larger than the image upload limit.
fn result_images(result: &Value) -> Vec<ToolImage> {
    const MAX_BASE64: usize = (crate::attachments::MAX_IMAGE_BYTES as usize).div_ceil(3) * 4;
    let Some(content) = result.get("content").and_then(Value::as_array) else {
        return Vec::new();
    };
    content
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("image"))
        .filter_map(|block| {
            let mime_type = block.get("mimeType")?.as_str()?.to_ascii_lowercase();
            let data = block.get("data")?.as_str()?;
            (crate::attachments::SUPPORTED_IMAGE_MIME_TYPES.contains(&mime_type.as_str())
                && !data.is_empty()
                && data.len() <= MAX_BASE64)
                .then(|| ToolImage {
                    attachment_id: crate::attachments::new_id(),
                    mime_type,
                    data: data.to_owned(),
                })
        })
        .take(MAX_TOOL_OUTPUT_IMAGES)
        .collect()
}

/// Gives a dynamic tool's output its images: t3's output blocks, the text
/// first and then one `{type:"image", mimeType}` per image, which also
/// names the j3 attachment holding its bytes (`attachmentId`). False when
/// the item is not a dynamic tool.
fn attach_tool_images(item: &mut TurnItem, images: &[ToolImage]) -> bool {
    let TurnItemKind::DynamicTool(tool) = &mut item.kind else {
        return false;
    };
    let mut blocks = Vec::with_capacity(images.len() + 1);
    if let Some(Some(Value::String(text))) = tool.output.take() {
        blocks.push(json!({"type": "text", "text": text}));
    }
    blocks.extend(images.iter().map(|image| {
        json!({"type": "image", "mimeType": image.mime_type, "attachmentId": image.attachment_id})
    }));
    tool.output = Some(Some(Value::Array(blocks)));
    true
}

/// The image blocks of a dynamic tool's output that clients show, in the
/// order that numbers `tool-output-image` assets (t3's `toolOutputImages`):
/// raster types only, at most [`MAX_TOOL_OUTPUT_IMAGES`].
pub fn tool_output_images(output: &Value) -> impl Iterator<Item = &Value> {
    output
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| {
            block.get("type").and_then(Value::as_str) == Some("image")
                && block
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .is_some_and(|mime| {
                        crate::attachments::SUPPORTED_IMAGE_MIME_TYPES.contains(&mime)
                    })
        })
        .take(MAX_TOOL_OUTPUT_IMAGES)
}

/// The attachment holding image `index` of a dynamic tool's output.
pub fn tool_output_image(output: &Value, index: usize) -> Option<&str> {
    tool_output_images(output)
        .nth(index)?
        .get("attachmentId")?
        .as_str()
}

/// A command the model refuses; the message is shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reject(pub String);

impl Reject {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// A user message to run, queue or steer.
#[derive(Debug, Clone, PartialEq)]
pub struct UserInput {
    pub message_id: String,
    pub text: String,
    pub attachments: Vec<ChatAttachment>,
    pub context: Option<MessageContext>,
    pub model_selection: ModelSelection,
    pub created_by: Actor,
    pub creation_source: CreationSource,
}

/// Fields of a new thread.
#[derive(Debug, Clone)]
pub struct NewThread {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub model_selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
    pub created_by: Actor,
    pub creation_source: CreationSource,
    pub cwd: String,
}

/// How a `message.dispatch` is delivered once resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    Start,
    Queue,
    Steer { run_id: String },
    Restart { run_id: String },
}

/// The run currently driven by omp.
#[derive(Debug, Clone)]
struct Live {
    run_id: String,
    node_id: String,
    attempt_id: Option<String>,
    turn_id: Option<String>,
    /// `(omp message id, content index)` → item id.
    blocks: HashMap<(String, i64), String>,
    /// `toolCallId` → item id (`None`: no row, e.g. `ask`).
    tools: HashMap<String, Option<String>>,
    /// Tool names by call id, for end events.
    tool_names: HashMap<String, String>,
    status: Option<PromptStatus>,
    error: Option<PromptError>,
    interrupt_requested: bool,
    compaction: Option<String>,
    todo_item: Option<String>,
}

impl Live {
    fn new(run_id: String, node_id: String) -> Self {
        Self {
            run_id,
            node_id,
            attempt_id: None,
            turn_id: None,
            blocks: HashMap::new(),
            tools: HashMap::new(),
            tool_names: HashMap::new(),
            status: None,
            error: None,
            interrupt_requested: false,
            compaction: None,
            todo_item: None,
        }
    }
}

/// What a pending UI request answers to.
#[derive(Debug, Clone)]
enum UiMethod {
    Ask(Vec<AskQuestion>),
    Select,
    Confirm,
    Value,
}

#[derive(Debug, Clone)]
struct PendingUi {
    omp_id: String,
    method: UiMethod,
    item_id: String,
}

pub struct ThreadModel {
    pub proj: Projection,
    live: Option<Live>,
    /// Runtime request id → pending omp UI request.
    ui: HashMap<String, PendingUi>,
    next_ordinal: u64,
    /// Last `recentOutput` lines per subagent id: its result when the
    /// transcript can't be read.
    subagent_output: HashMap<String, Vec<String>>,
}

fn now() -> String {
    t3::now_iso()
}

fn session_id(thread_id: &str) -> String {
    format!("ps_{thread_id}")
}

fn provider_thread_id(thread_id: &str) -> String {
    format!("pth_{thread_id}")
}

fn non_blank(text: &str, fallback: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn truncate(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => text[..at].to_owned(),
        None => text.to_owned(),
    }
}

/// Text of a tool result `{content:[{type:"text",text}]}`.
fn result_text(result: &Value) -> Option<String> {
    let content = result.get("content")?.as_array()?;
    let text: Vec<&str> = content
        .iter()
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect();
    (!text.is_empty()).then(|| text.concat())
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

/// Whether a tool's `path` names a file rather than an internal URL
/// (`agent://`, `xd://`, `proc://`, …) or a web page.
fn is_file_path(path: &str) -> bool {
    !path.contains("://")
}

/// The file a hashline `edit` input names first, from its `[PATH#TAG]`
/// header.
fn hashline_path(input: &str) -> Option<&str> {
    input.lines().find_map(|line| {
        let inner = line.trim().strip_prefix('[')?.strip_suffix(']')?;
        let (path, tag) = inner.rsplit_once('#')?;
        let path = path.trim();
        (tag.len() == 4 && tag.chars().all(|c| c.is_ascii_hexdigit()) && !path.is_empty())
            .then_some(path)
    })
}

/// The file an `edit` or `write` call changes, if it changes one.
fn changed_file<'a>(name: &str, args: &'a Value) -> Option<&'a str> {
    let path = arg_str(args, "path")
        .or_else(|| (name == "edit").then(|| arg_str(args, "input").and_then(hashline_path))?)?;
    is_file_path(path).then(|| path.trim())
}

/// TurnItem kind for an omp tool call (ios-surface §5 "Tool calls").
fn tool_kind(name: &str, args: &Value) -> TurnItemKind {
    match name {
        "bash" => TurnItemKind::CommandExecution(CommandExecutionItem {
            input: arg_str(args, "command").unwrap_or_default().to_owned(),
            output: None,
            output_omitted: None,
            output_indicates_failure: None,
            exit_code: None,
            monitor: None,
        }),
        "edit" | "write" if changed_file(name, args).is_some() => {
            TurnItemKind::FileChange(FileChangeItem {
                file_name: changed_file(name, args).unwrap_or_default().to_owned(),
                additions: None,
                deletions: None,
                diff_str: None,
                old_str: None,
                new_str: (name == "write")
                    .then(|| args.get("content").and_then(Value::as_str))
                    .flatten()
                    .map(str::to_owned),
                changes: None,
            })
        }
        // `glob` and `find` take their pattern as `path`.
        "grep" | "find" | "glob" => TurnItemKind::FileSearch(FileSearchItem {
            pattern: arg_str(args, "pattern")
                .or_else(|| arg_str(args, "path"))
                .map(str::to_owned),
            results: None,
        }),
        "web_search" => TurnItemKind::WebSearch(WebSearchItem {
            patterns: arg_str(args, "query").map(|query| vec![query.to_owned()]),
            results: None,
        }),
        _ => TurnItemKind::DynamicTool(DynamicToolItem {
            tool_name: Some(name.to_owned()),
            viewed_image_path: None,
            input: args.clone(),
            output: None,
            output_omitted: None,
        }),
    }
}

/// `thinking` level from a selection, as omp's level.
pub fn thinking_level(selection: &ModelSelection) -> Option<ThinkingLevel> {
    match selection.option(THINKING_OPTION)? {
        ProviderOptionSelectionValue::String(level) => {
            serde_json::from_value(Value::String(level.clone())).ok()
        }
        ProviderOptionSelectionValue::Bool(_) => None,
    }
}

/// Splits a model slug `provider/id` at the first `/`.
pub fn split_model(slug: &str) -> Option<(&str, &str)> {
    slug.split_once('/')
        .filter(|(provider, id)| !provider.is_empty() && !id.is_empty())
}

/// Rewrites the first `$name` naming a known skill into a leading
/// `/skill:name` (omp expands one leading skill command).
pub fn rewrite_skill(text: &str, skills: &[String]) -> String {
    let bytes = text.as_bytes();
    let mut at = 0;
    while let Some(offset) = text[at..].find('$') {
        let start = at + offset;
        let name_start = start + 1;
        let end = text[name_start..]
            .find(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_' || c == ':' || c == '.'))
            .map_or(text.len(), |len| name_start + len);
        let name = &text[name_start..end];
        let boundary = start == 0 || bytes[start - 1].is_ascii_whitespace();
        if boundary && !name.is_empty() && skills.iter().any(|skill| skill == name) {
            let before = text[..start].trim();
            let after = text[end..].trim();
            let rest = match (before.is_empty(), after.is_empty()) {
                (true, _) => after.to_owned(),
                (_, true) => before.to_owned(),
                _ => format!("{before} {after}"),
            };
            return if rest.is_empty() {
                format!("/skill:{name}")
            } else {
                format!("/skill:{name} {rest}")
            };
        }
        at = name_start;
    }
    text.to_owned()
}

impl ThreadModel {
    /// A brand-new thread with its provider session and provider thread.
    pub fn create(spec: NewThread) -> Self {
        let at = now();
        let pth = provider_thread_id(&spec.id);
        let ps = session_id(&spec.id);
        let thread = AppThread {
            created_by: spec.created_by,
            creation_source: spec.creation_source,
            id: spec.id.clone(),
            project_id: spec.project_id,
            title: non_blank(&spec.title, "New thread"),
            provider_instance_id: PROVIDER_INSTANCE.to_owned(),
            model_selection: spec.model_selection.clone(),
            runtime_mode: spec.runtime_mode,
            interaction_mode: spec.interaction_mode,
            branch: None,
            worktree_path: None,
            linked_pull_request: None,
            pull_requests: None,
            branch_pull_request: None,
            active_provider_thread_id: Some(pth.clone()),
            history_origin: None,
            link_origin: None,
            delegated_from: None,
            lineage: AppThreadLineage::root(spec.id.clone()),
            forked_from: None,
            created_at: at.clone(),
            updated_at: at.clone(),
            archived_at: None,
            settled_override: None,
            settled_at: None,
            unsettled_at: None,
            snoozed_until: None,
            snoozed_at: None,
            last_snooze_wake_at: None,
            limit_recovery: None,
            guest_share: None,
            pinned_at: None,
            auto_settle_disabled_at: None,
            pin_order_key: None,
            active_order_key: None,
            last_visited_at: None,
            opens: None,
            title_regeneration: None,
            rollback_request_id: None,
            rollback_failure: None,
            deleted_at: None,
        };
        let mut proj = Projection::new(thread);
        proj.sessions.push(ProviderSession {
            id: ps.clone(),
            driver: DRIVER.to_owned(),
            provider_instance_id: PROVIDER_INSTANCE.to_owned(),
            status: ProviderSessionStatus::Stopped,
            cwd: spec.cwd,
            model: Some(spec.model_selection.model.clone()),
            capabilities: ProviderCapabilities::omp(),
            created_at: at.clone(),
            updated_at: at.clone(),
            last_error: None,
        });
        proj.provider_threads.push(ProviderThread {
            id: pth.clone(),
            driver: DRIVER.to_owned(),
            provider_instance_id: PROVIDER_INSTANCE.to_owned(),
            provider_session_id: Some(ps.clone()),
            app_thread_id: Some(spec.id.clone()),
            owner_node_id: None,
            native_thread_ref: None,
            native_conversation_head_ref: None,
            status: ProviderThreadStatus::NotLoaded,
            first_run_ordinal: None,
            last_run_ordinal: None,
            handoff_ids: Vec::new(),
            forked_from: None,
            pending_background_tasks: Vec::new(),
            context_usage: None,
            native_metadata: None,
            goal: None,
            created_at: at.clone(),
            updated_at: at,
        });
        proj.touch_created(Kind::Thread, &spec.id);
        proj.touch_created(Kind::Session, &ps);
        proj.touch_created(Kind::ProviderThread, &pth);
        Self::load(proj)
    }

    /// Wraps a projection loaded from the store.
    pub fn load(proj: Projection) -> Self {
        let next_ordinal = proj
            .items
            .iter()
            .map(|item| item.ordinal + 1)
            .max()
            .unwrap_or(1);
        Self {
            proj,
            live: None,
            ui: HashMap::new(),
            next_ordinal,
            subagent_output: HashMap::new(),
        }
    }

    pub fn thread_id(&self) -> &str {
        &self.proj.thread.id
    }

    fn ps(&self) -> String {
        session_id(&self.proj.thread.id)
    }

    fn pth(&self) -> String {
        provider_thread_id(&self.proj.thread.id)
    }

    /// The run omp is driving, if any.
    pub fn live_run_id(&self) -> Option<&str> {
        self.live.as_ref().map(|live| live.run_id.as_str())
    }

    /// The live run has a running provider turn (steering is possible).
    pub fn has_running_turn(&self) -> bool {
        self.live
            .as_ref()
            .is_some_and(|live| live.turn_id.is_some())
    }

    /// No run is active and no UI request is pending.
    pub fn is_settled(&self) -> bool {
        self.live.is_none() && self.proj.active_run().is_none()
    }

    // -----------------------------------------------------------------------
    // Entity helpers
    // -----------------------------------------------------------------------

    fn push_item(
        &mut self,
        run_id: Option<String>,
        node_id: Option<String>,
        status: TurnItemStatus,
        title: Option<String>,
        kind: TurnItemKind,
    ) -> String {
        let ordinal = self.next_ordinal;
        self.next_ordinal += 1;
        let id = format!("ti_{ordinal}");
        let at = now();
        let provider_turn_id = self
            .live
            .as_ref()
            .filter(|live| Some(&live.run_id) == run_id.as_ref())
            .and_then(|live| live.turn_id.clone());
        let terminal = is_terminal(status);
        self.proj.items.push(TurnItem {
            deferred_work: None,
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id: id.clone(),
            thread_id: self.proj.thread.id.clone(),
            run_id,
            node_id,
            provider_thread_id: Some(self.pth()),
            provider_turn_id,
            native_item_ref: None,
            parent_item_id: None,
            ordinal,
            status,
            title,
            started_at: Some(at.clone()),
            completed_at: terminal.then(|| at.clone()),
            updated_at: at,
            kind,
        });
        self.proj.touch_updated(Kind::Item, &id);
        id
    }

    /// Item of the live run.
    fn push_live_item(
        &mut self,
        status: TurnItemStatus,
        title: Option<String>,
        kind: TurnItemKind,
    ) -> String {
        let (run_id, node_id) = self.ensure_live();
        self.push_item(Some(run_id), Some(node_id), status, title, kind)
    }

    /// Updates an item and marks it changed (streaming or not).
    fn update_item(&mut self, id: &str, streaming: bool, update: impl FnOnce(&mut TurnItem)) {
        let Some(item) = self.proj.item_mut(id) else {
            return;
        };
        update(item);
        item.updated_at = now();
        if is_terminal(item.status) && item.completed_at.is_none() {
            item.completed_at = Some(item.updated_at.clone());
        }
        if streaming {
            self.proj.touch_streaming(Kind::Item, id);
        } else {
            self.proj.touch_updated(Kind::Item, id);
        }
    }

    fn update_run(&mut self, id: &str, update: impl FnOnce(&mut Run)) {
        if let Some(run) = self.proj.run_mut(id) {
            update(run);
            self.proj.touch_updated(Kind::Run, id);
        }
    }

    fn set_session_status_inner(&mut self, status: ProviderSessionStatus) {
        let ps = self.ps();
        if let Some(session) = self.proj.sessions.iter_mut().find(|s| s.id == ps)
            && session.status != status
        {
            session.status = status;
            session.updated_at = now();
            self.proj.touch_updated(Kind::Session, &ps);
        }
    }

    /// omp process lifecycle: `starting`, `ready`, `stopped`, `error`.
    pub fn set_session_status(&mut self, status: ProviderSessionStatus, error: Option<String>) {
        let ps = self.ps();
        if let Some(session) = self.proj.sessions.iter_mut().find(|s| s.id == ps)
            && (session.status != status || session.last_error != error)
        {
            session.status = status;
            session.last_error = error;
            session.updated_at = now();
            self.proj.touch_updated(Kind::Session, &ps);
        }
        if status == ProviderSessionStatus::Stopped {
            self.set_provider_thread_status(ProviderThreadStatus::NotLoaded, None);
        } else if status == ProviderSessionStatus::Ready && self.live.is_none() {
            self.set_provider_thread_status(ProviderThreadStatus::Idle, None);
        }
    }

    fn set_provider_thread_status(
        &mut self,
        status: ProviderThreadStatus,
        run_ordinal: Option<u64>,
    ) {
        let pth = self.pth();
        if let Some(thread) = self.proj.provider_threads.iter_mut().find(|t| t.id == pth) {
            let mut changed = thread.status != status;
            thread.status = status;
            if let Some(ordinal) = run_ordinal {
                if thread.first_run_ordinal.is_none() {
                    thread.first_run_ordinal = Some(ordinal);
                }
                changed |= thread.last_run_ordinal != Some(ordinal);
                thread.last_run_ordinal = Some(ordinal);
            }
            if changed {
                thread.updated_at = now();
                self.proj.touch_updated(Kind::ProviderThread, &pth);
            }
        }
    }

    fn touch_thread(&mut self, event: &'static str) {
        self.proj.thread.updated_at = now();
        let id = self.proj.thread.id.clone();
        self.proj.touch(Kind::Thread, &id, event);
    }

    // -----------------------------------------------------------------------
    // Thread metadata (D4–D9)
    // -----------------------------------------------------------------------

    pub fn archive(&mut self) {
        self.proj.thread.archived_at = Some(now());
        self.touch_thread("thread.archived");
    }

    pub fn unarchive(&mut self) {
        self.proj.thread.archived_at = None;
        self.touch_thread("thread.unarchived");
    }

    pub fn delete(&mut self) {
        self.proj.thread.deleted_at = Some(now());
        self.touch_thread("thread.deleted");
    }

    pub fn rename(&mut self, title: &str) -> Result<(), Reject> {
        let title = title.trim();
        if title.is_empty() {
            return Err(Reject::new("The title can't be empty."));
        }
        if self.proj.thread.title != title {
            self.proj.thread.title = title.to_owned();
            self.touch_thread("thread.metadata-updated");
        }
        Ok(())
    }

    pub fn set_runtime_mode(&mut self, mode: RuntimeMode) {
        if self.proj.thread.runtime_mode != mode {
            self.proj.thread.runtime_mode = mode;
            self.touch_thread("thread.runtime-mode-updated");
        }
    }

    pub fn set_interaction_mode(&mut self, mode: ProviderInteractionMode) {
        if self.proj.thread.interaction_mode != mode {
            self.proj.thread.interaction_mode = mode;
            self.touch_thread("thread.interaction-mode-updated");
        }
    }

    /// Records the effective model selection of the thread.
    pub fn set_model_selection(&mut self, selection: &ModelSelection) {
        if self.proj.thread.model_selection != *selection {
            self.proj.thread.model_selection = selection.clone();
            self.touch_thread("thread.model-selection-updated");
            let ps = self.ps();
            if let Some(session) = self.proj.sessions.iter_mut().find(|s| s.id == ps)
                && session.model.as_deref() != Some(selection.model.as_str())
            {
                session.model = Some(selection.model.clone());
                session.updated_at = now();
                self.proj.touch_updated(Kind::Session, &ps);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Runs and the queue (D1, D12–D16)
    // -----------------------------------------------------------------------

    /// Resolves the effective delivery of `message.dispatch`
    /// (ios-surface §3.4; `S/orchestration-v2/CommandPolicy.ts:92-182`).
    pub fn resolve_delivery(
        &self,
        mode: &t3::commands::DispatchMode,
        intent: Option<t3::commands::DeliveryIntent>,
    ) -> Delivery {
        use t3::commands::{DeliveryIntent, DispatchMode};
        let Some(active) = self.proj.active_run() else {
            return Delivery::Start;
        };
        let run_id = active.id.clone();
        let steerable = matches!(active.status, RunStatus::Running | RunStatus::Waiting)
            && self.has_running_turn();
        match intent {
            Some(DeliveryIntent::Steer) => Delivery::Steer { run_id },
            Some(DeliveryIntent::Restart) => Delivery::Restart { run_id },
            Some(DeliveryIntent::Auto) => {
                if steerable {
                    Delivery::Steer { run_id }
                } else {
                    Delivery::Queue
                }
            }
            None => match mode {
                DispatchMode::SteerActive { .. } => Delivery::Steer { run_id },
                DispatchMode::RestartActive { .. } => Delivery::Restart { run_id },
                DispatchMode::QueueAfterActive
                | DispatchMode::StartImmediately
                | DispatchMode::DeferStart { .. } => Delivery::Queue,
            },
        }
    }

    fn user_message(&mut self, input: &UserInput, run_id: &str, node_id: &str) {
        let at = now();
        let message = ConversationMessage {
            notification: None,
            created_by: input.created_by,
            creation_source: input.creation_source,
            scheduled_task_id: None,
            sender_thread_id: None,
            idle_compaction: None,
            workflow_script: None,
            id: input.message_id.clone(),
            thread_id: self.proj.thread.id.clone(),
            run_id: Some(run_id.to_owned()),
            node_id: Some(node_id.to_owned()),
            role: MessageRole::User,
            text: input.text.clone(),
            context: input.context.clone(),
            attachments: input.attachments.clone(),
            streaming: false,
            created_at: at.clone(),
            updated_at: at,
            delegated_completion: None,
        };
        if let Some(existing) = self.proj.message_mut(&input.message_id) {
            *existing = message;
        } else {
            self.proj.messages.push(message);
        }
        self.proj.touch_updated(Kind::Message, &input.message_id);
    }

    fn user_item(input: &UserInput, intent: UserMessageInputIntent) -> TurnItemKind {
        TurnItemKind::UserMessage(UserMessageItem {
            created_by: input.created_by,
            creation_source: input.creation_source,
            message_id: input.message_id.clone(),
            scheduled_task_id: None,
            sender_thread_id: None,
            idle_compaction: None,
            workflow_script: None,
            input_intent: intent,
            text: input.text.clone(),
            text_omitted: None,
            context: input.context.clone(),
            attachments: input.attachments.clone(),
        })
    }

    fn new_run(&mut self, input: &UserInput, status: RunStatus) -> (String, String) {
        let ordinal = self
            .proj
            .runs
            .iter()
            .map(|run| run.ordinal)
            .max()
            .unwrap_or(0)
            + 1;
        let run_id = format!("run_{ordinal}");
        let node_id = format!("node_{ordinal}");
        self.proj.runs.push(Run {
            id: run_id.clone(),
            thread_id: self.proj.thread.id.clone(),
            ordinal,
            provider_instance_id: PROVIDER_INSTANCE.to_owned(),
            model_selection: input.model_selection.clone(),
            provider_thread_id: Some(self.pth()),
            user_message_id: input.message_id.clone(),
            root_node_id: Some(node_id.clone()),
            active_attempt_id: None,
            status,
            queue_position: None,
            queue_held: None,
            requested_at: now(),
            started_at: None,
            completed_at: None,
            checkpoint_id: None,
            context_handoff_id: None,
            restart_continuation_of_run_id: None,
            delegated_task_id: None,
            work_started_at: None,
            restart_cancelled_background_work: None,
            source_plan_ref: None,
            delegated_completion: None,
            workspace_preparation: None,
        });
        self.proj.touch_created(Kind::Run, &run_id);
        (run_id, node_id)
    }

    /// Starts a run for `input` (status `starting`); the actor then prompts omp.
    pub fn start_run(&mut self, input: &UserInput) -> String {
        self.set_model_selection(&input.model_selection);
        let (run_id, node_id) = self.new_run(input, RunStatus::Starting);
        self.user_message(input, &run_id, &node_id);
        self.push_item(
            Some(run_id.clone()),
            Some(node_id.clone()),
            TurnItemStatus::Completed,
            None,
            Self::user_item(input, UserMessageInputIntent::TurnStart),
        );
        self.live = Some(Live::new(run_id.clone(), node_id));
        run_id
    }

    /// Queues `input` behind the active run.
    pub fn queue_run(&mut self, input: &UserInput) -> String {
        let held = self
            .proj
            .queued_runs()
            .iter()
            .any(|run| run.queue_held == Some(true));
        let (run_id, node_id) = self.new_run(input, RunStatus::Queued);
        if let Some(run) = self.proj.run_mut(&run_id) {
            run.queue_held = Some(held);
        }
        self.user_message(input, &run_id, &node_id);
        self.push_item(
            Some(run_id.clone()),
            Some(node_id),
            TurnItemStatus::Pending,
            None,
            Self::user_item(input, UserMessageInputIntent::QueuedTurn),
        );
        self.renumber_queue();
        run_id
    }

    /// Moves `run_id` to the front of the queue.
    pub fn queue_first(&mut self, run_id: &str) {
        if let Some(run) = self.proj.run_mut(run_id) {
            run.queue_position = Some(Some(0));
        }
        self.renumber_queue();
    }

    fn renumber_queue(&mut self) {
        let order: Vec<String> = self
            .proj
            .queued_runs()
            .into_iter()
            .map(|run| run.id.clone())
            .collect();
        for (index, id) in order.iter().enumerate() {
            let position = Some(Some(index as u64 + 1));
            if self
                .proj
                .run(id)
                .is_some_and(|run| run.queue_position != position)
            {
                self.update_run(id, |run| run.queue_position = position);
            }
        }
    }

    /// The first queued run that may start now.
    pub fn next_startable(&self) -> Option<String> {
        if self.proj.active_run().is_some() {
            return None;
        }
        self.proj
            .queued_runs()
            .first()
            .filter(|run| run.queue_held != Some(true))
            .map(|run| run.id.clone())
    }

    fn input_of(&self, run: &Run) -> Result<UserInput, Reject> {
        let message = self
            .proj
            .messages
            .iter()
            .rev()
            .find(|message| message.id == run.user_message_id)
            .ok_or_else(|| Reject::new("The queued message no longer exists."))?;
        Ok(UserInput {
            message_id: message.id.clone(),
            text: message.text.clone(),
            attachments: message.attachments.clone(),
            context: message.context.clone(),
            model_selection: run.model_selection.clone(),
            created_by: message.created_by,
            creation_source: message.creation_source,
        })
    }

    fn queued(&self, run_id: &str) -> Result<&Run, Reject> {
        self.proj
            .run(run_id)
            .filter(|run| run.status == RunStatus::Queued)
            .ok_or_else(|| Reject::new("That message is no longer queued."))
    }

    fn queued_item_id(&self, run_id: &str) -> Option<String> {
        self.proj
            .items
            .iter()
            .find(|item| {
                item.run_id.as_deref() == Some(run_id)
                    && matches!(&item.kind, TurnItemKind::UserMessage(m)
                        if m.input_intent == UserMessageInputIntent::QueuedTurn)
            })
            .map(|item| item.id.clone())
    }

    /// Starts queued run `run_id` (`queued` → `starting`). Returns what to
    /// send to omp.
    pub fn begin_queued(&mut self, run_id: &str) -> Result<UserInput, Reject> {
        let input = self.input_of(self.queued(run_id)?)?;
        self.set_model_selection(&input.model_selection);
        let node_id = self
            .proj
            .run(run_id)
            .and_then(|run| run.root_node_id.clone())
            .unwrap_or_default();
        self.update_run(run_id, |run| {
            run.status = RunStatus::Starting;
            run.queue_position = None;
            run.queue_held = None;
        });
        if let Some(item_id) = self.queued_item_id(run_id) {
            self.update_item(&item_id, false, |item| {
                item.status = TurnItemStatus::Completed
            });
        }
        self.renumber_queue();
        self.live = Some(Live::new(run_id.to_owned(), node_id));
        Ok(input)
    }

    /// D12: cancels a queued run; its feed item is hidden.
    pub fn cancel_queued(&mut self, run_id: &str) -> Result<(), Reject> {
        self.queued(run_id)?;
        let at = now();
        self.update_run(run_id, |run| {
            run.status = RunStatus::Cancelled;
            run.queue_position = None;
            run.queue_held = None;
            run.completed_at = Some(at);
        });
        if let Some(item_id) = self.queued_item_id(run_id) {
            self.update_item(&item_id, false, |item| {
                item.status = TurnItemStatus::Cancelled
            });
        }
        self.renumber_queue();
        Ok(())
    }

    /// D13: edits a queued message. `attachments: None` keeps the old ones.
    pub fn edit_queued(
        &mut self,
        run_id: &str,
        text: &str,
        attachments: Option<Vec<ChatAttachment>>,
        context: Option<MessageContext>,
    ) -> Result<(), Reject> {
        let message_id = self.queued(run_id)?.user_message_id.clone();
        let at = now();
        if let Some(message) = self.proj.message_mut(&message_id) {
            message.text = text.to_owned();
            if let Some(attachments) = &attachments {
                message.attachments.clone_from(attachments);
            }
            if context.is_some() {
                message.context.clone_from(&context);
            }
            message.updated_at = at;
            self.proj.touch_updated(Kind::Message, &message_id);
        }
        if let Some(item_id) = self.queued_item_id(run_id) {
            self.update_item(&item_id, false, |item| {
                if let TurnItemKind::UserMessage(message) = &mut item.kind {
                    message.text = text.to_owned();
                    if let Some(attachments) = attachments {
                        message.attachments = attachments;
                    }
                    if context.is_some() {
                        message.context = context;
                    }
                }
            });
        }
        Ok(())
    }

    /// `run.interrupt{holdQueue:true}`: every queued run waits for Resume.
    pub fn hold_queue(&mut self) {
        let ids: Vec<String> = self
            .proj
            .queued_runs()
            .into_iter()
            .filter(|run| run.queue_held != Some(true))
            .map(|run| run.id.clone())
            .collect();
        for id in ids {
            self.update_run(&id, |run| run.queue_held = Some(true));
        }
    }

    /// D15 `queue.resume`.
    pub fn resume_queue(&mut self) {
        let ids: Vec<String> = self
            .proj
            .queued_runs()
            .into_iter()
            .filter(|run| run.queue_held == Some(true))
            .map(|run| run.id.clone())
            .collect();
        for id in ids {
            self.update_run(&id, |run| run.queue_held = Some(false));
        }
    }

    fn require_steerable(&self, run_id: &str) -> Result<(String, String), Reject> {
        let active = self
            .proj
            .active_run()
            .filter(|run| run.id == run_id)
            .ok_or_else(|| Reject::new("That run is no longer active."))?;
        if !self.has_running_turn() {
            return Err(Reject::new(
                "The agent hasn't started yet; queue the message instead.",
            ));
        }
        Ok((
            active.id.clone(),
            active.root_node_id.clone().unwrap_or_default(),
        ))
    }

    /// Steers `input` into active run `run_id`.
    pub fn steer(&mut self, input: &UserInput, run_id: &str) -> Result<(), Reject> {
        let (run_id, node_id) = self.require_steerable(run_id)?;
        self.user_message(input, &run_id, &node_id);
        self.push_item(
            Some(run_id),
            Some(node_id),
            TurnItemStatus::Completed,
            None,
            Self::user_item(input, UserMessageInputIntent::Steer),
        );
        Ok(())
    }

    /// D14: turns queued run `queued_run_id` into a steer of `target_run_id`.
    pub fn promote(
        &mut self,
        queued_run_id: &str,
        target_run_id: &str,
    ) -> Result<UserInput, Reject> {
        let input = self.input_of(self.queued(queued_run_id)?)?;
        let (run_id, node_id) = self.require_steerable(target_run_id)?;
        self.cancel_queued(queued_run_id)?;
        self.user_message(&input, &run_id, &node_id);
        self.push_item(
            Some(run_id),
            Some(node_id),
            TurnItemStatus::Completed,
            None,
            Self::user_item(&input, UserMessageInputIntent::PromotedQueuedToSteer),
        );
        Ok(input)
    }

    /// Stop requested: records the request row. Returns the active run.
    pub fn request_interrupt(&mut self, reason: Option<&str>) -> Option<String> {
        let run = self.proj.active_run()?;
        let run_id = run.id.clone();
        let node_id = run.root_node_id.clone();
        if let Some(live) = self.live.as_mut().filter(|live| live.run_id == run_id) {
            live.interrupt_requested = true;
        }
        let message = non_blank(reason.unwrap_or_default(), "Stop requested");
        self.push_item(
            Some(run_id.clone()),
            node_id,
            TurnItemStatus::Completed,
            None,
            TurnItemKind::RunInterruptRequest(RunInterruptRequestItem {
                message,
                unresponsive_at: None,
            }),
        );
        Some(run_id)
    }

    /// The prompt was refused or omp failed before running: the run fails.
    pub fn fail_run(&mut self, message: &str) {
        if let Some(live) = self.live.as_mut() {
            live.status = Some(PromptStatus::Error);
            live.error = Some(PromptError {
                message: message.to_owned(),
                retryable: false,
                provider: None,
                model: None,
                http_status: None,
                code: None,
            });
        }
        self.finish_run();
    }

    /// omp handled the prompt locally (`agentInvoked:false`).
    pub fn complete_without_agent(&mut self) {
        if let Some(live) = self.live.as_mut() {
            live.status = Some(PromptStatus::Completed);
        }
        self.finish_run();
    }

    /// The omp process is gone (force stop or crash) mid-run.
    pub fn abandon_run(&mut self, interrupted: bool) {
        if let Some(live) = self.live.as_mut() {
            live.status = Some(if interrupted {
                PromptStatus::Aborted
            } else {
                PromptStatus::Error
            });
            if !interrupted && live.error.is_none() {
                live.error = Some(PromptError {
                    message: "The agent process exited unexpectedly.".into(),
                    retryable: true,
                    provider: None,
                    model: None,
                    http_status: None,
                    code: None,
                });
            }
            live.interrupt_requested |= interrupted;
        }
        self.finish_run();
        for (request_id, _) in std::mem::take(&mut self.ui) {
            self.close_request(&request_id, RuntimeRequestStatus::Expired);
        }
    }

    /// Server restart: nothing survives in omp. Active runs are interrupted,
    /// pending requests expire and queued runs wait for Resume.
    pub fn recover(&mut self) {
        let at = now();
        let active: Vec<String> = self
            .proj
            .runs
            .iter()
            .filter(|run| run.status.is_active())
            .map(|run| run.id.clone())
            .collect();
        for run_id in &active {
            self.update_run(run_id, |run| {
                run.status = RunStatus::Interrupted;
                run.completed_at = Some(at.clone());
            });
        }
        let attempts: Vec<String> = self
            .proj
            .attempts
            .iter()
            .filter(|attempt| {
                matches!(
                    attempt.status,
                    RunAttemptStatus::Pending | RunAttemptStatus::Running
                )
            })
            .map(|attempt| attempt.id.clone())
            .collect();
        for id in attempts {
            if let Some(attempt) = self.proj.attempts.iter_mut().find(|a| a.id == id) {
                attempt.status = RunAttemptStatus::Interrupted;
                attempt.completed_at = Some(at.clone());
                self.proj.touch_updated(Kind::Attempt, &id);
            }
        }
        let turns: Vec<String> = self
            .proj
            .provider_turns
            .iter()
            .filter(|turn| {
                matches!(
                    turn.status,
                    ProviderTurnStatus::Pending | ProviderTurnStatus::Running
                )
            })
            .map(|turn| turn.id.clone())
            .collect();
        for id in turns {
            if let Some(turn) = self.proj.provider_turns.iter_mut().find(|t| t.id == id) {
                turn.status = ProviderTurnStatus::Interrupted;
                turn.completed_at = Some(at.clone());
                self.proj.touch_updated(Kind::ProviderTurn, &id);
            }
        }
        let pending: Vec<String> = self
            .proj
            .requests
            .iter()
            .filter(|request| request.status == RuntimeRequestStatus::Pending)
            .map(|request| request.id.clone())
            .collect();
        for id in pending {
            self.close_request(&id, RuntimeRequestStatus::Expired);
        }
        for run_id in &active {
            self.close_open_items(run_id, TurnItemStatus::Interrupted);
        }
        self.hold_queue();
        self.live = None;
        self.ui.clear();
        self.session_closed();
        self.set_session_status(ProviderSessionStatus::Stopped, None);
    }

    /// Something in the projection is left over from a previous process.
    pub fn needs_recovery(&self) -> bool {
        self.proj.runs.iter().any(|run| run.status.is_active())
            || self
                .proj
                .requests
                .iter()
                .any(|request| request.status == RuntimeRequestStatus::Pending)
            || self
                .proj
                .sessions
                .iter()
                .any(|session| !matches!(session.status, ProviderSessionStatus::Stopped))
            || self.has_live_agent_state()
    }

    // -----------------------------------------------------------------------
    // Runtime requests (D10, D11)
    // -----------------------------------------------------------------------

    /// D10: answers a pending question. Returns the omp response to send.
    pub fn answer(
        &mut self,
        request_id: &str,
        answers: &ProviderUserInputAnswers,
    ) -> Result<ExtensionUiResponse, Reject> {
        let pending = self
            .ui
            .get(request_id)
            .ok_or_else(|| Reject::new("This question is no longer waiting for an answer."))?
            .clone();
        let response = build_ui_response(&pending, answers);
        self.ui.remove(request_id);
        let at = now();
        if let Some(request) = self.proj.request_mut(request_id) {
            request.status = RuntimeRequestStatus::Resolved;
            request.resolved_at = Some(at);
            request.answers = Some(answers.clone());
        }
        self.proj.touch_updated(Kind::Request, request_id);
        self.update_item(&pending.item_id, false, |item| {
            item.status = TurnItemStatus::Completed;
            if let TurnItemKind::UserInputRequest(request) = &mut item.kind {
                let question_text_by_id = request
                    .questions
                    .iter()
                    .map(|question| (question.id.clone(), question.question.clone()))
                    .collect();
                request.question_answer = Some(UserInputAnswerPayload {
                    request_id: request_id.to_owned(),
                    question_text_by_id: Some(question_text_by_id),
                    answers: answers.clone(),
                    attachments_by_question_id: BTreeMap::new(),
                });
            }
        });
        self.after_request_closed();
        Ok(response)
    }

    /// D11: dismisses a pending question. Returns the omp cancellation.
    pub fn dismiss(&mut self, request_id: &str) -> Result<ExtensionUiResponse, Reject> {
        let pending = self
            .ui
            .remove(request_id)
            .ok_or_else(|| Reject::new("This question is no longer waiting for an answer."))?;
        self.close_request(request_id, RuntimeRequestStatus::Cancelled);
        self.after_request_closed();
        Ok(ExtensionUiResponse::CancelUiResponse(CancelUiResponse {
            id: pending.omp_id,
            cancelled: LitTrue,
            timed_out: None,
        }))
    }

    /// A request's timeout elapsed. Returns the omp cancellation, if it was
    /// still pending.
    pub fn expire(&mut self, request_id: &str) -> Option<ExtensionUiResponse> {
        let pending = self.ui.remove(request_id)?;
        self.close_request(request_id, RuntimeRequestStatus::Expired);
        self.after_request_closed();
        Some(ExtensionUiResponse::CancelUiResponse(CancelUiResponse {
            id: pending.omp_id,
            cancelled: LitTrue,
            timed_out: Some(true),
        }))
    }

    fn close_request(&mut self, request_id: &str, status: RuntimeRequestStatus) {
        let at = now();
        if let Some(request) = self
            .proj
            .request_mut(request_id)
            .filter(|request| request.status == RuntimeRequestStatus::Pending)
        {
            request.status = status;
            request.resolved_at = Some(at);
            self.proj.touch_updated(Kind::Request, request_id);
        }
        let item_id = self
            .proj
            .items
            .iter()
            .rev()
            .find_map(|item| match &item.kind {
                TurnItemKind::UserInputRequest(request)
                    if request.request_id == request_id && !is_terminal(item.status) =>
                {
                    Some(item.id.clone())
                }
                _ => None,
            });
        if let Some(item_id) = item_id {
            self.update_item(&item_id, false, |item| {
                item.status = TurnItemStatus::Cancelled
            });
        }
    }

    /// Back to `running` once no question is pending.
    fn after_request_closed(&mut self) {
        if !self.ui.is_empty() {
            return;
        }
        if let Some(run_id) = self.live.as_ref().map(|live| live.run_id.clone())
            && self
                .proj
                .run(&run_id)
                .is_some_and(|run| run.status == RunStatus::Waiting)
        {
            self.update_run(&run_id, |run| run.status = RunStatus::Running);
            self.set_session_status_inner(ProviderSessionStatus::Running);
        }
    }

    // -----------------------------------------------------------------------
    // omp events
    // -----------------------------------------------------------------------

    /// The live run, creating an implicit one when omp works without a
    /// prompt of ours (e.g. a background job woke the agent).
    fn ensure_live(&mut self) -> (String, String) {
        if let Some(live) = &self.live {
            return (live.run_id.clone(), live.node_id.clone());
        }
        let input = UserInput {
            message_id: String::new(),
            text: String::new(),
            attachments: Vec::new(),
            context: None,
            model_selection: self.proj.thread.model_selection.clone(),
            created_by: Actor::Agent,
            creation_source: CreationSource::Provider,
        };
        let (run_id, node_id) = self.new_run(&input, RunStatus::Starting);
        let message_id = format!("msg_{run_id}");
        if let Some(run) = self.proj.run_mut(&run_id) {
            run.user_message_id = message_id;
        }
        self.live = Some(Live::new(run_id.clone(), node_id.clone()));
        (run_id, node_id)
    }

    /// `agent_start`: the attempt and provider turn start running.
    fn begin_attempt(&mut self) {
        let (run_id, node_id) = self.ensure_live();
        if self
            .live
            .as_ref()
            .is_some_and(|live| live.attempt_id.is_some())
        {
            return;
        }
        let ordinal = self.proj.run(&run_id).map_or(1, |run| run.ordinal);
        let attempt_id = format!("att_{ordinal}");
        let turn_id = format!("ptu_{ordinal}");
        let at = now();
        let pth = self.pth();
        self.proj.attempts.push(RunAttempt {
            id: attempt_id.clone(),
            native_thread_id: None,
            run_id: run_id.clone(),
            attempt_ordinal: 1,
            root_node_id: node_id.clone(),
            provider_instance_id: PROVIDER_INSTANCE.to_owned(),
            provider_thread_id: pth.clone(),
            provider_turn_id: Some(turn_id.clone()),
            reason: RunAttemptReason::Initial,
            status: RunAttemptStatus::Running,
            started_at: Some(at.clone()),
            completed_at: None,
        });
        self.proj.touch_created(Kind::Attempt, &attempt_id);
        let started = at.clone();
        let attempt = attempt_id.clone();
        self.update_run(&run_id, |run| {
            run.status = RunStatus::Running;
            run.active_attempt_id = Some(attempt);
            run.started_at.get_or_insert(started);
        });
        self.proj.provider_turns.push(ProviderTurn {
            id: turn_id.clone(),
            provider_thread_id: pth,
            node_id,
            run_attempt_id: Some(attempt_id.clone()),
            native_turn_ref: None,
            ordinal,
            status: ProviderTurnStatus::Running,
            started_at: Some(at),
            completed_at: None,
            token_usage: None,
            turn_token_usage: None,
        });
        self.proj.touch_updated(Kind::ProviderTurn, &turn_id);
        if let Some(live) = self.live.as_mut() {
            live.attempt_id = Some(attempt_id);
            live.turn_id = Some(turn_id);
        }
        self.set_session_status_inner(ProviderSessionStatus::Running);
        self.set_provider_thread_status(ProviderThreadStatus::Active, Some(ordinal));
    }

    /// Items of `run_id` still open are closed with `status`.
    fn close_open_items(&mut self, run_id: &str, status: TurnItemStatus) {
        let open: Vec<String> = self
            .proj
            .items
            .iter()
            .filter(|item| item.run_id.as_deref() == Some(run_id))
            // Subagents end with their own lifecycle, not with the run.
            .filter(|item| !matches!(item.kind, TurnItemKind::Subagent(_)))
            .filter(|item| {
                matches!(
                    item.status,
                    TurnItemStatus::Running | TurnItemStatus::Waiting | TurnItemStatus::Idle
                )
            })
            .map(|item| item.id.clone())
            .collect();
        for id in open {
            let streaming_status = if status == TurnItemStatus::Failed {
                TurnItemStatus::Interrupted
            } else {
                status
            };
            self.update_item(&id, false, |item| match &mut item.kind {
                TurnItemKind::AssistantMessage(message) => {
                    message.streaming = false;
                    item.status = TurnItemStatus::Completed;
                }
                TurnItemKind::Reasoning(reasoning) => {
                    reasoning.streaming = false;
                    item.status = TurnItemStatus::Completed;
                }
                _ => item.status = streaming_status,
            });
            self.sync_assistant_message(&id, false);
        }
    }

    /// Ends the live run per the last `prompt_result` (ios-surface §5 "Run
    /// lifecycle"). Returns whether a run ended.
    fn finish_run(&mut self) -> bool {
        let Some(live) = self.live.take() else {
            return false;
        };
        let at = now();
        let status = match live.status {
            Some(PromptStatus::Error) => RunStatus::Failed,
            Some(PromptStatus::Aborted) => RunStatus::Interrupted,
            _ if live.interrupt_requested => RunStatus::Interrupted,
            _ => RunStatus::Completed,
        };
        let item_status = match status {
            RunStatus::Failed => TurnItemStatus::Failed,
            RunStatus::Interrupted => TurnItemStatus::Interrupted,
            _ => TurnItemStatus::Completed,
        };
        self.close_open_items(&live.run_id, item_status);
        // Requests can't be answered once the run is over.
        for (request_id, _) in std::mem::take(&mut self.ui) {
            self.close_request(&request_id, RuntimeRequestStatus::Expired);
        }
        let node = Some(live.node_id.clone());
        if let (RunStatus::Failed, Some(error)) = (status, &live.error) {
            let failure = failure_of(error);
            self.push_item(
                Some(live.run_id.clone()),
                node.clone(),
                TurnItemStatus::Completed,
                None,
                TurnItemKind::Error(ErrorItem {
                    failure,
                    retry: None,
                }),
            );
        }
        if status == RunStatus::Interrupted && live.interrupt_requested {
            self.push_item(
                Some(live.run_id.clone()),
                node,
                TurnItemStatus::Completed,
                None,
                TurnItemKind::RunInterruptResult(MessageItem {
                    message: "Stopped".into(),
                }),
            );
        }
        let (turn_status, attempt_status) = match status {
            RunStatus::Failed => (ProviderTurnStatus::Failed, RunAttemptStatus::Failed),
            RunStatus::Interrupted => (
                ProviderTurnStatus::Interrupted,
                RunAttemptStatus::Interrupted,
            ),
            _ => (ProviderTurnStatus::Completed, RunAttemptStatus::Completed),
        };
        if let Some(turn_id) = &live.turn_id
            && let Some(turn) = self
                .proj
                .provider_turns
                .iter_mut()
                .find(|t| &t.id == turn_id)
        {
            turn.status = turn_status;
            turn.completed_at = Some(at.clone());
            self.proj.touch_updated(Kind::ProviderTurn, turn_id);
        }
        if let Some(attempt_id) = &live.attempt_id
            && let Some(attempt) = self.proj.attempts.iter_mut().find(|a| &a.id == attempt_id)
        {
            attempt.status = attempt_status;
            attempt.completed_at = Some(at.clone());
            self.proj.touch_updated(Kind::Attempt, attempt_id);
        }
        self.update_run(&live.run_id, |run| {
            run.status = status;
            run.completed_at = Some(at);
        });
        self.set_session_status_inner(ProviderSessionStatus::Ready);
        self.set_provider_thread_status(ProviderThreadStatus::Idle, None);
        true
    }

    /// Feeds one omp frame.
    pub fn apply(&mut self, event: &Event) -> Vec<Effect> {
        let Event::Notification(notification) = event else {
            return Vec::new();
        };
        let mut effects = Vec::new();
        match notification {
            RpcNotification::PromptResult(result) => {
                if let Some(live) = self.live.as_mut() {
                    live.status = Some(result.status);
                    live.error.clone_from(&result.error);
                    if !result.agent_invoked && live.attempt_id.is_none() {
                        live.status.get_or_insert(PromptStatus::Completed);
                    }
                }
                if (result.session_settled || !result.agent_invoked) && self.finish_run() {
                    effects.push(Effect::RunSettled);
                }
            }
            RpcNotification::SessionSettled(_) => {
                // A run omp has not picked up yet (no agent_start, no
                // prompt_result) is not what settled: this is the trailing
                // settle of the previous run, which already ended on its
                // `prompt_result{sessionSettled:true}`.
                let picked_up = self
                    .live
                    .as_ref()
                    .is_some_and(|live| live.attempt_id.is_some() || live.status.is_some());
                if picked_up && self.finish_run() {
                    effects.push(Effect::RunSettled);
                }
            }
            RpcNotification::ExtensionUiRequest(request) => {
                if let Some(effect) = self.apply_ui_request(request) {
                    effects.push(effect);
                }
            }
            RpcNotification::CommandOutput(output) => {
                if !output.text.trim().is_empty() {
                    self.push_live_item(
                        TurnItemStatus::Completed,
                        None,
                        TurnItemKind::SystemNotice(MessageItem {
                            message: output.text.clone(),
                        }),
                    );
                }
            }
            RpcNotification::RpcAgentEvent(event) => self.apply_agent_event(event, &mut effects),
            RpcNotification::SubagentLifecycle(lifecycle) => {
                self.apply_subagent_lifecycle(&lifecycle.payload, &mut effects);
            }
            RpcNotification::SubagentProgress(progress) => {
                self.apply_subagent_progress(&progress.payload);
            }
            _ => {}
        }
        effects
    }

    fn apply_agent_event(&mut self, event: &RpcAgentEvent, effects: &mut Vec<Effect>) {
        match event {
            RpcAgentEvent::AgentStart(_) => self.begin_attempt(),
            RpcAgentEvent::AgentEnd(_) => effects.push(Effect::RefreshState),
            RpcAgentEvent::MessageUpdate(update) => {
                if matches!(update.message, AgentMessage::Assistant(_)) {
                    let message_id = update.message_id.clone().unwrap_or_default();
                    self.apply_assistant_event(&message_id, &update.assistant_message_event);
                }
            }
            RpcAgentEvent::MessageEnd(end) => {
                if let AgentMessage::Assistant(message) = &end.message {
                    let message_id = end.message_id.clone().unwrap_or_default();
                    let content = message.content.as_deref().unwrap_or_default();
                    for (index, block) in content.iter().enumerate() {
                        self.apply_final_block(&message_id, index as i64, block);
                    }
                    self.close_message_blocks(&message_id);
                }
            }
            RpcAgentEvent::ToolExecutionStart(start) => {
                let args = start.args.clone().unwrap_or(Value::Object(Map::new()));
                if let Some(item_id) = self.ensure_tool(
                    &start.tool_call_id,
                    &start.tool_name,
                    &args,
                    start.intent.as_deref(),
                ) {
                    // Execution args are final (the streamed call may still
                    // carry host-only keys); the intent names the row.
                    let intent = start
                        .intent
                        .clone()
                        .filter(|intent| !intent.trim().is_empty());
                    let kind = start
                        .args
                        .as_ref()
                        .map(|args| tool_kind(&start.tool_name, args));
                    self.update_item(&item_id, false, |item| {
                        item.status = TurnItemStatus::Running;
                        if let Some(intent) = intent {
                            item.title = Some(intent);
                        }
                        if let Some(kind) = kind {
                            item.kind = kind;
                        }
                    });
                }
            }
            RpcAgentEvent::ToolExecutionUpdate(update) => {
                let item_id = self
                    .live
                    .as_ref()
                    .and_then(|live| live.tools.get(&update.tool_call_id).cloned().flatten());
                if let (Some(item_id), Some(text)) = (
                    item_id,
                    update.partial_result.as_ref().and_then(result_text),
                ) {
                    self.update_item(&item_id, true, |item| set_tool_output(item, text, None));
                }
            }
            RpcAgentEvent::ToolExecutionEnd(end) => {
                let item_id = match self
                    .live
                    .as_ref()
                    .and_then(|live| live.tools.get(&end.tool_call_id).cloned())
                {
                    Some(item_id) => item_id,
                    None => self.ensure_tool(
                        &end.tool_call_id,
                        &end.tool_name,
                        &Value::Object(Map::new()),
                        None,
                    ),
                };
                if let Some(item_id) = item_id {
                    let failed = end.is_error == Some(true);
                    let text = end.result.as_ref().and_then(result_text);
                    let images = end.result.as_ref().map(result_images).unwrap_or_default();
                    let details = end
                        .result
                        .as_ref()
                        .and_then(|result| result.get("details"))
                        .cloned();
                    let mut attached = false;
                    self.update_item(&item_id, false, |item| {
                        item.status = if failed {
                            TurnItemStatus::Failed
                        } else {
                            TurnItemStatus::Completed
                        };
                        if let Some(text) = text {
                            set_tool_output(item, text, details.as_ref());
                        }
                        if !images.is_empty() {
                            attached = attach_tool_images(item, &images);
                        }
                    });
                    if attached {
                        effects.push(Effect::StoreToolImages(images));
                    }
                }
                if end.tool_name == TODO_TOOL {
                    effects.push(Effect::RefreshState);
                }
            }
            RpcAgentEvent::AutoCompactionStart(_) | RpcAgentEvent::ManualCompactionStart(_) => {
                let item_id = self.push_live_item(
                    TurnItemStatus::Running,
                    Some("Compacting context".into()),
                    TurnItemKind::Compaction(CompactionItem {
                        driver: None,
                        summary: None,
                        before_token_count: None,
                        after_token_count: None,
                    }),
                );
                if let Some(live) = self.live.as_mut() {
                    live.compaction = Some(item_id);
                }
            }
            RpcAgentEvent::AutoCompactionEnd(end) => {
                self.end_compaction(
                    end.aborted,
                    end.result
                        .as_ref()
                        .map(|r| (r.summary.clone(), r.tokens_before)),
                    end.error_message.is_some() && end.skipped != Some(true),
                );
            }
            RpcAgentEvent::ManualCompactionEnd(end) => {
                self.end_compaction(
                    end.aborted,
                    end.result
                        .as_ref()
                        .map(|r| (r.summary.clone(), r.tokens_before)),
                    end.error_message.is_some() && end.skipped != Some(true),
                );
            }
            RpcAgentEvent::AutoRetryStart(retry) => {
                self.push_live_item(
                    TurnItemStatus::Completed,
                    None,
                    TurnItemKind::Error(ErrorItem {
                        failure: ProviderFailure {
                            class: ProviderFailureClass::ProviderError,
                            message: truncate(
                                &non_blank(&retry.error_message, "The provider request failed."),
                                MAX_FAILURE_MESSAGE,
                            ),
                            code: None,
                            retryable: Some(true),
                            reset_at: None,
                        },
                        retry: Some(ProviderRetry {
                            attempt: retry.attempt.max(1) as u64,
                            max_attempts: (retry.max_attempts > 0)
                                .then_some(retry.max_attempts as u64),
                            retry_delay_ms: Some(retry.delay_ms.max(0) as u64),
                        }),
                    }),
                );
            }
            RpcAgentEvent::Notice(notice) => {
                if !notice.message.trim().is_empty() {
                    self.push_live_item(
                        TurnItemStatus::Completed,
                        None,
                        TurnItemKind::SystemNotice(MessageItem {
                            message: notice.message.clone(),
                        }),
                    );
                }
            }
            RpcAgentEvent::TodoAutoClear(_) => self.apply_todos(&[]),
            RpcAgentEvent::ModelChanged(_) => effects.push(Effect::RefreshState),
            RpcAgentEvent::ThinkingLevelChanged(changed) => {
                // `configured` is set when an automatic level resolved; the
                // user's selection did not change then.
                if changed.configured.is_none()
                    && let Some(level) = changed.thinking_level
                {
                    self.set_thinking_option(level);
                }
            }
            RpcAgentEvent::GoalUpdated(update) => self.apply_goal(update.goal.as_ref()),
            _ => {}
        }
    }

    fn set_thinking_option(&mut self, level: ThinkingLevel) {
        let Ok(Value::String(level)) = serde_json::to_value(level) else {
            return;
        };
        let mut selection = self.proj.thread.model_selection.clone();
        let options = selection.options.get_or_insert_with(Vec::new);
        options.retain(|option| option.id != THINKING_OPTION);
        options.push(ProviderOptionSelection {
            id: THINKING_OPTION.to_owned(),
            value: ProviderOptionSelectionValue::String(level),
        });
        self.set_model_selection(&selection);
    }

    fn end_compaction(&mut self, aborted: bool, result: Option<(String, i64)>, failed: bool) {
        let Some(item_id) = self.live.as_mut().and_then(|live| live.compaction.take()) else {
            return;
        };
        self.update_item(&item_id, false, |item| {
            item.status = if aborted {
                TurnItemStatus::Cancelled
            } else if failed {
                TurnItemStatus::Failed
            } else {
                TurnItemStatus::Completed
            };
            if let TurnItemKind::Compaction(compaction) = &mut item.kind
                && let Some((summary, tokens_before)) = result
            {
                compaction.summary = Some(summary);
                compaction.before_token_count = u64::try_from(tokens_before).ok();
            }
        });
    }

    // -----------------------------------------------------------------------
    // Assistant text, thinking and tool calls
    // -----------------------------------------------------------------------

    fn block_item(&self, message_id: &str, index: i64) -> Option<String> {
        self.live
            .as_ref()
            .and_then(|live| live.blocks.get(&(message_id.to_owned(), index)).cloned())
    }

    /// Creates the item for one text/thinking block.
    fn open_block(&mut self, message_id: &str, index: i64, answer: bool) -> String {
        if let Some(id) = self.block_item(message_id, index) {
            return id;
        }
        let item_id = if answer {
            let ordinal = self.next_ordinal;
            let assistant_message_id = format!("msg_ti_{ordinal}");
            let id = self.push_live_item(
                TurnItemStatus::Running,
                None,
                TurnItemKind::AssistantMessage(AssistantMessageItem {
                    message_id: assistant_message_id,
                    text: String::new(),
                    text_omitted: None,
                    attachments: None,
                    streaming: true,
                }),
            );
            self.sync_assistant_message(&id, false);
            id
        } else {
            self.push_live_item(
                TurnItemStatus::Running,
                None,
                TurnItemKind::Reasoning(ReasoningItem {
                    text: String::new(),
                    streaming: true,
                }),
            )
        };
        if let Some(live) = self.live.as_mut() {
            live.blocks
                .insert((message_id.to_owned(), index), item_id.clone());
        }
        item_id
    }

    /// Mirrors an `assistant_message` item into its conversation message.
    fn sync_assistant_message(&mut self, item_id: &str, streaming_touch: bool) {
        let Some(item) = self.proj.item(item_id) else {
            return;
        };
        let TurnItemKind::AssistantMessage(body) = &item.kind else {
            return;
        };
        let message = ConversationMessage {
            notification: None,
            created_by: Actor::Agent,
            creation_source: CreationSource::Provider,
            scheduled_task_id: None,
            sender_thread_id: None,
            idle_compaction: None,
            workflow_script: None,
            id: body.message_id.clone(),
            thread_id: item.thread_id.clone(),
            run_id: item.run_id.clone(),
            node_id: item.node_id.clone(),
            role: MessageRole::Assistant,
            text: body.text.clone(),
            context: None,
            attachments: Vec::new(),
            streaming: body.streaming,
            created_at: item
                .started_at
                .clone()
                .unwrap_or_else(|| item.updated_at.clone()),
            updated_at: item.updated_at.clone(),
            delegated_completion: None,
        };
        let id = message.id.clone();
        match self.proj.message_mut(&id) {
            Some(existing) => *existing = message,
            None => self.proj.messages.push(message),
        }
        if streaming_touch {
            self.proj.touch_streaming(Kind::Message, &id);
        } else {
            self.proj.touch_updated(Kind::Message, &id);
        }
    }

    fn append_block(&mut self, item_id: &str, delta: &str) {
        self.update_item(item_id, true, |item| match &mut item.kind {
            TurnItemKind::AssistantMessage(message) => message.text.push_str(delta),
            TurnItemKind::Reasoning(reasoning) => reasoning.text.push_str(delta),
            _ => {}
        });
        self.sync_assistant_message(item_id, true);
    }

    fn finish_block(&mut self, item_id: &str, text: Option<&str>) {
        let unchanged = self.proj.item(item_id).is_some_and(|item| {
            is_terminal(item.status)
                && match (&item.kind, text) {
                    (_, None) => true,
                    (TurnItemKind::AssistantMessage(message), Some(text)) => message.text == text,
                    (TurnItemKind::Reasoning(reasoning), Some(text)) => reasoning.text == text,
                    _ => false,
                }
        });
        if unchanged {
            return;
        }
        self.update_item(item_id, false, |item| {
            item.status = TurnItemStatus::Completed;
            match &mut item.kind {
                TurnItemKind::AssistantMessage(message) => {
                    if let Some(text) = text {
                        message.text = text.to_owned();
                    }
                    message.streaming = false;
                }
                TurnItemKind::Reasoning(reasoning) => {
                    if let Some(text) = text {
                        reasoning.text = text.to_owned();
                    }
                    reasoning.streaming = false;
                }
                _ => {}
            }
        });
        self.sync_assistant_message(item_id, false);
    }

    fn apply_assistant_event(&mut self, message_id: &str, event: &AssistantMessageEvent) {
        match event {
            AssistantMessageEvent::TextStart(start) => {
                self.open_block(message_id, start.content_index.unwrap_or(0), true);
            }
            AssistantMessageEvent::TextDelta(delta) => {
                let item = self.open_block(message_id, delta.content_index.unwrap_or(0), true);
                self.append_block(&item, delta.delta.as_deref().unwrap_or_default());
            }
            AssistantMessageEvent::TextEnd(end) => {
                let item = self.open_block(message_id, end.content_index.unwrap_or(0), true);
                self.finish_block(&item, end.content.as_deref());
            }
            AssistantMessageEvent::ThinkingStart(start) => {
                let narration = start
                    .extra
                    .get("block")
                    .and_then(|block| block.get("narration"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                self.open_block(message_id, start.content_index.unwrap_or(0), narration);
            }
            AssistantMessageEvent::ThinkingDelta(delta) => {
                let item = self.open_block(message_id, delta.content_index.unwrap_or(0), false);
                self.append_block(&item, delta.delta.as_deref().unwrap_or_default());
            }
            AssistantMessageEvent::ThinkingEnd(end) => {
                let item = self.open_block(message_id, end.content_index.unwrap_or(0), false);
                self.finish_block(&item, end.content.as_deref());
            }
            AssistantMessageEvent::ToolcallEnd(end) => {
                if let Some(call) = &end.tool_call {
                    self.ensure_tool_call(call);
                }
            }
            _ => {}
        }
    }

    /// `message_end` content is authoritative.
    fn apply_final_block(&mut self, message_id: &str, index: i64, block: &AssistantContent) {
        match block {
            AssistantContent::Text(text) => {
                let content = text.text.as_deref().unwrap_or_default();
                match self.block_item(message_id, index) {
                    Some(item) => self.finish_block(&item, Some(content)),
                    None if !content.trim().is_empty() => {
                        let item = self.open_block(message_id, index, true);
                        self.finish_block(&item, Some(content));
                    }
                    None => {}
                }
            }
            AssistantContent::Thinking(thinking) => {
                let content = thinking.thinking.as_deref().unwrap_or_default();
                match self.block_item(message_id, index) {
                    Some(item) => self.finish_block(&item, Some(content)),
                    None if !content.trim().is_empty() => {
                        let item =
                            self.open_block(message_id, index, thinking.narration == Some(true));
                        self.finish_block(&item, Some(content));
                    }
                    None => {}
                }
            }
            AssistantContent::ToolCall(call) => {
                self.ensure_tool_call(call);
            }
            _ => {}
        }
    }

    fn close_message_blocks(&mut self, message_id: &str) {
        let open: Vec<String> = self
            .live
            .as_ref()
            .map(|live| {
                live.blocks
                    .iter()
                    .filter(|((id, _), _)| id == message_id)
                    .map(|(_, item)| item.clone())
                    .collect()
            })
            .unwrap_or_default();
        for item_id in open {
            if self
                .proj
                .item(&item_id)
                .is_some_and(|item| !is_terminal(item.status))
            {
                self.finish_block(&item_id, None);
            }
        }
    }

    fn ensure_tool_call(&mut self, call: &ToolCall) {
        let (Some(id), Some(name)) = (&call.id, &call.name) else {
            return;
        };
        let args = call
            .arguments
            .clone()
            .map(Value::Object)
            .unwrap_or(Value::Object(Map::new()));
        self.ensure_tool(id, name, &args, call.intent.as_deref());
    }

    /// The item of tool call `call_id`, created on first sight.
    fn ensure_tool(
        &mut self,
        call_id: &str,
        name: &str,
        args: &Value,
        intent: Option<&str>,
    ) -> Option<String> {
        if let Some(existing) = self.live.as_ref().and_then(|live| live.tools.get(call_id)) {
            return existing.clone();
        }
        self.ensure_live();
        let item_id = (name != ASK_TOOL).then(|| {
            let title = intent
                .filter(|intent| !intent.trim().is_empty())
                .unwrap_or(name)
                .to_owned();
            self.push_live_item(TurnItemStatus::Running, Some(title), tool_kind(name, args))
        });
        if let Some(live) = self.live.as_mut() {
            live.tools.insert(call_id.to_owned(), item_id.clone());
            live.tool_names.insert(call_id.to_owned(), name.to_owned());
        }
        item_id
    }

    // -----------------------------------------------------------------------
    // UI requests
    // -----------------------------------------------------------------------

    fn apply_ui_request(&mut self, request: &ExtensionUiRequest) -> Option<Effect> {
        let (omp_id, method, questions, timeout) = match request {
            ExtensionUiRequest::Ask(ask) => (
                ask.id.clone(),
                UiMethod::Ask(ask.questions.clone()),
                ask.questions
                    .iter()
                    .enumerate()
                    .map(|(index, question)| ask_question(index, question))
                    .collect(),
                ask.timeout,
            ),
            ExtensionUiRequest::Select(select) => {
                let details = select.option_details.as_deref().unwrap_or_default();
                let options = select
                    .options
                    .iter()
                    .enumerate()
                    .filter(|(_, label)| !label.trim().is_empty())
                    .map(|(index, label)| UserInputQuestionOption {
                        label: label.clone(),
                        description: details
                            .get(index)
                            .and_then(|detail| detail.description.as_deref())
                            .map_or_else(|| label.clone(), |d| non_blank(d, label)),
                        value: Some(label.clone()),
                        preview: None,
                    })
                    .collect();
                (
                    select.id.clone(),
                    UiMethod::Select,
                    vec![UserInputQuestion {
                        id: "answer".into(),
                        header: "Choose".into(),
                        question: non_blank(&select.title, "Choose an option"),
                        options,
                        multi_select: Some(false),
                        allow_custom_answer: Some(false),
                        initial_answer: None,
                        required: Some(true),
                    }],
                    select.timeout,
                )
            }
            ExtensionUiRequest::Confirm(confirm) => (
                confirm.id.clone(),
                UiMethod::Confirm,
                vec![UserInputQuestion {
                    id: "answer".into(),
                    header: non_blank(&confirm.title, "Confirm"),
                    question: non_blank(&confirm.message, &non_blank(&confirm.title, "Confirm?")),
                    options: ["Yes", "No"]
                        .into_iter()
                        .map(|label| UserInputQuestionOption {
                            label: label.into(),
                            description: label.into(),
                            value: Some(label.into()),
                            preview: None,
                        })
                        .collect(),
                    multi_select: Some(false),
                    allow_custom_answer: Some(false),
                    initial_answer: None,
                    required: Some(true),
                }],
                confirm.timeout,
            ),
            ExtensionUiRequest::Input(input) => (
                input.id.clone(),
                UiMethod::Value,
                vec![UserInputQuestion {
                    id: "answer".into(),
                    header: "Input".into(),
                    question: non_blank(&input.title, "Enter a value"),
                    options: Vec::new(),
                    multi_select: None,
                    allow_custom_answer: Some(true),
                    initial_answer: None,
                    required: Some(true),
                }],
                input.timeout,
            ),
            ExtensionUiRequest::Editor(editor) => (
                editor.id.clone(),
                UiMethod::Value,
                vec![UserInputQuestion {
                    id: "answer".into(),
                    header: "Edit".into(),
                    question: non_blank(&editor.title, "Edit the text"),
                    options: Vec::new(),
                    multi_select: None,
                    allow_custom_answer: Some(true),
                    initial_answer: editor.prefill.clone(),
                    required: Some(true),
                }],
                None,
            ),
            ExtensionUiRequest::Cancel(cancel) => {
                let request_id = self
                    .ui
                    .iter()
                    .find(|(_, pending)| pending.omp_id == cancel.target_id)
                    .map(|(request_id, _)| request_id.clone())?;
                self.ui.remove(&request_id);
                self.close_request(&request_id, RuntimeRequestStatus::Cancelled);
                self.after_request_closed();
                return None;
            }
            ExtensionUiRequest::Notify(notify) => {
                if !notify.message.trim().is_empty() {
                    self.push_live_item(
                        TurnItemStatus::Completed,
                        None,
                        TurnItemKind::SystemNotice(MessageItem {
                            message: notify.message.clone(),
                        }),
                    );
                }
                return None;
            }
            _ => return None,
        };
        let (run_id, node_id) = self.ensure_live();
        let request_id = format!("rq_{}", self.proj.requests.len() + 1);
        let turn_id = self.live.as_ref().and_then(|live| live.turn_id.clone());
        self.proj.requests.push(RuntimeRequest {
            id: request_id.clone(),
            node_id: node_id.clone(),
            provider_turn_id: turn_id,
            native_request_ref: Some(ProviderRef {
                driver: DRIVER.to_owned(),
                native_id: Some(omp_id.clone()),
                strength: NativeRefStrength::Weak,
                fingerprint: None,
                ordinal: None,
            }),
            kind: RuntimeRequestKind::UserInput,
            status: RuntimeRequestStatus::Pending,
            response_capability: ResponseCapability::Live {
                provider_session_id: self.ps(),
                supports_feedback: None,
            },
            created_at: now(),
            resolved_at: None,
            decision: None,
            feedback: None,
            answers: None,
        });
        self.proj.touch_updated(Kind::Request, &request_id);
        let item_id = self.push_item(
            Some(run_id.clone()),
            Some(node_id),
            TurnItemStatus::Waiting,
            None,
            TurnItemKind::UserInputRequest(UserInputRequestItem {
                request_id: request_id.clone(),
                questions,
                question_answer: None,
                response_mode: None,
            }),
        );
        self.ui.insert(
            request_id.clone(),
            PendingUi {
                omp_id,
                method,
                item_id,
            },
        );
        if self
            .proj
            .run(&run_id)
            .is_some_and(|run| matches!(run.status, RunStatus::Running | RunStatus::Starting))
        {
            self.update_run(&run_id, |run| run.status = RunStatus::Waiting);
            self.set_session_status_inner(ProviderSessionStatus::Waiting);
        }
        timeout.filter(|ms| *ms > 0).map(|ms| Effect::ExpireAfter {
            request_id,
            after_ms: ms as u64,
        })
    }

    // -----------------------------------------------------------------------
    // State refresh: todos and model
    // -----------------------------------------------------------------------

    /// Applies a `get_state` snapshot (todos and the effective model).
    pub fn apply_state(&mut self, state: &SessionState) {
        self.apply_todos(&state.todo_phases);
        self.apply_goal_state(state.goal.as_ref());
        if let Some(model) = &state.model {
            let slug = format!("{}/{}", model.provider, model.id);
            if self.proj.thread.model_selection.model != slug {
                let mut selection = self.proj.thread.model_selection.clone();
                selection.model = slug;
                self.set_model_selection(&selection);
            }
        }
    }

    /// Replaces the thread's todo list (ios-surface §5 "Todos").
    pub fn apply_todos(&mut self, phases: &[TodoPhase]) {
        let steps: Vec<PlanStep> = phases
            .iter()
            .enumerate()
            .flat_map(|(phase_index, phase)| {
                phase
                    .tasks
                    .iter()
                    .enumerate()
                    .filter(|(_, task)| !task.content.trim().is_empty())
                    .map(move |(task_index, task)| PlanStep {
                        id: format!("{phase_index}.{task_index}"),
                        text: task.content.trim().to_owned(),
                        status: match task.status {
                            TodoStatus::InProgress => PlanStepStatus::Running,
                            TodoStatus::Completed | TodoStatus::Abandoned => {
                                PlanStepStatus::Completed
                            }
                            TodoStatus::Pending | TodoStatus::Blocked => PlanStepStatus::Pending,
                        },
                        duration_anchor_at: None,
                        duration_ms: None,
                    })
            })
            .collect();
        let plan_id = format!("plan_{}", self.proj.thread.id);
        let existing = self.proj.plans.iter().position(|plan| plan.id == plan_id);
        if let Some(index) = existing
            && matches!(&self.proj.plans[index].kind, PlanArtifactKind::TodoList { steps: old, .. } if *old == steps)
        {
            return;
        }
        if existing.is_none() && steps.is_empty() {
            return;
        }
        let (run_id, node_id) = match (&self.live, self.proj.latest_run()) {
            (Some(live), _) => (live.run_id.clone(), live.node_id.clone()),
            (None, Some(run)) => (run.id.clone(), run.root_node_id.clone().unwrap_or_default()),
            (None, None) => return,
        };
        let status = if steps
            .iter()
            .all(|step| step.status == PlanStepStatus::Completed)
        {
            PlanArtifactStatus::Completed
        } else {
            PlanArtifactStatus::Active
        };
        let plan = PlanArtifact {
            id: plan_id.clone(),
            thread_id: self.proj.thread.id.clone(),
            run_id: Some(run_id.clone()),
            node_id: node_id.clone(),
            status,
            detail_in_turn_item: Some(True),
            kind: PlanArtifactKind::TodoList {
                steps: steps.clone(),
                explanation: None,
            },
        };
        match existing {
            Some(index) => self.proj.plans[index] = plan,
            None => self.proj.plans.push(plan),
        }
        self.proj.touch_updated(Kind::Plan, &plan_id);

        let item_status = if status == PlanArtifactStatus::Completed {
            TurnItemStatus::Completed
        } else {
            TurnItemStatus::Running
        };
        let body = TurnItemKind::TodoList(TodoListItem {
            plan_id,
            steps,
            explanation: None,
        });
        let current = self
            .live
            .as_ref()
            .filter(|live| live.run_id == run_id)
            .and_then(|live| live.todo_item.clone())
            .or_else(|| {
                self.proj
                    .items
                    .iter()
                    .rev()
                    .find(|item| {
                        item.run_id.as_deref() == Some(run_id.as_str())
                            && matches!(item.kind, TurnItemKind::TodoList(_))
                    })
                    .map(|item| item.id.clone())
            });
        match current {
            Some(item_id) => self.update_item(&item_id, false, |item| {
                item.status = item_status;
                item.kind = body;
            }),
            None => {
                let item_id = self.push_item(
                    Some(run_id),
                    Some(node_id),
                    item_status,
                    Some("Todos".into()),
                    body,
                );
                if let Some(live) = self.live.as_mut() {
                    live.todo_item = Some(item_id);
                }
            }
        }
    }
}

fn is_terminal(status: TurnItemStatus) -> bool {
    matches!(
        status,
        TurnItemStatus::Completed
            | TurnItemStatus::Failed
            | TurnItemStatus::Cancelled
            | TurnItemStatus::Interrupted
    )
}

fn set_tool_output(item: &mut TurnItem, text: String, details: Option<&Value>) {
    match &mut item.kind {
        TurnItemKind::CommandExecution(command) => {
            if let Some(code) = details
                .and_then(|details| details.get("exitCode"))
                .and_then(Value::as_i64)
            {
                command.exit_code = Some(code);
            }
            command.output = Some(text);
        }
        TurnItemKind::FileChange(change) => {
            if let Some(diff) = details
                .and_then(|details| details.get("diff"))
                .and_then(Value::as_str)
            {
                let (additions, deletions) = diff_counts(diff);
                change.additions = Some(additions);
                change.deletions = Some(deletions);
                change.diff_str = Some(diff.to_owned());
            }
        }
        TurnItemKind::FileSearch(search) => {
            let mut results = grep_matches(&text);
            if results.is_empty()
                && let Some(files) = details
                    .and_then(|details| details.get("files"))
                    .and_then(Value::as_array)
            {
                results = files
                    .iter()
                    .filter_map(Value::as_str)
                    .take(MAX_SEARCH_RESULTS)
                    .map(|file| FileSearchResult {
                        file_name: file.to_owned(),
                        line: None,
                        column: None,
                        preview: None,
                    })
                    .collect();
            }
            search.results = Some(results);
        }
        TurnItemKind::WebSearch(search) => {
            if let Some(sources) = details
                .and_then(|details| details.pointer("/response/sources"))
                .and_then(Value::as_array)
            {
                let field = |source: &Value, key: &str| {
                    source
                        .get(key)
                        .and_then(Value::as_str)
                        .map(|text| truncate(text.trim(), 300))
                };
                search.results = Some(
                    sources
                        .iter()
                        .map(|source| WebSearchResult {
                            title: field(source, "title"),
                            url: field(source, "url"),
                            snippet: field(source, "snippet"),
                        })
                        .collect(),
                );
            }
        }
        TurnItemKind::DynamicTool(tool) => tool.output = Some(Some(Value::String(text))),
        _ => {}
    }
}

/// Most search results kept on a `file_search` item.
const MAX_SEARCH_RESULTS: usize = 200;

/// Added and removed lines of an omp edit diff (`+12|text`, `-12|text`,
/// context ` 12|text`).
fn diff_counts(diff: &str) -> (u64, u64) {
    diff.lines().fold((0, 0), |(added, removed), line| {
        if line.starts_with('+') && !line.starts_with("+++") {
            (added + 1, removed)
        } else if line.starts_with('-') && !line.starts_with("---") {
            (added, removed + 1)
        } else {
            (added, removed)
        }
    })
}

/// The matching lines of omp's `grep` output: `# dir/` and `## file#TAG`
/// headers (`# file#TAG` alone when one file matched), then numbered lines
/// where `*` marks a match (`*15:text`) and a space a context line
/// (` 14:text`).
fn grep_matches(text: &str) -> Vec<FileSearchResult> {
    let mut dir = "";
    let mut file = None::<String>;
    let mut results = Vec::new();
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("## ") {
            let name = name.rsplit_once('#').map_or(name, |(name, _)| name);
            file = Some(format!("{dir}{}", name.trim()));
        } else if let Some(path) = line.strip_prefix("# ") {
            // `# dir/` heads a directory's files; a lone file is headed
            // `# file#TAG` itself.
            match path.trim().rsplit_once('#') {
                Some((name, _)) => {
                    dir = "";
                    file = Some(name.to_owned());
                }
                None => {
                    dir = path.trim();
                    file = None;
                }
            }
        } else if let Some(rest) = line.strip_prefix('*')
            && let Some((number, preview)) = rest.split_once(':')
            && let Ok(number) = number.trim().parse::<u64>()
            && let Some(file) = &file
        {
            results.push(FileSearchResult {
                file_name: file.clone(),
                line: Some(number),
                column: None,
                preview: Some(truncate(preview.trim(), 300)),
            });
            if results.len() == MAX_SEARCH_RESULTS {
                break;
            }
        }
    }
    results
}

/// `ProviderFailure` from omp's `PromptError`.
fn failure_of(error: &PromptError) -> ProviderFailure {
    let cost_cap = error.code.is_some();
    let class = if cost_cap || error.http_status == Some(429) {
        ProviderFailureClass::UsageLimit
    } else {
        ProviderFailureClass::ProviderError
    };
    ProviderFailure {
        class,
        message: truncate(
            &non_blank(&error.message, "The run failed."),
            MAX_FAILURE_MESSAGE,
        ),
        code: cost_cap.then(|| "cost_cap".to_owned()),
        retryable: Some(error.retryable),
        reset_at: None,
    }
}

/// `ask` question → t3 question (ios-surface §5 "Questions").
fn ask_question(index: usize, question: &AskQuestion) -> UserInputQuestion {
    UserInputQuestion {
        id: non_blank(&question.id, &format!("q{}", index + 1)),
        header: non_blank(question.header.as_deref().unwrap_or_default(), "Question"),
        question: non_blank(&question.question, "Question"),
        options: question
            .options
            .iter()
            .filter(|option| !option.label.trim().is_empty())
            .map(|option| UserInputQuestionOption {
                label: option.label.clone(),
                description: non_blank(
                    option.description.as_deref().unwrap_or_default(),
                    &option.label,
                ),
                value: Some(option.label.clone()),
                preview: option.preview.clone(),
            })
            .collect(),
        multi_select: Some(question.multi),
        allow_custom_answer: Some(true),
        initial_answer: None,
        required: None,
    }
}

/// Answer values for one question: a string or an array of strings.
fn answer_values(answer: Option<&Value>) -> Vec<String> {
    match answer {
        Some(Value::String(text)) => vec![text.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// D10 answers → omp `extension_ui_response` (ios-surface §5 "D10 answer").
fn build_ui_response(
    pending: &PendingUi,
    answers: &ProviderUserInputAnswers,
) -> ExtensionUiResponse {
    let id = pending.omp_id.clone();
    let first = |key: &str| {
        answer_values(answers.get(key).or_else(|| answers.values().next()))
            .into_iter()
            .next()
            .unwrap_or_default()
    };
    match &pending.method {
        UiMethod::Ask(questions) => ExtensionUiResponse::AnswersUiResponse(AnswersUiResponse {
            id,
            answers: questions
                .iter()
                .enumerate()
                .map(|(index, question)| {
                    let key = non_blank(&question.id, &format!("q{}", index + 1));
                    let values = answer_values(answers.get(&key));
                    let (selected, custom): (Vec<String>, Vec<String>) = values
                        .into_iter()
                        .filter(|value| !value.trim().is_empty())
                        .partition(|value| question.options.iter().any(|o| &o.label == value));
                    let custom = (!custom.is_empty()).then(|| custom.join(", "));
                    if question.multi {
                        AskAnswer {
                            id: question.id.clone(),
                            selected_options: selected,
                            custom_input: custom,
                        }
                    } else if let Some(option) = selected.into_iter().next() {
                        AskAnswer {
                            id: question.id.clone(),
                            selected_options: vec![option],
                            custom_input: None,
                        }
                    } else {
                        AskAnswer {
                            id: question.id.clone(),
                            selected_options: Vec::new(),
                            custom_input: custom,
                        }
                    }
                })
                .collect(),
        }),
        UiMethod::Select | UiMethod::Value => {
            ExtensionUiResponse::ValueUiResponse(ValueUiResponse {
                id,
                value: first("answer"),
            })
        }
        UiMethod::Confirm => ExtensionUiResponse::ConfirmUiResponse(ConfirmUiResponse {
            id,
            confirmed: first("answer") == "Yes",
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_rewrite() {
        let skills = vec!["jev".to_owned(), "unslop".to_owned()];
        assert_eq!(
            rewrite_skill("$jev is this ok?", &skills),
            "/skill:jev is this ok?"
        );
        assert_eq!(
            rewrite_skill("please $unslop this", &skills),
            "/skill:unslop please this"
        );
        assert_eq!(rewrite_skill("cost is $5", &skills), "cost is $5");
        assert_eq!(rewrite_skill("a$jev", &skills), "a$jev");
        assert_eq!(rewrite_skill("$jev", &skills), "/skill:jev");
    }

    #[test]
    fn model_split() {
        assert_eq!(
            split_model("opencode-go/glm-5.3-flash"),
            Some(("opencode-go", "glm-5.3-flash"))
        );
        assert_eq!(split_model("a/b/c"), Some(("a", "b/c")));
        assert_eq!(split_model("nope"), None);
    }

    #[test]
    fn hashline_edits_and_writes_map_to_file_changes() {
        let edit = tool_kind(
            "edit",
            &serde_json::json!({"input": "[src/a.rs#1A2B]\nPUT 3:\n+x\n[src/b.rs#FFFF]\nCUT 1"}),
        );
        let TurnItemKind::FileChange(change) = edit else {
            panic!("hashline edit is a file change: {edit:?}");
        };
        assert_eq!(change.file_name, "src/a.rs");
        // Writes to internal URLs (peer messages, tool devices) are not files.
        for path in ["agent://Reviewer", "xd://lsp"] {
            let write = tool_kind("write", &serde_json::json!({"path": path, "content": "hi"}));
            assert!(matches!(write, TurnItemKind::DynamicTool(_)), "{path}");
        }
        let glob = tool_kind("glob", &serde_json::json!({"path": "src/**/*.rs"}));
        let TurnItemKind::FileSearch(search) = glob else {
            panic!("glob is a file search");
        };
        assert_eq!(search.pattern.as_deref(), Some("src/**/*.rs"));
    }

    #[test]
    fn tool_results_fill_diffs_matches_and_sources() {
        let mut item: TurnItem = serde_json::from_value(serde_json::json!({
            "id":"ti","threadId":"T","runId":null,"nodeId":null,"providerThreadId":null,
            "providerTurnId":null,"nativeItemRef":null,"parentItemId":null,"ordinal":1,
            "status":"running","title":null,"startedAt":null,"completedAt":null,
            "updatedAt":"2026-11-02T00:00:00.000Z","type":"system_notice","message":""
        }))
        .unwrap();
        item.kind = tool_kind(
            "edit",
            &serde_json::json!({"input": "[a.rs#0000]\nPUT 1:\n+x"}),
        );
        let diff = " 1|a\n-2|b\n+2|c\n+3|d";
        set_tool_output(
            &mut item,
            "ok".into(),
            Some(&serde_json::json!({"diff": diff})),
        );
        let TurnItemKind::FileChange(change) = &item.kind else {
            panic!();
        };
        assert_eq!((change.additions, change.deletions), (Some(2), Some(1)));
        assert_eq!(change.diff_str.as_deref(), Some(diff));

        item.kind = tool_kind("grep", &serde_json::json!({"pattern": "fn main"}));
        let grep =
            "# src/\n## main.rs#AB12\n 1:// x\n*2:fn main() {\n## lib.rs#CD34\n*9:  fn main_like()";
        set_tool_output(&mut item, grep.into(), None);
        let TurnItemKind::FileSearch(search) = &item.kind else {
            panic!();
        };
        let found: Vec<(&str, Option<u64>, Option<&str>)> = search
            .results
            .iter()
            .flatten()
            .map(|r| (r.file_name.as_str(), r.line, r.preview.as_deref()))
            .collect();
        assert_eq!(
            found,
            [
                ("src/main.rs", Some(2), Some("fn main() {")),
                ("src/lib.rs", Some(9), Some("fn main_like()"))
            ]
        );

        set_tool_output(
            &mut item,
            "# main.rs#6FB6\n*1:fn main() {\n 2:  x".into(),
            None,
        );
        let TurnItemKind::FileSearch(search) = &item.kind else {
            panic!();
        };
        assert_eq!(search.results.as_ref().unwrap()[0].file_name, "main.rs");

        item.kind = tool_kind("glob", &serde_json::json!({"path": "*.toml"}));
        set_tool_output(
            &mut item,
            "# ./\nCargo.toml".into(),
            Some(&serde_json::json!({"files": ["Cargo.toml"]})),
        );
        let TurnItemKind::FileSearch(search) = &item.kind else {
            panic!();
        };
        assert_eq!(search.results.as_ref().unwrap()[0].file_name, "Cargo.toml");

        item.kind = tool_kind("web_search", &serde_json::json!({"query": "gpui"}));
        set_tool_output(
            &mut item,
            "[1] Zed".into(),
            Some(&serde_json::json!({"response": {"sources": [
                {"title": "Zed", "url": "https://zed.dev", "snippet": "editor"}]}})),
        );
        let TurnItemKind::WebSearch(search) = &item.kind else {
            panic!();
        };
        let source = &search.results.as_ref().unwrap()[0];
        assert_eq!(source.url.as_deref(), Some("https://zed.dev"));
    }
}
