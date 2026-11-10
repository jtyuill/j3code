//! The v2 client (`j3_client::v2`) against a real in-process `j3_core`
//! server on an ephemeral port with a scripted omp session: connect and
//! shell, a streamed turn, the queue, requests, reconnect after a dropped
//! socket, attachments, and a terminal stream.

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    task::{Context, Poll, Wake, Waker},
    time::Duration,
};

use futures_util::future::BoxFuture;
use j3_client::{
    Connection, ConnectionState, Error, PROTOCOL, Status, ThreadSync, ThreadView, ThreadWatch,
    entities::{
        Answer, ItemKind, ItemStatus, ModelRef, RequestStatus, ThinkingLevel, ThreadStatus,
        TurnStatus,
    },
    envelope::ErrorCode,
    methods::{
        AnswerParams, AttachParams, CreateProjectParams, CreateThreadParams, Delivery,
        ListEntriesParams, QueueEntryParams, RenameThreadParams, SendParams, TerminalCloseParams,
        TerminalItem, TerminalState, TerminalWriteParams,
    },
};
use j3_core::{
    app::{App, AppConfig},
    engine::session::{AgentSession, Events, SessionFactory},
    omp::{
        self, OmpLauncher,
        omp_rpc::{
            self, AvailableSlashCommand, Event, ExtensionUiResponse, GoalOp, GoalResult,
            ImageContent, PromptAck, RpcNotification, SessionState, SubagentMessages,
            ThinkingLevel as OmpThinkingLevel,
        },
    },
    serve::Server,
    store::random_hex,
};
use serde_json::{Value, json};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{mpsc, watch},
    task::JoinHandle,
};

const WAIT: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------------------
// Scripted omp (from tests/client.rs)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Script {
    calls: Mutex<Vec<String>>,
    events: Mutex<Option<mpsc::UnboundedSender<Event>>>,
    ui_responses: Mutex<Vec<ExtensionUiResponse>>,
}

impl Script {
    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn emit(&self, value: Value) {
        let notification = RpcNotification::from_value(value).unwrap();
        self.events
            .lock()
            .unwrap()
            .as_ref()
            .expect("omp started")
            .send(Event::Notification(notification))
            .unwrap();
    }

    fn settle(&self, status: &str) {
        self.emit(json!({"type": "prompt_result", "agentInvoked": true, "status": status, "sessionSettled": true}));
    }

    fn text(&self, message_id: &str, event: Value) {
        self.emit(json!({"type": "message_update", "messageId": message_id,
            "message": {"role": "assistant"}, "assistantMessageEvent": event}));
    }

    /// omp starts an assistant text block.
    fn text_start(&self, message_id: &str) {
        self.text(message_id, json!({"type": "text_start", "contentIndex": 0}));
    }

    /// omp streams `delta` into the text block.
    fn say(&self, message_id: &str, delta: &str) {
        self.text(
            message_id,
            json!({"type": "text_delta", "contentIndex": 0, "delta": delta}),
        );
    }

    /// omp ends the text block with its whole `content`.
    fn text_end(&self, message_id: &str, content: &str) {
        self.text(
            message_id,
            json!({"type": "text_end", "contentIndex": 0, "content": content}),
        );
    }
}

struct FakeSession(Arc<Script>);

impl AgentSession for FakeSession {
    fn session_file(&self) -> &Path {
        Path::new("/tmp/j3-client-v2-fake-session.jsonl")
    }

