//! `j3-client` against a real in-process `j3_core` server on an ephemeral port
//! with a scripted omp session: handshake, projects, launch + streaming,
//! questions, queue/steer/interrupt, reconnect after a dropped socket, image
//! upload, workspace commands, the settle workflow calls.

use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    task::{Context, Poll, Wake, Waker},
    time::Duration,
};

use futures_util::future::BoxFuture;
use j3_client::{
    Answer, Connection, ConnectionState, ConnectionStatus, Error, SendMode, ThreadState,
    ThreadSync,
    t3::{
        model::ModelSelection,
        orchestration::{
            ProviderGoalStatus, RunStatus, SettledOverride, SubagentStatus, ThreadShell,
            TurnItemKind, TurnItemStatus, UserMessageInputIntent,
        },
    },
    workflow::{self, AgentStatus, T3Section, ThreadStatus},
};
use j3_core::{
    app::{App, AppConfig},
    engine::session::{AgentSession, Events, SessionFactory},
    omp::{
        self, OmpLauncher,
        omp_rpc::{
            self, AvailableSlashCommand, Event, ExtensionUiResponse, GoalOp, GoalResult,
            ImageContent, PromptAck, RpcNotification, SessionState, SubagentMessages,
            ThinkingLevel,
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

const MODEL: &str = "opencode-go/glm-5.3-flash";
const WAIT: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------------------
// Scripted omp (from crates/j3-core/tests/engine_app.rs)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Script {
    calls: Mutex<Vec<String>>,
    events: Mutex<Option<mpsc::UnboundedSender<Event>>>,
    ui_responses: Mutex<Vec<ExtensionUiResponse>>,
    /// omp's current goal (`Goal` JSON), driven by `goal` like omp does.
    goal: Mutex<Option<Value>>,
    /// Ids of running subagents (`cancel_subagent` stops them).
    subagents: Mutex<Vec<String>>,
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

    /// omp's lifecycle frame for subagent `id`, started by tool call `call_task`.
    fn subagent_lifecycle(&self, id: &str, status: &str) {
        let mut running = self.subagents.lock().unwrap();
        running.retain(|running| running != id);
        if status == "started" {
            running.push(id.to_owned());
        }
        drop(running);
        self.emit(
            json!({"type": "subagent_lifecycle", "payload": {"id": id, "agent": "task",
            "agentSource": "bundled", "status": status, "index": 0, "parentToolCallId": "call_task",
            "description": "Answer PONG", "detached": true}}),
        );
    }

    fn subagent_progress(&self, id: &str, status: &str, output: &[&str], tokens: u64) {
        self.emit(json!({"type": "subagent_progress", "payload": {"index": 0, "agent": "task",
            "agentSource": "bundled", "task": "Complete assignment thoroughly:\n\nReply PONG.",
            "assignment": "Reply PONG.", "parentToolCallId": "call_task", "detached": true,
            "progress": {"index": 0, "id": id, "agent": "task", "agentSource": "bundled", "status": status,
                "task": "Reply PONG.", "lastIntent": "Thinking about PONG", "recentTools": [],
                "recentOutput": output, "toolCount": 0, "requests": 1, "tokens": tokens, "cost": 0,
                "durationMs": 10, "resolvedModelIdentity": MODEL}}}));
    }
}

fn goal_refused(error: &str) -> omp::Error {
    omp::Error::Rpc(omp_rpc::Error::Command {
        command: "goal".into(),
        error: error.into(),
        code: None,
    })
}

struct FakeSession(Arc<Script>);

impl AgentSession for FakeSession {
    fn session_file(&self) -> &Path {
        Path::new("/tmp/j3-client-fake-session.jsonl")
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
        level: ThinkingLevel,
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
        Box::pin(async {
            Ok(serde_json::from_value(json!([
                {"name": "skill:jev", "source": "skill", "description": "Calibrated judgments"},
                {"name": "compact", "source": "builtin"}
            ]))
            .unwrap())
        })
    }

    fn goal(
        &self,
        op: GoalOp,
        objective: Option<String>,
        token_budget: Option<i64>,
    ) -> BoxFuture<'_, j3_core::engine::session::Result<GoalResult>> {
        self.0.record(format!("goal:{}", op.as_str()));
        let mut goal = self.0.goal.lock().unwrap();
        let status = |goal: &Option<Value>| goal.as_ref().map(|goal| goal["status"].clone());
        let outcome = match op {
            GoalOp::Create if goal.is_some() => Err(goal_refused("A goal is already active.")),
            GoalOp::Create => {
                *goal = Some(
                    json!({"id": "g1", "objective": objective.unwrap_or_default(),
                    "status": "active", "tokensUsed": 0, "timeUsedSeconds": 0, "createdAt": 1,
                    "updatedAt": 1, "tokenBudget": token_budget}),
                );
                Ok(())
            }
            GoalOp::Pause if status(&goal) == Some(json!("active")) => {
                goal.as_mut().unwrap()["status"] = json!("paused");
                Ok(())
            }
            GoalOp::Resume if status(&goal) == Some(json!("paused")) => {
                let goal = goal.as_mut().unwrap();
                goal["status"] = json!("active");
                goal["tokensUsed"] = json!(1200);
                goal["timeUsedSeconds"] = json!(61.6);
                Ok(())
            }
            GoalOp::Drop if goal.is_some() => {
                goal.as_mut().unwrap()["status"] = json!("dropped");
                Ok(())
            }
            _ => Err(goal_refused("No goal to change.")),
        };
        if outcome.is_ok() {
            // Like omp: `goal_updated` first, then the response.
            self.0
                .emit(json!({"type": "goal_updated", "goal": goal.clone()}));
            if status(&goal) == Some(json!("dropped")) {
                *goal = None;
            }
        }
        let result = json!({"goal": goal.clone(), "state": null});
        drop(goal);
        Box::pin(async move { outcome.map(|()| serde_json::from_value(result).unwrap()) })
    }

    fn cancel_subagent(
        &self,
        subagent_id: String,
    ) -> BoxFuture<'_, j3_core::engine::session::Result<bool>> {
        self.0.record(format!("cancel_subagent:{subagent_id}"));
        let running = self.0.subagents.lock().unwrap().contains(&subagent_id);
        if running {
            self.0.subagent_lifecycle(&subagent_id, "aborted");
        }
        Box::pin(async move { Ok(running) })
    }

    fn subagent_messages(
        &self,
        subagent_id: String,
    ) -> BoxFuture<'_, j3_core::engine::session::Result<SubagentMessages>> {
        self.0.record(format!("subagent_messages:{subagent_id}"));
        Box::pin(async {
            Ok(serde_json::from_value(json!({"sessionFile": "/tmp/sub.jsonl", "fromByte": 0,
                "nextByte": 10, "reset": false, "entries": [], "messages": [
                    {"role": "user", "content": [{"type": "text", "text": "Reply PONG."}], "timestamp": 1},
                    {"role": "assistant", "content": [{"type": "thinking", "thinking": "easy"},
                        {"type": "text", "text": "PONG, from the transcript"}], "timestamp": 2}]}))
            .unwrap())
        })
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
    let dir = std::env::temp_dir().join(format!("j3-client-{name}-{}", random_hex(10)));
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
        // `Server::bind` reports the requested address, so pick a free port first.
        let addr = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let server = Server::bind(app, &[addr]).await.unwrap();
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
    check: impl FnMut(&Arc<T>) -> bool,
) -> Arc<T> {
    match tokio::time::timeout(WAIT, rx.wait_for(check)).await {
        Ok(Ok(state)) => Arc::clone(&state),
        Ok(Err(_)) => panic!("{what}: state channel closed"),
        Err(_) => panic!("timed out waiting for {what}"),
    }
}

