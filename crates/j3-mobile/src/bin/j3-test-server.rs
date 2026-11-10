//! A j3 server whose omp is a script, for the iOS simulator smoke test
//! (`contrib/ios/smoke.sh`).
//!
//! It serves a fresh data directory with one project, `Smoke`, holding a
//! few files (for `@path` completion) and one thread, `Hello`. The scripted
//! omp answers every prompt with `Echo: <prompt>`; a prompt containing
//! "ask" first opens a `select` request ("Pick a color") and answers with
//! the choice once it is made. A side question (`/btw`) is answered with
//! `Aside: <question>`.
//!
//! ```sh
//! cargo run -p j3-mobile --features test-server --bin j3-test-server -- --port 3799
//! ```
//!
//! Prints `listening on http://<addr>` once the project and thread exist.

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use futures_util::future::BoxFuture;
use j3_client::{
    Connection, Status,
    entities::{ModelRef, ThinkingLevel},
    methods::{CreateProjectParams, CreateThreadParams},
};
use j3_core::{
    app::{App, AppConfig},
    engine::session::{AgentSession, Events, Result, SessionFactory},
    omp::{
        self, OmpLauncher,
        omp_rpc::{
            self, AvailableSlashCommand, Event, ExtensionUiResponse, GoalOp, GoalResult,
            ImageContent, PromptAck, RpcNotification, SessionState, SubagentMessages,
            ThinkingLevel as OmpThinkingLevel,
        },
    },
    serve::Server,
};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

/// The pause between scripted omp events, so the app sees a turn run.
const STEP: Duration = Duration::from_millis(150);

struct Script {
    events: mpsc::UnboundedSender<Event>,
    /// The open `select` request's answer channel.
    answer: Mutex<Option<oneshot::Sender<ExtensionUiResponse>>>,
    messages: AtomicU64,
}

impl Script {
    fn emit(&self, value: Value) {
        let notification = RpcNotification::from_value(value).expect("a scripted omp event");
        let _ = self.events.send(Event::Notification(notification));
    }

    fn settle(&self, status: &str) {
        self.emit(
            json!({"type": "prompt_result", "agentInvoked": true, "status": status,
            "sessionSettled": true}),
        );
    }

    /// Streams `text` as one assistant text block, word by word.
    async fn say(&self, text: &str) {
        let id = format!("m{}", self.messages.fetch_add(1, Ordering::Relaxed));
        let event = |event: Value| {
            json!({"type": "message_update", "messageId": id,
                "message": {"role": "assistant"}, "assistantMessageEvent": event})
        };
        self.emit(event(json!({"type": "text_start", "contentIndex": 0})));
        for word in text.split_inclusive(' ') {
            self.emit(event(
                json!({"type": "text_delta", "contentIndex": 0, "delta": word}),
            ));
            tokio::time::sleep(STEP / 4).await;
        }
        self.emit(event(
            json!({"type": "text_end", "contentIndex": 0, "content": text}),
        ));
    }

    /// Answers side question `question` (topic `btw-1`), word by word.
    async fn answer_aside(self: Arc<Self>, question: String) {
        let answer = format!("Aside: {question}");
        for word in answer.split_inclusive(' ') {
            tokio::time::sleep(STEP / 4).await;
            self.emit(json!({"type": "btw_delta", "recordId": "btw-1", "delta": word}));
        }
        self.emit(json!({"type": "btw_record", "record": aside(&question, &answer, "complete")}));
    }

