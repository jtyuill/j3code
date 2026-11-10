//! Thread actors: one tokio task per thread owning its [`ThreadModel`], its
//! omp session (started on first need, closed after the thread has been
//! idle a while) and its queue. Commands arrive over a mailbox, omp frames
//! over the session's event channel. Changes are committed through the
//! [`Hub`]; growing text waits up to [`ActorConfig::coalesce`] so it goes
//! out as one `item.delta`.

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use base64::Engine as _;
use j3_protocol::v2::{
    entities::{Answer, Attachment, ThreadMode, TurnErrorKind},
    envelope::{Error, ErrorCode},
    methods::{BtwItem, BtwRecord, BtwStatus, BtwTurn, Delivery},
};
use serde_json::{Value, json};
use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};

use super::{
    model::{Effect, ThreadModel, UserInput, omp_level, rewrite_skill},
    session::{AgentSession, Events, SessionFactory},
    triage,
};
use crate::{
    attachments::AttachmentStore,
    hub::{Commit, Hub, ResultFn, ThreadWrite},
    omp::{
        self,
        omp_rpc::{
            self, AvailableSlashCommand, BtwHistoryRecord, Event, GoalOp, ImageContent, LitImage,
            RpcNotification, SlashCommandSource,
        },
    },
};

/// Called with `(cwd, commands)` whenever a thread's omp reports its slash
/// commands, so the catalog can serve them per directory.
pub type CommandsSink = Arc<dyn Fn(&str, &[AvailableSlashCommand]) + Send + Sync>;

/// Shared by every actor of an app.
#[derive(Clone)]
pub struct ActorConfig {
    pub hub: Arc<Hub>,
    pub factory: Arc<dyn SessionFactory>,
    pub attachments: Arc<AttachmentStore>,
    /// Close omp after the thread has been idle this long.
    pub idle_timeout: Duration,
    /// Coalescing window for growing text.
    pub coalesce: Duration,
    pub commands: Option<CommandsSink>,
}

/// A thread command (wire.md §5.4–§5.9).
#[derive(Debug, Clone)]
pub enum Command {
    Rename(String),
    Archive,
    Unarchive,
    Delete {
        only_if_unused: bool,
    },
    SetMode(ThreadMode),
    /// `input.model` is the thread's when the client named none.
    Send {
        input: UserInput,
        delivery: Delivery,
    },
    Stop {
        hold_queue: bool,
        force: bool,
    },
    Answer {
        request_id: String,
        answers: Vec<Answer>,
    },
    Dismiss {
        request_id: String,
    },
    QueueEdit {
        entry_id: String,
        text: String,
        attachments: Option<Vec<Attachment>>,
    },
    QueueCancel(String),
    QueueSteer(String),
    QueueStart(String),
    QueueResume,
    Visit(String),
    MarkUnread,
    Settle(Option<String>),
    Unsettle,
    Snooze(String),
    Wake,
    Pin(Option<String>),
    Unpin,
    OrderPinned(String),
    OrderActive(String),
    SetAutoSettle(bool),
    GoalCreate {
        objective: String,
        budget: Option<u64>,
    },
    GoalPause,
    GoalResume,
    GoalDrop,
    SubagentStop(String),
}

pub type CommandReply = oneshot::Sender<Result<Value, Error>>;

/// Where a `btw.ask` stream's items go; closing it ends the stream.
pub type BtwSink = mpsc::UnboundedSender<Result<BtwItem, Error>>;

/// Messages to a thread actor.
pub enum Request {
    /// Runs a command; the reply is its result (wire.md §5: `{sequence}`,
    /// or `turns.send`'s).
    Command {
        command: Command,
        command_id: Option<String>,
        reply: CommandReply,
    },
    /// Commits the freshly created thread and starts its first turn. The
    /// reply is the commit's sequence and the thread row.
    Launch {
        input: UserInput,
        /// Ask omp to name the thread from its first message.
        generate_title: bool,
        command_id: Option<String>,
        reply: oneshot::Sender<Result<(u64, Value), Error>>,
    },
    /// omp named the thread. Applied only while the thread still has the
    /// title `from` it was launched with, so a rename in between wins.
    Retitle { from: String, title: String },
    /// `btw.ask` (wire.md §5.13): asks omp a side question and sends the
    /// answer to `items`. It replaces a side question still being answered.
    Btw {
        /// Names this ask in [`Request::BtwCancel`].
        ask_id: u64,
        question: String,
        record_id: Option<String>,
        items: BtwSink,
    },
    /// The stream of ask `ask_id` is gone: cancel it if it still runs.
    BtwCancel { ask_id: u64 },
    /// Commits everything pending now.
    Flush { reply: oneshot::Sender<()> },
    /// The auto-settle sweep: settle when still due on live state
    /// (`docs/triage.md` §8) and unchanged since `seen_updated_at`.
    AutoSettle {
        now_ms: i64,
        after_ms: i64,
        seen_updated_at: String,
        reply: oneshot::Sender<bool>,
    },
    /// Close omp and end the actor.
    Shutdown { reply: oneshot::Sender<()> },
}

