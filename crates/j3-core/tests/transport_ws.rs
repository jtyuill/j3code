//! Effect RPC session behaviour over a real WebSocket.

mod transport_support;

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures_util::{
    FutureExt, StreamExt,
    future::{self, BoxFuture},
    stream,
};
use j3_core::{
    attachments::AttachmentStore,
    rpc::{Response, RpcHandler, StreamItem},
};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use transport_support::{TempDir, Ws, http, start};

/// Counts drops, to prove interrupted work is cancelled.
struct DropGuard(Arc<AtomicUsize>);

impl Drop for DropGuard {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct TestHandler {
    /// Items for the next `Feed` stream, pushed by the test.
    feed: Mutex<Option<mpsc::UnboundedReceiver<StreamItem>>>,
    dropped: Arc<AtomicUsize>,
}

impl TestHandler {
    fn feed(&self) -> mpsc::UnboundedSender<StreamItem> {
        let (sender, receiver) = mpsc::unbounded_channel();
        *self.feed.lock().unwrap() = Some(receiver);
        sender
    }
}

impl RpcHandler for TestHandler {
    fn call(&self, tag: String, payload: Value) -> BoxFuture<'static, Response> {
        match tag.as_str() {
            "Echo" => {
                let text = payload["text"]
                    .as_str()
                    .unwrap()
                    .repeat(payload["n"].as_u64().unwrap() as usize);
                let len = text.len();
                future::ready(Response::ok(json!({"text": text, "len": len}))).boxed()
            }
            "Lookup" => future::ready(Response::fail(
                json!({"_tag": "NotFound", "id": payload["id"]}),
            ))
            .boxed(),
            "Boom" => future::ready(Response::die("kaboom")).boxed(),
            "Panic" => async { panic!("kaboom") }.boxed(),
            "Count" => {
                let up_to = payload["upTo"].as_u64().unwrap();
                let items = (1..=up_to).map(|n| StreamItem::Value(json!(n)));
                future::ready(Response::Stream(stream::iter(items).boxed())).boxed()
            }
            "BreakMidway" => {
                let items = vec![
                    StreamItem::Value(json!(1)),
                    StreamItem::Value(json!(2)),
                    StreamItem::Fail(json!({"_tag": "StreamBroke", "at": 3})),
                    StreamItem::Value(json!(4)),
                ];
                future::ready(Response::Stream(stream::iter(items).boxed())).boxed()
            }
            "Feed" => {
                let mut receiver = self.feed.lock().unwrap().take().unwrap();
                let items = stream::poll_fn(move |cx| receiver.poll_recv(cx));
                future::ready(Response::Stream(items.boxed())).boxed()
            }
            "Forever" => {
                let guard = DropGuard(self.dropped.clone());
                let items = stream::unfold((guard, 0u64), |(guard, n)| async move {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    Some((StreamItem::Value(json!(n)), (guard, n + 1)))
                });
                future::ready(Response::Stream(items.boxed())).boxed()
            }
            "Slow" => {
                let guard = DropGuard(self.dropped.clone());
                async move {
                    future::pending::<()>().await;
                    drop(guard);
                    Response::ok(Value::Null)
                }
                .boxed()
            }
            other => future::ready(Response::die(format!("Unknown request tag: {other}"))).boxed(),
        }
    }
}

async fn setup() -> (Arc<TestHandler>, std::net::SocketAddr, TempDir) {
    let dir = TempDir::new("transport-ws");
    let handler = Arc::new(TestHandler::default());
    let store = Arc::new(AttachmentStore::open(dir.0.clone()).unwrap());
    let addr = start(handler.clone(), store).await;
    (handler, addr, dir)
}

