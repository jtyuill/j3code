//! The FFI surface (`j3_mobile::Host`) against a real in-process `j3_core`
//! server with a scripted omp session, called as Swift would: listeners
//! receive snapshots on the library runtime and commands are awaited from
//! another runtime.

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures_util::future::BoxFuture;
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
use j3_mobile::{
    Answer, BtwListener, BtwRecord, BtwStatus, ClientError, ConnectionStatus, Host, HostListener,
    HostSnapshot, ItemKind, ItemStatus, ModelRef, RequestStatus, Subscription, ThinkingLevel,
    ThreadListener, ThreadSnapshot, ThreadStatus, ThreadSync, TurnStatus,
};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

const WAIT: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------------------
// Scripted omp (from j3-client's tests/v2_client.rs)
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

/// omp's record of the side question `question` (topic `btw-1`).
fn aside(question: &str, answer: &str, status: &str) -> Value {
    json!({"id": "btw-1", "leafId": null, "question": question, "answer": answer,
        "status": status, "createdAt": 1, "updatedAt": 1})
}

struct FakeSession(Arc<Script>);

impl AgentSession for FakeSession {
    fn session_file(&self) -> &Path {
        Path::new("/tmp/j3-mobile-fake-session.jsonl")
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
        Box::pin(
            async move { Ok(serde_json::from_value(aside(&question, "", "running")).unwrap()) },
        )
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
// Test server
// ---------------------------------------------------------------------------

struct TestServer {
    server: Option<Server>,
    script: Arc<Script>,
    addr: SocketAddr,
    data_dir: PathBuf,
    project_dir: PathBuf,
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("j3-mobile-{name}-{}", random_hex(10)));
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

// ---------------------------------------------------------------------------
// Listeners standing in for Swift
// ---------------------------------------------------------------------------

/// Keeps the last snapshot a listener received and counts the calls.
struct Recorder<T> {
    last: watch::Sender<Option<T>>,
    calls: AtomicUsize,
    /// The library runtime's worker the calls arrive on.
    threads: Mutex<Vec<String>>,
}

impl<T> Recorder<T> {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            last: watch::Sender::new(None),
            calls: AtomicUsize::new(0),
            threads: Mutex::new(Vec::new()),
        })
    }

    fn receive(&self, snapshot: T) {
        let thread = std::thread::current().name().unwrap_or_default().to_owned();
        self.threads.lock().unwrap().push(thread);
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.last.send_replace(Some(snapshot));
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// The first snapshot (the current one included) that passes `check`.
    async fn wait(&self, what: &str, check: impl Fn(&T) -> bool) -> T
    where
        T: Clone,
    {
        let mut rx = self.last.subscribe();
        match tokio::time::timeout(WAIT, rx.wait_for(|last| last.as_ref().is_some_and(&check)))
            .await
        {
            Ok(Ok(last)) => last.clone().unwrap(),
            Ok(Err(_)) => panic!("{what}: recorder closed"),
            Err(_) => panic!("timed out waiting for {what}"),
        }
    }
}

impl HostListener for Recorder<HostSnapshot> {
    fn host_changed(&self, snapshot: HostSnapshot) {
        self.receive(snapshot);
    }
}

impl ThreadListener for Recorder<ThreadSnapshot> {
    fn thread_changed(&self, snapshot: ThreadSnapshot) {
        self.receive(snapshot);
    }
}

/// A side question's topic as last received, and how it ended.
#[derive(Clone, Default, PartialEq, Debug)]
struct Aside {
    topic: Option<BtwRecord>,
    ended: Option<Option<String>>,
}

impl BtwListener for Recorder<Aside> {
    fn btw_changed(&self, topic: BtwRecord) {
        self.receive(Aside {
            topic: Some(topic),
            ended: None,
        });
    }

