//! Terminals over the v2 WebSocket (wire.md §5.11) against the real `App`:
//! attach, write, resize, a second client with the history replayed, exit
//! and restart, close, and thread deletion. The shell is `/bin/sh`, so the
//! test does not depend on the user's shell configuration.

mod engine_support;
mod v2_support;

use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};

use engine_support::{Script, open};
use serde_json::{Value, json};
use tokio::time::Instant;
use v2_support::{Client, TempDir, WAIT, serve};

/// One socket with the items of its terminal streams so far.
struct Terminals {
    ws: Client,
    items: HashMap<u64, Vec<Value>>,
}

impl Terminals {
    async fn connect(addr: SocketAddr) -> Self {
        Self {
            ws: Client::connect(addr).await,
            items: HashMap::new(),
        }
    }

    /// Opens `terminal.attach`; its stream id.
    async fn attach(&mut self, params: Value) -> u64 {
        self.ws.watch("terminal.attach", params).await
    }

    /// Reads stream `id` until `done` holds for its items so far.
    async fn until(&mut self, id: u64, what: &str, done: impl Fn(&[Value]) -> bool) {
        let deadline = Instant::now() + WAIT;
        loop {
            let items = self.items.entry(id).or_default();
            if done(items) {
                return;
            }
            let Some(item) = self.ws.next_item(id, deadline).await else {
                panic!("timed out waiting for {what} on stream {id}; saw {items:#?}");
            };
            self.items.entry(id).or_default().push(item);
        }
    }

    /// The first item of stream `id`, which must be its snapshot.
    async fn snapshot(&mut self, id: u64) -> Value {
        self.until(id, "the snapshot", |items| !items.is_empty())
            .await;
        let first = &self.items[&id][0];
        assert_eq!(first["type"], "snapshot", "{first}");
        first.clone()
    }

    async fn has(&mut self, id: u64, kind: &str) -> Value {
        self.until(id, kind, |items| {
            items.iter().any(|item| item["type"] == kind)
        })
        .await;
        self.items[&id]
            .iter()
            .find(|item| item["type"] == kind)
            .unwrap()
            .clone()
    }

    async fn output_contains(&mut self, id: u64, needle: &str) {
        self.until(id, &format!("{needle:?}"), |items| {
            output(items).contains(needle)
        })
        .await;
    }

    /// Stream `id`'s output so far, history included.
    fn output(&self, id: u64) -> String {
        output(&self.items[&id])
    }
}

/// Snapshot history and output of `items`.
fn output(items: &[Value]) -> String {
    items
        .iter()
        .filter_map(|item| match item["type"].as_str() {
            Some("snapshot") => item["history"].as_str(),
            Some("output") => item["data"].as_str(),
            _ => None,
        })
        .collect()
}

