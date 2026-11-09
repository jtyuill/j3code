//! v2 WebSocket session (`docs/protocol/wire.md` §3).
//!
//! One socket is one session. Every `call` runs on its own task, so calls on
//! a socket proceed concurrently. A [`Handler`] answers each call with a
//! [`Reply`]: `ok`/`err`, or a stream sent as window-gated `items` frames
//! followed by `end` or `err`.
//!
//! The session loop owns all per-call state. Call tasks hand their frames to
//! the loop, which forwards them only while the call is still open; that keeps
//! a cancelled call from sending anything after its `cancelled` error.

use std::{any::Any, collections::HashMap, panic::AssertUnwindSafe, sync::Arc, time::Duration};

use axum::extract::ws::{Message, WebSocket};
use futures_util::{
    FutureExt, SinkExt, StreamExt,
    future::BoxFuture,
    stream::{BoxStream, SplitSink},
};
use j3_protocol::v2::envelope::{
    CallId, ClientFrame, Error, ErrorCode, SERVER_TIMEOUT_SECS, ServerFrame,
};
use serde_json::Value;
use tokio::{sync::mpsc, task::AbortHandle, time::Instant};

/// Answers v2 calls.
pub trait Handler: Send + Sync + 'static {
    /// One call per `call` frame. Unknown methods: `Reply::Err` with
    /// `unknown_method`.
    fn call(&self, method: String, params: Value) -> BoxFuture<'static, Reply>;
}

/// A handler's answer to one call.
pub enum Reply {
    Ok(Value),
    Err(Error),
    /// `window`: initial number of `items` frames (wire.md §3.2; 8 normally, 64 for terminals).
    Stream {
        window: u32,
        items: BoxStream<'static, StreamEvent>,
    },
}

/// One element of a [`Reply::Stream`].
pub enum StreamEvent {
    Item(Value),
    /// Ends the stream with `err`.
    Fail(Error),
}

/// Most stream items buffered while the window is empty. Past this the
/// stream is not polled until credit arrives.
const MAX_BUFFERED_ITEMS: usize = 1024;

/// Runs one v2 session over `socket` until either side closes it. Every open
/// call is aborted when the session ends.
pub async fn serve_socket(socket: WebSocket, handler: Arc<dyn Handler>) {
    let (sink, mut source) = socket.split();
    let (out_tx, out_rx) = mpsc::unbounded_channel();
    let mut writer = tokio::spawn(write_frames(sink, out_rx));
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let mut session = Session {
        handler,
        out: out_tx,
        events: events_tx,
        open: HashMap::new(),
        next_slot: 0,
    };
    let timeout = Duration::from_secs(SERVER_TIMEOUT_SECS);
    let idle = tokio::time::sleep(timeout);
    tokio::pin!(idle);
    let flush = loop {
        tokio::select! {
            frame = source.next() => {
                idle.as_mut().reset(Instant::now() + timeout);
                let ok = match frame {
                    Some(Ok(Message::Text(text))) => session.on_frame(text.as_str()),
                    Some(Ok(Message::Binary(_))) => session.on_frame(""),
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => true,
                    Some(Ok(Message::Close(_)) | Err(_)) | None => break false,
                };
                if !ok {
                    break true;
                }
            }
            Some(event) = events_rx.recv() => session.on_event(event),
            () = &mut idle => break true,
        }
    };
    // Dropping the session aborts every call task and closes the writer's
    // channel, so the writer sends what is queued and closes the socket.
    drop(session);
    if flush {
        let _ = (&mut writer).await;
    } else {
        writer.abort();
    }
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
    let _ = sink.close().await;
}

/// A frame from a call task to the session loop.
struct Event {
    id: CallId,
    /// Identifies the task instance, so a late frame from a cancelled call is
    /// not mistaken for one from a newer call reusing its id.
    slot: u64,
    frame: ServerFrame,
    /// The call's terminal frame.
    last: bool,
}

struct Open {
    slot: u64,
    task: AbortHandle,
    acks: mpsc::UnboundedSender<u32>,
}

struct Session {
    handler: Arc<dyn Handler>,
    out: mpsc::UnboundedSender<String>,
    events: mpsc::UnboundedSender<Event>,
    open: HashMap<CallId, Open>,
    next_slot: u64,
}

impl Drop for Session {
    fn drop(&mut self) {
        for (_, call) in self.open.drain() {
            call.task.abort();
        }
    }
}

impl Session {
    fn send(&self, frame: &ServerFrame) {
        match serde_json::to_string(frame) {
            // A closed writer means the socket is gone; the read side notices.
            Ok(text) => {
                let _ = self.out.send(text);
            }
            Err(error) => tracing::error!(%error, "failed to encode a v2 server frame"),
        }
    }

