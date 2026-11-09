//! The connection supervisor: one task per [`crate::Connection`] that
//! connects, keeps the catalog, shell and watched threads current, and
//! reconnects with backoff until closed.
//!
//! Per attempt: `GET /api/v1/info` (protocol [`PROTOCOL`] required), the
//! WebSocket `/api/v1/ws?protocol=2`, `server.watch` (its first item is the
//! catalog), then `shell.watch` and one `threads.watch` per watched thread.
//! Resubscriptions pass `after` = the applied sequence when state is loaded
//! (wire.md §6.2).

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use j3_protocol::v2::{
    PROTOCOL,
    envelope::ErrorCode,
    methods::{self, Catalog, WatchParams, WatchThreadParams},
    streams::{ShellItem, ThreadItem},
};
use reqwest::{StatusCode, Url};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
    time::Instant,
};
use tracing::{debug, info, warn};

use crate::{
    ConnectionState, Error, Info, Status, ThreadSync, ThreadView,
    rpc::{Failure, RpcStream, Session},
    state::Cursor,
};

/// Reconnect backoff: 1 s doubling to 30 s.
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// A connection that lasted this long resets the backoff.
const STABLE_AFTER: Duration = Duration::from_secs(30);
const INFO_TIMEOUT: Duration = Duration::from_secs(10);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// Resubscribing a failed stream on a live socket: 250 ms doubling to 30 s.
const RESUBSCRIBE_MIN: Duration = Duration::from_millis(250);
const RESUBSCRIBE_MAX: Duration = Duration::from_secs(30);
/// How long a call waits for a connection before failing.
const CALL_WAIT: Duration = Duration::from_secs(10);

/// What calls see of the socket.
#[derive(Clone)]
pub(crate) enum SessionSlot {
    Down,
    Up(Session),
    Closed,
}

/// State shared by the public handles and the supervisor task.
pub(crate) struct Shared {
    pub(crate) http_base: String,
    /// `http_base` parsed, or why it could not be.
    pub(crate) base: Result<Url, String>,
    pub(crate) runtime: tokio::runtime::Handle,
    pub(crate) http: reqwest::Client,
    pub(crate) state: watch::Sender<Arc<ConnectionState>>,
    pub(crate) session: watch::Sender<SessionSlot>,
    /// A shell snapshot was applied, so `shell.sequence` can resume.
    shell_loaded: AtomicBool,
}

impl Shared {
    pub(crate) fn new(http_base: String, runtime: tokio::runtime::Handle) -> Self {
        let base = Url::parse(&http_base)
            .map_err(|e| format!("invalid server URL {http_base:?}: {e}"))
            .and_then(|url| match url.scheme() {
                "http" | "https" => Ok(url),
                scheme => Err(format!(
                    "invalid server URL {http_base:?}: expected http or https, not {scheme}"
                )),
            });
        Self {
            http_base,
            base,
            runtime,
            http: reqwest::Client::new(),
            state: watch::Sender::new(Arc::new(ConnectionState::default())),
            session: watch::Sender::new(SessionSlot::Down),
            shell_loaded: AtomicBool::new(false),
        }
    }

    pub(crate) fn update(&self, change: impl FnOnce(&mut ConnectionState)) {
        self.state.send_modify(|state| change(Arc::make_mut(state)));
    }

    /// `path` (absolute, e.g. `/api/v1/info`) on the server.
    pub(crate) fn url(&self, path: &str) -> Result<Url, Error> {
        let base = self.base.as_ref().map_err(|e| Error::Http(e.clone()))?;
        base.join(path)
            .map_err(|e| Error::Http(format!("invalid URL path {path:?}: {e}")))
    }

    /// The live session, waiting up to [`CALL_WAIT`] for one.
    pub(crate) async fn ready_session(&self) -> Result<Session, Error> {
        let mut slots = self.session.subscribe();
        let ready = slots.wait_for(|slot| !matches!(slot, SessionSlot::Down));
        match tokio::time::timeout(CALL_WAIT, ready).await {
            Ok(Ok(slot)) => match &*slot {
                SessionSlot::Up(session) => Ok(session.clone()),
                SessionSlot::Down | SessionSlot::Closed => Err(Error::Disconnected),
            },
            Ok(Err(_)) | Err(_) => Err(Error::Disconnected),
        }
    }
}

