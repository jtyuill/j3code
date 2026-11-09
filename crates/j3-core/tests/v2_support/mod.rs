//! Shared harness for the protocol v2 integration tests
//! (`docs/protocol/wire.md`): the app served on a real socket, a v2
//! WebSocket client, a thread mirror built from `threads.watch`, a minimal
//! HTTP/1.1 client and temporary directories.

#![allow(dead_code)]

use std::{
    collections::{HashMap, VecDeque},
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use j3_core::app::App;
use j3_protocol::v2::{
    methods::TerminalItem,
    streams::{ShellItem, ThreadItem, ThreadSnapshot},
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::Instant,
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

/// The longest any single expected frame may take.
pub const WAIT: Duration = Duration::from_secs(10);

/// Serves `app` behind the production HTTP router on 127.0.0.1:<ephemeral>
/// for the rest of the test.
pub async fn serve(app: &Arc<App>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let routes = j3_core::http::router(Arc::clone(app));
    tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    addr
}

// ---------------------------------------------------------------------------
// v2 client
// ---------------------------------------------------------------------------

/// The item type a stream's items are checked against.
#[derive(Clone, Copy)]
enum StreamKind {
    Shell,
    Thread,
    Terminal,
    /// Items not checked against a protocol type.
    Other,
}

/// A v2 client on one socket. Every `items` frame is acknowledged as it
/// arrives, and items of `shell.watch`, `threads.watch` and
/// `terminal.attach` are checked against the protocol types; frames are
/// buffered per call id until a test asks for them.
pub struct Client {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
    /// Non-`items` frames read but not yet taken, in arrival order.
    frames: VecDeque<Value>,
    /// Stream items read but not yet taken, by call id.
    items: HashMap<u64, VecDeque<Value>>,
    streams: HashMap<u64, StreamKind>,
}

impl Client {
    pub async fn connect(addr: SocketAddr) -> Self {
        let url = format!("ws://{addr}/api/v1/ws?protocol=2");
        let (socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        Self {
            socket,
            next_id: 1,
            frames: VecDeque::new(),
            items: HashMap::new(),
            streams: HashMap::new(),
        }
    }

    /// Sends one raw frame.
    pub async fn send(&mut self, frame: Value) {
        self.socket
            .send(Message::text(frame.to_string()))
            .await
            .unwrap();
    }

    /// Reads one frame before `deadline`; false on timeout.
    async fn pump(&mut self, deadline: Instant) -> bool {
        let frame = loop {
            let Ok(next) = tokio::time::timeout_at(deadline, self.socket.next()).await else {
                return false;
            };
            match next.expect("socket closed").expect("socket error") {
                Message::Text(text) => break serde_json::from_str::<Value>(text.as_str()).unwrap(),
                Message::Close(frame) => panic!("socket closed by server: {frame:?}"),
                _ => {}
            }
        };
        if frame["t"] != "items" {
            self.frames.push_back(frame);
            return true;
        }
        let id = frame["id"].as_u64().unwrap();
        let items = frame["items"].as_array().unwrap();
        assert!(!items.is_empty(), "empty items frame: {frame}");
        let kind = self.streams.get(&id).copied().unwrap_or(StreamKind::Other);
        for item in items {
            let checked = match kind {
                StreamKind::Shell => serde_json::from_value::<ShellItem>(item.clone()).err(),
                StreamKind::Thread => serde_json::from_value::<ThreadItem>(item.clone()).err(),
                StreamKind::Terminal => serde_json::from_value::<TerminalItem>(item.clone()).err(),
                StreamKind::Other => None,
            };
            if let Some(error) = checked {
                panic!("bad item on stream {id} {item}: {error}");
            }
        }
        self.items
            .entry(id)
            .or_default()
            .extend(items.iter().cloned());
        self.send(json!({"t": "ack", "id": id, "n": 1})).await;
        true
    }

    /// The first buffered or incoming non-`items` frame matching `wanted`.
    pub async fn frame(&mut self, what: &str, wanted: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(index) = self.frames.iter().position(&wanted) {
                return self.frames.remove(index).unwrap();
            }
            if !self.pump(deadline).await {
                panic!("timed out waiting for {what}; unread: {:?}", self.frames);
            }
        }
    }

    /// Sends a `call`; its id.
    async fn send_call(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"t": "call", "id": id, "method": method, "params": params}))
            .await;
        id
    }

    pub async fn call_frame(&mut self, method: &str, params: Value) -> Value {
        let id = self.send_call(method, params).await;
        self.frame(method, |frame| frame["id"] == id).await
    }

    /// A unary call: its `ok` result or `err` error.
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, Value> {
        let frame = self.call_frame(method, params).await;
        match frame["t"].as_str() {
            Some("ok") => Ok(frame["result"].clone()),
            Some("err") => Err(frame["error"].clone()),
            _ => panic!("{method}: unexpected frame {frame}"),
        }
    }

    pub async fn ok(&mut self, method: &str, params: Value) -> Value {
        self.call(method, params.clone())
            .await
            .unwrap_or_else(|error| panic!("{method} {params} failed: {error}"))
    }

    pub async fn err(&mut self, method: &str, params: Value) -> Value {
        match self.call(method, params.clone()).await {
            Ok(result) => panic!("{method} {params} succeeded: {result}"),
            Err(error) => error,
        }
    }

    /// Opens stream `method`; its call id.
    pub async fn watch(&mut self, method: &str, params: Value) -> u64 {
        let kind = match method {
            "shell.watch" => StreamKind::Shell,
            "threads.watch" => StreamKind::Thread,
            "terminal.attach" => StreamKind::Terminal,
            _ => StreamKind::Other,
        };
        let id = self.send_call(method, params).await;
        self.streams.insert(id, kind);
        id
    }

    /// The frame ending call `id` (`end`, `err` or `ok`).
    pub async fn ended(&mut self, id: u64) -> Value {
        self.frame(&format!("the end of {id}"), |frame| frame["id"] == id)
            .await
    }

    /// The next item of stream `id` before `deadline`; None on timeout.
    /// Panics when the stream ends.
    pub async fn next_item(&mut self, id: u64, deadline: Instant) -> Option<Value> {
        loop {
            if let Some(item) = self.items.get_mut(&id).and_then(VecDeque::pop_front) {
                return Some(item);
            }
            if let Some(end) = self.frames.iter().find(|frame| frame["id"] == id) {
                panic!("stream {id} ended: {end}");
            }
            if !self.pump(deadline).await {
                return None;
            }
        }
    }

    pub async fn item(&mut self, id: u64) -> Value {
        let deadline = Instant::now() + WAIT;
        self.next_item(id, deadline)
            .await
            .unwrap_or_else(|| panic!("no item on stream {id}"))
    }

    /// Reads stream `id` up to the first item matching `wanted`.
    pub async fn wait_item(
        &mut self,
        id: u64,
        what: &str,
        wanted: impl Fn(&Value) -> bool,
    ) -> Value {
        let deadline = Instant::now() + WAIT;
        let mut seen = Vec::new();
        loop {
            let Some(item) = self.next_item(id, deadline).await else {
                panic!("timed out waiting for {what}; saw {seen:#?}");
            };
            if wanted(&item) {
                return item;
            }
            seen.push(item);
        }
    }

    /// Reads `threads.watch` stream `id` into `mirror` until `done` holds for
    /// the item just applied and the resulting state. `done` is first asked
    /// with a `null` item, so a state the mirror already shows returns at once.
    pub async fn follow(
        &mut self,
        id: u64,
        mirror: &mut Mirror,
        what: &str,
        done: impl Fn(&Value, &Mirror) -> bool,
    ) -> Value {
        if done(&Value::Null, mirror) {
            return Value::Null;
        }
        let deadline = Instant::now() + WAIT;
        let mut seen = Vec::new();
        loop {
            let Some(item) = self.next_item(id, deadline).await else {
                panic!("timed out waiting for {what}; saw {seen:#?}\nstate {mirror:#?}");
            };
            mirror.apply(&item);
            if done(&item, mirror) {
                return item;
            }
            seen.push(item);
        }
    }

    /// Reads `threads.watch` stream `id` into `mirror` until it shows what
    /// `threads.get` reports; the stream and the getter must agree.
    pub async fn converge(&mut self, id: u64, mirror: &mut Mirror, thread_id: &str) {
        let got = self.ok("threads.get", json!({"threadId": thread_id})).await;
        serde_json::from_value::<ThreadSnapshot>(got.clone())
            .unwrap_or_else(|error| panic!("threads.get result {got}: {error}"));
        assert!(
            got["sequence"].as_u64().unwrap() >= mirror.sequence,
            "threads.get ({}) is older than the stream ({})",
            got["sequence"],
            mirror.sequence
        );
        let deadline = Instant::now() + WAIT;
        let mut seen = Vec::new();
        while !mirror.matches(&got) {
            match self.next_item(id, deadline).await {
                Some(item) => {
                    mirror.apply(&item);
                    seen.push(item);
                }
                None => {
                    eprintln!("stream items after threads.get: {seen:#?}");
                    mirror.assert_matches(&got);
                }
            }
        }
    }

    pub async fn close(mut self) {
        let _ = self.socket.close(None).await;
    }
}