async fn live(connection: &Connection) -> Arc<ConnectionState> {
    let mut state = connection.state();
    wait_state(&mut state, "a live connection", |s| {
        s.status == ConnectionStatus::Live
    })
    .await
}

async fn wait_calls(script: &Script, what: &str, check: impl Fn(&[String]) -> bool) {
    for _ in 0..500 {
        if check(&script.calls()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}: {:?}", script.calls());
}

fn model() -> ModelSelection {
    serde_json::from_value(
        json!({"instanceId": "omp", "model": MODEL, "options": [{"id": "thinking", "value": "high"}]}),
    )
    .unwrap()
}

fn assistant_text(state: &ThreadState) -> Vec<(String, bool)> {
    state
        .feed()
        .iter()
        .filter_map(|row| match &row.item.kind {
            TurnItemKind::AssistantMessage(message) => {
                Some((message.text.clone(), message.streaming))
            }
            _ => None,
        })
        .collect()
}

/// Creates the server's project and returns its id.
async fn project(connection: &Connection, server: &TestServer) -> String {
    let project = connection
        .create_project("Demo", &server.project_dir())
        .await
        .unwrap();
    assert_eq!(project.title, "Demo");
    project.id
}

/// Launches a thread and waits for omp's prompt and the run to be running.
async fn launch_running(
    connection: &Connection,
    server: &TestServer,
    project_id: &str,
    text: &str,
) -> (
    String,
    watch::Receiver<Arc<ThreadState>>,
    j3_client::ThreadHandle,
) {
    let thread_id = connection
        .launch(project_id, text, Vec::new(), model())
        .await
        .unwrap();
    let handle = connection.thread(&thread_id);
    let mut state = handle.state();
    wait_calls(&server.script, "prompt", |calls| {
        calls.iter().any(|c| c == &format!("prompt:{text}:0"))
    })
    .await;
    server.script.emit(json!({"type": "agent_start"}));
    wait_state(&mut state, "a running run", |s| {
        s.active_run()
            .is_some_and(|run| run.status == RunStatus::Running)
    })
    .await;
    (thread_id, state, handle)
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
async fn handshake_projects_and_browse() {
    let server = TestServer::start("handshake").await;
    let connection = Connection::open(server.url(), tokio::runtime::Handle::current());
    assert_eq!(connection.http_base(), server.url());

    let state = live(&connection).await;
    let descriptor = state.descriptor.as_ref().unwrap();
    assert_eq!(descriptor.label, "test-host");
    let config = state.config.as_ref().unwrap();
    assert_eq!(config.environment.environment_id, descriptor.environment_id);
    assert_eq!(config.providers[0].instance_id, "omp");
    assert!(state.shell.loaded && state.shell.synchronized);
    assert!(state.shell.projects.is_empty());

    // Creating a project reaches the shell through `project.updated`.
    let project_id = project(&connection, &server).await;
    let mut watch = connection.state();
    let state = wait_state(&mut watch, "the project in the shell", |s| {
        s.shell.project(&project_id).is_some()
    })
    .await;
    assert_eq!(
        state.shell.project(&project_id).unwrap().workspace_root,
        server.project_dir()
    );

    // Browse the project's parent from a thread with no tokio runtime.
    let parent = server
        .project_dir
        .parent()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let remote = connection.clone();
    let browse = std::thread::spawn(move || block_on(remote.browse(&format!("{parent}/"))))
        .join()
        .unwrap()
        .unwrap();
    assert!(
        browse
            .entries
            .iter()
            .any(|entry| entry.full_path == server.project_dir()),
        "{browse:?}"
    );

    // Typed failures carry the server's message.
    let error = connection
        .archive("missing-thread")
        .await
        .expect_err("archiving a missing thread fails");
    let j3_client::Error::Rejected { tag, message, .. } = &error else {
        panic!("expected a typed failure, got {error:?}")
    };
    assert_eq!(tag.as_deref(), Some("OrchestrationV2DispatchCommandError"));
    assert_eq!(&error.to_string(), message);

    connection.delete_project(&project_id).await.unwrap();
    wait_state(&mut watch, "the project removed", |s| {
        s.shell.project(&project_id).is_none()
    })
    .await;

    connection.close().await;
    assert_eq!(connection.state().borrow().status, ConnectionStatus::Closed);
    assert!(matches!(
        connection.browse("/").await,
        Err(j3_client::Error::Closed)
    ));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn launch_streams_text_and_answers_questions() {
    let server = TestServer::start("launch").await;
    let connection = Connection::open(server.url(), tokio::runtime::Handle::current());
    live(&connection).await;
    let project_id = project(&connection, &server).await;

    let (thread_id, mut state, _handle) =
        launch_running(&connection, &server, &project_id, "Say hi").await;
    let calls = server.script.calls();
    assert!(calls.contains(&"thinking:\"high\"".to_owned()), "{calls:?}");

    // The thread shows up in the shell as running.
    let mut shell = connection.state();
    let shell_state = wait_state(&mut shell, "the thread in the shell", |s| {
        s.shell
            .thread(&thread_id)
            .is_some_and(|thread| workflow::thread_status(thread) == ThreadStatus::Working)
    })
    .await;
    assert_eq!(shell_state.shell.threads_of(&project_id).len(), 1);
    assert_eq!(
        shell_state.shell.thread(&thread_id).unwrap().title,
        "Say hi"
    );

    // Streaming text reaches the feed piece by piece.
    server
        .script
        .text("m1", json!({"type": "text_start", "contentIndex": 0}));
    server.script.text(
        "m1",
        json!({"type": "text_delta", "contentIndex": 0, "delta": "Hel"}),
    );
    let partial = wait_state(&mut state, "streaming text", |s| {
        assistant_text(s) == vec![("Hel".to_owned(), true)]
    })
    .await;
    assert_eq!(partial.sync, ThreadSync::Live);
    server.script.text(
        "m1",
        json!({"type": "text_delta", "contentIndex": 0, "delta": "lo!"}),
    );
    server.script.text(
        "m1",
        json!({"type": "text_end", "contentIndex": 0, "content": "Hello!"}),
    );
    wait_state(&mut state, "final text", |s| {
        assistant_text(s) == vec![("Hello!".to_owned(), false)]
    })
    .await;
    let feed = state.borrow().feed().to_vec();
    assert!(matches!(&feed[0].item.kind,
        TurnItemKind::UserMessage(m) if m.text == "Say hi" && m.input_intent == UserMessageInputIntent::TurnStart));
    assert!(
        feed.iter()
            .enumerate()
            .all(|(index, row)| row.position == index as u64)
    );

    // omp asks; the question is pending with its item; the answer reaches omp.
    server.script.emit(
        json!({"type": "extension_ui_request", "id": "ui-1", "method": "select",
        "title": "Pick one", "options": ["alpha", "beta"]}),
    );
    let asked = wait_state(&mut state, "a pending question", |s| {
        !s.pending_user_inputs().is_empty()
    })
    .await;
    let (request_id, question_id) = {
        let pending = asked.pending_user_inputs();
        assert_eq!(pending.len(), 1);
        let question = &pending[0].questions[0];
        assert_eq!(question.question, "Pick one");
        (pending[0].request.id.clone(), question.id.clone())
    };
    assert_eq!(asked.active_run().unwrap().status, RunStatus::Waiting);
    let mut shell_rx = connection.state();
    wait_state(&mut shell_rx, "waiting for input in the shell", |s| {
        s.shell
            .thread(&thread_id)
            .is_some_and(|thread| workflow::thread_status(thread) == ThreadStatus::Input)
    })
    .await;

    connection
        .answer(
            &thread_id,
            &request_id,
            BTreeMap::from([(question_id, Answer::One("beta".into()))]),
        )
        .await
        .unwrap();
    wait_state(&mut state, "the question resolved", |s| {
        s.pending_user_inputs().is_empty()
            && s.active_run()
                .is_some_and(|run| run.status == RunStatus::Running)
    })
    .await;
    let responses = server.script.ui_responses.lock().unwrap().clone();
    assert_eq!(
        serde_json::to_value(&responses[0]).unwrap(),
        json!({"type": "extension_ui_response", "id": "ui-1", "value": "beta"})
    );

    server.script.settle("completed");
    let done = wait_state(&mut state, "the run completed", |s| {
        s.active_run().is_none()
    })
    .await;
    assert!(done.can_send_now());
    assert_eq!(
        done.projection.as_ref().unwrap().runs[0].status,
        RunStatus::Completed
    );

    // Workspace commands for the project's directory come from omp.
    let commands = connection
        .refresh_workspace(&server.project_dir())
        .await
        .unwrap();
    assert!(
        commands.skills.iter().any(|skill| skill.name == "jev"),
        "{commands:?}"
    );
    assert!(
        connection
            .state()
            .borrow()
            .workspaces
            .contains_key(&server.project_dir())
    );

    // Rename, visit, archive, unarchive, delete.
    connection.visit(&thread_id).await.unwrap();
    connection.rename(&thread_id, "Greeting").await.unwrap();
    wait_state(&mut shell, "the rename", |s| {
        s.shell
            .thread(&thread_id)
            .is_some_and(|thread| thread.title == "Greeting")
    })
    .await;
    connection.archive(&thread_id).await.unwrap();
    wait_state(&mut shell, "the thread archived", |s| {
        s.shell.thread(&thread_id).is_none()
    })
    .await;
    let archived = connection.archived().await.unwrap();
    assert!(archived.threads.iter().any(|thread| thread.id == thread_id));
    connection.unarchive(&thread_id).await.unwrap();
    wait_state(&mut shell, "the thread unarchived", |s| {
        s.shell.thread(&thread_id).is_some()
    })
    .await;
    connection.delete_thread(&thread_id).await.unwrap();
    wait_state(&mut state, "the thread deleted", |s| {
        s.sync == ThreadSync::Deleted
    })
    .await;
    wait_state(&mut shell, "the thread gone", |s| {
        s.shell.thread(&thread_id).is_none()
    })
    .await;

    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn queue_steer_and_interrupt() {
    let server = TestServer::start("queue").await;
    let connection = Connection::open(server.url(), tokio::runtime::Handle::current());
    live(&connection).await;
    let project_id = project(&connection, &server).await;
    let (thread_id, mut state, _handle) =
        launch_running(&connection, &server, &project_id, "first").await;
    let active = state.borrow().active_run().unwrap().id.clone();
    wait_state(&mut state, "steerable", |s| s.can_steer()).await;

    // Queue two; edit one, cancel the other.
    connection
        .send(
            &thread_id,
            "queued one",
            Vec::new(),
            Some(model()),
            SendMode::Queue,
        )
        .await
        .unwrap();
    connection
        .send(&thread_id, "queued two", Vec::new(), None, SendMode::Queue)
        .await
        .unwrap();
    let queued = wait_state(&mut state, "two queued runs", |s| {
        s.queued_runs().len() == 2
    })
    .await;
    let (q1, q2) = {
        let runs = queued.queued_runs();
        assert_eq!(runs[0].text, "queued one");
        assert_eq!(runs[1].text, "queued two");
        (runs[0].run.id.clone(), runs[1].run.id.clone())
    };
    connection
        .edit_queued(&thread_id, &q1, "queued one (edited)")
        .await
        .unwrap();
    connection.cancel_queued(&thread_id, &q2).await.unwrap();
    let edited = wait_state(&mut state, "edit + cancel", |s| {
        let runs = s.queued_runs();
        runs.len() == 1 && runs[0].text == "queued one (edited)"
    })
    .await;
    assert!(
        edited
            .feed()
            .iter()
            .all(|row| row.item.run_id.as_deref() != Some(q2.as_str())),
        "a cancelled queued message leaves the feed"
    );

    // Steer the running turn.
    connection
        .send(&thread_id, "steer this", Vec::new(), None, SendMode::Auto)
        .await
        .unwrap();
    wait_calls(&server.script, "steer", |calls| {
        calls.iter().any(|c| c == "steer:steer this")
    })
    .await;
    wait_state(&mut state, "the steer in the feed", |s| {
        s.feed().iter().any(|row| matches!(&row.item.kind,
            TurnItemKind::UserMessage(m) if m.input_intent == UserMessageInputIntent::Steer && m.text == "steer this"))
    })
    .await;

    // Interrupt with a held queue.
    connection
        .interrupt(&thread_id, &active, true)
        .await
        .unwrap();
    let stopped = wait_state(&mut state, "interrupted and held", |s| {
        s.active_run().is_none() && s.queue_held()
    })
    .await;
    let projection = stopped.projection.as_ref().unwrap();
    assert_eq!(
        projection
            .runs
            .iter()
            .find(|run| run.id == active)
            .unwrap()
            .status,
        RunStatus::Interrupted
    );
    assert!(
        stopped
            .feed()
            .iter()
            .any(|row| matches!(row.item.kind, TurnItemKind::RunInterruptResult(_)))
    );
    assert!(!stopped.can_steer());

    // Resume: the edited message starts.
    connection.resume_queue(&thread_id).await.unwrap();
    wait_calls(&server.script, "queued prompt", |calls| {
        calls.iter().any(|c| c == "prompt:queued one (edited):0")
    })
    .await;
    server.script.emit(json!({"type": "agent_start"}));
    wait_state(&mut state, "the queued run running", |s| {
        s.active_run()
            .is_some_and(|run| run.id == q1 && run.status == RunStatus::Running)
    })
    .await;

    // Promote a new queued message into the running turn.
    connection
        .send(&thread_id, "promote me", Vec::new(), None, SendMode::Queue)
        .await
        .unwrap();
    let queued = wait_state(&mut state, "a queued run", |s| s.queued_runs().len() == 1).await;
    let q3 = queued.queued_runs()[0].run.id.clone();
    connection
        .promote_to_steer(&thread_id, &q3, &q1)
        .await
        .unwrap();
    wait_state(&mut state, "the promoted steer", |s| {
        s.queued_runs().is_empty()
            && s.feed().iter().any(|row| matches!(&row.item.kind,
                TurnItemKind::UserMessage(m) if m.input_intent == UserMessageInputIntent::PromotedQueuedToSteer))
    })
    .await;
    assert!(
        server
            .script
            .calls()
            .contains(&"steer:promote me".to_owned())
    );

    // Stop without a run id.
    connection.stop(&thread_id).await.unwrap();
    wait_state(&mut state, "stopped", |s| s.active_run().is_none()).await;

    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn subagents_and_goals() {
    let server = TestServer::start("extras").await;
    let connection = Connection::open(server.url(), tokio::runtime::Handle::current());
    live(&connection).await;
    let project_id = project(&connection, &server).await;
    let (thread_id, mut state, _handle) =
        launch_running(&connection, &server, &project_id, "delegate").await;
    let run_id = state.borrow().active_run().unwrap().id.clone();
    let script = &server.script;

    // The agent starts a subagent through the `task` tool.
    script.emit(
        json!({"type": "tool_execution_start", "toolCallId": "call_task", "toolName": "task",
        "args": {"tasks": [{"agent": "task", "task": "Reply PONG."}]}}),
    );
    script.emit(json!({"type": "tool_execution_end", "toolCallId": "call_task", "toolName": "task",
        "result": {"content": [{"type": "text", "text": "Spawned agent `Pinger`."}]}, "isError": false}));
    script.subagent_lifecycle("Pinger", "started");
    script.subagent_progress("Pinger", "running", &[], 1200);
    let running = wait_state(&mut state, "a running subagent", |s| {
        s.subagent("Pinger").is_some_and(|subagent| {
            subagent.status == SubagentStatus::Running && subagent.progress.is_some()
        })
    })
    .await;
    let subagent = running.subagent("Pinger").unwrap();
    assert_eq!(subagent.title.as_deref(), Some("Answer PONG"));
    assert_eq!(subagent.prompt, "Reply PONG.");
    assert_eq!(subagent.model.as_deref(), Some(MODEL));
    assert_eq!(
        subagent.progress.as_deref(),
        Some("Thinking about PONG · 1.2k tokens")
    );
    assert_eq!(subagent.run_id.as_deref(), Some(run_id.as_str()));
    assert_eq!(running.subagents().len(), 1);
    // Its feed row sits under the `task` tool row.
    let row = running
        .feed()
        .iter()
        .find(|row| matches!(&row.item.kind, TurnItemKind::Subagent(body) if body.subagent_id == "Pinger"))
        .expect("a subagent row");
    assert_eq!(row.item.status, TurnItemStatus::Running);
    let parent = running
        .feed()
        .iter()
        .find(|candidate| Some(&candidate.item.id) == row.item.parent_item_id.as_ref())
        .expect("the task tool row");
    assert!(
        matches!(&parent.item.kind, TurnItemKind::DynamicTool(tool) if tool.tool_name.as_deref() == Some("task"))
    );

    // Stop it without stopping the run.
    connection
        .stop_subagent(&thread_id, &run_id, "Pinger")
        .await
        .unwrap();
    let stopped = wait_state(&mut state, "a cancelled subagent", |s| {
        s.subagent("Pinger")
            .is_some_and(|subagent| subagent.status == SubagentStatus::Cancelled)
    })
    .await;
    assert!(
        stopped
            .active_run()
            .is_some_and(|run| run.status == RunStatus::Running)
    );
    assert!(
        script
            .calls()
            .contains(&"cancel_subagent:Pinger".to_owned())
    );
    let error = connection
        .stop_subagent(&thread_id, &run_id, "Pinger")
        .await
        .unwrap_err();
    assert!(
        matches!(&error, Error::Rejected { message, .. } if message == "That subagent is no longer running."),
        "{error:?}"
    );

    // The agent revives it; it finishes and its transcript holds the answer.
    script.subagent_lifecycle("Pinger", "started");
    wait_state(&mut state, "the revived subagent", |s| {
        s.subagent("Pinger")
            .is_some_and(|subagent| subagent.status == SubagentStatus::Running)
    })
    .await;
    script.subagent_progress("Pinger", "completed", &["PONG"], 2400);
    script.subagent_lifecycle("Pinger", "completed");
    let done = wait_state(&mut state, "the subagent result", |s| {
        s.subagent("Pinger")
            .is_some_and(|subagent| subagent.result.as_deref() == Some("PONG, from the transcript"))
    })
    .await;
    let subagent = done.subagent("Pinger").unwrap();
    assert_eq!(subagent.status, SubagentStatus::Completed);
    assert!(subagent.completed_at.is_some());
    assert_eq!(done.subagents().len(), 1);

    // Goals: create, pause, resume (with usage), a refused second create, drop.
    connection
        .create_goal(&thread_id, "Ship the release", Some(50_000))
        .await
        .unwrap();
    let created = wait_state(&mut state, "an active goal", |s| {
        s.goal()
            .is_some_and(|goal| goal.status == ProviderGoalStatus::Active)
    })
    .await;
    let goal = created.goal().unwrap();
    assert_eq!(goal.objective, "Ship the release");
    assert_eq!(goal.token_budget, Some(Some(50_000)));
    let mut shell = connection.state();
    wait_state(&mut shell, "the goal on the shell row", |s| {
        s.shell.thread(&thread_id).is_some_and(|thread| {
            thread
                .goal
                .as_ref()
                .and_then(Option::as_ref)
                .is_some_and(|goal| goal.objective == "Ship the release")
        })
    })
    .await;
    connection.pause_goal(&thread_id).await.unwrap();
    wait_state(&mut state, "a paused goal", |s| {
        s.goal()
            .is_some_and(|goal| goal.status == ProviderGoalStatus::Paused)
    })
    .await;
    connection.resume_goal(&thread_id).await.unwrap();
    let resumed = wait_state(&mut state, "a resumed goal", |s| {
        s.goal()
            .is_some_and(|goal| goal.status == ProviderGoalStatus::Active)
    })
    .await;
    assert_eq!(resumed.goal().unwrap().tokens_used, Some(1200));
    assert_eq!(resumed.goal().unwrap().time_used_seconds, Some(62));
    let error = connection
        .create_goal(&thread_id, "Another", None)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, Error::Rejected { message, .. } if message == "A goal is already active."),
        "{error:?}"
    );
    connection.drop_goal(&thread_id).await.unwrap();
    wait_state(&mut state, "no goal", |s| s.goal().is_none()).await;
    assert_eq!(
        script
            .calls()
            .iter()
            .filter(|call| call.starts_with("goal:"))
            .collect::<Vec<_>>(),
        [
            "goal:create",
            "goal:pause",
            "goal:resume",
            "goal:create",
            "goal:drop"
        ]
    );

    script.settle("completed");
    wait_state(&mut state, "the run done", |s| s.active_run().is_none()).await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnects_after_the_socket_drops() {
    let server = TestServer::start("reconnect").await;
    let proxy = Proxy::start(server.addr).await;
    let connection = Connection::open(proxy.url(), tokio::runtime::Handle::current());
    live(&connection).await;
    let project_id = project(&connection, &server).await;
    let (thread_id, mut state, _handle) =
        launch_running(&connection, &server, &project_id, "keep going").await;

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
        let mut seen = vec![seen_shell.borrow_and_update().shell.snapshot_sequence];
        while seen_shell.changed().await.is_ok() {
            seen.push(seen_shell.borrow_and_update().shell.snapshot_sequence);
        }
        seen
    });

    server
        .script
        .text("m1", json!({"type": "text_start", "contentIndex": 0}));
    server.script.text(
        "m1",
        json!({"type": "text_delta", "contentIndex": 0, "delta": "before"}),
    );
    wait_state(&mut state, "text before the drop", |s| {
        assistant_text(s) == vec![("before".to_owned(), true)]
    })
    .await;
    let before = state.borrow().sequence;

    // Drop the socket; the server keeps going without the client.
    proxy.sever();
    let mut status = connection.state();
    wait_state(&mut status, "reconnecting", |s| {
        matches!(s.status, ConnectionStatus::Reconnecting { .. })
    })
    .await;
    server.script.text(
        "m1",
        json!({"type": "text_delta", "contentIndex": 0, "delta": " and after"}),
    );
    server.script.text(
        "m1",
        json!({"type": "text_end", "contentIndex": 0, "content": "before and after"}),
    );
    server.script.settle("completed");

    // The client reconnects and catches up.
    live(&connection).await;
    let caught_up = wait_state(&mut state, "caught up after reconnect", |s| {
        s.sync == ThreadSync::Live
            && s.active_run().is_none()
            && assistant_text(s) == vec![("before and after".to_owned(), false)]
    })
    .await;
    assert!(caught_up.sequence > before);
    let shell = connection.state().borrow().clone();
    assert!(
        shell
            .shell
            .thread(&thread_id)
            .is_some_and(|thread| workflow::thread_status(thread) == ThreadStatus::Ready)
    );

    // Calls work on the new socket.
    connection.rename(&thread_id, "Survived").await.unwrap();
    let mut shell_rx = connection.state();
    wait_state(&mut shell_rx, "rename after reconnect", |s| {
        s.shell
            .thread(&thread_id)
            .is_some_and(|thread| thread.title == "Survived")
    })
    .await;

    connection.close().await;
    drop(state);
    drop(_handle);
    drop(connection);
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

#[tokio::test(flavor = "multi_thread")]
async fn uploads_an_image_and_serves_it() {
    let server = TestServer::start("upload").await;
    let connection = Connection::open(server.url(), tokio::runtime::Handle::current());
    live(&connection).await;
    let project_id = project(&connection, &server).await;

    // A 1x1 PNG.
    let png: Vec<u8> = vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    let attachment = connection
        .upload_image("dot.png", "image/png", png.clone())
        .await
        .unwrap();
    let url = connection.asset_url(&attachment).await.unwrap();
    assert!(url.starts_with(&server.url()), "{url}");
    let response = reqwest::get(&url).await.unwrap();
    assert!(response.status().is_success());
    assert_eq!(
        response.headers()["content-type"].to_str().unwrap(),
        "image/png"
    );
    assert_eq!(response.bytes().await.unwrap().as_ref(), png.as_slice());

    // The attachment travels with a message to omp.
    let thread_id = connection
        .launch(&project_id, "look", vec![attachment.clone()], model())
        .await
        .unwrap();
    wait_calls(&server.script, "prompt with image", |calls| {
        calls.iter().any(|c| c == "prompt:look:1")
    })
    .await;
    let handle = connection.thread(&thread_id);
    let mut state = handle.state();
    let loaded = wait_state(&mut state, "the thread", |s| !s.feed().is_empty()).await;
    assert!(matches!(&loaded.feed()[0].item.kind,
        TurnItemKind::UserMessage(m) if m.attachments == vec![attachment.clone()]));

    server.stop().await;
}

/// The shell row of `thread_id` once `check` holds.
async fn wait_row(
    connection: &Connection,
    thread_id: &str,
    what: &str,
    check: impl Fn(&ThreadShell) -> bool,
) -> ThreadShell {
    let mut shell = connection.state();
    let state = wait_state(&mut shell, what, |s| {
        s.shell.thread(thread_id).is_some_and(&check)
    })
    .await;
    state.shell.thread(thread_id).unwrap().clone()
}

fn rejected(error: &Error) -> bool {
    matches!(error, Error::Rejected { tag, .. } if tag.as_deref() == Some("OrchestrationV2DispatchCommandError"))
}

#[tokio::test(flavor = "multi_thread")]
async fn settle_workflow_calls() {
    let server = TestServer::start("settle").await;
    let connection = Connection::open(server.url(), tokio::runtime::Handle::current());
    live(&connection).await;
    let project_id = project(&connection, &server).await;
    let (thread_id, mut state, _handle) =
        launch_running(&connection, &server, &project_id, "Settle me").await;
    let now = time::OffsetDateTime::now_utc;

    // Working, then Done: the first message stamped a visit before the run
    // completed, so the completion is unread.
    let row = wait_row(&connection, &thread_id, "the running row", |row| {
        workflow::agent_status(row) == AgentStatus::Working
    })
    .await;
    assert_eq!(workflow::thread_status(&row), ThreadStatus::Working);
    assert!(workflow::working_started_at(&row).is_some());
    server.script.settle("completed");
    wait_state(&mut state, "the run completed", |s| {
        s.active_run().is_none()
    })
    .await;
    let row = wait_row(&connection, &thread_id, "an unread completion", |row| {
        workflow::agent_status(row) == AgentStatus::Done
    })
    .await;
    assert!(workflow::has_unseen_completion(&row));
    assert_eq!(workflow::thread_status(&row).label(true), Some("Done"));
    assert_eq!(
        workflow::t3_section(&row, now(), true),
        Some(T3Section::Active)
    );

    // Visiting reads it; mark-unread brings it back.
    connection.visit(&thread_id).await.unwrap();
    wait_row(&connection, &thread_id, "the completion seen", |row| {
        workflow::agent_status(row) == AgentStatus::Idle
    })
    .await;
    connection.mark_unread(&thread_id).await.unwrap();
    wait_row(&connection, &thread_id, "unread again", |row| {
        workflow::has_unseen_completion(row)
    })
    .await;

    // Pin at a key, reorder it; Active reorders are refused while pinned.
    connection.pin(&thread_id, Some("m")).await.unwrap();
    let row = wait_row(&connection, &thread_id, "the pin", |row| {
        row.pin_order_key == Some(Some("m".into()))
    })
    .await;
    assert_eq!(
        workflow::t3_section(&row, now(), true),
        Some(T3Section::Pinned)
    );
    connection.reorder_pinned(&thread_id, "g").await.unwrap();
    wait_row(&connection, &thread_id, "the pin reorder", |row| {
        row.pin_order_key == Some(Some("g".into()))
    })
    .await;
    let error = connection
        .reorder_active(&thread_id, "t")
        .await
        .unwrap_err();
    assert!(rejected(&error), "{error:?}");
    connection.unpin(&thread_id).await.unwrap();
    let row = wait_row(&connection, &thread_id, "the unpin", |row| {
        row.pinned_at.clone().flatten().is_none()
    })
    .await;
    assert_eq!(row.pin_order_key.clone().flatten(), None);
    connection.reorder_active(&thread_id, "t").await.unwrap();
    wait_row(&connection, &thread_id, "the active reorder", |row| {
        row.active_order_key == Some(Some("t".into()))
    })
    .await;

    // Settle drops the order key; unsettle reopens it.
    connection.settle(&thread_id).await.unwrap();
    let row = wait_row(&connection, &thread_id, "settled", |row| {
        workflow::t3_section(row, now(), true) == Some(T3Section::Settled)
    })
    .await;
    assert!(row.settled_at.is_some());
    assert_eq!(row.active_order_key.clone().flatten(), None);
    assert_eq!(
        workflow::settled_timestamp(&row),
        row.settled_at.as_deref().map(|at| {
            time::OffsetDateTime::parse(at, &time::format_description::well_known::Rfc3339).unwrap()
        })
    );
    connection.unsettle(&thread_id).await.unwrap();
    let row = wait_row(&connection, &thread_id, "unsettled", |row| {
        row.settled_override == Some(SettledOverride::Active)
    })
    .await;
    assert!(row.unsettled_at.clone().flatten().is_some());
    assert_eq!(
        workflow::t3_section(&row, now(), true),
        Some(T3Section::Active)
    );

    // Snooze: the past is refused; a future wake hides it; unsnooze wakes it.
    assert!(workflow::can_snooze(&row, now()));
    let error = connection
        .snooze(&thread_id, now() - time::Duration::minutes(1))
        .await
        .unwrap_err();
    assert!(rejected(&error), "{error:?}");
    let presets = workflow::resolve_snooze_presets(now(), &workflow::TimeZone::UTC);
    let wake = presets[0].snoozed_until;
    connection.snooze(&thread_id, wake).await.unwrap();
    let row = wait_row(&connection, &thread_id, "snoozed", |row| {
        workflow::effective_snoozed(row, now())
    })
    .await;
    assert_eq!(
        workflow::t3_section(&row, now(), true),
        Some(T3Section::Snoozed)
    );
    assert_eq!(
        workflow::next_snooze_wake_after([&row], now()),
        Some(
            wake.replace_nanosecond(wake.millisecond() as u32 * 1_000_000)
                .unwrap()
        )
    );
    connection.unsnooze(&thread_id).await.unwrap();
    wait_row(&connection, &thread_id, "awake", |row| {
        row.snoozed_until.clone().flatten().is_none()
    })
    .await;

    // The per-thread auto-settle opt-out.
    connection.set_auto_settle(&thread_id, false).await.unwrap();
    wait_row(&connection, &thread_id, "auto-settle off", |row| {
        row.auto_settle_disabled_at.clone().flatten().is_some()
    })
    .await;
    connection.set_auto_settle(&thread_id, true).await.unwrap();
    wait_row(&connection, &thread_id, "auto-settle on", |row| {
        row.auto_settle_disabled_at.clone().flatten().is_none()
    })
    .await;

    // A real message reopens a settled thread.
    connection.settle(&thread_id).await.unwrap();
    wait_row(&connection, &thread_id, "settled again", |row| {
        row.settled_override == Some(SettledOverride::Settled)
    })
    .await;
    connection
        .send(
            &thread_id,
            "Back to work",
            Vec::new(),
            None,
            SendMode::Start,
        )
        .await
        .unwrap();
    let row = wait_row(&connection, &thread_id, "auto-unsettled", |row| {
        row.settled_override.is_none()
    })
    .await;
    assert_ne!(
        workflow::t3_section(&row, now(), false),
        Some(T3Section::Settled)
    );

    server.stop().await;
}

/// Read-only smoke test against a live server (default shieldy):
/// `cargo test -p j3-client --test client -- --ignored live_server_smoke`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live j3 server (J3_SMOKE_URL, default http://shieldy:3780)"]
async fn live_server_smoke() {
    let url = std::env::var("J3_SMOKE_URL").unwrap_or_else(|_| "http://shieldy:3780".into());
    let connection = Connection::open(url.clone(), tokio::runtime::Handle::current());
    let mut watch = connection.state();
    let state = wait_state(&mut watch, "a live connection", |s| {
        s.status == ConnectionStatus::Live
    })
    .await;
    let descriptor = state.descriptor.as_ref().unwrap();
    let config = state.config.as_ref().unwrap();
    println!(
        "{url}: environment {} ({}), server {}, protocol {:?}",
        descriptor.label,
        descriptor.environment_id,
        descriptor.server_version,
        descriptor.orchestration_protocol_version
    );
    for provider in &config.providers {
        println!(
            "provider {} ({}): {:?}, {} models, {} slash commands, {} skills",
            provider.instance_id,
            provider.driver,
            provider.status,
            provider.models.len(),
            provider.slash_commands.len(),
            provider.skills.len()
        );
    }
    println!(
        "shell: sequence {}, {} projects, {} threads",
        state.shell.snapshot_sequence,
        state.shell.projects.len(),
        state.shell.threads.len()
    );
    for project in &state.shell.projects {
        println!(
            "  {} {} — {} threads",
            project.title,
            project.workspace_root,
            state.shell.threads_of(&project.id).len()
        );
    }
    let home = connection.browse("~/").await.unwrap();
    println!(
        "browse ~/: {} ({} entries)",
        home.parent_path,
        home.entries.len()
    );
    assert!(!home.parent_path.is_empty());
    if let Some(thread) = state.shell.threads.first() {
        let handle = connection.thread(&thread.id);
        let mut thread_state = handle.state();
        let loaded = wait_state(&mut thread_state, "a thread snapshot", |s| {
            s.sync == ThreadSync::Live
        })
        .await;
        println!(
            "thread {:?}: {} feed items, active run: {}",
            thread.title,
            loaded.feed().len(),
            loaded.active_run().is_some()
        );
    }
    connection.close().await;
}
