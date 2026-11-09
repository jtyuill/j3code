//! Rebuilds a thread's turns and items from its omp session file, the
//! conversation's source of truth. Store migration 2 runs it once per
//! thread (`crate::store`), since the first protocol's stored items can't be
//! read any more.
//!
//! The session file is a tree: every entry names its `parentId`, and the
//! conversation is the chain from the last entry back to the root.
//! Branches omp left behind are not part of the thread.
//!
//! What only the old store held is not recovered: user messages that never
//! reached omp (cancelled queue entries), who pressed stop, and the live
//! one-line progress of subagents.

use std::collections::HashMap;

use j3_protocol::v2::entities::{
    Attachment, Delivery, Item, ItemKind, ItemStatus, JobState, ModelRef, StopItem, TextItem,
    ThinkingLevel, TodoItem, Turn, TurnError, TurnErrorKind, TurnStatus, UserItem, WokenBy,
};
use serde_json::Value;

use super::tools::{self, ResultImage};

/// omp's ask tool; its questions are requests, not feed items.
const ASK_TOOL: &str = "ask";
const TODO_TOOL: &str = "todo";
const TASK_TOOL: &str = "task";
/// omp's `wait` tool reports background jobs that ended while it waited.
const WAIT_TOOL: &str = "wait";

/// A thread rebuilt from its session.
#[derive(Debug, Default)]
pub struct Replay {
    pub turns: Vec<Turn>,
    pub items: Vec<Item>,
    /// The bytes of the images the items reference by attachment id (tool
    /// output images and user images), to be written to the attachment store.
    pub images: Vec<(Attachment, Vec<u8>)>,
    /// The model of the last `model_change`/`thinking_level_change`.
    pub model: Option<ModelRef>,
    /// omp's session title, if it set one.
    pub title: Option<String>,
}

/// Replays the lines of a session file. Lines that are not JSON objects are
/// skipped (a line omp is still writing, or damage). `image_bytes` reads
/// omp's image data (inline base64 or a blob reference); images it can't
/// read are left out.
pub fn replay<'a>(
    thread_id: &str,
    lines: impl IntoIterator<Item = &'a str>,
    image_bytes: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Replay {
    let mut title = None;
    let mut entries: Vec<Value> = Vec::new();
    for line in lines {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match entry.get("type").and_then(Value::as_str) {
            Some("title") => {
                title = entry
                    .get("title")
                    .and_then(Value::as_str)
                    .filter(|title| !title.trim().is_empty())
                    .map(str::to_owned);
            }
            Some("session") => {}
            _ if entry.get("id").and_then(Value::as_str).is_some() => entries.push(entry),
            _ => {}
        }
    }
    let mut builder = Builder::new(thread_id, image_bytes);
    for entry in active_branch(&entries) {
        builder.entry(entry);
    }
    let mut replay = builder.finish();
    replay.title = title;
    replay
}

/// The entries from the root to the last entry, following `parentId`.
fn active_branch(entries: &[Value]) -> Vec<&Value> {
    let by_id: HashMap<&str, &Value> = entries
        .iter()
        .filter_map(|entry| Some((entry.get("id")?.as_str()?, entry)))
        .collect();
    let mut chain = Vec::new();
    let mut cursor = entries.last();
    while let Some(entry) = cursor {
        // A cycle would mean a damaged file; stop rather than loop.
        if chain.len() > entries.len() {
            break;
        }
        chain.push(entry);
        cursor = entry
            .get("parentId")
            .and_then(Value::as_str)
            .and_then(|parent| by_id.get(parent).copied());
    }
    chain.reverse();
    chain
}

struct Builder<'a> {
    thread_id: &'a str,
    image_bytes: &'a dyn Fn(&str) -> Option<Vec<u8>>,
    turns: Vec<Turn>,
    items: Vec<Item>,
    images: Vec<(Attachment, Vec<u8>)>,
    model: Option<ModelRef>,
    /// The turn still open: omp has not ended it with a final stop.
    open: bool,
    /// Tool call id → index into `items`.
    calls: HashMap<String, usize>,
    /// Background job id → index into `items` (bash jobs and subagents).
    jobs: HashMap<String, usize>,
    last_at: String,
}

