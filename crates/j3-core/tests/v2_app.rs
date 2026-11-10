//! Protocol v2 end to end (`docs/protocol/wire.md`): a real `App` with a
//! scripted omp behind the real HTTP router on a real socket, driven by a v2
//! WebSocket client. Discovery, thread lifecycle and streaming, queue and
//! stop, requests, triage, and attachments, as a consumer sees them.

mod engine_support;
mod v2_support;

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use engine_support::{Script, btw_running, open, temp_dir, wait_calls};
use j3_core::app::App;
use j3_protocol::v2::entities::Attachment;
use serde_json::{Value, json};
use v2_support::{Client, Mirror, http, serve};

/// A 1×1 PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

fn model() -> Value {
    json!({"provider": "opencode-go", "id": "glm-5.3-flash", "thinking": "high"})
}

struct Env {
    app: Arc<App>,
    script: Arc<Script>,
    addr: SocketAddr,
    ws: Client,
    data_dir: PathBuf,
    project_dir: PathBuf,
}

impl Env {
    async fn new(name: &str) -> Self {
        let data_dir = temp_dir(&format!("v2-{name}"));
        let project_dir = temp_dir(&format!("v2-{name}-project"));
        let script = Arc::new(Script::default());
        let app = open(&data_dir, &script).await;
        let addr = serve(&app).await;
        let ws = Client::connect(addr).await;
        Self {
            app,
            script,
            addr,
            ws,
            data_dir,
            project_dir,
        }
    }

    async fn finish(self) {
        self.ws.close().await;
        self.app.shutdown().await;
        let _ = std::fs::remove_dir_all(&self.data_dir);
        let _ = std::fs::remove_dir_all(&self.project_dir);
    }

    /// `projects.create` on the project dir; the project id.
    async fn project(&mut self) -> String {
        let path = self.project_dir.to_string_lossy().into_owned();
        let result = self
            .ws
            .ok("projects.create", json!({"title": "Demo", "path": path}))
            .await;
        assert_eq!(result["project"]["title"], "Demo");
        assert_eq!(result["project"]["path"], json!(path));
        assert!(result["sequence"].as_u64().unwrap() > 0);
        result["project"]["id"].as_str().unwrap().to_owned()
    }

    /// `threads.create` with `text`; returns the thread id once omp has the
    /// prompt and reports the agent running.
    async fn running_thread(&mut self, project_id: &str, text: &str) -> String {
        let result = self
            .ws
            .ok(
                "threads.create",
                json!({"projectId": project_id, "model": model(), "text": text}),
            )
            .await;
        let thread_id = result["thread"]["id"].as_str().unwrap().to_owned();
        let prompt = format!("prompt:{text}:0");
        wait_calls(&self.script, "prompt", |calls| calls.contains(&prompt)).await;
        self.script.emit(json!({"type": "agent_start"}));
        thread_id
    }

    /// Opens `threads.watch`; checks it starts with `snapshot` then `synced`.
    async fn watch_thread(&mut self, thread_id: &str) -> (u64, Mirror) {
        let id = self
            .ws
            .watch("threads.watch", json!({"threadId": thread_id}))
            .await;
        let mut mirror = Mirror::default();
        let snapshot = self.ws.item(id).await;
        assert_eq!(snapshot["type"], "snapshot", "{snapshot}");
        mirror.apply(&snapshot);
        let synced = self.ws.item(id).await;
        assert_eq!(synced, json!({"type": "synced"}));
        (id, mirror)
    }

    /// Opens `shell.watch`; its snapshot (after which `synced` was read).
    async fn watch_shell(&mut self) -> (u64, Value) {
        let id = self.ws.watch("shell.watch", json!({})).await;
        let snapshot = self.ws.item(id).await;
        assert_eq!(snapshot["type"], "snapshot", "{snapshot}");
        let synced = self.ws.item(id).await;
        assert_eq!(synced, json!({"type": "synced"}));
        (id, snapshot)
    }
}

fn upgrade_headers() -> [(&'static str, &'static str); 4] {
    [
        ("connection", "Upgrade"),
        ("upgrade", "websocket"),
        ("sec-websocket-version", "13"),
        ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
    ]
}

fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