    async fn run(self: Arc<Self>, prompt: String) {
        tokio::time::sleep(STEP).await;
        self.emit(json!({"type": "agent_start"}));
        tokio::time::sleep(STEP).await;
        if prompt.to_lowercase().contains("ask") {
            self.say("Let me ask you first.").await;
            let (tx, rx) = oneshot::channel();
            *self.answer.lock().unwrap() = Some(tx);
            self.emit(
                json!({"type": "extension_ui_request", "id": "color", "method": "select",
                "title": "Pick a color", "options": ["red", "green", "blue"]}),
            );
            let Ok(response) = rx.await else {
                return;
            };
            let response = serde_json::to_value(&response).unwrap_or_default();
            let picked = response["value"].as_str().unwrap_or("nothing").to_owned();
            self.say(&format!("You picked {picked}.")).await;
        } else {
            self.say(&format!(
                "Echo: {prompt}\n\nThe area is $\\pi r^2$, and **this** is `code`."
            ))
            .await;
        }
        tokio::time::sleep(STEP).await;
        self.settle("completed");
    }
}

/// omp's record of the side question `question` (topic `btw-1`).
fn aside(question: &str, answer: &str, status: &str) -> Value {
    json!({"id": "btw-1", "leafId": null, "question": question, "answer": answer,
        "status": status, "createdAt": 1, "updatedAt": 1})
}

struct ScriptedSession {
    script: Arc<Script>,
    session_file: PathBuf,
}

impl AgentSession for ScriptedSession {
    fn session_file(&self) -> &Path {
        &self.session_file
    }

    fn prompt(&self, message: String, _: Vec<ImageContent>) -> BoxFuture<'_, Result<PromptAck>> {
        tokio::spawn(Arc::clone(&self.script).run(message));
        Box::pin(async {
            Ok(PromptAck {
                agent_invoked: Some(true),
            })
        })
    }

    fn steer(&self, _: String, _: Vec<ImageContent>) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn abort(&self) -> BoxFuture<'_, Result<()>> {
        self.script.answer.lock().unwrap().take();
        self.script.settle("aborted");
        Box::pin(async { Ok(()) })
    }

    fn set_model(&self, _: String, _: String) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn set_thinking_level(&self, _: OmpThinkingLevel) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn get_state(&self) -> BoxFuture<'_, Result<SessionState>> {
        Box::pin(async {
            Ok(serde_json::from_value(json!({
                "sessionId": "scripted",
                "model": {"id": "scripted", "name": "Scripted", "api": "x", "provider": "test",
                  "baseUrl": "x", "reasoning": true, "input": ["text"],
                  "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
                  "contextWindow": null, "maxTokens": null},
                "thinkingLevel": "low",
            }))
            .expect("a scripted session state"))
        })
    }

    fn available_commands(&self) -> BoxFuture<'_, Result<Vec<AvailableSlashCommand>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn goal(
        &self,
        op: GoalOp,
        _: Option<String>,
        _: Option<i64>,
    ) -> BoxFuture<'_, Result<GoalResult>> {
        Box::pin(async move {
            Err(omp::Error::Rpc(omp_rpc::Error::Command {
                command: format!("goal {}", op.as_str()),
                error: "Goals are not scripted.".into(),
                code: None,
            }))
        })
    }

    fn cancel_subagent(&self, _: String) -> BoxFuture<'_, Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    fn subagent_messages(&self, _: String) -> BoxFuture<'_, Result<SubagentMessages>> {
        Box::pin(async {
            Ok(
                serde_json::from_value(json!({"sessionFile": "/dev/null", "fromByte": 0,
                "nextByte": 0, "reset": false, "entries": [], "messages": []}))
                .expect("empty subagent messages"),
            )
        })
    }

    fn btw(
        &self,
        question: String,
        _: Option<String>,
    ) -> BoxFuture<'_, Result<omp_rpc::BtwHistoryRecord>> {
        let record = aside(&question, "", "running");
        tokio::spawn(Arc::clone(&self.script).answer_aside(question));
        Box::pin(async move { Ok(serde_json::from_value(record).expect("a btw record")) })
    }

    fn btw_cancel(&self, _: String) -> BoxFuture<'_, Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    fn respond_ui(&self, response: ExtensionUiResponse) -> Result<()> {
        if let Some(answer) = self.script.answer.lock().unwrap().take() {
            let _ = answer.send(response);
        }
        Ok(())
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async {})
    }
}