/// The actor has ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActorGone;

/// Mailbox of a running actor. Sending fails once the actor ended.
#[derive(Clone)]
pub struct ThreadHandle {
    tx: mpsc::UnboundedSender<Request>,
}

impl ThreadHandle {
    pub fn send(&self, request: Request) -> Result<(), ActorGone> {
        self.tx.send(request).map_err(|_| ActorGone)
    }

    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

/// Starts the actor for `model`. omp runs in `cwd` (the project directory);
/// `session_file` resumes an earlier omp session.
pub fn spawn(
    config: ActorConfig,
    model: ThreadModel,
    cwd: String,
    session_file: Option<String>,
) -> ThreadHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    let actor = Actor {
        mailbox: tx.downgrade(),
        config,
        model,
        cwd,
        session: None,
        events: None,
        stored_session_file: session_file.clone(),
        session_file,
        current_model: None,
        current_thinking: None,
        skills: Vec::new(),
        btw: None,
        flush_at: None,
        idle_since: Instant::now(),
        expiries: HashMap::new(),
        deleted: false,
    };
    tokio::spawn(actor.run(rx));
    ThreadHandle { tx }
}

struct Actor {
    /// Weak, so the actor's own handle does not keep its mailbox open.
    mailbox: mpsc::WeakUnboundedSender<Request>,
    config: ActorConfig,
    model: ThreadModel,
    cwd: String,
    session: Option<Box<dyn AgentSession>>,
    events: Option<Events>,
    session_file: Option<String>,
    /// The session file the thread index holds.
    stored_session_file: Option<String>,
    current_model: Option<String>,
    current_thinking: Option<omp_rpc::ThinkingLevel>,
    /// Skill names known to this thread's omp, for `$skill` rewriting.
    skills: Vec<String>,
    /// The side question omp is answering.
    btw: Option<BtwRun>,
    flush_at: Option<Instant>,
    idle_since: Instant,
    expiries: HashMap<String, Instant>,
    deleted: bool,
}

/// A side question being answered.
struct BtwRun {
    ask_id: u64,
    record_id: String,
    items: BtwSink,
}

/// omp's side-question topic as the wire's: its first turn, then the
/// follow-ups.
fn btw_record(record: &BtwHistoryRecord) -> BtwRecord {
    fn turn(
        question: &str,
        answer: &str,
        status: omp_rpc::BtwStatus,
        error: Option<&String>,
    ) -> BtwTurn {
        BtwTurn {
            question: question.to_owned(),
            answer: answer.to_owned(),
            status: match status {
                omp_rpc::BtwStatus::Running => BtwStatus::Running,
                omp_rpc::BtwStatus::Complete => BtwStatus::Complete,
                // A turn whose omp died mid-answer.
                omp_rpc::BtwStatus::Cancelled | omp_rpc::BtwStatus::Interrupted => {
                    BtwStatus::Cancelled
                }
                omp_rpc::BtwStatus::Error => BtwStatus::Error,
            },
            error: error.cloned(),
        }
    }
    let first = turn(
        &record.question,
        &record.answer,
        record.status,
        record.error.as_ref(),
    );
    let follow_ups = record.follow_ups.iter().flatten().map(|follow_up| {
        turn(
            &follow_up.question,
            &follow_up.answer,
            follow_up.status,
            follow_up.error.as_ref(),
        )
    });
    BtwRecord {
        id: record.id.clone(),
        turns: std::iter::once(first).chain(follow_ups).collect(),
    }
}

async fn next_event(events: &mut Option<Events>) -> Option<Event> {
    match events {
        Some(events) => events.recv().await,
        None => std::future::pending().await,
    }
}

fn saving_failed() -> Error {
    Error::internal("The change could not be saved.")
}

/// omp's own refusal text (e.g. "Goal mode is disabled"), else `fallback`.
fn omp_refusal(error: &omp::Error, fallback: &str) -> Error {
    match error {
        omp::Error::Rpc(omp_rpc::Error::Command { error, .. }) if !error.trim().is_empty() => {
            Error::rejected(error.trim())
        }
        _ => Error::new(ErrorCode::Unavailable, fallback),
    }
}

/// Commits a thread's pending changes and its row (plus `result` under
/// `command id`). Returns the commit's sequence; with nothing to write and
/// no result, the current high-water mark.
pub fn commit_thread(
    hub: &Hub,
    model: &mut ThreadModel,
    session_file: Option<&str>,
    removed: bool,
    result: Option<(String, ResultFn)>,
) -> Result<u64, Error> {
    let drained = model.drain();
    if drained.is_empty() && !removed && result.is_none() {
        return Ok(hub.high_water());
    }
    let commit = Commit {
        thread: Some(ThreadWrite {
            thread_id: model.thread.id.clone(),
            project_id: model.thread.project_id.clone(),
            session_file: session_file.map(str::to_owned),
            records: drained.records,
            items: drained.items,
            row: drained.row,
            removed,
        }),
        projects: Vec::new(),
        result,
    };
    hub.commit(commit).map_err(|error| {
        tracing::error!(%error, thread = %model.thread.id, "commit failed");
        saving_failed()
    })
}