    /// Handles one text frame; false when it was malformed and the session
    /// must close.
    fn on_frame(&mut self, text: &str) -> bool {
        let frame = match serde_json::from_str::<ClientFrame>(text) {
            Ok(frame) => frame,
            Err(error) => {
                self.send(&ServerFrame::Err {
                    id: None,
                    error: Error::bad_request(format!("malformed frame: {error}")),
                });
                return false;
            }
        };
        match frame {
            ClientFrame::Call { id, method, params } => self.start(id, method, params),
            ClientFrame::Ack { id, n } => {
                if let Some(call) = self.open.get(&id) {
                    let _ = call.acks.send(n);
                }
            }
            ClientFrame::Cancel { id } => {
                if let Some(call) = self.open.remove(&id) {
                    call.task.abort();
                    self.send(&ServerFrame::Err {
                        id: Some(id),
                        error: Error::new(ErrorCode::Cancelled, "cancelled"),
                    });
                }
            }
            ClientFrame::Ping => self.send(&ServerFrame::Pong),
        }
        true
    }

    fn start(&mut self, id: CallId, method: String, params: Value) {
        if self.open.contains_key(&id) {
            self.send(&ServerFrame::Err {
                id: Some(id),
                error: Error::bad_request(format!("call id {id} is already open")),
            });
            return;
        }
        let slot = self.next_slot;
        self.next_slot += 1;
        let (acks_tx, acks_rx) = mpsc::unbounded_channel();
        let emitter = Emitter {
            events: self.events.clone(),
            id,
            slot,
        };
        let task = tokio::spawn(run_call(
            self.handler.clone(),
            method,
            params,
            emitter,
            acks_rx,
        ));
        self.open.insert(
            id,
            Open {
                slot,
                task: task.abort_handle(),
                acks: acks_tx,
            },
        );
    }

    fn on_event(&mut self, event: Event) {
        let current = self
            .open
            .get(&event.id)
            .is_some_and(|call| call.slot == event.slot);
        if !current {
            return;
        }
        if event.last {
            self.open.remove(&event.id);
        }
        self.send(&event.frame);
    }
}

/// Sends one call's frames to the session loop.
struct Emitter {
    events: mpsc::UnboundedSender<Event>,
    id: CallId,
    slot: u64,
}

impl Emitter {
    fn emit(&self, frame: ServerFrame, last: bool) {
        let _ = self.events.send(Event {
            id: self.id,
            slot: self.slot,
            frame,
            last,
        });
    }

    fn err(&self, error: Error) {
        self.emit(
            ServerFrame::Err {
                id: Some(self.id),
                error,
            },
            true,
        );
    }
}

async fn run_call(
    handler: Arc<dyn Handler>,
    method: String,
    params: Value,
    emitter: Emitter,
    acks: mpsc::UnboundedReceiver<u32>,
) {
    let outcome = AssertUnwindSafe(async {
        match handler.call(method, params).await {
            Reply::Ok(result) => Ok(ServerFrame::Ok {
                id: emitter.id,
                result,
            }),
            Reply::Err(error) => Err(error),
            Reply::Stream { window, items } => run_stream(items, window, &emitter, acks)
                .await
                .map(|()| ServerFrame::End { id: emitter.id }),
        }
    })
    .catch_unwind()
    .await;
    match outcome {
        Ok(Ok(frame)) => emitter.emit(frame, true),
        Ok(Err(error)) => emitter.err(error),
        Err(panic) => emitter.err(Error::internal(panic_message(panic.as_ref()))),
    }
}

/// Sends `stream` as window-gated `items` frames. Items produced while the
/// window is empty are merged into the next frame.
async fn run_stream(
    mut stream: BoxStream<'static, StreamEvent>,
    mut window: u32,
    emitter: &Emitter,
    mut acks: mpsc::UnboundedReceiver<u32>,
) -> Result<(), Error> {
    let mut buffer = Vec::new();
    let mut end: Option<Result<(), Error>> = None;
    loop {
        if window > 0 && !buffer.is_empty() {
            window -= 1;
            emitter.emit(
                ServerFrame::Items {
                    id: emitter.id,
                    items: std::mem::take(&mut buffer),
                },
                false,
            );
        }
        if buffer.is_empty()
            && let Some(end) = end.take()
        {
            return end;
        }
        let pull = end.is_none() && buffer.len() < MAX_BUFFERED_ITEMS;
        tokio::select! {
            biased;
            ack = acks.recv() => match ack {
                Some(n) => window = window.saturating_add(n),
                // The session dropped this call; the task is being aborted.
                None => return Ok(()),
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

/// Adds a stream event to `buffer`; returns the outcome if the stream ended.
fn take_item(item: Option<StreamEvent>, buffer: &mut Vec<Value>) -> Option<Result<(), Error>> {
    match item {
        Some(StreamEvent::Item(value)) => {
            buffer.push(value);
            None
        }
        Some(StreamEvent::Fail(error)) => Some(Err(error)),
        None => Some(Ok(())),
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
