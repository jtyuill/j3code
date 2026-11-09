//! Protocol v2 end to end (`docs/protocol/wire.md` §5.4–§5.7, §6): a real
//! `App` with a scripted omp behind the real HTTP router on a real socket,
//! driven by a v2 WebSocket client. Steering, restart, the queue, requests,
//! deletes and the thread/shell streams, as a consumer sees them.

mod engine_support;
mod v2_support;

use std::{cell::Cell, net::SocketAddr, path::PathBuf, sync::Arc};

use engine_support::{Script, open, temp_dir, wait_calls};
use j3_core::app::App;
use serde_json::{Value, json};
use v2_support::{Client, Mirror, serve};

fn model() -> Value {
    json!({"provider": "opencode-go", "id": "glm-5.3-flash", "thinking": "high"})
}

struct Env {
    app: Arc<App>,
    script: Arc<Script>,
    ws: Client,
    data_dir: PathBuf,
    project_dir: PathBuf,
}

impl Env {
    async fn new(name: &str) -> Self {
        let data_dir = temp_dir(&format!("v2-turns-{name}"));
        let project_dir = temp_dir(&format!("v2-turns-{name}-project"));
        let script = Arc::new(Script::default());
        let app = open(&data_dir, &script).await;
        let addr: SocketAddr = serve(&app).await;
        let ws = Client::connect(addr).await;
        Self {
            app,
            script,
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

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// `delivery: "steer"` on a running turn becomes a `steer` call to omp and a
/// `user` item with `delivery: "steer"` in that same turn; it is never queued.
#[tokio::test]
async fn steer_goes_into_the_running_turn() {
    let mut env = Env::new("steer").await;
    let project_id = env.project().await;
    let thread_id = env.running_thread(&project_id, "first").await;
    let (stream, mut mirror) = env.watch_thread(&thread_id).await;
    env.ws
        .follow(stream, &mut mirror, "the agent running", |_, mirror| {
            mirror.thread["status"] == "running"
        })
        .await;
    let turn_id = mirror.turns[0]["id"].as_str().unwrap().to_owned();

    let sent = env
        .ws
        .ok(
            "turns.send",
            json!({"threadId": thread_id, "text": "go left", "delivery": "steer"}),
        )
        .await;
    assert_eq!(sent["turnId"], json!(turn_id), "{sent}");
    assert!(sent.get("queueEntryId").is_none(), "{sent}");
    wait_calls(&env.script, "the steer", |calls| {
        calls.contains(&"steer:go left".to_owned())
    })
    .await;

    let item = env
        .ws
        .follow(stream, &mut mirror, "the steer item", |item, _| {
            item["type"] == "item"
                && item["item"]["kind"] == "user"
                && item["item"]["delivery"] == "steer"
        })
        .await;
    assert_eq!(item["item"]["text"], "go left");
    assert_eq!(item["item"]["turnId"], json!(turn_id));
    assert_eq!(mirror.queue, Vec::<Value>::new(), "{mirror:#?}");

    env.finish().await;
}

/// `delivery: "restart"` with a queued entry: the active turn is aborted and
/// marked `stopped` with a `stop` item, then the restart prompt starts the
/// next turn ahead of the older queued entry.
#[tokio::test]
async fn restart_stops_and_runs_the_new_message_first() {
    let mut env = Env::new("restart").await;
    let project_id = env.project().await;
    let thread_id = env.running_thread(&project_id, "first").await;
    let (stream, mut mirror) = env.watch_thread(&thread_id).await;
    env.ws
        .follow(stream, &mut mirror, "the agent running", |_, mirror| {
            mirror.thread["status"] == "running"
        })
        .await;
    let first_turn = mirror.turns[0]["id"].as_str().unwrap().to_owned();

    let queued = env
        .ws
        .ok(
            "turns.send",
            json!({"threadId": thread_id, "text": "older", "delivery": "queue"}),
        )
        .await;
    let older_entry = queued["queueEntryId"].as_str().unwrap().to_owned();
    env.ws
        .follow(stream, &mut mirror, "the queued entry", |item, _| {
            item["type"] == "queue"
                && item["queue"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry["id"] == older_entry.as_str())
        })
        .await;

    let sent = env
        .ws
        .ok(
            "turns.send",
            json!({"threadId": thread_id, "text": "restart now", "delivery": "restart"}),
        )
        .await;
    let restart_entry = sent["queueEntryId"].as_str().unwrap().to_owned();
    assert!(sent.get("turnId").is_none(), "{sent}");
    wait_calls(&env.script, "the abort", |calls| {
        calls.contains(&"abort".to_owned())
    })
    .await;

    env.ws
        .follow(stream, &mut mirror, "the stopped turn", |_, mirror| {
            mirror.turn(&first_turn)["status"] == "stopped"
        })
        .await;
    let stops = mirror.items_of("stop");
    assert_eq!(stops.len(), 1, "{mirror:#?}");
    assert_eq!(stops[0]["turnId"], json!(first_turn));

    wait_calls(&env.script, "the restart prompt", |calls| {
        calls.contains(&"prompt:restart now:0".to_owned())
    })
    .await;
    env.ws
        .follow(stream, &mut mirror, "the restart turn", |_, mirror| {
            mirror.turns.len() == 2
                && mirror.turns[1]["status"] == "running"
                && mirror.queue.len() == 1
                && mirror.queue[0]["id"] == older_entry.as_str()
        })
        .await;
    assert_eq!(mirror.queue[0]["id"], json!(older_entry));
    assert!(
        !mirror
            .queue
            .iter()
            .any(|entry| entry["id"] == restart_entry.as_str()),
        "the restart entry is no longer queued: {mirror:#?}"
    );
    assert!(
        !env.script
            .calls()
            .iter()
            .any(|call| call == "prompt:older:0"),
        "the older entry waits: {:?}",
        env.script.calls()
    );

    // Once the restart turn ends the older entry runs, proving the order.
    env.script.settle("completed");
    wait_calls(&env.script, "the older prompt", |calls| {
        calls.contains(&"prompt:older:0".to_owned())
    })
    .await;
    let calls = env.script.calls();
    let restart_at = calls
        .iter()
        .position(|call| call == "prompt:restart now:0")
        .unwrap();
    let older_at = calls
        .iter()
        .position(|call| call == "prompt:older:0")
        .unwrap();
    assert!(restart_at < older_at, "{calls:?}");

    env.finish().await;
}

/// `turns.stop {holdQueue: true}` holds every entry; the held entry waits
/// until `queue.resume`, which starts it as the next turn.
#[tokio::test]
async fn hold_queue_and_resume() {
    let mut env = Env::new("hold").await;
    let project_id = env.project().await;
    let thread_id = env.running_thread(&project_id, "first").await;
    let (stream, mut mirror) = env.watch_thread(&thread_id).await;
    env.ws
        .follow(stream, &mut mirror, "the agent running", |_, mirror| {
            mirror.thread["status"] == "running"
        })
        .await;
    let first_turn = mirror.turns[0]["id"].as_str().unwrap().to_owned();

    let queued = env
        .ws
        .ok(
            "turns.send",
            json!({"threadId": thread_id, "text": "later", "delivery": "queue"}),
        )
        .await;
    let entry_id = queued["queueEntryId"].as_str().unwrap().to_owned();

    env.ws
        .ok(
            "turns.stop",
            json!({"threadId": thread_id, "holdQueue": true}),
        )
        .await;
    env.ws
        .follow(stream, &mut mirror, "the held entry", |item, mirror| {
            item["type"] == "queue"
                && mirror
                    .queue
                    .iter()
                    .any(|entry| entry["id"] == entry_id.as_str() && entry["held"] == true)
        })
        .await;
    env.ws
        .follow(stream, &mut mirror, "the stopped turn", |_, mirror| {
            mirror.turn(&first_turn)["status"] == "stopped"
        })
        .await;
    assert!(
        !env.script
            .calls()
            .iter()
            .any(|call| call == "prompt:later:0"),
        "a held entry never starts: {:?}",
        env.script.calls()
    );

    env.ws
        .ok("queue.resume", json!({"threadId": thread_id}))
        .await;
    wait_calls(&env.script, "the resumed prompt", |calls| {
        calls.contains(&"prompt:later:0".to_owned())
    })
    .await;
    env.ws
        .follow(stream, &mut mirror, "the resumed turn", |_, mirror| {
            mirror.turns.len() == 2 && mirror.queue.is_empty()
        })
        .await;

    env.finish().await;
}

/// `queue.edit` rewrites a waiting entry, `queue.cancel` of an unknown entry
/// is a conflict, and `queue.start` aborts the active turn so the entry runs
/// next.
#[tokio::test]
async fn queue_start_stops_the_active_turn() {
    let mut env = Env::new("queue-start").await;
    let project_id = env.project().await;
    let thread_id = env.running_thread(&project_id, "first").await;
    let (stream, mut mirror) = env.watch_thread(&thread_id).await;
    env.ws
        .follow(stream, &mut mirror, "the agent running", |_, mirror| {
            mirror.thread["status"] == "running"
        })
        .await;
    let first_turn = mirror.turns[0]["id"].as_str().unwrap().to_owned();

    let queued = env
        .ws
        .ok(
            "turns.send",
            json!({"threadId": thread_id, "text": "later", "delivery": "queue"}),
        )
        .await;
    let entry_id = queued["queueEntryId"].as_str().unwrap().to_owned();

    env.ws
        .ok(
            "queue.edit",
            json!({"threadId": thread_id, "entryId": entry_id, "text": "edited"}),
        )
        .await;
    env.ws
        .follow(stream, &mut mirror, "the edited entry", |item, mirror| {
            item["type"] == "queue"
                && mirror
                    .queue
                    .iter()
                    .any(|entry| entry["id"] == entry_id.as_str() && entry["text"] == "edited")
        })
        .await;

    let error = env
        .ws
        .err(
            "queue.cancel",
            json!({"threadId": thread_id, "entryId": "queue_missing"}),
        )
        .await;
    assert_eq!(error["code"], "conflict", "{error}");

    env.ws
        .ok(
            "queue.start",
            json!({"threadId": thread_id, "entryId": entry_id}),
        )
        .await;
    wait_calls(&env.script, "the abort", |calls| {
        calls.contains(&"abort".to_owned())
    })
    .await;
    env.ws
        .follow(stream, &mut mirror, "the started turn", |_, mirror| {
            mirror.turns.len() == 2
                && mirror.turn(&first_turn)["status"] == "stopped"
                && mirror.queue.is_empty()
        })
        .await;
    wait_calls(&env.script, "the edited prompt", |calls| {
        calls.contains(&"prompt:edited:0".to_owned())
    })
    .await;
    assert_eq!(mirror.queue, Vec::<Value>::new(), "{mirror:#?}");

    env.finish().await;
}

/// `queue.steer` moves a waiting entry into the running turn as a steer; on
/// an idle thread the same call is rejected.
#[tokio::test]
async fn queue_steer_moves_an_entry_into_the_running_turn() {
    let mut env = Env::new("queue-steer").await;
    let project_id = env.project().await;
    let thread_id = env.running_thread(&project_id, "first").await;
    let (stream, mut mirror) = env.watch_thread(&thread_id).await;
    env.ws
        .follow(stream, &mut mirror, "the agent running", |_, mirror| {
            mirror.thread["status"] == "running"
        })
        .await;
    let first_turn = mirror.turns[0]["id"].as_str().unwrap().to_owned();

    let queued = env
        .ws
        .ok(
            "turns.send",
            json!({"threadId": thread_id, "text": "steer this", "delivery": "queue"}),
        )
        .await;
    let entry_id = queued["queueEntryId"].as_str().unwrap().to_owned();
    env.ws
        .follow(stream, &mut mirror, "the queued entry", |item, _| {
            item["type"] == "queue"
                && item["queue"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry["id"] == entry_id.as_str())
        })
        .await;

    env.ws
        .ok(
            "queue.steer",
            json!({"threadId": thread_id, "entryId": entry_id}),
        )
        .await;
    wait_calls(&env.script, "the steer", |calls| {
        calls.contains(&"steer:steer this".to_owned())
    })
    .await;
    env.ws
        .follow(stream, &mut mirror, "the steered item", |_, mirror| {
            mirror.queue.is_empty()
                && mirror
                    .items_of("user")
                    .iter()
                    .any(|row| row["delivery"] == "steer" && row["text"] == "steer this")
        })
        .await;
    let steered = mirror
        .items_of("user")
        .into_iter()
        .find(|row| row["delivery"] == "steer")
        .unwrap()
        .clone();
    assert_eq!(steered["text"], "steer this");
    assert_eq!(steered["turnId"], json!(first_turn));

    // Park a held entry, then idle: there is nothing to steer into.
    let queued = env
        .ws
        .ok(
            "turns.send",
            json!({"threadId": thread_id, "text": "held back", "delivery": "queue"}),
        )
        .await;
    let held_entry = queued["queueEntryId"].as_str().unwrap().to_owned();
    env.ws
        .ok(
            "turns.stop",
            json!({"threadId": thread_id, "holdQueue": true}),
        )
        .await;
    env.ws
        .follow(stream, &mut mirror, "the idle thread", |_, mirror| {
            mirror.thread["status"] == "idle"
                && mirror
                    .queue
                    .iter()
                    .any(|entry| entry["id"] == held_entry.as_str() && entry["held"] == true)
        })
        .await;
    let error = env
        .ws
        .err(
            "queue.steer",
            json!({"threadId": thread_id, "entryId": held_entry}),
        )
        .await;
    assert_eq!(error["code"], "rejected", "{error}");
    assert_eq!(error["message"], "Nothing is running to steer into.");

    env.finish().await;
}

/// `threads.delete` says `thread.removed` on the thread stream and then ends
/// it, tells the shell, makes `threads.get` not_found and is idempotent;
/// `onlyIfUnused` refuses a thread with turns.
#[tokio::test]
async fn delete_ends_streams() {
    let mut env = Env::new("delete").await;
    let project_id = env.project().await;
    let thread_id = env.running_thread(&project_id, "doomed").await;
    let (thread_stream, mut _mirror) = env.watch_thread(&thread_id).await;
    let (shell, _snapshot) = env.watch_shell().await;

    let error = env
        .ws
        .err(
            "threads.delete",
            json!({"threadId": thread_id, "onlyIfUnused": true}),
        )
        .await;
    assert_eq!(error["code"], "rejected", "{error}");

    env.ws
        .ok("threads.delete", json!({"threadId": thread_id}))
        .await;
    let removed = env
        .ws
        .wait_item(thread_stream, "thread.removed", |item| {
            item["type"] == "thread.removed"
        })
        .await;
    assert!(removed["sequence"].as_u64().unwrap() > 0, "{removed}");
    let end = env.ws.ended(thread_stream).await;
    assert_eq!(end["t"], "end", "{end}");
    assert_eq!(end["id"], json!(thread_stream));

    let removed = env
        .ws
        .wait_item(shell, "the shell thread.removed", |item| {
            item["type"] == "thread.removed" && item["threadId"] == thread_id.as_str()
        })
        .await;
    assert_eq!(removed["threadId"], json!(thread_id));

    let error = env
        .ws
        .err("threads.get", json!({"threadId": thread_id}))
        .await;
    assert_eq!(error["code"], "not_found", "{error}");

    env.ws
        .ok("threads.delete", json!({"threadId": thread_id}))
        .await;

    env.finish().await;
}

/// The shell row's `queued` counts the queue entries and its `status` says
/// whether a turn runs.
#[tokio::test]
async fn shell_rows_count_the_queue() {
    let mut env = Env::new("shell-queue").await;
    let project_id = env.project().await;
    let (shell, snapshot) = env.watch_shell().await;
    assert_eq!(snapshot["threads"], json!([]), "{snapshot}");
    let thread_id = env.running_thread(&project_id, "first").await;

    let row = env
        .ws
        .wait_item(shell, "the running row", |item| {
            item["type"] == "thread"
                && item["thread"]["id"] == thread_id.as_str()
                && item["thread"]["status"] == "running"
                && item["thread"]["queued"] == 0
        })
        .await;
    assert_eq!(row["thread"]["queued"], 0, "{row}");

    let first = env
        .ws
        .ok(
            "turns.send",
            json!({"threadId": thread_id, "text": "one", "delivery": "queue"}),
        )
        .await;
    let first_entry = first["queueEntryId"].as_str().unwrap().to_owned();
    env.ws
        .ok(
            "turns.send",
            json!({"threadId": thread_id, "text": "two", "delivery": "queue"}),
        )
        .await;
    let row = env
        .ws
        .wait_item(shell, "two queued", |item| {
            item["type"] == "thread"
                && item["thread"]["id"] == thread_id.as_str()
                && item["thread"]["queued"] == 2
        })
        .await;
    assert_eq!(row["thread"]["status"], "running", "{row}");

    env.ws
        .ok(
            "queue.cancel",
            json!({"threadId": thread_id, "entryId": first_entry}),
        )
        .await;
    let row = env
        .ws
        .wait_item(shell, "one queued", |item| {
            item["type"] == "thread"
                && item["thread"]["id"] == thread_id.as_str()
                && item["thread"]["queued"] == 1
        })
        .await;
    assert_eq!(row["thread"]["status"], "running", "{row}");

    // Hold the last entry so stopping the turn leaves the thread idle.
    env.ws
        .ok(
            "turns.stop",
            json!({"threadId": thread_id, "holdQueue": true}),
        )
        .await;
    let row = env
        .ws
        .wait_item(shell, "the idle row", |item| {
            item["type"] == "thread"
                && item["thread"]["id"] == thread_id.as_str()
                && item["thread"]["status"] == "idle"
                && item["thread"]["queued"] == 1
        })
        .await;
    assert_eq!(row["thread"]["queued"], 1, "{row}");

    env.finish().await;
}

/// Requests carry their kind and omp's expiry; `select` is answered with a
/// value response, `input` is dismissed with a cancel response, and a
/// closed request can no longer be answered.
#[tokio::test]
async fn requests_have_kinds_and_expiry() {
    let mut env = Env::new("requests").await;
    let project_id = env.project().await;
    let thread_id = env.running_thread(&project_id, "choose").await;
    let (stream, mut mirror) = env.watch_thread(&thread_id).await;
    env.ws
        .follow(stream, &mut mirror, "the agent running", |_, mirror| {
            mirror.thread["status"] == "running"
        })
        .await;

    env.script
        .emit(json!({"type": "extension_ui_request", "id": "ui-select",
        "method": "select", "title": "Pick", "options": ["a", "b"], "timeout": 60000}));
    let item = env
        .ws
        .follow(stream, &mut mirror, "the select request", |item, _| {
            item["type"] == "request" && item["request"]["status"] == "open"
        })
        .await;
    let request = item["request"].clone();
    assert_eq!(request["kind"], "select", "{request}");
    assert_eq!(
        request["questions"][0]["options"],
        json!([{"label": "a"}, {"label": "b"}])
    );
    let opened_at = request["openedAt"].as_str().expect("openedAt");
    let expires_at = request["expiresAt"].as_str().expect("expiresAt");
    let opened = j3_core::clock::parse_ms(opened_at).expect("openedAt parses");
    let expires = j3_core::clock::parse_ms(expires_at).expect("expiresAt parses");
    assert!(
        (expires - opened - 60_000).abs() <= 1_000,
        "expiresAt {expires_at} is not about 60 s after openedAt {opened_at}"
    );

    env.ws
        .follow(
            stream,
            &mut mirror,
            "the pending request row",
            |_, mirror| mirror.thread["pendingRequest"]["kind"] == "select",
        )
        .await;
    assert_eq!(mirror.thread["pendingRequest"]["id"], request["id"]);

    let question_id = request["questions"][0]["id"].as_str().unwrap().to_owned();
    env.ws
        .ok(
            "requests.answer",
            json!({"threadId": thread_id, "requestId": request["id"],
                   "answers": [{"questionId": question_id, "values": ["b"]}]}),
        )
        .await;
    env.ws
        .follow(stream, &mut mirror, "the answered select", |item, _| {
            item["type"] == "request"
                && item["request"]["id"] == request["id"]
                && item["request"]["status"] == "answered"
        })
        .await;
    let responses = env.script.ui_responses.lock().unwrap().clone();
    assert!(
        responses
            .iter()
            .any(|response| serde_json::to_value(response).unwrap()
                == json!({"type": "extension_ui_response", "id": "ui-select", "value": "b"})),
        "{responses:?}"
    );

    env.script
        .emit(json!({"type": "extension_ui_request", "id": "ui-input",
        "method": "input", "title": "Name"}));
    let item = env
        .ws
        .follow(stream, &mut mirror, "the input request", |item, _| {
            item["type"] == "request"
                && item["request"]["status"] == "open"
                && item["request"]["kind"] == "input"
        })
        .await;
    let input = item["request"].clone();
    env.ws
        .ok(
            "requests.dismiss",
            json!({"threadId": thread_id, "requestId": input["id"]}),
        )
        .await;
    let item = env
        .ws
        .follow(stream, &mut mirror, "the dismissed input", |item, _| {
            item["type"] == "request"
                && item["request"]["id"] == input["id"]
                && item["request"]["status"] == "dismissed"
        })
        .await;
    assert_eq!(item["request"]["status"], "dismissed");
    let responses = env.script.ui_responses.lock().unwrap().clone();
    assert!(
        responses
            .iter()
            .any(|response| serde_json::to_value(response).unwrap()
                == json!({"type": "extension_ui_response", "id": "ui-input", "cancelled": true})),
        "{responses:?}"
    );

    let error = env
        .ws
        .err(
            "requests.answer",
            json!({"threadId": thread_id, "requestId": input["id"],
                   "answers": [{"questionId": "answer", "values": ["late"]}]}),
        )
        .await;
    assert_eq!(error["code"], "conflict", "{error}");

    env.finish().await;
}

/// A `todo` tool result's phases keep omp's own statuses, including
/// `abandoned` and `blocked`.
#[tokio::test]
async fn todo_statuses_come_from_omp() {
    let mut env = Env::new("todo").await;
    let project_id = env.project().await;
    let thread_id = env.running_thread(&project_id, "plan").await;
    let (stream, mut mirror) = env.watch_thread(&thread_id).await;
    env.ws
        .follow(stream, &mut mirror, "the agent running", |_, mirror| {
            mirror.thread["status"] == "running"
        })
        .await;

    env.script.emit(json!({"type": "tool_execution_start",
        "toolCallId": "c1", "toolName": "todo", "args": {}}));
    env.script.emit(json!({"type": "tool_execution_end",
        "toolCallId": "c1", "toolName": "todo",
        "result": {"content": [{"type": "text", "text": "ok"}],
            "details": {"phases": [{"name": "P", "tasks": [
                {"content": "x", "status": "abandoned"},
                {"content": "y", "status": "blocked"}
            ]}]}},
        "isError": false}));
    env.ws
        .follow(stream, &mut mirror, "the finished todo", |_, mirror| {
            mirror
                .items_of("todo")
                .iter()
                .any(|item| item["status"] == "completed")
        })
        .await;
    let todos = mirror.items_of("todo");
    assert_eq!(todos.len(), 1, "{mirror:#?}");
    let tasks = &todos[0]["phases"][0]["tasks"];
    assert_eq!(tasks[0]["text"], "x");
    assert_eq!(tasks[0]["status"], "abandoned");
    assert_eq!(tasks[1]["text"], "y");
    assert_eq!(tasks[1]["status"], "blocked");

    env.finish().await;
}

/// A subagent item keeps the agent that runs it: started is running and
/// completed is completed.
#[tokio::test]
async fn subagents_carry_their_agent() {
    let mut env = Env::new("subagent").await;
    let project_id = env.project().await;
    let thread_id = env.running_thread(&project_id, "scout").await;
    let (stream, mut mirror) = env.watch_thread(&thread_id).await;
    env.ws
        .follow(stream, &mut mirror, "the agent running", |_, mirror| {
            mirror.thread["status"] == "running"
        })
        .await;

    env.script
        .emit(json!({"type": "subagent_lifecycle", "payload": {
        "id": "Scout", "agent": "scout", "agentSource": "bundled",
        "status": "started", "index": 0}}));
    let item = env
        .ws
        .follow(stream, &mut mirror, "the running subagent", |item, _| {
            item["type"] == "item"
                && item["item"]["kind"] == "subagent"
                && item["item"]["status"] == "running"
        })
        .await;
    assert_eq!(item["item"]["subagentId"], "Scout");
    assert_eq!(item["item"]["agent"], "scout");

    env.script
        .emit(json!({"type": "subagent_lifecycle", "payload": {
        "id": "Scout", "agent": "scout", "agentSource": "bundled",
        "status": "completed", "index": 0}}));
    let item = env
        .ws
        .follow(stream, &mut mirror, "the completed subagent", |item, _| {
            item["type"] == "item"
                && item["item"]["kind"] == "subagent"
                && item["item"]["status"] == "completed"
        })
        .await;
    assert_eq!(item["item"]["agent"], "scout");
    assert_eq!(item["item"]["subagentId"], "Scout");

    env.finish().await;
}

/// Assistant text grows through `item.delta` frames with `field: "text"`,
/// and the final text is the whole answer once `text_end` arrives.
#[tokio::test]
async fn text_streams_as_deltas() {
    let mut env = Env::new("deltas").await;
    let project_id = env.project().await;
    let thread_id = env.running_thread(&project_id, "talk").await;
    let (stream, mut mirror) = env.watch_thread(&thread_id).await;
    env.ws
        .follow(stream, &mut mirror, "the agent running", |_, mirror| {
            mirror.thread["status"] == "running"
        })
        .await;

    for event in [
        json!({"type": "text_start", "contentIndex": 0}),
        json!({"type": "text_delta", "contentIndex": 0, "delta": "Hello "}),
        json!({"type": "text_delta", "contentIndex": 0, "delta": "world"}),
    ] {
        env.script
            .emit(json!({"type": "message_update", "messageId": "m1",
            "message": {"role": "assistant"}, "assistantMessageEvent": event}));
    }
    let saw_delta = Cell::new(false);
    env.ws
        .follow(
            stream,
            &mut mirror,
            "the streamed deltas",
            |item, mirror| {
                if item["type"] == "item.delta" && item["field"] == "text" {
                    saw_delta.set(true);
                }
                saw_delta.get()
                    && mirror
                        .items_of("text")
                        .iter()
                        .any(|row| row["text"] == "Hello world")
            },
        )
        .await;
    assert!(saw_delta.get(), "no text delta arrived");
    let texts = mirror.items_of("text");
    assert_eq!(texts.len(), 1, "{mirror:#?}");
    assert_eq!(texts[0]["text"], "Hello world");

    env.script
        .emit(json!({"type": "message_update", "messageId": "m1",
        "message": {"role": "assistant"},
        "assistantMessageEvent": {"type": "text_end", "contentIndex": 0,
            "content": "Hello world"}}));
    env.ws
        .follow(stream, &mut mirror, "the finished text", |_, mirror| {
            mirror
                .items_of("text")
                .iter()
                .any(|row| row["status"] == "completed")
        })
        .await;
    assert_eq!(mirror.items_of("text")[0]["text"], "Hello world");

    env.finish().await;
}
