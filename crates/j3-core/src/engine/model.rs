//! One thread's state (`docs/protocol/wire.md` §4): its row fields, turns,
//! feed items, requests and queue; the commands that change them; and the
//! omp event → entity mapping of wire.md §7. Tool calls go through
//! [`crate::v2::tools`], which the session replay shares, so a thread looks
//! the same whether it was streamed or rebuilt.
//!
//! Everything here is synchronous and free of I/O: inputs are commands and
//! omp [`Event`]s, outputs are recorded changes (drained by the actor into
//! one hub commit) and [`Effect`]s the actor must perform.

use std::collections::HashMap;

use j3_protocol::v2::{
    entities::{
        Answer, Attachment, CompactionItem, Delivery, Goal, Item, ItemKind, ItemStatus,
        MessageRole, MessageSummary, ModelRef, NoticeItem, NoticeLevel, Question, QuestionOption,
        QueueEntry, Request, RequestKind, RequestStatus, RequestSummary, StopItem, TextItem,
        ThinkingLevel, Thread, ThreadMode, ThreadStatus, TodoItem, Triage, Turn, TurnError,
        TurnErrorKind, TurnStatus, TurnSummary, UserItem, WokenBy,
    },
    envelope::{Error, ErrorCode},
    streams::DeltaField,
};
use serde_json::{Map, Value, json};

use super::triage::{self, Facts};
use crate::{
    clock::{iso, now_iso, now_ms},
    omp::omp_rpc::{
        self, AgentMessage, AnswersUiResponse, AskAnswer, AskQuestion, AssistantContent,
        AssistantMessageEvent, CancelUiResponse, ConfirmUiResponse, Event, ExtensionUiRequest,
        ExtensionUiResponse, LitTrue, NotifyType, PromptError, PromptStatus, RpcAgentEvent,
        RpcNotification, SessionState, ToolCall, ValueUiResponse,
    },
    store::{Record, ThreadRecords, random_hex},
    v2::tools,
};

mod agents;

/// omp's ask tool: its questions are requests, not feed items.
const ASK_TOOL: &str = "ask";
const TODO_TOOL: &str = "todo";
const TASK_TOOL: &str = "task";
const MAX_ERROR_MESSAGE: usize = 4096;

/// Work the actor must do after an input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Re-read omp's `get_state` (model, thinking level, goal) and feed it to
    /// [`ThreadModel::apply_state`].
    RefreshState,
    /// The active turn ended; the next queue entry may start.
    TurnEnded,
    /// Expire request `request_id` after `after_ms` unless it was answered.
    ExpireAfter { request_id: String, after_ms: u64 },
    /// Read the transcript of finished subagent `subagent_id` and feed it to
    /// [`ThreadModel::apply_subagent_transcript`].
    FetchSubagentResult { subagent_id: String },
    /// Keep images a tool returned as attachments.
    StoreImages(Vec<ToolImage>),
}

/// An image block of a tool result, to keep as an attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolImage {
    pub attachment_id: String,
    pub mime_type: String,
    /// Base64, as omp sent it.
    pub data: String,
}

/// A user message to start, queue or steer.
#[derive(Debug, Clone, PartialEq)]
pub struct UserInput {
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub model: ModelRef,
}

/// Fields of a new thread.
#[derive(Debug, Clone)]
pub struct NewThread {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub model: ModelRef,
    pub mode: ThreadMode,
}

/// The stored fields of the thread row; the rest of [`Thread`] is derived
/// from the turns, items, requests and queue.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadState {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
    pub model: ModelRef,
    pub mode: ThreadMode,
    pub goal: Option<Goal>,
    pub triage: Triage,
}

impl ThreadState {
    pub fn from_row(row: &Thread) -> Self {
        Self {
            id: row.id.clone(),
            project_id: row.project_id.clone(),
            title: row.title.clone(),
            created_at: row.created_at.clone(),
            updated_at: row.updated_at.clone(),
            archived_at: row.archived_at.clone(),
            model: row.model.clone(),
            mode: row.mode,
            goal: row.goal.clone(),
            triage: row.triage.clone(),
        }
    }
}

/// The turn omp is driving.
#[derive(Debug, Clone)]
struct Live {
    turn_id: String,
    /// omp reported `agent_start`: the turn is running and steerable.
    started: bool,
    /// `(omp message id, content index)` → item id.
    blocks: HashMap<(String, i64), String>,
    /// `toolCallId` → item id (`None`: no item, e.g. `ask`).
    tools: HashMap<String, Option<String>>,
    status: Option<PromptStatus>,
    error: Option<TurnError>,
    /// The stop item of a user stop.
    stop: Option<String>,
    compaction: Option<String>,
}

impl Live {
    fn new(turn_id: String) -> Self {
        Self {
            turn_id,
            started: false,
            blocks: HashMap::new(),
            tools: HashMap::new(),
            status: None,
            error: None,
            stop: None,
            compaction: None,
        }
    }
}

/// How a pending omp UI request is answered.
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
}

/// One changed entity, in the order first touched.
#[derive(Debug, Clone)]
enum Pending {
    Turn(String),
    /// `delta`: only text was appended since the last commit.
    Item {
        id: String,
        delta: Option<(DeltaField, String)>,
    },
    Request(String),
}

#[derive(Debug, Default)]
struct Changes {
    pending: Vec<Pending>,
    queue: bool,
    /// Something the row derives from changed.
    row: bool,
    /// Send the row even if it is unchanged (an accepted command).
    force_row: bool,
    /// Some change must be committed now, not after the coalescing window.
    urgent: bool,
}

/// Store rows and stream items of one commit.
#[derive(Debug, Default)]
pub struct Drained {
    pub records: Vec<Record>,
    /// `threads.watch` items without their `sequence`.
    pub items: Vec<Map<String, Value>>,
    /// The thread row, when it changed.
    pub row: Option<Value>,
}

impl Drained {
    pub fn is_empty(&self) -> bool {
        self.records.is_empty() && self.items.is_empty() && self.row.is_none()
    }
}

pub struct ThreadModel {
    pub thread: ThreadState,
    pub turns: Vec<Turn>,
    pub items: Vec<Item>,
    pub requests: Vec<Request>,
    pub queue: Vec<QueueEntry>,
    live: Option<Live>,
    /// Request id → the omp UI request it answers.
    ui: HashMap<String, PendingUi>,
    /// Last `recentOutput` lines per subagent: its result when the
    /// transcript can't be read.
    subagent_output: HashMap<String, Vec<String>>,
    changes: Changes,
    /// The row as last committed.
    committed: Option<Thread>,
}

fn item_type(kind: &str, field: &str, body: Value) -> Map<String, Value> {
    let mut item = Map::new();
    item.insert("type".into(), Value::String(kind.to_owned()));
    item.insert(field.into(), body);
    item
}

fn to_value<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("v2 entities always encode")
}

fn non_blank(text: &str, fallback: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.to_owned()
    }
}

pub(crate) fn truncate(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => text[..at].to_owned(),
        None => text.to_owned(),
    }
}

fn conflict(message: &str) -> Error {
    Error::new(ErrorCode::Conflict, message)
}

/// omp's thinking level as the wire's (the names are the same).
pub fn wire_level(level: omp_rpc::ThinkingLevel) -> Option<ThinkingLevel> {
    serde_json::from_value(serde_json::to_value(level).ok()?).ok()
}

