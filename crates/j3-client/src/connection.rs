//! The public handle to one server and its typed calls.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, PoisonError, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use j3_protocol::v2::{
    entities::Attachment,
    envelope,
    methods::{self, *},
    streams::ThreadSnapshot,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};

use crate::{
    ConnectionState, Error, ThreadView,
    supervisor::{self, Command, Shared, decode, encode},
};

const UPLOAD_TIMEOUT: Duration = Duration::from_secs(120);
/// Stream items buffered between the socket and a [`GitRun`], [`BtwRun`] or
/// [`TerminalSession`] consumer; beyond it the stream stops acking.
const STREAM_BUFFER: usize = 16;

/// One j3 server. Cheap to clone; the supervisor stops when the last clone
/// (and every [`ThreadWatch`]) is dropped, or on [`Connection::close`].
#[derive(Clone)]
pub struct Connection {
    inner: Arc<Inner>,
}

struct Inner {
    shared: Arc<Shared>,
    commands: mpsc::UnboundedSender<Command>,
    threads: Mutex<HashMap<String, Weak<Subscription>>>,
    generations: AtomicU64,
}

/// A live `threads.watch` of one thread. Cheap to clone; the subscription
/// runs while any clone is alive and resumes after reconnects.
#[derive(Clone)]
pub struct ThreadWatch {
    subscription: Arc<Subscription>,
}

struct Subscription {
    thread_id: String,
    generation: u64,
    state: watch::Receiver<Arc<ThreadView>>,
    commands: mpsc::UnboundedSender<Command>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Unwatch {
            thread_id: std::mem::take(&mut self.thread_id),
            generation: self.generation,
        });
    }
}

impl ThreadWatch {
    pub fn thread_id(&self) -> &str {
        &self.subscription.thread_id
    }

    pub fn state(&self) -> watch::Receiver<Arc<ThreadView>> {
        self.subscription.state.clone()
    }
}

/// The items of a stream call, forwarded from the connection's runtime.
/// Dropping it cancels the call.
struct Items<T> {
    rx: mpsc::Receiver<Result<T, Error>>,
    task: tokio::task::AbortHandle,
}

impl<T: DeserializeOwned + Send + 'static> Items<T> {
    fn start(shared: &Arc<Shared>, method: &'static str, params: Value) -> Self {
        let (tx, rx) = mpsc::channel(STREAM_BUFFER);
        let task = shared
            .runtime
            .spawn(forward(Arc::clone(shared), method, params, tx));
        Self {
            rx,
            task: task.abort_handle(),
        }
    }
}

