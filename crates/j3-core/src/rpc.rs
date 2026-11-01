//! Effect RPC server side of one WebSocket (`docs/protocol/effect-rpc.md`).
//!
//! One socket is one session. Every `Request` runs on its own task, so
//! requests on a socket proceed concurrently. A [`RpcHandler`] answers each
//! request with a [`Response`]: either one `Exit`, or a stream whose items are
//! sent as ack-gated `Chunk`s followed by an `Exit`.
//!
//! The session loop owns all per-request state. Request tasks hand their
//! messages to the loop, which forwards them only while the request is still
//! in flight; that keeps an interrupted request from sending anything after
//! its interrupt `Exit`.

use std::{
    any::Any,
    collections::HashMap,
    panic::AssertUnwindSafe,
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
};

use axum::extract::ws::{Message, WebSocket};
use futures_util::{
    FutureExt, SinkExt, StreamExt,
    future::BoxFuture,
    stream::{BoxStream, SplitSink},
};
use j3_protocol::effect_rpc::{ClientMessage, Exit, Request, RequestId, ServerMessage};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{sync::mpsc, task::AbortHandle};

/// A handler's answer to one request.
pub enum Response {
    /// Unary reply.
    Unary(Exit),
    /// Stream: each item is sent in a `Chunk` (ack-gated, batched while
    /// waiting). When the stream ends: `Exit` Success with value null.
    Stream(BoxStream<'static, StreamItem>),
}

/// One element of a [`Response::Stream`].
pub enum StreamItem {
    Value(Value),
    /// Ends the stream with Exit Failure [Fail{error}] (typed error JSON).
    Fail(Value),
}

impl Response {
    /// Exit Success.
    pub fn ok(value: Value) -> Self {
        Self::Unary(Exit::success(value))
    }

    /// Exit Failure [Fail{error}]. `error` is the typed error JSON, `_tag`
    /// included by the caller.
    pub fn fail(error: Value) -> Self {
        Self::Unary(Exit::fail(error))
    }

    /// Exit Failure [Die{defect}] with a string defect, the shape effect uses
    /// for unknown tags and undecodable payloads.
    pub fn die(message: impl Into<String>) -> Self {
        Self::Unary(Exit::die(Value::String(message.into())))
    }
}

/// Answers RPC requests.
pub trait RpcHandler: Send + Sync + 'static {
    /// One call per Request. Unknown tags: return `Response::die(...)` (never
    /// a connection Defect).
    fn call(&self, tag: String, payload: Value) -> BoxFuture<'static, Response>;
}

/// Most stream items buffered while a `Chunk` waits for its `Ack`. Past this
/// the stream is not polled until the client catches up.
const MAX_BUFFERED_ITEMS: usize = 1024;

/// Source of the `fiberId` reported in interrupt exits; one per session, like
/// effect's protocol fiber that performs the interrupt.
static NEXT_FIBER_ID: AtomicI64 = AtomicI64::new(1);

/// Runs one RPC session over `socket` until the client disconnects. Every
/// in-flight request is cancelled when the socket closes.
pub async fn serve_socket(socket: WebSocket, handler: Arc<dyn RpcHandler>) {
    let (sink, mut source) = socket.split();
    let (out_tx, out_rx) = mpsc::unbounded_channel();
    let writer = tokio::spawn(write_frames(sink, out_rx));
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let mut session = Session {
        handler,
        out: out_tx,
        events: events_tx,
        in_flight: HashMap::new(),
        next_slot: 0,
        fiber_id: NEXT_FIBER_ID.fetch_add(1, Ordering::Relaxed),
    };
    loop {
        tokio::select! {
            frame = source.next() => match frame {
                Some(Ok(Message::Text(text))) => session.on_frame(text.as_str()),
                Some(Ok(Message::Binary(bytes))) => match std::str::from_utf8(&bytes) {
                    Ok(text) => session.on_frame(text),
                    Err(error) => session.send(&syntax_error_defect(&error)),
                },
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
            },
            Some(event) = events_rx.recv() => session.on_event(event),
        }
    }
    // Dropping the session aborts every request task.
    drop(session);
    writer.abort();
}

async fn write_frames(
    mut sink: SplitSink<WebSocket, Message>,
    mut frames: mpsc::UnboundedReceiver<String>,
) {
    while let Some(frame) = frames.recv().await {
        if sink.send(Message::Text(frame.into())).await.is_err() {
            return;
        }
    }
}