/// A client's view of one thread, built from `threads.watch` items the way
/// wire.md §6 describes.
#[derive(Debug, Default)]
pub struct Mirror {
    pub sequence: u64,
    pub thread: Value,
    pub turns: Vec<Value>,
    pub items: Vec<Value>,
    pub requests: Vec<Value>,
    pub queue: Vec<Value>,
    /// Deltas seen, by item id.
    pub deltas: HashMap<String, usize>,
}

fn upsert(rows: &mut Vec<Value>, row: &Value) {
    match rows.iter_mut().find(|old| old["id"] == row["id"]) {
        Some(old) => *old = row.clone(),
        None => rows.push(row.clone()),
    }
}

impl Mirror {
    pub fn apply(&mut self, item: &Value) {
        if let Some(sequence) = item["sequence"].as_u64() {
            assert!(
                sequence >= self.sequence,
                "sequence went back from {} to {sequence}: {item}",
                self.sequence
            );
            self.sequence = sequence;
        }
        match item["type"].as_str().unwrap() {
            "snapshot" => {
                self.thread = item["thread"].clone();
                self.turns = item["turns"].as_array().unwrap().clone();
                self.items = item["items"].as_array().unwrap().clone();
                self.requests = item["requests"].as_array().unwrap().clone();
                self.queue = item["queue"].as_array().unwrap().clone();
            }
            "thread" => self.thread = item["thread"].clone(),
            "turn" => upsert(&mut self.turns, &item["turn"]),
            "item" => upsert(&mut self.items, &item["item"]),
            "item.delta" => {
                let id = item["itemId"].as_str().unwrap();
                *self.deltas.entry(id.to_owned()).or_default() += 1;
                let target = self
                    .items
                    .iter_mut()
                    .find(|row| row["id"] == id)
                    .unwrap_or_else(|| panic!("delta for an item never sent: {item}"));
                let field = match item["field"].as_str().unwrap() {
                    "text" => &mut target["text"],
                    "output" => &mut target["output"]["text"],
                    other => panic!("unknown delta field {other}"),
                };
                let text = format!(
                    "{}{}",
                    field.as_str().unwrap_or_default(),
                    item["append"].as_str().unwrap()
                );
                *field = json!(text);
            }
            "request" => upsert(&mut self.requests, &item["request"]),
            "queue" => self.queue = item["queue"].as_array().unwrap().clone(),
            "synced" | "thread.removed" => {}
            other => panic!("unknown thread item {other}"),
        }
        self.turns.sort_by_key(|turn| turn["ordinal"].as_u64());
        self.items.sort_by_key(|row| row["ordinal"].as_u64());
    }

