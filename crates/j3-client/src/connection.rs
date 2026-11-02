//! The public handle to one server and its typed calls.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Mutex, PoisonError, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use j3_protocol::j3::{
    GET_SUBAGENT_TRANSCRIPT, GET_USAGE, GoalCommand, GoalCreateCommand, SubagentTranscript,
    SubagentTranscriptInput, UsageSnapshot,
};
use j3_protocol::t3::{
    self,
    assets::{
        AssetCreateUrlInput, AssetCreateUrlResult, AssetDisposition, AssetResource,
        AttachmentCreateUploadUrlInput, AttachmentCreateUploadUrlResult,
        ImageAttachmentCreateUploadUrlInput, ImageType,
    },
    chat_attachment::{ChatAttachment, ChatImageAttachment},
    commands::{
        DeliveryIntent, DispatchCommandResult, DispatchMode, MessageDispatchCommand,
        OrchestrationV2Command, PromoteToSteerCommand, QueuedRunCommand, QueuedRunEditCommand,
        RunInterruptCommand, RuntimeRequestRespondCommand, SubagentStopCommand,
        ThreadAutoSettleSetCommand, ThreadCommand, ThreadDeleteCommand, ThreadLaunchInitialMessage,
        ThreadLaunchInput, ThreadLaunchResult, ThreadMetadataUpdateCommand, ThreadPinCommand,
        ThreadReorderCommand, ThreadSettleCommand, ThreadSnoozeCommand, ThreadStopCommand,
        ThreadUserReasonCommand, ThreadVisitCommand, UserInputDismissCommand, UserReason,
    },
    filesystem::{FilesystemBrowseInput, FilesystemBrowseResult},
    git::{
        GIT_RUN_STACKED_ACTION, GitActionProgress, GitActionProgressEvent,
        GitRunStackedActionInput, GitRunStackedActionResult, GitStackedAction, VCS_PULL,
        VCS_REFRESH_STATUS, VcsCwdInput, VcsPullResult, VcsStatusResult,
    },
    methods::{ArchivedShellSnapshot, EmptyInput},
    model::ModelSelection,
    orchestration::{
        Actor, CreationSource, ProviderInteractionMode, RuntimeMode, WorkspaceStrategy,
    },
    project::{
        PROJECTS_LIST_ENTRIES, PROJECTS_READ_FILE, Project, ProjectCreateMutation,
        ProjectDeleteMutation, ProjectListEntriesInput, ProjectListEntriesResult, ProjectMutation,
        ProjectReadFileInput, ProjectReadFileResult,
    },
    server::{ServerProviderUpdatedPayload, ServerRefreshProvidersInput},
    terminal::{
        TERMINAL_ATTACH, TERMINAL_CLOSE, TERMINAL_RESIZE, TERMINAL_WRITE, TerminalAttachInput,
        TerminalAttachStreamEvent, TerminalCloseInput, TerminalResizeInput, TerminalWriteInput,
    },
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};

use crate::{
    ConnectionState, Error, ThreadState, WorkspaceCommands,
    supervisor::{self, Command, SessionSlot, Shared, encode},
};

/// What a running git action reports (see [`Connection::run_git_action`]).
#[derive(Debug, Clone)]
pub enum GitActionUpdate {
    /// A progress event: `action_started`, `phase_started` or hook output.
    Progress(GitActionProgressEvent),
    /// The action ended with `action_finished`'s result, or why it failed
    /// (the server's error, else its `action_failed` message). Always last.
    Finished(Result<Box<GitRunStackedActionResult>, Error>),
}

/// A running `git.runStackedAction`. Dropping it interrupts the action.
pub struct GitAction {
    updates: mpsc::UnboundedReceiver<GitActionUpdate>,
    task: tokio::task::AbortHandle,
    finished: bool,
}

impl GitAction {
    /// The next update, from any executor; `None` after
    /// [`GitActionUpdate::Finished`].
    pub async fn next(&mut self) -> Option<GitActionUpdate> {
        if self.finished {
            return None;
        }
        let update = self
            .updates
            .recv()
            .await
            .unwrap_or(GitActionUpdate::Finished(Err(Error::Closed)));
        self.finished = matches!(update, GitActionUpdate::Finished(_));
        Some(update)
    }
}