    fn btw_ended(&self, error: Option<String>) {
        let topic = self.last.borrow().clone().and_then(|aside| aside.topic);
        self.receive(Aside {
            topic,
            ended: Some(error),
        });
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

/// A host watched until it is connected with the shell caught up.
async fn connected(
    server: &TestServer,
) -> (Arc<Host>, Arc<Recorder<HostSnapshot>>, Arc<Subscription>) {
    let host = Host::new(server.url());
    let recorder = Recorder::new();
    let subscription = host.watch(recorder.clone());
    recorder
        .wait("a synced host", |s| {
            s.status == ConnectionStatus::Connected && s.synced
        })
        .await;
    (host, recorder, subscription)
}

async fn project(host: &Host, server: &TestServer) -> String {
    host.create_project("Demo".into(), server.project_dir(), None)
        .await
        .unwrap()
        .id
}

/// Creates a thread, watches it, waits for omp's prompt and starts the
/// agent; returns once its turn is running.
async fn create_running(
    host: &Host,
    server: &TestServer,
    project_id: &str,
    text: &str,
) -> (String, Arc<Recorder<ThreadSnapshot>>, Arc<Subscription>) {
    let thread = host
        .create_thread(
            project_id.to_owned(),
            model(),
            None,
            Some(text.to_owned()),
            text.to_owned(),
            Vec::new(),
        )
        .await
        .unwrap();
    assert_eq!(thread.project_id, project_id);
    let recorder = Recorder::new();
    let subscription = host.watch_thread(thread.id.clone(), recorder.clone());
    wait_calls(&server.script, "prompt", |calls| {
        calls.iter().any(|c| c == &format!("prompt:{text}:0"))
    })
    .await;
    server.script.emit(json!({"type": "agent_start"}));
    recorder
        .wait("a running turn", |s| {
            s.sync == ThreadSync::Synced
                && s.turns
                    .last()
                    .is_some_and(|turn| turn.status == TurnStatus::Running)
        })
        .await;
    (thread.id, recorder, subscription)
}

fn assistant_text(snapshot: &ThreadSnapshot) -> Vec<String> {
    snapshot
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn host_snapshot_has_the_created_project() {
    let server = TestServer::start("connect").await;
    let (host, recorder, _subscription) = connected(&server).await;
    let first = recorder.wait("any snapshot", |_| true).await;
    let info = first.info.as_ref().expect("server info");
    assert!(!info.server_id.is_empty());
    assert!(
        first.catalog.is_some(),
        "the catalog arrives with the handshake"
    );

    let project = host
        .create_project("Demo".into(), server.project_dir(), None)
        .await
        .unwrap();
    let snapshot = recorder
        .wait("the project in the snapshot", |s| {
            s.projects.iter().any(|p| p.id == project.id)
        })
        .await;
    assert_eq!(
        snapshot
            .projects
            .iter()
            .find(|p| p.id == project.id)
            .unwrap()
            .title,
        "Demo"
    );
    assert!(
        recorder
            .threads
            .lock()
            .unwrap()
            .iter()
            .all(|name| name == "j3-mobile"),
        "listeners are called on the library runtime"
    );

    // Server errors keep their code and sentence.
    let error = host
        .create_project("Nowhere".into(), "/definitely/not/a/dir".into(), None)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, ClientError::Server { message, .. } if !message.is_empty()),
        "{error:?}"
    );

    host.close().await;
    recorder
        .wait("the closed status", |s| {
            s.status == ConnectionStatus::Closed
        })
        .await;
    let closed = host.browse_directories("/".into()).await.unwrap_err();
    assert_eq!(closed, ClientError::Disconnected);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn thread_listener_sees_a_streamed_reply() {
    let server = TestServer::start("turn").await;
    let (host, shell, _host_subscription) = connected(&server).await;
    let project_id = project(&host, &server).await;
    let (thread_id, thread, _subscription) =
        create_running(&host, &server, &project_id, "Say hi").await;

    server.script.text_start("m1");
    server.script.say("m1", "Hel");
    server.script.say("m1", "lo");
    server.script.say("m1", "!");
    server.script.text_end("m1", "Hello!");
    server.script.settle("completed");

    let done = thread
        .wait("the completed turn", |s| {
            s.turns
                .last()
                .is_some_and(|turn| turn.status == TurnStatus::Completed)
                && s.thread
                    .as_ref()
                    .is_some_and(|row| row.status == ThreadStatus::Idle)
        })
        .await;
    assert_eq!(done.thread_id, thread_id);
    assert!(matches!(&done.items[0].kind,
        ItemKind::User { text, .. } if text == "Say hi"));
    assert_eq!(assistant_text(&done), ["Hello!"]);
    assert!(
        done.items
            .iter()
            .any(|item| matches!(item.kind, ItemKind::Text { .. })
                && item.status == ItemStatus::Completed),
        "{:?}",
        done.items
    );
    shell
        .wait("the idle row in the host snapshot", |s| {
            s.threads
                .iter()
                .any(|row| row.id == thread_id && row.status == ThreadStatus::Idle)
        })
        .await;

    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn answer_request_closes_an_ask() {
    let server = TestServer::start("request").await;
    let (host, _shell, _host_subscription) = connected(&server).await;
    let project_id = project(&host, &server).await;
    let (thread_id, thread, _subscription) =
        create_running(&host, &server, &project_id, "Ask me").await;

    server.script.emit(
        json!({"type": "extension_ui_request", "id": "ui-1", "method": "select",
        "title": "Pick one", "options": ["alpha", "beta"]}),
    );
    let asked = thread
        .wait("an open request", |s| {
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

    host.answer_request(
        thread_id,
        request.id.clone(),
        vec![Answer {
            question_id: request.questions[0].id.clone(),
            values: vec!["beta".into()],
            other: None,
        }],
    )
    .await
    .unwrap();
    let answered = thread
        .wait("the request answered", |s| {
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
async fn dropping_a_subscription_stops_callbacks() {
    let server = TestServer::start("drop").await;
    let (host, _shell, _host_subscription) = connected(&server).await;
    let project_id = project(&host, &server).await;
    let (thread_id, witness, _witness_subscription) =
        create_running(&host, &server, &project_id, "Go on").await;

    let dropped = Recorder::new();
    let subscription = host.watch_thread(thread_id, dropped.clone());
    dropped
        .wait("the second subscription's snapshot", |s| {
            s.sync == ThreadSync::Synced
        })
        .await;
    drop(subscription);
    // A call under way when it was dropped may still finish.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let calls = dropped.calls();
    assert!(calls > 0);

    server.script.text_start("m1");
    server.script.say("m1", "after the drop");
    server.script.settle("completed");
    witness
        .wait("the reply at the other subscription", |s| {
            assistant_text(s) == ["after the drop"]
                && s.turns
                    .last()
                    .is_some_and(|turn| turn.status == TurnStatus::Completed)
        })
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(dropped.calls(), calls, "no calls after the drop");
    assert!(
        dropped
            .last
            .borrow()
            .as_ref()
            .is_some_and(|s| assistant_text(s).is_empty())
    );

    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_btw_streams_the_answer_and_cancels() {
    let server = TestServer::start("btw").await;
    let (host, _recorder, _subscription) = connected(&server).await;
    let project_id = project(&host, &server).await;
    let (thread_id, _thread, _watch) = create_running(&host, &server, &project_id, "work").await;

    let recorder = Recorder::<Aside>::new();
    let ask = host.ask_btw(thread_id.clone(), "why?".into(), None, recorder.clone());
    recorder
        .wait("the running topic", |a| a.topic.is_some())
        .await;
    server
        .script
        .emit(json!({"type": "btw_delta", "recordId": "btw-1", "delta": "Be"}));
    server
        .script
        .emit(json!({"type": "btw_delta", "recordId": "btw-1", "delta": "cause."}));
    let growing = recorder
        .wait("the growing answer", |a| {
            a.topic
                .as_ref()
                .is_some_and(|t| t.turns[0].answer == "Because.")
        })
        .await;
    assert_eq!(growing.topic.unwrap().turns[0].status, BtwStatus::Running);
    server
        .script
        .emit(json!({"type": "btw_record", "record": aside("why?", "Because.", "complete")}));
    let done = recorder.wait("the end", |a| a.ended.is_some()).await;
    assert_eq!(done.ended, Some(None));
    let topic = done.topic.unwrap();
    assert_eq!(topic.id, "btw-1");
    assert_eq!(topic.turns[0].status, BtwStatus::Complete);
    drop(ask);

    // A refused question ends with the server's reason and no topic.
    let refused = Recorder::<Aside>::new();
    let _ask = host.ask_btw(thread_id.clone(), " ".into(), None, refused.clone());
    let ended = refused.wait("the refusal", |a| a.ended.is_some()).await;
    assert_eq!(ended.topic, None);
    assert_eq!(
        ended.ended,
        Some(Some("Type a question after /btw.".into()))
    );

    // Cancelling the subscription cancels the question in omp.
    let cancelled = Recorder::<Aside>::new();
    let ask = host.ask_btw(thread_id, "still there?".into(), None, cancelled.clone());
    cancelled
        .wait("the running topic", |a| a.topic.is_some())
        .await;
    ask.cancel();
    wait_calls(&server.script, "btw_cancel", |calls| {
        calls.iter().any(|c| c == "btw_cancel:btw-1")
    })
    .await;
    assert_eq!(cancelled.last.borrow().as_ref().unwrap().ended, None);

    server.stop().await;
}
