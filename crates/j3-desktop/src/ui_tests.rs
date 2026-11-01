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
    ElementSnapshot, TestAppContextExt as _, TestEventExt as _, TestQueryExt as _, TestWindowExt as _,
};
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, ElementId, MouseButton, Role, TestAppContext,
    Window, WindowBounds, WindowOptions, px, size,
};
use j3_client::{
    Connection, ConnectionStatus, ThreadState,
    t3::{
        model::ModelSelection,
        orchestration::{RunStatus, TurnItemKind, UserMessageInputIntent},
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
    actions, local_server::LocalServer, services::Services, workspace::Workspace,
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

    fn called(&self, call: &str) -> bool {
        self.calls.lock().unwrap().iter().any(|c| c == call)
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
            *script.events.lock().unwrap() = Some(tx);
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

/// A utility omp (`--no-session`) for the server's model catalog: `ready`,
/// then one canned response per command.
fn fake_omp_script() -> String {
    let model = model_info().to_string();
    format!(
        r#"#!/bin/sh
printf '%s\n' '{{"type":"ready"}}'
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"type":"get_available_models"'*) command=get_available_models; data='{{"models":[{model}]}}' ;;
    *'"type":"get_state"'*) command=get_state; data='{{"sessionId":"utility","model":{model},"thinkingLevel":"low"}}' ;;
    *'"type":"get_available_commands"'*) command=get_available_commands; data='{{"commands":[]}}' ;;
    *'"type":"set_ask_dialog"'*) command=set_ask_dialog; data='{{"enabled":true}}' ;;
    *) command=unknown; data='{{}}' ;;
  esac
  printf '{{"type":"response","command":"%s","success":true,"id":"%s","data":%s}}\n' "$command" "$id" "$data"
done
"#
    )
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