impl Drop for GitAction {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Streams one stacked action, forwarding progress to `updates`. Dropping
/// the future (aborting its task) drops the stream, which interrupts it.
async fn drive_git_action(
    shared: &Shared,
    payload: Value,
    updates: &mpsc::UnboundedSender<GitActionUpdate>,
) -> Result<Box<GitRunStackedActionResult>, Error> {
    let decode = |e: serde_json::Error| Error::Decode(format!("{GIT_RUN_STACKED_ACTION}: {e}"));
    let session = shared.ready_session().await?;
    let mut stream = session.stream(GIT_RUN_STACKED_ACTION, payload)?;
    let (mut result, mut failed) = (None, None);
    let ended = loop {
        match stream.next().await {
            None => break None,
            Some(Err(error)) => break Some(error),
            Some(Ok(values)) => {
                for value in values {
                    let event: GitActionProgressEvent =
                        serde_json::from_value(value).map_err(decode)?;
                    match event.progress {
                        GitActionProgress::ActionFinished { result: finished } => {
                            result = Some(finished)
                        }
                        GitActionProgress::ActionFailed { message, .. } => failed = Some(message),
                        progress => {
                            let _ =
                                updates.send(GitActionUpdate::Progress(GitActionProgressEvent {
                                    progress,
                                    ..event
                                }));
                        }
                    }
                }
            }
        }
    };
    if let Some(result) = result {
        return Ok(result);
    }
    Err(match (ended, failed) {
        (Some(error @ Error::Rejected { .. }), _) => error,
        (_, Some(message)) => Error::rejected(serde_json::json!({ "message": message })),
        (Some(error), None) => error,
        (None, None) => Error::Decode(format!(
            "{GIT_RUN_STACKED_ACTION}: the stream ended without a result"
        )),
    })
}

/// An attached terminal (see [`Connection::attach_terminal`]). Dropping it
/// detaches; the shell keeps running on the server.
pub struct TerminalSession {
    events: mpsc::UnboundedReceiver<Result<TerminalAttachStreamEvent, Error>>,
    task: tokio::task::AbortHandle,
}

impl TerminalSession {
    /// The next event, from any executor: a `snapshot` first, then live
    /// events. `None` once the stream ended (after `closed`); an error is
    /// always last.
    pub async fn next(&mut self) -> Option<Result<TerminalAttachStreamEvent, Error>> {
        self.events.recv().await
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Forwards one `terminal.attach` stream to `events` until it ends or the
/// receiver is gone. Aborting the task drops the stream, which interrupts it.
async fn drive_terminal(
    shared: &Shared,
    payload: Value,
    events: &mpsc::UnboundedSender<Result<TerminalAttachStreamEvent, Error>>,
) -> Result<(), Error> {
    let session = shared.ready_session().await?;
    let mut stream = session.stream(TERMINAL_ATTACH, payload)?;
    while let Some(values) = stream.next().await {
        for value in values? {
            let event = serde_json::from_value(value)
                .map_err(|e| Error::Decode(format!("{TERMINAL_ATTACH}: {e}")))?;
            if events.send(Ok(event)).is_err() {
                return Ok(());
            }
        }
    }
    Ok(())
}

const UPLOAD_TIMEOUT: Duration = Duration::from_secs(120);

/// How [`Connection::send`] delivers a message (ios-surface §3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendMode {
    /// Start a run now (`start_immediately`).
    Start,
    /// Queue behind the active run (`queue_after_active`).
    Queue,
    /// Steer the active run if one is running, else start
    /// (`start_immediately` + `deliveryIntent: "auto"`).
    Auto,
}

/// One answer to a `user_input_request` question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    One(String),
    Many(Vec<String>),
}

/// One environment (server). Cheap to clone; the supervisor stops when the
/// last clone (and every [`ThreadHandle`]) is dropped, or on [`Connection::close`].
#[derive(Clone)]
pub struct Connection {
    inner: Arc<Inner>,
}

struct Inner {
    shared: Arc<Shared>,
    commands: mpsc::UnboundedSender<Command>,
    threads: Mutex<HashMap<String, Weak<ThreadSubscription>>>,
    generations: AtomicU64,
}

/// A live subscription to one thread. Cheap to clone; the subscription runs
/// while any clone is alive.
#[derive(Clone)]
pub struct ThreadHandle {
    subscription: Arc<ThreadSubscription>,
}

struct ThreadSubscription {
    thread_id: String,
    generation: u64,
    state: watch::Receiver<Arc<ThreadState>>,
    commands: mpsc::UnboundedSender<Command>,
}

impl Drop for ThreadSubscription {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Unsubscribe {
            thread_id: std::mem::take(&mut self.thread_id),
            generation: self.generation,
        });
    }
}

impl ThreadHandle {
    pub fn thread_id(&self) -> &str {
        &self.subscription.thread_id
    }

    pub fn state(&self) -> watch::Receiver<Arc<ThreadState>> {
        self.subscription.state.clone()
    }
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// A thread title from its first message (`deriveThreadTitleFromPrompt`):
/// whitespace collapsed, at most 72 characters.
fn title_from_prompt(text: &str, attachments: &[ChatAttachment]) -> String {
    let title = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = if !title.is_empty() {
        title
    } else {
        let images = attachments
            .iter()
            .filter(|attachment| matches!(attachment, ChatAttachment::Image(_)))
            .count();
        match images {
            0 => "New thread".to_owned(),
            1 => "Attached image".to_owned(),
            _ => "Attached images".to_owned(),
        }
    };
    if title.chars().count() <= 72 {
        title
    } else {
        let head: String = title.chars().take(69).collect();
        format!("{}...", head.trim_end())
    }
}

impl Connection {
    /// Starts the supervisor for `http_base` (e.g. `"http://shieldy:3780"`) on
    /// `runtime`. It connects, reconnects with backoff (1 s → 30 s, reset after
    /// a stable connection), pings, and resubscribes; it never gives up.
    pub fn open(http_base: impl Into<String>, runtime: tokio::runtime::Handle) -> Self {
        let http_base = http_base.into();
        let base = reqwest::Url::parse(&http_base)
            .map_err(|e| format!("invalid server URL {http_base:?}: {e}"))
            .and_then(|url| match url.scheme() {
                "http" | "https" => Ok(url),
                scheme => Err(format!(
                    "invalid server URL {http_base:?}: expected http or https, not {scheme}"
                )),
            });
        let shared = Arc::new(Shared {
            http_base,
            base,
            runtime: runtime.clone(),
            http: reqwest::Client::new(),
            state: watch::Sender::new(Arc::new(ConnectionState::default())),
            session: watch::Sender::new(SessionSlot::Down),
        });
        let (commands, rx) = mpsc::unbounded_channel();
        runtime.spawn(supervisor::run(Arc::clone(&shared), rx));
        Self {
            inner: Arc::new(Inner {
                shared,
                commands,
                threads: Mutex::new(HashMap::new()),
                generations: AtomicU64::new(0),
            }),
        }
    }

