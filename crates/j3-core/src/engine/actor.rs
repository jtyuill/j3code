//! Thread actors: one tokio task per thread owning its [`ThreadModel`], its
//! omp session (spawned on first need, closed after settling idle) and the
//! j3-owned queue. Commands arrive over a mailbox; omp frames over the
//! session's event channel. Changes are committed through the [`Hub`];
//! streaming text is coalesced for [`ActorConfig::coalesce`].

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use base64::Engine as _;
use j3_protocol::t3::{
    chat_attachment::ChatAttachment,
    commands::{MessageDispatchCommand, OrchestrationV2Command},
    orchestration::{ProviderApprovalDecision, ProviderSessionStatus, ThreadProjection},
};
use serde_json::{Value, json};
use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};

use super::{
    model::{
        Delivery, Effect, Reject, ThreadModel, UserInput, rewrite_skill, split_model,
        thinking_level, triage::now_ms,
    },
    session::{AgentSession, Events, SessionFactory},
};
use crate::{
    attachments::AttachmentStore,
    hub::{Commit, Hub, ReceiptFn, ShellChange, ThreadWrite},
    omp::{
        self,
        omp_rpc::{
            self, AvailableSlashCommand, Event, GoalOp, ImageContent, LitImage, RpcNotification,
            SlashCommandSource, ThinkingLevel,
        },
    },
    store::ThreadRow,
};

/// Called with `(cwd, commands)` whenever a thread's omp reports its slash
/// command catalog, so the provider catalog can serve it per cwd.
pub type CommandsSink = Arc<dyn Fn(&str, &[AvailableSlashCommand]) + Send + Sync>;

/// Shared by every actor of an app.
#[derive(Clone)]
pub struct ActorConfig {
    pub hub: Arc<Hub>,
    pub factory: Arc<dyn SessionFactory>,
    pub attachments: Arc<AttachmentStore>,
    /// Close omp after the thread has been settled this long.
    pub idle_timeout: Duration,
    /// Coalescing window for streaming updates.
    pub coalesce: Duration,
    pub commands: Option<CommandsSink>,
}

pub type DispatchReply = oneshot::Sender<Result<u64, Reject>>;
pub type LaunchReply = oneshot::Sender<Result<(u64, ThreadProjection), Reject>>;
/// `Ok(None)`: the thread is not due for automatic settlement.
pub type AutoSettleReply = oneshot::Sender<Result<Option<u64>, Reject>>;

