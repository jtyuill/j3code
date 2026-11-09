//! The scripted omp behind the test server (as in `crates/j3-core/tests/v2_app.rs`), the
//! shell-script utility omp for the model catalog, usage and word prediction, and the fake
//! microphone and recogniser behind dictation.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use futures_util::future::BoxFuture;
use j3_core::{
    engine::session::{AgentSession, Events, Result, SessionFactory},
    omp::{
        self,
        omp_rpc::{
            self, AvailableSlashCommand, Event, ExtensionUiResponse, GoalOp, GoalResult,
            ImageContent, PromptAck, RpcNotification, SessionState, SubagentMessages,
            ThinkingLevel,
        },
    },
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::speech::{
    AudioSource, AudioStream, ModelStatus, Pass, Recognizer,
    models::{Hardware, SpeechModel},
};

/// The provider of the model the fake omp lists (and the tests launch with).
pub const PROVIDER: &str = "opencode-go";
/// That model's id within its provider.
pub const MODEL_ID: &str = "glm-5.3-flash";
/// That model as the composer names it (`provider/id`).
pub const MODEL: &str = "opencode-go/glm-5.3-flash";

/// What the fake sessions were asked, and their event channels to answer through.
#[derive(Default)]
pub struct Script {
    calls: Mutex<Vec<String>>,
    /// Each started session's event channel, in start order.
    sessions: Mutex<Vec<mpsc::UnboundedSender<Event>>>,
    pub ui_responses: Mutex<Vec<ExtensionUiResponse>>,
    /// omp's current goal (`Goal` JSON), driven by `goal` like omp does.
    goal: Mutex<Option<Value>>,
    /// Ids of running subagents (`cancel_subagent` stops them).
    subagents: Mutex<Vec<String>>,
}

impl Script {
    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    pub fn called(&self, call: &str) -> bool {
        self.calls.lock().unwrap().iter().any(|c| c == call)
    }

    /// How many calls start with `prefix`.
    pub fn count(&self, prefix: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.starts_with(prefix))
            .count()
    }

    /// Sends an omp notification to the latest session.
    pub fn emit(&self, value: Value) {
        let last = self.sessions.lock().unwrap().len().checked_sub(1);
        self.emit_to(last.expect("omp started"), value);
    }

    /// Sends an omp notification to the `session`th session (0-based).
    pub fn emit_to(&self, session: usize, value: Value) {
        let notification = RpcNotification::from_value(value).unwrap();
        self.sessions.lock().unwrap()[session]
            .send(Event::Notification(notification))
            .unwrap();
    }

    pub fn settle(&self, status: &str) {
        self.emit(json!({"type": "prompt_result", "agentInvoked": true, "status": status, "sessionSettled": true}));
    }

    pub fn text(&self, message_id: &str, event: Value) {
        self.emit(json!({"type": "message_update", "messageId": message_id,
            "message": {"role": "assistant"}, "assistantMessageEvent": event}));
    }

    /// omp's lifecycle frame for subagent `id`, started by tool call `call_task`.
    pub fn subagent_lifecycle(&self, id: &str, status: &str) {
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

    pub fn subagent_progress(&self, id: &str, status: &str, output: &[&str], tokens: u64) {
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

struct FakeSession {
    script: Arc<Script>,
    session_file: PathBuf,
}

impl AgentSession for FakeSession {
    fn session_file(&self) -> &Path {
        &self.session_file
    }

    fn prompt(
        &self,
        message: String,
        images: Vec<ImageContent>,
    ) -> BoxFuture<'_, Result<PromptAck>> {
        self.script
            .record(format!("prompt:{message}:{}", images.len()));
        Box::pin(async {
            Ok(PromptAck {
                agent_invoked: Some(true),
            })
        })
    }

    fn steer(&self, message: String, _: Vec<ImageContent>) -> BoxFuture<'_, Result<()>> {
        self.script.record(format!("steer:{message}"));
        Box::pin(async { Ok(()) })
    }

    fn abort(&self) -> BoxFuture<'_, Result<()>> {
        self.script.record("abort".into());
        self.script.settle("aborted");
        Box::pin(async { Ok(()) })
    }

    fn set_model(&self, provider: String, model_id: String) -> BoxFuture<'_, Result<()>> {
        self.script
            .record(format!("set_model:{provider}/{model_id}"));
        Box::pin(async { Ok(()) })
    }

    fn set_thinking_level(&self, level: ThinkingLevel) -> BoxFuture<'_, Result<()>> {
        self.script
            .record(format!("thinking:{}", serde_json::to_value(level).unwrap()));
        Box::pin(async { Ok(()) })
    }

    fn get_state(&self) -> BoxFuture<'_, Result<SessionState>> {
        Box::pin(async {
            Ok(serde_json::from_value(json!({
                "sessionId": "fake",
                "model": model_info(),
                "thinkingLevel": "low",
            }))
            .unwrap())
        })
    }

    fn available_commands(&self) -> BoxFuture<'_, Result<Vec<AvailableSlashCommand>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn goal(
        &self,
        op: GoalOp,
        objective: Option<String>,
        token_budget: Option<i64>,
    ) -> BoxFuture<'_, Result<GoalResult>> {
        self.script.record(format!("goal:{}", op.as_str()));
        let mut goal = self.script.goal.lock().unwrap();
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
            self.script
                .emit(json!({"type": "goal_updated", "goal": goal.clone()}));
            if status(&goal) == Some(json!("dropped")) {
                *goal = None;
            }
        }
        let result = json!({"goal": goal.clone(), "state": null});
        drop(goal);
        Box::pin(async move { outcome.map(|()| serde_json::from_value(result).unwrap()) })
    }

    fn cancel_subagent(&self, subagent_id: String) -> BoxFuture<'_, Result<bool>> {
        self.script.record(format!("cancel_subagent:{subagent_id}"));
        let running = self.script.subagents.lock().unwrap().contains(&subagent_id);
        if running {
            self.script.subagent_lifecycle(&subagent_id, "aborted");
        }
        Box::pin(async move { Ok(running) })
    }

    fn subagent_messages(&self, subagent_id: String) -> BoxFuture<'_, Result<SubagentMessages>> {
        self.script
            .record(format!("subagent_messages:{subagent_id}"));
        Box::pin(async {
            Ok(serde_json::from_value(json!({"sessionFile": "/tmp/sub.jsonl", "fromByte": 0,
                "nextByte": 10, "reset": false, "entries": [], "messages": [
                    {"role": "user", "content": [{"type": "text", "text": "Reply PONG."}], "timestamp": 1},
                    {"role": "assistant", "content": [{"type": "text", "text": "PONG, from the transcript"}],
                        "timestamp": 2}]}))
            .unwrap())
        })
    }

    fn respond_ui(&self, response: ExtensionUiResponse) -> Result<()> {
        self.script.ui_responses.lock().unwrap().push(response);
        Ok(())
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, ()> {
        self.script.record("close".into());
        Box::pin(async {})
    }
}