/// Whether process `pid` ends within a few seconds.
async fn gone(pid: u32) -> bool {
    for _ in 0..100 {
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        if !alive {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test]
async fn terminal_attach_write_resize_reattach_exit_and_close() {
    // SAFETY: the only test in this binary, set before any shell starts.
    unsafe { std::env::set_var("SHELL", "/bin/sh") };
    let data_dir = TempDir::new("terminal-data");
    let project_dir = TempDir::new("terminal-project");
    let script = Arc::new(Script::default());
    let app = open(&data_dir.0, &script).await;
    let addr = serve(&app).await;

    let mut first = Terminals::connect(addr).await;
    let project = first
        .ws
        .ok(
            "projects.create",
            json!({"title": "Demo", "path": project_dir.0.to_string_lossy()}),
        )
        .await;
    let thread = first
        .ws
        .ok(
            "threads.create",
            json!({
                "projectId": project["project"]["id"],
                "model": {"provider": "opencode-go", "id": "glm-5.3-flash", "thinking": "high"},
                "title": "Shell",
                "text": "hello",
            }),
        )
        .await;
    let thread_id = thread["thread"]["id"].as_str().unwrap().to_owned();
    let term = |id: &str| json!({"threadId": thread_id, "terminalId": id});
    let with = |id: &str, extra: Value| {
        let mut params = term(id);
        params
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        params
    };

    // An unknown thread has no terminals.
    let unknown = first
        .attach(json!({"threadId": "nope", "terminalId": "term-1"}))
        .await;
    let ended = first.ws.ended(unknown).await;
    assert_eq!(ended["t"], "err", "{ended}");
    assert_eq!(ended["error"]["code"], "not_found", "{ended}");

    // Attaching starts the shell in the project folder.
    let one = first
        .attach(with("term-1", json!({"cols": 100, "rows": 30})))
        .await;
    let snapshot = first.snapshot(one).await;
    assert_eq!(snapshot["state"], "running", "{snapshot}");

    first
        .ws
        .ok(
            "terminal.write",
            with("term-1", json!({"data": "echo hi-$((40+2)) pid-$$; pwd\n"})),
        )
        .await;
    first.output_contains(one, "hi-42").await;
    let canonical = project_dir.0.canonicalize().unwrap();
    first
        .output_contains(one, canonical.file_name().unwrap().to_str().unwrap())
        .await;
    let pid: u32 = first
        .output(one)
        .split("hi-42 pid-")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|pid| pid.parse().ok())
        .expect("the shell's pid");

    // Resizing reaches the shell's terminal.
    first
        .ws
        .ok(
            "terminal.resize",
            with("term-1", json!({"cols": 120, "rows": 40})),
        )
        .await;
    first
        .ws
        .ok(
            "terminal.write",
            with("term-1", json!({"data": "stty size\n"})),
        )
        .await;
    first.output_contains(one, "40 120").await;

    // A second client gets the history, then live output like the first.
    let mut second = Terminals::connect(addr).await;
    let replayed = second.attach(term("term-1")).await;
    let replay = second.snapshot(replayed).await;
    let history = replay["history"].as_str().unwrap();
    assert!(history.contains("hi-42"), "{history:?}");
    assert!(history.contains("40 120"), "{history:?}");
    second
        .ws
        .ok(
            "terminal.write",
            with("term-1", json!({"data": "echo both-$((1+1))\n"})),
        )
        .await;
    second.output_contains(replayed, "both-2").await;
    first.output_contains(one, "both-2").await;

    // A shell that exits reports how; re-attaching shows it until restarted.
    let two = first
        .attach(with("term-2", json!({"cols": 80, "rows": 24})))
        .await;
    first.snapshot(two).await;
    first
        .ws
        .ok(
            "terminal.write",
            with("term-2", json!({"data": "exit 3\n"})),
        )
        .await;
    let exited = first.has(two, "exited").await;
    assert_eq!(exited, json!({"type": "exited", "exitCode": 3}));
    let late = second
        .ws
        .err(
            "terminal.write",
            with("term-2", json!({"data": "echo too late\n"})),
        )
        .await;
    assert_ne!(late["code"], "internal", "{late}");
    let exited_view = second.attach(term("term-2")).await;
    let snapshot_exited = second.snapshot(exited_view).await;
    assert_eq!(snapshot_exited["state"], "exited", "{snapshot_exited}");
    assert_eq!(snapshot_exited["exitCode"], 3);
    let restarted = second
        .attach(with("term-2", json!({"restart": true})))
        .await;
    let fresh = second.snapshot(restarted).await;
    assert_eq!(fresh["state"], "running", "{fresh}");
    // The other attached clients learn of the restart.
    let restart = first.has(two, "restarted").await;
    assert_eq!(restart["state"], "running", "{restart}");
    second.has(exited_view, "restarted").await;

    // Closing ends every attached stream with `closed`.
    first.ws.ok("terminal.close", term("term-1")).await;
    first.has(one, "closed").await;
    assert_eq!(first.ws.ended(one).await["t"], "end");
    second.has(replayed, "closed").await;
    assert_eq!(second.ws.ended(replayed).await["t"], "end");
    assert!(gone(pid).await, "the closed shell still runs");
    let error = first
        .ws
        .err(
            "terminal.write",
            with("term-1", json!({"data": "echo gone\n"})),
        )
        .await;
    assert_eq!(error["code"], "not_found", "{error}");
    // Closing an unknown terminal succeeds.
    first.ws.ok("terminal.close", term("term-9")).await;

    // Deleting the thread closes its remaining terminals.
    first
        .ws
        .ok("threads.delete", json!({"threadId": thread_id}))
        .await;
    second.has(restarted, "closed").await;
    first.has(two, "closed").await;
    app.shutdown().await;
}