/// A message from a request task to the session loop.
struct Event {
    request_id: RequestId,
    /// Identifies the task instance, so a late message from an interrupted
    /// request is not mistaken for one from a newer request reusing its id.
    slot: u64,
    message: ServerMessage,
    /// The request's terminal `Exit`.
    last: bool,
}

struct InFlight {
    slot: u64,
    task: AbortHandle,
    acks: mpsc::UnboundedSender<()>,
}

struct Session {
    handler: Arc<dyn RpcHandler>,
    out: mpsc::UnboundedSender<String>,
    events: mpsc::UnboundedSender<Event>,
    in_flight: HashMap<RequestId, InFlight>,
    next_slot: u64,
    fiber_id: i64,
}

impl Drop for Session {
    fn drop(&mut self) {
        for (_, request) in self.in_flight.drain() {
            request.task.abort();
        }
    }
}

impl Session {
    fn send(&self, message: &ServerMessage) {
        match serde_json::to_string(message) {
            Ok(frame) => {
                // A closed writer means the socket is gone; the read side
                // notices and ends the session.
                let _ = self.out.send(frame);
            }
            Err(error) => tracing::error!(%error, "failed to encode an RPC server message"),
        }
    }

    fn on_frame(&mut self, text: &str) {
        match serde_json::from_str::<Value>(text) {
            Ok(Value::Array(messages)) => {
                for message in messages {
                    self.on_message(message);
                }
            }
            Ok(message) => self.on_message(message),
            Err(error) => self.send(&syntax_error_defect(&error)),
        }
    }

    fn on_message(&mut self, raw: Value) {
        match ClientMessage::deserialize(&raw) {
            Ok(ClientMessage::Request(request)) => self.start(request),
            Ok(ClientMessage::Ack { request_id }) => {
                if let Some(request) = self.in_flight.get(&request_id) {
                    let _ = request.acks.send(());
                }
            }
            Ok(ClientMessage::Interrupt { request_id }) => self.interrupt(request_id),
            Ok(ClientMessage::Ping) => self.send(&ServerMessage::Pong),
            // Only the HTTP protocol sends Eof; a socket session ends on close.
            Ok(ClientMessage::Eof) => {}
            Ok(ClientMessage::Unknown(_)) => self.send(&ServerMessage::defect(Value::String(
                format!("Unknown request tag: {}", js_display(raw.get("_tag"))),
            ))),
            Err(error) => self.on_undecodable(&raw, &error),
        }
    }

    /// A message with a known `_tag` whose fields don't decode.
    fn on_undecodable(&mut self, raw: &Value, error: &serde_json::Error) {
        // Ack and Interrupt with an unusable id address no request.
        if raw.get("_tag").and_then(Value::as_str) != Some("Request") {
            return;
        }
        match raw.get("id").map(RequestId::deserialize) {
            Some(Ok(id)) => {
                if !self.in_flight.contains_key(&id) {
                    self.send(&ServerMessage::exit_die(
                        id,
                        Value::String(format!("Invalid request: {error}")),
                    ));
                }
            }
            _ => self.send(&ServerMessage::defect(Value::String(format!(
                "Invalid request id: {}",
                js_display(raw.get("id"))
            )))),
        }
    }

    fn start(&mut self, request: Request) {
        // Effect ignores a request whose id is already running.
        if self.in_flight.contains_key(&request.id) {
            return;
        }
        let slot = self.next_slot;
        self.next_slot += 1;
        let (acks_tx, acks_rx) = mpsc::unbounded_channel();
        let emitter = Emitter {
            events: self.events.clone(),
            request_id: request.id.clone(),
            slot,
        };
        let task = tokio::spawn(run_request(
            self.handler.clone(),
            request.tag,
            request.payload,
            emitter,
            acks_rx,
        ));
        self.in_flight.insert(
            request.id,
            InFlight {
                slot,
                task: task.abort_handle(),
                acks: acks_tx,
            },
        );
    }

    fn interrupt(&mut self, request_id: RequestId) {
        // An unknown id is a request that already finished; its Exit is sent,
        // and an Exit for an id the client no longer tracks would be
        // broadcast to every pending request on its side.
        if let Some(request) = self.in_flight.remove(&request_id) {
            request.task.abort();
            self.send(&ServerMessage::exit(
                request_id,
                Exit::interrupt(Some(self.fiber_id)),
            ));
        }
    }