/// Starts [`FakeSession`]s over one [`Script`]; every session's transcript is
/// `session_file` (subagent transcripts sit in the directory of its stem).
pub struct FakeFactory {
    pub script: Arc<Script>,
    pub session_file: PathBuf,
}

impl SessionFactory for FakeFactory {
    fn start(
        &self,
        _cwd: PathBuf,
        _resume: Option<PathBuf>,
    ) -> BoxFuture<'static, Result<(Box<dyn AgentSession>, Events)>> {
        let script = Arc::clone(&self.script);
        let session_file = self.session_file.clone();
        Box::pin(async move {
            let (tx, rx) = mpsc::unbounded_channel();
            script.sessions.lock().unwrap().push(tx);
            let session = FakeSession {
                script,
                session_file,
            };
            Ok((Box::new(session) as Box<dyn AgentSession>, rx))
        })
    }
}

/// omp's description of [`MODEL`].
fn model_info() -> Value {
    json!({"id": MODEL_ID, "name": "GLM 5.3 Flash", "api": "x", "provider": PROVIDER,
        "baseUrl": "x", "reasoning": true, "input": ["text"],
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
        "contextWindow": null, "maxTokens": null})
}

/// A utility omp (`--no-session`) for the server's model catalog and the composer's word
/// prediction: `ready`, then one canned response per command. `omp usage` prints
/// `usage.json` beside it. It predicts `tor` after any draft ending in `refac` (`or` after
/// `refact`) and logs prediction feedback to `predict-feedback.log` beside itself.
pub fn fake_omp_script() -> String {
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

/// Writes [`fake_omp_script`] to `dir/fake-omp`, executable; returns its path.
pub fn install_fake_omp(dir: &Path) -> PathBuf {
    let path = dir.join("fake-omp");
    std::fs::write(&path, fake_omp_script()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

/// A microphone that has heard a second of sound the moment it opens.
pub struct FakeMicrophone;

struct FakeStream;

impl AudioStream for FakeStream {
    fn rate(&self) -> u32 {
        16_000
    }
}

impl AudioSource for FakeMicrophone {
    fn open(
        &self,
        sink: Arc<Mutex<Vec<f32>>>,
    ) -> std::result::Result<Box<dyn AudioStream>, String> {
        sink.lock()
            .unwrap()
            .extend(std::iter::repeat_n(0.1, 16_000));
        Ok(Box::new(FakeStream))
    }
}

/// Hears "hello" while recording and "hello there" in the end.
pub struct FakeRecognizer(pub Hardware);

impl Recognizer for FakeRecognizer {
    fn hardware(&self) -> &Hardware {
        &self.0
    }

    fn prepare(
        &self,
        _: SpeechModel,
        _: &mut dyn FnMut(ModelStatus),
    ) -> std::result::Result<(), String> {
        Ok(())
    }

    fn transcribe(&self, _: &[f32], pass: Pass) -> std::result::Result<String, String> {
        Ok(match pass {
            Pass::Partial => "hello",
            Pass::Final => "hello there",
        }
        .to_owned())
    }
}
