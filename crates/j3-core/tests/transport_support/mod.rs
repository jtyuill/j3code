//! Helpers for the `transport_*` integration tests: a server on an
//! ephemeral port, a raw WebSocket client and a minimal HTTP/1.1 client.

#![allow(dead_code)]

use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use j3_core::{
    attachments::AttachmentStore,
    http::{HttpState, router},
    rpc::RpcHandler,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

pub const TIMEOUT: Duration = Duration::from_secs(5);

pub fn descriptor() -> Value {
    json!({
        "environmentId": "env-test",
        "label": "test",
        "platform": {"os": "darwin", "arch": "arm64"},
        "serverVersion": "0.1.0",
        "orchestrationProtocolVersion": 2,
        "capabilities": {"repositoryIdentity": false, "attachmentUploads": true},
    })
}

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

/// Serves the router on 127.0.0.1:<ephemeral> for the rest of the test.
pub async fn start(rpc: Arc<dyn RpcHandler>, attachments: Arc<AttachmentStore>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(HttpState {
        descriptor: descriptor(),
        rpc,
        attachments,
    });
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

pub struct Ws {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl Ws {
    pub async fn connect(addr: SocketAddr) -> Self {
        let url = format!("ws://{addr}/ws?clientSurface=mobile&orchestrationProtocol=2");
        let (socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        Self { socket }
    }

    pub async fn send_raw(&mut self, frame: &str) {
        self.socket.send(Message::text(frame)).await.unwrap();
    }

    pub async fn send(&mut self, message: Value) {
        self.send_raw(&message.to_string()).await;
    }

    pub async fn request(&mut self, id: Value, tag: &str, payload: Value) {
        self.send(json!({
            "_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": [],
        }))
        .await;
    }

    pub async fn ack(&mut self, id: Value) {
        self.send(json!({"_tag": "Ack", "requestId": id})).await;
    }

    /// Next text frame, parsed.
    pub async fn recv(&mut self) -> Value {
        self.try_recv(TIMEOUT)
            .await
            .expect("expected a frame from the server")
    }

    /// Next text frame within `wait`, or None.
    pub async fn try_recv(&mut self, wait: Duration) -> Option<Value> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let frame = tokio::time::timeout_at(deadline, self.socket.next())
                .await
                .ok()?;
            match frame.expect("socket closed").expect("socket error") {
                Message::Text(text) => return Some(serde_json::from_str(text.as_str()).unwrap()),
                Message::Close(_) => panic!("socket closed by server"),
                _ => {}
            }
        }
    }

    /// Asserts the server sends nothing for `millis`.
    pub async fn expect_silence(&mut self, millis: u64) {
        if let Some(frame) = self.try_recv(Duration::from_millis(millis)).await {
            panic!("expected no frame, got {frame}");
        }
    }

    pub async fn close(mut self) {
        self.socket.close(None).await.unwrap();
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
    tokio::time::timeout(TIMEOUT, stream.read_to_end(&mut raw))
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
