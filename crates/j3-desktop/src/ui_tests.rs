//! UI integration tests: the real views against an in-process j3 server
//! with a scripted omp.
//!
//! Each test opens the production [`Workspace`] in a headless window whose
//! local environment is a real `j3_core` server on a free port. Agent
//! sessions are scripted ([`Script`]); the model catalog comes from a tiny
//! shell-script omp that answers the utility queries. GPUI may park while
//! the tokio runtime does network work, so its clock follows real time and
//! the bounded waits below are real-time bounds.

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::future::BoxFuture;
use gpui_kit::test::{
    ElementSnapshot, TestAppContextExt as _, TestEventExt as _, TestQueryExt as _,
    TestWindowExt as _,
};
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, ElementId, MouseButton, TestAppContext, Window,
    WindowBounds, WindowOptions, px, size,
};
use j3_client::{
    Connection, ConnectionStatus, ThreadState,
    t3::{
        model::ModelSelection,
        orchestration::{RunStatus, ThreadShell, TurnItemKind, UserMessageInputIntent},
    },
};
use j3_core::{
    app::{App as CoreApp, AppConfig},
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
use tokio::sync::mpsc;

use crate::{
    actions,
    assist::AssistOmp,
    local_server::LocalServer,
    mention, models,
    services::Services,
    speech::{
        AudioSource, AudioStream, ModelStatus, Pass, Recognizer, SpeechEngine,
        models::{Hardware, SpeechModel},
    },
    workspace::Workspace,
};

/// The model the fake omp lists (and the tests launch with).
const MODEL: &str = "opencode-go/glm-5.3-flash";
/// Bound on every wait for the server, the fake or the UI.
const WAIT: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------------------
// Scripted omp (as in crates/j3-client/tests/client.rs)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Script {
    calls: Mutex<Vec<String>>,
    /// Each started session's event channel, in start order.
    sessions: Mutex<Vec<mpsc::UnboundedSender<Event>>>,
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

    fn called(&self, call: &str) -> bool {
        self.calls.lock().unwrap().iter().any(|c| c == call)
    }

    /// Sends an omp notification to the latest session.
    fn emit(&self, value: Value) {
        let last = self.sessions.lock().unwrap().len().checked_sub(1);
        self.emit_to(last.expect("omp started"), value);
    }

    /// Sends an omp notification to the `session`th session (0-based).
    fn emit_to(&self, session: usize, value: Value) {
        let notification = RpcNotification::from_value(value).unwrap();
        self.sessions.lock().unwrap()[session]
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
        Path::new("/tmp/j3-desktop-fake-session.jsonl")
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
                "model": model_info(),
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
                    {"role": "assistant", "content": [{"type": "text", "text": "PONG, from the transcript"}],
                        "timestamp": 2}]}))
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
            script.sessions.lock().unwrap().push(tx);
            Ok((Box::new(FakeSession(script)) as Box<dyn AgentSession>, rx))
        })
    }
}

/// omp's description of [`MODEL`].
fn model_info() -> Value {
    json!({"id": "glm-5.3-flash", "name": "GLM 5.3 Flash", "api": "x", "provider": "opencode-go",
        "baseUrl": "x", "reasoning": true, "input": ["text"],
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
        "contextWindow": null, "maxTokens": null})
}

/// A utility omp (`--no-session`) for the server's model catalog and the
/// composer's word prediction: `ready`, then one canned response per
/// command. It predicts `tor` after any draft ending in `refac` (`or` after
/// `refact`) and logs prediction feedback to `predict-feedback.log` beside
/// itself.
fn fake_omp_script() -> String {
    let model = model_info().to_string();
    format!(
        r#"#!/bin/sh
if [ "$1" = usage ]; then exec cat "$(dirname "$0")/usage.json"; fi
printf '%s\n' '{{"type":"ready"}}'
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"type":"get_available_models"'*) command=get_available_models; data='{{"models":[{model}]}}' ;;
    *'"type":"get_state"'*) command=get_state; data='{{"sessionId":"utility","model":{model},"thinkingLevel":"low"}}' ;;
    *'"type":"get_available_commands"'*) command=get_available_commands; data='{{"commands":[]}}' ;;
    *'"type":"set_ask_dialog"'*) command=set_ask_dialog; data='{{"enabled":true}}' ;;
    *'"type":"predict_word_feedback"'*)
      printf '%s\n' "$line" >> "$(dirname "$0")/predict-feedback.log"
      command=predict_word_feedback; data='{{}}' ;;
    *'"type":"predict_word"'*)
      command=predict_word
      case "$line" in
        *'refact"'*) data='{{"suffix":"or"}}' ;;
        *'refac"'*) data='{{"suffix":"tor"}}' ;;
        *) data='{{"suffix":null}}' ;;
      esac ;;
    *) command=unknown; data='{{}}' ;;
  esac
  printf '{{"type":"response","command":"%s","success":true,"id":"%s","data":%s}}\n' "$command" "$id" "$data"
done
"#
    )
}

/// A microphone that has heard a second of sound the moment it opens.
struct FakeMicrophone;

struct FakeStream;

impl AudioStream for FakeStream {
    fn rate(&self) -> u32 {
        16_000
    }
}

impl AudioSource for FakeMicrophone {
    fn open(&self, sink: Arc<Mutex<Vec<f32>>>) -> Result<Box<dyn AudioStream>, String> {
        sink.lock()
            .unwrap()
            .extend(std::iter::repeat_n(0.1, 16_000));
        Ok(Box::new(FakeStream))
    }
}

/// Hears "hello" while recording and "hello there" in the end.
struct FakeRecognizer(Hardware);

impl Recognizer for FakeRecognizer {
    fn hardware(&self) -> &Hardware {
        &self.0
    }

    fn prepare(&self, _: SpeechModel, _: &mut dyn FnMut(ModelStatus)) -> Result<(), String> {
        Ok(())
    }

    fn transcribe(&self, _: &[f32], pass: Pass) -> Result<String, String> {
        Ok(match pass {
            Pass::Partial => "hello",
            Pass::Final => "hello there",
        }
        .to_owned())
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("j3-desktop-{name}-{}", random_hex(10)));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn model() -> ModelSelection {
    serde_json::from_value(
        json!({"instanceId": "omp", "model": MODEL, "options": [{"id": "thinking", "value": "high"}]}),
    )
    .unwrap()
}

/// Serves a j3 server over `data_dir` on a free port, sessions faked by
/// `script`; returns it with its port.
fn serve(
    runtime: &tokio::runtime::Runtime,
    data_dir: &Path,
    omp_path: &Path,
    script: Arc<Script>,
) -> (Server, u16) {
    runtime.block_on(async {
        let app = CoreApp::open_with(
            AppConfig {
                data_dir: data_dir.to_owned(),
                label: "test-host".into(),
                omp: OmpLauncher::new(omp_path),
            },
            Arc::new(FakeFactory(script)),
            Duration::from_secs(600),
        )
        .await
        .unwrap();
        // `Server::bind` reports the requested address, so pick a free port first.
        let addr: SocketAddr = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        (Server::bind(app, &[addr]).await.unwrap(), addr.port())
    })
}

// ---------------------------------------------------------------------------
// The app under test
// ---------------------------------------------------------------------------

struct Harness {
    window: AnyWindowHandle,
    script: Arc<Script>,
    /// The local environment's connection (the app's own).
    connection: Connection,
    project_dir: PathBuf,
    data_dir: PathBuf,
    config_dir: PathBuf,
    runtime: Option<tokio::runtime::Runtime>,
}

/// The UI preferences tests start with: quiet, every T3 shelf open.
fn test_ui() -> Value {
    json!({"sounds": false, "expandedShelves": ["working", "snoozed", "settled"]})
}

impl Harness {
    /// Serves a fresh j3 server and opens the workspace window on it, with
    /// the local connection live.
    async fn start(name: &str, cx: &mut TestAppContext) -> Self {
        Self::start_with(name, cx, |_| json!([]), test_ui()).await
    }

    /// Like [`Harness::start`], with these UI preferences.
    async fn start_ui(name: &str, cx: &mut TestAppContext, ui: Value) -> Self {
        Self::start_with(name, cx, |_| json!([]), ui).await
    }