struct ScriptedFactory {
    sessions: PathBuf,
    count: AtomicU64,
}

impl SessionFactory for ScriptedFactory {
    fn start(
        &self,
        _cwd: PathBuf,
        _resume: Option<PathBuf>,
    ) -> BoxFuture<'static, Result<(Box<dyn AgentSession>, Events)>> {
        let n = self.count.fetch_add(1, Ordering::Relaxed);
        let session_file = self.sessions.join(format!("session-{n}.jsonl"));
        Box::pin(async move {
            let (events, rx) = mpsc::unbounded_channel();
            let script = Arc::new(Script {
                events,
                answer: Mutex::new(None),
                messages: AtomicU64::new(0),
            });
            let session = ScriptedSession {
                script,
                session_file,
            };
            Ok((Box::new(session) as Box<dyn AgentSession>, rx))
        })
    }
}

struct Args {
    listen: SocketAddr,
    dir: PathBuf,
}

fn args() -> Args {
    let mut listen: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut dir = std::env::temp_dir().join(format!("j3-test-server-{}", std::process::id()));
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().unwrap_or_else(|| usage(&arg));
        match arg.as_str() {
            "--port" => listen.set_port(value.parse().unwrap_or_else(|_| usage(&arg))),
            "--listen" => listen = value.parse().unwrap_or_else(|_| usage(&arg)),
            "--dir" => dir = value.into(),
            _ => usage(&arg),
        }
    }
    Args { listen, dir }
}

fn usage(arg: &str) -> ! {
    eprintln!(
        "j3-test-server: bad argument {arg}\n\
         usage: j3-test-server [--port N | --listen ADDR:PORT] [--dir DIR]"
    );
    std::process::exit(2);
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[tokio::main]
async fn main() {
    let Args { listen, dir } = args();
    let _ = std::fs::remove_dir_all(&dir);
    let data_dir = dir.join("data");
    let project_dir = dir.join("smoke");
    let sessions = dir.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    write(&project_dir.join("README.md"), "# Smoke\n");
    write(&project_dir.join("src/main.rs"), "fn main() {}\n");
    write(&project_dir.join("docs/notes.md"), "Notes.\n");

    let app = App::open_with(
        AppConfig {
            data_dir: data_dir.clone(),
            label: "smoke".into(),
            // No real omp: the model catalog fails fast.
            omp: OmpLauncher::new(data_dir.join("no-omp")),
        },
        Arc::new(ScriptedFactory {
            sessions,
            count: AtomicU64::new(0),
        }),
        Duration::from_secs(3600),
    )
    .await
    .expect("open the app");
    let server = Server::bind(app, &[listen]).await.expect("bind");
    let addr = server.addrs()[0];
    let url = format!("http://{addr}");

    let connection = Connection::open(url.clone(), tokio::runtime::Handle::current());
    let mut state = connection.state();
    tokio::time::timeout(
        Duration::from_secs(15),
        state.wait_for(|s| s.status == Status::Connected && s.shell.synced),
    )
    .await
    .expect("connect to self")
    .expect("connection state");
    let model = ModelRef {
        provider: "test".into(),
        id: "scripted".into(),
        thinking: Some(ThinkingLevel::Low),
    };
    let project = connection
        .projects_create(CreateProjectParams {
            title: "Smoke".into(),
            path: project_dir.to_string_lossy().into_owned(),
            default_model: Some(model.clone()),
            command_id: None,
        })
        .await
        .expect("create the project")
        .project;
    connection
        .threads_create(CreateThreadParams {
            project_id: project.id,
            model,
            mode: None,
            title: Some("Hello".into()),
            text: "Hello".into(),
            attachments: Vec::new(),
            command_id: None,
        })
        .await
        .expect("create the thread");
    connection.close().await;

    println!("listening on {url}");
    tokio::signal::ctrl_c().await.ok();
    server.shutdown().await;
}
