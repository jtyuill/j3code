//! Effect RPC over one WebSocket (`docs/protocol/effect-rpc.md`): JSON text
//! frames, numeric request ids from one process-wide counter, an `Ack` for
//! every `Chunk` the consumer pulls, `Interrupt` for abandoned requests, a
//! `Ping` every 5 s, and a socket that stays silent for 15 s counts as dead.

use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use j3_protocol::effect_rpc::{
    CauseReason, ClientMessage, Exit, Request, RequestId, ServerMessage, decode_server_frame,
};
use serde_json::Value;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::{Instant, MissedTickBehavior},
};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, warn};

use crate::Error;

const PING_INTERVAL: Duration = Duration::from_secs(5);
const PING_TIMEOUT: Duration = Duration::from_secs(15);
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);

/// Effect's client allocates ids from one module-global counter starting at 0.
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(0);

fn next_id() -> u64 {
    NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
}

enum Cmd {
    Unary {
        id: u64,
        tag: &'static str,
        payload: Value,
        reply: oneshot::Sender<Result<Value, Error>>,
    },
    Stream {
        id: u64,
        tag: &'static str,
        payload: Value,
        events: mpsc::UnboundedSender<StreamEvent>,
    },
    Ack(u64),
    Interrupt(u64),
    Close,
}

enum StreamEvent {
    Chunk(Vec<Value>),
    End(Result<(), Error>),
}

/// A connected socket. Cheap to clone; the socket closes when [`Session::close`]
/// is called or the server goes away.
#[derive(Clone)]
pub(crate) struct Session {
    tx: mpsc::UnboundedSender<Cmd>,
}

impl Session {
    /// Opens the WebSocket at `url` and starts its I/O task, which resolves
    /// with the reason the socket ended.
    pub(crate) async fn connect(url: &str) -> Result<(Self, JoinHandle<String>), String> {
        let (socket, _) = tokio::time::timeout(OPEN_TIMEOUT, tokio_tungstenite::connect_async(url))
            .await
            .map_err(|_| format!("opening {url} timed out"))?
            .map_err(|e| format!("opening {url}: {e}"))?;
        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run_socket(socket, rx));
        Ok((Self { tx }, task))
    }

    /// One unary call. Dropping the returned future interrupts the request.
    pub(crate) async fn request(&self, tag: &'static str, payload: Value) -> Result<Value, Error> {
        let id = next_id();
        let (reply, response) = oneshot::channel();
        self.tx
            .send(Cmd::Unary {
                id,
                tag,
                payload,
                reply,
            })
            .map_err(|_| closed())?;
        let mut guard = InterruptGuard {
            id,
            tx: &self.tx,
            armed: true,
        };
        let result = response.await.unwrap_or_else(|_| Err(closed()));
        guard.armed = false;
        result
    }

    /// Starts a streaming call. Dropping the stream before it ends interrupts it.
    pub(crate) fn stream(&self, tag: &'static str, payload: Value) -> Result<RpcStream, Error> {
        let id = next_id();
        let (events, rx) = mpsc::unbounded_channel();
        self.tx
            .send(Cmd::Stream {
                id,
                tag,
                payload,
                events,
            })
            .map_err(|_| closed())?;
        Ok(RpcStream {
            id,
            rx,
            tx: self.tx.clone(),
            done: false,
        })
    }

    /// Closes the socket; in-flight requests fail with [`Error::Disconnected`].
    pub(crate) fn close(&self) {
        let _ = self.tx.send(Cmd::Close);
    }
}

fn closed() -> Error {
    Error::Disconnected("the connection to the server was lost".into())
}

struct InterruptGuard<'a> {
    id: u64,
    tx: &'a mpsc::UnboundedSender<Cmd>,
    armed: bool,
}

impl Drop for InterruptGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.tx.send(Cmd::Interrupt(self.id));
        }
    }
}

/// The chunks of one streaming call.
pub(crate) struct RpcStream {
    id: u64,
    rx: mpsc::UnboundedReceiver<StreamEvent>,
    tx: mpsc::UnboundedSender<Cmd>,
    done: bool,
}

impl RpcStream {
    /// The next chunk (acked as it is pulled); `None` once the stream ended
    /// normally. A failure ends the stream.
    pub(crate) async fn next(&mut self) -> Option<Result<Vec<Value>, Error>> {
        if self.done {
            return None;
        }
        match self.rx.recv().await {
            Some(StreamEvent::Chunk(values)) => {
                let _ = self.tx.send(Cmd::Ack(self.id));
                Some(Ok(values))
            }
            Some(StreamEvent::End(result)) => {
                self.done = true;
                result.err().map(Err)
            }
            None => {
                self.done = true;
                Some(Err(closed()))
            }
        }
    }
}

impl Drop for RpcStream {
    fn drop(&mut self) {
        if !self.done {
            let _ = self.tx.send(Cmd::Interrupt(self.id));
        }
    }
}

/// Maps an `Exit` to the call's result.
fn exit_result(exit: Exit) -> Result<Value, Error> {
    match exit {
        Exit::Success { value } => Ok(value),
        Exit::Failure { cause } => Err(cause_error(cause)),
    }
}