async fn wait_for_drops(counter: &AtomicUsize, expected: usize) {
    for _ in 0..200 {
        if counter.load(Ordering::SeqCst) >= expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("work was not cancelled");
}

fn assert_interrupt_exit(frame: &Value, id: Value) {
    assert_eq!(frame["_tag"], "Exit");
    assert_eq!(frame["requestId"], id);
    assert_eq!(frame["exit"]["_tag"], "Failure");
    let cause = frame["exit"]["cause"].as_array().unwrap();
    assert_eq!(cause.len(), 1);
    assert_eq!(cause[0]["_tag"], "Interrupt");
    assert!(cause[0]["fiberId"].is_i64(), "fiberId like effect: {frame}");
}

#[tokio::test]
async fn transport_unary_success_fail_die() {
    let (_handler, addr, _dir) = setup().await;
    let mut ws = Ws::connect(addr).await;

    // The client's exact frame, trace fields included.
    ws.send_raw(r#"{"_tag":"Request","id":0,"tag":"Echo","payload":{"text":"ab","n":3},"traceId":"c4eefc760890fbe92a0be451d0f788a1","spanId":"36129aee6ef293f4","sampled":true,"headers":[]}"#).await;
    assert_eq!(
        ws.recv().await,
        json!({"_tag":"Exit","requestId":0,"exit":{"_tag":"Success","value":{"text":"ababab","len":6}}})
    );

    ws.request(json!(1), "Lookup", json!({"id": "x1"})).await;
    assert_eq!(
        ws.recv().await,
        json!({"_tag":"Exit","requestId":1,"exit":{"_tag":"Failure","cause":[{"_tag":"Fail","error":{"_tag":"NotFound","id":"x1"}}]}})
    );

    ws.request(json!(2), "Boom", json!({})).await;
    assert_eq!(
        ws.recv().await,
        json!({"_tag":"Exit","requestId":2,"exit":{"_tag":"Failure","cause":[{"_tag":"Die","defect":"kaboom"}]}})
    );

    // A panicking handler is a Die on that request, not a dead socket.
    ws.request(json!(3), "Panic", json!({})).await;
    assert_eq!(
        ws.recv().await,
        json!({"_tag":"Exit","requestId":3,"exit":{"_tag":"Failure","cause":[{"_tag":"Die","defect":{"name":"Error","message":"kaboom"}}]}})
    );
    ws.send(json!({"_tag": "Ping"})).await;
    assert_eq!(ws.recv().await, json!({"_tag": "Pong"}));
}

#[tokio::test]
async fn transport_unknown_tag_dies_only_that_request() {
    let (handler, addr, _dir) = setup().await;
    let mut ws = Ws::connect(addr).await;
    let feed = handler.feed();
    ws.request(json!(7), "Feed", json!({})).await;
    feed.send(StreamItem::Value(json!("a"))).unwrap();
    assert_eq!(
        ws.recv().await,
        json!({"_tag":"Chunk","requestId":7,"values":["a"]})
    );
    ws.ack(json!(7)).await;

    // String ids are echoed as strings (fixture raw-unknown-tag).
    ws.send_raw(r#"{"_tag":"Request","id":"100","tag":"Nope","payload":{},"headers":[]}"#)
        .await;
    assert_eq!(
        ws.recv().await,
        json!({"_tag":"Exit","requestId":"100","exit":{"_tag":"Failure","cause":[{"_tag":"Die","defect":"Unknown request tag: Nope"}]}})
    );

    // The stream on the same socket keeps working.
    feed.send(StreamItem::Value(json!("b"))).unwrap();
    assert_eq!(
        ws.recv().await,
        json!({"_tag":"Chunk","requestId":7,"values":["b"]})
    );
    ws.ack(json!(7)).await;
    drop(feed);
    assert_eq!(
        ws.recv().await,
        json!({"_tag":"Exit","requestId":7,"exit":{"_tag":"Success","value":null}})
    );
}

#[tokio::test]
async fn transport_stream_ack_gating_and_batching() {
    let (handler, addr, _dir) = setup().await;
    let mut ws = Ws::connect(addr).await;
    let feed = handler.feed();
    ws.request(json!(4), "Feed", json!({})).await;

    feed.send(StreamItem::Value(json!(1))).unwrap();
    assert_eq!(
        ws.recv().await,
        json!({"_tag":"Chunk","requestId":4,"values":[1]})
    );
    // Items produced while the chunk is unacked wait, then go as one batch.
    for n in 2..=4 {
        feed.send(StreamItem::Value(json!(n))).unwrap();
    }
    ws.expect_silence(200).await;
    ws.ack(json!(4)).await;
    assert_eq!(
        ws.recv().await,
        json!({"_tag":"Chunk","requestId":4,"values":[2,3,4]})
    );
    // The end of the stream also waits for the last chunk's ack.
    drop(feed);
    ws.expect_silence(200).await;
    ws.ack(json!(4)).await;
    assert_eq!(
        ws.recv().await,
        json!({"_tag":"Exit","requestId":4,"exit":{"_tag":"Success","value":null}})
    );

    // Everything ready at once is one chunk (fixture stream-batched).
    ws.request(json!(5), "Count", json!({"upTo": 3})).await;
    assert_eq!(
        ws.recv().await,
        json!({"_tag":"Chunk","requestId":5,"values":[1,2,3]})
    );
    ws.expect_silence(100).await;
    ws.ack(json!(5)).await;
    assert_eq!(
        ws.recv().await,
        json!({"_tag":"Exit","requestId":5,"exit":{"_tag":"Success","value":null}})
    );

    // A failing stream delivers what came before the failure, then Fail.
    ws.request(json!(6), "BreakMidway", json!({})).await;
    assert_eq!(
        ws.recv().await,
        json!({"_tag":"Chunk","requestId":6,"values":[1,2]})
    );
    ws.ack(json!(6)).await;
    assert_eq!(
        ws.recv().await,
        json!({"_tag":"Exit","requestId":6,"exit":{"_tag":"Failure","cause":[{"_tag":"Fail","error":{"_tag":"StreamBroke","at":3}}]}})
    );
}

#[tokio::test]
async fn transport_interrupt_cancels_stream_and_unary() {
    let (handler, addr, _dir) = setup().await;
    let mut ws = Ws::connect(addr).await;

    ws.request(json!(6), "Forever", json!({})).await;
    assert_eq!(
        ws.recv().await,
        json!({"_tag":"Chunk","requestId":6,"values":[0]})
    );
    ws.ack(json!(6)).await;
    let second = ws.recv().await;
    assert_eq!(second["_tag"], "Chunk");
    ws.ack(json!(6)).await;
    ws.send(json!({"_tag": "Interrupt", "requestId": 6})).await;
    // A chunk may already be on the wire; then comes the interrupt Exit.
    let mut frame = ws.recv().await;
    while frame["_tag"] == "Chunk" {
        frame = ws.recv().await;
    }
    assert_interrupt_exit(&frame, json!(6));
    wait_for_drops(&handler.dropped, 1).await;
    // Nothing more for the interrupted stream; acks for it are ignored.
    ws.ack(json!(6)).await;
    ws.expect_silence(150).await;

    ws.request(json!(7), "Slow", json!({})).await;
    ws.send(json!({"_tag": "Ping"})).await;
    assert_eq!(ws.recv().await, json!({"_tag": "Pong"}));
    ws.send(json!({"_tag": "Interrupt", "requestId": 7})).await;
    assert_interrupt_exit(&ws.recv().await, json!(7));
    wait_for_drops(&handler.dropped, 2).await;

    // Interrupting a finished request sends nothing.
    ws.send(json!({"_tag": "Interrupt", "requestId": 7})).await;
    ws.expect_silence(150).await;
}

#[tokio::test]
async fn transport_ping_pong() {
    let (_handler, addr, _dir) = setup().await;
    let mut ws = Ws::connect(addr).await;
    for _ in 0..3 {
        ws.send_raw(r#"{"_tag":"Ping"}"#).await;
        assert_eq!(ws.recv().await, json!({"_tag": "Pong"}));
    }
}

#[tokio::test]
async fn transport_array_frame() {
    let (_handler, addr, _dir) = setup().await;
    let mut ws = Ws::connect(addr).await;
    ws.send_raw(r#"[{"_tag":"Request","id":"102","tag":"Echo","payload":{"text":"a","n":1},"headers":[]},{"_tag":"Ping"}]"#).await;
    let mut frames = vec![ws.recv().await, ws.recv().await];
    frames.sort_by_key(|frame| frame["_tag"].as_str().unwrap().to_owned());
    assert_eq!(
        frames,
        vec![
            json!({"_tag":"Exit","requestId":"102","exit":{"_tag":"Success","value":{"text":"a","len":1}}}),
            json!({"_tag":"Pong"}),
        ]
    );
}

#[tokio::test]
async fn transport_malformed_json_defect() {
    let (_handler, addr, _dir) = setup().await;
    let mut ws = Ws::connect(addr).await;
    ws.send_raw("{not json").await;
    let frame = ws.recv().await;
    assert_eq!(frame["_tag"], "Defect");
    assert_eq!(frame["defect"]["name"], "SyntaxError");
    assert!(frame["defect"]["message"].is_string());
    assert_eq!(frame.as_object().unwrap().len(), 2);

    // An unknown message tag is a connection Defect, as in effect.
    ws.send_raw(r#"{"_tag":"Bogus"}"#).await;
    assert_eq!(
        ws.recv().await,
        json!({"_tag":"Defect","defect":"Unknown request tag: Bogus"})
    );

    // The socket stays usable.
    ws.send_raw(r#"{"_tag":"Ping"}"#).await;
    assert_eq!(ws.recv().await, json!({"_tag": "Pong"}));
}

#[tokio::test]
async fn transport_socket_close_cancels_requests() {
    let (handler, addr, _dir) = setup().await;
    let mut ws = Ws::connect(addr).await;
    ws.request(json!(1), "Slow", json!({})).await;
    ws.request(json!(2), "Forever", json!({})).await;
    assert_eq!(ws.recv().await["_tag"], "Chunk");
    ws.close().await;
    wait_for_drops(&handler.dropped, 2).await;
}

#[tokio::test]
async fn transport_ws_requires_orchestration_protocol_2() {
    let (_handler, addr, _dir) = setup().await;
    let expected = json!({
        "code": "orchestration_protocol_incompatible",
        "message": "Update this client to one that supports orchestration protocol 2.",
        "orchestrationProtocolVersion": 2,
    });
    for path in ["/ws", "/ws?orchestrationProtocol=1", "/ws?clientOs=iOS"] {
        let response = http(addr, "GET", path, &[], b"").await;
        assert_eq!(response.status, 426, "{path}");
        assert_eq!(response.header("content-type"), Some("application/json"));
        assert_eq!(response.json(), expected);
    }

    // A real WebSocket handshake without the parameter is refused the same way.
    let refused = tokio_tungstenite::connect_async(format!("ws://{addr}/ws")).await;
    match refused {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status().as_u16(), 426);
        }
        other => panic!("expected a 426 handshake failure, got {other:?}"),
    }
}