/// Messages to a thread actor.
pub enum Request {
    /// A thread-scoped `dispatchCommand` (already decoded).
    Dispatch {
        command: Box<OrchestrationV2Command>,
        reply: DispatchReply,
    },
    /// Commit the freshly created thread (and start its first message).
    Launch {
        command_id: String,
        input: Option<Box<UserInput>>,
        reply: LaunchReply,
    },
    /// Current projection.
    Projection {
        reply: oneshot::Sender<ThreadProjection>,
    },
    /// M18: kill the omp process now.
    ForceStop { reply: oneshot::Sender<u64> },
    /// t3 `thread.auto-settle` from the sweep: settle when the thread has
    /// been inactive longer than `after_ms` as of `now_ms` and is unchanged
    /// since `snapshot_at` (its shell's `updatedAt` the sweep saw).
    AutoSettle {
        now_ms: i64,
        after_ms: i64,
        snapshot_at: String,
        reply: AutoSettleReply,
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

/// Starts the actor for `model`. `session_file` resumes an earlier omp
/// session; `last_shell` is the stored shell row (`None` for a new thread,
/// so its first commit announces it on the shell stream).
pub fn spawn(
    config: ActorConfig,
    model: ThreadModel,
    session_file: Option<String>,
    last_shell: Option<Value>,
) -> ThreadHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    let actor = Actor {
        config,
        model,
        session: None,
        events: None,
        session_file,
        current_model: None,
        current_thinking: None,
        skills: Vec::new(),
        flush_at: None,
        idle_since: Instant::now(),
        expiries: HashMap::new(),
        last_shell,
        deleted: false,
    };
    tokio::spawn(actor.run(rx));
    ThreadHandle { tx }
}

struct Actor {
    config: ActorConfig,
    model: ThreadModel,
    session: Option<Box<dyn AgentSession>>,
    events: Option<Events>,
    session_file: Option<String>,
    current_model: Option<String>,
    current_thinking: Option<ThinkingLevel>,
    /// Skill names known to this thread's omp, for `$skill` rewriting.
    skills: Vec<String>,
    flush_at: Option<Instant>,
    idle_since: Instant,
    expiries: HashMap<String, Instant>,
    last_shell: Option<Value>,
    deleted: bool,
}

async fn next_event(events: &mut Option<Events>) -> Option<Event> {
    match events {
        Some(events) => events.recv().await,
        None => std::future::pending().await,
    }
}

fn reject(message: &str) -> Reject {
    Reject(message.to_owned())
}

/// omp's own refusal text (e.g. "Goal mode is disabled"), else `fallback`.
fn omp_refusal(error: &omp::Error, fallback: &str) -> Reject {
    match error {
        omp::Error::Rpc(omp_rpc::Error::Command { error, .. }) if !error.trim().is_empty() => {
            Reject(error.trim().to_owned())
        }
        _ => reject(fallback),
    }
}

impl Actor {
    fn cwd(&self) -> String {
        self.model
            .proj
            .sessions
            .first()
            .map(|session| session.cwd.clone())
            .unwrap_or_default()
    }

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
                    // Settled long enough: release omp and the actor. The
                    // next command reloads the thread from the store.
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
                        let _ = self.commit_now(None);
                    }
                }
            }
        }
    }

    /// When the actor may wind down: nothing running, queued or pending, and
    /// no active goal (omp pauses a goal it restores on restart).
    fn idle_deadline(&self) -> Option<Instant> {
        let busy = !self.model.is_settled()
            || self.model.next_startable().is_some()
            || !self.expiries.is_empty()
            || self.flush_at.is_some()
            || (self.session.is_some() && self.model.goal_active());
        (!busy).then(|| self.idle_since + self.config.idle_timeout)
    }

    // -----------------------------------------------------------------------
    // Commits
    // -----------------------------------------------------------------------

    fn flush(&mut self) {
        if self.model.proj.has_pending() {
            let _ = self.commit_now(None);
        }
        self.flush_at = None;
    }

    /// Commits pending changes now when urgent, else within the coalescing
    /// window.
    fn schedule_commit(&mut self) {
        if self.model.proj.has_urgent() {
            let _ = self.commit_now(None);
        } else if self.model.proj.has_pending() && self.flush_at.is_none() {
            self.flush_at = Some(Instant::now() + self.config.coalesce);
        }
    }

    /// Commits pending changes (plus `receipt`); returns the commit sequence.
    fn commit_now(&mut self, receipt: Option<(String, ReceiptFn)>) -> Result<u64, Reject> {
        self.flush_at = None;
        self.idle_since = Instant::now();
        commit_thread(
            &self.config.hub,
            &mut self.model,
            self.session_file.clone(),
            self.deleted,
            &mut self.last_shell,
            receipt,
        )
    }

    fn commit_with_receipt(&mut self, command_id: &str) -> Result<u64, Reject> {
        self.commit_now(Some((
            command_id.to_owned(),
            Box::new(|sequence| json!({"sequence": sequence})),
        )))
    }
}

