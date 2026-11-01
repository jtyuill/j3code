//! `App` end to end over its `RpcHandler`, with a scripted omp session:
//! launch, dispatch idempotency, queue hold/resume/cancel/edit/promote,
//! steer, stop, answers, rejections, persistence and recovery on reopen,
//! and t3's thread triage (settle, snooze, pin, reorder, mark-unread,
//! visit, auto-settle).

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::{StreamExt, future::BoxFuture, stream::BoxStream};
use j3_core::{
    app::{App, AppConfig},
    engine::session::{AgentSession, Events, SessionFactory},
    omp::{
        OmpLauncher,
        omp_rpc::{
            AvailableSlashCommand, Event, ExtensionUiResponse, GoalOp, GoalResult, ImageContent,
            PromptAck, RpcNotification, SessionState, SubagentMessages, ThinkingLevel,
        },
    },
    rpc::{Response, RpcHandler, StreamItem},
    store::random_hex,
};
use j3_protocol::{
    effect_rpc::{CauseReason, Exit},
    t3::{
        commands::{
            DispatchCommandResult, OrchestrationV2DispatchCommandError, ThreadLaunchResult,
        },
        orchestration::{
            AppThread, RunStatus, RuntimeRequestStatus, SettledOverride, ShellStreamItem,
            ThreadProjection, ThreadShell, ThreadStreamItem, TurnItemKind, TurnItemStatus,
            UserMessageInputIntent,
        },
        project::Project,
        unix_ms_from_iso,
    },
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

const MODEL: &str = "opencode-go/glm-5.3-flash";

// ---------------------------------------------------------------------------
// Scripted omp
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Script {
    calls: Mutex<Vec<String>>,
    events: Mutex<Option<mpsc::UnboundedSender<Event>>>,
    ui_responses: Mutex<Vec<ExtensionUiResponse>>,
    starts: Mutex<usize>,
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

    fn say(&self, message_id: &str, text: &str) {
        for event in [
            json!({"type": "text_start", "contentIndex": 0}),
            json!({"type": "text_delta", "contentIndex": 0, "delta": text}),
            json!({"type": "text_end", "contentIndex": 0, "content": text}),
        ] {
            self.emit(json!({"type": "message_update", "messageId": message_id,
                "message": {"role": "assistant"}, "assistantMessageEvent": event}));
        }
    }
}

struct FakeSession(Arc<Script>);

impl AgentSession for FakeSession {
    fn session_file(&self) -> &Path {
        Path::new("/tmp/j3-fake-session.jsonl")
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
        // omp reports the aborted run, then settles.
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
        _: Option<i64>,
    ) -> BoxFuture<'_, j3_core::engine::session::Result<GoalResult>> {
        self.0.record(format!(
            "goal:{}:{}",
            op.as_str(),
            objective.unwrap_or_default()
        ));
        Box::pin(async {
            Ok(serde_json::from_value(json!({"goal": null, "state": null})).unwrap())
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
            *script.starts.lock().unwrap() += 1;
            Ok((Box::new(FakeSession(script)) as Box<dyn AgentSession>, rx))
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct Env {
    app: Arc<App>,
    script: Arc<Script>,
    data_dir: PathBuf,
    project_dir: PathBuf,
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("j3-engine-{name}-{}", random_hex(10)));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn open(data_dir: &Path, script: &Arc<Script>) -> Arc<App> {
    App::open_with(
        AppConfig {
            data_dir: data_dir.to_owned(),
            label: "test-host".into(),
            omp: OmpLauncher::default(),
        },
        Arc::new(FakeFactory(Arc::clone(script))),
        Duration::from_secs(600),
    )
    .await
    .unwrap()
}

async fn env(name: &str) -> Env {
    let data_dir = temp_dir(name);
    let project_dir = temp_dir(&format!("{name}-project"));
    let script = Arc::new(Script::default());
    let app = open(&data_dir, &script).await;
    let env = Env {
        app,
        script,
        data_dir,
        project_dir,
    };
    let project = call(
        &env.app,
        "projects.mutate",
        json!({"type": "project.create", "commandId": "c-project", "projectId": "P1",
               "title": "Demo", "workspaceRoot": env.project_dir.to_string_lossy()}),
    )
    .await
    .unwrap();
    let project: Project = serde_json::from_value(project).unwrap();
    assert_eq!(project.title, "Demo");
    env
}

async fn call(app: &Arc<App>, tag: &str, payload: Value) -> Result<Value, Exit> {
    match app.call(tag.to_owned(), payload).await {
        Response::Unary(Exit::Success { value }) => Ok(value),
        Response::Unary(exit) => Err(exit),
        Response::Stream(_) => panic!("{tag} is unary"),
    }
}

async fn stream(app: &Arc<App>, tag: &str, payload: Value) -> BoxStream<'static, StreamItem> {
    match app.call(tag.to_owned(), payload).await {
        Response::Stream(stream) => stream,
        Response::Unary(exit) => panic!("{tag} is a stream: {exit:?}"),
    }
}

fn failure(exit: Exit) -> Value {
    let Exit::Failure { cause } = exit else {
        panic!("expected a failure")
    };
    match cause.into_iter().next() {
        Some(CauseReason::Fail { error }) => error,
        other => panic!("expected a typed failure, got {other:?}"),
    }
}

async fn dispatch(app: &Arc<App>, command: Value) -> Result<u64, Value> {
    match call(app, "orchestration.dispatchCommand", command).await {
        Ok(value) => Ok(serde_json::from_value::<DispatchCommandResult>(value)
            .unwrap()
            .sequence),
        Err(exit) => {
            let error = failure(exit);
            let _: OrchestrationV2DispatchCommandError =
                serde_json::from_value(error.clone()).unwrap();
            Err(error)
        }
    }
}

async fn projection(app: &Arc<App>, thread_id: &str) -> ThreadProjection {
    let value = call(
        app,
        "orchestration.getThreadProjection",
        json!({"threadId": thread_id}),
    )
    .await
    .unwrap();
    serde_json::from_value(value).unwrap()
}

/// Polls until `check` holds for the thread's projection.
async fn wait_for(
    app: &Arc<App>,
    thread_id: &str,
    what: &str,
    check: impl Fn(&ThreadProjection) -> bool,
) -> ThreadProjection {
    for _ in 0..200 {
        let projection = projection(app, thread_id).await;
        if check(&projection) {
            return projection;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "timed out waiting for {what}: {:#?}",
        projection(app, thread_id).await.runs
    );
}

async fn wait_calls(script: &Script, what: &str, check: impl Fn(&[String]) -> bool) {
    for _ in 0..200 {
        if check(&script.calls()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}: {:?}", script.calls());
}

fn selection() -> Value {
    json!({"instanceId": "omp", "model": MODEL, "options": [{"id": "thinking", "value": "high"}]})
}

async fn launch(env: &Env, command_id: &str, thread_id: &str, text: &str) -> ThreadLaunchResult {
    let value = call(
        &env.app,
        "orchestration.launchThread",
        json!({"commandId": command_id, "creationSource": "mobile", "threadId": thread_id,
               "projectId": "P1", "title": text, "modelSelection": selection(),
               "runtimeMode": "full-access", "interactionMode": "default",
               "workspaceStrategy": {"type": "root"},
               "initialMessage": {"messageId": format!("{thread_id}-m1"), "text": text, "attachments": []}}),
    )
    .await
    .unwrap();
    serde_json::from_value(value).unwrap()
}

fn message(thread_id: &str, command_id: &str, text: &str, extra: Value) -> Value {
    let mut command = json!({"type": "message.dispatch", "commandId": command_id,
        "createdBy": "user", "creationSource": "mobile", "threadId": thread_id,
        "messageId": format!("{command_id}-msg"), "text": text, "attachments": [],
        "modelSelection": selection(), "dispatchMode": {"type": "start_immediately"}});
    for (key, value) in extra.as_object().unwrap() {
        command[key] = value.clone();
    }
    command
}

fn run_status(projection: &ThreadProjection, run_id: &str) -> RunStatus {
    projection
        .runs
        .iter()
        .find(|run| run.id == run_id)
        .unwrap()
        .status
}

/// Launches `thread_id` with `text` and lets its run complete; returns the
/// run's completion time (epoch ms).
async fn finish(env: &Env, thread_id: &str, text: &str) -> i64 {
    launch(env, &format!("c-{thread_id}"), thread_id, text).await;
    let prompt = format!("prompt:{text}:0");
    wait_calls(&env.script, "prompt", |calls| calls.contains(&prompt)).await;
    env.script.emit(json!({"type": "agent_start"}));
    env.script.settle("completed");
    let done = wait_for(&env.app, thread_id, "completion", |p| {
        p.runs[0].status == RunStatus::Completed
    })
    .await;
    unix_ms_from_iso(done.runs[0].completed_at.as_deref().unwrap()).unwrap()
}

async fn thread(app: &Arc<App>, thread_id: &str) -> AppThread {
    projection(app, thread_id).await.thread
}

/// The thread's row in a fresh shell snapshot.
async fn shell_row(app: &Arc<App>, thread_id: &str) -> ThreadShell {
    let mut shell = stream(app, "orchestration.subscribeShell", json!({})).await;
    let Some(StreamItem::Value(snapshot)) = shell.next().await else {
        panic!("shell snapshot")
    };
    let ShellStreamItem::Snapshot(snapshot) = serde_json::from_value(snapshot).unwrap() else {
        panic!("snapshot item")
    };
    snapshot
        .snapshot
        .threads
        .into_iter()
        .find(|shell| shell.id == thread_id)
        .unwrap()
}

/// Reads the thread stream up to the next domain event of `event_type`.
async fn next_event(events: &mut BoxStream<'static, StreamItem>, event_type: &str) -> AppThread {
    loop {
        let item = tokio::time::timeout(Duration::from_secs(2), events.next())
            .await
            .unwrap_or_else(|_| panic!("no {event_type} event"));
        let Some(StreamItem::Value(item)) = item else {
            panic!("thread stream ended")
        };
        let ThreadStreamItem::Event(event) = serde_json::from_value(item).unwrap() else {
            continue;
        };
        if event.event.body.tag() == Some(event_type) {
            return serde_json::from_value(
                serde_json::to_value(&event.event.body).unwrap()["payload"].take(),
            )
            .unwrap();
        }
    }
}

fn triage(kind: &str, command_id: &str, thread_id: &str, extra: Value) -> Value {
    let mut command = json!({"type": kind, "commandId": command_id, "threadId": thread_id});
    for (key, value) in extra.as_object().unwrap() {
        command[key] = value.clone();
    }
    command
}

async fn reject_message(app: &Arc<App>, command: Value) -> String {
    let error = dispatch(app, command).await.unwrap_err();
    error["message"].as_str().unwrap().to_owned()
}

fn set<T: Clone>(field: &Option<Option<T>>) -> Option<T> {
    field.clone().flatten()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn engine_app_launch_stream_and_idempotent_dispatch() {
    let env = env("launch").await;
    let mut shell = stream(
        &env.app,
        "orchestration.subscribeShell",
        json!({"requestCompletionMarker": true}),
    )
    .await;
    let Some(StreamItem::Value(snapshot)) = shell.next().await else {
        panic!("shell snapshot")
    };
    let ShellStreamItem::Snapshot(snapshot) = serde_json::from_value(snapshot).unwrap() else {
        panic!("snapshot item")
    };
    assert_eq!(snapshot.snapshot.projects.len(), 1);
    let Some(StreamItem::Value(marker)) = shell.next().await else {
        panic!("marker")
    };
    assert_eq!(marker["kind"], "synchronized");

    let launched = launch(&env, "c-launch", "T1", "Say hi with $jev please").await;
    assert!(!launched.resumed);
    assert_eq!(launched.projection.runs[0].status, RunStatus::Starting);
    assert_eq!(launched.projection.runs[0].user_message_id, "T1-m1");
    assert_eq!(launched.projection.messages[0].id, "T1-m1");
    let launch_sequence = launched.sequence.unwrap();

    // The shell sees the new thread with a sequence ≥ the launch result's.
    let item = loop {
        let Some(StreamItem::Value(item)) = shell.next().await else {
            panic!("shell item")
        };
        if item["kind"] == "thread.updated" {
            break item;
        }
    };
    let ShellStreamItem::ThreadUpdated(updated) = serde_json::from_value(item).unwrap() else {
        panic!("thread.updated")
    };
    assert_eq!(updated.thread.id, "T1");

    // omp gets the model, thinking level and the rewritten skill invocation.
    wait_calls(&env.script, "prompt", |calls| {
        calls.iter().any(|c| c.starts_with("prompt:"))
    })
    .await;
    let calls = env.script.calls();
    assert!(calls.contains(&"thinking:\"high\"".to_owned()), "{calls:?}");
    assert!(
        calls.contains(&"prompt:/skill:jev Say hi with please:0".to_owned()),
        "{calls:?}"
    );

    // A subscriber sees snapshot, marker, then live events.
    let mut thread = stream(
        &env.app,
        "orchestration.subscribeThread",
        json!({"threadId": "T1", "requestCompletionMarker": true}),
    )
    .await;
    let Some(StreamItem::Value(first)) = thread.next().await else {
        panic!()
    };
    let ThreadStreamItem::Snapshot(snapshot) = serde_json::from_value(first).unwrap() else {
        panic!("thread snapshot")
    };
    assert!(snapshot.snapshot_sequence >= launch_sequence);
    assert_eq!(snapshot.projection.visible_turn_items.len(), 1);
    let Some(StreamItem::Value(marker)) = thread.next().await else {
        panic!()
    };
    assert_eq!(marker["kind"], "synchronized");

    env.script.emit(json!({"type": "agent_start"}));
    env.script.say("m1", "Hi!");
    env.script.settle("completed");
    let done = wait_for(&env.app, "T1", "completion", |p| {
        p.runs[0].status == RunStatus::Completed
    })
    .await;
    assert!(
        done.visible_turn_items
            .iter()
            .any(|row| matches!(&row.item.kind,
        TurnItemKind::AssistantMessage(m) if m.text == "Hi!" && !m.streaming))
    );
    let mut last = snapshot.snapshot_sequence;
    let mut saw_completion = false;
    while !saw_completion {
        let Some(StreamItem::Value(item)) = thread.next().await else {
            panic!()
        };
        let ThreadStreamItem::Event(event) = serde_json::from_value(item).unwrap() else {
            panic!("event item")
        };
        assert!(event.sequence > last);
        last = event.sequence;
        if let j3_protocol::t3::orchestration::DomainEventBody::RunUpdated(run) = event.event.body {
            saw_completion = run.status == RunStatus::Completed;
        }
    }

    // Same commandId twice → same sequence, one change.
    let rename = json!({"type": "thread.metadata.update", "commandId": "c-rename", "threadId": "T1", "title": "Renamed"});
    let first = dispatch(&env.app, rename.clone()).await.unwrap();
    let second = dispatch(&env.app, rename).await.unwrap();
    assert_eq!(first, second);
    assert_eq!(projection(&env.app, "T1").await.thread.title, "Renamed");

    // A repeated launch replays.
    let replay = launch(&env, "c-launch", "T1", "Say hi with $jev please").await;
    assert!(replay.resumed);
    assert_eq!(replay.sequence, Some(launch_sequence));
    assert_eq!(replay.projection.runs.len(), 1);

    // Repeating a message with the same commandId does not start a 2nd run.
    let again = message("T1", "c-msg2", "again", json!({}));
    let a = dispatch(&env.app, again.clone()).await.unwrap();
    let b = dispatch(&env.app, again).await.unwrap();
    assert_eq!(a, b);
    let p = projection(&env.app, "T1").await;
    assert_eq!(p.runs.len(), 2);

    env.app.shutdown().await;
    let _ = std::fs::remove_dir_all(&env.data_dir);
    let _ = std::fs::remove_dir_all(&env.project_dir);
}

#[tokio::test]
async fn engine_app_queue_steer_stop_and_promote() {
    let env = env("queue").await;
    launch(&env, "c-l", "T2", "first").await;
    wait_calls(&env.script, "prompt", |calls| {
        calls.iter().any(|c| c == "prompt:first:0")
    })
    .await;
    env.script.emit(json!({"type": "agent_start"}));
    wait_for(&env.app, "T2", "running", |p| {
        p.runs[0].status == RunStatus::Running
    })
    .await;

    // Queue two messages behind the active run.
    dispatch(
        &env.app,
        message(
            "T2",
            "q1",
            "queued one",
            json!({"dispatchMode": {"type": "queue_after_active"}}),
        ),
    )
    .await
    .unwrap();
    dispatch(
        &env.app,
        message(
            "T2",
            "q2",
            "queued two",
            json!({"dispatchMode": {"type": "queue_after_active"}}),
        ),
    )
    .await
    .unwrap();
    let p = projection(&env.app, "T2").await;
    let queued: Vec<_> = p
        .runs
        .iter()
        .filter(|r| r.status == RunStatus::Queued)
        .collect();
    assert_eq!(queued.len(), 2);
    assert_eq!(queued[0].queue_position, Some(Some(1)));
    assert_eq!(queued[1].queue_position, Some(Some(2)));
    let (q1, q2) = (queued[0].id.clone(), queued[1].id.clone());
    let queued_item = p
        .visible_turn_items
        .iter()
        .find(|row| row.item.run_id.as_deref() == Some(q1.as_str()))
        .unwrap();
    assert_eq!(queued_item.item.status, TurnItemStatus::Pending);

    // Edit the first, cancel the second.
    dispatch(&env.app, json!({"type": "queued-run.edit", "commandId": "e1", "threadId": "T2", "runId": q1, "text": "queued one (edited)"})).await.unwrap();
    dispatch(
        &env.app,
        json!({"type": "queued-run.cancel", "commandId": "x2", "threadId": "T2", "runId": q2}),
    )
    .await
    .unwrap();
    let p = projection(&env.app, "T2").await;
    assert_eq!(run_status(&p, &q2), RunStatus::Cancelled);
    assert!(
        p.visible_turn_items
            .iter()
            .all(|row| row.item.run_id.as_deref() != Some(q2.as_str())),
        "cancelled queued message is hidden"
    );
    assert!(p.messages.iter().any(|m| m.text == "queued one (edited)"));

    // Steer while running (composer "auto").
    dispatch(
        &env.app,
        message("T2", "s1", "steer this", json!({"deliveryIntent": "auto"})),
    )
    .await
    .unwrap();
    assert!(env.script.calls().contains(&"steer:steer this".to_owned()));
    let p = projection(&env.app, "T2").await;
    assert!(p.visible_turn_items.iter().any(|row| matches!(&row.item.kind,
        TurnItemKind::UserMessage(m) if m.input_intent == UserMessageInputIntent::Steer && m.text == "steer this")));

    // Stop with holdQueue: the run is interrupted, the queue held.
    let active = p
        .runs
        .iter()
        .find(|r| r.status.is_active())
        .unwrap()
        .id
        .clone();
    dispatch(&env.app, json!({"type": "run.interrupt", "commandId": "stop1", "threadId": "T2", "runId": active, "holdQueue": true})).await.unwrap();
    let p = wait_for(&env.app, "T2", "interrupt", |p| {
        run_status(p, &active) == RunStatus::Interrupted
    })
    .await;
    assert!(
        p.turn_items
            .iter()
            .any(|i| matches!(i.kind, TurnItemKind::RunInterruptRequest(_)))
    );
    assert!(
        p.turn_items
            .iter()
            .any(|i| matches!(i.kind, TurnItemKind::RunInterruptResult(_)))
    );
    assert_eq!(run_status(&p, &q1), RunStatus::Queued);
    assert_eq!(
        p.runs.iter().find(|r| r.id == q1).unwrap().queue_held,
        Some(true)
    );
    let prompts = env
        .script
        .calls()
        .iter()
        .filter(|c| c.starts_with("prompt:"))
        .count();
    assert_eq!(prompts, 1, "a held queue does not start");

    // Resume: the edited queued message starts.
    dispatch(
        &env.app,
        json!({"type": "queue.resume", "commandId": "r1", "threadId": "T2"}),
    )
    .await
    .unwrap();
    wait_calls(&env.script, "queued prompt", |calls| {
        calls.iter().any(|c| c == "prompt:queued one (edited):0")
    })
    .await;
    env.script.emit(json!({"type": "agent_start"}));
    let p = wait_for(&env.app, "T2", "queued run running", |p| {
        run_status(p, &q1) == RunStatus::Running
    })
    .await;
    let item = p
        .visible_turn_items
        .iter()
        .find(|row| row.item.run_id.as_deref() == Some(q1.as_str()))
        .unwrap();
    assert_eq!(item.item.status, TurnItemStatus::Completed);

    // Promote a new queued message into the running turn.
    dispatch(
        &env.app,
        message(
            "T2",
            "q3",
            "promote me",
            json!({"dispatchMode": {"type": "queue_after_active"}}),
        ),
    )
    .await
    .unwrap();
    let p = projection(&env.app, "T2").await;
    let q3 = p
        .runs
        .iter()
        .find(|r| r.status == RunStatus::Queued)
        .unwrap()
        .id
        .clone();
    dispatch(&env.app, json!({"type": "queued-message.promote-to-steer", "commandId": "p3", "threadId": "T2", "queuedRunId": q3, "targetRunId": q1})).await.unwrap();
    assert!(env.script.calls().contains(&"steer:promote me".to_owned()));
    let p = projection(&env.app, "T2").await;
    assert_eq!(run_status(&p, &q3), RunStatus::Cancelled);
    assert!(p.visible_turn_items.iter().any(|row| matches!(&row.item.kind,
        TurnItemKind::UserMessage(m) if m.input_intent == UserMessageInputIntent::PromotedQueuedToSteer)
        && row.item.run_id.as_deref() == Some(q1.as_str())));

    // Answer a question from omp.
    env.script.emit(
        json!({"type": "extension_ui_request", "id": "ui-1", "method": "select",
        "title": "Pick one", "options": ["alpha", "beta"]}),
    );
    let p = wait_for(&env.app, "T2", "question", |p| {
        p.runtime_requests
            .iter()
            .any(|r| r.status == RuntimeRequestStatus::Pending)
    })
    .await;
    assert_eq!(run_status(&p, &q1), RunStatus::Waiting);
    let request = p
        .runtime_requests
        .iter()
        .find(|r| r.status == RuntimeRequestStatus::Pending)
        .unwrap()
        .id
        .clone();
    dispatch(&env.app, json!({"type": "runtime-request.respond", "commandId": "a1", "threadId": "T2", "requestId": request, "answers": {"answer": "beta"}})).await.unwrap();
    let responses = env.script.ui_responses.lock().unwrap().clone();
    assert_eq!(
        serde_json::to_value(&responses[0]).unwrap(),
        json!({"type": "extension_ui_response", "id": "ui-1", "value": "beta"})
    );
    let p = projection(&env.app, "T2").await;
    assert_eq!(run_status(&p, &q1), RunStatus::Running);

    env.script.settle("completed");
    wait_for(&env.app, "T2", "done", |p| {
        run_status(p, &q1) == RunStatus::Completed
    })
    .await;
    env.app.shutdown().await;
    let _ = std::fs::remove_dir_all(&env.data_dir);
    let _ = std::fs::remove_dir_all(&env.project_dir);
}

#[tokio::test]
async fn engine_app_rejections() {
    let env = env("reject").await;
    launch(&env, "c-l", "T3", "hello").await;

    let error = dispatch(
        &env.app,
        json!({"type": "thread.fork", "commandId": "f1", "threadId": "T3"}),
    )
    .await
    .unwrap_err();
    assert_eq!(error["_tag"], "OrchestrationV2DispatchCommandError");
    assert_eq!(error["commandId"], "f1");
    assert_eq!(error["commandType"], "thread.fork");

    let error = dispatch(
        &env.app,
        json!({"type": "thread.archive", "commandId": "a1", "threadId": "missing"}),
    )
    .await
    .unwrap_err();
    let message = error["message"].as_str().unwrap().to_lowercase();
    assert!(
        !message.contains("connection") && !message.contains("timeout"),
        "{message}"
    );

    // Pinning and visiting work while a run is active.
    dispatch(
        &env.app,
        json!({"type": "thread.pin", "commandId": "pin", "threadId": "T3"}),
    )
    .await
    .unwrap();
    dispatch(&env.app, json!({"type": "thread.visit", "commandId": "v", "threadId": "T3", "visitedAt": "2026-11-01T12:00:00.000Z"})).await.unwrap();

    let mut missing = stream(
        &env.app,
        "orchestration.subscribeThread",
        json!({"threadId": "missing"}),
    )
    .await;
    let Some(StreamItem::Fail(error)) = missing.next().await else {
        panic!("typed failure")
    };
    assert_eq!(error["_tag"], "OrchestrationV2GetThreadProjectionError");

    let exit = call(&env.app, "vcs.status", json!({})).await.unwrap_err();
    let Exit::Failure { cause } = exit else {
        panic!()
    };
    assert!(matches!(cause[0], CauseReason::Die { .. }));

    let error = failure(call(&env.app, "orchestration.launchThread", json!({"commandId": "w", "projectId": "P1",
        "title": "x", "modelSelection": selection(), "runtimeMode": "full-access", "interactionMode": "default",
        "workspaceStrategy": {"type": "worktree", "baseRef": "main"}})).await.unwrap_err());
    assert_eq!(error["_tag"], "OrchestrationV2ThreadLaunchError");

    // Archive / unarchive / delete.
    dispatch(
        &env.app,
        json!({"type": "thread.archive", "commandId": "ar", "threadId": "T3"}),
    )
    .await
    .unwrap();
    let archived = call(
        &env.app,
        "orchestration.getArchivedShellSnapshot",
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(archived["threads"][0]["id"], "T3");
    dispatch(
        &env.app,
        json!({"type": "thread.unarchive", "commandId": "un", "threadId": "T3"}),
    )
    .await
    .unwrap();
    dispatch(
        &env.app,
        json!({"type": "thread.delete", "commandId": "del", "threadId": "T3"}),
    )
    .await
    .unwrap();
    let error = failure(
        call(
            &env.app,
            "orchestration.getThreadProjection",
            json!({"threadId": "T3"}),
        )
        .await
        .unwrap_err(),
    );
    assert_eq!(error["_tag"], "OrchestrationV2GetThreadProjectionError");
    // Deleting again is fine.
    dispatch(
        &env.app,
        json!({"type": "thread.delete", "commandId": "del2", "threadId": "T3"}),
    )
    .await
    .unwrap();

    env.app.shutdown().await;
    let _ = std::fs::remove_dir_all(&env.data_dir);
    let _ = std::fs::remove_dir_all(&env.project_dir);
}

#[tokio::test]
async fn engine_app_persistence_and_recovery_on_reopen() {
    let env = env("recover").await;
    let environment_id = env.app.descriptor()["environmentId"].clone();
    launch(&env, "c-l", "T4", "ask me").await;
    wait_calls(&env.script, "prompt", |calls| {
        calls.iter().any(|c| c.starts_with("prompt:"))
    })
    .await;
    env.script.emit(json!({"type": "agent_start"}));
    env.script.emit(json!({"type": "extension_ui_request", "id": "ui-9", "method": "ask",
        "questions": [{"id": "q", "question": "Proceed?", "options": [{"label": "Yes"}, {"label": "No"}]}]}));
    let before = wait_for(&env.app, "T4", "question", |p| {
        p.runtime_requests.len() == 1
    })
    .await;
    assert_eq!(before.runs[0].status, RunStatus::Waiting);
    let high_water = serde_json::from_value::<DispatchCommandResult>(
        call(&env.app, "orchestration.dispatchCommand", json!({"type": "thread.visit", "commandId": "v1", "threadId": "T4", "visitedAt": "2026-11-01T12:00:00.000Z"})).await.unwrap(),
    )
    .unwrap()
    .sequence;
    env.app.shutdown().await;
    drop(env.app);

    let app = open(&env.data_dir, &env.script).await;
    assert_eq!(app.descriptor()["environmentId"], environment_id);
    let after = projection(&app, "T4").await;
    assert_eq!(after.runs[0].status, RunStatus::Interrupted);
    assert_eq!(
        after.runtime_requests[0].status,
        RuntimeRequestStatus::Expired
    );
    let question = after
        .turn_items
        .iter()
        .find(|i| matches!(i.kind, TurnItemKind::UserInputRequest(_)))
        .unwrap();
    assert_eq!(question.status, TurnItemStatus::Cancelled);
    // Recovery committed through the hub: sequences kept growing.
    let sequence = serde_json::from_value::<DispatchCommandResult>(
        call(&app, "orchestration.dispatchCommand", json!({"type": "thread.visit", "commandId": "v2", "threadId": "T4", "visitedAt": "2026-11-01T12:00:00.000Z"})).await.unwrap(),
    )
    .unwrap()
    .sequence;
    assert!(sequence > high_water, "{sequence} > {high_water}");
    // Receipts survive restarts.
    let rename = json!({"type": "thread.metadata.update", "commandId": "c-ren", "threadId": "T4", "title": "Kept"});
    let first = dispatch(&app, rename.clone()).await.unwrap();
    app.shutdown().await;
    drop(app);
    let app = open(&env.data_dir, &env.script).await;
    assert_eq!(dispatch(&app, rename).await.unwrap(), first);
    // The thread resumes with omp on the next message.
    dispatch(&app, message("T4", "after", "continue", json!({})))
        .await
        .unwrap();
    wait_calls(&env.script, "resumed prompt", |calls| {
        calls.iter().any(|c| c == "prompt:continue:0")
    })
    .await;
    assert_eq!(*env.script.starts.lock().unwrap(), 2);
    app.shutdown().await;
    let _ = std::fs::remove_dir_all(&env.data_dir);
    let _ = std::fs::remove_dir_all(&env.project_dir);
}

#[tokio::test]
async fn engine_app_triage_commands() {
    let env = env("triage").await;
    let app = &env.app;
    let far = "2999-01-01T00:00:00.000Z";

    // While the first run is active: settle and mark-unread are refused,
    // the first message already set the read watermark.
    let launched = launch(&env, "c-l", "T5", "first").await;
    assert!(launched.projection.thread.last_visited_at.is_some());
    wait_calls(&env.script, "prompt", |calls| {
        calls.iter().any(|c| c == "prompt:first:0")
    })
    .await;
    env.script.emit(json!({"type": "agent_start"}));
    assert_eq!(
        reject_message(app, triage("thread.settle", "s0", "T5", json!({}))).await,
        "This thread is still running. Stop it before settling."
    );
    assert_eq!(
        reject_message(app, triage("thread.mark-unread", "u0", "T5", json!({}))).await,
        "This thread has no finished run to mark unread."
    );
    // A queued message blocks snoozing; a pending question blocks it first.
    dispatch(
        app,
        message(
            "T5",
            "q1",
            "later",
            json!({"dispatchMode": {"type": "queue_after_active"}}),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        reject_message(
            app,
            triage("thread.snooze", "z0", "T5", json!({"snoozedUntil": far}))
        )
        .await,
        "This thread has a queued message and can't be snoozed."
    );
    env.script.emit(
        json!({"type": "extension_ui_request", "id": "ui-1", "method": "confirm",
        "title": "Proceed?", "message": "ok?"}),
    );
    let waiting = wait_for(app, "T5", "question", |p| {
        p.runtime_requests
            .iter()
            .any(|r| r.status == RuntimeRequestStatus::Pending)
    })
    .await;
    assert_eq!(
        reject_message(
            app,
            triage("thread.snooze", "z1", "T5", json!({"snoozedUntil": far}))
        )
        .await,
        "This thread is waiting on an approval or question and can't be snoozed."
    );
    assert_eq!(
        reject_message(app, triage("thread.settle", "s1", "T5", json!({}))).await,
        "This thread is waiting on an approval or question. Answer it before settling."
    );
    let request = waiting.runtime_requests[0].id.clone();
    dispatch(
        app,
        json!({"type": "runtime-request.respond", "commandId": "a1", "threadId": "T5",
        "requestId": request, "decision": "accept"}),
    )
    .await
    .unwrap();
    let queued = projection(app, "T5")
        .await
        .runs
        .iter()
        .find(|r| r.status == RunStatus::Queued)
        .unwrap()
        .id
        .clone();
    dispatch(
        app,
        triage("queued-run.cancel", "x1", "T5", json!({"runId": queued})),
    )
    .await
    .unwrap();
    env.script.settle("completed");
    let done = wait_for(app, "T5", "completion", |p| {
        p.runs[0].status == RunStatus::Completed
    })
    .await;
    let completed = unix_ms_from_iso(done.runs[0].completed_at.as_deref().unwrap()).unwrap();

    let mut events = stream(
        app,
        "orchestration.subscribeThread",
        json!({"threadId": "T5"}),
    )
    .await;

    // Settle, then settle again: the second keeps settledAt and updatedAt.
    dispatch(app, triage("thread.settle", "s2", "T5", json!({})))
        .await
        .unwrap();
    let settled = next_event(&mut events, "thread.settled").await;
    assert_eq!(settled.settled_override, Some(SettledOverride::Settled));
    assert!(settled.settled_at.is_some());
    dispatch(app, triage("thread.settle", "s3", "T5", json!({})))
        .await
        .unwrap();
    let again = next_event(&mut events, "thread.settled").await;
    assert_eq!(again.settled_at, settled.settled_at);
    assert_eq!(again.updated_at, settled.updated_at);
    let shell = shell_row(app, "T5").await;
    assert_eq!(shell.settled_override, Some(SettledOverride::Settled));
    assert_eq!(shell.settled_at, settled.settled_at);
    // Settled threads can't be reordered among active ones.
    assert_eq!(
        reject_message(
            app,
            triage(
                "thread.active.reorder",
                "r0",
                "T5",
                json!({"orderKey": "m"})
            )
        )
        .await,
        "This thread is not active, so it can't be reordered."
    );

    // Unsettle → active.
    dispatch(
        app,
        triage("thread.unsettle", "un1", "T5", json!({"reason": "user"})),
    )
    .await
    .unwrap();
    let active = next_event(&mut events, "thread.unsettled").await;
    assert_eq!(active.settled_override, Some(SettledOverride::Active));
    assert_eq!(active.settled_at, None);
    assert!(set(&active.unsettled_at).is_some());

    // Active reorder never moves updatedAt.
    dispatch(
        app,
        triage(
            "thread.active.reorder",
            "r1",
            "T5",
            json!({"orderKey": "k"}),
        ),
    )
    .await
    .unwrap();
    let reordered = next_event(&mut events, "thread.active-reordered").await;
    assert_eq!(set(&reordered.active_order_key).as_deref(), Some("k"));
    assert_eq!(reordered.updated_at, active.updated_at);
    assert_eq!(
        reject_message(
            app,
            triage(
                "thread.active.reorder",
                "r2",
                "T5",
                json!({"orderKey": "  "})
            )
        )
        .await,
        "The order key can't be empty."
    );

    // Pin with a key; reorder; unpin; reorder after unpin is refused.
    dispatch(
        app,
        triage("thread.pin", "p1", "T5", json!({"orderKey": "m"})),
    )
    .await
    .unwrap();
    let pinned = next_event(&mut events, "thread.pinned").await;
    assert!(set(&pinned.pinned_at).is_some());
    assert_eq!(set(&pinned.pin_order_key).as_deref(), Some("m"));
    assert_eq!(
        reject_message(
            app,
            triage(
                "thread.active.reorder",
                "r3",
                "T5",
                json!({"orderKey": "j"})
            )
        )
        .await,
        "This thread is not active, so it can't be reordered."
    );
    dispatch(
        app,
        triage("thread.pin.reorder", "pr1", "T5", json!({"orderKey": "n"})),
    )
    .await
    .unwrap();
    let moved = next_event(&mut events, "thread.pin-reordered").await;
    assert_eq!(set(&moved.pin_order_key).as_deref(), Some("n"));
    assert_eq!(set(&moved.pinned_at), set(&pinned.pinned_at));
    let shell = shell_row(app, "T5").await;
    assert_eq!(set(&shell.pin_order_key).as_deref(), Some("n"));
    assert_eq!(set(&shell.active_order_key).as_deref(), Some("k"));
    dispatch(app, triage("thread.unpin", "up1", "T5", json!({})))
        .await
        .unwrap();
    let unpinned = next_event(&mut events, "thread.unpinned").await;
    assert_eq!(set(&unpinned.pinned_at), None);
    assert_eq!(set(&unpinned.pin_order_key), None);
    assert_eq!(
        reject_message(
            app,
            triage("thread.pin.reorder", "pr2", "T5", json!({"orderKey": "o"}))
        )
        .await,
        "This thread is not pinned, so it can't be reordered."
    );

    // Settling clears the order keys; pinning a settled thread promotes it.
    dispatch(app, triage("thread.settle", "s4", "T5", json!({})))
        .await
        .unwrap();
    let settled = next_event(&mut events, "thread.settled").await;
    assert_eq!(set(&settled.active_order_key), None);
    assert_eq!(set(&settled.unsettled_at), None);
    dispatch(app, triage("thread.pin", "p2", "T5", json!({})))
        .await
        .unwrap();
    let promoted = next_event(&mut events, "thread.pinned").await;
    assert_eq!(promoted.settled_override, Some(SettledOverride::Active));
    assert_eq!(promoted.settled_at, None);
    assert!(set(&promoted.pinned_at).is_some());
    dispatch(app, triage("thread.unpin", "up2", "T5", json!({})))
        .await
        .unwrap();
    next_event(&mut events, "thread.unpinned").await;

    // Snooze: the wake time must be ahead.
    assert_eq!(
        reject_message(
            app,
            triage(
                "thread.snooze",
                "z2",
                "T5",
                json!({"snoozedUntil": "2000-01-01T00:00:00.000Z"})
            )
        )
        .await,
        "The snooze time is not in the future."
    );
    dispatch(
        app,
        triage(
            "thread.snooze",
            "z3",
            "T5",
            json!({"snoozedUntil": "2999-01-01T00:00:00Z"}),
        ),
    )
    .await
    .unwrap();
    let snoozed = next_event(&mut events, "thread.snoozed").await;
    assert_eq!(set(&snoozed.snoozed_until).as_deref(), Some(far));
    assert!(set(&snoozed.snoozed_at).is_some());
    assert_eq!(
        set(&shell_row(app, "T5").await.snoozed_until).as_deref(),
        Some(far)
    );
    // Pinning a snoozed thread wakes it.
    dispatch(app, triage("thread.pin", "p3", "T5", json!({})))
        .await
        .unwrap();
    let woke = next_event(&mut events, "thread.pinned").await;
    assert_eq!(set(&woke.snoozed_until), None);
    assert_eq!(set(&woke.snoozed_at), None);
    assert!(set(&woke.last_snooze_wake_at).is_some());
    dispatch(app, triage("thread.unpin", "up3", "T5", json!({})))
        .await
        .unwrap();
    next_event(&mut events, "thread.unpinned").await;
    // Unsnooze: wakes and records the wake; a second one changes nothing.
    dispatch(
        app,
        triage("thread.snooze", "z4", "T5", json!({"snoozedUntil": far})),
    )
    .await
    .unwrap();
    next_event(&mut events, "thread.snoozed").await;
    dispatch(
        app,
        triage("thread.unsnooze", "uz1", "T5", json!({"reason": "user"})),
    )
    .await
    .unwrap();
    let awake = next_event(&mut events, "thread.unsnoozed").await;
    assert_eq!(set(&awake.snoozed_until), None);
    assert!(set(&awake.last_snooze_wake_at) > set(&woke.last_snooze_wake_at));
    dispatch(
        app,
        triage("thread.unsnooze", "uz2", "T5", json!({"reason": "user"})),
    )
    .await
    .unwrap();
    let still = next_event(&mut events, "thread.unsnoozed").await;
    assert_eq!(still.last_snooze_wake_at, awake.last_snooze_wake_at);
    assert_eq!(still.updated_at, awake.updated_at);

    // Auto-settle opt-out keeps its first timestamp.
    dispatch(
        app,
        triage(
            "thread.auto-settle.set",
            "as1",
            "T5",
            json!({"enabled": false}),
        ),
    )
    .await
    .unwrap();
    let off = next_event(&mut events, "thread.auto-settle-set").await;
    assert!(set(&off.auto_settle_disabled_at).is_some());
    dispatch(
        app,
        triage(
            "thread.auto-settle.set",
            "as2",
            "T5",
            json!({"enabled": false}),
        ),
    )
    .await
    .unwrap();
    let still_off = next_event(&mut events, "thread.auto-settle-set").await;
    assert_eq!(
        still_off.auto_settle_disabled_at,
        off.auto_settle_disabled_at
    );
    assert_eq!(still_off.updated_at, off.updated_at);
    dispatch(
        app,
        triage(
            "thread.auto-settle.set",
            "as3",
            "T5",
            json!({"enabled": true}),
        ),
    )
    .await
    .unwrap();
    let on = next_event(&mut events, "thread.auto-settle-set").await;
    assert_eq!(set(&on.auto_settle_disabled_at), None);

    // Visit only moves forward and never moves updatedAt.
    let visit = |command_id: &str, at: &str| {
        triage(
            "thread.visit",
            command_id,
            "T5",
            json!({"visitedAt": at, "opened": true}),
        )
    };
    dispatch(app, visit("v1", "2999-01-01T00:00:00.000Z"))
        .await
        .unwrap();
    let visited = next_event(&mut events, "thread.visited").await;
    assert_eq!(visited.last_visited_at.as_deref(), Some(far));
    assert_eq!(visited.updated_at, on.updated_at);
    dispatch(app, visit("v2", "2001-01-01T00:00:00.000Z"))
        .await
        .unwrap();
    let not_back = next_event(&mut events, "thread.visited").await;
    assert_eq!(not_back.last_visited_at.as_deref(), Some(far));
    assert_eq!(
        reject_message(app, visit("v3", "yesterday")).await,
        "The visit time is not a valid timestamp."
    );

    // Mark unread: just before the latest completion; no updatedAt change.
    dispatch(app, triage("thread.mark-unread", "mu1", "T5", json!({})))
        .await
        .unwrap();
    let unread = next_event(&mut events, "thread.marked-unread").await;
    assert_eq!(
        unread.last_visited_at.as_deref().and_then(unix_ms_from_iso),
        Some(completed - 1)
    );
    assert_eq!(unread.updated_at, on.updated_at);
    assert_eq!(
        shell_row(app, "T5").await.last_visited_at,
        Some(unread.last_visited_at.clone())
    );

    // A sent message un-settles and wakes the thread.
    dispatch(app, triage("thread.settle", "s5", "T5", json!({})))
        .await
        .unwrap();
    next_event(&mut events, "thread.settled").await;
    dispatch(
        app,
        triage("thread.snooze", "z5", "T5", json!({"snoozedUntil": far})),
    )
    .await
    .unwrap();
    next_event(&mut events, "thread.snoozed").await;
    dispatch(app, message("T5", "m2", "back again", json!({})))
        .await
        .unwrap();
    let woken = next_event(&mut events, "thread.unsnoozed").await;
    assert_eq!(woken.settled_override, None);
    assert_eq!(woken.settled_at, None);
    assert!(set(&woken.unsettled_at).is_some());
    assert_eq!(set(&woken.snoozed_until), None);
    let shell = shell_row(app, "T5").await;
    assert_eq!(shell.settled_override, None);
    assert_eq!(set(&shell.snoozed_until), None);

    // Archived threads refuse triage; idempotent replays keep their sequence.
    let first = dispatch(app, triage("thread.archive", "ar", "T5", json!({})))
        .await
        .unwrap();
    assert_eq!(
        dispatch(app, triage("thread.archive", "ar", "T5", json!({})))
            .await
            .unwrap(),
        first
    );
    assert_eq!(
        reject_message(app, triage("thread.pin", "p4", "T5", json!({}))).await,
        "This thread is archived."
    );
    assert_eq!(
        reject_message(app, triage("thread.settle", "s6", "T5", json!({}))).await,
        "This thread is archived."
    );

    env.app.shutdown().await;
    let _ = std::fs::remove_dir_all(&env.data_dir);
    let _ = std::fs::remove_dir_all(&env.project_dir);
}

#[tokio::test]
async fn engine_app_auto_settle_sweep_and_persistence() {
    let env = env("autosettle").await;
    let app = &env.app;
    let after = Duration::from_secs(3 * 24 * 60 * 60);
    let after_ms = after.as_millis() as i64;

    let quiet = finish(&env, "A1", "quiet").await;
    finish(&env, "A2", "pinned").await;
    dispatch(
        app,
        triage("thread.pin", "p", "A2", json!({"orderKey": "m"})),
    )
    .await
    .unwrap();
    finish(&env, "A3", "opted out").await;
    dispatch(
        app,
        triage(
            "thread.auto-settle.set",
            "o",
            "A3",
            json!({"enabled": false}),
        ),
    )
    .await
    .unwrap();
    finish(&env, "A4", "snoozed").await;
    dispatch(
        app,
        triage(
            "thread.snooze",
            "z",
            "A4",
            json!({"snoozedUntil": "2999-01-01T00:00:00.000Z"}),
        ),
    )
    .await
    .unwrap();
    let woke = finish(&env, "A5", "woke").await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    dispatch(
        app,
        triage(
            "thread.snooze",
            "z5",
            "A5",
            json!({"snoozedUntil": "2999-01-01T00:00:00.000Z"}),
        ),
    )
    .await
    .unwrap();
    dispatch(
        app,
        triage("thread.unsnooze", "w5", "A5", json!({"reason": "user"})),
    )
    .await
    .unwrap();
    let wake =
        unix_ms_from_iso(&set(&thread(app, "A5").await.last_snooze_wake_at).unwrap()).unwrap();
    assert!(wake > woke);

    // Nothing is due yet: inactivity must exceed `after`.
    assert_eq!(app.auto_settle(quiet + after_ms, after).await, 0);

    // Inactive for longer than `after`: A1 settles at its last activity;
    // pinned, opted-out and snoozed threads stay. A5's wake restarted its
    // timer.
    let mut events = stream(
        app,
        "orchestration.subscribeThread",
        json!({"threadId": "A1"}),
    )
    .await;
    let now = woke + after_ms + 1;
    assert!(wake > woke + 1);
    assert_eq!(app.auto_settle(now, after).await, 1);
    let settled = next_event(&mut events, "thread.settled").await;
    assert_eq!(settled.settled_override, Some(SettledOverride::Settled));
    assert_eq!(
        settled.settled_at.as_deref().and_then(unix_ms_from_iso),
        Some(quiet)
    );
    for id in ["A2", "A3", "A4", "A5"] {
        assert_eq!(thread(app, id).await.settled_override, None, "{id}");
    }
    // A second sweep finds nothing new.
    assert_eq!(app.auto_settle(now, after).await, 0);
    let now = wake + after_ms + 1;
    assert_eq!(app.auto_settle(now, after).await, 1);
    assert_eq!(
        thread(app, "A5")
            .await
            .settled_at
            .as_deref()
            .and_then(unix_ms_from_iso),
        Some(wake)
    );
    // A user's unsettle wins over later sweeps.
    dispatch(
        app,
        triage("thread.unsettle", "u", "A1", json!({"reason": "user"})),
    )
    .await
    .unwrap();
    assert_eq!(app.auto_settle(now + 30 * after_ms, after).await, 0);

    // Every field survives a restart, in the projection and the shell.
    let before: Vec<(AppThread, ThreadShell)> = {
        let mut rows = Vec::new();
        for id in ["A1", "A2", "A3", "A4", "A5"] {
            rows.push((thread(app, id).await, shell_row(app, id).await));
        }
        rows
    };
    env.app.shutdown().await;
    let app = open(&env.data_dir, &env.script).await;
    for (thread_before, shell_before) in before {
        let id = thread_before.id.clone();
        assert_eq!(thread(&app, &id).await, thread_before, "{id}");
        assert_eq!(shell_row(&app, &id).await, shell_before, "{id}");
    }
    let a1 = thread(&app, "A1").await;
    assert_eq!(a1.settled_override, Some(SettledOverride::Active));
    let a2 = thread(&app, "A2").await;
    assert_eq!(set(&a2.pin_order_key).as_deref(), Some("m"));
    assert!(set(&thread(&app, "A3").await.auto_settle_disabled_at).is_some());
    assert!(set(&thread(&app, "A4").await.snoozed_until).is_some());
    assert_eq!(
        thread(&app, "A5").await.settled_override,
        Some(SettledOverride::Settled)
    );

    app.shutdown().await;
    let _ = std::fs::remove_dir_all(&env.data_dir);
    let _ = std::fs::remove_dir_all(&env.project_dir);
}
