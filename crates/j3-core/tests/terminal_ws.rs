//! Terminal sessions over a real WebSocket against the real `App`: attach,
//! write, resize, a second client with the history replayed, exit and
//! restart, close, and thread deletion. The shell is `/bin/sh`, so the
//! test does not depend on the user's shell configuration.

mod transport_support;

use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};

use j3_core::{
    app::{App, AppConfig},
    engine::session::OmpFactory,
    omp::OmpLauncher,
};
use j3_protocol::t3::terminal::{
    TerminalAttachStreamEvent, TerminalSessionSnapshot, TerminalSessionStatus,
};
use serde_json::{Value, json};
use transport_support::{TempDir, Ws, start};

const WAIT: Duration = Duration::from_secs(20);

/// One socket, with every stream's events and every request's `Exit`.
struct Client {
    ws: Ws,
    events: HashMap<i64, Vec<TerminalAttachStreamEvent>>,
    exits: HashMap<i64, Value>,
}

impl Client {
    async fn connect(addr: SocketAddr) -> Self {
        Self {
            ws: Ws::connect(addr).await,
            events: HashMap::new(),
            exits: HashMap::new(),
        }
    }

    async fn request(&mut self, id: i64, tag: &str, payload: Value) {
        self.ws.request(json!(id), tag, payload).await;
    }

    /// Reads frames (acking chunks) until `done` holds.
    async fn until(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = tokio::time::Instant::now() + WAIT;
        while !done(self) {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let Some(frame) = self.ws.try_recv(left).await else {
                panic!(
                    "timed out waiting for {what}; events {:?}, exits {:?}",
                    self.events, self.exits
                );
            };
            let id = frame["requestId"].as_i64().unwrap_or(-1);
            match frame["_tag"].as_str() {
                Some("Chunk") => {
                    for value in frame["values"].as_array().unwrap() {
                        let event = serde_json::from_value(value.clone()).unwrap();
                        self.events.entry(id).or_default().push(event);
                    }
                    self.ws.ack(json!(id)).await;
                }
                Some("Exit") => {
                    self.exits.insert(id, frame["exit"].clone());
                }
                _ => {}
            }
        }
    }

    /// The unary answer of request `id`.
    async fn exit(&mut self, id: i64) -> Value {
        self.until(&format!("the exit of {id}"), |c| c.exits.contains_key(&id))
            .await;
        self.exits[&id].clone()
    }

    /// Snapshot history and output of stream `id` so far.
    fn output(&self, id: i64) -> String {
        let mut text = String::new();
        for event in self.events.get(&id).into_iter().flatten() {
            match event {
                TerminalAttachStreamEvent::Snapshot(snapshot) => {
                    text.push_str(&snapshot.snapshot.history)
                }
                TerminalAttachStreamEvent::Output(output) => text.push_str(&output.data),
                _ => {}
            }
        }
        text
    }

    async fn output_contains(&mut self, id: i64, needle: &str) {
        self.until(&format!("{needle:?} on stream {id}"), |c| {
            c.output(id).contains(needle)
        })
        .await;
    }

    fn snapshot(&self, id: i64) -> TerminalSessionSnapshot {
        match self.events.get(&id).and_then(|events| events.first()) {
            Some(TerminalAttachStreamEvent::Snapshot(snapshot)) => snapshot.snapshot.clone(),
            other => panic!("stream {id} did not start with a snapshot: {other:?}"),
        }
    }

    fn has(&self, id: i64, kind: &str) -> bool {
        self.events
            .get(&id)
            .is_some_and(|events| events.iter().any(|event| event.tag() == Some(kind)))
    }
}

fn success() -> Value {
    json!({"_tag": "Success", "value": null})
}