    /// Like [`Harness::start`], with the remote environments `remotes`
    /// returns for the local server's port and the UI preferences `ui`
    /// written to the desktop config.
    async fn start_with(
        name: &str,
        cx: &mut TestAppContext,
        remotes: impl FnOnce(u16) -> Value,
        ui: Value,
    ) -> Self {
        // The server and connections run on tokio threads; GPUI waits for them.
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        let data_dir = temp_dir(name);
        let project_dir = temp_dir(&format!("{name}-project"));
        let config_dir = temp_dir(&format!("{name}-config"));
        let omp_path = data_dir.join("fake-omp");
        std::fs::write(&omp_path, fake_omp_script()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&omp_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let script = Arc::new(Script::default());
        let (server, port) = serve(&runtime, &data_dir, &omp_path, Arc::clone(&script));
        let config_path = config_dir.join("desktop.json");
        std::fs::write(
            &config_path,
            json!({ "environments": remotes(port), "ui": ui }).to_string(),
        )
        .unwrap();
        let local = Arc::new(LocalServer::from_server(server));
        // The composer's word prediction asks the fake omp too.
        let assist = AssistOmp::new(OmpLauncher::new(&omp_path), project_dir.clone());
        let speech = SpeechEngine::new(
            Box::new(FakeMicrophone),
            Arc::new(FakeRecognizer(Hardware {
                gpu: None,
                threads: 4,
            })),
        );
        let services = Services::new(
            runtime.handle().clone(),
            local,
            assist,
            speech,
            Some(config_path),
        );

        let (window, _) = cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(services);
            actions::init(cx);
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Default::default(),
                        size(px(1280.), px(820.)),
                    ))),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| Workspace::new(window, cx)),
            )
            .expect("open the test window")
        });
        let connection = cx.update(|cx| Workspace::global_connections(cx)[0].clone());
        let live = connection.clone();
        cx.wait_for(window, WAIT, move |_, _| {
            let state = live.state();
            let state = state.borrow();
            state.status == ConnectionStatus::Live && state.shell.loaded
        })
        .await;
        Self {
            window,
            script,
            connection,
            project_dir,
            data_dir,
            config_dir,
            runtime: Some(runtime),
        }
    }

    /// Waits for `condition` over the rendered window and the app.
    async fn until(
        &self,
        cx: &mut TestAppContext,
        condition: impl FnMut(&mut Window, &mut App) -> bool,
    ) {
        cx.wait_for(self.window, WAIT, condition).await;
    }

    /// Waits until the fake has recorded `call`.
    async fn until_called(&self, cx: &mut TestAppContext, call: &str) {
        let script = Arc::clone(&self.script);
        let call = call.to_owned();
        self.until(cx, move |_, _| script.called(&call)).await;
    }

    /// Waits until the thread's client state satisfies `check`.
    async fn until_thread(
        &self,
        cx: &mut TestAppContext,
        thread_id: &str,
        mut check: impl FnMut(&ThreadState) -> bool + 'static,
    ) {
        let handle = self.connection.thread(thread_id);
        self.until(cx, move |_, _| check(&handle.state().borrow()))
            .await;
    }

    /// Waits until the shell's row of the thread satisfies `check`.
    async fn until_shell(
        &self,
        cx: &mut TestAppContext,
        thread_id: &str,
        mut check: impl FnMut(&ThreadShell) -> bool + 'static,
    ) {
        let connection = self.connection.clone();
        let thread_id = thread_id.to_owned();
        self.until(cx, move |_, _| {
            connection
                .state()
                .borrow()
                .shell
                .thread(&thread_id)
                .is_some_and(&mut check)
        })
        .await;
    }

    /// Waits until `desktop.json` satisfies `check`.
    async fn until_config(&self, cx: &mut TestAppContext, check: impl Fn(&Value) -> bool) {
        let path = self.config_dir.join("desktop.json");
        self.until(cx, move |_, _| {
            std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .is_some_and(|config| check(&config))
        })
        .await;
    }

    fn update<R>(&self, cx: &mut TestAppContext, f: impl FnOnce(&mut Window, &mut App) -> R) -> R {
        cx.update_window(self.window, |_, window, cx| f(window, cx))
            .expect("the test window is open")
    }

    /// Creates the project "Demo" through the connection and waits until
    /// the app has it.
    async fn project(&self, cx: &mut TestAppContext) -> String {
        let project = self
            .connection
            .create_project("Demo", &self.project_dir.to_string_lossy())
            .await
            .unwrap();
        let connection = self.connection.clone();
        let id = project.id.clone();
        self.until(cx, move |_, _| {
            connection.state().borrow().shell.project(&id).is_some()
        })
        .await;
        project.id
    }

    /// Launches a thread through the connection and starts its run (in the
    /// newest fake session); returns its id.
    async fn launch(&self, cx: &mut TestAppContext, project_id: &str, text: &str) -> String {
        let thread_id = self
            .connection
            .launch(project_id, text, Vec::new(), model())
            .await
            .unwrap();
        self.until_called(cx, &format!("prompt:{text}:0")).await;
        self.script.emit(json!({"type": "agent_start"}));
        self.until_shell(cx, &thread_id, |thread| thread.active_run_id.is_some())
            .await;
        thread_id
    }

    /// Launches a thread and lets its run complete; returns its id.
    async fn completed(&self, cx: &mut TestAppContext, project_id: &str, text: &str) -> String {
        let thread_id = self.launch(cx, project_id, text).await;
        self.script.settle("completed");
        self.until_shell(cx, &thread_id, |thread| {
            thread.active_run_id.is_none()
                && thread
                    .latest_run_completed_at
                    .as_ref()
                    .is_some_and(Option::is_some)
        })
        .await;
        thread_id
    }

    /// Launches a thread, starts its run and opens it from the sidebar;
    /// returns its id.
    async fn open_running_thread(
        &self,
        cx: &mut TestAppContext,
        project_id: &str,
        text: &str,
    ) -> String {
        let thread_id = self.launch(cx, project_id, text).await;
        let row = format!("{text}, Working");
        self.until(cx, |window, _| in_sidebar(window, &row)).await;
        self.update(cx, |window, cx| click_sidebar(window, &row, cx));
        // The thread view's composer offers Stop while the run is active.
        self.until(cx, |window, _| stoppable(window)).await;
        self.until_thread(cx, &thread_id, |state| state.can_steer())
            .await;
        thread_id
    }

    /// Focuses the composer's text area by clicking it.
    fn click_composer(&self, cx: &mut TestAppContext) {
        self.update(cx, |window, cx| {
            click_label(window, "Message", cx);
            assert_eq!(window.find_by_label("Message").focused(), Some(true));
        });
    }

    fn stop(mut self, cx: &mut TestAppContext) {
        let (local, assist) = cx.update(|cx| {
            let services = Services::global(cx);
            (Arc::clone(services.local()), Arc::clone(services.assist()))
        });
        let runtime = self.runtime.take().unwrap();
        runtime.block_on(async { tokio::join!(local.shutdown(), assist.close()) });
        runtime.shutdown_background();
        for dir in [&self.data_dir, &self.project_dir, &self.config_dir] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Whether the composer's one button offers Stop: a run is active and the
/// draft is empty.
fn stoppable(window: &Window) -> bool {
    label_of(window, "send-message").as_deref() == Some("Stop")
}

/// Whether an observed element carries exactly this accessible label.
fn has_label(window: &Window, label: &str) -> bool {
    !window.find_all_by_label(label).is_empty()
}

/// The accessible label of the element registered under `id`, if any.
fn label_of(window: &Window, id: &str) -> Option<String> {
    window
        .try_find(id.to_owned())
        .and_then(|element| element.label().map(str::to_owned))
}

/// The element labelled `label` inside the element `scope`, if both exist.
fn in_scope(window: &mut Window, scope: &str, label: &str) -> Option<ElementSnapshot> {
    window.try_find(scope.to_owned())?;
    window
        .within(scope.to_owned())
        .find_all_by_label(label)
        .into_iter()
        .next()
}

/// The sidebar row labelled `label`, if shown.
fn sidebar_row(window: &mut Window, label: &str) -> Option<ElementSnapshot> {
    in_scope(window, "sidebar", label)
}

fn in_sidebar(window: &mut Window, label: &str) -> bool {
    sidebar_row(window, label).is_some()
}

/// Clicks a sidebar row through native pointer events.
fn click_sidebar(window: &mut Window, label: &str, cx: &mut App) {
    let position = sidebar_row(window, label)
        .unwrap_or_else(|| panic!("sidebar row {label:?}"))
        .bounds()
        .center();
    window.pointer_move(position, None, cx);
    window.pointer_down(position, MouseButton::Left, cx);
    window.pointer_up(position, MouseButton::Left, cx);
}

/// Clicks the center of the element with this accessible label through
/// native pointer events (for rows whose ids are list indices).
fn click_label(window: &mut Window, label: &str, cx: &mut App) {
    let position = window.find_by_label(label).bounds().center();
    window.pointer_move(position, None, cx);
    window.pointer_down(position, MouseButton::Left, cx);
    window.pointer_up(position, MouseButton::Left, cx);
}

/// The top edge of the sidebar row labelled `label`.
fn top_of(window: &mut Window, label: &str) -> gpui_kit::Pixels {
    sidebar_row(window, label)
        .unwrap_or_else(|| panic!("sidebar row {label:?}"))
        .bounds()
        .top()
}

/// The feed row whose accessible label is `text`.
fn feed_row(window: &Window, text: &str) -> Option<ElementSnapshot> {
    let feed = ElementId::Name("feed".into());
    window
        .find_all_by_label(text)
        .into_iter()
        .find(|row| row.path().contains(&feed))
}

/// A thread row's element id (its sidebar row in T3 mode).
fn thread_row(thread_id: &str) -> String {
    format!("thread-local-{thread_id}")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[gpui_kit::test]
async fn the_titlebar_and_text_measure_frame_the_thread(cx: &mut TestAppContext) {
    let app = Harness::start("frame", cx).await;
    let project_id = app.project(cx).await;
    app.open_running_thread(cx, &project_id, "framed").await;
    app.update(cx, |window, _| {
        // The panes' header row is the top of the window (on macOS, its
        // titlebar), and the sidebar's header clears the traffic lights.
        let row = crate::titlebar::row_height(window);
        assert_eq!(window.find("t3-list").bounds().top(), row);
        let filter = window.find("project-filter").bounds();
        assert!(filter.bottom() <= row);
        if let Some(inset) = crate::titlebar::traffic_lights_inset(window) {
            assert!(filter.left() >= inset);
        }
        // T3's header offers adding a project next to the new-thread button.
        assert!(window.try_find("add-project").is_some());
        // The conversation uses the (tighter) default measure.
        assert_eq!(
            window.find("composer").bounds().size.width,
            window.rem_size() * crate::ui_settings::DEFAULT_TEXT_MEASURE
        );
    });
    app.stop(cx);
}

#[gpui_kit::test]
async fn usage_shows_the_model_provider_refreshes_on_press_and_keeps_the_cache_on_failure(
    cx: &mut TestAppContext,
) {
    let app = Harness::start("usage", cx).await;
    let cache = app.config_dir.join("usage.json");
    let omp_usage = app.data_dir.join("usage.json");
    let limit = |id: &str, label: &str, used: f64| {
        json!({"id": id, "label": label, "status": "ok",
            "window": {"id": "5h", "label": "5 Hour", "resetsAt": 1_793_573_042_000_i64},
            "amount": {"usedFraction": used, "unit": "percent"}})
    };
    std::fs::write(
        &omp_usage,
        json!({"generatedAt": 1_793_566_275_810_i64, "reports": [
            {"provider": "anthropic", "metadata": {"email": "me@example.com"},
             "limits": [limit("anthropic:5h", "Claude 5 Hour", 0.5)]},
            {"provider": "opencode-go", "metadata": {},
             "limits": [limit("go:5h", "5 Hour limit", 0.78)]},
        ]})
        .to_string(),
    )
    .unwrap();
    app.project(cx).await;
    let connection = app.connection.clone();
    app.until(cx, move |_, _| {
        connection
            .state()
            .borrow()
            .config
            .as_ref()
            .is_some_and(|config| config.providers.iter().any(|p| !p.models.is_empty()))
    })
    .await;
    // The button sits beside the composer's model picker.
    app.update(cx, |window, cx| window.click("new-thread", cx));
    app.until(cx, |window, _| window.try_find("usage").is_some())
        .await;
    // Showing the gauge fetches at once (nothing is cached yet); the
    // gauge then shows the model provider's peak.
    let fetched = cache.clone();
    app.until(cx, move |window, _| {
        fetched.exists() && has_label(window, "78%")
    })
    .await;

    app.update(cx, |window, cx| window.click("usage", cx));
    // Only the model's provider (opencode-go, report 1) is listed.
    app.until(cx, |window, _| window.try_find("usage-local-1-0").is_some())
        .await;
    app.update(cx, |window, _| {
        assert!(window.try_find("usage-local-0-0").is_none());
    });
    let cached: Value = serde_json::from_slice(&std::fs::read(&cache).unwrap()).unwrap();
    assert_eq!(
        cached["local"]["reports"][1]["limits"][0]["amount"]["usedFraction"],
        0.78
    );

    // A failed refresh shows its error next to the cached snapshot.
    std::fs::remove_file(&omp_usage).unwrap();
    app.update(cx, |window, cx| window.click("usage", cx));
    app.until(cx, |window, _| window.try_find("usage-panel").is_none())
        .await;
    app.update(cx, |window, cx| window.click("usage", cx));
    app.until(cx, |window, _| {
        window.try_find("usage-error-local").is_some()
    })
    .await;
    app.update(cx, |window, _| {
        assert!(window.try_find("usage-local-1-0").is_some());
    });
    let after: Value = serde_json::from_slice(&std::fs::read(&cache).unwrap()).unwrap();
    assert_eq!(after, cached);
    app.stop(cx);
}

#[gpui_kit::test]
async fn settings_switch_the_workflow_and_persist(cx: &mut TestAppContext) {
    let app = Harness::start("settings", cx).await;
    // T3 is the default.
    app.until(cx, |window, _| window.try_find("t3-list").is_some())
        .await;
    app.update(cx, |window, _| {
        assert!(window.try_find("herdr-list").is_none());
        assert!(window.try_find("project-filter").is_some());
    });

    // ⌘, opens Settings over the window; herdr is one click away.
    app.update(cx, |window, cx| window.press("secondary-,", cx));
    app.until(cx, |window, _| window.try_find("close-settings").is_some())
        .await;
    app.update(cx, |window, cx| {
        assert!(window.try_find("t3-list").is_none());
        window.click("workflow-herdr", cx);
    });
    app.until_config(cx, |config| config["ui"]["workflow"] == "herdr")
        .await;
    // A switch saves too.
    app.until(cx, |window, _| has_label(window, "Working shelf"))
        .await;
    app.update(cx, |window, cx| click_label(window, "Working shelf", cx));
    app.until_config(cx, |config| config["ui"]["workingShelf"] == false)
        .await;

    // Escape leaves Settings onto the herdr sidebar.
    app.update(cx, |window, cx| window.press("escape", cx));
    app.until(cx, |window, _| {
        window.try_find("close-settings").is_none()
            && window.try_find("herdr-spaces").is_some()
            && window.try_find("t3-list").is_none()
    })
    .await;

    // And back to T3 with the Done button.
    app.update(cx, |window, cx| window.press("secondary-,", cx));
    app.until(cx, |window, _| window.try_find("workflow-t3").is_some())
        .await;
    app.update(cx, |window, cx| window.click("workflow-t3", cx));
    app.until_config(cx, |config| config["ui"]["workflow"] == "t3")
        .await;
    app.update(cx, |window, cx| window.click("close-settings", cx));
    app.until(cx, |window, _| window.try_find("t3-list").is_some())
        .await;

    // What was saved is what the next launch loads.
    let path = app.config_dir.join("desktop.json");
    let preferences = crate::preferences::Preferences::load(Some(path), "test-host");
    assert_eq!(preferences.ui().workflow, crate::ui_settings::Workflow::T3);
    assert!(!preferences.ui().working_shelf);
    assert!(!preferences.ui().sounds);

    app.stop(cx);
}

#[gpui_kit::test]
async fn t3_sections_settle_and_advance_pin_and_reorder(cx: &mut TestAppContext) {
    let app = Harness::start("t3", cx).await;
    let project_id = app.project(cx).await;
    app.until(cx, |window, _| has_label(window, "No threads yet"))
        .await;

    // Three finished threads: unread ("Done") in Active, newest first.
    let alpha = app.completed(cx, &project_id, "Alpha").await;
    let beta = app.completed(cx, &project_id, "Beta").await;
    let gamma = app.completed(cx, &project_id, "Gamma").await;
    app.until(cx, |window, _| {
        ["Alpha, Done", "Beta, Done", "Gamma, Done"]
            .iter()
            .all(|label| in_scope(window, "t3-active", label).is_some())
    })
    .await;
    app.update(cx, |window, _| {
        assert!(top_of(window, "Gamma, Done") < top_of(window, "Beta, Done"));
        assert!(top_of(window, "Beta, Done") < top_of(window, "Alpha, Done"));
    });

    // Opening a thread reads it.
    app.update(cx, |window, cx| click_sidebar(window, "Gamma, Done", cx));
    app.until(cx, |window, _| {
        in_sidebar(window, "Gamma") && feed_row(window, "Gamma").is_some()
    })
    .await;

    // ⌘⇧S settles it and moves on to the next unsettled row.
    app.update(cx, |window, cx| window.press("secondary-shift-s", cx));
    app.until(cx, |window, _| {
        in_scope(window, "t3-settled", "Gamma, Settled").is_some()
            && feed_row(window, "Beta").is_some()
            && in_sidebar(window, "Beta")
    })
    .await;
    app.update(cx, |window, _| {
        // The Settled shelf sits below Active.
        assert!(window.find("shelf-settled").bounds().top() > top_of(window, "Alpha, Done"));
        assert_eq!(
            label_of(window, "shelf-settled").as_deref(),
            Some("Settled 1")
        );
    });
    let settled = app
        .connection
        .state()
        .borrow()
        .shell
        .thread(&gamma)
        .cloned();
    assert!(settled.is_some_and(|thread| thread.settled_at.is_some()));

    // ⌘⇧S on a settled thread un-settles it, in place.
    app.update(cx, |window, cx| click_sidebar(window, "Gamma, Settled", cx));
    app.until(cx, |window, _| feed_row(window, "Gamma").is_some())
        .await;
    app.update(cx, |window, cx| window.press("secondary-shift-s", cx));
    app.until(cx, |window, _| {
        in_scope(window, "t3-active", "Gamma").is_some()
    })
    .await;

    // Pin two threads from the context menu.
    for (id, label) in [(&alpha, "Alpha, Done"), (&beta, "Beta")] {
        app.update(cx, |window, cx| window.right_click(thread_row(id), cx));
        app.until(cx, |window, _| has_label(window, "Pin thread"))
            .await;
        app.update(cx, |window, cx| click_label(window, "Pin thread", cx));
        app.until(cx, |window, _| {
            in_scope(window, "t3-pinned", label).is_some()
        })
        .await;
    }
    // Keyless pins: the newest first.
    app.update(cx, |window, _| {
        assert!(top_of(window, "Beta") < top_of(window, "Alpha, Done"));
        assert!(top_of(window, "Alpha, Done") < top_of(window, "Gamma"));
    });

    // Dragging Alpha onto Beta puts it first and gives both an order key.
    app.update(cx, |window, cx| {
        window.drag_to(thread_row(&alpha), thread_row(&beta), cx)
    });
    app.until(cx, |window, _| {
        top_of(window, "Alpha, Done") < top_of(window, "Beta")
    })
    .await;
    app.until_shell(cx, &beta, |thread| {
        thread.pin_order_key.as_ref().is_some_and(Option::is_some)
    })
    .await;

    // Dropping a pinned row on the Settled shelf settles it.
    app.update(cx, |window, cx| {
        window.drag_to(thread_row(&beta), "shelf-settled", cx)
    });
    app.until(cx, |window, _| {
        in_scope(window, "t3-settled", "Beta, Settled").is_some()
            && label_of(window, "shelf-settled").as_deref() == Some("Settled 1")
    })
    .await;

    app.stop(cx);
}

#[gpui_kit::test]
async fn t3_snooze_hides_a_thread_until_it_wakes(cx: &mut TestAppContext) {
    let app = Harness::start("snooze", cx).await;
    let project_id = app.project(cx).await;
    let nap = app.completed(cx, &project_id, "Nap").await;
    app.until(cx, |window, _| {
        in_scope(window, "t3-active", "Nap, Done").is_some()
    })
    .await;

    // Snooze ▸ Custom… for an hour: the row moves to the Snoozed shelf.
    app.update(cx, |window, cx| window.right_click(thread_row(&nap), cx));
    app.until(cx, |window, _| has_label(window, "Snooze")).await;
    app.update(cx, |window, cx| click_label(window, "Snooze", cx));
    app.until(cx, |window, _| has_label(window, "Custom…"))
        .await;
    app.update(cx, |window, cx| click_label(window, "Custom…", cx));
    app.until(cx, |window, _| window.try_find("dialog-confirm").is_some())
        .await;
    app.update(cx, |window, cx| window.click("dialog-confirm", cx));
    app.until(cx, |window, _| {
        in_scope(window, "t3-snoozed", "Nap, Snoozed").is_some()
            && in_scope(window, "t3-active", "Nap, Done").is_none()
    })
    .await;
    app.update(cx, |window, _| {
        assert_eq!(
            label_of(window, "shelf-snoozed").as_deref(),
            Some("Snoozed 1")
        );
    });

    // Wake thread brings it back at once.
    app.update(cx, |window, cx| window.right_click(thread_row(&nap), cx));
    app.until(cx, |window, _| has_label(window, "Wake thread"))
        .await;
    app.update(cx, |window, cx| click_label(window, "Wake thread", cx));
    app.until(cx, |window, _| {
        in_scope(window, "t3-active", "Nap, Done").is_some()
    })
    .await;

    // A short snooze hides the row until its wake time, then the sidebar
    // brings it back by itself, marked as woken.
    let wake = time::OffsetDateTime::now_utc() + time::Duration::seconds(3);
    app.connection.snooze(&nap, wake).await.unwrap();
    app.until(cx, |window, _| {
        in_scope(window, "t3-snoozed", "Nap, Snoozed").is_some()
    })
    .await;
    app.until(cx, |window, _| {
        in_scope(window, "t3-active", "Nap, Woke").is_some()
            && window.try_find("t3-snoozed").is_none()
    })
    .await;
    assert!(time::OffsetDateTime::now_utc() >= wake);

    app.stop(cx);
}

#[gpui_kit::test]
async fn herdr_glyphs_attention_notifications_and_goto(cx: &mut TestAppContext) {
    let app = Harness::start_ui(
        "herdr",
        cx,
        json!({"workflow": "herdr", "agentOrder": "attention", "sounds": false}),
    )
    .await;
    let project_id = app.project(cx).await;
    app.until(cx, |window, _| in_sidebar(window, "Demo, unknown"))
        .await;

    // Idle: finished and read (opened).
    let idle = app.completed(cx, &project_id, "Calm").await;
    app.until(cx, |window, _| in_sidebar(window, "Calm, done"))
        .await;
    app.update(cx, |window, cx| click_sidebar(window, "Calm, done", cx));
    app.until(cx, |window, _| in_sidebar(window, "Calm, idle"))
        .await;

    // Done: finished while another thread is open; it says so.
    let done = app.completed(cx, &project_id, "Finish").await;
    app.until(cx, |window, _| {
        in_sidebar(window, "Finish, done") && has_label(window, "Finish finished")
    })
    .await;

    // Blocked on a question.
    let blocked = app.launch(cx, &project_id, "Ask").await;
    app.script.emit(
        json!({"type": "extension_ui_request", "id": "ui-1", "method": "select",
        "title": "Pick one", "options": ["alpha", "beta"]}),
    );
    app.until(cx, |window, _| {
        in_sidebar(window, "Ask, blocked") && has_label(window, "Ask needs attention")
    })
    .await;

    // Working.
    let working = app.launch(cx, &project_id, "Busy").await;
    app.until(cx, |window, _| in_sidebar(window, "Busy, working"))
        .await;

    // Dots, and attention order: blocked, done, working, idle.
    app.update(cx, |window, _| {
        let glyph = |id: &str| label_of(window, &format!("glyph-local-{id}"));
        assert_eq!(glyph(&blocked).as_deref(), Some("●"));
        assert_eq!(glyph(&done).as_deref(), Some("●"));
        assert_eq!(glyph(&working).as_deref(), Some("●"));
        assert_eq!(glyph(&idle).as_deref(), Some("○"));
        let tops: Vec<_> = [
            "Ask, blocked",
            "Finish, done",
            "Busy, working",
            "Calm, idle",
        ]
        .iter()
        .map(|label| top_of(window, label))
        .collect();
        assert!(tops.windows(2).all(|pair| pair[0] < pair[1]), "{tops:?}");
        // The space rolls up its most urgent agent.
        assert!(in_sidebar(window, "Demo, blocked"));
    });

    // Symbols, from Settings.
    app.update(cx, |window, cx| window.press("secondary-,", cx));
    app.until(cx, |window, _| {
        window.try_find("status-indicators-symbols").is_some()
    })
    .await;
    app.update(cx, |window, cx| {
        window.click("status-indicators-symbols", cx);
        window.click("close-settings", cx);
    });
    app.until(cx, |window, _| {
        label_of(window, &format!("glyph-local-{blocked}")).as_deref() == Some("×")
    })
    .await;
    app.update(cx, |window, _| {
        let glyph = |id: &str| label_of(window, &format!("glyph-local-{id}"));
        assert_eq!(glyph(&done).as_deref(), Some("✓"));
        assert_eq!(glyph(&working).as_deref(), Some("◐"));
        assert_eq!(glyph(&idle).as_deref(), Some("○"));
    });

    // The goto picker: everything, then only what a filter key keeps.
    app.update(cx, |window, cx| window.press("secondary-g", cx));
    app.until(cx, |window, _| {
        in_scope(window, "goto-picker", "Busy, working").is_some()
    })
    .await;
    let picked = |window: &mut Window| {
        [
            "Ask, blocked",
            "Finish, done",
            "Busy, working",
            "Calm, idle",
        ]
        .iter()
        .filter(|label| in_scope(window, "goto-picker", label).is_some())
        .count()
    };
    app.update(cx, |window, cx| {
        assert_eq!(picked(window), 4);
        window.press("b", cx);
        assert_eq!(picked(window), 1);
        assert!(in_scope(window, "goto-picker", "Ask, blocked").is_some());
        window.press("a", cx);
        assert_eq!(picked(window), 4);
        window.press("d", cx);
        assert_eq!(picked(window), 1);
        // Enter jumps; opening the done thread reads it.
        window.press("enter", cx);
    });
    app.until(cx, |window, _| {
        window.try_find("goto-picker").is_none()
            && feed_row(window, "Finish").is_some()
            && in_sidebar(window, "Finish, idle")
    })
    .await;

    // Search narrows by name.
    app.update(cx, |window, cx| window.press("secondary-g", cx));
    app.until(cx, |window, _| window.try_find("goto-picker").is_some())
        .await;
    app.update(cx, |window, cx| {
        window.press("/", cx);
        window.input("busy", cx);
    });
    app.until(cx, |window, _| picked(window) == 1).await;
    app.update(cx, |window, cx| window.press("enter", cx));
    app.until(cx, |window, _| feed_row(window, "Busy").is_some())
        .await;

    // A thread that finishes while open was seen: idle, and no toast.
    app.script.emit_to(
        3,
        json!({"type": "prompt_result", "agentInvoked": true, "status": "completed",
            "sessionSettled": true}),
    );
    app.until(cx, |window, _| in_sidebar(window, "Busy, idle"))
        .await;
    app.update(cx, |window, _| assert!(!has_label(window, "Busy finished")));

    app.stop(cx);
}

#[gpui_kit::test]
async fn new_thread_launches_from_the_composer_and_streams_the_reply(cx: &mut TestAppContext) {
    let app = Harness::start("launch", cx).await;
    let project_id = app.project(cx).await;
    // The composer needs the model catalog the fake omp serves.
    let connection = app.connection.clone();
    app.until(cx, move |_, _| {
        connection
            .state()
            .borrow()
            .config
            .as_ref()
            .is_some_and(|config| {
                config
                    .providers
                    .iter()
                    .any(|provider| !provider.models.is_empty())
            })
    })
    .await;

    // The sidebar's new-thread button opens a new thread in the (only)
    // project, with the composer focused.
    app.update(cx, |window, cx| window.click("new-thread", cx));
    app.until(cx, |window, _| {
        window
            .find_all_by_label("Message")
            .first()
            .is_some_and(|input| input.focused() == Some(true))
    })
    .await;
    app.update(cx, |window, cx| {
        assert!(!stoppable(window));
        // An empty message is not sent.
        window.press("enter", cx);
        window.input("Say hi", cx);
        assert_eq!(window.find_by_label("Message").value(), Some("Say hi"));
        window.press("enter", cx);
    });
    app.until_called(cx, "prompt:Say hi:0").await;
    assert_eq!(
        app.script
            .calls()
            .iter()
            .filter(|call| call.starts_with("prompt:"))
            .count(),
        1
    );

    // The launched thread opens with the first message; the draft clears.
    app.until(cx, |window, _| feed_row(window, "Say hi").is_some())
        .await;
    app.update(cx, |window, _| {
        assert_eq!(window.find_by_label("Message").value(), Some(""));
    });

    // Streaming text reaches the feed piece by piece.
    app.script.emit(json!({"type": "agent_start"}));
    app.until(cx, |window, _| in_sidebar(window, "Say hi, Working"))
        .await;
    app.script
        .text("m1", json!({"type": "text_start", "contentIndex": 0}));
    app.script.text(
        "m1",
        json!({"type": "text_delta", "contentIndex": 0, "delta": "Hel"}),
    );
    app.until(cx, |window, _| feed_row(window, "Hel").is_some())
        .await;
    app.script.text(
        "m1",
        json!({"type": "text_delta", "contentIndex": 0, "delta": "lo!"}),
    );
    app.script.text(
        "m1",
        json!({"type": "text_end", "contentIndex": 0, "content": "Hello!"}),
    );
    app.until(cx, |window, _| {
        feed_row(window, "Hello!").is_some() && feed_row(window, "Hel").is_none()
    })
    .await;
    app.update(cx, |window, _| {
        let reply = feed_row(window, "Hello!").unwrap();
        assert!(reply.bounds().top() > feed_row(window, "Say hi").unwrap().bounds().top());
    });

    app.script.settle("completed");
    app.until(cx, |window, _| {
        in_sidebar(window, "Say hi") && !stoppable(window)
    })
    .await;
    // The launch carried the composer's model (the one the fake omp lists).
    let state = app.connection.state();
    let state = state.borrow();
    let thread = state.shell.threads_of(&project_id)[0];
    assert_eq!(thread.model_selection.instance_id, "omp");
    assert_eq!(thread.model_selection.model, MODEL);
    drop(state);

    app.stop(cx);
}

/// Opens a new thread in a fresh project once the model catalog has loaded,
/// with the composer focused.
async fn focused_new_thread_composer(app: &Harness, cx: &mut TestAppContext) {
    app.project(cx).await;
    let connection = app.connection.clone();
    app.until(cx, move |_, _| {
        connection
            .state()
            .borrow()
            .config
            .as_ref()
            .is_some_and(|config| {
                config
                    .providers
                    .iter()
                    .any(|provider| !provider.models.is_empty())
            })
    })
    .await;
    app.update(cx, |window, cx| window.click("new-thread", cx));
    app.until(cx, |window, _| {
        window
            .find_all_by_label("Message")
            .first()
            .is_some_and(|input| input.focused() == Some(true))
    })
    .await;
}

/// The composer's draft text.
fn draft(window: &Window) -> String {
    window
        .find_by_label("Message")
        .value()
        .unwrap_or_default()
        .to_owned()
}

#[gpui_kit::test]
async fn a_caret_mention_completes_to_a_model_chip_and_sends_its_selector(cx: &mut TestAppContext) {
    let app = Harness::start("mention", cx).await;
    focused_new_thread_composer(&app, cx).await;
    let name = {
        let state = app.connection.state();
        let state = state.borrow();
        let config = state.config.as_ref().expect("the model catalog");
        models::model_choices(config)
            .iter()
            .find(|choice| choice.slug() == MODEL)
            .map(mention::display_name)
            .expect("the fake omp's model")
    };
    let row = format!("completion-{MODEL}");
    let chip = format!("model:{MODEL}");

    app.update(cx, |window, cx| window.input("Have ^", cx));
    app.until(cx, |window, _| window.try_find(row.clone()).is_some())
        .await;
    app.update(cx, |window, cx| window.press("enter", cx));
    app.until(cx, |window, _| window.try_find(chip.clone()).is_some())
        .await;
    app.update(cx, |window, _| {
        assert_eq!(draft(window), format!("Have ^{MODEL} "));
        assert_eq!(label_of(window, &chip).as_deref(), Some(name.as_str()));
        assert!(window.try_find(row.clone()).is_none());
    });

    app.update(cx, |window, cx| window.press("enter", cx));
    app.until_called(cx, &format!("prompt:Have ^{MODEL}:0"))
        .await;
    app.stop(cx);
}

#[gpui_kit::test]
async fn a_magic_keyword_becomes_a_token_once_a_delimiter_follows_and_stops_when_extended(
    cx: &mut TestAppContext,
) {
    let app = Harness::start("keyword", cx).await;
    focused_new_thread_composer(&app, cx).await;
    let keyword_tokens = |window: &Window| window.find_all_by_label("orchestrate").len();

    // At the end of the draft the word stays plain text.
    app.update(cx, |window, cx| window.input("please orchestrate", cx));
    app.update(cx, |window, _| {
        assert_eq!(draft(window), "please orchestrate");
        assert_eq!(keyword_tokens(window), 0);
    });

    // Once a character follows it, the word is one token and the text is
    // untouched.
    app.update(cx, |window, cx| window.input(" this", cx));
    app.until(cx, |window, _| keyword_tokens(window) == 1).await;
    app.update(cx, |window, _| {
        assert!(window.try_find("kw:orchestrate").is_some());
        assert_eq!(draft(window), "please orchestrate this");
    });

    // Typing straight after the word makes it `orchestrated`: plain again.
    app.update(cx, |window, cx| window.input(" orchestrate ", cx));
    app.until(cx, |window, _| keyword_tokens(window) == 2).await;
    app.update(cx, |window, cx| {
        // Back over the space: the caret sits at the end of the word's token.
        window.press("left", cx);
        window.input("d", cx);
    });
    app.until(cx, |window, _| {
        draft(window) == "please orchestrate this orchestrated "
    })
    .await;
    app.until(cx, |window, _| keyword_tokens(window) == 1).await;
    app.update(cx, |window, _| {
        assert_eq!(draft(window), "please orchestrate this orchestrated ");
    });
    app.stop(cx);
}

#[gpui_kit::test]
async fn the_composer_offers_omps_word_prediction_as_ghost_text(cx: &mut TestAppContext) {
    let app = Harness::start("predict", cx).await;
    focused_new_thread_composer(&app, cx).await;
    let feedback = app.data_dir.join("predict-feedback.log");
    let feedback = move || std::fs::read_to_string(&feedback).unwrap_or_default();
    let ghost = |window: &Window| label_of(window, "composer-ghost");

    // omp's suggestion follows the caret; Tab takes it with a space.
    app.update(cx, |window, cx| window.input("please refac", cx));
    app.until(cx, |window, _| ghost(window).as_deref() == Some("tor"))
        .await;
    app.update(cx, |window, cx| window.press("tab", cx));
    app.until(cx, |window, _| draft(window) == "please refactor ")
        .await;
    app.update(cx, |window, _| assert_eq!(ghost(window), None));
    let log = feedback.clone();
    app.until(cx, move |_, _| log().contains(r#""accepted":true"#))
        .await;
    assert!(feedback().contains(r#""suggestion":"tor""#));

    // That space is provisional: punctuation takes its place.
    app.update(cx, |window, cx| window.input(",", cx));
    app.until(cx, |window, _| draft(window) == "please refactor,")
        .await;

    // Typing through the suggestion keeps the rest; → takes it without a
    // space.
    app.update(cx, |window, cx| window.input(" refac", cx));
    app.until(cx, |window, _| ghost(window).as_deref() == Some("tor"))
        .await;
    app.update(cx, |window, cx| window.input("t", cx));
    app.update(cx, |window, _| {
        assert_eq!(ghost(window).as_deref(), Some("or"))
    });
    app.update(cx, |window, cx| window.press("right", cx));
    app.until(cx, |window, _| draft(window) == "please refactor, refactor")
        .await;

    // Typing past it rejects it.
    app.update(cx, |window, cx| window.input(" refac", cx));
    app.until(cx, |window, _| ghost(window).as_deref() == Some("tor"))
        .await;
    app.update(cx, |window, cx| window.input("e", cx));
    app.update(cx, |window, _| assert_eq!(ghost(window), None));
    let log = feedback.clone();
    app.until(cx, move |_, _| log().contains(r#""accepted":false"#))
        .await;
    app.stop(cx);
}

#[gpui_kit::test]
async fn the_microphone_dictates_into_the_draft(cx: &mut TestAppContext) {
    let app = Harness::start("dictate", cx).await;
    focused_new_thread_composer(&app, cx).await;

    // The transcript lands at the caret, a space after the draft's word,
    // and is replaced as it grows.
    app.update(cx, |window, cx| {
        assert_eq!(label_of(window, "dictate").as_deref(), Some("Dictate"));
        window.input("Note:", cx);
        window.click("dictate", cx);
    });
    app.until(cx, |window, _| {
        label_of(window, "dictate").as_deref() == Some("Stop dictation")
            && draft(window) == "Note: hello"
    })
    .await;
    // Nothing is sent while the words are still coming.
    app.update(cx, |window, cx| window.press("enter", cx));

    app.update(cx, |window, cx| window.click("dictate", cx));
    app.until(cx, |window, _| {
        label_of(window, "dictate").as_deref() == Some("Dictate")
            && draft(window) == "Note: hello there"
    })
    .await;
    assert!(
        !app.script
            .calls()
            .iter()
            .any(|call| call.starts_with("prompt:"))
    );

    // Then the draft sends as typed.
    app.update(cx, |window, cx| window.press("enter", cx));
    app.until_called(cx, "prompt:Note: hello there:0").await;
    app.stop(cx);
}

#[gpui_kit::test]
async fn a_bang_draft_shows_no_model_menu_and_no_model_chip(cx: &mut TestAppContext) {
    let app = Harness::start("bang", cx).await;
    focused_new_thread_composer(&app, cx).await;
    let text = format!("!ls ^{MODEL} ");

    app.update(cx, |window, cx| window.input(&text, cx));
    app.until(cx, |window, _| draft(window) == text).await;
    cx.executor().timer(Duration::from_millis(300)).await;
    app.update(cx, |window, _| {
        assert_eq!(draft(window), text);
        assert!(window.try_find(format!("completion-{MODEL}")).is_none());
        assert!(window.try_find(format!("model:{MODEL}")).is_none());
    });
    app.stop(cx);
}

#[gpui_kit::test]
async fn a_pending_question_is_answered_from_its_card(cx: &mut TestAppContext) {
    let app = Harness::start("question", cx).await;
    let project_id = app.project(cx).await;
    let thread_id = app.open_running_thread(cx, &project_id, "Ask me").await;
    app.update(cx, |window, _| {
        assert!(window.try_find("question-card").is_none())
    });

    app.script.emit(
        json!({"type": "extension_ui_request", "id": "ui-1", "method": "select",
        "title": "Pick one", "options": ["alpha", "beta"]}),
    );
    app.until(cx, |window, _| {
        window.try_find("question-card").is_some() && has_label(window, "beta")
    })
    .await;
    app.update(cx, |window, _| {
        assert!(in_sidebar(window, "Ask me, Input"));
        assert!(has_label(window, "alpha"));
    });

    app.update(cx, |window, cx| {
        click_label(window, "beta", cx);
        assert_eq!(window.find_by_label("beta").checked(), Some(true));
        assert_eq!(window.find_by_label("alpha").checked(), Some(false));
    });
    // Choosing the answer of the last (only) question confirms the
    // questionnaire once the selection has shown, which submits it.
    let script = Arc::clone(&app.script);
    app.until(cx, move |_, _| {
        !script.ui_responses.lock().unwrap().is_empty()
    })
    .await;
    let responses = app.script.ui_responses.lock().unwrap().clone();
    assert_eq!(
        serde_json::to_value(&responses[0]).unwrap(),
        json!({"type": "extension_ui_response", "id": "ui-1", "value": "beta"})
    );
    app.until_thread(cx, &thread_id, |state| {
        state.pending_user_inputs().is_empty()
            && state
                .active_run()
                .is_some_and(|run| run.status == RunStatus::Running)
    })
    .await;
    app.until(cx, |window, _| {
        window.try_find("question-card").is_none() && in_sidebar(window, "Ask me, Working")
    })
    .await;
    assert_eq!(
        app.script.ui_responses.lock().unwrap().len(),
        1,
        "answered once"
    );

    app.stop(cx);
}

#[gpui_kit::test]
async fn enter_queues_while_running_and_the_queued_message_can_steer(cx: &mut TestAppContext) {
    let app = Harness::start("steer", cx).await;
    let project_id = app.project(cx).await;
    let thread_id = app.open_running_thread(cx, &project_id, "first").await;

    // One button: Stop with nothing typed, Queue once there is a draft.
    app.click_composer(cx);
    app.update(cx, |window, cx| {
        assert_eq!(window.find("send-message").label(), Some("Stop"));
        // Shift+Enter breaks the line and sends nothing.
        window.input("queue me", cx);
        assert_eq!(window.find("send-message").label(), Some("Queue"));
        window.press("shift-enter", cx);
        assert_eq!(window.find_by_label("Message").value(), Some("queue me\n"));
        // Enter queues the (trimmed) draft; it does not steer the running turn.
        window.press("enter", cx);
    });
    app.until_thread(cx, &thread_id, |state| state.queued_runs().len() == 1)
        .await;
    let queued = {
        let handle = app.connection.thread(&thread_id);
        let state = handle.state();
        let state = state.borrow();
        let queued = &state.queued_runs()[0];
        assert_eq!(queued.text, "queue me");
        queued.run.id.clone()
    };
    app.update(cx, |window, _| {
        assert_eq!(window.find_by_label("Message").value(), Some(""));
    });
    let row = format!("queued-{queued}");
    app.until(cx, |window, _| {
        label_of(window, &row).as_deref() == Some("queue me")
    })
    .await;
    app.update(cx, |window, _| {
        assert!(window.try_find(format!("send-now-{queued}")).is_some());
        assert!(window.try_find(format!("steer-{queued}")).is_some());
        // The draft is gone, so the button offers Stop again.
        assert_eq!(window.find("send-message").label(), Some("Stop"));
    });
    assert!(!app.script.called("steer:queue me"));
    assert!(!app.script.called("prompt:queue me:0"));

    // Steer on the queued message sends it into the running turn.
    app.update(cx, |window, cx| window.click(format!("steer-{queued}"), cx));
    app.until_called(cx, "steer:queue me").await;
    app.until_thread(cx, &thread_id, |state| {
        state.queued_runs().is_empty()
            && state.feed().iter().any(|row| matches!(&row.item.kind,
                TurnItemKind::UserMessage(m) if m.input_intent == UserMessageInputIntent::PromotedQueuedToSteer && m.text == "queue me"))
    })
    .await;

    app.stop(cx);
}

#[gpui_kit::test]
async fn stop_interrupts_the_run(cx: &mut TestAppContext) {
    let app = Harness::start("stop", cx).await;
    let project_id = app.project(cx).await;
    let thread_id = app.open_running_thread(cx, &project_id, "long task").await;

    // The Stop button.
    app.update(cx, |window, cx| window.click("send-message", cx));
    app.until_called(cx, "abort").await;
    app.until_thread(cx, &thread_id, |state| state.active_run().is_none())
        .await;
    app.until(cx, |window, _| {
        !stoppable(window)
            && window.find("send-message").label() == Some("Send")
            && in_sidebar(window, "long task")
    })
    .await;
    app.update(cx, |window, _| assert!(in_sidebar(window, "long task")));

    // A new message starts another run; the StopRun shortcut stops it too.
    app.click_composer(cx);
    app.update(cx, |window, cx| {
        window.input("again", cx);
        window.press("enter", cx);
    });
    app.until_called(cx, "prompt:again:0").await;
    app.script.emit(json!({"type": "agent_start"}));
    app.until(cx, |window, _| stoppable(window)).await;
    app.update(cx, |window, cx| window.press("secondary-.", cx));
    app.until(cx, |window, _| !stoppable(window)).await;
    assert_eq!(
        app.script
            .calls()
            .iter()
            .filter(|call| *call == "abort")
            .count(),
        2
    );
    app.until_thread(cx, &thread_id, |state| {
        state.active_run().is_none()
            && state.projection.as_ref().is_some_and(|p| {
                p.runs
                    .iter()
                    .filter(|run| run.status == RunStatus::Interrupted)
                    .count()
                    == 2
            })
    })
    .await;

    app.stop(cx);
}

/// The visible text of terminal `id` (its accessibility value).
fn terminal_text(window: &Window, id: &str) -> String {
    window
        .try_find(format!("terminal-{id}"))
        .and_then(|terminal| terminal.value().map(str::to_owned))
        .unwrap_or_default()
}

#[gpui_kit::test]
async fn the_terminal_drawer_runs_a_shell_on_the_server(cx: &mut TestAppContext) {
    let app = Harness::start("terminal", cx).await;
    let project_id = app.project(cx).await;
    app.open_running_thread(cx, &project_id, "shell").await;

    // ctrl-` opens the drawer with the focus in its first terminal, whose
    // shell runs on the server and answers typed input.
    app.update(cx, |window, cx| window.press("ctrl-`", cx));
    app.until(cx, |window, _| {
        window
            .try_find("terminal-term-1")
            .is_some_and(|terminal| terminal.focused() == Some(true))
    })
    .await;
    app.update(cx, |window, cx| {
        window.input("echo hi-$((40+2))", cx);
        window.press("enter", cx);
    });
    app.until(cx, |window, _| {
        terminal_text(window, "term-1").contains("hi-42")
    })
    .await;

    // The header button hides it and the composer gets the focus back.
    app.update(cx, |window, cx| window.click("toggle-terminal", cx));
    app.until(cx, |window, _| window.try_find("terminal-drawer").is_none())
        .await;
    app.update(cx, |window, _| {
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
    });

    // Reopened, the same shell is there with its output.
    app.update(cx, |window, cx| window.press("ctrl-`", cx));
    app.until(cx, |window, _| {
        terminal_text(window, "term-1").contains("hi-42")
    })
    .await;

    // `+` opens a second terminal; its × ends it.
    app.update(cx, |window, cx| window.click("terminal-new", cx));
    app.until(cx, |window, _| {
        window
            .try_find("terminal-term-2")
            .is_some_and(|terminal| terminal.focused() == Some(true))
    })
    .await;
    app.update(cx, |window, cx| window.click("terminal-close-term-2", cx));
    app.until(cx, |window, _| {
        window.try_find("terminal-tab-term-2").is_none()
            && window.try_find("terminal-term-1").is_some()
    })
    .await;

    // An exited shell shows why, and Restart starts a new one.
    app.update(cx, |window, cx| {
        window.click("terminal-term-1", cx);
        window.input("exit 7", cx);
        window.press("enter", cx);
    });
    app.until(cx, |window, _| {
        window
            .try_find("terminal-status")
            .is_some_and(|status| status.label() == Some("Shell exited with code 7"))
    })
    .await;
    app.update(cx, |window, cx| window.click("terminal-restart", cx));
    app.until(cx, |window, _| window.try_find("terminal-status").is_none())
        .await;
    app.update(cx, |window, cx| {
        window.input("echo back-$((1+1))", cx);
        window.press("enter", cx);
    });
    app.until(cx, |window, _| {
        terminal_text(window, "term-1").contains("back-2")
    })
    .await;

    app.stop(cx);
}

#[gpui_kit::test]
async fn goal_chip_controls_and_subagent_progress(cx: &mut TestAppContext) {
    let app = Harness::start("extras", cx).await;
    let project_id = app.project(cx).await;
    app.open_running_thread(cx, &project_id, "delegate").await;

    // Set a goal from the thread-actions menu and its dialog.
    app.update(cx, |window, cx| {
        assert!(window.try_find("goal").is_none());
        window.click("thread-actions", cx);
    });
    app.until(cx, |window, _| has_label(window, "Set goal…"))
        .await;
    app.update(cx, |window, cx| click_label(window, "Set goal…", cx));
    app.until(cx, |window, _| window.try_find("dialog-confirm").is_some())
        .await;
    app.update(cx, |window, cx| {
        window.input("Ship the release", cx);
        window.click("dialog-confirm", cx);
    });
    app.until(cx, |window, _| {
        label_of(window, "goal").as_deref() == Some("Ship the release, Active")
    })
    .await;
    app.update(cx, |window, _| {
        assert!(window.try_find("goal-resume").is_none());
    });

    // Pause, resume, drop.
    app.update(cx, |window, cx| window.click("goal-pause", cx));
    app.until(cx, |window, _| {
        label_of(window, "goal").as_deref() == Some("Ship the release, Paused")
            && window.try_find("goal-resume").is_some()
    })
    .await;
    app.update(cx, |window, cx| window.click("goal-resume", cx));
    app.until(cx, |window, _| {
        label_of(window, "goal").as_deref() == Some("Ship the release, Active")
    })
    .await;
    app.update(cx, |window, cx| window.click("goal-drop", cx));
    app.until(cx, |window, _| window.try_find("goal").is_none())
        .await;
    assert_eq!(
        app.script
            .calls()
            .iter()
            .filter(|call| call.starts_with("goal:"))
            .collect::<Vec<_>>(),
        ["goal:create", "goal:pause", "goal:resume", "goal:drop"]
    );

    // A subagent row shows live progress, then its result.
    let script = &app.script;
    script.emit(
        json!({"type": "tool_execution_start", "toolCallId": "call_task", "toolName": "task",
        "args": {"tasks": [{"agent": "task", "task": "Reply PONG."}]}}),
    );
    script.emit(json!({"type": "tool_execution_end", "toolCallId": "call_task", "toolName": "task",
        "result": {"content": [{"type": "text", "text": "Spawned agent `Pinger`."}]}, "isError": false}));
    script.subagent_lifecycle("Pinger", "started");
    script.subagent_progress("Pinger", "running", &[], 1200);
    app.until(cx, |window, _| {
        label_of(window, "subagent-Pinger").as_deref()
            == Some("Answer PONG, Running, Thinking about PONG · 1.2k tokens")
    })
    .await;
    app.update(cx, |window, _| {
        assert!(window.try_find("stop-subagent-Pinger").is_some());
    });

    script.subagent_progress("Pinger", "completed", &["PONG"], 2400);
    script.subagent_lifecycle("Pinger", "completed");
    app.until(cx, |window, _| {
        label_of(window, "subagent-Pinger").as_deref()
            == Some("Answer PONG, Done, PONG, from the transcript")
    })
    .await;
    app.update(cx, |window, _| {
        assert!(window.try_find("stop-subagent-Pinger").is_none());
    });

    // ←← in the empty composer opens the agents hub with the transcript
    // omp wrote next to the thread's session; Esc closes it.
    let dir = Path::new("/tmp/j3-desktop-fake-session");
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("Pinger.jsonl"),
        concat!(
            r#"{"type":"message","message":{"role":"user","content":"Reply PONG."}}"#,
            "\n",
            r#"{"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","id":"c1","name":"bash","arguments":{"command":"echo PONG"}}]}}"#,
            "\n",
        ),
    )
    .unwrap();
    app.click_composer(cx);
    app.update(cx, |window, cx| window.press("left", cx));
    std::thread::sleep(std::time::Duration::from_millis(60));
    app.update(cx, |window, cx| window.press("left", cx));
    app.until(cx, |window, _| window.try_find("agent-hub").is_some())
        .await;
    app.until(cx, |window, _| {
        label_of(window, "hub-agent-Pinger").as_deref() == Some("Pinger, Answer PONG, Done")
    })
    .await;
    app.until(cx, |window, _| {
        window.try_find("hub-entry-Pinger/1").is_some()
    })
    .await;
    app.update(cx, |window, cx| {
        assert!(window.try_find("composer").is_none());
        window.press("escape", cx);
    });
    app.until(cx, |window, _| {
        window.try_find("agent-hub").is_none() && window.try_find("composer").is_some()
    })
    .await;

    app.stop(cx);
}

#[gpui_kit::test]
async fn a_remote_that_is_the_local_server_is_listed_once(cx: &mut TestAppContext) {
    // A second, separate server for an "other" remote.
    let other_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let other_dir = temp_dir("dedupe-other");
    let other_omp = other_dir.join("fake-omp");
    std::fs::write(&other_omp, fake_omp_script()).unwrap();
    let (other_server, other_port) = serve(&other_runtime, &other_dir, &other_omp, Arc::default());
    let other = LocalServer::from_server(other_server);

    // "twin" reaches the local server under another host name.
    let app = Harness::start_with(
        "dedupe",
        cx,
        |port| {
            json!([
                {"label": "twin", "url": format!("http://localhost:{port}")},
                {"label": "other", "url": format!("http://127.0.0.1:{other_port}")},
            ])
        },
        json!({"workflow": "herdr", "sounds": false}),
    )
    .await;

    let connections = cx.update(|cx| Workspace::global_connections(cx));
    assert_eq!(connections.len(), 3, "local, twin and other connect");
    let live = connections.clone();
    app.until(cx, move |_, _| {
        live.iter().all(|connection| {
            let state = connection.state();
            let state = state.borrow();
            state.status == ConnectionStatus::Live && state.shell.loaded
        })
    })
    .await;
    // A project on each server: the local one is listed once, not again
    // under the twin; the other server's after it.
    app.project(cx).await;
    connections[2]
        .create_project("Remote", &app.project_dir.to_string_lossy())
        .await
        .unwrap();
    app.until(cx, |window, _| {
        in_sidebar(window, "Demo, unknown") && in_sidebar(window, "Remote, unknown")
    })
    .await;
    app.update(cx, |window, _| {
        let demo = in_scope(window, "herdr-spaces", "Demo, unknown");
        assert_eq!(
            window
                .within("herdr-spaces")
                .find_all_by_label("Demo, unknown")
                .len(),
            1
        );
        let remote = in_scope(window, "herdr-spaces", "Remote, unknown").unwrap();
        assert!(remote.bounds().top() > demo.unwrap().bounds().top());
    });

    // Hidden, not forgotten: the config entry stays.
    let config: Value = serde_json::from_str(
        &std::fs::read_to_string(app.config_dir.join("desktop.json")).unwrap(),
    )
    .unwrap();
    let labels: Vec<&str> = config["environments"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|env| env["label"].as_str())
        .collect();
    assert_eq!(labels, ["twin", "other"]);

    app.stop(cx);
    other_runtime.block_on(other.shutdown());
    other_runtime.shutdown_background();
    let _ = std::fs::remove_dir_all(other_dir);
}

/// Runs `git` in `dir`; returns its trimmed stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[gpui_kit::test]
async fn the_header_commits_uncommitted_changes(cx: &mut TestAppContext) {
    let app = Harness::start("git", cx).await;
    // A repository without a remote, with one uncommitted file.
    git(&app.project_dir, &["init", "-q"]);
    git(&app.project_dir, &["config", "user.name", "j3 test"]);
    git(
        &app.project_dir,
        &["config", "user.email", "test@j3.invalid"],
    );
    std::fs::write(app.project_dir.join("notes.txt"), "one\ntwo\n").unwrap();
    let project_id = app.project(cx).await;
    app.open_running_thread(cx, &project_id, "write notes")
        .await;

    // Changes and no remote: the quick action commits.
    app.until(cx, |window, _| {
        label_of(window, "git-quick-action").as_deref() == Some("Commit")
    })
    .await;

    // Commit… opens the dialog listing the file; a typed message is used as is.
    app.update(cx, |window, cx| click_label(window, "Git actions", cx));
    app.until(cx, |window, _| has_label(window, "Commit…"))
        .await;
    app.update(cx, |window, cx| click_label(window, "Commit…", cx));
    app.until(cx, |window, _| window.try_find("git-commit").is_some())
        .await;
    app.update(cx, |window, cx| {
        assert!(window.try_find("git-commit-files").is_some());
        window.input("Add notes", cx);
        window.click("git-commit", cx);
    });
    let dir = app.project_dir.clone();
    app.until(cx, move |_, _| {
        std::process::Command::new("git")
            .args(["log", "-1", "--format=%s"])
            .current_dir(&dir)
            .output()
            .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "Add notes")
    })
    .await;
    assert_eq!(git(&app.project_dir, &["status", "--porcelain"]), "");
    // The action ended: the button is back, with nothing left to commit.
    app.until(cx, |window, _| {
        label_of(window, "git-quick-action").as_deref() == Some("Commit")
            && !has_label(window, "Commit…")
    })
    .await;

    app.stop(cx);
}

#[gpui_kit::test]
async fn the_file_pane_browses_and_previews_the_project(cx: &mut TestAppContext) {
    let app = Harness::start("files", cx).await;
    let dir = &app.project_dir;
    git(dir, &["init", "-q"]);
    std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("target")).unwrap();
    std::fs::write(
        dir.join("src/main.rs"),
        "fn main() {\n    println!(\"hi\");\n}\n",
    )
    .unwrap();
    std::fs::write(dir.join("logo.png"), b"\x89PNG\r\n\x1a\n\0\0").unwrap();
    let project_id = app.project(cx).await;
    app.open_running_thread(cx, &project_id, "browse files")
        .await;

    // The header button opens the pane on the project root, focused, with
    // directories first.
    app.update(cx, |window, cx| window.click("toggle-files", cx));
    app.until(cx, |window, _| {
        has_label(window, "target") && has_label(window, "logo.png")
    })
    .await;
    app.update(cx, |window, _| {
        assert_eq!(window.find("file-tree").focused(), Some(true));
        let top = |label: &str| window.find_by_label(label).bounds().top();
        assert!(top("src") < top("target"));
        assert!(top("target") < top(".gitignore"));
        assert!(top(".gitignore") < top("logo.png"));
    });

    // A click expands a directory; its entries load beneath it.
    app.update(cx, |window, cx| click_label(window, "src", cx));
    app.until(cx, |window, _| has_label(window, "src/main.rs"))
        .await;

    // ↓ moves to the file and Enter previews it, line numbers and all.
    app.update(cx, |window, cx| {
        window.press("down", cx);
        window.press("enter", cx);
    });
    app.until(cx, |window, _| {
        window
            .try_find_by_label("Contents of src/main.rs")
            .is_some_and(|editor| {
                editor
                    .value()
                    .is_some_and(|text| text.contains("println!(\"hi\")"))
            })
    })
    .await;

    // A binary file gets a notice instead of a preview.
    app.update(cx, |window, cx| click_label(window, "logo.png", cx));
    app.until(cx, |window, _| {
        has_label(window, "Binary file; there is no preview.")
    })
    .await;

    // The shortcut's action closes the pane and returns to the composer.
    app.update(cx, |window, cx| {
        window.dispatch_action(Box::new(actions::ToggleFiles), cx)
    });
    app.until(cx, |window, _| window.try_find("file-pane").is_none())
        .await;
    app.update(cx, |window, _| {
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
    });
    app.stop(cx);
}

/// A 1×1 PNG.
const PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

#[gpui_kit::test]
async fn tool_and_host_images_come_from_the_threads_server(cx: &mut TestAppContext) {
    use base64::Engine as _;
    use j3_client::t3::assets::AssetResource;

    let app = Harness::start("images", cx).await;
    let png = base64::engine::general_purpose::STANDARD
        .decode(PNG_BASE64)
        .unwrap();
    std::fs::write(app.project_dir.join("plot.png"), &png).unwrap();
    std::fs::write(app.project_dir.join("notes.txt"), "text").unwrap();
    let project_id = app.project(cx).await;
    let thread_id = app.open_running_thread(cx, &project_id, "look").await;

    // A tool that returns an image: the row draws it, fetched from the
    // server as a `tool-output-image` asset.
    app.script
        .emit(json!({"type": "tool_execution_start", "toolCallId": "c1",
        "toolName": "read", "args": {"path": "plot.png"}}));
    app.script.emit(
        json!({"type": "tool_execution_end", "toolCallId": "c1", "toolName": "read",
        "result": {"content": [
            {"type": "text", "text": "Read image file [image/png]"},
            {"type": "image", "data": PNG_BASE64, "mimeType": "image/png"},
        ]}, "isError": false}),
    );
    app.until(cx, |window, _| has_label(window, "Tool output image"))
        .await;

    // Markdown images name files on the thread's host (`workspace-file`):
    // images in the project are served; other files and escapes are not.
    let fetch = |path: &str| {
        let connection = app.connection.clone();
        let resource = AssetResource::WorkspaceFile {
            thread_id: thread_id.clone(),
            path: path.to_owned(),
        };
        app.runtime.as_ref().unwrap().spawn(async move {
            let url = connection.resource_url(resource).await.ok()?;
            let response = reqwest::get(url).await.ok()?.error_for_status().ok()?;
            Some(response.bytes().await.ok()?.to_vec())
        })
    };
    assert_eq!(fetch("plot.png").await.unwrap(), Some(png));
    assert_eq!(fetch("notes.txt").await.unwrap(), None);
    assert_eq!(fetch("../plot.png").await.unwrap(), None);
    app.stop(cx);
}

#[gpui_kit::test]
async fn the_feed_keeps_following_the_reply_after_a_wheel_scroll_at_the_bottom(
    cx: &mut TestAppContext,
) {
    let app = Harness::start("follow", cx).await;
    let project_id = app.project(cx).await;
    app.open_running_thread(cx, &project_id, "long").await;
    app.script.emit(json!({"type": "agent_start"}));
    app.script
        .text("m1", json!({"type": "text_start", "contentIndex": 0}));
    // The reply overflows the feed, which follows its bottom edge.
    stream_paragraphs(&app, 0..30, cx).await;
    app.until(cx, |window, _| {
        reply_row(window).is_some_and(|row| row.bounds().top() < px(0.))
    })
    .await;
    let (top, pinned) = app.update(cx, |window, _| {
        let bounds = reply_row(window).unwrap().bounds();
        (bounds.top(), bounds.bottom())
    });

    // A wheel scroll down at the bottom, as a trackpad flick ending there,
    // over the visible end of the reply.
    app.update(cx, |window, cx| {
        let bounds = reply_row(window).unwrap().bounds();
        let position = gpui_kit::point(bounds.center().x, bounds.bottom() - px(40.));
        window.pointer_move(position, None, cx);
        window.dispatch_event(
            gpui_kit::PlatformInput::Scroll(gpui_kit::ScrollEvent {
                position,
                delta: gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-80.))),
                ..Default::default()
            }),
            cx,
        );
    });

    // More text: once it is laid out and the edge bounce has settled, the
    // reply's bottom edge is in view where it was.
    stream_paragraphs(&app, 30..40, cx).await;
    app.until(cx, |window, _| {
        reply_row(window).is_some_and(|row| {
            row.bounds().top() < top - px(200.) && row.bounds().bottom() == pinned
        })
    })
    .await;
    app.stop(cx);
}

/// Streams `Paragraph {i}: …` for each `i` into reply `m1` and waits until
/// the feed shows the last one.
async fn stream_paragraphs(app: &Harness, range: std::ops::Range<usize>, cx: &mut TestAppContext) {
    let marker = format!("Paragraph {}:", range.end - 1);
    for i in range {
        let delta = format!(
            "Paragraph {i}: enough words to wrap across the column at least once or twice.\n\n"
        );
        app.script.text(
            "m1",
            json!({"type": "text_delta", "contentIndex": 0, "delta": delta}),
        );
    }
    app.until(cx, move |window, _| {
        reply_row(window).is_some_and(|row| row.label().unwrap().contains(&marker))
    })
    .await;
}

/// The streaming reply of the follow test (the row labelled with its text).
fn reply_row(window: &Window) -> Option<ElementSnapshot> {
    let feed = ElementId::Name("feed".into());
    window.elements().into_iter().find(|row| {
        row.path().contains(&feed)
            && row
                .label()
                .is_some_and(|label| label.starts_with("Paragraph 0:"))
    })
}
