//! The connection supervisor: one task per [`crate::Connection`] that
//! connects, keeps the server config, shell and subscribed threads current,
//! and reconnects with backoff, forever.
//!
//! Per attempt (ios-surface §1.1): `GET /.well-known/t3/environment` (protocol
//! 2 required), WebSocket `/ws?…&orchestrationProtocol=2`, the
//! `subscribeServerConfig` handshake (first item is the config snapshot), then
//! `orchestration.subscribeShell` and one `orchestration.subscribeThread` per
//! subscribed thread. Resubscriptions pass `afterSequence` when state is
//! already loaded; items at or below the applied sequence are dropped and
//! snapshots replace state wholesale.

use std::{collections::HashMap, sync::Arc, time::Duration};

use j3_protocol::t3::{
    environment::{ExecutionEnvironmentDescriptor, ORCHESTRATION_PROTOCOL_VERSION},
    methods::{SubscribeShellInput, SubscribeThreadInput},
    orchestration::{ShellStreamItem, ThreadStreamItem},
    server::{ServerConfig, ServerConfigStreamEvent, SubscribeServerConfigInput},
};
use reqwest::Url;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
    time::Instant,
};
use tracing::{debug, info, warn};

use crate::{
    ConnectionState, ConnectionStatus, Error, ShellState, ThreadState, ThreadSync,
    rpc::{RpcStream, Session},
    shell::apply_config,
};

/// Reconnect backoff: 1 s doubling to 30 s.
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// A connection that lasted this long resets the backoff.
const STABLE_AFTER: Duration = Duration::from_secs(30);
const DESCRIPTOR_TIMEOUT: Duration = Duration::from_secs(10);
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
}

impl Shared {
    pub(crate) fn update(&self, change: impl FnOnce(&mut ConnectionState)) {
        self.state.send_modify(|state| change(Arc::make_mut(state)));
    }

    pub(crate) fn url(&self, path: &str) -> Result<Url, Error> {
        let base = self.base.as_ref().map_err(|e| Error::Http(e.clone()))?;
        base.join(path)
            .map_err(|e| Error::Http(format!("invalid URL path {path:?}: {e}")))
    }

    /// The live session, waiting up to [`CALL_WAIT`] for one.
    pub(crate) async fn ready_session(&self) -> Result<Session, Error> {
        let mut slots = self.session.subscribe();
        let slot = match tokio::time::timeout(
            CALL_WAIT,
            slots.wait_for(|slot| !matches!(slot, SessionSlot::Down)),
        )
        .await
        {
            Ok(Ok(slot)) => slot.clone(),
            Ok(Err(_)) => SessionSlot::Closed,
            Err(_) => {
                let reason = match &self.state.borrow().status {
                    ConnectionStatus::Reconnecting { error, .. } => error.clone(),
                    _ => format!("could not reach {}", self.http_base),
                };
                return Err(Error::Disconnected(reason));
            }
        };
        match slot {
            SessionSlot::Up(session) => Ok(session),
            SessionSlot::Closed => Err(Error::Closed),
            SessionSlot::Down => unreachable!("waited for a non-Down slot"),
        }
    }
}

pub(crate) enum Command {
    Subscribe {
        thread_id: String,
        generation: u64,
        state: watch::Sender<Arc<ThreadState>>,
    },
    Unsubscribe {
        thread_id: String,
        generation: u64,
    },
    Close(oneshot::Sender<()>),
}

struct Slot {
    generation: u64,
    state: watch::Sender<Arc<ThreadState>>,
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