pub(crate) enum Command {
    Watch {
        thread_id: String,
        generation: u64,
        state: watch::Sender<Arc<ThreadView>>,
    },
    Unwatch {
        thread_id: String,
        generation: u64,
    },
    Close(oneshot::Sender<()>),
}

struct Slot {
    generation: u64,
    state: watch::Sender<Arc<ThreadView>>,
    task: Option<JoinHandle<()>>,
}

struct Live {
    session: Session,
    tasks: Vec<JoinHandle<()>>,
}

struct Supervisor {
    shared: Arc<Shared>,
    slots: HashMap<String, Slot>,
    live: Option<Live>,
}

enum Flow {
    Continue,
    Stop(Option<oneshot::Sender<()>>),
}

/// Runs until [`Command::Close`] or until every handle is dropped.
pub(crate) async fn run(shared: Arc<Shared>, mut commands: mpsc::UnboundedReceiver<Command>) {
    let mut supervisor = Supervisor {
        shared,
        slots: HashMap::new(),
        live: None,
    };
    let mut failures: u32 = 0;
    loop {
        let attempt = connect(Arc::clone(&supervisor.shared));
        tokio::pin!(attempt);
        let result = loop {
            tokio::select! {
                result = &mut attempt => break result,
                command = commands.recv() => {
                    if let Flow::Stop(ack) = supervisor.command(command) {
                        return supervisor.stop(ack);
                    }
                }
            }
        };

        let delay = match result {
            Ok(connected) => {
                let connected_at = Instant::now();
                let mut socket = supervisor.go_live(connected);
                let reason = loop {
                    tokio::select! {
                        reason = &mut socket => {
                            break reason.unwrap_or_else(|e| format!("socket task failed: {e}"));
                        }
                        command = commands.recv() => {
                            if let Flow::Stop(ack) = supervisor.command(command) {
                                return supervisor.stop(ack);
                            }
                        }
                    }
                };
                supervisor.go_down();
                if connected_at.elapsed() >= STABLE_AFTER {
                    failures = 0;
                }
                info!(base = %supervisor.shared.http_base, %reason, "disconnected");
                supervisor.reconnecting(&mut failures, reason)
            }
            Err(Failure::Failed(error)) => {
                debug!(base = %supervisor.shared.http_base, %error, "connecting failed");
                supervisor.reconnecting(&mut failures, error)
            }
            Err(Failure::Incompatible(message)) => {
                warn!(base = %supervisor.shared.http_base, %message, "incompatible server");
                supervisor
                    .shared
                    .update(|state| state.status = Status::Incompatible { message });
                BACKOFF_MAX
            }
        };
        let sleep = tokio::time::sleep(delay);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                () = &mut sleep => break,
                command = commands.recv() => {
                    if let Flow::Stop(ack) = supervisor.command(command) {
                        return supervisor.stop(ack);
                    }
                }
            }
        }
    }
}

impl Supervisor {
    /// Publishes a failed attempt; the delay before the next.
    fn reconnecting(&self, failures: &mut u32, error: String) -> Duration {
        *failures = failures.saturating_add(1);
        let attempt = *failures;
        self.shared
            .update(|state| state.status = Status::Reconnecting { attempt, error });
        BACKOFF_MIN
            .saturating_mul(1 << (attempt - 1).min(5))
            .min(BACKOFF_MAX)
    }

    fn command(&mut self, command: Option<Command>) -> Flow {
        match command {
            None => Flow::Stop(None),
            Some(Command::Close(ack)) => Flow::Stop(Some(ack)),
            Some(Command::Watch {
                thread_id,
                generation,
                state,
            }) => {
                if let Some(previous) = self.slots.remove(&thread_id) {
                    if let Some(task) = previous.task {
                        task.abort();
                    }
                    // The handle was dropped and reopened before the unwatch
                    // arrived: carry the state over.
                    if state.borrow().thread.is_none() {
                        state.send_replace(previous.state.borrow().clone());
                    }
                }
                let task = self
                    .live
                    .as_ref()
                    .and_then(|live| spawn_thread(&live.session, &state));
                self.slots.insert(
                    thread_id,
                    Slot {
                        generation,
                        state,
                        task,
                    },
                );
                Flow::Continue
            }
            Some(Command::Unwatch {
                thread_id,
                generation,
            }) => {
                if self
                    .slots
                    .get(&thread_id)
                    .is_some_and(|slot| slot.generation == generation)
                    && let Some(slot) = self.slots.remove(&thread_id)
                    && let Some(task) = slot.task
                {
                    task.abort();
                }
                Flow::Continue
            }
        }
    }