impl Actor {
    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Request>) {
        loop {
            let idle_deadline = self.idle_deadline();
            let expiry = self
                .expiries
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(id, at)| (id.clone(), *at));
            tokio::select! {
                request = rx.recv() => match request {
                    Some(Request::Shutdown { reply }) => {
                        self.flush();
                        self.close_session().await;
                        self.flush();
                        let _ = reply.send(());
                        break;
                    }
                    Some(request) => {
                        self.handle(request).await;
                        if self.deleted {
                            break;
                        }
                    }
                    None => {
                        self.flush();
                        self.close_session().await;
                        self.flush();
                        break;
                    }
                },
                event = next_event(&mut self.events) => match event {
                    Some(event) => self.on_event(event).await,
                    None => self.on_process_exit(),
                },
                () = tokio::time::sleep_until(self.flush_at.unwrap_or_else(Instant::now)), if self.flush_at.is_some() => {
                    self.flush();
                }
                () = tokio::time::sleep_until(idle_deadline.unwrap_or_else(Instant::now)), if idle_deadline.is_some() => {
                    // Idle long enough: release omp and the actor. The next
                    // command reloads the thread from the store.
                    self.close_session().await;
                    self.flush();
                    if rx.is_empty() {
                        break;
                    }
                }
                () = tokio::time::sleep_until(expiry.as_ref().map_or_else(Instant::now, |(_, at)| *at)), if expiry.is_some() => {
                    if let Some((request_id, _)) = expiry {
                        self.expiries.remove(&request_id);
                        if let Some(response) = self.model.expire(&request_id) {
                            self.respond_ui(response);
                        }
                        let _ = self.commit(None);
                    }
                }
            }
        }
    }

    /// When the actor may wind down: nothing running, queued or pending, and
    /// no active goal (omp pauses a goal it restores on restart).
    fn idle_deadline(&self) -> Option<Instant> {
        let busy = !self.model.is_idle()
            || self.model.next_startable().is_some()
            || !self.expiries.is_empty()
            || self.flush_at.is_some()
            || self.btw.is_some()
            || (self.session.is_some() && self.model.goal_active());
        (!busy).then(|| self.idle_since + self.config.idle_timeout)
    }

    // -----------------------------------------------------------------------
    // Commits
    // -----------------------------------------------------------------------

    fn flush(&mut self) {
        if self.model.has_pending() {
            let _ = self.commit(None);
        }
        self.flush_at = None;
    }

    /// Commits pending changes now when urgent, else within the coalescing
    /// window.
    fn schedule_commit(&mut self) {
        if self.model.has_urgent() {
            let _ = self.commit(None);
        } else if self.model.has_pending() && self.flush_at.is_none() {
            self.flush_at = Some(Instant::now() + self.config.coalesce);
        }
    }

    /// Commits pending changes (plus a command result); the sequence.
    fn commit(&mut self, result: Option<(String, ResultFn)>) -> Result<u64, Error> {
        self.flush_at = None;
        self.idle_since = Instant::now();
        if self.session_file != self.stored_session_file {
            // The index row carries the session file.
            self.model.touch_command();
        }
        let sequence = commit_thread(
            &self.config.hub,
            &mut self.model,
            self.session_file.as_deref(),
            self.deleted,
            result,
        )?;
        self.stored_session_file.clone_from(&self.session_file);
        Ok(sequence)
    }

    // -----------------------------------------------------------------------
    // omp session
    // -----------------------------------------------------------------------

    async fn ensure_session(&mut self) -> Result<(), String> {
        if self.session.is_some() {
            return Ok(());
        }
        let resume = self.session_file.as_ref().map(PathBuf::from);
        match self
            .config
            .factory
            .start(PathBuf::from(&self.cwd), resume)
            .await
        {
            Ok((session, events)) => {
                self.session_file = Some(session.session_file().to_string_lossy().into_owned());
                match session.get_state().await {
                    Ok(state) => {
                        self.current_model = state
                            .model
                            .as_ref()
                            .map(|model| format!("{}/{}", model.provider, model.id));
                        self.current_thinking = state.thinking_level;
                        self.model.apply_goal_state(state.goal.as_ref());
                    }
                    Err(error) => tracing::warn!(%error, "omp get_state after start"),
                }
                match session.available_commands().await {
                    Ok(commands) => self.set_commands(&commands),
                    Err(error) => tracing::debug!(%error, "omp get_available_commands"),
                }
                self.session = Some(session);
                self.events = Some(events);
                let _ = self.commit(None);
                Ok(())
            }
            Err(error) => {
                tracing::warn!(%error, thread = %self.model.thread_id(), "starting omp");
                Err("The agent could not be started on this machine.".to_owned())
            }
        }
    }

    fn set_commands(&mut self, commands: &[AvailableSlashCommand]) {
        self.skills = commands
            .iter()
            .filter(|command| command.source == SlashCommandSource::Skill)
            .map(|command| {
                command
                    .name
                    .strip_prefix("skill:")
                    .unwrap_or(&command.name)
                    .to_owned()
            })
            .collect();
        if let Some(sink) = &self.config.commands {
            sink(&self.cwd, commands);
        }
    }

    async fn close_session(&mut self) {
        self.fail_btw();
        self.events = None;
        if let Some(session) = self.session.take() {
            session.close().await;
            if !self.deleted {
                self.model.session_closed();
            }
        }
    }

    fn respond_ui(&self, response: omp_rpc::ExtensionUiResponse) {
        if let Some(session) = &self.session
            && let Err(error) = session.respond_ui(response)
        {
            tracing::warn!(%error, "answering omp ui request");
        }
    }

    fn on_process_exit(&mut self) {
        self.fail_btw();
        self.events = None;
        if self.session.take().is_some() {
            tracing::warn!(thread = %self.model.thread_id(), "omp exited");
        }
        if !self.model.is_idle() {
            self.model.abandon_turn(false);
        }
        self.model.session_closed();
        let _ = self.commit(None);
    }

    async fn on_event(&mut self, event: Event) {
        match &event {
            Event::Notification(RpcNotification::AvailableCommandsUpdate(update)) => {
                let commands = update.commands.clone();
                self.set_commands(&commands);
            }
            Event::Notification(RpcNotification::BtwDelta(delta)) => {
                if let Some(run) = self.btw.as_ref().filter(|r| r.record_id == delta.record_id) {
                    let _ = run.items.send(Ok(BtwItem::Delta {
                        record_id: delta.record_id.clone(),
                        append: delta.delta.clone(),
                    }));
                }
                return;
            }
            Event::Notification(RpcNotification::BtwRecord(update)) => {
                self.on_btw_record(&update.record);
                return;
            }
            _ => {}
        }
        let effects = self.model.apply(&event);
        self.run_effects(effects).await;
        self.schedule_commit();
    }

    async fn run_effects(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::RefreshState => {
                    let state = match &self.session {
                        Some(session) => session.get_state().await,
                        None => continue,
                    };
                    match state {
                        Ok(state) => {
                            self.current_model = state
                                .model
                                .as_ref()
                                .map(|model| format!("{}/{}", model.provider, model.id));
                            self.current_thinking = state.thinking_level;
                            self.model.apply_state(&state);
                        }
                        Err(error) => tracing::debug!(%error, "omp get_state"),
                    }
                }
                Effect::ExpireAfter {
                    request_id,
                    after_ms,
                } => {
                    self.expiries
                        .insert(request_id, Instant::now() + Duration::from_millis(after_ms));
                }
                Effect::TurnEnded => {
                    // Commit the ended turn before starting the next one.
                    let _ = self.commit(None);
                    self.start_next().await;
                }
                Effect::FetchSubagentResult { subagent_id } => {
                    let transcript = match &self.session {
                        Some(session) => session.subagent_messages(subagent_id.clone()).await,
                        None => continue,
                    };
                    match transcript {
                        Ok(transcript) => {
                            self.model
                                .apply_subagent_transcript(&subagent_id, &transcript);
                        }
                        Err(error) => tracing::debug!(%error, "omp get_subagent_messages"),
                    }
                }
                Effect::StoreImages(images) => {
                    for image in images {
                        let session_file = self.session_file.as_deref().map(std::path::Path::new);
                        let bytes = match crate::attachments::image_bytes(&image.data, session_file)
                        {
                            Ok(bytes) => bytes,
                            Err(error) => {
                                tracing::warn!(%error, "reading a tool output image");
                                continue;
                            }
                        };
                        let name = format!(
                            "output.{}",
                            image.mime_type.strip_prefix("image/").unwrap_or("png")
                        );
                        if let Err(error) = self
                            .config
                            .attachments
                            .store_image(&image.attachment_id, &name, &image.mime_type, &bytes)
                            .await
                        {
                            tracing::warn!(%error, "storing a tool output image");
                        }
                    }
                }
            }
        }
        self.expiries
            .retain(|request_id, _| self.model.is_open_request(request_id));
    }

    // -----------------------------------------------------------------------
    // Side questions
    // -----------------------------------------------------------------------

    /// `btw.ask`: omp answers on its own, beside whatever the thread runs.
    async fn ask_btw(
        &mut self,
        ask_id: u64,
        question: &str,
        record_id: Option<String>,
        items: BtwSink,
    ) {
        let question = question.trim();
        if question.is_empty() {
            let _ = items.send(Err(Error::rejected("Type a question after /btw.")));
            return;
        }
        if let Err(message) = self.ensure_session().await {
            let _ = items.send(Err(Error::new(ErrorCode::Unavailable, message)));
            return;
        }
        // omp answers one side question at a time: the new one wins.
        if let Some(previous) = self.btw.take() {
            self.cancel_btw(&previous.record_id).await;
            let _ = previous.items.send(Err(Error::new(
                ErrorCode::Conflict,
                "Another side question replaced this one.",
            )));
        }
        let Some(session) = &self.session else {
            return;
        };
        match session.btw(question.to_owned(), record_id).await {
            Ok(record) => {
                let _ = items.send(Ok(BtwItem::Record {
                    record: btw_record(&record),
                }));
                self.btw = Some(BtwRun {
                    ask_id,
                    record_id: record.id,
                    items,
                });
            }
            Err(error) => {
                tracing::warn!(%error, "omp btw");
                let _ = items.send(Err(omp_refusal(
                    &error,
                    "The side question could not be asked.",
                )));
            }
        }
    }

    async fn cancel_btw(&self, record_id: &str) {
        if let Some(session) = &self.session
            && let Err(error) = session.btw_cancel(record_id.to_owned()).await
        {
            tracing::debug!(%error, "omp btw_cancel");
        }
    }

    /// omp's `btw_record`: the topic on a status change. One that is no
    /// longer running ends the stream.
    fn on_btw_record(&mut self, record: &BtwHistoryRecord) {
        let Some(run) = self.btw.as_ref().filter(|run| run.record_id == record.id) else {
            return;
        };
        let record = btw_record(record);
        let running = record
            .turns
            .last()
            .is_some_and(|turn| turn.status == BtwStatus::Running);
        let _ = run.items.send(Ok(BtwItem::Record { record }));
        if !running {
            self.btw = None;
            self.idle_since = Instant::now();
        }
    }

    /// omp is gone: the side question it was answering fails.
    fn fail_btw(&mut self) {
        if let Some(run) = self.btw.take() {
            let _ = run.items.send(Err(Error::new(
                ErrorCode::Unavailable,
                "The agent stopped before it answered.",
            )));
        }
    }

    async fn start_next(&mut self) {
        if let Some(input) = self.begin_next() {
            self.send_prompt(input).await;
        }
    }

    /// Starts the next startable queue entry as a turn and commits it.
    fn begin_next(&mut self) -> Option<UserInput> {
        while let Some(entry_id) = self.model.next_startable() {
            match self.model.begin_queued(&entry_id) {
                Ok(input) => {
                    let _ = self.commit(None);
                    return Some(input);
                }
                Err(_) => {
                    let _ = self.model.cancel_queued(&entry_id);
                }
            }
        }
        None
    }

    /// Prompts omp for the live turn, then for each queue entry that becomes
    /// startable because a turn ended without reaching omp's agent loop.
    async fn send_prompt(&mut self, input: UserInput) {
        let mut input = input;
        while self.prompt_once(input).await {
            match self.begin_next() {
                Some(next) => input = next,
                None => return,
            }
        }
    }

    /// Asks omp to name the thread from its first message, off the actor's
    /// task; the answer comes back as [`Request::Retitle`].
    fn generate_title(&self, text: &str) {
        let title = self
            .config
            .factory
            .title(PathBuf::from(&self.cwd), text.to_owned());
        let from = self.model.thread.title.clone();
        let mailbox = self.mailbox.clone();
        tokio::spawn(async move {
            if let Some(title) = title.await
                && let Some(mailbox) = mailbox.upgrade()
            {
                let _ = mailbox.send(Request::Retitle { from, title });
            }
        });
    }

    async fn retitle(&mut self, from: &str, title: &str) {
        if self.model.thread.title != from || self.model.rename(title).is_err() {
            return;
        }
        let _ = self.commit(None);
        // Mirror it into the omp session, so `omp --resume` lists the same name.
        if let Some(session) = &self.session
            && let Err(error) = session.set_session_name(title.to_owned()).await
        {
            tracing::debug!(%error, "omp set_session_name");
        }
    }

    async fn images(&self, attachments: &[Attachment]) -> Vec<ImageContent> {
        let mut images = Vec::new();
        for attachment in attachments {
            if !crate::attachments::SUPPORTED_IMAGE_MIME_TYPES
                .contains(&attachment.mime_type.as_str())
            {
                continue;
            }
            match self.config.attachments.read(&attachment.id).await {
                Ok((bytes, mime_type)) => images.push(ImageContent {
                    r#type: Some(LitImage),
                    data: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
                    mime_type: Some(mime_type),
                    ..ImageContent::default()
                }),
                Err(error) => tracing::warn!(%error, id = %attachment.id, "reading attachment"),
            }
        }
        images
    }

    /// What omp gets as `input`'s text: `$skill` rewritten, then one
    /// `@"path"` mention per uploaded file that is not an image omp takes
    /// inline. omp reads a mentioned file into the prompt itself, as for an
    /// `@path` the user typed.
    async fn prompt_text(&self, input: &UserInput) -> String {
        let mut text = rewrite_skill(&input.text, &self.skills);
        let mut mentions = Vec::new();
        for attachment in &input.attachments {
            if crate::attachments::SUPPORTED_IMAGE_MIME_TYPES
                .contains(&attachment.mime_type.as_str())
            {
                continue;
            }
            match self.config.attachments.named_path(&attachment.id).await {
                Ok(path) => mentions.push(format!("@\"{}\"", path.display())),
                Err(error) => tracing::warn!(%error, id = %attachment.id, "reading attachment"),
            }
        }
        if !mentions.is_empty() {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&mentions.join("\n"));
        }
        text
    }

    /// Prompts omp for the live turn (already committed). Returns true when
    /// the turn already ended and the queue may advance.
    async fn prompt_once(&mut self, input: UserInput) -> bool {
        if let Err(message) = self.ensure_session().await {
            self.model.fail_turn(&message, TurnErrorKind::Omp);
            let _ = self.commit(None);
            return false;
        }
        if let Err(message) = self.apply_model(&input).await {
            self.model.fail_turn(&message, TurnErrorKind::Refused);
            let _ = self.commit(None);
            return true;
        }
        let images = self.images(&input.attachments).await;
        let text = self.prompt_text(&input).await;
        let Some(session) = &self.session else {
            return false;
        };
        match session.prompt(text, images).await {
            Ok(ack) if ack.agent_invoked == Some(false) => {
                self.model.complete_without_agent();
                let _ = self.commit(None);
                true
            }
            Ok(_) => false,
            Err(error) => {
                tracing::warn!(%error, "omp prompt");
                self.model.fail_turn(
                    "The agent did not accept the message.",
                    TurnErrorKind::Refused,
                );
                let _ = self.commit(None);
                true
            }
        }
    }

    /// Switches omp's model and thinking level to the turn's.
    async fn apply_model(&mut self, input: &UserInput) -> Result<(), String> {
        let Some(session) = &self.session else {
            return Ok(());
        };
        let model = &input.model;
        let slug = format!("{}/{}", model.provider, model.id);
        if self.current_model.as_deref() != Some(slug.as_str()) {
            if model.provider.is_empty() || model.id.is_empty() {
                return Err(format!("Unknown model \"{slug}\"."));
            }
            if let Err(error) = session
                .set_model(model.provider.clone(), model.id.clone())
                .await
            {
                tracing::warn!(%error, "omp set_model");
                return Err(format!("The model \"{slug}\" is not available."));
            }
            self.current_model = Some(slug);
        }
        if let Some(level) = model.thinking.and_then(omp_level)
            && self.current_thinking != Some(level)
        {
            if let Err(error) = session.set_thinking_level(level).await {
                tracing::warn!(%error, "omp set_thinking_level");
            } else {
                self.current_thinking = Some(level);
            }
        }
        Ok(())
    }

    async fn steer_omp(&self, input: &UserInput) -> Result<(), Error> {
        let Some(session) = &self.session else {
            return Err(Error::rejected("The agent is not running."));
        };
        let images = self.images(&input.attachments).await;
        session
            .steer(self.prompt_text(input).await, images)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "omp steer");
                Error::new(
                    ErrorCode::Unavailable,
                    "The agent did not accept the message.",
                )
            })
    }

    /// omp `goal` with `op`, starting omp if needed; the result is applied
    /// right away (the matching `goal_updated` frame follows).
    async fn goal_command(
        &mut self,
        op: GoalOp,
        objective: Option<String>,
        budget: Option<u64>,
    ) -> Result<(), Error> {
        let budget = budget
            .map(i64::try_from)
            .transpose()
            .map_err(|_| Error::rejected("The token budget is too large."))?;
        self.ensure_session()
            .await
            .map_err(|message| Error::new(ErrorCode::Unavailable, message))?;
        let Some(session) = &self.session else {
            return Err(Error::rejected("The agent is not running."));
        };
        match session.goal(op, objective, budget).await {
            Ok(result) => {
                self.model.apply_goal(result.goal.as_ref());
                Ok(())
            }
            Err(error) => {
                tracing::warn!(%error, op = op.as_str(), "omp goal");
                Err(omp_refusal(&error, "The goal could not be changed."))
            }
        }
    }

    /// omp `cancel_subagent` for a subagent of this thread.
    async fn stop_subagent(&self, subagent_id: &str) -> Result<(), Error> {
        let ended = || Error::new(ErrorCode::Conflict, "That subagent is no longer running.");
        match self.model.subagent_running(subagent_id) {
            None => return Err(Error::not_found("subagent", subagent_id)),
            Some(false) => return Err(ended()),
            Some(true) => {}
        }
        let Some(session) = &self.session else {
            return Err(ended());
        };
        match session.cancel_subagent(subagent_id.to_owned()).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(ended()),
            Err(error) => {
                tracing::warn!(%error, "omp cancel_subagent");
                Err(omp_refusal(&error, "The subagent could not be stopped."))
            }
        }
    }

    // -----------------------------------------------------------------------
    // Requests
    // -----------------------------------------------------------------------

    async fn handle(&mut self, request: Request) {
        match request {
            Request::Command {
                command,
                command_id,
                reply,
            } => {
                if let Some(command_id) = &command_id
                    && let Ok(Some(result)) =
                        self.config.hub.read(|store| store.receipt(command_id))
                {
                    let _ = reply.send(Ok(result));
                    return;
                }
                self.command(command, command_id, reply).await;
            }
            Request::Launch {
                input,
                generate_title,
                command_id,
                reply,
            } => {
                self.model.start_turn(&input);
                self.model.on_user_message();
                let thread_id = self.model.thread_id().to_owned();
                let result = command_id.map(|command_id| {
                    let result: ResultFn = Box::new(
                        move |sequence| json!({"threadId": thread_id, "sequence": sequence}),
                    );
                    (command_id, result)
                });
                let committed = self.commit(result).map(|sequence| {
                    let row = serde_json::to_value(self.model.row()).unwrap_or(Value::Null);
                    (sequence, row)
                });
                let started = committed.is_ok();
                let _ = reply.send(committed);
                if started {
                    if generate_title {
                        self.generate_title(&input.text);
                    }
                    self.send_prompt(input).await;
                }
            }
            Request::Retitle { from, title } => self.retitle(&from, &title).await,
            Request::Btw {
                ask_id,
                question,
                record_id,
                items,
            } => self.ask_btw(ask_id, &question, record_id, items).await,
            Request::BtwCancel { ask_id } => {
                if let Some(run) = self.btw.take_if(|run| run.ask_id == ask_id) {
                    self.cancel_btw(&run.record_id).await;
                    self.idle_since = Instant::now();
                }
            }
            Request::Flush { reply } => {
                self.flush();
                let _ = reply.send(());
            }
            Request::AutoSettle {
                now_ms,
                after_ms,
                seen_updated_at,
                reply,
            } => {
                let settled = self.model.auto_settle(now_ms, after_ms, &seen_updated_at)
                    && self.commit(None).is_ok();
                let _ = reply.send(settled);
            }
            Request::Shutdown { reply } => {
                let _ = reply.send(());
            }
        }
    }

    async fn command(&mut self, command: Command, command_id: Option<String>, reply: CommandReply) {
        let mut prompt: Option<UserInput> = None;
        let mut abort = false;
        let mut force = false;
        let mut start_next = false;
        let mut turn_id = None;
        let mut queue_entry_id = None;
        let outcome: Result<(), Error> = match command {
            Command::Rename(title) => self.model.rename(&title),
            Command::Archive => {
                self.model.archive();
                Ok(())
            }
            Command::Unarchive => {
                self.model.unarchive();
                Ok(())
            }
            Command::Delete { only_if_unused } => {
                if only_if_unused && !self.model.turns.is_empty() {
                    Err(Error::rejected("This thread has messages, so it was kept."))
                } else {
                    self.close_session().await;
                    self.deleted = true;
                    Ok(())
                }
            }
            Command::SetMode(mode) => {
                self.model.set_mode(mode);
                Ok(())
            }
            Command::Send { input, delivery } => {
                if input.text.trim().is_empty() && input.attachments.is_empty() {
                    Err(Error::rejected("The message is empty."))
                } else {
                    let sent = self.send(input, delivery).await;
                    sent.map(|(turn, entry, start, stop)| {
                        turn_id = turn;
                        queue_entry_id = entry;
                        prompt = start;
                        abort = stop;
                    })
                }
            }
            Command::Stop {
                hold_queue,
                force: kill,
            } => {
                if hold_queue {
                    self.model.hold_queue();
                }
                if kill {
                    force = true;
                } else {
                    abort = self.model.request_stop().is_some();
                }
                self.model.touch_command();
                Ok(())
            }
            Command::Answer {
                request_id,
                answers,
            } => self
                .model
                .answer(&request_id, &answers)
                .map(|response| self.respond_ui(response)),
            Command::Dismiss { request_id } => self
                .model
                .dismiss(&request_id)
                .map(|response| self.respond_ui(response)),
            Command::QueueEdit {
                entry_id,
                text,
                attachments,
            } => self.model.edit_queued(&entry_id, &text, attachments),
            Command::QueueCancel(entry_id) => self.model.cancel_queued(&entry_id),
            Command::QueueSteer(entry_id) => self.steer_queued(&entry_id).await,
            Command::QueueStart(entry_id) => match self.model.queued_input(&entry_id) {
                Err(error) => Err(error),
                Ok(_) if self.model.is_idle() => self.model.begin_queued(&entry_id).map(|input| {
                    prompt = Some(input);
                }),
                Ok(_) => self.model.queue_first(&entry_id).map(|()| {
                    abort = self.model.request_stop().is_some();
                }),
            },
            Command::QueueResume => {
                self.model.resume_queue();
                self.model.touch_command();
                start_next = true;
                Ok(())
            }
            Command::Visit(visited_at) => self
                .model
                .triage(|triage, _, _| triage::visit(triage, &visited_at)),
            Command::MarkUnread => self.model.triage(triage::mark_unread),
            Command::Settle(settled_at) => self.model.triage(|triage, facts, now| {
                triage::settle(triage, facts, now, settled_at.as_deref())
            }),
            Command::Unsettle => self.model.triage(triage::unsettle),
            Command::Snooze(until) => self
                .model
                .triage(|triage, facts, now| triage::snooze(triage, facts, now, &until)),
            Command::Wake => self.model.triage(triage::wake),
            Command::Pin(order) => self
                .model
                .triage(|triage, facts, now| triage::pin(triage, facts, now, order.as_deref())),
            Command::Unpin => self.model.triage(triage::unpin),
            Command::OrderPinned(order) => self
                .model
                .triage(|triage, facts, now| triage::order_pinned(triage, facts, now, &order)),
            Command::OrderActive(order) => self
                .model
                .triage(|triage, facts, _| triage::order_active(triage, facts, &order)),
            Command::SetAutoSettle(enabled) => self
                .model
                .triage(|triage, facts, now| triage::set_auto_settle(triage, facts, now, enabled)),
            Command::GoalCreate { objective, budget } => {
                let objective = objective.trim().to_owned();
                if objective.is_empty() {
                    Err(Error::rejected("Describe the goal first."))
                } else {
                    self.goal_command(GoalOp::Create, Some(objective), budget)
                        .await
                }
            }
            Command::GoalPause => self.goal_command(GoalOp::Pause, None, None).await,
            Command::GoalResume => self.goal_command(GoalOp::Resume, None, None).await,
            Command::GoalDrop => {
                // A completed goal is gone from omp already; only j3 shows it.
                if self.model.clear_completed_goal() {
                    Ok(())
                } else {
                    self.goal_command(GoalOp::Drop, None, None).await
                }
            }
            Command::SubagentStop(subagent_id) => self.stop_subagent(&subagent_id).await,
        };
        if let Err(error) = outcome {
            // Commit whatever a refused command left consistent (normally
            // nothing).
            self.schedule_commit();
            let _ = reply.send(Err(error));
            return;
        }
        // The row goes out even unchanged, so the commit has its own sequence.
        self.model.touch_command();
        let result = move |sequence: u64| {
            let mut result = json!({"sequence": sequence});
            if let Some(turn_id) = &turn_id {
                result["turnId"] = json!(turn_id);
            }
            if let Some(entry_id) = &queue_entry_id {
                result["queueEntryId"] = json!(entry_id);
            }
            result
        };
        let stored = command_id.map(|id| {
            let result = result.clone();
            let stored: ResultFn = Box::new(result);
            (id, stored)
        });
        let _ = reply.send(self.commit(stored).map(result));
        if self.deleted {
            return;
        }
        if force {
            self.close_session().await;
            if !self.model.is_idle() {
                self.model.abandon_turn(true);
            }
            self.expiries.clear();
            let _ = self.commit(None);
            start_next = true;
        } else if abort {
            match &self.session {
                Some(session) => {
                    if let Err(error) = session.abort().await {
                        tracing::warn!(%error, "omp abort");
                    }
                }
                None => {
                    // Nothing runs in omp (the process is gone): end the turn here.
                    self.model.abandon_turn(true);
                    let _ = self.commit(None);
                    start_next = true;
                }
            }
        }
        if let Some(input) = prompt {
            self.send_prompt(input).await;
        } else if start_next {
            self.start_next().await;
        }
    }

    /// `turns.send` (wire.md §5.5). Returns `(turn id, queue entry id,
    /// input to prompt, stop the active turn)`.
    async fn send(
        &mut self,
        input: UserInput,
        delivery: Delivery,
    ) -> Result<(Option<String>, Option<String>, Option<UserInput>, bool), Error> {
        let sent = if self.model.is_idle() {
            let turn_id = self.model.start_turn(&input);
            (Some(turn_id), None, Some(input), false)
        } else {
            match delivery {
                Delivery::Auto | Delivery::Queue => {
                    (None, Some(self.model.enqueue(&input)), None, false)
                }
                Delivery::Steer if self.model.has_running_turn() => {
                    self.steer_omp(&input).await?;
                    (Some(self.model.steer(&input)?), None, None, false)
                }
                // omp has not started the turn yet: it waits in the queue.
                Delivery::Steer => (None, Some(self.model.enqueue(&input)), None, false),
                Delivery::Restart => {
                    let entry_id = self.model.enqueue(&input);
                    self.model.queue_first(&entry_id)?;
                    let stop = self.model.request_stop().is_some();
                    (None, Some(entry_id), None, stop)
                }
            }
        };
        // A sent message un-settles and wakes the thread (docs/triage.md §6.2).
        self.model.on_user_message();
        Ok(sent)
    }

    /// `queue.steer`: the entry goes into the running turn as a steer.
    async fn steer_queued(&mut self, entry_id: &str) -> Result<(), Error> {
        let input = self.model.queued_input(entry_id)?;
        if self.model.is_idle() {
            return Err(Error::rejected("Nothing is running to steer into."));
        }
        if !self.model.has_running_turn() {
            return Err(Error::new(
                ErrorCode::Conflict,
                "The agent hasn't started yet; the message stays queued.",
            ));
        }
        self.steer_omp(&input).await?;
        self.model.steer_queued(entry_id).map(drop)
    }
}