        let error = match result {
            Ok(connected) => {
                let connected_at = Instant::now();
                let mut socket = supervisor.go_live(connected);
                let reason = loop {
                    tokio::select! {
                        reason = &mut socket => break reason.unwrap_or_else(|e| format!("socket task failed: {e}")),
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
                reason
            }
            Err(error) => {
                debug!(base = %supervisor.shared.http_base, %error, "connecting failed");
                error
            }
        };

        failures = failures.saturating_add(1);
        supervisor.shared.update(|state| {
            state.status = ConnectionStatus::Reconnecting {
                attempt: failures,
                error,
            }
        });
        let delay = BACKOFF_MIN
            .saturating_mul(1 << (failures - 1).min(5))
            .min(BACKOFF_MAX);
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
    fn command(&mut self, command: Option<Command>) -> Flow {
        match command {
            None => Flow::Stop(None),
            Some(Command::Close(ack)) => Flow::Stop(Some(ack)),
            Some(Command::Subscribe {
                thread_id,
                generation,
                state,
            }) => {
                if let Some(previous) = self.slots.remove(&thread_id) {
                    if let Some(task) = previous.task {
                        task.abort();
                    }
                    // The handle was dropped and reopened before the
                    // unsubscribe arrived: carry the state over.
                    if state.borrow().projection.is_none() {
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
            Some(Command::Unsubscribe {
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
            descriptor,
            config,
            config_stream,
        } = connected;
        let changed_environment = self
            .shared
            .state
            .borrow()
            .descriptor
            .as_ref()
            .is_some_and(|known| known.environment_id != descriptor.environment_id);
        if changed_environment {
            // A different server answers at this address: nothing carried over
            // applies to it.
            warn!(base = %self.shared.http_base, "the server's environment id changed");
            for slot in self.slots.values() {
                let thread_id = slot.state.borrow().thread_id.clone();
                slot.state
                    .send_replace(Arc::new(ThreadState::new(thread_id)));
            }
        }
        self.shared.update(|state| {
            if changed_environment {
                state.shell = ShellState::default();
                state.workspaces.clear();
            }
            state.descriptor = Some(descriptor);
            state.config = Some(config);
            state.status = ConnectionStatus::Synchronizing;
            state.shell.synchronized = false;
        });
        self.shared
            .session
            .send_replace(SessionSlot::Up(session.clone()));
        let tasks = vec![
            tokio::spawn(run_config(
                session.clone(),
                config_stream,
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

    fn go_down(&mut self) {
        if let Some(live) = self.live.take() {
            live.session.close();
            for task in live.tasks {
                task.abort();
            }
        }
        self.shared.session.send_replace(SessionSlot::Down);
        for slot in self.slots.values_mut() {
            if let Some(task) = slot.task.take() {
                task.abort();
            }
            slot.state.send_if_modified(|state| {
                if state.sync == ThreadSync::Live {
                    Arc::make_mut(state).sync = ThreadSync::Synchronizing;
                    true
                } else {
                    false
                }
            });
        }
        self.shared.update(|state| state.shell.synchronized = false);
    }

    fn stop(mut self, ack: Option<oneshot::Sender<()>>) {
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
        self.shared.session.send_replace(SessionSlot::Closed);
        self.shared
            .update(|state| state.status = ConnectionStatus::Closed);
        if let Some(ack) = ack {
            let _ = ack.send(());
        }
    }
}

struct Connected {
    session: Session,
    socket: JoinHandle<String>,
    descriptor: ExecutionEnvironmentDescriptor,
    config: ServerConfig,
    config_stream: RpcStream,
}

/// One connection attempt, through the config handshake.
async fn connect(shared: Arc<Shared>) -> Result<Connected, String> {
    let descriptor = fetch_descriptor(&shared).await?;
    let version = descriptor.orchestration_protocol_version.unwrap_or(1);
    if version != ORCHESTRATION_PROTOCOL_VERSION {
        return Err(format!(
            "{} speaks orchestration protocol {version}; this client needs {ORCHESTRATION_PROTOCOL_VERSION}",
            shared.http_base
        ));
    }
    let socket_url = socket_url(shared.base.as_ref().map_err(Clone::clone)?);
    let (session, socket) = Session::connect(socket_url.as_str()).await?;
    let handshake = async {
        let input = SubscribeServerConfigInput {
            workspace_snapshots_on_demand: Some(true),
            ..Default::default()
        };
        let mut stream = session
            .stream("subscribeServerConfig", encode(&input))
            .map_err(|e| e.to_string())?;
        let first = loop {
            match stream.next().await {
                Some(Ok(values)) => {
                    if let Some(first) = values.into_iter().next() {
                        break first;
                    }
                }
                Some(Err(e)) => return Err(format!("server config: {e}")),
                None => return Err("the server config stream ended".to_owned()),
            }
        };
        let event: ServerConfigStreamEvent = decode(first, "server config")?;
        let ServerConfigStreamEvent::Snapshot(snapshot) = event else {
            return Err("the server config stream did not start with a snapshot".to_owned());
        };
        if snapshot.config.environment.environment_id != descriptor.environment_id {
            return Err(format!(
                "{} reports two environment ids ({} over HTTP, {} over the socket)",
                shared.http_base,
                descriptor.environment_id,
                snapshot.config.environment.environment_id
            ));
        }
        Ok((snapshot.config, stream))
    };
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake).await {
        Ok(Ok((config, config_stream))) => Ok(Connected {
            session,
            socket,
            descriptor,
            config,
            config_stream,
        }),
        Ok(Err(error)) => {
            session.close();
            Err(error)
        }
        Err(_) => {
            session.close();
            Err("the server did not send its config in time".to_owned())
        }
    }
}

async fn fetch_descriptor(shared: &Shared) -> Result<ExecutionEnvironmentDescriptor, String> {
    let url = shared
        .url("/.well-known/t3/environment")
        .map_err(|e| e.to_string())?;
    let response = shared
        .http
        .get(url.clone())
        .timeout(DESCRIPTOR_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("could not reach {}: {e}", shared.http_base))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("{url} answered {status}"));
    }
    response
        .json()
        .await
        .map_err(|e| format!("{url} sent an unexpected descriptor: {e}"))
}

/// `ws[s]://host[:port]/ws?clientSurface=desktop&orchestrationProtocol=2`.
fn socket_url(base: &Url) -> Url {
    let mut url = base.clone();
    let scheme = if base.scheme() == "https" {
        "wss"
    } else {
        "ws"
    };
    url.set_scheme(scheme)
        .expect("http(s) URLs can switch to ws(s)");
    url.set_path("/ws");
    url.set_query(Some(&format!(
        "clientSurface=desktop&orchestrationProtocol={ORCHESTRATION_PROTOCOL_VERSION}"
    )));
    url.set_fragment(None);
    url
}

pub(crate) fn encode<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("wire types always encode")
}

fn decode<T: DeserializeOwned>(value: Value, what: &str) -> Result<T, String> {
    serde_json::from_value(value).map_err(|e| format!("{what}: {e}"))
}

fn resubscribe_delay(failures: u32) -> Duration {
    RESUBSCRIBE_MIN
        .saturating_mul(1 << failures.min(7))
        .min(RESUBSCRIBE_MAX)
}

/// Keeps `config` current after the handshake. The stream lives as long as
/// the socket; if it ends, the session is dropped and reconnected.
async fn run_config(session: Session, mut stream: RpcStream, shared: Arc<Shared>) {
    let reason = loop {
        match stream.next().await {
            Some(Ok(values)) => {
                let events: Result<Vec<ServerConfigStreamEvent>, _> = values
                    .into_iter()
                    .map(|value| decode(value, "server config"))
                    .collect();
                match events {
                    Ok(events) => shared.update(|state| {
                        for event in events {
                            apply_config(&mut state.config, event);
                        }
                    }),
                    Err(error) => break error,
                }
            }
            Some(Err(e)) => break format!("server config: {e}"),
            None => break "the server config stream ended".to_owned(),
        }
    };
    warn!(base = %shared.http_base, %reason, "dropping the session");
    session.close();
}

/// Keeps the shell current; resubscribes on a failed stream.
async fn run_shell(session: Session, shared: Arc<Shared>) {
    let mut failures = 0;
    loop {
        let input = {
            let state = shared.state.borrow();
            SubscribeShellInput {
                after_sequence: state.shell.loaded.then_some(state.shell.snapshot_sequence),
                request_completion_marker: Some(true),
                ..Default::default()
            }
        };
        shared.update(|state| {
            state.shell.synchronized = false;
            state.status = ConnectionStatus::Synchronizing;
        });
        let Ok(mut stream) = session.stream("orchestration.subscribeShell", encode(&input)) else {
            return;
        };
        let error = loop {
            match stream.next().await {
                Some(Ok(values)) => {
                    let items: Result<Vec<ShellStreamItem>, _> = values
                        .into_iter()
                        .map(|value| decode(value, "shell item"))
                        .collect();
                    let items = match items {
                        Ok(items) => items,
                        Err(error) => break Error::Decode(error),
                    };
                    failures = 0;
                    shared.state.send_if_modified(|state| {
                        let state = Arc::make_mut(state);
                        let mut changed = false;
                        for item in items {
                            changed |= state.shell.apply(item);
                        }
                        if state.shell.synchronized && state.status != ConnectionStatus::Live {
                            state.status = ConnectionStatus::Live;
                            changed = true;
                        }
                        changed
                    });
                }
                Some(Err(error)) => break error,
                None => break Error::Decode("the shell stream ended".into()),
            }
        };
        if matches!(error, Error::Disconnected(_)) {
            return;
        }
        warn!(base = %shared.http_base, %error, "shell subscription failed");
        tokio::time::sleep(resubscribe_delay(failures)).await;
        failures += 1;
    }
}

fn spawn_thread(
    session: &Session,
    state: &watch::Sender<Arc<ThreadState>>,
) -> Option<JoinHandle<()>> {
    if state.borrow().sync == ThreadSync::Deleted {
        return None;
    }
    Some(tokio::spawn(run_thread(session.clone(), state.clone())))
}

/// Keeps one thread current; resubscribes on a failed stream; stops once the
/// thread is deleted.
async fn run_thread(session: Session, state: watch::Sender<Arc<ThreadState>>) {
    let mut failures = 0;
    loop {
        let input = {
            let current = state.borrow();
            SubscribeThreadInput {
                thread_id: current.thread_id.clone(),
                after_sequence: current.projection.is_some().then_some(current.sequence),
                request_completion_marker: Some(true),
                accept_bounded_snapshot: None,
                accept_deferred_work: None,
                accept_compact_turn_items: None,
                accept_deduped_bodies: None,
                accept_compact_ids: None,
            }
        };
        state.send_if_modified(|current| {
            if current.projection.is_some() && current.sync != ThreadSync::Synchronizing {
                Arc::make_mut(current).sync = ThreadSync::Synchronizing;
                true
            } else {
                false
            }
        });
        let Ok(mut stream) = session.stream("orchestration.subscribeThread", encode(&input)) else {
            return;
        };
        let mut awaiting = true;
        let error = loop {
            match stream.next().await {
                Some(Ok(values)) => {
                    let items: Result<Vec<ThreadStreamItem>, _> = values
                        .into_iter()
                        .map(|value| decode(value, "thread item"))
                        .collect();
                    let items = match items {
                        Ok(items) => items,
                        Err(error) => break Error::Decode(error),
                    };
                    failures = 0;
                    state.send_if_modified(|current| {
                        let current = Arc::make_mut(current);
                        let mut changed = false;
                        for item in items {
                            changed |= current.apply(item, &mut awaiting);
                        }
                        changed
                    });
                    if state.borrow().sync == ThreadSync::Deleted {
                        return;
                    }
                }
                Some(Err(error)) => break error,
                None => break Error::Decode("the thread stream ended".into()),
            }
        };
        if matches!(error, Error::Disconnected(_)) {
            return;
        }
        warn!(%error, thread = %state.borrow().thread_id, "thread subscription failed");
        state.send_modify(|current| {
            Arc::make_mut(current).sync = ThreadSync::Failed(error.to_string());
        });
        tokio::time::sleep(resubscribe_delay(failures)).await;
        failures += 1;
    }
}