    fn prompt(
        &self,
        message: String,
        images: Vec<ImageContent>,
    ) -> BoxFuture<'_, j3_core::engine::session::Result<PromptAck>> {
        self.0.record(format!("prompt:{message}:{}", images.len()));
        Box::pin(async {
            Ok(PromptAck {
                agent_invoked: Some(true),
            })
        })
    }

    fn steer(
        &self,
        message: String,
        _: Vec<ImageContent>,
    ) -> BoxFuture<'_, j3_core::engine::session::Result<()>> {
        self.0.record(format!("steer:{message}"));
        Box::pin(async { Ok(()) })
    }

    fn abort(&self) -> BoxFuture<'_, j3_core::engine::session::Result<()>> {
        self.0.record("abort".into());
        self.0.settle("aborted");
        Box::pin(async { Ok(()) })
    }

    fn set_model(
        &self,
        provider: String,
        model_id: String,
    ) -> BoxFuture<'_, j3_core::engine::session::Result<()>> {
        self.0.record(format!("set_model:{provider}/{model_id}"));
        Box::pin(async { Ok(()) })
    }

    fn set_thinking_level(
        &self,
        level: OmpThinkingLevel,
    ) -> BoxFuture<'_, j3_core::engine::session::Result<()>> {
        self.0
            .record(format!("thinking:{}", serde_json::to_value(level).unwrap()));
        Box::pin(async { Ok(()) })
    }

    fn get_state(&self) -> BoxFuture<'_, j3_core::engine::session::Result<SessionState>> {
        Box::pin(async {
            Ok(serde_json::from_value(json!({
                "sessionId": "fake",
                "model": {"id": "glm-5.3-flash", "name": "GLM", "api": "x", "provider": "opencode-go",
                  "baseUrl": "x", "reasoning": true, "input": ["text"],
                  "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
                  "contextWindow": null, "maxTokens": null},
                "thinkingLevel": "low",
            }))
            .unwrap())
        })
    }

    fn available_commands(
        &self,
    ) -> BoxFuture<'_, j3_core::engine::session::Result<Vec<AvailableSlashCommand>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn goal(
        &self,
        op: GoalOp,
        _objective: Option<String>,
        _token_budget: Option<i64>,
    ) -> BoxFuture<'_, j3_core::engine::session::Result<GoalResult>> {
        self.0.record(format!("goal:{}", op.as_str()));
        Box::pin(async {
            Err(omp::Error::Rpc(omp_rpc::Error::Command {
                command: "goal".into(),
                error: "Goals are not scripted.".into(),
                code: None,
            }))
        })
    }

    fn cancel_subagent(
        &self,
        subagent_id: String,
    ) -> BoxFuture<'_, j3_core::engine::session::Result<bool>> {
        self.0.record(format!("cancel_subagent:{subagent_id}"));
        Box::pin(async { Ok(false) })
    }

    fn subagent_messages(
        &self,
        subagent_id: String,
    ) -> BoxFuture<'_, j3_core::engine::session::Result<SubagentMessages>> {
        self.0.record(format!("subagent_messages:{subagent_id}"));
        Box::pin(async {
            Ok(
                serde_json::from_value(json!({"sessionFile": "/tmp/sub.jsonl", "fromByte": 0,
                "nextByte": 0, "reset": false, "entries": [], "messages": []}))
                .unwrap(),
            )
        })
    }

    fn btw(
        &self,
        question: String,
        _: Option<String>,
    ) -> BoxFuture<'_, j3_core::engine::session::Result<omp_rpc::BtwHistoryRecord>> {
        self.0.record(format!("btw:{question}"));
        Box::pin(async move {
            Ok(serde_json::from_value(
                json!({"id": "btw-1", "leafId": null, "question": question, "answer": "",
                    "status": "running", "createdAt": 1, "updatedAt": 1}),
            )
            .unwrap())
        })
    }

    fn btw_cancel(
        &self,
        record_id: String,
    ) -> BoxFuture<'_, j3_core::engine::session::Result<bool>> {
        self.0.record(format!("btw_cancel:{record_id}"));
        Box::pin(async { Ok(true) })
    }

    fn respond_ui(&self, response: ExtensionUiResponse) -> j3_core::engine::session::Result<()> {
        self.0.ui_responses.lock().unwrap().push(response);
        Ok(())
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, ()> {
        self.0.record("close".into());
        Box::pin(async {})
    }
}

struct FakeFactory(Arc<Script>);

impl SessionFactory for FakeFactory {
    fn start(
        &self,
        _cwd: PathBuf,
        _resume: Option<PathBuf>,
    ) -> BoxFuture<'static, j3_core::engine::session::Result<(Box<dyn AgentSession>, Events)>> {
        let script = Arc::clone(&self.0);
        Box::pin(async move {
            let (tx, rx) = mpsc::unbounded_channel();
            *script.events.lock().unwrap() = Some(tx);
            Ok((Box::new(FakeSession(script)) as Box<dyn AgentSession>, rx))
        })
    }
}

