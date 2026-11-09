//! Protocol v2 triage end to end (`docs/triage.md` with the §12.1 decisions,
//! `docs/protocol/wire.md` §5.9): a real `App` with a scripted omp behind the
//! real HTTP router on a real socket, driven by a v2 WebSocket client. Every
//! command, its refusals and the state it leaves on the thread row, plus the
//! auto-settle sweep and recovery across a restart.

mod engine_support;
mod v2_support;

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use engine_support::{Script, open, temp_dir, wait_calls};
use j3_core::{
    app::App,
    clock::{iso, now_ms, parse_ms},
};
use serde_json::{Value, json};
use v2_support::{Client, Mirror, serve};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

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
        let data_dir = temp_dir(&format!("triage-{name}"));
        let project_dir = temp_dir(&format!("triage-{name}-project"));
        let script = Arc::new(Script::default());
        let app = open(&data_dir, &script).await;
        let addr = serve(&app).await;
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
        assert_eq!(result["project"]["path"], json!(path));
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

    async fn get_thread(&mut self, thread_id: &str) -> Value {
        self.ws
            .ok("threads.get", json!({"threadId": thread_id}))
            .await
    }

    async fn triage(&mut self, thread_id: &str) -> Value {
        self.get_thread(thread_id).await["thread"]["triage"].clone()
    }

    /// One accepted triage command; asserts its commit sequence moved forward.
    async fn step(&mut self, method: &str, params: Value, last: &mut u64) -> Value {
        let result = self.ws.ok(method, params).await;
        let sequence = result["sequence"]
            .as_u64()
            .unwrap_or_else(|| panic!("{method}: {result}"));
        assert!(
            sequence > *last,
            "{method} answered sequence {sequence}, not after {last}"
        );
        *last = sequence;
        result
    }

    /// Reports a completed turn and follows the stream until the turn reads
    /// completed and the thread idle.
    async fn complete_turn(
        &mut self,
        stream: u64,
        mirror: &mut Mirror,
        turn_id: &str,
        message_id: &str,
    ) {
        self.script.say(message_id, "done");
        self.script.settle("completed");
        self.ws
            .follow(stream, mirror, "the completed turn", |_, mirror| {
                mirror.turns.iter().any(|turn| {
                    turn["id"].as_str() == Some(turn_id) && turn["status"] == "completed"
                }) && mirror.thread["status"] == "idle"
            })
            .await;
    }
}

/// Opens `shell.watch` and returns its snapshot (after which `synced` was read).
async fn shell_snapshot(client: &mut Client) -> Value {
    let id = client.watch("shell.watch", json!({})).await;
    let snapshot = client.item(id).await;
    assert_eq!(snapshot["type"], "snapshot", "{snapshot}");
    let synced = client.item(id).await;
    assert_eq!(synced, json!({"type": "synced"}));
    snapshot
}

/// One thread row of a `shell.watch` snapshot.
fn shell_row<'a>(snapshot: &'a Value, thread_id: &str) -> &'a Value {
    snapshot["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"].as_str() == Some(thread_id))
        .unwrap_or_else(|| panic!("thread {thread_id} in the shell snapshot: {snapshot}"))
}

/// Shuts `app` down and opens it again on the same data directory, served on
/// a fresh socket with a fresh client.
async fn reopen(app: &Arc<App>, data_dir: &Path, script: &Arc<Script>) -> (Arc<App>, Client) {
    app.shutdown().await;
    let reopened = open(data_dir, script).await;
    let addr: SocketAddr = serve(&reopened).await;
    let ws = Client::connect(addr).await;
    (reopened, ws)
}