impl Harness {
    /// Serves a fresh j3 server and opens the workspace window on it, with
    /// the local connection live.
    async fn start(name: &str, cx: &mut TestAppContext) -> Self {
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
        let config_path = config_dir.join("desktop.json");
        std::fs::write(&config_path, r#"{"environments":[]}"#).unwrap();

        let script = Arc::new(Script::default());
        let factory = Arc::new(FakeFactory(Arc::clone(&script)));
        let server = runtime.block_on(async {
            let app = CoreApp::open_with(
                AppConfig {
                    data_dir: data_dir.clone(),
                    label: "test-host".into(),
                    omp: OmpLauncher::new(&omp_path),
                },
                factory,
                Duration::from_secs(600),
            )
            .await
            .unwrap();
            // `Server::bind` reports the requested address, so pick a free port first.
            let addr: SocketAddr = std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap();
            Server::bind(app, &[addr]).await.unwrap()
        });
        let local = Arc::new(LocalServer::from_server(server));
        let services = Services::new(runtime.handle().clone(), local, Some(config_path));

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
    async fn until(&self, cx: &mut TestAppContext, condition: impl FnMut(&mut Window, &mut App) -> bool) {
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
        self.until(cx, move |_, _| check(&handle.state().borrow())).await;
    }

    fn update<R>(&self, cx: &mut TestAppContext, f: impl FnOnce(&mut Window, &mut App) -> R) -> R {
        cx.update_window(self.window, |_, window, cx| f(window, cx))
            .expect("the test window is open")
    }

    /// Creates the project "Demo" through the connection and waits for its
    /// sidebar row.
    async fn project(&self, cx: &mut TestAppContext) -> String {
        let project = self
            .connection
            .create_project("Demo", &self.project_dir.to_string_lossy())
            .await
            .unwrap();
        self.until(cx, |window, _| has_label(window, "Demo")).await;
        project.id
    }

    /// Launches a thread through the connection, starts its run and opens
    /// it from the sidebar; returns its id.
    async fn open_running_thread(&self, cx: &mut TestAppContext, project_id: &str, text: &str) -> String {
        let thread_id = self
            .connection
            .launch(project_id, text, Vec::new(), model())
            .await
            .unwrap();
        self.until_called(cx, &format!("prompt:{text}:0")).await;
        self.script.emit(json!({"type": "agent_start"}));
        let row = format!("{text}, Running");
        self.until(cx, |window, _| has_label(window, &row)).await;
        self.update(cx, |window, cx| click_label(window, &row, cx));
        // The thread view's composer offers Stop while the run is active.
        self.until(cx, |window, _| window.try_find("stop-run").is_some()).await;
        self.until_thread(cx, &thread_id, |state| state.can_steer()).await;
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
        let local = cx.update(|cx| Arc::clone(Services::global(cx).local()));
        let runtime = self.runtime.take().unwrap();
        runtime.block_on(local.shutdown());
        runtime.shutdown_background();
        for dir in [&self.data_dir, &self.project_dir, &self.config_dir] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
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

/// Clicks the center of the element with this accessible label through
/// native pointer events (for rows whose ids are list indices).
fn click_label(window: &mut Window, label: &str, cx: &mut App) {
    let position = window.find_by_label(label).bounds().center();
    window.pointer_move(position, None, cx);
    window.pointer_down(position, MouseButton::Left, cx);
    window.pointer_up(position, MouseButton::Left, cx);
}

/// The sidebar tree item named `label` (the tree's own entry, which carries
/// selection; the row inside it repeats or extends the label).
fn tree_item(window: &Window, label: &str) -> ElementSnapshot {
    let mut items: Vec<_> = window
        .find_all_by_role(Role::TreeItem)
        .into_iter()
        .filter(|item| item.label() == Some(label))
        .collect();
    assert_eq!(items.len(), 1, "one tree item {label:?}");
    items.remove(0)
}

/// The feed row whose accessible label is `text`.
fn feed_row(window: &Window, text: &str) -> Option<ElementSnapshot> {
    let feed = ElementId::Name("feed".into());
    window
        .find_all_by_label(text)
        .into_iter()
        .find(|row| row.path().contains(&feed))
}

fn hostname() -> String {
    gethostname::gethostname().to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[gpui_kit::test]
async fn sidebar_shows_the_environment_its_projects_and_threads(cx: &mut TestAppContext) {
    let app = Harness::start("sidebar", cx).await;

    // The local environment, empty.
    app.until(cx, |window, _| has_label(window, "No projects")).await;
    app.update(cx, |window, _| {
        assert!(has_label(window, &hostname()));
        assert!(window.try_find("add-project-local").is_some());
    });

    // A project created through the connection gets a row and a new-thread button.
    let project_id = app.project(cx).await;
    app.update(cx, |window, _| {
        assert!(!has_label(window, "No projects"));
        let project = tree_item(window, "Demo");
        let env = tree_item(window, &hostname());
        assert!(project.bounds().top() > env.bounds().top());
        assert!(window.try_find(format!("new-thread-local-{project_id}")).is_some());
    });

    // Its threads appear under it with their activity.
    app
        .connection
        .launch(&project_id, "Say hi", Vec::new(), model())
        .await
        .unwrap();
    app.until_called(cx, "prompt:Say hi:0").await;
    app.script.emit(json!({"type": "agent_start"}));
    app.until(cx, |window, _| has_label(window, "Say hi, Running")).await;
    app.script.settle("completed");
    app.until(cx, |window, _| has_label(window, "Say hi, Idle")).await;
    app.update(cx, |window, _| {
        let thread = window.find_by_label("Say hi, Idle");
        assert!(thread.bounds().top() > tree_item(window, "Demo").bounds().top());
        assert_eq!(tree_item(window, "Say hi").selected(), Some(false));
    });

    // Clicking the row opens the thread and selects it.
    app.update(cx, |window, cx| click_label(window, "Say hi, Idle", cx));
    app.until(cx, |window, _| {
        tree_item(window, "Say hi").selected() == Some(true)
    })
    .await;
    // The thread view opens and loads the feed.
    app.until(cx, |window, _| {
        window.try_find("set-goal").is_some() && feed_row(window, "Say hi").is_some()
    })
    .await;

    app.stop(cx);
}

#[gpui_kit::test]
async fn new_thread_launches_from_the_composer_and_streams_the_reply(cx: &mut TestAppContext) {
    let app = Harness::start("launch", cx).await;
    let project_id = app.project(cx).await;
    // The composer needs the model catalog the fake omp serves.
    let connection = app.connection.clone();
    app.until(cx, move |_, _| {
        connection.state().borrow().config.as_ref().is_some_and(|config| {
            config.providers.iter().any(|provider| !provider.models.is_empty())
        })
    })
    .await;

    // The project's new-thread button opens a new thread there, with the
    // composer focused.
    app.update(cx, |window, cx| {
        window.click(format!("new-thread-local-{project_id}"), cx);
    });
    app.until(cx, |window, _| {
        window
            .find_all_by_label("Message")
            .first()
            .is_some_and(|input| input.focused() == Some(true))
    })
    .await;
    app.update(cx, |window, cx| {
        assert!(window.try_find("stop-run").is_none());
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
    app.until(cx, |window, _| {
        feed_row(window, "Say hi").is_some() && window.try_find("set-goal").is_some()
    })
    .await;
    app.update(cx, |window, _| {
        assert_eq!(window.find_by_label("Message").value(), Some(""));
    });

    // Streaming text reaches the feed piece by piece.
    app.script.emit(json!({"type": "agent_start"}));
    app.until(cx, |window, _| has_label(window, "Say hi, Running")).await;
    app.script
        .text("m1", json!({"type": "text_start", "contentIndex": 0}));
    app.script.text(
        "m1",
        json!({"type": "text_delta", "contentIndex": 0, "delta": "Hel"}),
    );
    app.until(cx, |window, _| feed_row(window, "Hel").is_some()).await;
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
        has_label(window, "Say hi, Idle") && window.try_find("stop-run").is_none()
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

#[gpui_kit::test]
async fn a_pending_question_is_answered_from_its_card(cx: &mut TestAppContext) {
    let app = Harness::start("question", cx).await;
    let project_id = app.project(cx).await;
    let thread_id = app.open_running_thread(cx, &project_id, "Ask me").await;
    app.update(cx, |window, _| assert!(window.try_find("question-card").is_none()));

    app.script.emit(
        json!({"type": "extension_ui_request", "id": "ui-1", "method": "select",
        "title": "Pick one", "options": ["alpha", "beta"]}),
    );
    app.until(cx, |window, _| {
        window.try_find("question-card").is_some() && has_label(window, "beta")
    })
    .await;
    app.update(cx, |window, _| {
        assert!(has_label(window, "Ask me, Waiting for input"));
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
    app.until(cx, move |_, _| !script.ui_responses.lock().unwrap().is_empty())
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
        window.try_find("question-card").is_none() && has_label(window, "Ask me, Running")
    })
    .await;
    assert_eq!(app.script.ui_responses.lock().unwrap().len(), 1, "answered once");

    app.stop(cx);
}

#[gpui_kit::test]
async fn enter_steers_and_secondary_enter_queues_while_running(cx: &mut TestAppContext) {
    let app = Harness::start("steer", cx).await;
    let project_id = app.project(cx).await;
    let thread_id = app.open_running_thread(cx, &project_id, "first").await;

    // Enter steers the running turn.
    app.click_composer(cx);
    app.update(cx, |window, cx| {
        assert_eq!(window.find("send-message").label(), Some("Steer"));
        window.input("steer this", cx);
        window.press("enter", cx);
    });
    app.until_called(cx, "steer:steer this").await;
    app.until(cx, |window, _| feed_row(window, "steer this").is_some()).await;
    app.until_thread(cx, &thread_id, |state| {
        state.feed().iter().any(|row| matches!(&row.item.kind,
            TurnItemKind::UserMessage(m) if m.input_intent == UserMessageInputIntent::Steer && m.text == "steer this"))
    })
    .await;
    app.update(cx, |window, _| {
        assert_eq!(window.find_by_label("Message").value(), Some(""));
    });

    // ⌘⏎ queues; the message shows in the queue panel, not the turn.
    app.update(cx, |window, cx| {
        window.input("queue me", cx);
        window.press("secondary-enter", cx);
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
    let row = format!("queued-{queued}");
    app.until(cx, |window, _| label_of(window, &row).as_deref() == Some("queue me"))
        .await;
    app.update(cx, |window, _| {
        assert!(window.try_find(format!("send-now-{queued}")).is_some());
        assert!(window.try_find(format!("steer-{queued}")).is_some());
    });
    assert!(!app.script.called("steer:queue me"));
    assert!(!app.script.called("prompt:queue me:0"));

    app.stop(cx);
}

#[gpui_kit::test]
async fn stop_interrupts_the_run(cx: &mut TestAppContext) {
    let app = Harness::start("stop", cx).await;
    let project_id = app.project(cx).await;
    let thread_id = app.open_running_thread(cx, &project_id, "long task").await;

    // The Stop button.
    app.update(cx, |window, cx| window.click("stop-run", cx));
    app.until_called(cx, "abort").await;
    app.until_thread(cx, &thread_id, |state| state.active_run().is_none())
        .await;
    app.until(cx, |window, _| {
        window.try_find("stop-run").is_none()
            && window.find("send-message").label() == Some("Send")
            && has_label(window, "long task, Idle")
    })
    .await;
    app.update(cx, |window, _| assert!(has_label(window, "long task, Idle")));

    // A new message starts another run; ⌘. (StopRun) stops it too.
    app.click_composer(cx);
    app.update(cx, |window, cx| {
        window.input("again", cx);
        window.press("enter", cx);
    });
    app.until_called(cx, "prompt:again:0").await;
    app.script.emit(json!({"type": "agent_start"}));
    app.until(cx, |window, _| window.try_find("stop-run").is_some()).await;
    app.update(cx, |window, cx| window.press("cmd-.", cx));
    app.until(cx, |window, _| window.try_find("stop-run").is_none()).await;
    assert_eq!(
        app.script.calls().iter().filter(|call| *call == "abort").count(),
        2
    );
    app.until_thread(cx, &thread_id, |state| {
        state.active_run().is_none()
            && state
                .projection
                .as_ref()
                .is_some_and(|p| p.runs.iter().filter(|run| run.status == RunStatus::Interrupted).count() == 2)
    })
    .await;

    app.stop(cx);
}

#[gpui_kit::test]
async fn goal_chip_controls_and_subagent_progress(cx: &mut TestAppContext) {
    let app = Harness::start("extras", cx).await;
    let project_id = app.project(cx).await;
    app.open_running_thread(cx, &project_id, "delegate").await;

    // Set a goal through the header button and its dialog.
    app.update(cx, |window, cx| {
        assert!(window.try_find("goal").is_none());
        window.click("set-goal", cx);
    });
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
        assert!(window.try_find("set-goal").is_none());
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
    app.until(cx, |window, _| {
        window.try_find("goal").is_none() && window.try_find("set-goal").is_some()
    })
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
        label_of(window, "subagent-Pinger").as_deref() == Some("Answer PONG, Done, PONG, from the transcript")
    })
    .await;
    app.update(cx, |window, _| {
        assert!(window.try_find("stop-subagent-Pinger").is_none());
    });

    app.stop(cx);
}