impl<T> Drop for Items<T> {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Sends a stream's items to `tx` until it ends, fails, or `tx` is gone.
/// Aborting the task drops the stream, which cancels the call.
async fn forward<T: DeserializeOwned>(
    shared: Arc<Shared>,
    method: &'static str,
    params: Value,
    tx: mpsc::Sender<Result<T, Error>>,
) {
    let outcome = async {
        let session = shared.ready_session().await?;
        let mut stream = session.stream(method, params)?;
        while let Some(values) = stream.next().await {
            for value in values? {
                if tx.send(Ok(decode(value, method)?)).await.is_err() {
                    return Ok(());
                }
            }
        }
        Ok(())
    };
    if let Err(error) = outcome.await {
        let _ = tx.send(Err(error)).await;
    }
}

/// A running `git.run`. Dropping it cancels the call.
pub struct GitRun(Items<GitRunItem>);

impl GitRun {
    /// The next item, from any executor: `phase`s, then `finished`. `None`
    /// once the stream ended; a failure (its `data` has the `phase`) is
    /// always last.
    pub async fn next(&mut self) -> Option<Result<GitRunItem, Error>> {
        self.0.rx.recv().await
    }
}

/// A running `btw.ask`. Dropping it cancels the side question.
pub struct BtwRun(Items<BtwItem>);

impl BtwRun {
    /// The next item, from any executor: the topic as it starts, the
    /// answer's text as it grows, then the finished topic. `None` once the
    /// stream ended; a failure is always last.
    pub async fn next(&mut self) -> Option<Result<BtwItem, Error>> {
        self.0.rx.recv().await
    }
}

/// Applies a `btw.ask` item to the topic a client shows: a `record`
/// replaces it, a `delta` grows its last answer.
pub fn apply_btw(topic: &mut Option<BtwRecord>, item: BtwItem) {
    match item {
        BtwItem::Record { record } => *topic = Some(record),
        BtwItem::Delta { record_id, append } => {
            if let Some(turn) = topic
                .as_mut()
                .filter(|topic| topic.id == record_id)
                .and_then(|topic| topic.turns.last_mut())
            {
                turn.answer.push_str(&append);
            }
        }
    }
}

/// An attached terminal (`terminal.attach`). Dropping it detaches; the
/// shell keeps running on the server.
pub struct TerminalSession(Items<TerminalItem>);

impl TerminalSession {
    /// The next item, from any executor: `snapshot` first, then live
    /// events. `None` once the stream ended; a failure is always last.
    pub async fn next(&mut self) -> Option<Result<TerminalItem, Error>> {
        self.0.rx.recv().await
    }
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

impl Connection {
    /// Starts the supervisor for `http_base` (e.g. `"http://shieldy:3780"`)
    /// on `runtime`. It connects, reconnects with backoff (1 s → 30 s, reset
    /// after a stable connection), pings and resubscribes until closed.
    pub fn open(http_base: impl Into<String>, runtime: tokio::runtime::Handle) -> Self {
        let shared = Arc::new(Shared::new(http_base.into(), runtime.clone()));
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

    /// A connection that never connects: calls fail with
    /// [`Error::Disconnected`] at once and its state stays the default.
    /// For fixtures and offline previews.
    pub fn offline(http_base: impl Into<String>, runtime: tokio::runtime::Handle) -> Self {
        let shared = Arc::new(Shared::new(http_base.into(), runtime));
        shared.session.send_replace(supervisor::SessionSlot::Closed);
        let (commands, _) = mpsc::unbounded_channel();
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

    /// Status, server info, catalog and shell.
    pub fn state(&self) -> watch::Receiver<Arc<ConnectionState>> {
        self.inner.shared.state.subscribe()
    }

    /// The live state of a thread. The subscription runs while any
    /// [`ThreadWatch`] of the thread is alive and resumes after reconnects.
    pub fn thread(&self, thread_id: &str) -> ThreadWatch {
        let mut threads = self
            .inner
            .threads
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(subscription) = threads.get(thread_id).and_then(Weak::upgrade) {
            return ThreadWatch { subscription };
        }
        threads.retain(|_, subscription| subscription.strong_count() > 0);
        let generation = self.inner.generations.fetch_add(1, Ordering::Relaxed);
        let (state, rx) = watch::channel(Arc::new(ThreadView::new(thread_id.to_owned())));
        let _ = self.inner.commands.send(Command::Watch {
            thread_id: thread_id.to_owned(),
            generation,
            state,
        });
        let subscription = Arc::new(Subscription {
            thread_id: thread_id.to_owned(),
            generation,
            state: rx,
            commands: self.inner.commands.clone(),
        });
        threads.insert(thread_id.to_owned(), Arc::downgrade(&subscription));
        ThreadWatch { subscription }
    }

    /// Stops the supervisor and closes the socket. Later calls fail with
    /// [`Error::Disconnected`].
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
            Err(_) => Err(Error::Disconnected),
        }
    }

    async fn call<T: DeserializeOwned + Send + 'static>(
        &self,
        method: &'static str,
        params: &impl Serialize,
    ) -> Result<T, Error> {
        let shared = Arc::clone(&self.inner.shared);
        let params = encode(params);
        self.spawn(async move {
            let session = shared.ready_session().await?;
            decode(session.call(method, params).await?, method)
        })
        .await
    }