/// Every triage command on an idle thread with one completed turn: visits move
/// the watermark forward only, mark unread rewinds to just before the turn
/// ended, pin/order/unpin, active order, auto-settle opt-out, snooze/wake, and
/// settle/unsettle. Each accepted command gets its own later sequence, and the
/// state it leaves is read back from `threads.get`.
#[tokio::test]
async fn triage_commands_round_trip() {
    let mut env = Env::new("commands").await;
    let project_id = env.project().await;
    let thread_id = env.running_thread(&project_id, "work").await;
    let (stream, mut mirror) = env.watch_thread(&thread_id).await;
    let turn_id = mirror.turns[0]["id"].as_str().unwrap().to_owned();
    env.complete_turn(stream, &mut mirror, &turn_id, "answer-1")
        .await;

    let snapshot = env.get_thread(&thread_id).await;
    let mut last = snapshot["sequence"].as_u64().unwrap();
    let bumped = snapshot["thread"]["triage"]["bumpedAt"].clone();
    assert!(bumped.is_string(), "{snapshot}");

    // A visit moves the watermark forward only and never bumps.
    let later = "2100-01-01T00:00:00.000Z";
    env.step(
        "triage.visit",
        json!({"threadId": thread_id, "visitedAt": later}),
        &mut last,
    )
    .await;
    let earlier = "2099-01-01T00:00:00.000Z";
    env.step(
        "triage.visit",
        json!({"threadId": thread_id, "visitedAt": earlier}),
        &mut last,
    )
    .await;
    let triage = env.triage(&thread_id).await;
    assert_eq!(triage["visitedAt"], later, "{triage}");
    assert_eq!(triage["bumpedAt"], bumped, "a visit never bumps: {triage}");

    // Mark unread goes one millisecond before the latest turn ended.
    let ended_at = env.get_thread(&thread_id).await["turns"][0]["endedAt"].clone();
    let ended = parse_ms(ended_at.as_str().unwrap()).unwrap();
    env.step(
        "triage.markUnread",
        json!({"threadId": thread_id}),
        &mut last,
    )
    .await;
    let triage = env.triage(&thread_id).await;
    assert_eq!(triage["visitedAt"], json!(iso(ended - 1)), "{triage}");
    assert_eq!(
        triage["bumpedAt"], bumped,
        "mark unread never bumps: {triage}"
    );

    // A thread whose only turn is still running has nothing finished to
    // mark unread.
    let running = env.running_thread(&project_id, "still running").await;
    let error = env
        .ws
        .err("triage.markUnread", json!({"threadId": running}))
        .await;
    assert_eq!(error["code"], "rejected", "{error}");

    // Pin with a key, reorder the pin, then unpin.
    env.step(
        "triage.pin",
        json!({"threadId": thread_id, "order": "m"}),
        &mut last,
    )
    .await;
    let triage = env.triage(&thread_id).await;
    assert!(triage["pinnedAt"].is_string(), "{triage}");
    assert_eq!(triage["pinOrder"], "m", "{triage}");
    let pinned_at = triage["pinnedAt"].clone();

    env.step(
        "triage.orderPinned",
        json!({"threadId": thread_id, "order": "q"}),
        &mut last,
    )
    .await;
    let triage = env.triage(&thread_id).await;
    assert_eq!(triage["pinOrder"], "q", "{triage}");
    assert_eq!(triage["pinnedAt"], pinned_at, "a reorder keeps pinnedAt");

    env.step("triage.unpin", json!({"threadId": thread_id}), &mut last)
        .await;
    let triage = env.triage(&thread_id).await;
    assert!(triage.get("pinnedAt").is_none(), "{triage}");
    assert!(triage.get("pinOrder").is_none(), "{triage}");

    let error = env
        .ws
        .err(
            "triage.orderPinned",
            json!({"threadId": thread_id, "order": "q"}),
        )
        .await;
    assert_eq!(error["code"], "rejected", "{error}");
    assert_eq!(
        error["message"], "This thread is not pinned, so it can't be reordered.",
        "{error}"
    );

    // An unpinned, unsettled thread takes an active order key.
    env.step(
        "triage.orderActive",
        json!({"threadId": thread_id, "order": "c"}),
        &mut last,
    )
    .await;
    let triage = env.triage(&thread_id).await;
    assert_eq!(triage["activeOrder"], "c", "{triage}");

    // Auto-settle: off records when, on clears it.
    env.step(
        "triage.setAutoSettle",
        json!({"threadId": thread_id, "enabled": false}),
        &mut last,
    )
    .await;
    let triage = env.triage(&thread_id).await;
    assert!(triage["autoSettleOffAt"].is_string(), "{triage}");
    env.step(
        "triage.setAutoSettle",
        json!({"threadId": thread_id, "enabled": true}),
        &mut last,
    )
    .await;
    let triage = env.triage(&thread_id).await;
    assert!(triage.get("autoSettleOffAt").is_none(), "{triage}");

    // A past snooze is refused; a future one is stored in millisecond form.
    let error = env
        .ws
        .err(
            "triage.snooze",
            json!({"threadId": thread_id, "until": "2000-01-01T00:00:00.000Z"}),
        )
        .await;
    assert_eq!(error["code"], "rejected", "{error}");
    assert_eq!(
        error["message"], "The snooze time is not in the future.",
        "{error}"
    );

    let wake_time = "2100-06-01T12:34:56.000Z";
    env.step(
        "triage.snooze",
        json!({"threadId": thread_id, "until": "2100-06-01T12:34:56Z"}),
        &mut last,
    )
    .await;
    let triage = env.triage(&thread_id).await;
    assert_eq!(triage["snoozedUntil"], wake_time, "{triage}");
    assert!(triage["snoozedAt"].is_string(), "{triage}");

    env.step("triage.wake", json!({"threadId": thread_id}), &mut last)
        .await;
    let triage = env.triage(&thread_id).await;
    assert!(triage.get("snoozedUntil").is_none(), "{triage}");
    assert!(triage.get("snoozedAt").is_none(), "{triage}");
    assert!(triage["lastWakeAt"].is_string(), "{triage}");

    env.step("triage.settle", json!({"threadId": thread_id}), &mut last)
        .await;
    let triage = env.triage(&thread_id).await;
    assert_eq!(triage["settled"], "settled", "{triage}");
    assert!(triage["settledAt"].is_string(), "{triage}");

    env.step("triage.unsettle", json!({"threadId": thread_id}), &mut last)
        .await;
    let triage = env.triage(&thread_id).await;
    assert_eq!(triage["settled"], "active", "{triage}");
    assert!(triage.get("settledAt").is_none(), "{triage}");
    assert!(triage["unsettledAt"].is_string(), "{triage}");

    env.finish().await;
}