    pub fn http_base(&self) -> &str {
        &self.inner.shared.http_base
    }

    /// Status, descriptor, server config and shell.
    pub fn state(&self) -> watch::Receiver<Arc<ConnectionState>> {
        self.inner.shared.state.subscribe()
    }

    /// The live state of a thread. The subscription runs while any handle for
    /// the thread is alive and resumes after reconnects.
    pub fn thread(&self, thread_id: &str) -> ThreadHandle {
        let mut threads = self
            .inner
            .threads
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(subscription) = threads.get(thread_id).and_then(Weak::upgrade) {
            return ThreadHandle { subscription };
        }
        threads.retain(|_, subscription| subscription.strong_count() > 0);
        let generation = self.inner.generations.fetch_add(1, Ordering::Relaxed);
        let (state, rx) = watch::channel(Arc::new(ThreadState::new(thread_id.to_owned())));
        let _ = self.inner.commands.send(Command::Subscribe {
            thread_id: thread_id.to_owned(),
            generation,
            state,
        });
        let subscription = Arc::new(ThreadSubscription {
            thread_id: thread_id.to_owned(),
            generation,
            state: rx,
            commands: self.inner.commands.clone(),
        });
        threads.insert(thread_id.to_owned(), Arc::downgrade(&subscription));
        ThreadHandle { subscription }
    }

    /// Stops the supervisor and closes the socket. Later calls fail with
    /// [`Error::Closed`].
    pub async fn close(&self) {
        let (ack, done) = oneshot::channel();
        if self.inner.commands.send(Command::Close(ack)).is_ok() {
            let _ = done.await;
        }
    }

    // -----------------------------------------------------------------------
    // Plumbing
    // -----------------------------------------------------------------------

    /// Runs `work` on the connection's runtime, so callers on any executor
    /// can await it.
    async fn spawn<T: Send + 'static>(
        &self,
        work: impl Future<Output = Result<T, Error>> + Send + 'static,
    ) -> Result<T, Error> {
        match self.inner.shared.runtime.spawn(work).await {
            Ok(result) => result,
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(_) => Err(Error::Closed),
        }
    }