    /// Publishes the session and starts its subscriptions; returns the socket
    /// task, which resolves when the session ends.
    fn go_live(&mut self, connected: Connected) -> JoinHandle<String> {
        let Connected {
            session,
            socket,
            info,
            catalog,
            catalog_stream,
        } = connected;
        let other_server = self
            .shared
            .state
            .borrow()
            .info
            .as_ref()
            .is_some_and(|known| known.server_id != info.server_id);
        if other_server {
            // A different server answers at this address: nothing carried
            // over applies to it.
            warn!(base = %self.shared.http_base, "the server id changed");
            self.shared.shell_loaded.store(false, Ordering::Relaxed);
            for slot in self.slots.values() {
                let thread_id = slot.state.borrow().thread_id.clone();
                slot.state
                    .send_replace(Arc::new(ThreadView::new(thread_id)));
            }
        }
        self.shared.update(|state| {
            if other_server {
                state.shell = Default::default();
            }
            state.info = Some(info);
            state.catalog = Some(catalog);
            state.status = Status::Connected;
            state.shell.synced = false;
        });
        self.shared
            .session
            .send_replace(SessionSlot::Up(session.clone()));
        let tasks = vec![
            tokio::spawn(run_catalog(
                session.clone(),
                catalog_stream,
                Arc::clone(&self.shared),
            )),
            tokio::spawn(run_shell(session.clone(), Arc::clone(&self.shared))),
        ];
        for slot in self.slots.values_mut() {
            slot.task = spawn_thread(&session, &slot.state);
        }
        info!(base = %self.shared.http_base, "connected");
        self.live = Some(Live { session, tasks });
        socket
    }

    fn end_live(&mut self) {
        if let Some(live) = self.live.take() {
            live.session.close();
            for task in live.tasks {
                task.abort();
            }
        }
        for slot in self.slots.values_mut() {
            if let Some(task) = slot.task.take() {
                task.abort();
            }
        }
    }

    fn go_down(&mut self) {
        self.end_live();
        self.shared.session.send_replace(SessionSlot::Down);
        for slot in self.slots.values() {
            slot.state.send_if_modified(|view| {
                if view.sync == ThreadSync::Synced {
                    Arc::make_mut(view).sync = ThreadSync::Loading;
                    true
                } else {
                    false
                }
            });
        }
        self.shared.state.send_if_modified(|state| {
            if state.shell.synced {
                Arc::make_mut(state).shell.synced = false;
                true
            } else {
                false
            }
        });
    }

    fn stop(mut self, ack: Option<oneshot::Sender<()>>) {
        self.end_live();
        self.shared.session.send_replace(SessionSlot::Closed);
        self.shared.update(|state| {
            state.status = Status::Closed;
            state.shell.synced = false;
        });
        if let Some(ack) = ack {
            let _ = ack.send(());
        }
    }
}

struct Connected {
    session: Session,
    socket: JoinHandle<String>,
    info: Info,
    catalog: Catalog,
    catalog_stream: RpcStream,
}

/// One connection attempt, through the first catalog.
async fn connect(shared: Arc<Shared>) -> Result<Connected, Failure> {
    let info = fetch_info(&shared).await?;
    let base = shared
        .base
        .as_ref()
        .map_err(|e| Failure::Failed(e.clone()))?;
    let (session, socket) = Session::connect(socket_url(base).as_str()).await?;
    let handshake = async {
        let mut stream = session
            .stream(methods::SERVER_WATCH, Value::Object(Default::default()))
            .map_err(|e| e.to_string())?;
        let first = loop {
            match stream.next().await {
                Some(Ok(items)) => {
                    if let Some(first) = items.into_iter().next() {
                        break first;
                    }
                }
                Some(Err(e)) => return Err(format!("server.watch: {e}")),
                None => return Err("server.watch ended before the catalog".to_owned()),
            }
        };
        let catalog: Catalog = decode(first, "catalog").map_err(|e| e.to_string())?;
        Ok((catalog, stream))
    };
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake).await {
        Ok(Ok((catalog, catalog_stream))) => Ok(Connected {
            session,
            socket,
            info,
            catalog,
            catalog_stream,
        }),
        Ok(Err(error)) => {
            session.close();
            Err(Failure::Failed(error))
        }
        Err(_) => {
            session.close();
            Err(Failure::Failed(
                "the server did not send its catalog in time".to_owned(),
            ))
        }
    }
}

