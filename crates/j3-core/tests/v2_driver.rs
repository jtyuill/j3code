//! The v2 session over a real WebSocket: unary replies, stream window gating,
//! cancel, malformed frames and duplicate call ids.

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{Router, extract::WebSocketUpgrade, routing::get};
use futures_util::{FutureExt, SinkExt, StreamExt, future::BoxFuture, stream};
use j3_core::v2::driver::{Handler, Reply, StreamEvent, serve_socket};
use j3_protocol::v2::envelope::Error;
use serde_json::{Value, json};
use tokio::{net::TcpStream, sync::mpsc};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

const WAIT: Duration = Duration::from_millis(500);
const QUIET: Duration = Duration::from_millis(50);

/// `echo` → ok params, `fail` → err rejected, `slow` → ok after a delay,
/// `feed` → stream of the values the test pushes, `pending` → silent stream.
struct TestHandler {
    feed: Mutex<Option<mpsc::UnboundedReceiver<Value>>>,
}

impl Handler for TestHandler {
    fn call(&self, method: String, params: Value) -> BoxFuture<'static, Reply> {
        let feed = (method == "feed").then(|| self.feed.lock().unwrap().take().unwrap());
        async move {
            match method.as_str() {
                "echo" => Reply::Ok(params),
                "fail" => Reply::Err(Error::rejected("no")),
                "slow" => {
                    tokio::time::sleep(QUIET * 2).await;
                    Reply::Ok(json!("done"))
                }
                "feed" => {
                    let mut rx = feed.unwrap();
                    let items = stream::poll_fn(move |cx| rx.poll_recv(cx)).map(StreamEvent::Item);
                    Reply::Stream {
                        window: 1,
                        items: items.boxed(),
                    }
                }
                _ => Reply::Stream {
                    window: 8,
                    items: stream::pending().boxed(),
                },
            }
        }
        .boxed()
    }
}

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn start() -> (Ws, mpsc::UnboundedSender<Value>) {
    let (feed_tx, feed_rx) = mpsc::unbounded_channel();
    let handler: Arc<dyn Handler> = Arc::new(TestHandler {
        feed: Mutex::new(Some(feed_rx)),
    });
    let app = Router::new().route(
        "/ws",
        get(move |ws: WebSocketUpgrade| {
            let handler = handler.clone();
            async move { ws.on_upgrade(move |socket| serve_socket(socket, handler)) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .unwrap();
    (ws, feed_tx)
}

async fn send(ws: &mut Ws, frame: Value) {
    ws.send(Message::Text(frame.to_string().into()))
        .await
        .unwrap();
}

/// Next text frame within `wait`; None on timeout. Panics on close.
async fn recv_within(ws: &mut Ws, wait: Duration) -> Option<Value> {
    match tokio::time::timeout(wait, ws.next()).await {
        Err(_) => None,
        Ok(Some(Ok(Message::Text(text)))) => Some(serde_json::from_str(&text).unwrap()),
        Ok(other) => panic!("expected a text frame, got {other:?}"),
    }
}

async fn recv(ws: &mut Ws) -> Value {
    recv_within(ws, WAIT)
        .await
        .expect("timed out waiting for a frame")
}

#[tokio::test]
async fn unary_ok_and_err() {
    let (mut ws, _feed) = start().await;
    send(
        &mut ws,
        json!({"t": "call", "id": 1, "method": "echo", "params": {"a": 1}}),
    )
    .await;
    assert_eq!(
        recv(&mut ws).await,
        json!({"t": "ok", "id": 1, "result": {"a": 1}})
    );
    send(&mut ws, json!({"t": "call", "id": 2, "method": "fail"})).await;
    assert_eq!(
        recv(&mut ws).await,
        json!({"t": "err", "id": 2, "error": {"code": "rejected", "message": "no"}})
    );
}

#[tokio::test]
async fn stream_frames_wait_for_ack_and_merge() {
    let (mut ws, feed) = start().await;
    send(&mut ws, json!({"t": "call", "id": 7, "method": "feed"})).await;
    feed.send(json!(1)).unwrap();
    assert_eq!(
        recv(&mut ws).await,
        json!({"t": "items", "id": 7, "items": [1]})
    );
    feed.send(json!(2)).unwrap();
    feed.send(json!(3)).unwrap();
    assert_eq!(recv_within(&mut ws, QUIET).await, None, "window is empty");
    send(&mut ws, json!({"t": "ack", "id": 7, "n": 1})).await;
    assert_eq!(
        recv(&mut ws).await,
        json!({"t": "items", "id": 7, "items": [2, 3]})
    );
    drop(feed);
    assert_eq!(recv(&mut ws).await, json!({"t": "end", "id": 7}));
}

#[tokio::test]
async fn cancel_ends_a_pending_stream() {
    let (mut ws, _feed) = start().await;
    send(&mut ws, json!({"t": "call", "id": 3, "method": "pending"})).await;
    send(&mut ws, json!({"t": "cancel", "id": 3})).await;
    let frame = recv(&mut ws).await;
    assert_eq!(frame["id"], 3);
    assert_eq!(frame["error"]["code"], "cancelled");
    assert_eq!(recv_within(&mut ws, QUIET).await, None);
}

#[tokio::test]
async fn malformed_frame_closes_the_socket() {
    let (mut ws, _feed) = start().await;
    send(&mut ws, json!({"t": "nope"})).await;
    let frame = recv(&mut ws).await;
    assert_eq!(frame["t"], "err");
    assert_eq!(frame["id"], Value::Null);
    assert_eq!(frame["error"]["code"], "bad_request");
    match tokio::time::timeout(WAIT, ws.next()).await.unwrap() {
        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {}
        other => panic!("expected close, got {other:?}"),
    }
}

#[tokio::test]
async fn duplicate_open_id_is_rejected() {
    let (mut ws, _feed) = start().await;
    send(&mut ws, json!({"t": "call", "id": 5, "method": "slow"})).await;
    send(&mut ws, json!({"t": "call", "id": 5, "method": "echo"})).await;
    let frame = recv(&mut ws).await;
    assert_eq!(frame["t"], "err");
    assert_eq!(frame["id"], 5);
    assert_eq!(frame["error"]["code"], "bad_request");
    assert_eq!(
        recv(&mut ws).await,
        json!({"t": "ok", "id": 5, "result": "done"})
    );
}