impl<'a> Builder<'a> {
    /// Keeps an image under its content id; None when its bytes can't be
    /// read.
    fn keep_image(&mut self, image: ResultImage, name: String) -> Option<Attachment> {
        let id = crate::attachments::content_id(&image.data)?;
        let bytes = (self.image_bytes)(&image.data)?;
        let attachment = Attachment {
            id,
            name,
            mime_type: image.mime_type,
            size: bytes.len() as u64,
        };
        self.images.push((attachment.clone(), bytes));
        Some(attachment)
    }

    fn new(thread_id: &'a str, image_bytes: &'a dyn Fn(&str) -> Option<Vec<u8>>) -> Self {
        Self {
            thread_id,
            image_bytes,
            turns: Vec::new(),
            items: Vec::new(),
            images: Vec::new(),
            model: None,
            open: false,
            calls: HashMap::new(),
            jobs: HashMap::new(),
            last_at: String::new(),
        }
    }

    fn entry(&mut self, entry: &Value) {
        let at = entry
            .get("timestamp")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        match entry.get("type").and_then(Value::as_str) {
            Some("model_change") => {
                if let Some((provider, id)) = entry
                    .get("model")
                    .and_then(Value::as_str)
                    .and_then(|slug| slug.split_once('/'))
                {
                    let thinking = self.model.as_ref().and_then(|model| model.thinking);
                    self.model = Some(ModelRef {
                        provider: provider.to_owned(),
                        id: id.to_owned(),
                        thinking,
                    });
                }
            }
            Some("thinking_level_change") => {
                let level = entry
                    .get("thinkingLevel")
                    .and_then(|level| serde_json::from_value::<ThinkingLevel>(level.clone()).ok());
                if let Some(model) = &mut self.model {
                    model.thinking = level;
                }
            }
            Some("message") => {
                if let Some(message) = entry.get("message") {
                    self.message(message, &at);
                }
            }
            Some("custom_message")
                if entry.get("customType").and_then(Value::as_str) == Some("async-result") =>
            {
                self.async_result(entry, &at);
            }
            _ => {}
        }
        if !at.is_empty() {
            self.last_at = at;
        }
    }

    fn message(&mut self, message: &Value, at: &str) {
        match message.get("role").and_then(Value::as_str) {
            Some("user") => self.user(message, at),
            Some("assistant") => self.assistant(message, at),
            Some("toolResult") => self.tool_result(message, at),
            _ => {}
        }
    }

    fn user(&mut self, message: &Value, at: &str) {
        let steer = self.open && message.get("steering").and_then(Value::as_bool) == Some(true);
        if !steer {
            self.start_turn(at, None);
        }
        let (text, images) = user_content(message.get("content"));
        let attachments = images
            .into_iter()
            .enumerate()
            .filter_map(|(index, image)| {
                let name = format!("image-{}.{}", index + 1, extension(&image.mime_type));
                self.keep_image(image, name)
            })
            .collect();
        self.push(
            at,
            ItemStatus::Completed,
            ItemKind::User(UserItem {
                text,
                attachments,
                delivery: if steer {
                    Delivery::Steer
                } else {
                    Delivery::Prompt
                },
            }),
        );
    }