/// Commits a thread model's pending changes, its index row and the derived
/// shell item (when the shell changed), plus an optional receipt.
pub fn commit_thread(
    hub: &Hub,
    model: &mut ThreadModel,
    session_file: Option<String>,
    deleted: bool,
    last_shell: &mut Option<Value>,
    receipt: Option<(String, ReceiptFn)>,
) -> Result<u64, Reject> {
    let drained = model.proj.drain().map_err(|error| {
        tracing::error!(%error, "encoding thread entities");
        reject("The change could not be saved.")
    })?;
    let shell_json = serde_json::to_value(model.proj.shell()).map_err(|error| {
        tracing::error!(%error, "encoding thread shell");
        reject("The change could not be saved.")
    })?;
    let thread = &model.proj.thread;
    let location = if thread.archived_at.is_some() {
        "archive"
    } else {
        "active"
    };
    let mut changes = Vec::new();
    if deleted {
        changes.push(ShellChange::ThreadRemoved {
            location,
            thread_id: thread.id.clone(),
        });
    } else if last_shell.as_ref() != Some(&shell_json) {
        changes.push(ShellChange::ThreadUpdated {
            location,
            thread: shell_json.clone(),
        });
    }
    if drained.rows.is_empty() && changes.is_empty() && receipt.is_none() {
        return Ok(hub.high_water());
    }
    let commit = Commit {
        thread: Some(ThreadWrite {
            thread_id: thread.id.clone(),
            entities: drained.rows,
            events: drained.events,
            index: Some(ThreadRow {
                id: thread.id.clone(),
                project_id: thread.project_id.clone(),
                session_file,
                archived: thread.archived_at.is_some(),
                deleted,
                shell: shell_json.clone(),
            }),
            delete: deleted,
        }),
        projects: Vec::new(),
        shell: changes,
        receipt,
    };
    match hub.commit(commit) {
        Ok(sequence) => {
            *last_shell = Some(shell_json);
            Ok(sequence)
        }
        Err(error) => {
            tracing::error!(%error, thread = %thread.id, "commit failed");
            Err(reject("The change could not be saved."))
        }
    }
}

impl Actor {
    // -----------------------------------------------------------------------
    // omp session
    // -----------------------------------------------------------------------

    async fn ensure_session(&mut self) -> Result<(), String> {
        if self.session.is_some() {
            return Ok(());
        }
        self.model
            .set_session_status(ProviderSessionStatus::Starting, None);
        let _ = self.commit_now(None);
        let cwd = PathBuf::from(self.cwd());
        let resume = self.session_file.as_ref().map(PathBuf::from);
        match self.config.factory.start(cwd, resume).await {
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
                self.model
                    .set_session_status(ProviderSessionStatus::Ready, None);
                let _ = self.commit_now(None);
                Ok(())
            }
            Err(error) => {
                tracing::warn!(%error, thread = %self.model.thread_id(), "starting omp");
                let message = "The agent could not be started on this machine.".to_owned();
                self.model
                    .set_session_status(ProviderSessionStatus::Error, Some(error.to_string()));
                Err(message)
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
            sink(&self.cwd(), commands);
        }
    }

    async fn close_session(&mut self) {
        self.events = None;
        if let Some(session) = self.session.take() {
            session.close().await;
            if !self.deleted {
                self.model.session_closed();
                self.model
                    .set_session_status(ProviderSessionStatus::Stopped, None);
            }
        }
    }

    fn respond_ui(&self, response: crate::omp::omp_rpc::ExtensionUiResponse) {
        if let Some(session) = &self.session
            && let Err(error) = session.respond_ui(response)
        {
            tracing::warn!(%error, "answering omp ui request");
        }
    }

    fn on_process_exit(&mut self) {
        self.events = None;
        if let Some(session) = self.session.take() {
            drop(session);
            tracing::warn!(thread = %self.model.thread_id(), "omp exited");
        }
        if self.model.live_run_id().is_some() {
            self.model.abandon_run(false);
        }
        self.model.session_closed();
        self.model
            .set_session_status(ProviderSessionStatus::Stopped, None);
        let _ = self.commit_now(None);
    }

    async fn on_event(&mut self, event: Event) {
        if let Event::Notification(RpcNotification::AvailableCommandsUpdate(update)) = &event {
            let commands = update.commands.clone();
            self.set_commands(&commands);
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
                Effect::RunSettled => {
                    // Commit the settled run before starting the next one.
                    let _ = self.commit_now(None);
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
            }
        }
        self.expiries.retain(|request_id, _| {
            self.model.proj.request(request_id).is_some_and(|r| {
                r.status == j3_protocol::t3::orchestration::RuntimeRequestStatus::Pending
            })
        });
    }

    async fn start_next(&mut self) {
        if let Some(input) = self.begin_next() {
            self.send_prompt(input).await;
        }
    }

    /// Moves the next startable queued run to `starting` and commits it.
    fn begin_next(&mut self) -> Option<UserInput> {
        while let Some(run_id) = self.model.next_startable() {
            match self.model.begin_queued(&run_id) {
                Ok(input) => {
                    let _ = self.commit_now(None);
                    return Some(input);
                }
                Err(_) => {
                    let _ = self.model.cancel_queued(&run_id);
                }
            }
        }
        None
    }