/// The wire's thinking level as omp's.
pub fn omp_level(level: ThinkingLevel) -> Option<omp_rpc::ThinkingLevel> {
    serde_json::from_value(serde_json::to_value(level).ok()?).ok()
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

/// The turn error of omp's `PromptError`; cost caps and HTTP 429 are usage
/// limits (the Inbox "Limited" state).
fn turn_error(error: &PromptError) -> TurnError {
    let usage_limit = error.code.is_some() || error.http_status == Some(429);
    TurnError {
        message: truncate(
            &non_blank(&error.message, "The turn failed."),
            MAX_ERROR_MESSAGE,
        ),
        kind: if usage_limit {
            TurnErrorKind::UsageLimit
        } else {
            TurnErrorKind::Provider
        },
    }
}

impl ThreadModel {
    /// A brand-new thread.
    pub fn create(spec: NewThread) -> Self {
        let at = now_iso();
        Self::with(
            ThreadState {
                id: spec.id,
                project_id: spec.project_id,
                title: non_blank(&spec.title, "New thread"),
                created_at: at.clone(),
                updated_at: at.clone(),
                archived_at: None,
                model: spec.model,
                mode: spec.mode,
                goal: None,
                triage: Triage {
                    bumped_at: at,
                    ..Triage::default()
                },
            },
            ThreadRecords::default(),
            None,
        )
    }

    /// A thread loaded from its stored row and records.
    pub fn load(row: &Thread, records: ThreadRecords) -> Self {
        Self::with(ThreadState::from_row(row), records, Some(row.clone()))
    }

    fn with(thread: ThreadState, records: ThreadRecords, committed: Option<Thread>) -> Self {
        let mut model = Self {
            thread,
            turns: records.turns,
            items: records.items,
            requests: records.requests,
            queue: records.queue,
            live: None,
            ui: HashMap::new(),
            subagent_output: HashMap::new(),
            changes: Changes::default(),
            committed,
        };
        if model.committed.is_none() {
            model.changes.row = true;
            model.changes.urgent = true;
        }
        model
    }

    pub fn thread_id(&self) -> &str {
        &self.thread.id
    }

    // -----------------------------------------------------------------------
    // Derived state
    // -----------------------------------------------------------------------

    /// The turn that has not ended, if any (only the latest can be active).
    pub fn active_turn(&self) -> Option<&Turn> {
        self.turns.last().filter(|turn| turn.status.is_active())
    }

    fn open_requests(&self) -> impl Iterator<Item = &Request> {
        self.requests
            .iter()
            .filter(|request| request.status == RequestStatus::Open)
    }

    /// omp is working on the active turn (it can be steered).
    pub fn has_running_turn(&self) -> bool {
        self.live.as_ref().is_some_and(|live| live.started)
    }

    /// No turn is active.
    pub fn is_idle(&self) -> bool {
        self.live.is_none() && self.active_turn().is_none()
    }

    pub fn status(&self) -> ThreadStatus {
        match self.active_turn() {
            None if self
                .turns
                .last()
                .is_some_and(|turn| turn.status == TurnStatus::Failed) =>
            {
                ThreadStatus::Failed
            }
            None => ThreadStatus::Idle,
            Some(turn) => {
                if self.live.as_ref().is_some_and(|live| live.stop.is_some()) {
                    ThreadStatus::Stopping
                } else if self.open_requests().next().is_some() {
                    ThreadStatus::Waiting
                } else if turn.started_at.is_none() {
                    ThreadStatus::Starting
                } else {
                    ThreadStatus::Running
                }
            }
        }
    }

    /// The thread list row (wire.md §4.2).
    pub fn row(&self) -> Thread {
        let thread = &self.thread;
        let last_message = self.items.iter().rev().find_map(|item| {
            let (role, text) = match &item.kind {
                ItemKind::User(user) => (MessageRole::User, &user.text),
                ItemKind::Text(text) if item.status.is_terminal() => {
                    (MessageRole::Assistant, &text.text)
                }
                _ => return None,
            };
            Some(MessageSummary {
                role,
                text: truncate(text, MessageSummary::MAX_TEXT),
                at: item
                    .ended_at
                    .clone()
                    .unwrap_or_else(|| item.started_at.clone()),
            })
        });
        Thread {
            id: thread.id.clone(),
            project_id: thread.project_id.clone(),
            title: thread.title.clone(),
            created_at: thread.created_at.clone(),
            updated_at: thread.updated_at.clone(),
            archived_at: thread.archived_at.clone(),
            status: self.status(),
            model: thread.model.clone(),
            mode: thread.mode,
            active_turn_id: self.active_turn().map(|turn| turn.id.clone()),
            pending_request: self.open_requests().next().map(|request| RequestSummary {
                id: request.id.clone(),
                kind: request.kind,
                title: request.title.clone(),
                opened_at: request.opened_at.clone(),
            }),
            queued: self.queue.len() as u32,
            last_message,
            latest_turn: self.turns.last().map(TurnSummary::from),
            latest_user_message_at: self.items.iter().rev().find_map(|item| {
                matches!(item.kind, ItemKind::User(_)).then(|| item.started_at.clone())
            }),
            goal: thread.goal.clone(),
            triage: thread.triage.clone(),
        }
    }

    // -----------------------------------------------------------------------
    // Change tracking
    // -----------------------------------------------------------------------

    fn touch_row(&mut self) {
        self.changes.row = true;
        self.changes.urgent = true;
    }

    /// An accepted command: its row is sent even when unchanged.
    pub fn touch_command(&mut self) {
        self.changes.force_row = true;
        self.touch_row();
    }

    fn touch_turn(&mut self, id: &str) {
        if !self
            .changes
            .pending
            .iter()
            .any(|pending| matches!(pending, Pending::Turn(turn) if turn == id))
        {
            self.changes.pending.push(Pending::Turn(id.to_owned()));
        }
        self.touch_row();
    }

    fn touch_request(&mut self, id: &str) {
        if !self
            .changes
            .pending
            .iter()
            .any(|pending| matches!(pending, Pending::Request(request) if request == id))
        {
            self.changes.pending.push(Pending::Request(id.to_owned()));
        }
        self.touch_row();
    }

    fn touch_queue(&mut self) {
        self.changes.queue = true;
        self.touch_row();
    }

    /// A structural change of item `id`: the whole item is sent. `urgent`
    /// is false for progress-only changes that may wait for the coalescing
    /// window.
    fn touch_item(&mut self, id: &str, urgent: bool) {
        match self
            .changes
            .pending
            .iter_mut()
            .find_map(|pending| match pending {
                Pending::Item { id: pending, delta } if pending == id => Some(delta),
                _ => None,
            }) {
            Some(delta) => *delta = None,
            None => self.changes.pending.push(Pending::Item {
                id: id.to_owned(),
                delta: None,
            }),
        }
        self.changes.row = true;
        self.changes.urgent |= urgent;
    }

    /// Text appended to item `id`'s `field`: sent as `item.delta` unless the
    /// item changes structurally before the commit.
    fn touch_delta(&mut self, id: &str, field: DeltaField, append: &str) {
        let pending = self
            .changes
            .pending
            .iter_mut()
            .find_map(|pending| match pending {
                Pending::Item { id: pending, delta } if pending == id => Some(delta),
                _ => None,
            });
        match pending {
            Some(Some((pending_field, text))) if *pending_field == field => text.push_str(append),
            Some(Some(_)) => self.touch_item(id, true),
            Some(None) => {}
            None => self.changes.pending.push(Pending::Item {
                id: id.to_owned(),
                delta: Some((field, append.to_owned())),
            }),
        }
        self.changes.row = true;
    }

    /// Some change must be committed now.
    pub fn has_urgent(&self) -> bool {
        self.changes.urgent
    }

    pub fn has_pending(&self) -> bool {
        !self.changes.pending.is_empty() || self.changes.queue || self.changes.row
    }

    /// Takes every recorded change as store records and stream items, plus
    /// the row when it changed (its `updatedAt` then moves to now).
    pub fn drain(&mut self) -> Drained {
        let changes = std::mem::take(&mut self.changes);
        let mut drained = Drained::default();
        for pending in changes.pending {
            match pending {
                Pending::Turn(id) => {
                    if let Some(turn) = self.turns.iter().find(|turn| turn.id == id) {
                        let value = to_value(turn);
                        drained
                            .records
                            .push(Record::new("turn", &id, turn.ordinal.into(), &value));
                        drained.items.push(item_type("turn", "turn", value));
                    }
                }
                Pending::Item { id, delta } => {
                    if let Some(item) = self.items.iter().find(|item| item.id == id) {
                        let value = to_value(item);
                        drained
                            .records
                            .push(Record::new("item", &id, item.ordinal as i64, &value));
                        match delta {
                            Some((_, append)) if append.is_empty() => {}
                            Some((field, append)) => {
                                let mut delta = Map::new();
                                delta.insert("type".into(), json!("item.delta"));
                                delta.insert("itemId".into(), json!(id));
                                delta.insert("field".into(), to_value(&field));
                                delta.insert("append".into(), json!(append));
                                drained.items.push(delta);
                            }
                            None => drained.items.push(item_type("item", "item", value)),
                        }
                    }
                }
                Pending::Request(id) => {
                    if let Some((index, request)) =
                        self.requests.iter().enumerate().find(|(_, r)| r.id == id)
                    {
                        let value = to_value(request);
                        drained
                            .records
                            .push(Record::new("request", &id, index as i64, &value));
                        drained.items.push(item_type("request", "request", value));
                    }
                }
            }
        }
        if changes.queue {
            let value = to_value(&self.queue);
            drained
                .records
                .push(Record::new("queue", "queue", 0, &value));
            drained.items.push(item_type("queue", "queue", value));
        }
        if changes.row || changes.force_row {
            let mut row = self.row();
            let changed = self.committed.as_ref().is_none_or(|old| {
                let mut probe = row.clone();
                probe.updated_at.clone_from(&old.updated_at);
                probe != *old
            });
            if changed {
                self.thread.updated_at = now_iso();
                row.updated_at.clone_from(&self.thread.updated_at);
            }
            if changed || changes.force_row {
                drained.row = Some(to_value(&row));
                self.committed = Some(row);
            }
        }
        drained
    }

    // -----------------------------------------------------------------------
    // Entities
    // -----------------------------------------------------------------------

    fn next_ordinal(&self) -> u64 {
        self.items.last().map_or(1, |item| item.ordinal + 1)
    }

    fn turn_mut(&mut self, id: &str) -> Option<&mut Turn> {
        self.turns.iter_mut().find(|turn| turn.id == id)
    }

    fn item_mut(&mut self, id: &str) -> Option<&mut Item> {
        self.items.iter_mut().rev().find(|item| item.id == id)
    }

    fn item(&self, id: &str) -> Option<&Item> {
        self.items.iter().rev().find(|item| item.id == id)
    }

    /// Adds an item to turn `turn_id`.
    fn push_item(&mut self, turn_id: &str, status: ItemStatus, kind: ItemKind) -> String {
        let ordinal = self.next_ordinal();
        let id = format!("item_{ordinal}");
        let at = now_iso();
        self.items.push(Item {
            id: id.clone(),
            thread_id: self.thread.id.clone(),
            turn_id: turn_id.to_owned(),
            ordinal,
            status,
            started_at: at.clone(),
            ended_at: status.is_terminal().then_some(at),
            kind,
        });
        self.touch_item(&id, true);
        id
    }

    /// Adds an item to the live turn (starting one when omp works on its
    /// own).
    fn push_live_item(&mut self, status: ItemStatus, kind: ItemKind) -> String {
        let turn_id = self.ensure_live();
        self.push_item(&turn_id, status, kind)
    }

    /// Changes an item structurally; a terminal status sets `endedAt`.
    fn update_item(&mut self, id: &str, urgent: bool, update: impl FnOnce(&mut Item)) {
        let Some(item) = self.item_mut(id) else {
            return;
        };
        let before = item.clone();
        update(item);
        if item.status.is_terminal() && item.ended_at.is_none() {
            item.ended_at = Some(now_iso());
        }
        if *item != before {
            self.touch_item(id, urgent);
        }
    }

    fn update_turn(&mut self, id: &str, update: impl FnOnce(&mut Turn)) {
        if let Some(turn) = self.turn_mut(id) {
            update(turn);
            self.touch_turn(id);
        }
    }

    // -----------------------------------------------------------------------
    // Thread metadata
    // -----------------------------------------------------------------------

    pub fn archive(&mut self) {
        if self.thread.archived_at.is_none() {
            self.thread.archived_at = Some(now_iso());
        }
        self.touch_command();
    }

    pub fn unarchive(&mut self) {
        self.thread.archived_at = None;
        self.touch_command();
    }

    pub fn rename(&mut self, title: &str) -> Result<(), Error> {
        let title = title.trim();
        if title.is_empty() {
            return Err(Error::rejected("The title can't be empty."));
        }
        title.clone_into(&mut self.thread.title);
        self.touch_command();
        Ok(())
    }

    pub fn set_mode(&mut self, mode: ThreadMode) {
        self.thread.mode = mode;
        self.touch_command();
    }

    /// The effective model of the next turn.
    pub fn set_model(&mut self, model: &ModelRef) {
        if self.thread.model != *model {
            self.thread.model = model.clone();
            self.touch_row();
        }
    }

    // -----------------------------------------------------------------------
    // Turns and the queue
    // -----------------------------------------------------------------------

    /// Starts a turn for `input`; the actor then prompts omp. Returns the
    /// turn id.
    pub fn start_turn(&mut self, input: &UserInput) -> String {
        self.set_model(&input.model);
        let turn_id = self.new_turn(input.model.clone(), None);
        self.push_item(
            &turn_id,
            ItemStatus::Completed,
            ItemKind::User(UserItem {
                text: input.text.clone(),
                attachments: input.attachments.clone(),
                delivery: Delivery::Prompt,
            }),
        );
        self.live = Some(Live::new(turn_id.clone()));
        turn_id
    }

    fn new_turn(&mut self, model: ModelRef, woken_by: Option<WokenBy>) -> String {
        let ordinal = self.turns.last().map_or(1, |turn| turn.ordinal + 1);
        let id = format!("turn_{ordinal}");
        self.turns.push(Turn {
            id: id.clone(),
            thread_id: self.thread.id.clone(),
            ordinal,
            status: TurnStatus::Running,
            model,
            requested_at: now_iso(),
            started_at: None,
            ended_at: None,
            error: None,
            woken_by,
        });
        self.touch_turn(&id);
        id
    }

    /// Queues `input` behind the active turn. Returns the entry id.
    pub fn enqueue(&mut self, input: &UserInput) -> String {
        let id = format!("queue_{}", random_hex(12));
        let held = self.queue.iter().any(|entry| entry.held);
        self.queue.push(QueueEntry {
            id: id.clone(),
            thread_id: self.thread.id.clone(),
            position: self.queue.len() as u32,
            text: input.text.clone(),
            attachments: input.attachments.clone(),
            model: input.model.clone(),
            held,
            created_at: now_iso(),
        });
        self.touch_queue();
        id
    }

    fn renumber_queue(&mut self) {
        for (position, entry) in self.queue.iter_mut().enumerate() {
            entry.position = position as u32;
        }
        self.touch_queue();
    }

    fn queued_index(&self, entry_id: &str) -> Result<usize, Error> {
        self.queue
            .iter()
            .position(|entry| entry.id == entry_id)
            .ok_or_else(|| conflict("That message is no longer queued."))
    }

    /// Moves queue entry `entry_id` to the front.
    pub fn queue_first(&mut self, entry_id: &str) -> Result<(), Error> {
        let index = self.queued_index(entry_id)?;
        let entry = self.queue.remove(index);
        self.queue.insert(0, entry);
        self.renumber_queue();
        Ok(())
    }

    /// Removes queue entry `entry_id`; its input.
    fn take_queued(&mut self, entry_id: &str) -> Result<UserInput, Error> {
        let index = self.queued_index(entry_id)?;
        let entry = self.queue.remove(index);
        self.renumber_queue();
        Ok(UserInput {
            text: entry.text,
            attachments: entry.attachments,
            model: entry.model,
        })
    }

    pub fn queued_input(&self, entry_id: &str) -> Result<UserInput, Error> {
        let entry = &self.queue[self.queued_index(entry_id)?];
        Ok(UserInput {
            text: entry.text.clone(),
            attachments: entry.attachments.clone(),
            model: entry.model.clone(),
        })
    }

    /// The first queue entry that may start now.
    pub fn next_startable(&self) -> Option<String> {
        if !self.is_idle() {
            return None;
        }
        self.queue
            .first()
            .filter(|entry| !entry.held)
            .map(|entry| entry.id.clone())
    }

    /// Starts queue entry `entry_id` as a turn. Returns what to send omp.
    pub fn begin_queued(&mut self, entry_id: &str) -> Result<UserInput, Error> {
        let input = self.take_queued(entry_id)?;
        self.start_turn(&input);
        self.on_user_message();
        Ok(input)
    }

    pub fn cancel_queued(&mut self, entry_id: &str) -> Result<(), Error> {
        self.take_queued(entry_id).map(drop)
    }

    /// Edits a queued message; `attachments: None` keeps them.
    pub fn edit_queued(
        &mut self,
        entry_id: &str,
        text: &str,
        attachments: Option<Vec<Attachment>>,
    ) -> Result<(), Error> {
        let index = self.queued_index(entry_id)?;
        let entry = &mut self.queue[index];
        text.clone_into(&mut entry.text);
        if let Some(attachments) = attachments {
            entry.attachments = attachments;
        }
        self.touch_queue();
        Ok(())
    }

    /// Every queue entry waits for [`Self::resume_queue`].
    pub fn hold_queue(&mut self) {
        if self.queue.iter().any(|entry| !entry.held) {
            for entry in &mut self.queue {
                entry.held = true;
            }
            self.touch_queue();
        }
    }

    pub fn resume_queue(&mut self) {
        if self.queue.iter().any(|entry| entry.held) {
            for entry in &mut self.queue {
                entry.held = false;
            }
            self.touch_queue();
        }
    }

    /// Steers `input` into the running turn. Returns its id.
    pub fn steer(&mut self, input: &UserInput) -> Result<String, Error> {
        let turn_id = match &self.live {
            Some(live) if live.started => live.turn_id.clone(),
            Some(_) => {
                return Err(conflict(
                    "The agent hasn't started yet; queue the message instead.",
                ));
            }
            None => return Err(Error::rejected("Nothing is running to steer into.")),
        };
        self.push_item(
            &turn_id,
            ItemStatus::Completed,
            ItemKind::User(UserItem {
                text: input.text.clone(),
                attachments: input.attachments.clone(),
                delivery: Delivery::Steer,
            }),
        );
        Ok(turn_id)
    }

    /// Turns queue entry `entry_id` into a steer of the running turn.
    pub fn steer_queued(&mut self, entry_id: &str) -> Result<UserInput, Error> {
        let input = self.queued_input(entry_id)?;
        self.steer(&input)?;
        self.take_queued(entry_id)?;
        Ok(input)
    }

    /// A user stop: records the stop item. Returns the active turn.
    pub fn request_stop(&mut self) -> Option<String> {
        let turn_id = self.active_turn()?.id.clone();
        if self.live.as_ref().is_some_and(|live| live.stop.is_some()) {
            return Some(turn_id);
        }
        let item_id = self.push_item(
            &turn_id,
            ItemStatus::Running,
            ItemKind::Stop(StopItem {
                requested_at: now_iso(),
                stopped_at: None,
            }),
        );
        match self.live.as_mut() {
            Some(live) => live.stop = Some(item_id),
            None => {
                let mut live = Live::new(turn_id.clone());
                live.stop = Some(item_id);
                self.live = Some(live);
            }
        }
        Some(turn_id)
    }

    /// The prompt was refused or omp failed before running: the turn fails.
    pub fn fail_turn(&mut self, message: &str, kind: TurnErrorKind) {
        if let Some(live) = self.live.as_mut() {
            live.status = Some(PromptStatus::Error);
            live.error = Some(TurnError {
                message: message.to_owned(),
                kind,
            });
        }
        self.finish_turn();
    }

    /// omp handled the prompt itself (`agentInvoked: false`).
    pub fn complete_without_agent(&mut self) {
        if let Some(live) = self.live.as_mut() {
            live.status = Some(PromptStatus::Completed);
        }
        self.finish_turn();
    }

    /// The omp process is gone (a forced stop or a crash) mid-turn.
    pub fn abandon_turn(&mut self, stopped: bool) {
        if let Some(live) = self.live.as_mut() {
            if stopped {
                live.status = Some(PromptStatus::Aborted);
            } else {
                live.status = Some(PromptStatus::Error);
                live.error.get_or_insert(TurnError {
                    message: "The agent process exited unexpectedly.".into(),
                    kind: TurnErrorKind::Crashed,
                });
            }
        }
        self.finish_turn();
        self.expire_all();
    }

    fn expire_all(&mut self) {
        let open: Vec<String> = self.ui.drain().map(|(id, _)| id).collect();
        for request_id in open {
            self.close_request(&request_id, RequestStatus::Expired, None);
        }
    }

    /// Server restart: nothing survives in omp. An active turn stops, its
    /// open items are cancelled, open requests expire, the queue waits for
    /// `queue.resume`, and running subagents end.
    pub fn recover(&mut self) {
        let at = now_iso();
        if let Some(turn_id) = self.active_turn().map(|turn| turn.id.clone()) {
            self.update_turn(&turn_id, |turn| {
                turn.status = TurnStatus::Stopped;
                turn.ended_at = Some(at.clone());
            });
            self.close_open_items(&turn_id, ItemStatus::Cancelled, true);
        }
        let open: Vec<String> = self
            .open_requests()
            .map(|request| request.id.clone())
            .collect();
        for request_id in open {
            self.close_request(&request_id, RequestStatus::Expired, None);
        }
        self.hold_queue();
        self.live = None;
        self.ui.clear();
        self.session_closed();
    }

    /// Left over from an earlier process: an active turn, an open request or
    /// live agent state.
    pub fn needs_recovery(&self) -> bool {
        self.active_turn().is_some()
            || self.open_requests().next().is_some()
            || self.has_live_agent_state()
    }

    // -----------------------------------------------------------------------
    // Requests
    // -----------------------------------------------------------------------

    fn open_request(&self, request_id: &str) -> Result<&Request, Error> {
        let request = self
            .requests
            .iter()
            .find(|request| request.id == request_id)
            .ok_or_else(|| Error::not_found("request", request_id))?;
        if request.status != RequestStatus::Open || !self.ui.contains_key(request_id) {
            return Err(conflict(
                "This question is no longer waiting for an answer.",
            ));
        }
        Ok(request)
    }

    /// Answers an open request. Returns omp's response.
    pub fn answer(
        &mut self,
        request_id: &str,
        answers: &[Answer],
    ) -> Result<ExtensionUiResponse, Error> {
        self.open_request(request_id)?;
        let pending = self.ui.remove(request_id).expect("checked open");
        let response = ui_response(&pending, answers);
        self.close_request(request_id, RequestStatus::Answered, Some(answers.to_vec()));
        Ok(response)
    }

    /// Dismisses an open request. Returns omp's cancellation.
    pub fn dismiss(&mut self, request_id: &str) -> Result<ExtensionUiResponse, Error> {
        self.open_request(request_id)?;
        let pending = self.ui.remove(request_id).expect("checked open");
        self.close_request(request_id, RequestStatus::Dismissed, None);
        Ok(ExtensionUiResponse::CancelUiResponse(CancelUiResponse {
            id: pending.omp_id,
            cancelled: LitTrue,
            timed_out: None,
        }))
    }

    /// A request's timeout passed. Returns omp's cancellation if it was
    /// still open.
    pub fn expire(&mut self, request_id: &str) -> Option<ExtensionUiResponse> {
        let pending = self.ui.remove(request_id)?;
        self.close_request(request_id, RequestStatus::Expired, None);
        Some(ExtensionUiResponse::CancelUiResponse(CancelUiResponse {
            id: pending.omp_id,
            cancelled: LitTrue,
            timed_out: Some(true),
        }))
    }

    pub fn is_open_request(&self, request_id: &str) -> bool {
        self.ui.contains_key(request_id)
    }

    fn close_request(
        &mut self,
        request_id: &str,
        status: RequestStatus,
        answers: Option<Vec<Answer>>,
    ) {
        let Some(request) = self
            .requests
            .iter_mut()
            .find(|request| request.id == request_id && request.status == RequestStatus::Open)
        else {
            return;
        };
        request.status = status;
        request.closed_at = Some(now_iso());
        request.answers = answers;
        let turn_id = request.turn_id.clone();
        self.touch_request(request_id);
        // Back to running once nothing is open.
        if self.open_requests().next().is_none()
            && self
                .active_turn()
                .is_some_and(|turn| turn.id == turn_id && turn.status == TurnStatus::Waiting)
        {
            self.update_turn(&turn_id, |turn| turn.status = TurnStatus::Running);
        }
    }

    // -----------------------------------------------------------------------
    // Triage (docs/triage.md)
    // -----------------------------------------------------------------------

    /// Marks every turn and item changed, so the next commit writes them all
    /// (a thread rebuilt from its session file).
    pub fn rewrite_all(&mut self) {
        let turns: Vec<String> = self.turns.iter().map(|turn| turn.id.clone()).collect();
        for id in turns {
            self.touch_turn(&id);
        }
        let items: Vec<String> = self.items.iter().map(|item| item.id.clone()).collect();
        for id in items {
            self.touch_item(&id, true);
        }
        self.touch_queue();
        self.touch_command();
    }

    /// Runs triage command `command` on this thread as of now.
    pub fn triage(
        &mut self,
        command: impl FnOnce(&mut Triage, &Facts, i64) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let row = self.row();
        let facts = Facts::of(&row);
        let mut triage = self.thread.triage.clone();
        command(&mut triage, &facts, now_ms())?;
        self.thread.triage = triage;
        self.touch_command();
        Ok(())
    }

    /// A user message was accepted or a queued one started (triage §6.2).
    pub fn on_user_message(&mut self) {
        triage::on_user_message(&mut self.thread.triage, now_ms());
        self.touch_row();
    }

    /// The auto-settle sweep's re-check on live state (triage §8): settles
    /// the thread when it is still due as of `now` and has not changed since
    /// the stored row the sweep saw (`seen_updated_at`). Returns whether it
    /// settled.
    pub fn auto_settle(&mut self, now: i64, after_ms: i64, seen_updated_at: &str) -> bool {
        if self.thread.updated_at != seen_updated_at {
            return false;
        }
        let row = self.row();
        let facts = Facts::of(&row);
        let Some(since) = triage::auto_settle_at(&self.thread.triage, &facts, now, after_ms) else {
            return false;
        };
        let mut triage = self.thread.triage.clone();
        if triage::settle(&mut triage, &facts, now, Some(&iso(since))).is_err() {
            return false;
        }
        self.thread.triage = triage;
        self.touch_command();
        true
    }

    // -----------------------------------------------------------------------
    // omp events
    // -----------------------------------------------------------------------

    /// The live turn, starting one when omp works without a prompt of ours
    /// (a background job woke the agent).
    fn ensure_live(&mut self) -> String {
        if let Some(live) = &self.live {
            return live.turn_id.clone();
        }
        let turn_id = self.new_turn(self.thread.model.clone(), Some(WokenBy::BackgroundJob));
        self.live = Some(Live::new(turn_id.clone()));
        turn_id
    }

    /// `agent_start`: omp is working on the turn.
    fn begin_turn(&mut self) {
        let turn_id = self.ensure_live();
        if let Some(live) = self.live.as_mut() {
            live.started = true;
        }
        if self
            .turns
            .iter()
            .any(|turn| turn.id == turn_id && turn.started_at.is_none())
        {
            self.update_turn(&turn_id, |turn| turn.started_at = Some(now_iso()));
        }
    }

    /// Items of `turn_id` still running end with `status`; text keeps
    /// what it has and completes. Subagents end with their own lifecycle
    /// unless `subagents`.
    fn close_open_items(&mut self, turn_id: &str, status: ItemStatus, subagents: bool) {
        let open: Vec<String> = self
            .items
            .iter()
            .filter(|item| item.turn_id == turn_id && item.status == ItemStatus::Running)
            .filter(|item| subagents || !matches!(item.kind, ItemKind::Subagent(_)))
            .map(|item| item.id.clone())
            .collect();
        for id in open {
            self.update_item(&id, true, |item| {
                item.status = match item.kind {
                    ItemKind::Text(_) | ItemKind::Thinking(_) => ItemStatus::Completed,
                    _ => status,
                };
            });
        }
    }

    /// Ends the live turn per omp's last `prompt_result` (wire.md §7).
    /// Returns whether a turn ended.
    fn finish_turn(&mut self) -> bool {
        let Some(live) = self.live.take() else {
            return false;
        };
        let at = now_iso();
        let status = match live.status {
            Some(PromptStatus::Error) => TurnStatus::Failed,
            Some(PromptStatus::Aborted) => TurnStatus::Stopped,
            _ if live.stop.is_some() => TurnStatus::Stopped,
            _ => TurnStatus::Completed,
        };
        let item_status = if status == TurnStatus::Completed {
            ItemStatus::Completed
        } else {
            ItemStatus::Cancelled
        };
        self.close_open_items(&live.turn_id, item_status, false);
        // A request can't be answered once its turn is over.
        self.expire_all();
        if status == TurnStatus::Stopped {
            match &live.stop {
                Some(item_id) => self.update_item(item_id, true, |item| {
                    item.status = ItemStatus::Completed;
                    if let ItemKind::Stop(stop) = &mut item.kind {
                        stop.stopped_at = Some(at.clone());
                    }
                }),
                None => {
                    self.push_item(
                        &live.turn_id,
                        ItemStatus::Completed,
                        ItemKind::Stop(StopItem {
                            requested_at: at.clone(),
                            stopped_at: Some(at.clone()),
                        }),
                    );
                }
            }
        } else if let Some(item_id) = &live.stop {
            // The turn ended before the stop took effect.
            self.update_item(item_id, true, |item| item.status = ItemStatus::Completed);
        }
        let error = (status == TurnStatus::Failed).then(|| {
            live.error.clone().unwrap_or_else(|| TurnError {
                message: "omp reported an error.".into(),
                kind: TurnErrorKind::Omp,
            })
        });
        self.update_turn(&live.turn_id, |turn| {
            turn.status = status;
            turn.ended_at = Some(at.clone());
            turn.started_at.get_or_insert(at);
            turn.error = error;
        });
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
                    live.error = result.error.as_ref().map(turn_error);
                }
                if (result.session_settled || !result.agent_invoked) && self.finish_turn() {
                    effects.push(Effect::TurnEnded);
                }
            }
            RpcNotification::SessionSettled(_) => {
                // A turn omp has not picked up yet (no agent_start, no
                // prompt_result) is not what settled: this is the trailing
                // settle of the previous turn.
                let picked_up = self
                    .live
                    .as_ref()
                    .is_some_and(|live| live.started || live.status.is_some());
                if picked_up && self.finish_turn() {
                    effects.push(Effect::TurnEnded);
                }
            }
            RpcNotification::ExtensionUiRequest(request) => {
                if let Some(effect) = self.apply_ui_request(request) {
                    effects.push(effect);
                }
            }
            RpcNotification::CommandOutput(output) => {
                self.notice(NoticeLevel::Info, &output.text);
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

    fn notice(&mut self, level: NoticeLevel, text: &str) {
        if !text.trim().is_empty() {
            self.push_live_item(
                ItemStatus::Completed,
                ItemKind::Notice(NoticeItem {
                    level,
                    text: text.to_owned(),
                }),
            );
        }
    }

    fn apply_agent_event(&mut self, event: &RpcAgentEvent, effects: &mut Vec<Effect>) {
        match event {
            RpcAgentEvent::AgentStart(_) => self.begin_turn(),
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
                let args = start
                    .args
                    .clone()
                    .unwrap_or_else(|| Value::Object(Map::new()));
                if let Some(item_id) =
                    self.ensure_tool(&start.tool_call_id, &start.tool_name, &args)
                {
                    // Execution args are final (the streamed call may still
                    // carry host-only keys).
                    let fresh = start
                        .args
                        .as_ref()
                        .map(|args| tools::tool_item(&start.tool_call_id, &start.tool_name, args));
                    let intent = start
                        .intent
                        .clone()
                        .filter(|intent| !intent.trim().is_empty());
                    self.update_item(&item_id, true, |item| {
                        if let ItemKind::Tool(tool) = &mut item.kind {
                            if let Some(fresh) = fresh {
                                tool.input = fresh.input;
                                tool.detail = fresh.detail;
                                tool.intent = fresh.intent.or(tool.intent.take());
                            }
                            if intent.is_some() {
                                tool.intent = intent;
                            }
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
                    update.partial_result.as_ref().and_then(tools::result_text),
                ) {
                    self.tool_progress(&item_id, text);
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
                    ),
                };
                if let Some(item_id) = item_id {
                    let result = end.result.clone().unwrap_or(Value::Null);
                    self.finish_tool(
                        &item_id,
                        &end.tool_name,
                        &result,
                        end.is_error == Some(true),
                        effects,
                    );
                }
            }
            RpcAgentEvent::AutoCompactionStart(_) | RpcAgentEvent::ManualCompactionStart(_) => {
                let item_id = self.push_live_item(
                    ItemStatus::Running,
                    ItemKind::Compaction(CompactionItem::default()),
                );
                if let Some(live) = self.live.as_mut() {
                    live.compaction = Some(item_id);
                }
            }
            RpcAgentEvent::AutoCompactionEnd(end) => self.end_compaction(
                end.aborted,
                end.result
                    .as_ref()
                    .map(|r| (r.summary.clone(), r.tokens_before)),
                end.error_message.is_some() && end.skipped != Some(true),
            ),
            RpcAgentEvent::ManualCompactionEnd(end) => self.end_compaction(
                end.aborted,
                end.result
                    .as_ref()
                    .map(|r| (r.summary.clone(), r.tokens_before)),
                end.error_message.is_some() && end.skipped != Some(true),
            ),
            RpcAgentEvent::AutoRetryStart(retry) => {
                let attempts = if retry.max_attempts > 0 {
                    format!("{}/{}", retry.attempt.max(1), retry.max_attempts)
                } else {
                    retry.attempt.max(1).to_string()
                };
                let message = truncate(
                    &non_blank(&retry.error_message, "The provider request failed."),
                    MAX_ERROR_MESSAGE,
                );
                self.notice(
                    NoticeLevel::Warning,
                    &format!("Retrying (attempt {attempts}): {message}"),
                );
            }
            RpcAgentEvent::Notice(notice) => {
                self.notice(notice_level(notice.level), &notice.message);
            }
            RpcAgentEvent::ModelChanged(_) => effects.push(Effect::RefreshState),
            RpcAgentEvent::ThinkingLevelChanged(changed) => {
                // `configured` is set when an automatic level resolved; the
                // user's choice did not change then.
                if changed.configured.is_none()
                    && let Some(level) = changed.thinking_level.and_then(wire_level)
                {
                    let mut model = self.thread.model.clone();
                    model.thinking = Some(level);
                    self.set_model(&model);
                }
            }
            RpcAgentEvent::GoalUpdated(update) => self.apply_goal(update.goal.as_ref()),
            _ => {}
        }
    }

    /// A partial tool result: its output so far.
    fn tool_progress(&mut self, item_id: &str, text: String) {
        let Some(item) = self.item_mut(item_id) else {
            return;
        };
        let ItemKind::Tool(tool) = &mut item.kind else {
            return;
        };
        let output = tool.output.get_or_insert_with(Default::default);
        match text.strip_prefix(output.text.as_str()) {
            Some(append) if !output.text.is_empty() || !append.is_empty() => {
                let append = append.to_owned();
                output.text = text;
                if !append.is_empty() {
                    self.touch_delta(item_id, DeltaField::Output, &append);
                }
            }
            _ => {
                output.text = text;
                self.touch_item(item_id, false);
            }
        }
    }

    /// omp's tool result for item `item_id`.
    fn finish_tool(
        &mut self,
        item_id: &str,
        name: &str,
        result: &Value,
        failed: bool,
        effects: &mut Vec<Effect>,
    ) {
        let images: Vec<ToolImage> = tools::result_images(result)
            .into_iter()
            .filter_map(|image| {
                Some(ToolImage {
                    attachment_id: crate::attachments::content_id(&image.data)?,
                    mime_type: image.mime_type,
                    data: image.data,
                })
            })
            .collect();
        let status = if failed {
            ItemStatus::Failed
        } else {
            ItemStatus::Completed
        };
        self.update_item(item_id, true, |item| {
            item.status = status;
            match &mut item.kind {
                ItemKind::Todo(todo) => {
                    if let Some(phases) = result.pointer("/details/phases") {
                        todo.phases = tools::todo_phases(phases);
                    }
                }
                ItemKind::Tool(tool) => {
                    tools::apply_result(tool, result);
                    if let Some(output) = &mut tool.output {
                        output.images = images
                            .iter()
                            .map(|image| j3_protocol::v2::entities::ToolImage {
                                attachment_id: image.attachment_id.clone(),
                                mime_type: image.mime_type.clone(),
                            })
                            .collect();
                    }
                }
                _ => {}
            }
        });
        if !images.is_empty() {
            effects.push(Effect::StoreImages(images));
        }
        if name == TASK_TOOL {
            self.spawned_subagents(result);
        }
    }

    fn end_compaction(&mut self, aborted: bool, result: Option<(String, i64)>, failed: bool) {
        let Some(item_id) = self.live.as_mut().and_then(|live| live.compaction.take()) else {
            return;
        };
        self.update_item(&item_id, true, |item| {
            item.status = if aborted {
                ItemStatus::Cancelled
            } else if failed {
                ItemStatus::Failed
            } else {
                ItemStatus::Completed
            };
            if let ItemKind::Compaction(compaction) = &mut item.kind
                && let Some((summary, tokens_before)) = result
            {
                compaction.summary = Some(summary);
                compaction.tokens_before = u64::try_from(tokens_before).ok();
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

    /// The item of one text or thinking block, created on first sight.
    /// Narrated thinking is shown as an answer.
    fn open_block(&mut self, message_id: &str, index: i64, answer: bool) -> String {
        if let Some(id) = self.block_item(message_id, index) {
            return id;
        }
        let body = TextItem {
            text: String::new(),
        };
        let kind = if answer {
            ItemKind::Text(body)
        } else {
            ItemKind::Thinking(body)
        };
        let item_id = self.push_live_item(ItemStatus::Running, kind);
        if let Some(live) = self.live.as_mut() {
            live.blocks
                .insert((message_id.to_owned(), index), item_id.clone());
        }
        item_id
    }

    fn append_block(&mut self, item_id: &str, delta: &str) {
        if delta.is_empty() {
            return;
        }
        let Some(item) = self.item_mut(item_id) else {
            return;
        };
        if let ItemKind::Text(body) | ItemKind::Thinking(body) = &mut item.kind {
            body.text.push_str(delta);
            self.touch_delta(item_id, DeltaField::Text, delta);
        }
    }

    /// Completes a block; `text` (omp's final content) replaces the
    /// streamed text when it differs.
    fn finish_block(&mut self, item_id: &str, text: Option<&str>) {
        self.update_item(item_id, true, |item| {
            item.status = ItemStatus::Completed;
            if let (ItemKind::Text(body) | ItemKind::Thinking(body), Some(text)) =
                (&mut item.kind, text)
                && body.text != text
            {
                text.clone_into(&mut body.text);
            }
        });
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
            AssistantContent::ToolCall(call) => self.ensure_tool_call(call),
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
                .item(&item_id)
                .is_some_and(|item| !item.status.is_terminal())
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
            .map_or_else(|| Value::Object(Map::new()), Value::Object);
        if let Some(item_id) = self.ensure_tool(id, name, &args)
            && let Some(intent) = call
                .intent
                .clone()
                .filter(|intent| !intent.trim().is_empty())
        {
            self.update_item(&item_id, true, |item| {
                if let ItemKind::Tool(tool) = &mut item.kind {
                    tool.intent.get_or_insert(intent);
                }
            });
        }
    }

    /// The item of tool call `call_id`, created on first sight (`ask` has
    /// none: its questions arrive as a UI request).
    fn ensure_tool(&mut self, call_id: &str, name: &str, args: &Value) -> Option<String> {
        if let Some(existing) = self.live.as_ref().and_then(|live| live.tools.get(call_id)) {
            return existing.clone();
        }
        self.ensure_live();
        let item_id = (name != ASK_TOOL).then(|| {
            let kind = if name == TODO_TOOL {
                ItemKind::Todo(TodoItem { phases: Vec::new() })
            } else {
                ItemKind::Tool(tools::tool_item(call_id, name, args))
            };
            self.push_live_item(ItemStatus::Running, kind)
        });
        if let Some(live) = self.live.as_mut() {
            live.tools.insert(call_id.to_owned(), item_id.clone());
        }
        item_id
    }

    // -----------------------------------------------------------------------
    // UI requests
    // -----------------------------------------------------------------------

    fn apply_ui_request(&mut self, request: &ExtensionUiRequest) -> Option<Effect> {
        let single = |text: &str, options: Vec<QuestionOption>, default: Option<String>| Question {
            id: "answer".into(),
            text: text.to_owned(),
            header: None,
            options,
            multiple: false,
            allow_other: false,
            default,
        };
        let (omp_id, method, kind, title, questions, timeout) = match request {
            ExtensionUiRequest::Ask(ask) => (
                ask.id.clone(),
                UiMethod::Ask(ask.questions.clone()),
                RequestKind::Ask,
                None,
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
                    .map(|(index, label)| QuestionOption {
                        label: label.clone(),
                        description: details
                            .get(index)
                            .and_then(|detail| detail.description.clone())
                            .filter(|description| !description.trim().is_empty()),
                    })
                    .collect();
                (
                    select.id.clone(),
                    UiMethod::Select,
                    RequestKind::Select,
                    Some(non_blank(&select.title, "Choose an option")),
                    vec![single(
                        &non_blank(&select.title, "Choose an option"),
                        options,
                        None,
                    )],
                    select.timeout,
                )
            }
            ExtensionUiRequest::Confirm(confirm) => {
                let options = ["Yes", "No"]
                    .into_iter()
                    .map(|label| QuestionOption {
                        label: label.into(),
                        description: None,
                    })
                    .collect();
                let title = non_blank(&confirm.title, "Confirm");
                (
                    confirm.id.clone(),
                    UiMethod::Confirm,
                    RequestKind::Confirm,
                    Some(title.clone()),
                    vec![single(&non_blank(&confirm.message, &title), options, None)],
                    confirm.timeout,
                )
            }
            ExtensionUiRequest::Input(input) => {
                let title = non_blank(&input.title, "Enter a value");
                (
                    input.id.clone(),
                    UiMethod::Value,
                    RequestKind::Input,
                    Some(title.clone()),
                    vec![single(&title, Vec::new(), None)],
                    input.timeout,
                )
            }
            ExtensionUiRequest::Editor(editor) => {
                let title = non_blank(&editor.title, "Edit the text");
                (
                    editor.id.clone(),
                    UiMethod::Value,
                    RequestKind::Editor,
                    Some(title.clone()),
                    vec![single(&title, Vec::new(), editor.prefill.clone())],
                    None,
                )
            }
            ExtensionUiRequest::Cancel(cancel) => {
                let request_id = self
                    .ui
                    .iter()
                    .find(|(_, pending)| pending.omp_id == cancel.target_id)
                    .map(|(request_id, _)| request_id.clone())?;
                self.ui.remove(&request_id);
                self.close_request(&request_id, RequestStatus::Dismissed, None);
                return None;
            }
            ExtensionUiRequest::Notify(notify) => {
                let level = notify.notify_type.map_or(NoticeLevel::Info, notice_level);
                self.notice(level, &notify.message);
                return None;
            }
            _ => return None,
        };
        let turn_id = self.ensure_live();
        let request_id = format!("req_{}", self.requests.len() + 1);
        let opened = now_ms();
        let timeout = timeout.filter(|ms| *ms > 0);
        self.requests.push(Request {
            id: request_id.clone(),
            thread_id: self.thread.id.clone(),
            turn_id: turn_id.clone(),
            kind,
            status: RequestStatus::Open,
            title,
            questions,
            answers: None,
            opened_at: iso(opened),
            expires_at: timeout.map(|ms| iso(opened + ms)),
            closed_at: None,
        });
        self.touch_request(&request_id);
        self.ui
            .insert(request_id.clone(), PendingUi { omp_id, method });
        if self
            .active_turn()
            .is_some_and(|turn| turn.id == turn_id && turn.status == TurnStatus::Running)
        {
            self.update_turn(&turn_id, |turn| turn.status = TurnStatus::Waiting);
        }
        timeout.map(|ms| Effect::ExpireAfter {
            request_id,
            after_ms: ms as u64,
        })
    }

    // -----------------------------------------------------------------------
    // State refresh
    // -----------------------------------------------------------------------

    /// omp's `get_state`: the effective model and the goal.
    pub fn apply_state(&mut self, state: &SessionState) {
        self.apply_goal_state(state.goal.as_ref());
        if let Some(model) = &state.model {
            let mut current = self.thread.model.clone();
            current.provider.clone_from(&model.provider);
            current.id.clone_from(&model.id);
            if let Some(level) = state.thinking_level.and_then(wire_level) {
                current.thinking = Some(level);
            }
            self.set_model(&current);
        }
    }
}

fn notice_level(level: NotifyType) -> NoticeLevel {
    match level {
        NotifyType::Info => NoticeLevel::Info,
        NotifyType::Warning => NoticeLevel::Warning,
        NotifyType::Error => NoticeLevel::Error,
    }
}

/// omp `ask` question → request question; omp's recommended option is the
/// default.
fn ask_question(index: usize, question: &AskQuestion) -> Question {
    let options: Vec<QuestionOption> = question
        .options
        .iter()
        .filter(|option| !option.label.trim().is_empty())
        .map(|option| QuestionOption {
            label: option.label.clone(),
            description: option
                .description
                .clone()
                .filter(|description| !description.trim().is_empty()),
        })
        .collect();
    let default = question
        .recommended
        .and_then(|index| usize::try_from(index).ok())
        .and_then(|index| question.options.get(index))
        .map(|option| option.label.clone());
    Question {
        id: non_blank(&question.id, &format!("q{}", index + 1)),
        text: non_blank(&question.question, "Question"),
        header: question
            .header
            .clone()
            .filter(|header| !header.trim().is_empty()),
        options,
        multiple: question.multi,
        allow_other: true,
        default,
    }
}

/// The answer to question `id`: chosen values plus the typed `other`.
fn answer_values<'a>(answers: &'a [Answer], id: &str) -> Option<&'a Answer> {
    answers.iter().find(|answer| answer.question_id == id)
}

/// A request's answers → omp's `extension_ui_response` (wire.md §5.6).
fn ui_response(pending: &PendingUi, answers: &[Answer]) -> ExtensionUiResponse {
    let id = pending.omp_id.clone();
    let single = || {
        answer_values(answers, "answer")
            .or(answers.first())
            .and_then(|answer| {
                answer
                    .values
                    .first()
                    .cloned()
                    .or_else(|| answer.other.clone())
            })
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
                    let answer = answer_values(answers, &key);
                    let (selected, mut custom): (Vec<String>, Vec<String>) = answer
                        .map(|answer| answer.values.clone())
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|value| !value.trim().is_empty())
                        .partition(|value| question.options.iter().any(|o| &o.label == value));
                    custom.extend(
                        answer
                            .and_then(|answer| answer.other.clone())
                            .filter(|other| !other.trim().is_empty()),
                    );
                    let custom = (!custom.is_empty()).then(|| custom.join(", "));
                    let selected = if question.multi {
                        selected
                    } else {
                        selected.into_iter().take(1).collect()
                    };
                    let custom = if question.multi || selected.is_empty() {
                        custom
                    } else {
                        None
                    };
                    AskAnswer {
                        id: question.id.clone(),
                        selected_options: selected,
                        custom_input: custom,
                    }
                })
                .collect(),
        }),
        UiMethod::Select | UiMethod::Value => {
            ExtensionUiResponse::ValueUiResponse(ValueUiResponse {
                id,
                value: single(),
            })
        }
        UiMethod::Confirm => ExtensionUiResponse::ConfirmUiResponse(ConfirmUiResponse {
            id,
            confirmed: single() == "Yes",
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

    fn model() -> ModelRef {
        ModelRef {
            provider: "p".into(),
            id: "m".into(),
            thinking: None,
        }
    }

    fn thread() -> ThreadModel {
        ThreadModel::create(NewThread {
            id: "t".into(),
            project_id: "p".into(),
            title: String::new(),
            model: model(),
            mode: ThreadMode::Default,
        })
    }

    #[test]
    fn streamed_text_is_sent_as_deltas_after_the_item() {
        let mut thread = thread();
        thread.start_turn(&UserInput {
            text: "hi".into(),
            attachments: Vec::new(),
            model: model(),
        });
        let first = thread.drain();
        assert!(first.row.is_some(), "a new thread announces its row");
        thread.begin_turn();
        let item = thread.open_block("m1", 0, true);
        thread.append_block(&item, "Hel");
        let opened = thread.drain();
        assert!(opened.items.iter().any(|item| item["type"] == "item"));
        thread.append_block(&item, "lo ");
        thread.append_block(&item, "there");
        assert!(!thread.has_urgent(), "text growth may wait for the window");
        let deltas = thread.drain();
        let delta: Vec<_> = deltas
            .items
            .iter()
            .filter(|item| item["type"] == "item.delta")
            .collect();
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0]["append"], "lo there");
        assert_eq!(delta[0]["field"], "text");
        assert_eq!(deltas.records.len(), 1, "the stored item stays whole");
        thread.finish_block(&item, Some("Hello there"));
        let done = thread.drain();
        assert!(done.items.iter().any(|item| item["type"] == "item"
            && item["item"]["status"] == "completed"
            && item["item"]["text"] == "Hello there"));
    }

    #[test]
    fn requests_wait_and_answers_map_to_omp() {
        let mut thread = thread();
        thread.start_turn(&UserInput {
            text: "hi".into(),
            attachments: Vec::new(),
            model: model(),
        });
        thread.begin_turn();
        let request = ExtensionUiRequest::Confirm(omp_rpc::ConfirmUiRequest {
            id: "ui".into(),
            title: "Deploy?".into(),
            message: "Ship it?".into(),
            timeout: Some(1000),
        });
        let effect = thread.apply_ui_request(&request);
        assert!(matches!(
            effect,
            Some(Effect::ExpireAfter { after_ms: 1000, .. })
        ));
        assert_eq!(thread.status(), ThreadStatus::Waiting);
        let request = thread.requests[0].clone();
        assert_eq!(request.kind, RequestKind::Confirm);
        assert!(request.expires_at.is_some());
        let response = thread
            .answer(
                &request.id,
                &[Answer {
                    question_id: "answer".into(),
                    values: vec!["Yes".into()],
                    other: None,
                }],
            )
            .unwrap();
        assert!(matches!(
            response,
            ExtensionUiResponse::ConfirmUiResponse(ConfirmUiResponse {
                confirmed: true,
                ..
            })
        ));
        assert_eq!(thread.status(), ThreadStatus::Running);
        let error = thread.dismiss(&request.id).unwrap_err();
        assert_eq!(error.code, ErrorCode::Conflict);
    }
}