fn cause_error(cause: Vec<CauseReason>) -> Error {
    // A typed failure explains the most; then a defect; else an interrupt.
    let mut fallback = None;
    for reason in cause {
        match reason {
            CauseReason::Fail { error } => return Error::rejected(error),
            CauseReason::Die { defect } => fallback = Some(Error::defect(&defect)),
            CauseReason::Interrupt { .. } => {
                fallback.get_or_insert(Error::Interrupted);
            }
        }
    }
    fallback.unwrap_or(Error::Interrupted)
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Default)]
struct Pending {
    unary: HashMap<u64, oneshot::Sender<Result<Value, Error>>>,
    streams: HashMap<u64, mpsc::UnboundedSender<StreamEvent>>,
}

impl Pending {
    fn fail_all(&mut self, error: &Error) {
        for (_, reply) in self.unary.drain() {
            let _ = reply.send(Err(error.clone()));
        }
        for (_, events) in self.streams.drain() {
            let _ = events.send(StreamEvent::End(Err(error.clone())));
        }
    }
}

fn encode(message: &ClientMessage) -> Message {
    Message::text(serde_json::to_string(message).expect("client messages always encode"))
}

fn request_message(id: u64, tag: &str, payload: Value) -> ClientMessage {
    ClientMessage::Request(Request {
        id: RequestId::Number(id),
        tag: tag.to_owned(),
        payload,
        headers: Vec::new(),
        is_notification: None,
        trace_id: None,
        span_id: None,
        sampled: None,
    })
}

/// The socket's I/O loop. Resolves with the reason it ended.
async fn run_socket(socket: Socket, mut commands: mpsc::UnboundedReceiver<Cmd>) -> String {
    let (mut sink, mut frames) = socket.split();
    let mut pending = Pending::default();
    let mut ping = tokio::time::interval_at(Instant::now() + PING_INTERVAL, PING_INTERVAL);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let silence = tokio::time::sleep(PING_TIMEOUT);
    tokio::pin!(silence);

    let reason = loop {
        tokio::select! {
            command = commands.recv() => {
                let outgoing = match command {
                    None | Some(Cmd::Close) => break "closed by the client".to_owned(),
                    Some(Cmd::Unary { id, tag, payload, reply }) => {
                        pending.unary.insert(id, reply);
                        request_message(id, tag, payload)
                    }
                    Some(Cmd::Stream { id, tag, payload, events }) => {
                        pending.streams.insert(id, events);
                        request_message(id, tag, payload)
                    }
                    Some(Cmd::Ack(id)) => {
                        if !pending.streams.contains_key(&id) {
                            continue;
                        }
                        ClientMessage::ack(id)
                    }
                    Some(Cmd::Interrupt(id)) => {
                        let known = pending.unary.remove(&id).is_some()
                            | pending.streams.remove(&id).is_some();
                        if !known {
                            continue;
                        }
                        ClientMessage::interrupt(id)
                    }
                };
                if let Err(e) = sink.send(encode(&outgoing)).await {
                    break format!("sending: {e}");
                }
            }
            frame = frames.next() => {
                silence.as_mut().reset(Instant::now() + PING_TIMEOUT);
                let text = match frame {
                    None => break "the server closed the connection".to_owned(),
                    Some(Err(e)) => break format!("receiving: {e}"),
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Close(frame))) => {
                        break match frame {
                            Some(frame) => format!("the server closed the connection ({})", frame.reason),
                            None => "the server closed the connection".to_owned(),
                        };
                    }
                    Some(Ok(_)) => continue,
                };
                let messages = match decode_server_frame(text.as_str()) {
                    Ok(messages) => messages,
                    Err(e) => {
                        warn!(error = %e, "undecodable frame from the server");
                        continue;
                    }
                };
                for message in messages {
                    if let Some(interrupt) = on_message(&mut pending, message)
                        && let Err(e) = sink.send(encode(&interrupt)).await
                    {
                        pending.fail_all(&Error::Disconnected(format!("sending: {e}")));
                        return format!("sending: {e}");
                    }
                }
            }
            _ = ping.tick() => {
                if let Err(e) = sink.send(encode(&ClientMessage::Ping)).await {
                    break format!("sending: {e}");
                }
            }
            () = &mut silence => break format!("no response from the server for {}s", PING_TIMEOUT.as_secs()),
        }
    };
    debug!(%reason, "socket ended");
    let _ = sink.close().await;
    pending.fail_all(&Error::Disconnected(reason.clone()));
    reason
}

/// Routes one server message; returns an `Interrupt` to send for a stream
/// whose consumer went away.
fn on_message(pending: &mut Pending, message: ServerMessage) -> Option<ClientMessage> {
    match message {
        ServerMessage::Chunk { request_id, values } => {
            let RequestId::Number(id) = request_id else {
                return None;
            };
            let events = pending.streams.get(&id)?;
            if events.send(StreamEvent::Chunk(values)).is_err() {
                pending.streams.remove(&id);
                return Some(ClientMessage::interrupt(id));
            }
            None
        }
        ServerMessage::Exit { request_id, exit } => {
            let RequestId::Number(id) = request_id else {
                return None;
            };
            if let Some(reply) = pending.unary.remove(&id) {
                let _ = reply.send(exit_result(exit));
            } else if let Some(events) = pending.streams.remove(&id) {
                let _ = events.send(StreamEvent::End(exit_result(exit).map(drop)));
            }
            None
        }
        ServerMessage::Defect { defect } => {
            pending.fail_all(&Error::defect(&defect));
            None
        }
        ServerMessage::Pong
        | ServerMessage::ClientProtocolError { .. }
        | ServerMessage::Request(_)
        | ServerMessage::Unknown(_) => None,
    }
}