    fn assistant(&mut self, message: &Value, at: &str) {
        if !self.open {
            // omp woke without a prompt: a background job delivered its
            // result after the user's turn had ended.
            self.start_turn(at, Some(WokenBy::BackgroundJob));
        }
        for block in message
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match block.get("type").and_then(Value::as_str) {
                Some("thinking") => {
                    let text = block
                        .get("thinking")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if !text.trim().is_empty() {
                        let kind = ItemKind::Thinking(TextItem {
                            text: text.to_owned(),
                        });
                        self.push(at, ItemStatus::Completed, kind);
                    }
                }
                Some("text") => {
                    let text = block
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if !text.trim().is_empty() {
                        let kind = ItemKind::Text(TextItem {
                            text: text.to_owned(),
                        });
                        self.push(at, ItemStatus::Completed, kind);
                    }
                }
                Some("toolCall") => self.tool_call(block, at),
                _ => {}
            }
        }
        match message.get("stopReason").and_then(Value::as_str) {
            Some("toolUse") => {}
            Some("aborted") => {
                self.push(
                    at,
                    ItemStatus::Completed,
                    ItemKind::Stop(StopItem {
                        requested_at: at.to_owned(),
                        stopped_at: Some(at.to_owned()),
                    }),
                );
                self.end_turn(at, TurnStatus::Stopped, None);
            }
            Some("error") => {
                let text = message
                    .get("errorMessage")
                    .and_then(Value::as_str)
                    .unwrap_or("omp reported an error.");
                let error = TurnError {
                    message: text.to_owned(),
                    kind: error_kind(text),
                };
                self.end_turn(at, TurnStatus::Failed, Some(error));
            }
            _ => self.end_turn(at, TurnStatus::Completed, None),
        }
    }

    fn tool_call(&mut self, block: &Value, at: &str) {
        let (Some(id), Some(name)) = (
            block.get("id").and_then(Value::as_str),
            block.get("name").and_then(Value::as_str),
        ) else {
            return;
        };
        if name == ASK_TOOL {
            return;
        }
        let args = block.get("arguments").cloned().unwrap_or(Value::Null);
        let kind = if name == TODO_TOOL {
            ItemKind::Todo(TodoItem { phases: Vec::new() })
        } else {
            ItemKind::Tool(tools::tool_item(id, name, &args))
        };
        let index = self.push(at, ItemStatus::Running, kind);
        self.calls.insert(id.to_owned(), index);
    }

    fn tool_result(&mut self, message: &Value, at: &str) {
        let Some(&index) = message
            .get("toolCallId")
            .and_then(Value::as_str)
            .and_then(|id| self.calls.get(id))
        else {
            return;
        };
        let failed = message.get("isError").and_then(Value::as_bool) == Some(true);
        let images: Vec<_> = tools::result_images(message)
            .into_iter()
            .filter_map(|image| {
                let name = format!("output.{}", extension(&image.mime_type));
                self.keep_image(image, name)
            })
            .collect();
        let mut subagents = Vec::new();
        let mut waited = false;
        let item = &mut self.items[index];
        item.status = if failed {
            ItemStatus::Failed
        } else {
            ItemStatus::Completed
        };
        item.ended_at = Some(at.to_owned());
        match &mut item.kind {
            ItemKind::Todo(todo) => {
                if let Some(phases) = message.pointer("/details/phases") {
                    todo.phases = tools::todo_phases(phases);
                }
            }
            ItemKind::Tool(tool) => {
                tools::apply_result(tool, message);
                if let Some(output) = &mut tool.output {
                    output.images = images
                        .iter()
                        .map(|attachment| j3_protocol::v2::entities::ToolImage {
                            attachment_id: attachment.id.clone(),
                            mime_type: attachment.mime_type.clone(),
                        })
                        .collect();
                    if let Some(job) = &output.background {
                        self.jobs.insert(job.job_id.clone(), index);
                    }
                }
                if tool.name == TASK_TOOL {
                    subagents = tools::spawned_subagents(message);
                }
                waited = tool.name == WAIT_TOOL;
            }
            _ => {}
        }
        for subagent in subagents {
            let id = subagent.subagent_id.clone();
            let index = self.push(at, ItemStatus::Running, ItemKind::Subagent(subagent));
            self.jobs.insert(id, index);
        }
        if waited {
            let text = tools::result_text(message).unwrap_or_default();
            for (job_id, state) in ended_jobs(message.pointer("/details/jobs"), "id") {
                let output = wait_section(&text, &job_id);
                self.deliver(&job_id, state, output, &text, at);
            }
        }
    }

    /// omp delivered background job results without a tool call.
    fn async_result(&mut self, entry: &Value, at: &str) {
        let content = entry
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default();
        for (job_id, state) in ended_jobs(entry.pointer("/details/jobs"), "jobId") {
            self.deliver(&job_id, state, job_output(content), content, at);
        }
    }

    /// Background job `job_id` ended: a backgrounded tool call gets its final
    /// `output`, a subagent the `<task-result>` answer found in `text`.
    fn deliver(&mut self, job_id: &str, state: JobState, output: String, text: &str, at: &str) {
        let Some(&index) = self.jobs.get(job_id) else {
            return;
        };
        let item = &mut self.items[index];
        match &mut item.kind {
            ItemKind::Tool(tool) => tools::finish_job(tool, state, output),
            ItemKind::Subagent(subagent) if item.status == ItemStatus::Running => {
                subagent.result = task_output(text, job_id);
                item.status = item_status(state);
                item.ended_at = Some(at.to_owned());
            }
            _ => {}
        }
    }

    fn start_turn(&mut self, at: &str, woken_by: Option<WokenBy>) {
        if self.open {
            // A prompt omp never answered with a final stop (crash, kill).
            let at = self.last_at.clone();
            self.end_turn(&at, TurnStatus::Stopped, None);
        }
        let ordinal = self.turns.len() as u32 + 1;
        self.turns.push(Turn {
            id: format!("turn_{ordinal}"),
            thread_id: self.thread_id.to_owned(),
            ordinal,
            status: TurnStatus::Running,
            model: self.model.clone().unwrap_or_else(|| ModelRef {
                provider: String::new(),
                id: String::new(),
                thinking: None,
            }),
            requested_at: at.to_owned(),
            started_at: Some(at.to_owned()),
            ended_at: None,
            error: None,
            woken_by,
        });
        self.open = true;
    }

    fn end_turn(&mut self, at: &str, status: TurnStatus, error: Option<TurnError>) {
        let Some(turn) = self.turns.last_mut() else {
            return;
        };
        turn.status = status;
        turn.ended_at = Some(at.to_owned());
        turn.error = error;
        self.open = false;
        let turn_id = turn.id.clone();
        let cancelled = status != TurnStatus::Completed;
        for item in self
            .items
            .iter_mut()
            .rev()
            .take_while(|item| item.turn_id == turn_id)
        {
            if item.status == ItemStatus::Running && !matches!(item.kind, ItemKind::Subagent(_)) {
                item.status = if cancelled {
                    ItemStatus::Cancelled
                } else {
                    ItemStatus::Completed
                };
                item.ended_at = Some(at.to_owned());
            }
        }
    }

    fn push(&mut self, at: &str, status: ItemStatus, kind: ItemKind) -> usize {
        if self.turns.is_empty() {
            self.start_turn(at, None);
        }
        let ordinal = self.items.len() as u64 + 1;
        let turn_id = self
            .turns
            .last()
            .map(|turn| turn.id.clone())
            .unwrap_or_default();
        self.items.push(Item {
            id: format!("item_{ordinal}"),
            thread_id: self.thread_id.to_owned(),
            turn_id,
            ordinal,
            status,
            started_at: at.to_owned(),
            ended_at: status.is_terminal().then(|| at.to_owned()),
            kind,
        });
        self.items.len() - 1
    }

    fn finish(mut self) -> Replay {
        if self.open {
            // The session ends mid-turn: omp was stopped or is still running.
            // A live thread's actor takes over from here.
            let at = self.last_at.clone();
            self.end_turn(&at, TurnStatus::Stopped, None);
        }
        Replay {
            turns: self.turns,
            items: self.items,
            images: self.images,
            model: self.model,
            title: None,
        }
    }
}