    async fn call<T: DeserializeOwned + Send + 'static>(
        &self,
        tag: &'static str,
        payload: &impl Serialize,
    ) -> Result<T, Error> {
        let shared = Arc::clone(&self.inner.shared);
        let payload = encode(payload);
        self.spawn(async move {
            let session = shared.ready_session().await?;
            let value = session.request(tag, payload).await?;
            serde_json::from_value(value).map_err(|e| Error::Decode(format!("{tag}: {e}")))
        })
        .await
    }

    async fn dispatch(&self, command: OrchestrationV2Command) -> Result<u64, Error> {
        let result: DispatchCommandResult =
            self.call("orchestration.dispatchCommand", &command).await?;
        Ok(result.sequence)
    }

    async fn thread_command(
        &self,
        thread_id: &str,
        make: fn(ThreadCommand) -> OrchestrationV2Command,
    ) -> Result<u64, Error> {
        self.dispatch(make(ThreadCommand {
            command_id: new_id(),
            thread_id: thread_id.to_owned(),
        }))
        .await
    }

    async fn queued_run_command(
        &self,
        thread_id: &str,
        run_id: &str,
        make: fn(QueuedRunCommand) -> OrchestrationV2Command,
    ) -> Result<u64, Error> {
        self.dispatch(make(QueuedRunCommand {
            command_id: new_id(),
            thread_id: thread_id.to_owned(),
            run_id: run_id.to_owned(),
        }))
        .await
    }

    // -----------------------------------------------------------------------
    // Projects
    // -----------------------------------------------------------------------

    /// Adds a project for a directory on the server (`projects.mutate`).
    pub async fn create_project(
        &self,
        title: &str,
        workspace_root: &str,
    ) -> Result<Project, Error> {
        self.call(
            "projects.mutate",
            &ProjectMutation::Create(ProjectCreateMutation {
                command_id: new_id(),
                project_id: new_id(),
                title: title.to_owned(),
                workspace_root: workspace_root.to_owned(),
                create_workspace_root_if_missing: None,
                default_model_selection: None,
                scripts: None,
            }),
        )
        .await
    }

    pub async fn delete_project(&self, project_id: &str) -> Result<(), Error> {
        let _: Value = self
            .call(
                "projects.mutate",
                &ProjectMutation::Delete(ProjectDeleteMutation {
                    command_id: new_id(),
                    project_id: project_id.to_owned(),
                    force: None,
                }),
            )
            .await?;
        Ok(())
    }

    /// Lists the directories under `partial_path` on the server (`~` expands
    /// to the server user's home).
    pub async fn browse(&self, partial_path: &str) -> Result<FilesystemBrowseResult, Error> {
        self.call(
            "filesystem.browse",
            &FilesystemBrowseInput {
                partial_path: partial_path.to_owned(),
                cwd: None,
            },
        )
        .await
    }

    /// One directory level of a project on the server
    /// (`projects.listEntries`): `dir` is relative to `cwd`, empty for `cwd`
    /// itself. Directories come first; git-ignored entries are flagged.
    pub async fn list_entries(
        &self,
        cwd: &str,
        dir: &str,
    ) -> Result<ProjectListEntriesResult, Error> {
        self.call(
            PROJECTS_LIST_ENTRIES,
            &ProjectListEntriesInput {
                cwd: cwd.to_owned(),
                directory_path: (!dir.is_empty()).then(|| dir.to_owned()),
            },
        )
        .await
    }

    /// The text of a file on the server (`projects.readFile`): `path` is
    /// relative to `cwd` or absolute. At most 1 MiB comes back
    /// (`truncated`); binary files fail with failure `binary_file`.
    pub async fn read_file(&self, cwd: &str, path: &str) -> Result<ProjectReadFileResult, Error> {
        self.call(
            PROJECTS_READ_FILE,
            &ProjectReadFileInput {
                cwd: cwd.to_owned(),
                relative_path: path.to_owned(),
                allow_missing: None,
            },
        )
        .await
    }

    // -----------------------------------------------------------------------
    // Threads
    // -----------------------------------------------------------------------

    /// Creates a thread in `project_id` with its first message
    /// (`orchestration.launchThread`); returns the new thread's id.
    pub async fn launch(
        &self,
        project_id: &str,
        text: &str,
        attachments: Vec<ChatAttachment>,
        model: ModelSelection,
    ) -> Result<String, Error> {
        let input = ThreadLaunchInput {
            command_id: new_id(),
            creation_source: Some(CreationSource::Web),
            thread_id: Some(new_id()),
            reuse_existing_thread: None,
            project_id: project_id.to_owned(),
            title: title_from_prompt(text, &attachments),
            generate_title: None,
            model_selection: model,
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: ProviderInteractionMode::Default,
            workspace_strategy: WorkspaceStrategy::Root { branch: None },
            initial_message: Some(ThreadLaunchInitialMessage {
                message_id: Some(new_id()),
                text: text.to_owned(),
                context: None,
                attachments,
            }),
        };
        let result: ThreadLaunchResult = self.call("orchestration.launchThread", &input).await?;
        Ok(result.thread_id)
    }

    /// Sends a user message (`message.dispatch`); returns the command's
    /// sequence. `model` overrides the thread's model for this message.
    pub async fn send(
        &self,
        thread_id: &str,
        text: &str,
        attachments: Vec<ChatAttachment>,
        model: Option<ModelSelection>,
        mode: SendMode,
    ) -> Result<u64, Error> {
        let (dispatch_mode, delivery_intent) = match mode {
            SendMode::Start => (DispatchMode::StartImmediately, None),
            SendMode::Queue => (DispatchMode::QueueAfterActive, None),
            SendMode::Auto => (DispatchMode::StartImmediately, Some(DeliveryIntent::Auto)),
        };
        self.dispatch(OrchestrationV2Command::MessageDispatch(
            MessageDispatchCommand {
                notification: None,
                guest_message_id: None,
                created_by: Actor::User,
                creation_source: CreationSource::Web,
                scheduled_task_id: None,
                sender_thread_id: None,
                idle_compaction: None,
                command_id: new_id(),
                thread_id: thread_id.to_owned(),
                message_id: new_id(),
                text: text.to_owned(),
                context: None,
                attachments,
                title_seed: None,
                model_selection: model,
                source_plan_ref: None,
                restart_continuation_of_run_id: None,
                usage_limit_continuation_of_run_id: None,
                manual_continuation_of_run_id: None,
                usage_limit_recovery_request_id: None,
                delivery_intent,
                delegated_completion: None,
                dispatch_mode,
            },
        ))
        .await
    }

    /// Stops a run (`run.interrupt`). With `hold_queue`, queued runs wait for
    /// [`Connection::resume_queue`].
    pub async fn interrupt(
        &self,
        thread_id: &str,
        run_id: &str,
        hold_queue: bool,
    ) -> Result<u64, Error> {
        self.dispatch(OrchestrationV2Command::RunInterrupt(RunInterruptCommand {
            command_id: new_id(),
            thread_id: thread_id.to_owned(),
            run_id: run_id.to_owned(),
            reason: None,
            hold_queue: Some(hold_queue),
        }))
        .await
    }

    /// Stops whatever the thread runs (`thread.stop`), when no run id is known.
    pub async fn stop(&self, thread_id: &str) -> Result<u64, Error> {
        self.dispatch(OrchestrationV2Command::ThreadStop(ThreadStopCommand {
            command_id: new_id(),
            thread_id: thread_id.to_owned(),
            reason: None,
            stop_delegated_tasks: None,
        }))
        .await
    }

    pub async fn archive(&self, thread_id: &str) -> Result<u64, Error> {
        self.thread_command(thread_id, OrchestrationV2Command::ThreadArchive)
            .await
    }

    pub async fn unarchive(&self, thread_id: &str) -> Result<u64, Error> {
        self.thread_command(thread_id, OrchestrationV2Command::ThreadUnarchive)
            .await
    }

    pub async fn delete_thread(&self, thread_id: &str) -> Result<u64, Error> {
        self.dispatch(OrchestrationV2Command::ThreadDelete(ThreadDeleteCommand {
            command_id: new_id(),
            thread_id: thread_id.to_owned(),
            only_if_unused: None,
        }))
        .await
    }

    pub async fn rename(&self, thread_id: &str, title: &str) -> Result<u64, Error> {
        self.dispatch(OrchestrationV2Command::ThreadMetadataUpdate(
            ThreadMetadataUpdateCommand {
                command_id: new_id(),
                thread_id: thread_id.to_owned(),
                title: Some(title.to_owned()),
                regenerate_title: None,
                branch: None,
                worktree_path: None,
                expected_worktree_path: None,
                expected_branch: None,
                expected_empty: None,
                limit_recovery: None,
                linked_pull_request: None,
            },
        ))
        .await
    }

    /// Records that the thread was opened (`thread.visit`).
    pub async fn visit(&self, thread_id: &str) -> Result<u64, Error> {
        self.dispatch(OrchestrationV2Command::ThreadVisit(ThreadVisitCommand {
            command_id: new_id(),
            thread_id: thread_id.to_owned(),
            visited_at: t3::now_iso(),
            opened: Some(true),
            search_query: None,
        }))
        .await
    }

    // -----------------------------------------------------------------------
    // Settle workflow (t3's thread.settle … thread.mark-unread)
    // -----------------------------------------------------------------------

    /// Settles the thread (`thread.settle`): it moves to the Settled shelf
    /// and loses its pin, order keys and snooze. The server stamps
    /// `settledAt`.
    pub async fn settle(&self, thread_id: &str) -> Result<u64, Error> {
        self.dispatch(OrchestrationV2Command::ThreadSettle(ThreadSettleCommand {
            command_id: new_id(),
            thread_id: thread_id.to_owned(),
            settled_at: None,
        }))
        .await
    }

    /// Brings a settled thread back to Active (`thread.unsettle`).
    pub async fn unsettle(&self, thread_id: &str) -> Result<u64, Error> {
        self.dispatch(OrchestrationV2Command::ThreadUnsettle(
            ThreadUserReasonCommand {
                command_id: new_id(),
                thread_id: thread_id.to_owned(),
                reason: UserReason::User,
            },
        ))
        .await
    }

    /// Hides the thread until `until` (`thread.snooze`). The server refuses
    /// a wake in the past, a pending question and queued runs; see
    /// [`crate::workflow::can_snooze`].
    pub async fn snooze(&self, thread_id: &str, until: time::OffsetDateTime) -> Result<u64, Error> {
        let until_ms =
            i64::try_from(until.unix_timestamp_nanos().div_euclid(1_000_000)).unwrap_or(i64::MAX);
        self.dispatch(OrchestrationV2Command::ThreadSnooze(ThreadSnoozeCommand {
            command_id: new_id(),
            thread_id: thread_id.to_owned(),
            snoozed_until: t3::iso_from_unix_ms(until_ms),
        }))
        .await
    }

    /// Wakes a snoozed thread now (`thread.unsnooze`).
    pub async fn unsnooze(&self, thread_id: &str) -> Result<u64, Error> {
        self.dispatch(OrchestrationV2Command::ThreadUnsnooze(
            ThreadUserReasonCommand {
                command_id: new_id(),
                thread_id: thread_id.to_owned(),
                reason: UserReason::User,
            },
        ))
        .await
    }

    /// Turns automatic settling of an inactive thread on or off
    /// (`thread.auto-settle.set`).
    pub async fn set_auto_settle(&self, thread_id: &str, enabled: bool) -> Result<u64, Error> {
        self.dispatch(OrchestrationV2Command::ThreadAutoSettleSet(
            ThreadAutoSettleSetCommand {
                command_id: new_id(),
                thread_id: thread_id.to_owned(),
                enabled,
            },
        ))
        .await
    }

    /// Pins the thread (`thread.pin`), at `order_key` within Pinned
    /// ([`crate::workflow::plan_drop`]) or, without one, after the keyed
    /// pins. Pinning a settled or snoozed thread brings it back.
    pub async fn pin(&self, thread_id: &str, order_key: Option<&str>) -> Result<u64, Error> {
        self.dispatch(OrchestrationV2Command::ThreadPin(ThreadPinCommand {
            command_id: new_id(),
            thread_id: thread_id.to_owned(),
            order_key: order_key.map(str::to_owned),
        }))
        .await
    }

    pub async fn unpin(&self, thread_id: &str) -> Result<u64, Error> {
        self.thread_command(thread_id, OrchestrationV2Command::ThreadUnpin)
            .await
    }

    /// Moves a pinned thread to `order_key` (`thread.pin.reorder`).
    pub async fn reorder_pinned(&self, thread_id: &str, order_key: &str) -> Result<u64, Error> {
        self.dispatch(OrchestrationV2Command::ThreadPinReorder(
            ThreadReorderCommand {
                command_id: new_id(),
                thread_id: thread_id.to_owned(),
                order_key: order_key.to_owned(),
            },
        ))
        .await
    }

    /// Moves an Active thread to `order_key` (`thread.active.reorder`); the
    /// server refuses pinned and settled threads.
    pub async fn reorder_active(&self, thread_id: &str, order_key: &str) -> Result<u64, Error> {
        self.dispatch(OrchestrationV2Command::ThreadActiveReorder(
            ThreadReorderCommand {
                command_id: new_id(),
                thread_id: thread_id.to_owned(),
                order_key: order_key.to_owned(),
            },
        ))
        .await
    }

    /// Marks the latest completion unread again (`thread.mark-unread`); the
    /// server refuses a thread with no completed run.
    pub async fn mark_unread(&self, thread_id: &str) -> Result<u64, Error> {
        self.thread_command(thread_id, OrchestrationV2Command::ThreadMarkUnread)
            .await
    }

    /// Answers a pending question (`runtime-request.respond`); `answers` maps
    /// question ids to answers.
    pub async fn answer(
        &self,
        thread_id: &str,
        request_id: &str,
        answers: BTreeMap<String, Answer>,
    ) -> Result<u64, Error> {
        let answers = answers
            .into_iter()
            .map(|(question, answer)| {
                let value = match answer {
                    Answer::One(text) => Value::String(text),
                    Answer::Many(choices) => {
                        Value::Array(choices.into_iter().map(Value::String).collect())
                    }
                };
                (question, value)
            })
            .collect();
        self.dispatch(OrchestrationV2Command::RuntimeRequestRespond(
            RuntimeRequestRespondCommand {
                command_id: new_id(),
                thread_id: thread_id.to_owned(),
                request_id: request_id.to_owned(),
                decision: None,
                feedback: None,
                answers: Some(answers),
                attachments_by_question_id: None,
            },
        ))
        .await
    }

    /// Dismisses a pending question (`thread.user-input.dismiss`).
    pub async fn dismiss(&self, thread_id: &str, request_id: &str) -> Result<u64, Error> {
        self.dispatch(OrchestrationV2Command::UserInputDismiss(
            UserInputDismissCommand {
                command_id: new_id(),
                thread_id: thread_id.to_owned(),
                request_id: request_id.to_owned(),
            },
        ))
        .await
    }

    pub async fn cancel_queued(&self, thread_id: &str, run_id: &str) -> Result<u64, Error> {
        self.queued_run_command(thread_id, run_id, OrchestrationV2Command::QueuedRunCancel)
            .await
    }

    /// Replaces a queued message's text (attachments are kept).
    pub async fn edit_queued(
        &self,
        thread_id: &str,
        run_id: &str,
        text: &str,
    ) -> Result<u64, Error> {
        self.dispatch(OrchestrationV2Command::QueuedRunEdit(
            QueuedRunEditCommand {
                context: None,
                command_id: new_id(),
                thread_id: thread_id.to_owned(),
                run_id: run_id.to_owned(),
                text: text.to_owned(),
                attachments: None,
            },
        ))
        .await
    }

    /// Starts one queued run now ("send now").
    pub async fn start_queued(&self, thread_id: &str, run_id: &str) -> Result<u64, Error> {
        self.queued_run_command(thread_id, run_id, OrchestrationV2Command::QueuedRunStart)
            .await
    }

    /// Moves a queued message into the running turn as a steer.
    pub async fn promote_to_steer(
        &self,
        thread_id: &str,
        queued_run_id: &str,
        target_run_id: &str,
    ) -> Result<u64, Error> {
        self.dispatch(OrchestrationV2Command::QueuedMessagePromoteToSteer(
            PromoteToSteerCommand {
                command_id: new_id(),
                thread_id: thread_id.to_owned(),
                queued_run_id: queued_run_id.to_owned(),
                target_run_id: target_run_id.to_owned(),
            },
        ))
        .await
    }

    /// Releases a held queue (`queue.resume`).
    pub async fn resume_queue(&self, thread_id: &str) -> Result<u64, Error> {
        self.thread_command(thread_id, OrchestrationV2Command::QueueResume)
            .await
    }

    // -----------------------------------------------------------------------
    // Goals and subagents (docs/protocol/j3-extensions.md)
    // -----------------------------------------------------------------------

    /// Starts omp goal mode on the thread (`j3.goal.create`). omp refuses it
    /// while the thread has an active or paused goal, in plan mode, or with
    /// `goal.enabled` off; the error carries omp's reason. The goal shows in
    /// [`ThreadState::goal`] and on the thread's shell row.
    pub async fn create_goal(
        &self,
        thread_id: &str,
        objective: &str,
        token_budget: Option<u64>,
    ) -> Result<u64, Error> {
        self.dispatch(OrchestrationV2Command::GoalCreate(GoalCreateCommand {
            command_id: new_id(),
            thread_id: thread_id.to_owned(),
            objective: objective.to_owned(),
            token_budget,
        }))
        .await
    }

    /// Pauses the thread's active goal (`j3.goal.pause`).
    pub async fn pause_goal(&self, thread_id: &str) -> Result<u64, Error> {
        self.goal_command(thread_id, OrchestrationV2Command::GoalPause)
            .await
    }

    /// Resumes a paused or budget-limited goal (`j3.goal.resume`).
    pub async fn resume_goal(&self, thread_id: &str) -> Result<u64, Error> {
        self.goal_command(thread_id, OrchestrationV2Command::GoalResume)
            .await
    }

    /// Drops the thread's goal, including a completed one (`j3.goal.drop`).
    pub async fn drop_goal(&self, thread_id: &str) -> Result<u64, Error> {
        self.goal_command(thread_id, OrchestrationV2Command::GoalDrop)
            .await
    }

    async fn goal_command(
        &self,
        thread_id: &str,
        make: fn(GoalCommand) -> OrchestrationV2Command,
    ) -> Result<u64, Error> {
        self.dispatch(make(GoalCommand {
            command_id: new_id(),
            thread_id: thread_id.to_owned(),
        }))
        .await
    }

    /// Stops one running subagent without stopping its run (`subagent.stop`).
    /// `run_id` is the subagent's `runId`.
    pub async fn stop_subagent(
        &self,
        thread_id: &str,
        run_id: &str,
        subagent_id: &str,
    ) -> Result<u64, Error> {
        self.dispatch(OrchestrationV2Command::SubagentStop(SubagentStopCommand {
            command_id: new_id(),
            thread_id: thread_id.to_owned(),
            run_id: run_id.to_owned(),
            subagent_id: subagent_id.to_owned(),
        }))
        .await
    }

    /// Archived threads (`orchestration.getArchivedShellSnapshot`).
    pub async fn archived(&self) -> Result<ArchivedShellSnapshot, Error> {
        self.call("orchestration.getArchivedShellSnapshot", &EmptyInput {})
            .await
    }

    // -----------------------------------------------------------------------
    // Attachments
    // -----------------------------------------------------------------------

    /// Uploads an image (`attachments.createUploadUrl`, then `POST` of the
    /// bytes) and returns the reference to put in a message.
    pub async fn upload_image(
        &self,
        name: &str,
        mime_type: &str,
        bytes: Vec<u8>,
    ) -> Result<ChatAttachment, Error> {
        let size_bytes = bytes.len() as u64;
        let ticket: AttachmentCreateUploadUrlResult = self
            .call(
                "attachments.createUploadUrl",
                &AttachmentCreateUploadUrlInput::Image(ImageAttachmentCreateUploadUrlInput {
                    kind: Some(ImageType),
                    name: name.to_owned(),
                    mime_type: mime_type.to_owned(),
                    size_bytes,
                }),
            )
            .await?;
        let shared = Arc::clone(&self.inner.shared);
        let content_type = mime_type.to_owned();
        self.spawn(async move {
            let url = shared.url(&ticket.relative_url)?;
            let response = shared
                .http
                .post(url)
                .header(reqwest::header::CONTENT_TYPE, content_type)
                .body(bytes)
                .timeout(UPLOAD_TIMEOUT)
                .send()
                .await
                .map_err(|e| Error::Http(format!("uploading the image: {e}")))?;
            let status = response.status();
            if status.is_success() {
                Ok(())
            } else {
                let body = response.text().await.unwrap_or_default();
                Err(Error::Http(format!(
                    "uploading the image: the server answered {status}: {}",
                    body.trim()
                )))
            }
        })
        .await?;
        Ok(ChatAttachment::Image(ChatImageAttachment {
            id: ticket.attachment_id,
            name: name.to_owned(),
            mime_type: mime_type.to_owned(),
            size_bytes,
            source: None,
        }))
    }

    /// An absolute URL serving the attachment's bytes (`assets.createUrl`).
    pub async fn asset_url(&self, attachment: &ChatAttachment) -> Result<String, Error> {
        let (file_name, mime_type) = match attachment {
            ChatAttachment::Image(image) => (image.name.clone(), image.mime_type.clone()),
            ChatAttachment::File(file) => (file.name.clone(), file.mime_type.clone()),
            ChatAttachment::Unknown(other) => (other.name.clone(), other.mime_type.clone()),
        };
        let result: AssetCreateUrlResult = self
            .call(
                "assets.createUrl",
                &AssetCreateUrlInput {
                    resource: AssetResource::Attachment {
                        attachment_id: attachment.id().to_owned(),
                        file_name: Some(file_name),
                        mime_type: Some(mime_type),
                        disposition: Some(AssetDisposition::Inline),
                        render_intent: None,
                    },
                },
            )
            .await?;
        Ok(self.inner.shared.url(&result.relative_url)?.to_string())
    }

    // -----------------------------------------------------------------------
    // Provider
    // -----------------------------------------------------------------------

    /// The slash commands and skills omp offers in `cwd`
    /// (`server.refreshProviders`); also stored in
    /// [`ConnectionState::workspaces`].
    pub async fn refresh_workspace(&self, cwd: &str) -> Result<WorkspaceCommands, Error> {
        let instance_id = self
            .inner
            .shared
            .state
            .borrow()
            .config
            .as_ref()
            .and_then(|config| config.providers.first())
            .map(|provider| provider.instance_id.clone());
        let result: ServerProviderUpdatedPayload = self
            .call(
                "server.refreshProviders",
                &ServerRefreshProvidersInput {
                    instance_id: instance_id.clone(),
                    cwd: Some(cwd.to_owned()),
                    ..Default::default()
                },
            )
            .await?;
        let provider = result
            .providers
            .iter()
            .find(|provider| Some(&provider.instance_id) == instance_id.as_ref())
            .or(result.providers.first())
            .ok_or_else(|| Error::Decode("server.refreshProviders: no provider".into()))?;
        let commands = provider
            .workspace_snapshots
            .iter()
            .flatten()
            .find(|snapshot| snapshot.cwd == cwd)
            .map(|snapshot| WorkspaceCommands {
                slash_commands: snapshot.slash_commands.clone(),
                skills: snapshot.skills.clone(),
            })
            .unwrap_or_else(|| WorkspaceCommands {
                slash_commands: provider.slash_commands.clone(),
                skills: provider.skills.clone(),
            });
        let stored = commands.clone();
        let cwd = cwd.to_owned();
        self.inner.shared.update(move |state| {
            state.workspaces.insert(cwd, stored);
        });
        Ok(commands)
    }

    /// The server host's provider usage limits (`j3.getUsage`, which runs
    /// `omp usage --json` there).
    pub async fn usage(&self) -> Result<UsageSnapshot, Error> {
        self.call(GET_USAGE, &EmptyInput {}).await
    }

    /// Subagent `subagent_id`'s transcript entries from byte `from_byte` of
    /// its session file (`j3.getSubagentTranscript`); pass the answer's
    /// `next_byte` to continue.
    pub async fn subagent_transcript(
        &self,
        thread_id: &str,
        subagent_id: &str,
        from_byte: u64,
    ) -> Result<SubagentTranscript, Error> {
        let input = SubagentTranscriptInput {
            thread_id: thread_id.to_owned(),
            subagent_id: subagent_id.to_owned(),
            from_byte,
        };
        self.call(GET_SUBAGENT_TRANSCRIPT, &input).await
    }

    // -----------------------------------------------------------------------
    // Git
    // -----------------------------------------------------------------------

    /// The git status of `cwd`, freshly computed (`vcs.refreshStatus`).
    pub async fn git_status(&self, cwd: &str) -> Result<VcsStatusResult, Error> {
        self.call(
            VCS_REFRESH_STATUS,
            &VcsCwdInput {
                cwd: cwd.to_owned(),
            },
        )
        .await
    }

    /// Fast-forwards `cwd`'s branch from its upstream (`vcs.pull`).
    pub async fn pull(&self, cwd: &str) -> Result<VcsPullResult, Error> {
        self.call(
            VCS_PULL,
            &VcsCwdInput {
                cwd: cwd.to_owned(),
            },
        )
        .await
    }

    /// Starts a commit/push (`git.runStackedAction`) in `cwd` on the
    /// connection's runtime. `commit_message` `None` lets the server write
    /// one. Progress and outcome arrive through the returned [`GitAction`];
    /// dropping it interrupts the action.
    pub fn run_git_action(
        &self,
        cwd: &str,
        action: GitStackedAction,
        commit_message: Option<String>,
    ) -> GitAction {
        let shared = Arc::clone(&self.inner.shared);
        let payload = encode(&GitRunStackedActionInput {
            action_id: new_id(),
            cwd: cwd.to_owned(),
            action,
            commit_message,
            feature_branch: None,
            file_paths: None,
        });
        let (tx, updates) = mpsc::unbounded_channel();
        let task = self.inner.shared.runtime.spawn(async move {
            let outcome = drive_git_action(&shared, payload, &tx).await;
            let _ = tx.send(GitActionUpdate::Finished(outcome));
        });
        GitAction {
            updates,
            task: task.abort_handle(),
            finished: false,
        }
    }

    // -----------------------------------------------------------------------
    // Terminals
    // -----------------------------------------------------------------------

    /// Attaches to a thread's terminal (`terminal.attach`) on the
    /// connection's runtime, starting its shell in the thread's project
    /// folder when it has none. `size` is `(cols, rows)`; `restart` starts
    /// a new shell when the last one exited.
    pub fn attach_terminal(
        &self,
        thread_id: &str,
        terminal_id: &str,
        size: Option<(u16, u16)>,
        restart: bool,
    ) -> TerminalSession {
        let shared = Arc::clone(&self.inner.shared);
        let payload = encode(&TerminalAttachInput {
            thread_id: thread_id.to_owned(),
            terminal_id: terminal_id.to_owned(),
            cwd: None,
            worktree_path: None,
            cols: size.map(|(cols, _)| cols),
            rows: size.map(|(_, rows)| rows),
            env: None,
            restart_if_not_running: restart.then_some(true),
        });
        let (tx, events) = mpsc::unbounded_channel();
        let task = self.inner.shared.runtime.spawn(async move {
            if let Err(error) = drive_terminal(&shared, payload, &tx).await {
                let _ = tx.send(Err(error));
            }
        });
        TerminalSession {
            events,
            task: task.abort_handle(),
        }
    }

    /// Sends input to a terminal's shell (`terminal.write`).
    pub async fn write_terminal(
        &self,
        thread_id: &str,
        terminal_id: &str,
        data: String,
    ) -> Result<(), Error> {
        self.call::<Value>(
            TERMINAL_WRITE,
            &TerminalWriteInput {
                thread_id: thread_id.to_owned(),
                terminal_id: terminal_id.to_owned(),
                data,
            },
        )
        .await
        .map(drop)
    }

    /// Resizes a terminal (`terminal.resize`).
    pub async fn resize_terminal(
        &self,
        thread_id: &str,
        terminal_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<(), Error> {
        self.call::<Value>(
            TERMINAL_RESIZE,
            &TerminalResizeInput {
                thread_id: thread_id.to_owned(),
                terminal_id: terminal_id.to_owned(),
                cols,
                rows,
            },
        )
        .await
        .map(drop)
    }

    /// Ends a terminal's shell (`terminal.close`); its attached streams end
    /// with `closed`.
    pub async fn close_terminal(&self, thread_id: &str, terminal_id: &str) -> Result<(), Error> {
        self.call::<Value>(
            TERMINAL_CLOSE,
            &TerminalCloseInput {
                thread_id: thread_id.to_owned(),
                terminal_id: Some(terminal_id.to_owned()),
                delete_history: None,
            },
        )
        .await
        .map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_collapse_whitespace_and_truncate() {
        assert_eq!(title_from_prompt("  fix\n the   bug ", &[]), "fix the bug");
        assert_eq!(title_from_prompt("   ", &[]), "New thread");
        let long = "word ".repeat(30);
        let title = title_from_prompt(&long, &[]);
        assert!(title.ends_with("..."));
        assert!(title.chars().count() <= 72);
    }
}