    /// Prompts omp for the live run, then for each queued run that becomes
    /// startable because a run ended without reaching omp's agent loop.
    async fn send_prompt(&mut self, input: UserInput) {
        let mut input = input;
        while self.prompt_once(input).await {
            match self.begin_next() {
                Some(next) => input = next,
                None => return,
            }
        }
    }

    async fn images(&self, attachments: &[ChatAttachment]) -> Vec<ImageContent> {
        let mut images = Vec::new();
        for attachment in attachments {
            let ChatAttachment::Image(image) = attachment else {
                continue;
            };
            match self.config.attachments.read(&image.id).await {
                Ok((bytes, mime_type)) => images.push(ImageContent {
                    r#type: Some(LitImage),
                    data: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
                    mime_type: Some(mime_type),
                    ..ImageContent::default()
                }),
                Err(error) => tracing::warn!(%error, id = %image.id, "reading attachment"),
            }
        }
        images
    }

    /// Prompts omp for the live run (already committed as `starting`).
    /// Returns true when the run already ended and the queue may advance.
    async fn prompt_once(&mut self, input: UserInput) -> bool {
        if let Err(message) = self.ensure_session().await {
            self.model.fail_run(&message);
            let _ = self.commit_now(None);
            return false;
        }
        if let Err(message) = self.apply_selection(&input).await {
            self.model.fail_run(&message);
            let _ = self.commit_now(None);
            return true;
        }
        let images = self.images(&input.attachments).await;
        let text = rewrite_skill(&input.text, &self.skills);
        let Some(session) = &self.session else {
            return false;
        };
        match session.prompt(text, images).await {
            Ok(ack) if ack.agent_invoked == Some(false) => {
                self.model.complete_without_agent();
                let _ = self.commit_now(None);
                true
            }
            Ok(_) => false,
            Err(error) => {
                tracing::warn!(%error, "omp prompt");
                self.model.fail_run("The agent did not accept the message.");
                let _ = self.commit_now(None);
                true
            }
        }
    }