#[tokio::test]
async fn v2_discovery_and_call_errors() {
    let mut env = Env::new("discovery").await;

    let info = http(env.addr, "GET", "/api/v1/info", &[], b"").await;
    assert_eq!(info.status, 200);
    let info = info.json();
    assert_eq!(info["protocol"], 2);
    assert_eq!(info["serverId"], env.app.server_id());
    assert!(!info["serverId"].as_str().unwrap().is_empty());
    assert_eq!(info["name"], "test-host");
    assert!(info["capabilities"].is_array());

    for path in ["/api/v1/ws", "/api/v1/ws?protocol=1"] {
        let refused = http(env.addr, "GET", path, &upgrade_headers(), b"").await;
        assert_eq!(refused.status, 426, "{path}");
        let body = refused.json();
        assert_eq!(body["code"], "protocol_mismatch", "{path}");
        assert_eq!(body["protocol"], 2, "{path}");
        assert!(body["message"].is_string(), "{path}");
    }

    env.ws.send(json!({"t": "ping"})).await;
    let pong = env.ws.frame("pong", |frame| frame["t"] == "pong").await;
    assert_eq!(pong, json!({"t": "pong"}));

    let error = env.ws.err("threads.nope", json!({})).await;
    assert_eq!(error["code"], "unknown_method", "{error}");

    let error = env.ws.err("threads.get", json!({})).await;
    assert_eq!(error["code"], "bad_request", "{error}");
    assert!(
        error["message"].as_str().unwrap().contains("threadId"),
        "the message names the field: {error}"
    );
    let error = env
        .ws
        .err(
            "turns.send",
            json!({"threadId": "T", "text": "x", "delivery": "sideways"}),
        )
        .await;
    assert_eq!(error["code"], "bad_request", "{error}");

    // A missing thread is not_found.
    let error = env.ws.err("threads.get", json!({"threadId": "nope"})).await;
    assert_eq!(error["code"], "not_found", "{error}");

    env.finish().await;
}

/// wire.md §3.1: `not_found` has `data: {kind, id}`.
#[tokio::test]
async fn v2_not_found_names_the_entity() {
    let mut env = Env::new("not-found").await;
    let error = env.ws.err("threads.get", json!({"threadId": "nope"})).await;
    assert_eq!(error["code"], "not_found", "{error}");
    assert_eq!(
        error["data"],
        json!({"kind": "thread", "id": "nope"}),
        "{error}"
    );
    env.finish().await;
}

