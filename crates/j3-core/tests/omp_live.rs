//! Live tests against a real `omp` on PATH. They spend model tokens, so they
//! are ignored by default:
//!
//! ```sh
//! J3_TEST_MODEL=<provider/model> cargo test -p j3-core --test omp_live -- --ignored
//! ```
//!
//! Without `J3_TEST_MODEL` omp uses its configured default model.

use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use j3_core::omp::{
    OmpEvents, OmpLauncher, OmpSession,
    omp_rpc::{
        AgentMessage, AnswersUiResponse, AskAnswer, AssistantContent, Event, ExtensionUiRequest,
        ExtensionUiResponse, GetMessagesPageCommand, PromptCommand, PromptStatus, RpcAgentEvent,
        RpcNotification,
    },
};

const RUN_TIMEOUT: Duration = Duration::from_secs(240);

fn launcher() -> OmpLauncher {
    match std::env::var("J3_TEST_MODEL") {
        Ok(model) => OmpLauncher::default().arg("--model").arg(model),
        Err(_) => OmpLauncher::default(),
    }
}

/// Fresh project directory; omp keys its session directory by cwd.
fn project_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("j3-omp-live-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[derive(Debug, Default)]
struct Run {
    /// Visible text of every assistant message, in order.
    assistant_text: String,
    asks: usize,
    status: Option<PromptStatus>,
    /// Other frame types seen, for failure diagnostics.
    kinds: Vec<String>,
}

/// Prompts and drives the session until it settles, answering every `ask`
/// question with `choice`.
async fn run(session: &OmpSession, events: &mut OmpEvents, message: &str, choice: &str) -> Run {
    // Drop frames left over from startup or an earlier run.
    while events.try_recv().is_ok() {}
    let ack = session
        .call(PromptCommand {
            message: message.into(),
            images: None,
            streaming_behavior: None,
        })
        .await
        .expect("prompt accepted");
    assert_ne!(
        ack.agent_invoked,
        Some(false),
        "prompt did not invoke the agent"
    );

    let mut run = Run::default();
    tokio::time::timeout(RUN_TIMEOUT, async {
        while let Some(event) = events.recv().await {
            let Event::Notification(notification) = event else {
                continue;
            };
            match notification {
                RpcNotification::ExtensionUiRequest(ExtensionUiRequest::Ask(ask)) => {
                    run.asks += 1;
                    let answers = ask
                        .questions
                        .iter()
                        .map(|question| AskAnswer {
                            id: question.id.clone(),
                            selected_options: vec![choice.into()],
                            custom_input: None,
                        })
                        .collect();
                    session
                        .respond_ui(ExtensionUiResponse::AnswersUiResponse(AnswersUiResponse {
                            id: ask.id,
                            answers,
                        }))
                        .expect("answer queued");
                }
                RpcNotification::ExtensionUiRequest(
                    request @ (ExtensionUiRequest::Select(_)
                    | ExtensionUiRequest::Confirm(_)
                    | ExtensionUiRequest::Input(_)
                    | ExtensionUiRequest::Editor(_)),
                ) => panic!("unexpected blocking dialog: {request:?}"),
                RpcNotification::RpcAgentEvent(RpcAgentEvent::MessageEnd(end)) => {
                    if let AgentMessage::Assistant(message) = end.message {
                        for block in message.content.iter().flatten() {
                            if let AssistantContent::Text(text) = block {
                                run.assistant_text
                                    .push_str(text.text.as_deref().unwrap_or_default());
                            }
                        }
                    }
                }
                RpcNotification::PromptResult(result) => {
                    run.status = Some(result.status);
                    if result.session_settled {
                        return;
                    }
                }
                RpcNotification::SessionSettled(_) if run.status.is_some() => return,
                other => {
                    let value = serde_json::to_value(&other).unwrap_or_default();
                    run.kinds
                        .push(value["type"].as_str().unwrap_or("?").to_owned());
                }
            }
        }
        panic!("omp exited mid-run: {run:?}");
    })
    .await
    .unwrap_or_else(|_| panic!("run did not settle within {RUN_TIMEOUT:?}"));
    run
}

/// Drains remaining frames; true once the channel reports closed.
async fn drain_closed(events: &mut OmpEvents) -> bool {
    tokio::time::timeout(Duration::from_secs(5), async {
        while events.recv().await.is_some() {}
    })
    .await
    .is_ok()
}

fn process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn remove_session(session_file: &Path) {
    let _ = std::fs::remove_file(session_file);
    // omp keeps per-session artifacts in a sibling directory named after the file stem.
    if let Some(stem) = session_file.file_stem() {
        let _ = std::fs::remove_dir_all(session_file.with_file_name(stem));
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "spawns real omp and spends model tokens"]
async fn ask_dialog_then_resume_by_session_file() {
    let project = project_dir();
    let launcher = launcher();

    let (session, mut events) = launcher.start(&project, None).await.expect("omp starts");
    let session_id = session.session_id().to_owned();
    let session_file = session.session_file().to_owned();
    assert!(!session_id.is_empty());

    let first = run(
		&session,
		&mut events,
		"Use the ask tool exactly once to ask me which color I prefer, offering exactly two options \
		 labeled \"red\" and \"blue\". After I answer, reply with only the color I chose.",
		"blue",
	)
	.await;
    assert_eq!(first.status, Some(PromptStatus::Completed), "{first:?}");
    assert_eq!(first.asks, 1, "{first:?}");
    assert!(
        first.assistant_text.to_lowercase().contains("blue"),
        "{first:?}"
    );

    // App threads must be normal omp sessions so `omp --resume` can open them.
    assert!(
        session_file.is_file(),
        "session file {session_file:?} not written"
    );
    let agent_dir = std::env::var_os("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").expect("HOME set")).join(".omp/agent")
        });
    assert!(
        session_file.starts_with(agent_dir.join("sessions")),
        "{session_file:?}"
    );

    let pid = session.pid().expect("spawned process");
    session.close().await.expect("clean shutdown");
    assert!(drain_closed(&mut events).await, "events end after close");
    assert!(!process_alive(pid), "omp {pid} still running after close");

    let (resumed, mut events) = launcher
        .start(&project, Some(&session_file))
        .await
        .expect("omp resumes");
    assert_eq!(resumed.session_id(), session_id);
    assert_eq!(resumed.session_file(), session_file);
    let page = resumed
        .call(GetMessagesPageCommand {
            cursor: None,
            limit: Some(256),
        })
        .await
        .expect("history page");
    assert!(
        page.total_messages >= 2,
        "resumed history has {} messages",
        page.total_messages
    );

    let second = run(
        &resumed,
        &mut events,
        "Which color did I choose earlier? Reply with only that color.",
        "red",
    )
    .await;
    assert_eq!(second.status, Some(PromptStatus::Completed), "{second:?}");
    assert_eq!(second.asks, 0, "{second:?}");
    assert!(
        second.assistant_text.to_lowercase().contains("blue"),
        "{second:?}"
    );

    resumed.close().await.expect("clean shutdown");
    remove_session(&session_file);
    let _ = std::fs::remove_dir_all(&project);
}