    fn on_event(&mut self, event: Event) {
        let current = self
            .in_flight
            .get(&event.request_id)
            .is_some_and(|request| request.slot == event.slot);
        if !current {
            return;
        }
        if event.last {
            self.in_flight.remove(&event.request_id);
        }
        self.send(&event.message);
    }
}

/// Sends one request's messages to the session loop.
struct Emitter {
    events: mpsc::UnboundedSender<Event>,
    request_id: RequestId,
    slot: u64,
}

impl Emitter {
    fn emit(&self, message: ServerMessage, last: bool) {
        let _ = self.events.send(Event {
            request_id: self.request_id.clone(),
            slot: self.slot,
            message,
            last,
        });
    }

    fn chunk(&self, values: Vec<Value>) {
        self.emit(ServerMessage::chunk(self.request_id.clone(), values), false);
    }

    fn exit(&self, exit: Exit) {
        self.emit(ServerMessage::exit(self.request_id.clone(), exit), true);
    }
}

async fn run_request(
    handler: Arc<dyn RpcHandler>,
    tag: String,
    payload: Value,
    emitter: Emitter,
    acks: mpsc::UnboundedReceiver<()>,
) {
    let outcome = AssertUnwindSafe(async {
        match handler.call(tag, payload).await {
            Response::Unary(exit) => exit,
            Response::Stream(stream) => run_stream(stream, &emitter, acks).await,
        }
    })
    .catch_unwind()
    .await;
    // A panicking handler is a defect of this request only, like effect with
    // `disableFatalDefects`.
    let exit = outcome.unwrap_or_else(|panic| {
        Exit::die(json!({"name": "Error", "message": panic_message(panic.as_ref())}))
    });
    emitter.exit(exit);
}

/// Sends `stream` as ack-gated chunks and returns its terminal exit. At most
/// one chunk is unacked; items that arrive meanwhile are batched into the
/// next chunk. The exit follows the ack of the last chunk.
async fn run_stream(
    mut stream: BoxStream<'static, StreamItem>,
    emitter: &Emitter,
    mut acks: mpsc::UnboundedReceiver<()>,
) -> Exit {
    let mut buffer = Vec::new();
    let mut awaiting_ack = false;
    let mut end: Option<Exit> = None;
    loop {
        if !awaiting_ack {
            if !buffer.is_empty() {
                // Acks that arrived before this chunk don't release it.
                while acks.try_recv().is_ok() {}
                emitter.chunk(std::mem::take(&mut buffer));
                awaiting_ack = true;
                continue;
            }
            if let Some(exit) = end {
                return exit;
            }
        }
        let pull = end.is_none() && buffer.len() < MAX_BUFFERED_ITEMS;
        tokio::select! {
            biased;
            ack = acks.recv(), if awaiting_ack => match ack {
                Some(()) => awaiting_ack = false,
                // The session dropped this request; the task is being aborted.
                None => return Exit::interrupt(None),
            },
            item = stream.next(), if pull => {
                end = take_item(item, &mut buffer);
                // Batch everything else that is already available.
                while end.is_none() && buffer.len() < MAX_BUFFERED_ITEMS {
                    match stream.next().now_or_never() {
                        Some(item) => end = take_item(item, &mut buffer),
                        None => break,
                    }
                }
            }
        }
    }
}

/// Adds a stream item to `buffer`; returns the exit if the stream ended.
fn take_item(item: Option<StreamItem>, buffer: &mut Vec<Value>) -> Option<Exit> {
    match item {
        Some(StreamItem::Value(value)) => {
            buffer.push(value);
            None
        }
        Some(StreamItem::Fail(error)) => Some(Exit::fail(error)),
        None => Some(Exit::success(Value::Null)),
    }
}

/// The connection-level defect effect sends for an undecodable frame.
fn syntax_error_defect(error: &dyn std::fmt::Display) -> ServerMessage {
    ServerMessage::defect(json!({"name": "SyntaxError", "message": error.to_string()}))
}

/// Renders a JSON value the way a JS template literal would.
fn js_display(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_owned(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

fn panic_message(panic: &(dyn Any + Send)) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else {
        "handler panicked".to_owned()
    }
}