/// An archived thread refuses pin, settle and snooze, while visit and
/// mark unread still work (triage §6.1 and §4.3).
#[tokio::test]
async fn triage_refuses_archived_threads() {
    let mut env = Env::new("archived").await;
    let project_id = env.project().await;
    let thread_id = env.running_thread(&project_id, "archive me").await;
    let (stream, mut mirror) = env.watch_thread(&thread_id).await;
    let turn_id = mirror.turns[0]["id"].as_str().unwrap().to_owned();
    env.complete_turn(stream, &mut mirror, &turn_id, "answer-1")
        .await;
    let ended_at = mirror.turn(&turn_id)["endedAt"]
        .as_str()
        .unwrap()
        .to_owned();

    let archived = env
        .ws
        .ok("threads.archive", json!({"threadId": thread_id}))
        .await;
    assert!(archived["sequence"].as_u64().unwrap() > 0, "{archived}");
    let snapshot = env.get_thread(&thread_id).await;
    assert!(
        snapshot["thread"]["archivedAt"].is_string(),
        "the thread row shows the archive: {snapshot}"
    );

    for (method, params) in [
        ("triage.pin", json!({"threadId": thread_id})),
        ("triage.settle", json!({"threadId": thread_id})),
        (
            "triage.snooze",
            json!({"threadId": thread_id, "until": "2100-01-01T00:00:00.000Z"}),
        ),
    ] {
        let error = env.ws.err(method, params).await;
        assert_eq!(error["code"], "rejected", "{method}: {error}");
        assert_eq!(
            error["message"], "This thread is archived.",
            "{method}: {error}"
        );
    }

    let visited_at = "2100-01-01T00:00:00.000Z";
    let visited = env
        .ws
        .ok(
            "triage.visit",
            json!({"threadId": thread_id, "visitedAt": visited_at}),
        )
        .await;
    assert!(visited["sequence"].as_u64().unwrap() > 0, "{visited}");
    assert_eq!(
        env.triage(&thread_id).await["visitedAt"],
        visited_at,
        "a visit is allowed on an archived thread"
    );

    let marked = env
        .ws
        .ok("triage.markUnread", json!({"threadId": thread_id}))
        .await;
    assert!(
        marked["sequence"].as_u64().unwrap() > visited["sequence"].as_u64().unwrap(),
        "{marked}"
    );
    let ended = parse_ms(&ended_at).unwrap();
    assert_eq!(
        env.triage(&thread_id).await["visitedAt"],
        json!(iso(ended - 1)),
        "mark unread rewinds on an archived thread"
    );

    env.finish().await;
}