/// `GET /api/v1/info`; a server without it, or with another `protocol`, is
/// incompatible.
async fn fetch_info(shared: &Shared) -> Result<Info, Failure> {
    let url = shared
        .url("/api/v1/info")
        .map_err(|e| Failure::Failed(e.to_string()))?;
    let response = shared
        .http
        .get(url.clone())
        .timeout(INFO_TIMEOUT)
        .send()
        .await
        .map_err(|e| Failure::Failed(format!("could not reach {}: {e}", shared.http_base)))?;
    let status = response.status();
    if status == StatusCode::NOT_FOUND {
        return Err(Failure::Incompatible(format!(
            "{} does not speak j3 protocol {PROTOCOL} (no /api/v1/info); update the server.",
            shared.http_base
        )));
    }
    if !status.is_success() {
        return Err(Failure::Failed(format!("{url} answered {status}")));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|e| Failure::Failed(format!("{url} sent unexpected data: {e}")))?;
    let protocol = body.get("protocol").and_then(Value::as_u64);
    if protocol != Some(u64::from(PROTOCOL)) {
        let speaks = protocol.map_or_else(
            || "an unknown protocol".to_owned(),
            |p| format!("protocol {p}"),
        );
        return Err(Failure::Incompatible(format!(
            "{} speaks j3 {speaks}; this app needs protocol {PROTOCOL}. Update the {}.",
            shared.http_base,
            if protocol.is_some_and(|p| p > u64::from(PROTOCOL)) {
                "app"
            } else {
                "server"
            },
        )));
    }
    serde_json::from_value(body)
        .map_err(|e| Failure::Failed(format!("{url} sent an unexpected info body: {e}")))
}

/// `ws[s]://host[:port]/api/v1/ws?protocol=2`.
fn socket_url(base: &Url) -> Url {
    let mut url = base.clone();
    let scheme = if base.scheme() == "https" {
        "wss"
    } else {
        "ws"
    };
    url.set_scheme(scheme)
        .expect("http(s) URLs can switch to ws(s)");
    url.set_path("/api/v1/ws");
    url.set_query(Some(&format!("protocol={PROTOCOL}")));
    url.set_fragment(None);
    url
}

pub(crate) fn encode<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("wire types always encode")
}

pub(crate) fn decode<T: DeserializeOwned>(value: Value, what: &str) -> Result<T, Error> {
    serde_json::from_value(value).map_err(|e| Error::Decode(format!("{what}: {e}")))
}

/// Decodes a batch of stream items. An item whose `type` this build does not
/// know (a newer server) is skipped; a known one that fails to decode fails
/// the batch.
fn decode_items<T: DeserializeOwned>(
    values: Vec<Value>,
    known: &[&str],
    what: &str,
) -> Result<Vec<T>, Error> {
    let mut items = Vec::with_capacity(values.len());
    for value in values {
        let kind = value.get("type").and_then(Value::as_str);
        if !kind.is_some_and(|kind| known.contains(&kind)) {
            debug!(?kind, "skipping an unknown {what}");
            continue;
        }
        items.push(decode(value, what)?);
    }
    Ok(items)
}

const SHELL_ITEMS: &[&str] = &[
    "snapshot",
    "synced",
    "project",
    "project.removed",
    "thread",
    "thread.removed",
];

const THREAD_ITEMS: &[&str] = &[
    "snapshot",
    "synced",
    "thread",
    "turn",
    "item",
    "item.delta",
    "request",
    "queue",
    "thread.removed",
];

fn resubscribe_delay(failures: u32) -> Duration {
    RESUBSCRIBE_MIN
        .saturating_mul(1 << failures.min(7))
        .min(RESUBSCRIBE_MAX)
}