fn fail_tag(exit: &Value) -> &str {
    exit["cause"][0]["error"]["_tag"]
        .as_str()
        .unwrap_or_default()
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

async fn open_app(data_dir: &TempDir, project_dir: &TempDir) -> Arc<App> {
    let omp = OmpLauncher::new("/nonexistent/omp");
    let app = App::open_with(
        AppConfig {
            data_dir: data_dir.0.clone(),
            label: "test-host".into(),
            omp: omp.clone(),
        },
        Arc::new(OmpFactory(omp)),
        Duration::from_secs(600),
    )
    .await
    .unwrap();
    for (tag, payload) in [
        (
            "projects.mutate",
            json!({"type": "project.create", "commandId": "c-project", "projectId": "P1",
                   "title": "Demo", "workspaceRoot": project_dir.0.to_string_lossy()}),
        ),
        (
            "orchestration.launchThread",
            json!({"commandId": "c-launch", "threadId": "T1", "projectId": "P1",
                   "title": "Shell", "generateTitle": false,
                   "modelSelection": {"instanceId": "omp", "model": "a/b"},
                   "runtimeMode": "full-access", "interactionMode": "default",
                   "workspaceStrategy": {"type": "root"}}),
        ),
    ] {
        match j3_core::rpc::RpcHandler::call(&*app, tag.to_owned(), payload).await {
            j3_core::rpc::Response::Unary(j3_protocol::effect_rpc::Exit::Success { .. }) => {}
            _ => panic!("{tag} failed"),
        }
    }
    app
}

#[tokio::test]
async fn terminal_attach_write_resize_reattach_exit_and_close() {
    // SAFETY: the only test in this binary, set before any shell starts.
    unsafe { std::env::set_var("SHELL", "/bin/sh") };
    let data_dir = TempDir::new("terminal-data");
    let project_dir = TempDir::new("terminal-project");
    let app = open_app(&data_dir, &project_dir).await;
    let addr = start(app.clone(), app.attachments()).await;
    let term = |id: &str| json!({"threadId": "T1", "terminalId": id});

    // An unknown thread has no terminals.
    let mut first = Client::connect(addr).await;
    first
        .request(
            9,
            "terminal.attach",
            json!({"threadId": "nope", "terminalId": "term-1"}),
        )
        .await;
    assert_eq!(fail_tag(&first.exit(9).await), "TerminalSessionLookupError");

    // Attaching starts the shell in the project folder.
    first
        .request(
            1,
            "terminal.attach",
            json!({"threadId": "T1", "terminalId": "term-1", "cols": 100, "rows": 30}),
        )
        .await;
    first
        .until("the snapshot", |c| c.events.contains_key(&1))
        .await;
    let snapshot = first.snapshot(1);
    assert_eq!(snapshot.status, TerminalSessionStatus::Running);
    assert_eq!(snapshot.cwd, project_dir.0.to_string_lossy());
    assert_eq!(snapshot.label, "Terminal 1");
    assert!(snapshot.pid.is_some());

    let mut write = term("term-1");
    write["data"] = json!("echo hi-$((40+2)); pwd\n");
    first.request(2, "terminal.write", write).await;
    assert_eq!(first.exit(2).await, success());
    first.output_contains(1, "hi-42").await;
    let canonical = project_dir.0.canonicalize().unwrap();
    first
        .output_contains(1, canonical.file_name().unwrap().to_str().unwrap())
        .await;

    // Resizing reaches the shell's terminal.
    let mut resize = term("term-1");
    resize["cols"] = json!(120);
    resize["rows"] = json!(40);
    first.request(3, "terminal.resize", resize).await;
    assert_eq!(first.exit(3).await, success());
    let mut write = term("term-1");
    write["data"] = json!("stty size\n");
    first.request(4, "terminal.write", write).await;
    first.output_contains(1, "40 120").await;

    // A second client gets the history, then live output like the first.
    let mut second = Client::connect(addr).await;
    second.request(1, "terminal.attach", term("term-1")).await;
    second
        .until("the snapshot", |c| c.events.contains_key(&1))
        .await;
    let replay = second.snapshot(1);
    assert!(replay.history.contains("hi-42"), "{:?}", replay.history);
    assert!(replay.history.contains("40 120"));
    let mut write = term("term-1");
    write["data"] = json!("echo both-$((1+1))\n");
    second.request(2, "terminal.write", write).await;
    second.output_contains(1, "both-2").await;
    first.output_contains(1, "both-2").await;

    // A shell that exits reports how; re-attaching shows it until restarted.
    let mut attach = term("term-2");
    attach["cols"] = json!(80);
    attach["rows"] = json!(24);
    first.request(10, "terminal.attach", attach).await;
    let mut write = term("term-2");
    write["data"] = json!("exit 3\n");
    first.request(11, "terminal.write", write).await;
    first.until("exited", |c| c.has(10, "exited")).await;
    let exited = first.events[&10]
        .iter()
        .find_map(|event| match event {
            TerminalAttachStreamEvent::Exited(exited) => Some(exited.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!((exited.exit_code, exited.exit_signal), (Some(3), None));
    let mut write = term("term-2");
    write["data"] = json!("echo too late\n");
    second.request(3, "terminal.write", write).await;
    assert_eq!(fail_tag(&second.exit(3).await), "TerminalNotRunningError");
    second.request(4, "terminal.attach", term("term-2")).await;
    second
        .until("the snapshot", |c| c.events.contains_key(&4))
        .await;
    assert_eq!(second.snapshot(4).status, TerminalSessionStatus::Exited);
    assert_eq!(second.snapshot(4).exit_code, Some(3));
    let mut restart = term("term-2");
    restart["restartIfNotRunning"] = json!(true);
    second.request(5, "terminal.attach", restart).await;
    second
        .until("the snapshot", |c| c.events.contains_key(&5))
        .await;
    assert_eq!(second.snapshot(5).status, TerminalSessionStatus::Running);
    // The other attached clients learn of the restart.
    first.until("restarted", |c| c.has(10, "restarted")).await;
    second.until("restarted", |c| c.has(4, "restarted")).await;

    // Closing ends every attached stream with `closed`.
    first.request(5, "terminal.close", term("term-1")).await;
    assert_eq!(first.exit(5).await, success());
    first.until("closed", |c| c.has(1, "closed")).await;
    assert_eq!(first.exit(1).await, success());
    second.until("closed", |c| c.has(1, "closed")).await;
    assert_eq!(second.exit(1).await, success());
    assert!(
        gone(snapshot.pid.unwrap()).await,
        "the closed shell still runs"
    );
    let mut write = term("term-1");
    write["data"] = json!("echo gone\n");
    first.request(6, "terminal.write", write).await;
    assert_eq!(fail_tag(&first.exit(6).await), "TerminalSessionLookupError");

    // Deleting the thread closes its remaining terminals.
    match j3_core::rpc::RpcHandler::call(
        &*app,
        "orchestration.dispatchCommand".to_owned(),
        json!({"type": "thread.delete", "commandId": "c-delete", "threadId": "T1"}),
    )
    .await
    {
        j3_core::rpc::Response::Unary(j3_protocol::effect_rpc::Exit::Success { .. }) => {}
        _ => panic!("thread.delete failed"),
    }
    second.until("closed", |c| c.has(5, "closed")).await;
    first.until("closed", |c| c.has(10, "closed")).await;
    app.shutdown().await;
}