/// The text blocks of a user message, one per paragraph, and its images.
fn user_content(content: Option<&Value>) -> (String, Vec<ResultImage>) {
    match content {
        Some(Value::String(text)) => (text.clone(), Vec::new()),
        Some(Value::Array(blocks)) => {
            let text = blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n\n");
            let images = tools::result_images(&serde_json::json!({ "content": blocks }));
            (text, images)
        }
        _ => (String::new(), Vec::new()),
    }
}

/// `(id, state)` of the jobs in an omp job list that have ended; `key` is
/// the id field (`jobId` in deliveries, `id` in `wait` results).
fn ended_jobs(jobs: Option<&Value>, key: &str) -> Vec<(String, JobState)> {
    jobs.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|job| {
            let id = job.get(key)?.as_str()?;
            let state = tools::job_state(job.get("status")?.as_str()?)?;
            (state != JobState::Running).then(|| (id.to_owned(), state))
        })
        .collect()
}

/// Job `id`'s part of a `wait` result: from its `### id [type]` heading to
/// the next heading.
fn wait_section(text: &str, id: &str) -> String {
    let heading = format!("### {id} [");
    let Some(start) = text.find(&heading) else {
        return String::new();
    };
    let body = &text[start..];
    let body = body.split_once('\n').map_or("", |(_, rest)| rest);
    let end = body
        .match_indices("\n#")
        .map(|(at, _)| at)
        .find(|&at| body[at + 1..].starts_with("## ") || body[at + 1..].starts_with("### "))
        .unwrap_or(body.len());
    body[..end].trim().to_owned()
}

