//! The v2 envelope over one WebSocket (`docs/protocol/wire.md` §3): `call`
//! frames with per-connection ids, `ack {id, n: 1}` for every `items` frame
//! the consumer pulls, `cancel` for abandoned calls, a `ping` every
//! [`PING_INTERVAL_SECS`], and [`CLIENT_TIMEOUT_SECS`] of silence counts as
//! a dead socket. `err` with `id: null` or a closed socket fails every open
//! call.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use j3_protocol::v2::envelope::{
    CLIENT_TIMEOUT_SECS, CallId, ClientFrame, PING_INTERVAL_SECS, ServerFrame,
};
use serde_json::Value;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::{Instant, MissedTickBehavior},
};
use tokio_tungstenite::tungstenite::{self, Message};
use tracing::{debug, warn};

use crate::Error;

const PING_INTERVAL: Duration = Duration::from_secs(PING_INTERVAL_SECS);
const SILENCE_LIMIT: Duration = Duration::from_secs(CLIENT_TIMEOUT_SECS);
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);

/// Why a connection attempt failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The server speaks another protocol version; retrying soon won't help.
    Incompatible(String),
    /// Anything else; worth retrying.
    Failed(String),
}

enum Cmd {
    Call {
        id: CallId,
        method: &'static str,
        params: Value,
        reply: Reply,
    },
    Ack(CallId),
    Cancel(CallId),
    Close,
}

enum Reply {
    Unary(oneshot::Sender<Result<Value, Error>>),
    Stream(mpsc::UnboundedSender<StreamEvent>),
}

enum StreamEvent {
    Items(Vec<Value>),
    End(Result<(), Error>),
}

/// A connected socket. Cheap to clone; the socket closes on
/// [`Session::close`], when the server goes away, or when it stays silent.
#[derive(Clone)]
pub(crate) struct Session {
    tx: mpsc::UnboundedSender<Cmd>,
    ids: Arc<AtomicU64>,
}

impl Session {
    /// Opens the WebSocket at `url` and starts its I/O task, which resolves
    /// with the reason the socket ended.
    pub(crate) async fn connect(url: &str) -> Result<(Self, JoinHandle<String>), Failure> {
        let (socket, _) = tokio::time::timeout(OPEN_TIMEOUT, tokio_tungstenite::connect_async(url))
            .await
            .map_err(|_| Failure::Failed(format!("opening {url} timed out")))?
            .map_err(|error| open_failure(url, error))?;
        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run_socket(socket, rx));
        Ok((
            Self {
                tx,
                ids: Arc::new(AtomicU64::new(0)),
            },
            task,
        ))
    }

    fn next_id(&self) -> CallId {
        self.ids.fetch_add(1, Ordering::Relaxed)
    }

    /// One unary call. Dropping the returned future cancels the call.
    pub(crate) async fn call(&self, method: &'static str, params: Value) -> Result<Value, Error> {
        let id = self.next_id();
        let (reply, response) = oneshot::channel();
        self.tx
            .send(Cmd::Call {
                id,
                method,
                params,
                reply: Reply::Unary(reply),
            })
            .map_err(|_| Error::Disconnected)?;
        let mut guard = CancelGuard {
            id,
            tx: &self.tx,
            armed: true,
        };
        let result = response.await.unwrap_or(Err(Error::Disconnected));
        guard.armed = false;
        result
    }

    /// Starts a stream call. Dropping the stream before it ends cancels it.
    pub(crate) fn stream(&self, method: &'static str, params: Value) -> Result<RpcStream, Error> {
        let id = self.next_id();
        let (events, rx) = mpsc::unbounded_channel();
        self.tx
            .send(Cmd::Call {
                id,
                method,
                params,
                reply: Reply::Stream(events),
            })
            .map_err(|_| Error::Disconnected)?;
        Ok(RpcStream {
            id,
            rx,
            tx: self.tx.clone(),
            done: false,
        })
    }

    /// Closes the socket; open calls fail with [`Error::Disconnected`].
    pub(crate) fn close(&self) {
        let _ = self.tx.send(Cmd::Close);
    }
}

/// A refused upgrade with `426` is a protocol mismatch (wire.md §1).
fn open_failure(url: &str, error: tungstenite::Error) -> Failure {
    if let tungstenite::Error::Http(response) = &error
        && response.status().as_u16() == 426
    {
        let message = response
            .body()
            .as_deref()
            .and_then(|body| serde_json::from_slice::<Value>(body).ok())
            .and_then(|body| body.get("message")?.as_str().map(str::to_owned))
            .unwrap_or_else(|| format!("{url} refused this client's protocol version"));
        return Failure::Incompatible(message);
    }
    Failure::Failed(format!("opening {url}: {error}"))
}

struct CancelGuard<'a> {
    id: CallId,
    tx: &'a mpsc::UnboundedSender<Cmd>,
    armed: bool,
}

impl Drop for CancelGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.tx.send(Cmd::Cancel(self.id));
        }
    }
}

/// The `items` frames of one stream call.
pub(crate) struct RpcStream {
    id: CallId,
    rx: mpsc::UnboundedReceiver<StreamEvent>,
    tx: mpsc::UnboundedSender<Cmd>,
    done: bool,
}

impl RpcStream {
    /// The next `items` frame, acked as it is pulled; `None` once the stream
    /// ended with `end`. A failure is the stream's last element.
    pub(crate) async fn next(&mut self) -> Option<Result<Vec<Value>, Error>> {
        if self.done {
            return None;
        }
        match self.rx.recv().await {
            Some(StreamEvent::Items(items)) => {
                let _ = self.tx.send(Cmd::Ack(self.id));
                Some(Ok(items))
            }
            Some(StreamEvent::End(result)) => {
                self.done = true;
                result.err().map(Err)
            }
            None => {
                self.done = true;
                Some(Err(Error::Disconnected))
            }
        }
    }
}