    // -----------------------------------------------------------------------
    // Streams
    // -----------------------------------------------------------------------

    /// Starts `git.run` on the connection's runtime. A fresh `commandId` is
    /// used when `params` has none.
    pub fn git_run(&self, mut params: GitRunParams) -> GitRun {
        params.command_id.get_or_insert_with(new_id);
        GitRun(Items::start(&self.inner.shared, GIT_RUN, encode(&params)))
    }

    /// Asks a side question (`btw.ask`) on the connection's runtime.
    pub fn btw_ask(&self, params: BtwAskParams) -> BtwRun {
        BtwRun(Items::start(&self.inner.shared, BTW_ASK, encode(&params)))
    }

    /// Attaches to a thread's terminal (`terminal.attach`) on the
    /// connection's runtime, starting its shell when it has none.
    pub fn attach_terminal(&self, params: AttachParams) -> TerminalSession {
        TerminalSession(Items::start(
            &self.inner.shared,
            TERMINAL_ATTACH,
            encode(&params),
        ))
    }

    // -----------------------------------------------------------------------
    // HTTP (wire.md §8)
    // -----------------------------------------------------------------------

    /// Uploads a file (`POST /api/v1/attachments`) for `turns.send`,
    /// `threads.create` and `queue.edit`. A closed or [offline](Self::offline)
    /// connection fails at once, as its calls do, without reaching the server.
    pub async fn upload(
        &self,
        name: &str,
        mime_type: &str,
        bytes: Vec<u8>,
    ) -> Result<Attachment, Error> {
        let shared = Arc::clone(&self.inner.shared);
        let (name, mime_type) = (name.to_owned(), mime_type.to_owned());
        if matches!(*shared.session.borrow(), supervisor::SessionSlot::Closed) {
            return Err(Error::Disconnected);
        }
        self.spawn(async move {
            let mut url = shared.url("/api/v1/attachments")?;
            url.query_pairs_mut().append_pair("name", &name);
            let response = shared
                .http
                .post(url)
                .header(reqwest::header::CONTENT_TYPE, mime_type)
                .body(bytes)
                .timeout(UPLOAD_TIMEOUT)
                .send()
                .await
                .map_err(|e| Error::Http(format!("uploading {name}: {e}")))?;
            let status = response.status();
            let body = response
                .bytes()
                .await
                .map_err(|e| Error::Http(format!("uploading {name}: {e}")))?;
            if status.is_success() {
                return serde_json::from_slice(&body)
                    .map_err(|e| Error::Decode(format!("upload: {e}")));
            }
            Err(match serde_json::from_slice::<envelope::Error>(&body) {
                Ok(error) => Error::Server(error),
                Err(_) => Error::Http(format!(
                    "uploading {name}: the server answered {status}: {}",
                    String::from_utf8_lossy(&body).trim()
                )),
            })
        })
        .await
    }

    /// The absolute URL serving attachment `attachment_id`'s bytes.
    pub fn attachment_url(&self, attachment_id: &str) -> String {
        match self.inner.shared.url("/") {
            Ok(mut url) => {
                url.set_query(None);
                url.set_fragment(None);
                if let Ok(mut segments) = url.path_segments_mut() {
                    segments
                        .clear()
                        .extend(["api", "v1", "attachments", attachment_id]);
                }
                url.into()
            }
            Err(_) => self.url(&format!("/api/v1/attachments/{attachment_id}")),
        }
    }

    /// `path` (absolute, e.g. a `projects.imageUrl` `/api/v1/assets/…` URL)
    /// as an absolute URL on the server.
    pub fn url(&self, path: &str) -> String {
        match self.inner.shared.url(path) {
            Ok(url) => url.into(),
            Err(_) => format!("{}{path}", self.http_base().trim_end_matches('/')),
        }
    }