    pub fn turn(&self, turn_id: &str) -> &Value {
        self.turns
            .iter()
            .find(|turn| turn["id"] == turn_id)
            .unwrap_or_else(|| panic!("no turn {turn_id}: {:#?}", self.turns))
    }

    pub fn items_of(&self, kind: &str) -> Vec<&Value> {
        self.items
            .iter()
            .filter(|row| row["kind"] == kind)
            .collect()
    }

    /// Whether `snapshot` (a `threads.get` result) shows this state.
    pub fn matches(&self, snapshot: &Value) -> bool {
        snapshot["thread"] == self.thread
            && snapshot["turns"] == json!(self.turns)
            && snapshot["items"] == json!(self.items)
            && snapshot["requests"] == json!(self.requests)
            && snapshot["queue"] == json!(self.queue)
    }

    pub fn assert_matches(&self, snapshot: &Value) {
        assert_eq!(snapshot["thread"], self.thread, "thread row");
        assert_eq!(snapshot["turns"], json!(self.turns), "turns");
        assert_eq!(snapshot["items"], json!(self.items), "items");
        assert_eq!(snapshot["requests"], json!(self.requests), "requests");
        assert_eq!(snapshot["queue"], json!(self.queue), "queue");
    }
}

// ---------------------------------------------------------------------------
// Directories and HTTP
// ---------------------------------------------------------------------------

/// A fresh directory under the system temp dir, removed on drop.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "j3-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

/// One HTTP/1.1 request on a fresh connection. A `content-length` header is
/// added for non-empty bodies unless `headers` sets one.
pub async fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> HttpResponse {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let mut head = format!("{method} {path} HTTP/1.1\r\nhost: {addr}\r\nconnection: close\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    let has_length = headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-length"));
    if !body.is_empty() && !has_length {
        head.push_str(&format!("content-length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    let mut raw = Vec::new();
    tokio::time::timeout(WAIT, stream.read_to_end(&mut raw))
        .await
        .unwrap()
        .unwrap();

    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("response head");
    let head = std::str::from_utf8(&raw[..split]).unwrap();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers: Vec<(String, String)> = lines
        .map(|line| {
            let (name, value) = line.split_once(':').unwrap();
            (name.trim().to_owned(), value.trim().to_owned())
        })
        .collect();
    let mut body = raw[split + 4..].to_vec();
    let chunked = headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("transfer-encoding") && value.eq_ignore_ascii_case("chunked")
    });
    if chunked {
        body = dechunk(&body);
    }
    HttpResponse {
        status,
        headers,
        body,
    }
}

fn dechunk(mut raw: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    loop {
        let line_end = raw.windows(2).position(|w| w == b"\r\n").unwrap();
        let size_text = std::str::from_utf8(&raw[..line_end]).unwrap();
        let size = usize::from_str_radix(size_text.split(';').next().unwrap().trim(), 16).unwrap();
        raw = &raw[line_end + 2..];
        if size == 0 {
            return body;
        }
        body.extend_from_slice(&raw[..size]);
        raw = &raw[size + 2..];
    }
}