/// Keeps `catalog` current after the handshake. The stream lives as long as
/// the socket; if it ends, the session is dropped and reconnected.
async fn run_catalog(session: Session, mut stream: RpcStream, shared: Arc<Shared>) {
    let reason = loop {
        match stream.next().await {
            Some(Ok(items)) => {
                // Each item is a whole catalog; the last one wins.
                let Some(last) = items.into_iter().next_back() else {
                    continue;
                };
                match decode::<Catalog>(last, "catalog") {
                    Ok(catalog) => shared.update(|state| state.catalog = Some(catalog)),
                    Err(error) => break error.to_string(),
                }
            }
            Some(Err(Error::Disconnected)) => return,
            Some(Err(e)) => break format!("server.watch: {e}"),
            None => break "server.watch ended".to_owned(),
        }
    };
    warn!(base = %shared.http_base, %reason, "dropping the session");
    session.close();
}

/// Keeps the shell current; resubscribes on a failed stream.
async fn run_shell(session: Session, shared: Arc<Shared>) {
    let mut failures = 0;
    loop {
        let after = shared
            .shell_loaded
            .load(Ordering::Relaxed)
            .then(|| shared.state.borrow().shell.sequence);
        let mut cursor = Cursor::resume(after);
        let Ok(mut stream) = session.stream(methods::SHELL_WATCH, encode(&WatchParams { after }))
        else {
            return;
        };
        let error = loop {
            let items = match stream.next().await {
                Some(Ok(values)) => {
                    match decode_items::<ShellItem>(values, SHELL_ITEMS, "shell item") {
                        Ok(items) => items,
                        Err(error) => break error,
                    }
                }
                Some(Err(error)) => break error,
                None => break Error::Decode("shell.watch ended".into()),
            };
            failures = 0;
            let mut loaded = false;
            shared.state.send_if_modified(|state| {
                let shell = &mut Arc::make_mut(state).shell;
                let mut changed = false;
                for item in items {
                    loaded |= matches!(item, ShellItem::Snapshot(_));
                    changed |= shell.apply(&mut cursor, item);
                }
                changed
            });
            if loaded {
                shared.shell_loaded.store(true, Ordering::Relaxed);
            }
        };
        if error == Error::Disconnected {
            return;
        }
        warn!(base = %shared.http_base, %error, "shell subscription failed");
        shared.state.send_if_modified(|state| {
            let changed = state.shell.synced;
            Arc::make_mut(state).shell.synced = false;
            changed
        });
        tokio::time::sleep(resubscribe_delay(failures)).await;
        failures += 1;
    }
}

fn spawn_thread(
    session: &Session,
    state: &watch::Sender<Arc<ThreadView>>,
) -> Option<JoinHandle<()>> {
    if state.borrow().sync == ThreadSync::Missing {
        return None;
    }
    Some(tokio::spawn(run_thread(session.clone(), state.clone())))
}

/// Keeps one thread current; resubscribes on a failed stream; stops once the
/// thread is missing.
async fn run_thread(session: Session, state: watch::Sender<Arc<ThreadView>>) {
    let mut failures = 0;
    loop {
        let params = {
            let view = state.borrow();
            WatchThreadParams {
                thread_id: view.thread_id.clone(),
                after: view.thread.is_some().then_some(view.sequence),
            }
        };
        let mut cursor = Cursor::resume(params.after);
        let Ok(mut stream) = session.stream(methods::THREADS_WATCH, encode(&params)) else {
            return;
        };
        let error = loop {
            let items = match stream.next().await {
                Some(Ok(values)) => {
                    match decode_items::<ThreadItem>(values, THREAD_ITEMS, "thread item") {
                        Ok(items) => items,
                        Err(error) => break error,
                    }
                }
                Some(Err(error)) => break error,
                None => break Error::Decode("threads.watch ended".into()),
            };
            failures = 0;
            state.send_if_modified(|view| {
                let view = Arc::make_mut(view);
                let mut changed = false;
                for item in items {
                    changed |= view.apply(&mut cursor, item);
                }
                changed
            });
            if state.borrow().sync == ThreadSync::Missing {
                return;
            }
        };
        match &error {
            Error::Disconnected => return,
            Error::Server(server) if server.code == ErrorCode::NotFound => {
                state.send_modify(|view| Arc::make_mut(view).sync = ThreadSync::Missing);
                return;
            }
            _ => {}
        }
        warn!(%error, thread = %params.thread_id, "thread subscription failed");
        state.send_modify(|view| {
            Arc::make_mut(view).sync = ThreadSync::Error {
                message: error.to_string(),
            };
        });
        tokio::time::sleep(resubscribe_delay(failures)).await;
        failures += 1;
    }
}