#[tokio::test]
async fn v2_thread_lifecycle_streams_text_and_completes() {
    let mut env = Env::new("lifecycle").await;
    let project_id = env.project().await;

    let (shell, snapshot) = env.watch_shell().await;
    let project = snapshot["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|project| project["id"] == project_id.as_str())
        .unwrap_or_else(|| panic!("project in shell snapshot: {snapshot}"));
    assert_eq!(project["path"], json!(path_str(&env.project_dir)));
    assert_eq!(snapshot["threads"], json!([]));

    let created = env
        .ws
        .ok(
            "threads.create",
            json!({"projectId": project_id, "model": model(), "text": "Say hi"}),
        )
        .await;
    let thread_id = created["thread"]["id"].as_str().unwrap().to_owned();
    assert_eq!(created["thread"]["projectId"], json!(project_id));
    assert_eq!(created["thread"]["model"]["provider"], "opencode-go");
    assert_eq!(created["thread"]["model"]["id"], "glm-5.3-flash");
    let created_sequence = created["sequence"].as_u64().unwrap();

    let row = env
        .ws
        .wait_item(shell, "shell thread", |item| {
            item["type"] == "thread" && item["thread"]["id"] == thread_id.as_str()
        })
        .await;
    assert_eq!(row["thread"]["projectId"], json!(project_id));

    wait_calls(&env.script, "prompt", |calls| {
        calls.contains(&"prompt:Say hi:0".to_owned())
    })
    .await;
    assert!(
        env.script.calls().contains(&"thinking:\"high\"".to_owned()),
        "{:?}",
        env.script.calls()
    );

    let (stream, mut mirror) = env.watch_thread(&thread_id).await;
    assert!(mirror.sequence >= created_sequence);
    assert_eq!(mirror.thread["id"], json!(thread_id));
    assert_eq!(mirror.turns.len(), 1, "{mirror:#?}");
    let turn_id = mirror.turns[0]["id"].as_str().unwrap().to_owned();
    assert_eq!(mirror.turns[0]["status"], "running");
    assert_eq!(mirror.turns[0]["ordinal"], 1);
    let users: Vec<Value> = mirror.items_of("user").into_iter().cloned().collect();
    assert_eq!(users.len(), 1, "{mirror:#?}");
    assert_eq!(users[0]["text"], "Say hi");
    assert_eq!(users[0]["delivery"], "prompt");
    assert_eq!(users[0]["turnId"], json!(turn_id));
    // wire.md §4.4 lists `attachments` as always present; the server omits
    // an empty list.
    assert!(
        users[0].get("attachments").is_none() || users[0]["attachments"] == json!([]),
        "{}",
        users[0]
    );

    env.script.emit(json!({"type": "agent_start"}));
    for event in [
        json!({"type": "text_start", "contentIndex": 0}),
        json!({"type": "text_delta", "contentIndex": 0, "delta": "Hi "}),
        json!({"type": "text_delta", "contentIndex": 0, "delta": "there!"}),
        json!({"type": "text_end", "contentIndex": 0, "content": "Hi there!"}),
    ] {
        env.script
            .emit(json!({"type": "message_update", "messageId": "m1",
            "message": {"role": "assistant"}, "assistantMessageEvent": event}));
    }
    env.ws
        .follow(stream, &mut mirror, "the whole answer", |_, mirror| {
            mirror
                .items_of("text")
                .iter()
                .any(|row| row["text"] == "Hi there!")
        })
        .await;
    env.script.settle("completed");
    env.ws
        .follow(stream, &mut mirror, "turn completed", |_, mirror| {
            mirror.turn(&turn_id)["status"] == "completed"
        })
        .await;
    let texts = mirror.items_of("text");
    assert_eq!(texts.len(), 1, "{mirror:#?}");
    assert_eq!(texts[0]["text"], "Hi there!");
    assert_eq!(texts[0]["turnId"], json!(turn_id));
    assert!(
        texts[0]["ordinal"].as_u64() > users[0]["ordinal"].as_u64(),
        "the answer follows the prompt"
    );
    assert!(mirror.turn(&turn_id)["endedAt"].is_string());

    // The stream settles on the same state threads.get reports.
    env.ws
        .follow(stream, &mut mirror, "idle thread row", |_, mirror| {
            mirror.thread["status"] == "idle" && mirror.thread.get("activeTurnId").is_none()
        })
        .await;
    env.ws.converge(stream, &mut mirror, &thread_id).await;

    env.finish().await;
}

#[tokio::test]
async fn v2_queue_cancel_and_stop() {
    let mut env = Env::new("queue").await;
    let project_id = env.project().await;
    let thread_id = env.running_thread(&project_id, "first").await;
    let (stream, mut mirror) = env.watch_thread(&thread_id).await;
    let turn_id = mirror.turns[0]["id"].as_str().unwrap().to_owned();

    let sent = env
        .ws
        .ok(
            "turns.send",
            json!({"threadId": thread_id, "text": "later", "delivery": "auto"}),
        )
        .await;
    let entry_id = sent["queueEntryId"]
        .as_str()
        .unwrap_or_else(|| panic!("queued: {sent}"))
        .to_owned();
    assert!(sent.get("turnId").is_none(), "{sent}");
    assert!(sent["sequence"].as_u64().unwrap() > 0);
    env.ws
        .follow(stream, &mut mirror, "queue entry", |item, _| {
            item["type"] == "queue"
                && item["queue"][0]["id"] == entry_id.as_str()
                && item["queue"][0]["text"] == "later"
        })
        .await;
    assert_eq!(mirror.queue.len(), 1);
    let entry = &mirror.queue[0];
    assert_eq!(entry["position"], 0);
    // wire.md §4.6 lists `held` as a boolean; the server omits it when false.
    assert!(
        entry.get("held").is_none() || entry["held"] == false,
        "{entry}"
    );
    assert_eq!(entry["threadId"], json!(thread_id));
    assert!(
        env.script
            .calls()
            .iter()
            .all(|call| !call.starts_with("steer:")),
        "auto delivery queues behind a running turn"
    );

    env.ws
        .ok(
            "queue.cancel",
            json!({"threadId": thread_id, "entryId": entry_id}),
        )
        .await;
    env.ws
        .follow(stream, &mut mirror, "empty queue", |item, _| {
            item["type"] == "queue" && item["queue"] == json!([])
        })
        .await;
    // A cancelled queue entry never becomes a turn.
    assert_eq!(mirror.turns.len(), 1, "{mirror:#?}");

    env.ws
        .ok("turns.stop", json!({"threadId": thread_id}))
        .await;
    env.ws
        .follow(stream, &mut mirror, "stopped turn", |_, mirror| {
            mirror.turn(&turn_id)["status"] == "stopped"
        })
        .await;
    assert!(env.script.calls().contains(&"abort".to_owned()));
    env.ws
        .follow(stream, &mut mirror, "idle thread row", |_, mirror| {
            mirror.thread["status"] == "idle"
        })
        .await;
    assert_eq!(
        mirror.items_of("stop").len(),
        1,
        "a user stop is a stop item: {mirror:#?}"
    );
    env.ws.converge(stream, &mut mirror, &thread_id).await;

    env.finish().await;
}

/// Every `queue` item shows each entry's prompt: a client renders whatever
/// item it last applied, so an entry must never flash an empty text.
#[tokio::test]
async fn v2_queue_entry_carries_its_text_from_the_first_item() {
    let mut env = Env::new("queue-text").await;
    let project_id = env.project().await;
    let thread_id = env.running_thread(&project_id, "first").await;
    let (stream, mut mirror) = env.watch_thread(&thread_id).await;
    let sent = env
        .ws
        .ok(
            "turns.send",
            json!({"threadId": thread_id, "text": "later", "delivery": "queue"}),
        )
        .await;
    let entry_id = sent["queueEntryId"].as_str().unwrap().to_owned();
    let first = env
        .ws
        .follow(stream, &mut mirror, "queue entry", |item, _| {
            item["type"] == "queue" && item["queue"][0]["id"] == entry_id.as_str()
        })
        .await;
    assert_eq!(first["queue"][0]["text"], "later", "first {first}");
    env.finish().await;
}

#[tokio::test]
async fn v2_ask_request_is_answered() {
    let mut env = Env::new("requests").await;
    let project_id = env.project().await;
    let thread_id = env.running_thread(&project_id, "ask me").await;
    let (stream, mut mirror) = env.watch_thread(&thread_id).await;
    let turn_id = mirror.turns[0]["id"].as_str().unwrap().to_owned();

    env.script.emit(
        json!({"type": "extension_ui_request", "id": "ui-7", "method": "ask",
        "questions": [
            {"id": "go", "question": "Proceed?", "options": [{"label": "Yes"}, {"label": "No"}]},
            {"id": "pick", "question": "Which ones?", "multi": true,
             "options": [{"label": "a"}, {"label": "b"}, {"label": "c"}]}
        ]}),
    );
    let item = env
        .ws
        .follow(stream, &mut mirror, "open request", |item, _| {
            item["type"] == "request" && item["request"]["status"] == "open"
        })
        .await;
    let request = &item["request"];
    let request_id = request["id"].as_str().unwrap().to_owned();
    assert_eq!(request["kind"], "ask");
    assert_eq!(request["threadId"], json!(thread_id));
    assert_eq!(request["turnId"], json!(turn_id));
    let questions = request["questions"].as_array().unwrap();
    assert_eq!(questions.len(), 2, "{request}");
    assert_eq!(questions[0]["id"], "go");
    assert_eq!(questions[0]["text"], "Proceed?");
    assert_eq!(
        questions[0]["options"],
        json!([{"label": "Yes"}, {"label": "No"}])
    );
    assert_eq!(questions[1]["id"], "pick");
    assert_eq!(questions[1]["text"], "Which ones?");
    assert_eq!(questions[1]["multiple"], true);
    assert!(request.get("answers").is_none());

    env.ws
        .follow(stream, &mut mirror, "waiting thread row", |_, mirror| {
            mirror.thread["pendingRequest"]["id"] == request_id.as_str()
                && mirror.turn(&turn_id)["status"] == "waiting"
        })
        .await;
    assert_eq!(mirror.thread["pendingRequest"]["kind"], "ask");
    assert_eq!(mirror.thread["status"], "waiting");

    env.ws
        .ok(
            "requests.answer",
            json!({"threadId": thread_id, "requestId": request_id, "answers": [
                {"questionId": "go", "values": ["Yes"]},
                {"questionId": "pick", "values": ["a", "c"]}
            ]}),
        )
        .await;
    let answered = env
        .ws
        .follow(stream, &mut mirror, "answered request", |item, _| {
            item["type"] == "request" && item["request"]["status"] == "answered"
        })
        .await;
    assert_eq!(answered["request"]["id"], json!(request_id));
    assert_eq!(
        answered["request"]["answers"],
        json!([
            {"questionId": "go", "values": ["Yes"]},
            {"questionId": "pick", "values": ["a", "c"]}
        ])
    );
    let responses = env.script.ui_responses.lock().unwrap().clone();
    assert_eq!(
        serde_json::to_value(&responses).unwrap(),
        json!([{"type": "extension_ui_response", "id": "ui-7", "answers": [
            {"id": "go", "selectedOptions": ["Yes"]},
            {"id": "pick", "selectedOptions": ["a", "c"]}
        ]}])
    );
    env.ws
        .follow(stream, &mut mirror, "running again", |_, mirror| {
            mirror.thread.get("pendingRequest").is_none()
                && mirror.turn(&turn_id)["status"] == "running"
        })
        .await;
    env.ws.converge(stream, &mut mirror, &thread_id).await;

    env.finish().await;
}

/// wire.md §5.6: a `confirm` is answered with the label of its `Yes` option.
#[tokio::test]
async fn v2_confirm_request_answered_yes() {
    let mut env = Env::new("confirm").await;
    let project_id = env.project().await;
    let thread_id = env.running_thread(&project_id, "confirm me").await;
    let (stream, mut mirror) = env.watch_thread(&thread_id).await;

    env.script.emit(
        json!({"type": "extension_ui_request", "id": "ui-c", "method": "confirm",
        "title": "Deploy?", "message": "Ship it to production?"}),
    );
    let item = env
        .ws
        .follow(stream, &mut mirror, "open confirm", |item, _| {
            item["type"] == "request" && item["request"]["status"] == "open"
        })
        .await;
    assert_eq!(item["request"]["kind"], "confirm", "{item}");
    let request = &item["request"];
    let question_id = request["questions"][0]["id"].as_str().unwrap();
    env.ws
        .ok(
            "requests.answer",
            json!({"threadId": thread_id, "requestId": request["id"],
                   "answers": [{"questionId": question_id, "values": ["Yes"]}]}),
        )
        .await;
    let responses = env.script.ui_responses.lock().unwrap().clone();
    assert_eq!(
        serde_json::to_value(&responses).unwrap(),
        json!([{"type": "extension_ui_response", "id": "ui-c", "confirmed": true}])
    );

    env.finish().await;
}

#[tokio::test]
async fn v2_triage_settle_refused_then_settles() {
    let mut env = Env::new("triage").await;
    let project_id = env.project().await;
    let thread_id = env.running_thread(&project_id, "work").await;
    let (stream, mut mirror) = env.watch_thread(&thread_id).await;
    let turn_id = mirror.turns[0]["id"].as_str().unwrap().to_owned();
    env.ws
        .follow(stream, &mut mirror, "running thread row", |_, mirror| {
            mirror.thread["status"] == "running"
        })
        .await;

    let error = env
        .ws
        .err("triage.settle", json!({"threadId": thread_id}))
        .await;
    assert_eq!(error["code"], "rejected", "{error}");
    assert_eq!(
        error["message"],
        "This thread is still running. Stop it before settling."
    );

    env.script.settle("completed");
    env.ws
        .follow(stream, &mut mirror, "turn completed", |_, mirror| {
            mirror.turn(&turn_id)["status"] == "completed" && mirror.thread["status"] == "idle"
        })
        .await;

    let (shell, snapshot) = env.watch_shell().await;
    let row = snapshot["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == thread_id.as_str())
        .unwrap();
    assert!(row["triage"].get("settled").is_none(), "{row}");

    let settled = env
        .ws
        .ok("triage.settle", json!({"threadId": thread_id}))
        .await;
    let sequence = settled["sequence"].as_u64().unwrap();
    let item = env
        .ws
        .wait_item(shell, "settled shell row", |item| {
            item["type"] == "thread"
                && item["thread"]["id"] == thread_id.as_str()
                && item["thread"]["triage"]["settled"] == "settled"
        })
        .await;
    assert!(item["sequence"].as_u64().unwrap() >= sequence, "{item}");
    assert!(item["thread"]["triage"]["settledAt"].is_string(), "{item}");
    env.ws
        .follow(stream, &mut mirror, "settled thread row", |_, mirror| {
            mirror.thread["triage"]["settled"] == "settled"
        })
        .await;

    env.finish().await;
}

#[tokio::test]
async fn v2_attachment_upload_download_and_send() {
    let mut env = Env::new("attachments").await;

    let size = PNG.len().to_string();
    let uploaded = http(
        env.addr,
        "POST",
        "/api/v1/attachments?name=a.png",
        &[("content-type", "image/png"), ("content-length", &size)],
        PNG,
    )
    .await;
    assert_eq!(
        uploaded.status,
        201,
        "{}",
        String::from_utf8_lossy(&uploaded.body)
    );
    let attachment: Attachment = serde_json::from_value(uploaded.json()).unwrap();
    assert_eq!(attachment.name, "a.png");
    assert_eq!(attachment.mime_type, "image/png");
    assert_eq!(attachment.size, PNG.len() as u64);

    let downloaded = http(
        env.addr,
        "GET",
        &format!("/api/v1/attachments/{}", attachment.id),
        &[],
        b"",
    )
    .await;
    assert_eq!(downloaded.status, 200);
    assert_eq!(downloaded.body, PNG);
    assert_eq!(downloaded.header("content-type"), Some("image/png"));

    // A missing name is refused.
    let refused = http(
        env.addr,
        "POST",
        "/api/v1/attachments",
        &[("content-type", "image/png"), ("content-length", &size)],
        PNG,
    )
    .await;
    assert_eq!(refused.status, 400);
    assert_eq!(refused.json()["code"], "bad_request");

    let project_id = env.project().await;
    let thread_id = env.running_thread(&project_id, "first").await;
    env.script.settle("completed");
    let (stream, mut mirror) = env.watch_thread(&thread_id).await;
    env.ws
        .follow(stream, &mut mirror, "idle", |_, mirror| {
            mirror.thread["status"] == "idle"
        })
        .await;

    let sent = env
        .ws
        .ok(
            "turns.send",
            json!({"threadId": thread_id, "text": "look", "attachments": [attachment.id],
                   "delivery": "auto"}),
        )
        .await;
    let turn_id = sent["turnId"]
        .as_str()
        .unwrap_or_else(|| panic!("an idle thread starts a turn: {sent}"))
        .to_owned();
    assert!(sent.get("queueEntryId").is_none(), "{sent}");
    wait_calls(&env.script, "prompt with an image", |calls| {
        calls.contains(&"prompt:look:1".to_owned())
    })
    .await;
    env.ws
        .follow(
            stream,
            &mut mirror,
            "user item with attachment",
            |_, mirror| {
                mirror
                    .items_of("user")
                    .iter()
                    .any(|row| row["text"] == "look" && row["turnId"] == turn_id.as_str())
            },
        )
        .await;
    let user = mirror
        .items_of("user")
        .into_iter()
        .find(|row| row["text"] == "look")
        .unwrap()
        .clone();
    assert_eq!(
        user["attachments"],
        json!([serde_json::to_value(&attachment).unwrap()])
    );

    env.finish().await;
}

#[tokio::test]
async fn v2_file_attachment_reaches_omp_as_a_mention() {
    let mut env = Env::new("file-attachment").await;

    let body = b"remember the milk";
    let size = body.len().to_string();
    let uploaded = http(
        env.addr,
        "POST",
        "/api/v1/attachments?name=my%20%22notes%22.txt",
        &[("content-type", "text/plain"), ("content-length", &size)],
        body,
    )
    .await;
    assert_eq!(uploaded.status, 201);
    let attachment: Attachment = serde_json::from_value(uploaded.json()).unwrap();

    let project_id = env.project().await;
    let created = env
        .ws
        .ok(
            "threads.create",
            json!({"projectId": project_id, "model": model(), "text": "summarize",
                   "attachments": [attachment.id]}),
        )
        .await;
    let thread_id = created["thread"]["id"].as_str().unwrap().to_owned();

    // omp gets the file as an `@"path"` mention after the text, and no image.
    wait_calls(&env.script, "prompt", |calls| {
        calls.iter().any(|call| call.starts_with("prompt:"))
    })
    .await;
    let calls = env.script.calls();
    let prompt = calls.iter().find(|c| c.starts_with("prompt:")).unwrap();
    let mention = prompt
        .strip_prefix("prompt:summarize\n\n@\"")
        .and_then(|rest| rest.strip_suffix("\":0"))
        .unwrap_or_else(|| panic!("no mention in {prompt:?}"));
    // The path ends in the upload's name (its quotes replaced: they would
    // end the mention) and holds the upload's bytes.
    assert!(mention.ends_with("/my _notes_.txt"), "{mention}");
    assert!(Path::new(mention).is_absolute(), "{mention}");
    assert_eq!(std::fs::read(mention).unwrap(), body);

    // The conversation keeps the text as typed, with the attachment beside it.
    let snapshot = env
        .ws
        .ok("threads.get", json!({"threadId": thread_id}))
        .await;
    let user = snapshot["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["kind"] == "user")
        .unwrap();
    assert_eq!(user["text"], "summarize");
    assert_eq!(user["attachments"][0]["name"], "my \"notes\".txt");

    env.ws
        .ok("attachments.delete", json!({"attachmentId": attachment.id}))
        .await;
    assert!(!Path::new(mention).exists());

    env.finish().await;
}

#[tokio::test]
async fn v2_btw_streams_the_answer_beside_the_turn() {
    let mut env = Env::new("btw").await;
    let project_id = env.project().await;
    let thread_id = env.running_thread(&project_id, "work").await;

    let refused = env
        .ws
        .err("btw.ask", json!({"threadId": thread_id, "question": "  "}))
        .await;
    assert_eq!(refused["code"], "rejected");
    let missing = env
        .ws
        .err("btw.ask", json!({"threadId": "nope", "question": "why?"}))
        .await;
    assert_eq!(missing["code"], "not_found");

    let ask = env
        .ws
        .watch(
            "btw.ask",
            json!({"threadId": thread_id, "question": " why? "}),
        )
        .await;
    let started = env.ws.item(ask).await;
    assert_eq!(
        started,
        json!({"type": "record", "record": {"id": "btw-1", "turns": [
            {"question": "why?", "answer": "", "status": "running"}]}})
    );
    assert!(env.script.calls().contains(&"btw:why?:".to_owned()));

    // Deltas of another topic are not this stream's.
    env.script
        .emit(json!({"type": "btw_delta", "recordId": "btw-0", "delta": "no"}));
    env.script
        .emit(json!({"type": "btw_delta", "recordId": "btw-1", "delta": "Because."}));
    assert_eq!(
        env.ws.item(ask).await,
        json!({"type": "delta", "recordId": "btw-1", "append": "Because."})
    );
    let mut done = btw_running("why?");
    done["answer"] = json!("Because.");
    done["status"] = json!("complete");
    env.script
        .emit(json!({"type": "btw_record", "record": done}));
    let finished = env.ws.item(ask).await;
    assert_eq!(finished["record"]["turns"][0]["status"], "complete");
    assert_eq!(finished["record"]["turns"][0]["answer"], "Because.");
    assert_eq!(env.ws.ended(ask).await["t"], "end");
    // Its stream is over, so nothing is left to cancel.
    assert!(
        !env.script
            .calls()
            .iter()
            .any(|c| c.starts_with("btw_cancel"))
    );

    // A follow-up names its topic; cancelling the call cancels it in omp.
    let follow_up = env
        .ws
        .watch(
            "btw.ask",
            json!({"threadId": thread_id, "question": "and then?", "recordId": "btw-1"}),
        )
        .await;
    env.ws.item(follow_up).await;
    assert!(
        env.script
            .calls()
            .contains(&"btw:and then?:btw-1".to_owned())
    );
    env.ws.send(json!({"t": "cancel", "id": follow_up})).await;
    assert_eq!(env.ws.ended(follow_up).await["error"]["code"], "cancelled");
    wait_calls(&env.script, "btw_cancel", |calls| {
        calls.contains(&"btw_cancel:btw-1".to_owned())
    })
    .await;

    // The turn was left alone throughout.
    let snapshot = env
        .ws
        .ok("threads.get", json!({"threadId": thread_id}))
        .await;
    assert_eq!(snapshot["thread"]["status"], "running");
    assert_eq!(snapshot["turns"].as_array().unwrap().len(), 1);

    env.finish().await;
}