    // -----------------------------------------------------------------------
    // Unary calls without params
    // -----------------------------------------------------------------------

    /// `server.usage`: provider usage limits from `omp usage --json` on the
    /// server host.
    pub async fn server_usage(&self) -> Result<Usage, Error> {
        self.call(SERVER_USAGE, &Empty {}).await
    }
}

/// One typed method per unary call of wire.md §5. `command` marks
/// state-changing methods: their `commandId` (wire.md §3.4) is filled with a
/// fresh uuid when the caller passes `None`.
macro_rules! unary {
    ($( $(#[$doc:meta])* $name:ident = $method:ident($params:ty) -> $result:ty $(, $command:ident)? ;)*) => {
        impl Connection {
            $(
                $(#[$doc])*
                pub async fn $name(&self, params: $params) -> Result<$result, Error> {
                    #[allow(unused_mut)]
                    let mut params = params;
                    $( params.$command.get_or_insert_with(new_id); )?
                    self.call(methods::$method, &params).await
                }
            )*
        }
    };
}

unary! {
    /// `server.refresh`: the catalog, with `projectId`'s slash commands.
    server_refresh = SERVER_REFRESH(RefreshParams) -> Catalog;

    /// `projects.create`.
    projects_create = PROJECTS_CREATE(CreateProjectParams) -> ProjectResult, command_id;
    /// `projects.update`.
    projects_update = PROJECTS_UPDATE(UpdateProjectParams) -> ProjectResult, command_id;
    /// `projects.delete`.
    projects_delete = PROJECTS_DELETE(DeleteProjectParams) -> Sequenced, command_id;
    /// `projects.browse`: directories under the server user's home.
    projects_browse = PROJECTS_BROWSE(BrowseParams) -> BrowseResult;
    /// `projects.listEntries`.
    projects_list_entries = PROJECTS_LIST_ENTRIES(ListEntriesParams) -> EntriesResult;
    /// `projects.searchEntries`.
    projects_search_entries = PROJECTS_SEARCH_ENTRIES(SearchEntriesParams) -> EntriesResult;
    /// `projects.readFile`.
    projects_read_file = PROJECTS_READ_FILE(ReadFileParams) -> ReadFileResult;
    /// `projects.imageUrl`; pass the result's `url` through [`Connection::url`].
    projects_image_url = PROJECTS_IMAGE_URL(ImageUrlParams) -> ImageUrlResult;

    /// `threads.create`: the first turn starts at once.
    threads_create = THREADS_CREATE(CreateThreadParams) -> ThreadResult, command_id;
    /// `threads.get`: a snapshot without subscribing.
    threads_get = THREADS_GET(ThreadParams) -> ThreadSnapshot;
    /// `threads.rename`.
    threads_rename = THREADS_RENAME(RenameThreadParams) -> Sequenced, command_id;
    /// `threads.archive`.
    threads_archive = THREADS_ARCHIVE(ThreadParams) -> Sequenced, command_id;
    /// `threads.unarchive`.
    threads_unarchive = THREADS_UNARCHIVE(ThreadParams) -> Sequenced, command_id;
    /// `threads.delete`.
    threads_delete = THREADS_DELETE(DeleteThreadParams) -> Sequenced, command_id;
    /// `threads.setMode`.
    threads_set_mode = THREADS_SET_MODE(SetModeParams) -> Sequenced, command_id;

    /// `turns.send`.
    turns_send = TURNS_SEND(SendParams) -> SendResult, command_id;
    /// `turns.stop`.
    turns_stop = TURNS_STOP(StopParams) -> Sequenced, command_id;

    /// `requests.answer`.
    requests_answer = REQUESTS_ANSWER(AnswerParams) -> Sequenced, command_id;
    /// `requests.dismiss`.
    requests_dismiss = REQUESTS_DISMISS(DismissParams) -> Sequenced, command_id;

    /// `queue.edit`.
    queue_edit = QUEUE_EDIT(QueueEditParams) -> Sequenced, command_id;
    /// `queue.cancel`.
    queue_cancel = QUEUE_CANCEL(QueueEntryParams) -> Sequenced, command_id;
    /// `queue.steer`.
    queue_steer = QUEUE_STEER(QueueEntryParams) -> Sequenced, command_id;
    /// `queue.start`.
    queue_start = QUEUE_START(QueueEntryParams) -> Sequenced, command_id;
    /// `queue.resume`.
    queue_resume = QUEUE_RESUME(ThreadParams) -> Sequenced, command_id;

    /// `triage.visit`.
    triage_visit = TRIAGE_VISIT(VisitParams) -> Sequenced, command_id;
    /// `triage.markUnread`.
    triage_mark_unread = TRIAGE_MARK_UNREAD(ThreadParams) -> Sequenced, command_id;
    /// `triage.settle`.
    triage_settle = TRIAGE_SETTLE(SettleParams) -> Sequenced, command_id;
    /// `triage.unsettle`.
    triage_unsettle = TRIAGE_UNSETTLE(ThreadParams) -> Sequenced, command_id;
    /// `triage.snooze`.
    triage_snooze = TRIAGE_SNOOZE(SnoozeParams) -> Sequenced, command_id;
    /// `triage.wake`.
    triage_wake = TRIAGE_WAKE(ThreadParams) -> Sequenced, command_id;
    /// `triage.pin`.
    triage_pin = TRIAGE_PIN(PinParams) -> Sequenced, command_id;
    /// `triage.unpin`.
    triage_unpin = TRIAGE_UNPIN(ThreadParams) -> Sequenced, command_id;
    /// `triage.orderPinned`.
    triage_order_pinned = TRIAGE_ORDER_PINNED(OrderParams) -> Sequenced, command_id;
    /// `triage.orderActive`.
    triage_order_active = TRIAGE_ORDER_ACTIVE(OrderParams) -> Sequenced, command_id;
    /// `triage.setAutoSettle`.
    triage_set_auto_settle = TRIAGE_SET_AUTO_SETTLE(AutoSettleParams) -> Sequenced, command_id;

    /// `goals.create`.
    goals_create = GOALS_CREATE(CreateGoalParams) -> Sequenced, command_id;
    /// `goals.pause`.
    goals_pause = GOALS_PAUSE(ThreadParams) -> Sequenced, command_id;
    /// `goals.resume`.
    goals_resume = GOALS_RESUME(ThreadParams) -> Sequenced, command_id;
    /// `goals.drop`.
    goals_drop = GOALS_DROP(ThreadParams) -> Sequenced, command_id;

    /// `subagents.stop`.
    subagents_stop = SUBAGENTS_STOP(StopSubagentParams) -> Sequenced, command_id;
    /// `subagents.transcript`: entries from byte `fromByte`; pass `nextByte`
    /// to continue.
    subagents_transcript = SUBAGENTS_TRANSCRIPT(TranscriptParams) -> TranscriptResult;

    /// `git.status`.
    git_status = GIT_STATUS(ProjectParams) -> GitStatus;
    /// `git.pull`: fast-forward only.
    git_pull = GIT_PULL(ProjectParams) -> PullResult, command_id;

    /// `terminal.write`.
    terminal_write = TERMINAL_WRITE(TerminalWriteParams) -> Empty;
    /// `terminal.resize`.
    terminal_resize = TERMINAL_RESIZE(TerminalResizeParams) -> Empty;
    /// `terminal.close`.
    terminal_close = TERMINAL_CLOSE(TerminalCloseParams) -> Empty;

    /// `attachments.delete`.
    attachments_delete = ATTACHMENTS_DELETE(DeleteAttachmentParams) -> Empty, command_id;
}