// ---------------------------------------------------------------------------
// Test server and a severable TCP proxy in front of it
// ---------------------------------------------------------------------------

struct TestServer {
    server: Option<Server>,
    script: Arc<Script>,
    addr: SocketAddr,
    data_dir: PathBuf,
    project_dir: PathBuf,
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("j3-client-v2-{name}-{}", random_hex(10)));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

impl TestServer {
    async fn start(name: &str) -> Self {
        let data_dir = temp_dir(name);
        let project_dir = temp_dir(&format!("{name}-project"));
        let script = Arc::new(Script::default());
        let app = App::open_with(
            AppConfig {
                data_dir: data_dir.clone(),
                label: "test-host".into(),
                // The model catalog fails fast instead of starting a real omp.
                omp: OmpLauncher::new(data_dir.join("no-omp")),
            },
            Arc::new(FakeFactory(Arc::clone(&script))),
            Duration::from_secs(600),
        )
        .await
        .unwrap();
        let any_port: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let server = Server::bind(app, &[any_port]).await.unwrap();
        let addr = server.addrs()[0];
        Self {
            server: Some(server),
            script,
            addr,
            data_dir,
            project_dir,
        }
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn project_dir(&self) -> String {
        self.project_dir.to_string_lossy().into_owned()
    }

    async fn stop(mut self) {
        if let Some(server) = self.server.take() {
            server.shutdown().await;
        }
        let _ = std::fs::remove_dir_all(&self.data_dir);
        let _ = std::fs::remove_dir_all(&self.project_dir);
    }
}

/// Forwards TCP to `target`; [`Proxy::sever`] cuts every open connection.
struct Proxy {
    addr: SocketAddr,
    connections: Arc<Mutex<Vec<JoinHandle<()>>>>,
    accept: JoinHandle<()>,
}

impl Proxy {
    async fn start(target: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connections: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::default();
        let tracked = Arc::clone(&connections);
        let accept = tokio::spawn(async move {
            while let Ok((mut inbound, _)) = listener.accept().await {
                let task = tokio::spawn(async move {
                    if let Ok(mut outbound) = TcpStream::connect(target).await {
                        let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                    }
                });
                tracked.lock().unwrap().push(task);
            }
        });
        Self {
            addr,
            connections,
            accept,
        }
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn sever(&self) {
        for task in self.connections.lock().unwrap().drain(..) {
            task.abort();
        }
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.accept.abort();
        self.sever();
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn wait_state<T>(
    rx: &mut watch::Receiver<Arc<T>>,
    what: &str,
    mut check: impl FnMut(&T) -> bool,
) -> Arc<T> {
    match tokio::time::timeout(WAIT, rx.wait_for(|state| check(state))).await {
        Ok(Ok(state)) => Arc::clone(&state),
        Ok(Err(_)) => panic!("{what}: state channel closed"),
        Err(_) => panic!("timed out waiting for {what}"),
    }
}

/// Connected with the shell caught up.
async fn synced(connection: &Connection) -> Arc<ConnectionState> {
    let mut state = connection.state();
    wait_state(&mut state, "a synced connection", |s| {
        s.status == Status::Connected && s.shell.synced
    })
    .await
}

async fn wait_calls(script: &Script, what: &str, check: impl Fn(&[String]) -> bool) {
    for _ in 0..1500 {
        if check(&script.calls()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}: {:?}", script.calls());
}

fn model() -> ModelRef {
    ModelRef {
        provider: "opencode-go".into(),
        id: "glm-5.3-flash".into(),
        thinking: Some(ThinkingLevel::High),
    }
}

/// Creates the server's project and returns its id.
async fn project(connection: &Connection, server: &TestServer) -> String {
    let created = connection
        .projects_create(CreateProjectParams {
            title: "Demo".into(),
            path: server.project_dir(),
            default_model: None,
            command_id: None,
        })
        .await
        .unwrap();
    assert_eq!(created.project.title, "Demo");
    created.project.id
}

/// Creates a thread, waits for omp's prompt and starts the agent; returns
/// the watch once its turn is running.
async fn create_running(
    connection: &Connection,
    server: &TestServer,
    project_id: &str,
    text: &str,
) -> (String, watch::Receiver<Arc<ThreadView>>, ThreadWatch) {
    let created = connection
        .threads_create(CreateThreadParams {
            project_id: project_id.to_owned(),
            model: model(),
            mode: None,
            title: Some(text.to_owned()),
            text: text.to_owned(),
            attachments: Vec::new(),
            command_id: None,
        })
        .await
        .unwrap();
    let thread_id = created.thread.id.clone();
    let watch = connection.thread(&thread_id);
    let mut state = watch.state();
    wait_calls(&server.script, "prompt", |calls| {
        calls.iter().any(|c| c == &format!("prompt:{text}:0"))
    })
    .await;
    server.script.emit(json!({"type": "agent_start"}));
    wait_state(&mut state, "a running turn", |s| {
        s.sync == ThreadSync::Synced
            && s.turns
                .last()
                .is_some_and(|turn| turn.status == TurnStatus::Running)
    })
    .await;
    (thread_id, state, watch)
}

fn assistant_text(view: &ThreadView) -> Vec<String> {
    view.items
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::Text(body) => Some(body.text.clone()),
            _ => None,
        })
        .collect()
}

/// A minimal executor with no tokio context, to show calls are
/// executor-agnostic.
fn block_on<F: Future>(future: F) -> F::Output {
    struct ThreadWaker(std::thread::Thread);
    impl Wake for ThreadWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::park(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn connects_and_tracks_projects() {
    let server = TestServer::start("connect").await;
    let connection = Connection::open(server.url(), tokio::runtime::Handle::current());
    let state = synced(&connection).await;
    let info = state.info.as_ref().expect("info");
    assert_eq!(info.protocol, PROTOCOL);
    assert!(!info.server_id.is_empty());
    assert!(
        info.capabilities.iter().any(|c| c == "terminal"),
        "{info:?}"
    );
    assert!(
        state.catalog.is_some(),
        "the catalog arrives with the handshake"
    );

    let created = connection
        .projects_create(CreateProjectParams {
            title: "Demo".into(),
            path: server.project_dir(),
            default_model: None,
            command_id: None,
        })
        .await
        .unwrap();
    let mut rx = connection.state();
    let shell = wait_state(&mut rx, "the project in the shell", |s| {
        s.shell.projects.contains_key(&created.project.id)
    })
    .await;
    assert_eq!(shell.shell.projects[&created.project.id].title, "Demo");
    assert!(shell.shell.sequence >= created.sequence);

    // Server errors keep their code and sentence.
    let error = connection
        .projects_create(CreateProjectParams {
            title: "Nowhere".into(),
            path: "/definitely/not/a/dir".into(),
            default_model: None,
            command_id: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Server(_)), "{error:?}");

    // Calls work from an executor without a tokio context.
    let listing = std::thread::spawn({
        let connection = connection.clone();
        let project_id = created.project.id.clone();
        move || {
            block_on(connection.projects_list_entries(ListEntriesParams {
                project_id,
                dir: None,
            }))
        }
    })
    .join()
    .unwrap()
    .unwrap();
    assert!(!listing.truncated);

    connection.close().await;
    assert_eq!(connection.state().borrow().status, Status::Closed);
    let closed = connection
        .projects_list_entries(ListEntriesParams {
            project_id: created.project.id,
            dir: None,
        })
        .await;
    assert_eq!(closed.unwrap_err(), Error::Disconnected);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn streams_a_turn_to_completion() {
    let server = TestServer::start("turn").await;
    let connection = Connection::open(server.url(), tokio::runtime::Handle::current());
    synced(&connection).await;
    let project_id = project(&connection, &server).await;
    let (thread_id, mut state, _watch) =
        create_running(&connection, &server, &project_id, "Say hi").await;

    let started = state.borrow().clone();
    assert!(matches!(&started.items[0].kind,
        ItemKind::User(user) if user.text == "Say hi"));
    assert_eq!(started.thread.as_ref().unwrap().id, thread_id);

    // Streamed text grows the item piece by piece.
    server.script.text_start("m1");
    server.script.say("m1", "Hel");
    wait_state(&mut state, "the first piece", |s| {
        assistant_text(s) == ["Hel"]
    })
    .await;
    server.script.say("m1", "lo");
    server.script.say("m1", "!");
    wait_state(&mut state, "the streamed text", |s| {
        assistant_text(s) == ["Hello!"]
    })
    .await;
    server.script.text_end("m1", "Hello!");

    server.script.settle("completed");
    let done = wait_state(&mut state, "the turn completed", |s| {
        s.turns
            .last()
            .is_some_and(|turn| turn.status == TurnStatus::Completed)
            && s.thread
                .as_ref()
                .is_some_and(|thread| thread.status == ThreadStatus::Idle)
    })
    .await;
    assert!(
        done.items
            .iter()
            .any(|item| matches!(&item.kind, ItemKind::Text(_))
                && item.status == ItemStatus::Completed),
        "{:?}",
        done.items
    );
    assert!(
        done.items
            .windows(2)
            .all(|pair| pair[0].ordinal < pair[1].ordinal),
        "items by ordinal"
    );
    assert_eq!(assistant_text(&done), ["Hello!"]);
    let mut shell = connection.state();
    wait_state(&mut shell, "the idle row in the shell", |s| {
        s.shell
            .threads
            .get(&thread_id)
            .is_some_and(|thread| thread.status == ThreadStatus::Idle)
    })
    .await;

    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn queues_and_cancels_a_prompt() {
    let server = TestServer::start("queue").await;
    let connection = Connection::open(server.url(), tokio::runtime::Handle::current());
    synced(&connection).await;
    let project_id = project(&connection, &server).await;
    let (thread_id, mut state, _watch) =
        create_running(&connection, &server, &project_id, "first").await;

    let sent = connection
        .turns_send(SendParams {
            thread_id: thread_id.clone(),
            text: "later".into(),
            attachments: Vec::new(),
            model: None,
            delivery: Delivery::Queue,
            command_id: None,
        })
        .await
        .unwrap();
    let entry_id = sent.queue_entry_id.expect("queued");
    let queued = wait_state(&mut state, "the queue entry", |s| s.queue.len() == 1).await;
    assert_eq!(queued.queue[0].id, entry_id);
    assert_eq!(queued.queue[0].text, "later");

    connection
        .queue_cancel(QueueEntryParams {
            thread_id: thread_id.clone(),
            entry_id,
            command_id: None,
        })
        .await
        .unwrap();
    wait_state(&mut state, "the queue empty", |s| s.queue.is_empty()).await;
    server.script.settle("completed");
    wait_state(&mut state, "the turn completed", |s| {
        s.turns
            .last()
            .is_some_and(|turn| turn.status == TurnStatus::Completed)
    })
    .await;
    assert!(
        !server
            .script
            .calls()
            .iter()
            .any(|c| c.starts_with("prompt:later")),
        "a cancelled entry never reaches omp"
    );

    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn answers_a_request() {
    let server = TestServer::start("request").await;
    let connection = Connection::open(server.url(), tokio::runtime::Handle::current());
    synced(&connection).await;
    let project_id = project(&connection, &server).await;
    let (thread_id, mut state, _watch) =
        create_running(&connection, &server, &project_id, "Ask me").await;

    server.script.emit(
        json!({"type": "extension_ui_request", "id": "ui-1", "method": "select",
        "title": "Pick one", "options": ["alpha", "beta"]}),
    );
    let asked = wait_state(&mut state, "an open request", |s| {
        s.requests.iter().any(|r| r.status == RequestStatus::Open)
    })
    .await;
    let request = asked
        .requests
        .iter()
        .find(|r| r.status == RequestStatus::Open)
        .unwrap()
        .clone();
    assert_eq!(request.questions.len(), 1);
    wait_state(&mut state, "the turn waiting", |s| {
        s.turns
            .last()
            .is_some_and(|turn| turn.status == TurnStatus::Waiting)
    })
    .await;

    connection
        .requests_answer(AnswerParams {
            thread_id: thread_id.clone(),
            request_id: request.id.clone(),
            answers: vec![Answer {
                question_id: request.questions[0].id.clone(),
                values: vec!["beta".into()],
                other: None,
            }],
            command_id: None,
        })
        .await
        .unwrap();
    let answered = wait_state(&mut state, "the request answered", |s| {
        s.requests
            .iter()
            .any(|r| r.id == request.id && r.status == RequestStatus::Answered)
    })
    .await;
    assert!(
        answered
            .requests
            .iter()
            .all(|r| r.status != RequestStatus::Open)
    );
    let responses = server.script.ui_responses.lock().unwrap().clone();
    assert_eq!(
        serde_json::to_value(&responses[0]).unwrap(),
        json!({"type": "extension_ui_response", "id": "ui-1", "value": "beta"})
    );

    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnects_and_catches_up() {
    let server = TestServer::start("reconnect").await;
    let proxy = Proxy::start(server.addr).await;
    let connection = Connection::open(proxy.url(), tokio::runtime::Handle::current());
    synced(&connection).await;
    let project_id = project(&connection, &server).await;
    let (thread_id, mut state, watch) =
        create_running(&connection, &server, &project_id, "keep going").await;

    // Record every published sequence to check none goes backwards.
    let mut seen_thread = state.clone();
    let thread_sequences = tokio::spawn(async move {
        let mut seen = vec![seen_thread.borrow_and_update().sequence];
        while seen_thread.changed().await.is_ok() {
            seen.push(seen_thread.borrow_and_update().sequence);
        }
        seen
    });
    let mut seen_shell = connection.state();
    let shell_sequences = tokio::spawn(async move {
        let mut seen = vec![seen_shell.borrow_and_update().shell.sequence];
        while seen_shell.changed().await.is_ok() {
            seen.push(seen_shell.borrow_and_update().shell.sequence);
        }
        seen
    });

    server.script.text_start("m1");
    server.script.say("m1", "before");
    wait_state(&mut state, "text before the drop", |s| {
        assistant_text(s) == ["before"]
    })
    .await;
    let before = state.borrow().sequence;

    // Drop the socket; the server goes on without the client, and another
    // client renames the thread meanwhile.
    proxy.sever();
    let mut status = connection.state();
    wait_state(&mut status, "reconnecting", |s| {
        matches!(s.status, Status::Reconnecting { .. })
    })
    .await;
    let other = Connection::open(server.url(), tokio::runtime::Handle::current());
    other
        .threads_rename(RenameThreadParams {
            thread_id: thread_id.clone(),
            title: "Survived".into(),
            command_id: None,
        })
        .await
        .unwrap();
    server.script.say("m1", " and after");
    server.script.text_end("m1", "before and after");
    server.script.settle("completed");

    // The client reconnects and catches up on both streams.
    let shell = wait_state(&mut status, "the rename in the shell", |s| {
        s.status == Status::Connected
            && s.shell.synced
            && s.shell
                .threads
                .get(&thread_id)
                .is_some_and(|thread| thread.title == "Survived")
    })
    .await;
    assert!(shell.shell.projects.contains_key(&project_id));
    let caught_up = wait_state(&mut state, "the thread caught up", |s| {
        s.sync == ThreadSync::Synced
            && s.thread
                .as_ref()
                .is_some_and(|thread| thread.title == "Survived")
            && s.turns
                .last()
                .is_some_and(|turn| turn.status == TurnStatus::Completed)
            && assistant_text(s) == ["before and after"]
    })
    .await;
    assert!(caught_up.sequence > before);

    other.close().await;
    connection.close().await;
    drop((state, watch, connection));
    let thread_sequences = thread_sequences.await.unwrap();
    let shell_sequences = shell_sequences.await.unwrap();
    assert!(
        thread_sequences.windows(2).all(|pair| pair[0] <= pair[1]),
        "thread sequence went backwards: {thread_sequences:?}"
    );
    assert!(
        shell_sequences.windows(2).all(|pair| pair[0] <= pair[1]),
        "shell sequence went backwards: {shell_sequences:?}"
    );
    drop(proxy);
    server.stop().await;
}

/// A 1x1 PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

#[tokio::test(flavor = "multi_thread")]
async fn uploads_and_sends_an_image() {
    let server = TestServer::start("upload").await;
    let connection = Connection::open(server.url(), tokio::runtime::Handle::current());
    synced(&connection).await;
    let project_id = project(&connection, &server).await;

    let attachment = connection
        .upload("dot.png", "image/png", PNG.to_vec())
        .await
        .unwrap();
    assert_eq!(attachment.name, "dot.png");
    assert_eq!(attachment.mime_type, "image/png");
    assert_eq!(attachment.size, PNG.len() as u64);
    let url = connection.attachment_url(&attachment.id);
    assert_eq!(
        url,
        format!("{}/api/v1/attachments/{}", server.url(), attachment.id)
    );
    let response = reqwest::get(&url).await.unwrap();
    assert!(response.status().is_success());
    assert_eq!(
        response.headers()["content-type"].to_str().unwrap(),
        "image/png"
    );
    assert_eq!(response.bytes().await.unwrap().as_ref(), PNG);
    assert_eq!(
        connection.url("/api/v1/assets/abc"),
        format!("{}/api/v1/assets/abc", server.url())
    );

    let (thread_id, mut state, _watch) =
        create_running(&connection, &server, &project_id, "first").await;
    server.script.settle("completed");
    wait_state(&mut state, "the first turn completed", |s| {
        s.turns
            .last()
            .is_some_and(|turn| turn.status == TurnStatus::Completed)
    })
    .await;

    let sent = connection
        .turns_send(SendParams {
            thread_id,
            text: "look".into(),
            attachments: vec![attachment.id.clone()],
            model: None,
            delivery: Delivery::Auto,
            command_id: None,
        })
        .await
        .unwrap();
    assert!(sent.turn_id.is_some(), "{sent:?}");
    wait_calls(&server.script, "the prompt with its image", |calls| {
        calls.iter().any(|c| c == "prompt:look:1")
    })
    .await;
    wait_state(&mut state, "the user item with the image", |s| {
        s.items.iter().any(|item| {
            matches!(&item.kind,
            ItemKind::User(user) if user.text == "look" && user.attachments == [attachment.clone()])
        })
    })
    .await;

    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn streams_a_terminal() {
    let server = TestServer::start("terminal").await;
    let connection = Connection::open(server.url(), tokio::runtime::Handle::current());
    synced(&connection).await;
    let project_id = project(&connection, &server).await;
    let (thread_id, _state, _watch) =
        create_running(&connection, &server, &project_id, "shell").await;

    let mut terminal = connection.attach_terminal(AttachParams {
        thread_id: thread_id.clone(),
        terminal_id: "term-1".into(),
        cols: Some(90),
        rows: Some(20),
        env: Default::default(),
        restart: None,
    });
    match terminal.next().await.expect("a snapshot").unwrap() {
        TerminalItem::Snapshot(snapshot) => assert_eq!(snapshot.state, TerminalState::Running),
        other => panic!("expected a snapshot, got {other:?}"),
    }
    connection
        .terminal_write(TerminalWriteParams {
            thread_id: thread_id.clone(),
            terminal_id: "term-1".into(),
            // Enough output for more than one window of frames.
            data: "for i in $(seq 1 40); do echo line-$i; done; echo hi-$((40+2))\n".into(),
        })
        .await
        .unwrap();
    let mut output = String::new();
    tokio::time::timeout(WAIT, async {
        while !output.contains("hi-42") {
            if let TerminalItem::Output { data } = terminal.next().await.expect("output").unwrap() {
                output.push_str(&data);
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no output; got {output:?}"));

    connection
        .terminal_close(TerminalCloseParams {
            thread_id,
            terminal_id: Some("term-1".into()),
            delete_history: None,
        })
        .await
        .unwrap();
    let closed = tokio::time::timeout(WAIT, async {
        let mut closed = false;
        while let Some(item) = terminal.next().await {
            closed |= matches!(item.unwrap(), TerminalItem::Closed);
        }
        closed
    })
    .await
    .unwrap();
    assert!(closed, "the stream ends with closed");

    // An unknown thread's terminal is not found.
    let error = connection
        .terminal_write(TerminalWriteParams {
            thread_id: "nope".into(),
            terminal_id: "term-9".into(),
            data: "x".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::NotFound), "{error:?}");

    server.stop().await;
}