/// The body of a background job's delivery: the text between omp's
/// `<system-notice>` header line and its closing tag.
fn job_output(content: &str) -> String {
    let body = content
        .trim()
        .strip_prefix("<system-notice>")
        .and_then(|rest| rest.strip_suffix("</system-notice>"))
        .unwrap_or(content)
        .trim_start_matches('\n');
    let body = match body.split_once('\n') {
        Some((first, rest)) if first.starts_with("Background job ") => rest,
        _ => body,
    };
    body.trim_end().to_owned()
}

/// A subagent's answer: the `<output>` of its `<task-result id="…">`.
fn task_output(content: &str, id: &str) -> Option<String> {
    let start = content.find(&format!("<task-result id=\"{id}\""))?;
    let rest = &content[start..];
    let output = rest.find("<output>")? + "<output>".len();
    let end = rest[output..].find("</output>")? + output;
    Some(rest[output..end].trim().to_owned())
}

fn item_status(state: JobState) -> ItemStatus {
    match state {
        JobState::Running => ItemStatus::Running,
        JobState::Completed => ItemStatus::Completed,
        JobState::Failed => ItemStatus::Failed,
        JobState::Cancelled => ItemStatus::Cancelled,
    }
}

/// omp's error text → kind. Usage limits matter to triage ("Limited").
fn error_kind(message: &str) -> TurnErrorKind {
    let lower = message.to_ascii_lowercase();
    if lower.contains("usage limit") || lower.contains("rate limit") || lower.contains("quota") {
        TurnErrorKind::UsageLimit
    } else {
        TurnErrorKind::Provider
    }
}