/// Snooze is refused while a question is open and while a message is queued,
/// and snoozing a settled thread un-settles it into Active (§12.1 decisions 1
/// and 5).
#[tokio::test]
async fn snooze_gate_and_settled_snooze() {
    let mut env = Env::new("snooze-gate").await;
    let project_id = env.project().await;
    let thread_id = env.running_thread(&project_id, "questions").await;
    let (stream, mut mirror) = env.watch_thread(&thread_id).await;
    let turn_id = mirror.turns[0]["id"].as_str().unwrap().to_owned();
    let until = "2100-06-01T12:34:56.000Z";

    // An open request blocks snoozing.
    env.script
        .emit(json!({"type": "extension_ui_request", "id": "ui-1",
        "method": "select", "title": "Pick", "options": ["a", "b"], "timeout": 60000}));
    env.ws
        .follow(stream, &mut mirror, "the open request", |_, mirror| {
            mirror.thread.get("pendingRequest").is_some()
                && mirror.turn(&turn_id)["status"] == "waiting"
        })
        .await;
    let snapshot = env.get_thread(&thread_id).await;
    let request_id = snapshot["thread"]["pendingRequest"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let question_id = snapshot["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|request| request["id"] == request_id.as_str())
        .unwrap_or_else(|| panic!("request {request_id} in {snapshot}"))["questions"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let error = env
        .ws
        .err(
            "triage.snooze",
            json!({"threadId": thread_id, "until": until}),
        )
        .await;
    assert_eq!(error["code"], "rejected", "{error}");

    env.ws
        .ok(
            "requests.answer",
            json!({"threadId": thread_id, "requestId": request_id,
                   "answers": [{"questionId": question_id, "values": ["a"]}]}),
        )
        .await;
    env.ws
        .follow(stream, &mut mirror, "running again", |_, mirror| {
            mirror.thread.get("pendingRequest").is_none()
                && mirror.turn(&turn_id)["status"] == "running"
        })
        .await;

    // A queued message blocks snoozing too.
    let queued = env
        .ws
        .ok(
            "turns.send",
            json!({"threadId": thread_id, "text": "later", "delivery": "queue"}),
        )
        .await;
    let entry_id = queued["queueEntryId"]
        .as_str()
        .unwrap_or_else(|| panic!("queued: {queued}"))
        .to_owned();
    env.ws
        .follow(stream, &mut mirror, "the queued message", |_, mirror| {
            mirror.queue.len() == 1 && mirror.queue[0]["id"] == entry_id.as_str()
        })
        .await;
    let error = env
        .ws
        .err(
            "triage.snooze",
            json!({"threadId": thread_id, "until": until}),
        )
        .await;
    assert_eq!(error["code"], "rejected", "{error}");
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("queued"),
        "{error}"
    );

    env.ws
        .ok(
            "queue.cancel",
            json!({"threadId": thread_id, "entryId": entry_id}),
        )
        .await;
    env.ws
        .follow(stream, &mut mirror, "empty queue", |_, mirror| {
            mirror.queue.is_empty()
        })
        .await;

    // Make the thread idle, settle it, then snooze it: the settle is cleared.
    env.ws
        .ok("turns.stop", json!({"threadId": thread_id}))
        .await;
    env.ws
        .follow(stream, &mut mirror, "the stopped turn", |_, mirror| {
            mirror.turn(&turn_id)["status"] == "stopped" && mirror.thread["status"] == "idle"
        })
        .await;

    let settled = env
        .ws
        .ok("triage.settle", json!({"threadId": thread_id}))
        .await;
    let sequence = settled["sequence"].as_u64().unwrap();
    let triage = env.triage(&thread_id).await;
    assert_eq!(triage["settled"], "settled", "{triage}");
    assert!(triage["settledAt"].is_string(), "{triage}");

    let snoozed = env
        .ws
        .ok(
            "triage.snooze",
            json!({"threadId": thread_id, "until": until}),
        )
        .await;
    assert!(
        snoozed["sequence"].as_u64().unwrap() > sequence,
        "{snoozed}"
    );
    let triage = env.triage(&thread_id).await;
    assert!(triage.get("settled").is_none(), "{triage}");
    assert!(triage.get("settledAt").is_none(), "{triage}");
    assert!(triage["unsettledAt"].is_string(), "{triage}");
    assert_eq!(triage["snoozedUntil"], until, "{triage}");

    env.finish().await;
}

/// The first message of a thread sets the read watermark just before the
/// turn's request time; a sent message clears an explicit settle (setting
/// `unsettledAt`) and a snooze (§6.2).
#[tokio::test]
async fn sending_a_message_unsettles_and_wakes() {
    let mut env = Env::new("send-unsettles").await;
    let project_id = env.project().await;
    let thread_id = env.running_thread(&project_id, "first").await;
    let (stream, mut mirror) = env.watch_thread(&thread_id).await;
    let turn_id = mirror.turns[0]["id"].as_str().unwrap().to_owned();

    let snapshot = env.get_thread(&thread_id).await;
    let triage = snapshot["thread"]["triage"].clone();
    let visited = parse_ms(triage["visitedAt"].as_str().unwrap()).unwrap();
    let requested = parse_ms(snapshot["turns"][0]["requestedAt"].as_str().unwrap()).unwrap();
    // Set at acceptance, one millisecond before "now" (triage §6.2).
    assert!(
        (visited - requested).abs() < 1000,
        "the first message sets the watermark when it is accepted: {triage}"
    );

    env.complete_turn(stream, &mut mirror, &turn_id, "answer-1")
        .await;
    let ended = parse_ms(mirror.turn(&turn_id)["endedAt"].as_str().unwrap()).unwrap();
    assert!(
        visited < ended,
        "the finished turn reads unread until the thread is visited"
    );
    env.ws
        .ok("triage.settle", json!({"threadId": thread_id}))
        .await;
    assert_eq!(env.triage(&thread_id).await["settled"], "settled");

    // A sent message clears the settle and sets `unsettledAt`.
    let sent = env
        .ws
        .ok(
            "turns.send",
            json!({"threadId": thread_id, "text": "again", "delivery": "auto"}),
        )
        .await;
    let second_turn = sent["turnId"]
        .as_str()
        .unwrap_or_else(|| panic!("a turn starts on an idle thread: {sent}"))
        .to_owned();
    let triage = env.triage(&thread_id).await;
    assert!(triage.get("settled").is_none(), "{triage}");
    assert!(triage["unsettledAt"].is_string(), "{triage}");
    assert!(triage.get("snoozedUntil").is_none(), "{triage}");

    env.complete_turn(stream, &mut mirror, &second_turn, "answer-2")
        .await;

    // A sent message also clears a snooze.
    let until = "2100-06-01T12:34:56.000Z";
    env.ws
        .ok(
            "triage.snooze",
            json!({"threadId": thread_id, "until": until}),
        )
        .await;
    assert_eq!(
        env.triage(&thread_id).await["snoozedUntil"],
        until,
        "the snooze is stored"
    );

    env.ws
        .ok(
            "turns.send",
            json!({"threadId": thread_id, "text": "wake", "delivery": "auto"}),
        )
        .await;
    let triage = env.triage(&thread_id).await;
    assert!(triage.get("snoozedUntil").is_none(), "{triage}");
    assert!(triage.get("snoozedAt").is_none(), "{triage}");
    assert!(triage.get("settled").is_none(), "{triage}");
    assert!(triage["unsettledAt"].is_string(), "{triage}");

    env.finish().await;
}

/// The auto-settle sweep settles only the quiet thread, at the time it went
/// quiet rather than the sweep's now, settles nothing on a second pass, and
/// every triage choice survives a restart in the list rows.
#[tokio::test]
async fn auto_settle_sweep_and_restart() {
    let mut env = Env::new("auto-settle").await;
    let project_id = env.project().await;

    let quiet = env.running_thread(&project_id, "quiet").await;
    let (quiet_stream, mut quiet_mirror) = env.watch_thread(&quiet).await;
    let quiet_turn = quiet_mirror.turns[0]["id"].as_str().unwrap().to_owned();
    env.complete_turn(quiet_stream, &mut quiet_mirror, &quiet_turn, "quiet-1")
        .await;
    let quiet_ended = quiet_mirror.turn(&quiet_turn)["endedAt"]
        .as_str()
        .unwrap()
        .to_owned();

    let pinned = env.running_thread(&project_id, "pinned").await;
    let (pinned_stream, mut pinned_mirror) = env.watch_thread(&pinned).await;
    let pinned_turn = pinned_mirror.turns[0]["id"].as_str().unwrap().to_owned();
    env.complete_turn(pinned_stream, &mut pinned_mirror, &pinned_turn, "pinned-1")
        .await;
    env.ws.ok("triage.pin", json!({"threadId": pinned})).await;

    let opted_out = env.running_thread(&project_id, "opted out").await;
    let (opted_stream, mut opted_mirror) = env.watch_thread(&opted_out).await;
    let opted_turn = opted_mirror.turns[0]["id"].as_str().unwrap().to_owned();
    env.complete_turn(opted_stream, &mut opted_mirror, &opted_turn, "opted-1")
        .await;
    env.ws
        .ok(
            "triage.setAutoSettle",
            json!({"threadId": opted_out, "enabled": false}),
        )
        .await;

    // Four days from now, nothing newer than three days is quiet.
    let now = now_ms() + 4 * DAY_MS;
    let after = Duration::from_secs(3 * 24 * 3600);
    assert_eq!(
        env.app.auto_settle(now, after).await,
        1,
        "only the quiet thread settles"
    );
    let triage = env.triage(&quiet).await;
    assert_eq!(triage["settled"], "settled", "{triage}");
    assert_eq!(
        triage["settledAt"], quiet_ended,
        "the sweep settles as of when the thread went quiet: {triage}"
    );
    assert_eq!(
        env.app.auto_settle(now, after).await,
        0,
        "a second sweep settles nothing new"
    );

    let (reopened, mut ws) = reopen(&env.app, &env.data_dir, &env.script).await;
    let snapshot = shell_snapshot(&mut ws).await;
    let triage = &shell_row(&snapshot, &quiet)["triage"];
    assert_eq!(triage["settled"], "settled", "{triage}");
    assert_eq!(triage["settledAt"], quiet_ended, "{triage}");
    assert!(shell_row(&snapshot, &pinned)["triage"]["pinnedAt"].is_string());
    assert!(
        shell_row(&snapshot, &opted_out)["triage"]["autoSettleOffAt"].is_string(),
        "the opt-out survives the restart"
    );

    ws.close().await;
    reopened.shutdown().await;
    env.finish().await;
}

/// A restart stops the active turn and expires the open request: the thread
/// comes back idle with no pending request.
#[tokio::test]
async fn recovery_on_reopen_stops_turns_and_expires_requests() {
    let mut env = Env::new("recovery").await;
    let project_id = env.project().await;
    let thread_id = env.running_thread(&project_id, "needs a hand").await;
    let (stream, mut mirror) = env.watch_thread(&thread_id).await;
    let turn_id = mirror.turns[0]["id"].as_str().unwrap().to_owned();

    env.script
        .emit(json!({"type": "extension_ui_request", "id": "ui-9",
        "method": "confirm", "title": "Deploy?", "message": "Ship it?",
        "timeout": 600000}));
    env.ws
        .follow(stream, &mut mirror, "the open request", |_, mirror| {
            mirror.thread.get("pendingRequest").is_some()
                && mirror.turn(&turn_id)["status"] == "waiting"
        })
        .await;
    let request_id = mirror.thread["pendingRequest"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let (reopened, mut ws) = reopen(&env.app, &env.data_dir, &env.script).await;
    let snapshot = ws.ok("threads.get", json!({"threadId": thread_id})).await;
    let thread = &snapshot["thread"];
    assert_eq!(thread["status"], "idle", "{snapshot}");
    assert!(thread.get("pendingRequest").is_none(), "{snapshot}");
    assert!(thread.get("activeTurnId").is_none(), "{snapshot}");

    let turn = snapshot["turns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|turn| turn["id"] == turn_id.as_str())
        .unwrap_or_else(|| panic!("turn {turn_id} in {snapshot}"));
    assert_eq!(turn["status"], "stopped", "{turn}");
    assert!(turn["endedAt"].is_string(), "{turn}");

    let request = snapshot["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|request| request["id"] == request_id.as_str())
        .unwrap_or_else(|| panic!("request {request_id} in {snapshot}"));
    assert_eq!(request["status"], "expired", "{request}");
    assert!(request["closedAt"].is_string(), "{request}");

    ws.close().await;
    reopened.shutdown().await;
    env.finish().await;
}

/// Repeating a `commandId` answers the stored result and acts once, for a
/// triage command and for a queued send.
#[tokio::test]
async fn repeated_command_ids_act_once() {
    let mut env = Env::new("command-ids").await;
    let project_id = env.project().await;
    let thread_id = env.running_thread(&project_id, "once").await;
    let (stream, mut mirror) = env.watch_thread(&thread_id).await;

    let pin = json!({"threadId": thread_id, "order": "m", "commandId": "pin-1"});
    let first = env.ws.ok("triage.pin", pin.clone()).await;
    let second = env.ws.ok("triage.pin", pin).await;
    assert_eq!(second, first, "a repeated pin answers the stored result");
    let triage = env.triage(&thread_id).await;
    assert_eq!(triage["pinOrder"], "m", "{triage}");
    assert!(triage["pinnedAt"].is_string(), "{triage}");

    let send = json!({"threadId": thread_id, "text": "later", "delivery": "queue",
        "commandId": "send-1"});
    let first = env.ws.ok("turns.send", send.clone()).await;
    let entry_id = first["queueEntryId"]
        .as_str()
        .unwrap_or_else(|| panic!("queued: {first}"))
        .to_owned();
    let second = env.ws.ok("turns.send", send).await;
    assert_eq!(second, first, "a repeated send answers the stored result");
    assert_eq!(second["queueEntryId"], json!(entry_id));

    env.ws
        .follow(
            stream,
            &mut mirror,
            "the one queued message",
            |_, mirror| mirror.queue.len() == 1 && mirror.queue[0]["id"] == entry_id.as_str(),
        )
        .await;
    let snapshot = env.get_thread(&thread_id).await;
    assert_eq!(
        snapshot["queue"].as_array().unwrap().len(),
        1,
        "the repeated send queued once: {snapshot}"
    );

    env.finish().await;
}