impl Drop for RpcStream {
    fn drop(&mut self) {
        if !self.done {
            let _ = self.tx.send(Cmd::Cancel(self.id));
        }
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Default)]
struct Open {
    unary: HashMap<CallId, oneshot::Sender<Result<Value, Error>>>,
    streams: HashMap<CallId, mpsc::UnboundedSender<StreamEvent>>,
}

impl Open {
    fn fail_all(&mut self, error: &Error) {
        for (_, reply) in self.unary.drain() {
            let _ = reply.send(Err(error.clone()));
        }
        for (_, events) in self.streams.drain() {
            let _ = events.send(StreamEvent::End(Err(error.clone())));
        }
    }

    /// Routes one frame for call `id`; returns a `cancel` to send when the
    /// stream's consumer went away.
    fn route(&mut self, frame: ServerFrame) -> Option<ClientFrame> {
        let unexpected = |what: &str| Error::Decode(format!("unexpected `{what}` frame"));
        match frame {
            ServerFrame::Ok { id, result } => {
                if let Some(reply) = self.unary.remove(&id) {
                    let _ = reply.send(Ok(result));
                } else if let Some(events) = self.streams.remove(&id) {
                    let _ = events.send(StreamEvent::End(Err(unexpected("ok"))));
                }
            }
            ServerFrame::Items { id, items } => {
                if let Some(events) = self.streams.get(&id) {
                    if events.send(StreamEvent::Items(items)).is_err() {
                        self.streams.remove(&id);
                        return Some(ClientFrame::Cancel { id });
                    }
                } else if let Some(reply) = self.unary.remove(&id) {
                    let _ = reply.send(Err(unexpected("items")));
                    return Some(ClientFrame::Cancel { id });
                }
            }
            ServerFrame::End { id } => {
                if let Some(events) = self.streams.remove(&id) {
                    let _ = events.send(StreamEvent::End(Ok(())));
                } else if let Some(reply) = self.unary.remove(&id) {
                    let _ = reply.send(Err(unexpected("end")));
                }
            }
            ServerFrame::Err {
                id: Some(id),
                error,
            } => {
                if let Some(reply) = self.unary.remove(&id) {
                    let _ = reply.send(Err(Error::Server(error)));
                } else if let Some(events) = self.streams.remove(&id) {
                    let _ = events.send(StreamEvent::End(Err(Error::Server(error))));
                }
            }
            // Handled by the socket loop.
            ServerFrame::Err { id: None, .. } | ServerFrame::Pong => {}
        }
        None
    }
}

fn encode(frame: &ClientFrame) -> Message {
    Message::text(serde_json::to_string(frame).expect("client frames always encode"))
}

/// The socket's I/O loop. Resolves with the reason it ended.
async fn run_socket(socket: Socket, mut commands: mpsc::UnboundedReceiver<Cmd>) -> String {
    let (mut sink, mut frames) = socket.split();
    let mut open = Open::default();
    let mut ping = tokio::time::interval_at(Instant::now() + PING_INTERVAL, PING_INTERVAL);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let silence = tokio::time::sleep(SILENCE_LIMIT);
    tokio::pin!(silence);

    let (reason, error) = loop {
        let outgoing = tokio::select! {
            command = commands.recv() => match command {
                None | Some(Cmd::Close) => break ("closed by the client".to_owned(), Error::Disconnected),
                Some(Cmd::Call { id, method, params, reply }) => {
                    match reply {
                        Reply::Unary(reply) => { open.unary.insert(id, reply); }
                        Reply::Stream(events) => { open.streams.insert(id, events); }
                    }
                    ClientFrame::Call { id, method: method.to_owned(), params }
                }
                Some(Cmd::Ack(id)) => {
                    if !open.streams.contains_key(&id) {
                        continue;
                    }
                    ClientFrame::Ack { id, n: 1 }
                }
                Some(Cmd::Cancel(id)) => {
                    let known = open.unary.remove(&id).is_some() | open.streams.remove(&id).is_some();
                    if !known {
                        continue;
                    }
                    ClientFrame::Cancel { id }
                }
            },
            frame = frames.next() => {
                silence.as_mut().reset(Instant::now() + SILENCE_LIMIT);
                let text = match frame {
                    None => break ("the server closed the connection".to_owned(), Error::Disconnected),
                    Some(Err(e)) => break (format!("receiving: {e}"), Error::Disconnected),
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Close(frame))) => {
                        let reason = match frame {
                            Some(frame) if !frame.reason.is_empty() => {
                                format!("the server closed the connection ({})", frame.reason)
                            }
                            _ => "the server closed the connection".to_owned(),
                        };
                        break (reason, Error::Disconnected);
                    }
                    Some(Ok(_)) => continue,
                };
                let frame = match serde_json::from_str::<ServerFrame>(text.as_str()) {
                    Ok(frame) => frame,
                    Err(e) => {
                        warn!(error = %e, "undecodable frame from the server");
                        continue;
                    }
                };
                if let ServerFrame::Err { id: None, error } = frame {
                    break (format!("the server ended the connection: {error}"), Error::Server(error));
                }
                match open.route(frame) {
                    Some(cancel) => cancel,
                    None => continue,
                }
            }
            _ = ping.tick() => ClientFrame::Ping,
            () = &mut silence => break (
                format!("no response from the server for {CLIENT_TIMEOUT_SECS}s"),
                Error::Disconnected,
            ),
        };
        if let Err(e) = sink.send(encode(&outgoing)).await {
            break (format!("sending: {e}"), Error::Disconnected);
        }
    };
    debug!(%reason, "socket ended");
    open.fail_all(&error);
    let _ = sink.close().await;
    reason
}