fn extension(mime_type: &str) -> &str {
    mime_type.rsplit('/').next().unwrap_or("bin")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lines(entries: &[Value]) -> Vec<String> {
        entries.iter().map(Value::to_string).collect()
    }

    fn msg(id: &str, parent: Option<&str>, at: &str, message: Value) -> Value {
        json!({"type": "message", "id": id, "parentId": parent, "timestamp": at, "message": message})
    }

    #[test]
    fn steer_stays_in_its_turn_and_a_new_prompt_starts_the_next() {
        let entries = [
            json!({"type": "model_change", "id": "m", "parentId": null,
                   "timestamp": "T0", "model": "anthropic/claude"}),
            msg(
                "u1",
                Some("m"),
                "T1",
                json!({"role": "user", "content": [{"type": "text", "text": "go"}]}),
            ),
            msg(
                "a1",
                Some("u1"),
                "T2",
                json!({"role": "assistant", "stopReason": "toolUse",
                "content": [{"type": "toolCall", "id": "c1", "name": "bash", "arguments": {"command": "ls"}}]}),
            ),
            msg(
                "r1",
                Some("a1"),
                "T03",
                json!({"role": "toolResult", "toolCallId": "c1",
                "content": [{"type": "text", "text": "a.rs"}], "details": {"exitCode": 0}}),
            ),
            msg(
                "u2",
                Some("r1"),
                "T4",
                json!({"role": "user", "steering": true,
                "content": [{"type": "text", "text": "faster"}]}),
            ),
            msg(
                "a2",
                Some("u2"),
                "T5",
                json!({"role": "assistant", "stopReason": "stop",
                "content": [{"type": "text", "text": "done"}]}),
            ),
            msg(
                "u3",
                Some("a2"),
                "T6",
                json!({"role": "user", "content": [{"type": "text", "text": "next"}]}),
            ),
            msg(
                "a3",
                Some("u3"),
                "T7",
                json!({"role": "assistant", "stopReason": "aborted", "content": []}),
            ),
        ];
        let text = lines(&entries);
        let replay = replay("t", text.iter().map(String::as_str), &|_| None);
        assert_eq!(replay.turns.len(), 2);
        assert_eq!(replay.turns[0].status, TurnStatus::Completed);
        assert_eq!(replay.turns[0].model.id, "claude");
        assert_eq!(replay.turns[1].status, TurnStatus::Stopped);
        let kinds: Vec<_> = replay
            .items
            .iter()
            .map(|item| match &item.kind {
                ItemKind::User(user) => format!("{}:{:?}", item.turn_id, user.delivery),
                ItemKind::Tool(tool) => format!(
                    "{}:tool:{}",
                    item.turn_id,
                    tool.output.as_ref().unwrap().text
                ),
                ItemKind::Text(text) => format!("{}:text:{}", item.turn_id, text.text),
                ItemKind::Stop(_) => format!("{}:stop", item.turn_id),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "turn_1:Prompt",
                "turn_1:tool:a.rs",
                "turn_1:Steer",
                "turn_1:text:done",
                "turn_2:Prompt",
                "turn_2:stop"
            ]
        );
    }

    #[test]
    fn abandoned_branch_is_skipped() {
        let entries = [
            msg(
                "u1",
                None,
                "T1",
                json!({"role": "user", "content": "first"}),
            ),
            msg(
                "a1",
                Some("u1"),
                "T2",
                json!({"role": "assistant", "stopReason": "stop",
                "content": [{"type": "text", "text": "old answer"}]}),
            ),
            msg(
                "a2",
                Some("u1"),
                "T03",
                json!({"role": "assistant", "stopReason": "stop",
                "content": [{"type": "text", "text": "new answer"}]}),
            ),
        ];
        let text = lines(&entries);
        let replay = replay("t", text.iter().map(String::as_str), &|_| None);
        let texts: Vec<_> = replay
            .items
            .iter()
            .filter_map(|item| match &item.kind {
                ItemKind::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["new answer"]);
    }

    #[test]
    fn background_results_complete_jobs_and_subagents() {
        let notice = |id: &str, body: &str| {
            format!(
                "<system-notice>\nBackground job {id} has completed. Resume your work using the result below.\n{body}\n</system-notice>"
            )
        };
        let entries = [
            msg("u1", None, "T1", json!({"role": "user", "content": "go"})),
            msg(
                "a1",
                Some("u1"),
                "T2",
                json!({"role": "assistant", "stopReason": "toolUse", "content": [
                {"type": "toolCall", "id": "c1", "name": "bash", "arguments": {"command": "make"}},
                {"type": "toolCall", "id": "c2", "name": "task", "arguments": {}}]}),
            ),
            msg(
                "r1",
                Some("a1"),
                "T03",
                json!({"role": "toolResult", "toolCallId": "c1",
                "content": [{"type": "text", "text": "Backgrounded as job bg_1"}],
                "details": {"async": {"state": "running", "jobId": "bg_1", "type": "bash"}}}),
            ),
            msg(
                "r2",
                Some("r1"),
                "T03",
                json!({"role": "toolResult", "toolCallId": "c2",
                "content": [{"type": "text", "text": "Spawned"}],
                "details": {"progress": [{"id": "Scout", "agent": "scout", "assignment": "look"}]}}),
            ),
            msg(
                "a2",
                Some("r2"),
                "T4",
                json!({"role": "assistant", "stopReason": "stop",
                "content": [{"type": "text", "text": "waiting"}]}),
            ),
            json!({"type": "custom_message", "customType": "async-result", "id": "j1", "parentId": "a2",
                   "timestamp": "T5", "content": notice("bg_1", "built ok"),
                   "details": {"jobs": [{"jobId": "bg_1", "type": "bash", "status": "completed"}]}}),
            json!({"type": "custom_message", "customType": "async-result", "id": "j2", "parentId": "j1",
                   "timestamp": "T6",
                   "content": notice("Scout", "<task-result id=\"Scout\" agent=\"scout\" status=\"completed\">\n\n<output>\nfound it\n</output>\n</task-result>"),
                   "details": {"jobs": [{"jobId": "Scout", "type": "task", "status": "completed"}]}}),
            msg(
                "a3",
                Some("j2"),
                "T7",
                json!({"role": "assistant", "stopReason": "stop",
                "content": [{"type": "text", "text": "all done"}]}),
            ),
        ];
        let text = lines(&entries);
        let replay = replay("t", text.iter().map(String::as_str), &|_| None);
        let bash = replay.items.iter().find_map(|item| match &item.kind {
            ItemKind::Tool(tool) if tool.name == "bash" => tool.output.clone(),
            _ => None,
        });
        let bash = bash.unwrap();
        assert_eq!(bash.text, "built ok");
        assert_eq!(bash.background.unwrap().state, JobState::Completed);
        let subagent = replay
            .items
            .iter()
            .find(|item| matches!(item.kind, ItemKind::Subagent(_)))
            .unwrap();
        assert_eq!(subagent.status, ItemStatus::Completed);
        let ItemKind::Subagent(subagent) = &subagent.kind else {
            unreachable!()
        };
        assert_eq!(subagent.result.as_deref(), Some("found it"));
        // omp answered the deliveries after the user's turn had ended: that
        // answer is a turn of its own.
        assert_eq!(replay.turns.len(), 2);
        assert_eq!(replay.turns[1].woken_by, Some(WokenBy::BackgroundJob));
    }

    #[test]
    fn images_are_kept_under_content_ids_from_blobs_and_inline_data() {
        use base64::Engine as _;
        let blob = format!("blob:sha256:{}", "ab".repeat(32));
        let inline = base64::engine::general_purpose::STANDARD.encode([7u8, 8, 9]);
        let entries = [
            msg(
                "u1",
                None,
                "T1",
                json!({"role": "user", "content": [
                {"type": "text", "text": "look"},
                {"type": "image", "mimeType": "image/png", "data": blob}]}),
            ),
            msg(
                "a1",
                Some("u1"),
                "T2",
                json!({"role": "assistant", "stopReason": "toolUse",
                "content": [{"type": "toolCall", "id": "c1", "name": "read", "arguments": {}}]}),
            ),
            msg(
                "r1",
                Some("a1"),
                "T03",
                json!({"role": "toolResult", "toolCallId": "c1",
                "content": [{"type": "image", "mimeType": "image/webp", "data": inline}]}),
            ),
            msg(
                "a2",
                Some("r1"),
                "T4",
                json!({"role": "assistant", "stopReason": "stop", "content": []}),
            ),
        ];
        let text = lines(&entries);
        let read = |data: &str| {
            if data.starts_with("blob:") {
                Some(vec![1, 2, 3, 4])
            } else {
                base64::engine::general_purpose::STANDARD.decode(data).ok()
            }
        };
        let replay = replay("t", text.iter().map(String::as_str), &read);
        let ids: Vec<_> = replay
            .images
            .iter()
            .map(|(a, bytes)| (a.id.clone(), a.size, bytes.len()))
            .collect();
        assert_eq!(
            ids,
            [
                (format!("att_{}", "ab".repeat(16)), 4, 4),
                (crate::attachments::content_id(&inline).unwrap(), 3, 3)
            ]
        );
        let ItemKind::User(user) = &replay.items[0].kind else {
            panic!()
        };
        assert_eq!(user.attachments[0].id, replay.images[0].0.id);
        let ItemKind::Tool(tool) = &replay.items[1].kind else {
            panic!()
        };
        assert_eq!(
            tool.output.as_ref().unwrap().images[0].attachment_id,
            replay.images[1].0.id
        );
        // Unreadable images are left out.
        let none = replay_none(&text);
        assert!(none.images.is_empty());
    }

    fn replay_none(text: &[String]) -> Replay {
        replay("t", text.iter().map(String::as_str), &|_| None)
    }
}