    /// Switches omp's model / thinking level to the run's selection.
    async fn apply_selection(&mut self, input: &UserInput) -> Result<(), String> {
        let Some(session) = &self.session else {
            return Ok(());
        };
        let selection = &input.model_selection;
        if self.current_model.as_deref() != Some(selection.model.as_str()) {
            let Some((provider, model_id)) = split_model(&selection.model) else {
                return Err(format!("Unknown model \"{}\".", selection.model));
            };
            if let Err(error) = session
                .set_model(provider.to_owned(), model_id.to_owned())
                .await
            {
                tracing::warn!(%error, "omp set_model");
                return Err(format!(
                    "The model \"{}\" is not available.",
                    selection.model
                ));
            }
            self.current_model = Some(selection.model.clone());
        }
        if let Some(level) = thinking_level(selection)
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

    async fn steer_omp(&self, input: &UserInput) -> Result<(), Reject> {
        let Some(session) = &self.session else {
            return Err(reject("The agent is not running."));
        };
        let images = self.images(&input.attachments).await;
        session
            .steer(rewrite_skill(&input.text, &self.skills), images)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "omp steer");
                reject("The agent did not accept the message.")
            })
    }

    /// omp `goal` with `op`, starting omp if needed; the result is applied
    /// right away (the matching `goal_updated` frame follows).
    async fn goal_command(
        &mut self,
        op: GoalOp,
        objective: Option<String>,
        token_budget: Option<u64>,
    ) -> Result<(), Reject> {
        let token_budget = token_budget
            .map(i64::try_from)
            .transpose()
            .map_err(|_| reject("The token budget is too large."))?;
        self.ensure_session().await.map_err(Reject)?;
        let Some(session) = &self.session else {
            return Err(reject("The agent is not running."));
        };
        match session.goal(op, objective, token_budget).await {
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
    async fn stop_subagent(&self, subagent_id: &str) -> Result<(), Reject> {
        let not_running = || reject("That subagent is no longer running.");
        if !self
            .model
            .proj
            .subagent(subagent_id)
            .is_some_and(|subagent| subagent.status.is_active())
        {
            return Err(not_running());
        }
        let Some(session) = &self.session else {
            return Err(not_running());
        };
        match session.cancel_subagent(subagent_id.to_owned()).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(not_running()),
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
            Request::Dispatch { command, reply } => {
                let command_id = command.command_id().unwrap_or_default().to_owned();
                if let Ok(Some(receipt)) = self.config.hub.read(|store| store.receipt(&command_id))
                {
                    let sequence = receipt.get("sequence").and_then(Value::as_u64).unwrap_or(0);
                    let _ = reply.send(Ok(sequence));
                    return;
                }
                self.dispatch(*command, &command_id, reply).await;
            }
            Request::Launch {
                command_id,
                input,
                reply,
            } => {
                if let Some(input) = &input {
                    self.model.start_run(input);
                    self.model.on_user_message(now_ms());
                }
                let thread_id = self.model.thread_id().to_owned();
                let result = self.commit_now(Some((
                    command_id,
                    Box::new(move |sequence| json!({"threadId": thread_id, "sequence": sequence})),
                )));
                let projection = self.model.proj.to_wire();
                let _ = reply.send(result.map(|sequence| (sequence, projection)));
                if let Some(input) = input {
                    self.send_prompt(*input).await;
                }
            }
            Request::Projection { reply } => {
                self.flush();
                let _ = reply.send(self.model.proj.to_wire());
            }
            Request::ForceStop { reply } => {
                self.close_session().await;
                if self.model.live_run_id().is_some() {
                    self.model.abandon_run(true);
                }
                self.expiries.clear();
                let sequence = self.commit_now(None).unwrap_or(0);
                let _ = reply.send(sequence);
                self.start_next().await;
            }
            Request::AutoSettle {
                now_ms,
                after_ms,
                snapshot_at,
                reply,
            } => {
                let _ = reply.send(self.auto_settle(now_ms, after_ms, &snapshot_at));
            }
            Request::Shutdown { reply } => {
                let _ = reply.send(());
            }
        }
    }

    fn auto_settle(
        &mut self,
        now_ms: i64,
        after_ms: i64,
        snapshot_at: &str,
    ) -> Result<Option<u64>, Reject> {
        let Some(settled_ms) = self.model.auto_settlement_at(now_ms, after_ms) else {
            return Ok(None);
        };
        let command_id = format!(
            "server:auto-settle:{}:{snapshot_at}",
            self.model.thread_id()
        );
        if let Ok(Some(receipt)) = self.config.hub.read(|store| store.receipt(&command_id)) {
            return Ok(receipt.get("sequence").and_then(Value::as_u64));
        }
        match self.model.auto_settle(now_ms, snapshot_at, settled_ms) {
            Ok(()) => self.commit_with_receipt(&command_id).map(Some),
            Err(error) => {
                self.schedule_commit();
                Err(error)
            }
        }
    }

    async fn dispatch(
        &mut self,
        command: OrchestrationV2Command,
        command_id: &str,
        reply: DispatchReply,
    ) {
        use OrchestrationV2Command as C;
        let mut after: Option<UserInput> = None;
        let mut abort = false;
        let mut start_next = false;
        let result: Result<(), Reject> = match command {
            C::MessageDispatch(message) => match self.dispatch_message(message).await {
                Ok(start) => {
                    after = start;
                    Ok(())
                }
                Err(error) => Err(error),
            },
            C::RunInterrupt(interrupt) => {
                if interrupt.hold_queue == Some(true) {
                    self.model.hold_queue();
                }
                let active = self.model.proj.active_run().map(|run| run.id.clone());
                if active.as_deref() == Some(interrupt.run_id.as_str()) {
                    self.model.request_interrupt(interrupt.reason.as_deref());
                    abort = true;
                }
                Ok(())
            }
            C::ThreadStop(stop) => {
                if self
                    .model
                    .request_interrupt(stop.reason.as_deref())
                    .is_some()
                {
                    abort = true;
                }
                Ok(())
            }
            C::ThreadArchive(_) => {
                self.model.archive();
                Ok(())
            }
            C::ThreadUnarchive(_) => {
                self.model.unarchive();
                Ok(())
            }
            C::ThreadDelete(delete) => {
                if delete.only_if_unused == Some(true) && !self.model.proj.runs.is_empty() {
                    Err(reject("This thread has messages, so it was kept."))
                } else {
                    self.close_session().await;
                    self.model.delete();
                    self.deleted = true;
                    Ok(())
                }
            }
            C::ThreadMetadataUpdate(update) => match &update.title {
                Some(title) => self.model.rename(title),
                None => Ok(()),
            },
            C::ThreadRuntimeModeSet(set) => {
                self.model.set_runtime_mode(set.runtime_mode);
                Ok(())
            }
            C::ThreadInteractionModeSet(set) => {
                self.model.set_interaction_mode(set.interaction_mode);
                Ok(())
            }
            C::RuntimeRequestRespond(respond) => {
                let outcome = match (&respond.answers, respond.decision) {
                    (Some(answers), _) => self.model.answer(&respond.request_id, answers),
                    (
                        None,
                        Some(ProviderApprovalDecision::Decline | ProviderApprovalDecision::Cancel),
                    ) => self.model.dismiss(&respond.request_id),
                    (None, Some(_)) => {
                        let answers = [("answer".to_owned(), json!("Yes"))].into_iter().collect();
                        self.model.answer(&respond.request_id, &answers)
                    }
                    (None, None) => Err(reject("Answer the question to continue.")),
                };
                outcome.map(|response| self.respond_ui(response))
            }
            C::UserInputDismiss(dismiss) => self
                .model
                .dismiss(&dismiss.request_id)
                .map(|response| self.respond_ui(response)),
            C::QueuedRunCancel(cancel) => self.model.cancel_queued(&cancel.run_id),
            C::QueuedRunEdit(edit) => {
                self.model
                    .edit_queued(&edit.run_id, &edit.text, edit.attachments, edit.context)
            }
            C::QueuedMessagePromoteToSteer(promote) => {
                match self.model.proj.run(&promote.queued_run_id) {
                    Some(_) => match self.steerable_input(&promote.queued_run_id) {
                        Ok(input) => match self.steer_omp(&input).await {
                            Ok(()) => self
                                .model
                                .promote(&promote.queued_run_id, &promote.target_run_id)
                                .map(drop),
                            Err(error) => Err(error),
                        },
                        Err(error) => Err(error),
                    },
                    None => Err(reject("That message is no longer queued.")),
                }
            }
            C::QueueResume(_) => {
                self.model.resume_queue();
                start_next = true;
                Ok(())
            }
            C::QueuedRunStart(start) => self.start_queued_now(&start.run_id).await.map(|input| {
                after = input;
            }),
            C::ThreadVisit(visit) => self.model.visit(&visit.visited_at),
            C::ThreadSettle(settle) => self.model.settle(now_ms(), settle.settled_at.as_deref()),
            C::ThreadUnsettle(_) => self.model.unsettle(now_ms()),
            C::ThreadSnooze(snooze) => self.model.snooze(now_ms(), &snooze.snoozed_until),
            C::ThreadUnsnooze(_) => self.model.unsnooze(now_ms()),
            C::ThreadAutoSettleSet(set) => self.model.set_auto_settle(now_ms(), set.enabled),
            C::ThreadPin(pin) => self.model.pin(now_ms(), pin.order_key.as_deref()),
            C::ThreadUnpin(_) => self.model.unpin(now_ms()),
            C::ThreadPinReorder(reorder) => self.model.reorder_pinned(now_ms(), &reorder.order_key),
            C::ThreadActiveReorder(reorder) => self.model.reorder_active(&reorder.order_key),
            C::ThreadMarkUnread(_) => self.model.mark_unread(now_ms()),
            C::SubagentStop(stop) => self.stop_subagent(&stop.subagent_id).await,
            C::GoalCreate(create) => {
                let objective = create.objective.trim();
                if objective.is_empty() {
                    Err(reject("Describe the goal first."))
                } else {
                    self.goal_command(
                        GoalOp::Create,
                        Some(objective.to_owned()),
                        create.token_budget,
                    )
                    .await
                }
            }
            C::GoalPause(_) => self.goal_command(GoalOp::Pause, None, None).await,
            C::GoalResume(_) => self.goal_command(GoalOp::Resume, None, None).await,
            C::GoalDrop(_) => {
                // A completed goal is gone from omp already; only j3 shows it.
                if self.model.clear_completed_goal() {
                    Ok(())
                } else {
                    self.goal_command(GoalOp::Drop, None, None).await
                }
            }
            C::Unknown(_) => Err(reject("This action isn't supported by this server.")),
        };
        match result {
            Ok(()) => {
                let committed = self.commit_with_receipt(command_id);
                let _ = reply.send(committed);
            }
            Err(error) => {
                // Drop partial changes of a rejected command from the batch
                // by committing whatever is consistent (nothing, normally).
                self.schedule_commit();
                let _ = reply.send(Err(error));
                return;
            }
        }
        if abort && let Some(session) = &self.session {
            if let Err(error) = session.abort().await {
                tracing::warn!(%error, "omp abort");
            }
        } else if abort {
            // Nothing is running in omp (e.g. the process is gone): end the
            // run here.
            self.model.abandon_run(true);
            let _ = self.commit_now(None);
        }
        if let Some(input) = after {
            self.send_prompt(input).await;
        } else if start_next {
            self.start_next().await;
        }
    }

    fn steerable_input(&self, queued_run_id: &str) -> Result<UserInput, Reject> {
        let run = self
            .model
            .proj
            .run(queued_run_id)
            .filter(|run| run.status == j3_protocol::t3::orchestration::RunStatus::Queued)
            .ok_or_else(|| reject("That message is no longer queued."))?;
        if !self.model.has_running_turn() {
            return Err(reject(
                "The agent hasn't started yet; the message stays queued.",
            ));
        }
        let message = self
            .model
            .proj
            .messages
            .iter()
            .find(|message| message.id == run.user_message_id)
            .ok_or_else(|| reject("The queued message no longer exists."))?;
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

    /// D16: send a queued message now — steer it into a running turn, or
    /// start it when nothing runs.
    async fn start_queued_now(&mut self, run_id: &str) -> Result<Option<UserInput>, Reject> {
        match self.model.proj.active_run().map(|run| run.id.clone()) {
            None => self.model.begin_queued(run_id).map(Some),
            Some(active) if self.model.has_running_turn() => {
                let input = self.steerable_input(run_id)?;
                self.steer_omp(&input).await?;
                self.model.promote(run_id, &active)?;
                Ok(None)
            }
            Some(_) => {
                self.model.resume_queue();
                self.model.queue_first(run_id);
                Ok(None)
            }
        }
    }

    /// D1. Returns the input to prompt once the command is committed.
    async fn dispatch_message(
        &mut self,
        message: MessageDispatchCommand,
    ) -> Result<Option<UserInput>, Reject> {
        let input = UserInput {
            message_id: message.message_id,
            text: message.text,
            attachments: message.attachments,
            context: message.context,
            model_selection: message
                .model_selection
                .unwrap_or_else(|| self.model.proj.thread.model_selection.clone()),
            created_by: message.created_by,
            creation_source: message.creation_source,
        };
        if input.text.trim().is_empty() && input.attachments.is_empty() {
            return Err(reject("The message is empty."));
        }
        let start = match self
            .model
            .resolve_delivery(&message.dispatch_mode, message.delivery_intent)
        {
            Delivery::Start => {
                self.model.start_run(&input);
                Some(input)
            }
            Delivery::Queue => {
                self.model.queue_run(&input);
                None
            }
            Delivery::Steer { run_id } => {
                if self.model.has_running_turn() {
                    self.steer_omp(&input).await?;
                    self.model.steer(&input, &run_id)?;
                } else {
                    self.model.queue_run(&input);
                }
                None
            }
            Delivery::Restart { .. } => {
                let run_id = self.model.queue_run(&input);
                self.model.queue_first(&run_id);
                if self
                    .model
                    .request_interrupt(Some("Restarting with a new message"))
                    .is_some()
                    && let Some(session) = &self.session
                    && let Err(error) = session.abort().await
                {
                    tracing::warn!(%error, "omp abort");
                }
                None
            }
        };
        // t3 `dispatchMessage`: a sent message un-settles and wakes the thread.
        self.model.on_user_message(now_ms());
        Ok(start)
    }
}
