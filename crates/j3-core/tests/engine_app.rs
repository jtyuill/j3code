//! `App` end to end over its `RpcHandler`, with a scripted omp session:
//! launch, dispatch idempotency, queue hold/resume/cancel/edit/promote,
//! steer, stop, answers, rejections, persistence and recovery on reopen.

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
            RunStatus, RuntimeRequestStatus, ShellStreamItem, ThreadProjection, ThreadStreamItem,
            TurnItemKind, TurnItemStatus, UserMessageInputIntent,
        },
        project::Project,
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

    // No-op triage commands succeed.
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
