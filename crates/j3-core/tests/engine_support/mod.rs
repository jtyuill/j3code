//! A scripted omp for integration tests that drive a real `App`: the test
//! pushes omp events through [`Script`] and reads back what the engine asked
//! omp to do.

#![allow(dead_code)]

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::future::BoxFuture;
use j3_core::{
    app::{App, AppConfig},
    engine::session::{AgentSession, Events, SessionFactory},
    omp::{
        OmpLauncher,
        omp_rpc::{
            AvailableSlashCommand, BtwHistoryRecord, Event, ExtensionUiResponse, GoalOp,
            GoalResult, ImageContent, PromptAck, RpcNotification, SessionState, SubagentMessages,
            ThinkingLevel,
        },
    },
    store::random_hex,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

#[derive(Default)]
pub struct Script {
    pub calls: Mutex<Vec<String>>,
    pub events: Mutex<Option<mpsc::UnboundedSender<Event>>>,
    pub ui_responses: Mutex<Vec<ExtensionUiResponse>>,
    pub starts: Mutex<usize>,
    /// What the fake omp names threads; none by default, like a greeting.
    pub title: Mutex<Option<String>>,
}

impl Script {
    pub fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    pub fn emit(&self, value: Value) {
        let notification = RpcNotification::from_value(value).unwrap();
        self.events
            .lock()
            .unwrap()
            .as_ref()
            .expect("omp started")
            .send(Event::Notification(notification))
            .unwrap();
    }

    pub fn settle(&self, status: &str) {
        self.emit(json!({"type": "prompt_result", "agentInvoked": true, "status": status, "sessionSettled": true}));
    }

    pub fn say(&self, message_id: &str, text: &str) {
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

/// omp's record of side question `question` as it starts: topic `btw-1`.
pub fn btw_running(question: &str) -> Value {
    json!({"id": "btw-1", "leafId": null, "question": question, "answer": "",
        "status": "running", "createdAt": 1, "updatedAt": 1})
}

pub struct FakeSession(pub Arc<Script>);

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

    fn btw(
        &self,
        question: String,
        record_id: Option<String>,
    ) -> BoxFuture<'_, j3_core::engine::session::Result<BtwHistoryRecord>> {
        self.0.record(format!(
            "btw:{question}:{}",
            record_id.as_deref().unwrap_or_default()
        ));
        Box::pin(async move { Ok(serde_json::from_value(btw_running(&question)).unwrap()) })
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

    fn set_session_name(
        &self,
        name: String,
    ) -> BoxFuture<'_, j3_core::engine::session::Result<()>> {
        self.0.record(format!("set_session_name:{name}"));
        Box::pin(async { Ok(()) })
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, ()> {
        self.0.record("close".into());
        Box::pin(async {})
    }
}

pub struct FakeFactory(pub Arc<Script>);

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

    fn title(&self, _cwd: PathBuf, _message: String) -> BoxFuture<'static, Option<String>> {
        let title = self.0.title.lock().unwrap().clone();
        Box::pin(async move { title })
    }
}

/// A fresh directory under the system temp dir.
pub fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("j3-engine-{name}-{}", random_hex(10)));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Opens the app in `data_dir` with `script` as its omp.
pub async fn open(data_dir: &Path, script: &Arc<Script>) -> Arc<App> {
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

/// Polls until `check` holds for the calls the fake omp received.
pub async fn wait_calls(script: &Script, what: &str, check: impl Fn(&[String]) -> bool) {
    for _ in 0..200 {
        if check(&script.calls()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}: {:?}", script.calls());
}
