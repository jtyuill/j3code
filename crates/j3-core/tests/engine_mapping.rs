//! The omp event → projection state machine over recorded omp frames
//! (`tests/fixtures/omp/*.jsonl`, captured from `omp --mode rpc` with
//! `messageUpdates:"delta"`). Every produced stream item is decoded back
//! through `j3_protocol::t3`.

use j3_core::{
    engine::{
        actor::commit_thread,
        model::{Effect, NewThread, ThreadModel, UserInput},
    },
    hub::{Hub, ThreadEvent},
    omp::omp_rpc::{
        AnswersUiResponse, AskAnswer, Event, ExtensionUiResponse, GoalResult, RpcNotification,
        SessionState, SubagentMessages,
    },
    store::Store,
};
use j3_protocol::t3::{
    model::ModelSelection,
    orchestration::{
        Actor, CreationSource, DomainEventBody, MessageRole, PlanArtifactKind, PlanArtifactStatus,
        PlanStepStatus, ProviderGoalStatus, ProviderInteractionMode, RunStatus, RuntimeMode,
        RuntimeRequestStatus, SubagentOrigin, SubagentStatus, ThreadProjection, ThreadShell,
        ThreadStreamItem, TurnItemKind, TurnItemStatus, UserMessageInputIntent,
    },
};
use serde_json::{Value, json};
use tokio::sync::broadcast;

const MODEL: &str = "opencode-go/glm-5.3-flash";

struct Harness {
    hub: Hub,
    model: ThreadModel,
    rx: broadcast::Receiver<ThreadEvent>,
    last_shell: Option<Value>,
    last_sequence: u64,
    events: Vec<DomainEventBody>,
    /// Subagents whose transcript the actor would read next.
    fetches: Vec<String>,
}

impl Harness {
    fn new() -> Self {
        let hub = Hub::new(Store::open_in_memory().unwrap());
        let rx = hub.subscribe_threads();
        let model = ThreadModel::create(NewThread {
            id: "T1".into(),
            project_id: "P1".into(),
            title: "Fixture".into(),
            model_selection: selection(),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: ProviderInteractionMode::Default,
            created_by: Actor::User,
            creation_source: CreationSource::Mobile,
            cwd: "/tmp".into(),
        });
        let mut harness = Self {
            hub,
            model,
            rx,
            last_shell: None,
            last_sequence: 0,
            events: Vec::new(),
            fetches: Vec::new(),
        };
        harness.commit();
        harness
    }

    /// Commits pending changes and decodes every broadcast item strictly.
    fn commit(&mut self) {
        commit_thread(
            &self.hub,
            &mut self.model,
            None,
            false,
            &mut self.last_shell,
            None,
        )
        .unwrap();
        while let Ok(event) = self.rx.try_recv() {
            assert!(event.sequence > self.last_sequence, "sequences increase");
            self.last_sequence = event.sequence;
            let item: ThreadStreamItem = serde_json::from_value((*event.item).clone())
                .unwrap_or_else(|error| panic!("bad stream item {error}: {}", event.item));
            let ThreadStreamItem::Event(item) = item else {
                panic!("expected an event item: {}", event.item);
            };
            assert!(
                !matches!(item.event.body, DomainEventBody::Unknown(_)),
                "unknown domain event {}",
                event.item
            );
            // Re-encoding gives back the same JSON (no lossy fields).
            assert_eq!(
                serde_json::to_value(&item).unwrap()["event"],
                event.item["event"]
            );
            self.events.push(item.event.body);
        }
        if let Some(shell) = &self.last_shell {
            let _: ThreadShell = serde_json::from_value(shell.clone()).unwrap();
        }
    }

    fn start(&mut self, text: &str) -> String {
        let run = self.model.start_run(&UserInput {
            message_id: "msg-user-1".into(),
            text: text.into(),
            attachments: Vec::new(),
            context: None,
            model_selection: selection(),
            created_by: Actor::User,
            creation_source: CreationSource::Mobile,
        });
        self.commit();
        run
    }

    /// Feeds a fixture; `answer` handles each newly pending runtime request.
    fn replay(&mut self, fixture: &str, mut answer: impl FnMut(&mut ThreadModel, &str)) {
        for line in fixture.lines().filter(|line| !line.trim().is_empty()) {
            let value: Value = serde_json::from_str(line).unwrap();
            if value["type"] == "response" {
                // The actor applies these command results (and `get_state`).
                match value["command"].as_str() {
                    Some("get_state") => {
                        let state: SessionState =
                            serde_json::from_value(value["data"].clone()).unwrap();
                        self.model.apply_state(&state);
                    }
                    Some("goal") if value["success"] == true => {
                        let result: GoalResult =
                            serde_json::from_value(value["data"].clone()).unwrap();
                        self.model.apply_goal(result.goal.as_ref());
                    }
                    Some("get_subagent_messages") if value["success"] == true => {
                        let transcript: SubagentMessages =
                            serde_json::from_value(value["data"].clone()).unwrap();
                        let subagent_id = self.fetches.remove(0);
                        self.model
                            .apply_subagent_transcript(&subagent_id, &transcript);
                    }
                    _ => {}
                }
                self.commit();
                continue;
            }
            let notification = RpcNotification::from_value(value).unwrap();
            let pending_before = self.pending_requests();
            for effect in self.model.apply(&Event::Notification(notification)) {
                if let Effect::FetchSubagentResult { subagent_id } = effect {
                    self.fetches.push(subagent_id);
                }
            }
            self.commit();
            for request_id in self.pending_requests() {
                if !pending_before.contains(&request_id) {
                    answer(&mut self.model, &request_id);
                    self.commit();
                }
            }
        }
    }

    fn pending_requests(&self) -> Vec<String> {
        self.model
            .proj
            .requests
            .iter()
            .filter(|request| request.status == RuntimeRequestStatus::Pending)
            .map(|request| request.id.clone())
            .collect()
    }

    /// The snapshot projection, decoded strictly.
    fn projection(&self) -> ThreadProjection {
        let json = serde_json::to_value(self.model.proj.to_wire()).unwrap();
        serde_json::from_value(json).unwrap()
    }
}

fn selection() -> ModelSelection {
    serde_json::from_value(json!({"instanceId": "omp", "model": MODEL})).unwrap()
}

fn assistant_texts(projection: &ThreadProjection) -> Vec<String> {
    projection
        .visible_turn_items
        .iter()
        .filter_map(|row| match &row.item.kind {
            TurnItemKind::AssistantMessage(message) => {
                assert!(!message.streaming);
                Some(message.text.clone())
            }
            _ => None,
        })
        .collect()
}

fn assert_completed_run(projection: &ThreadProjection) {
    let run = &projection.runs[0];
    assert_eq!(run.status, RunStatus::Completed);
    assert!(run.completed_at.is_some());
    assert_eq!(run.user_message_id, "msg-user-1");
    assert_eq!(projection.attempts.len(), 1);
    assert_eq!(projection.provider_turns.len(), 1);
    assert!(
        projection
            .visible_turn_items
            .iter()
            .all(|row| row.item.status != TurnItemStatus::Running)
    );
    let positions: Vec<u64> = projection
        .visible_turn_items
        .iter()
        .map(|row| row.position)
        .collect();
    assert_eq!(positions, (0..positions.len() as u64).collect::<Vec<_>>());
    // The first feed row is the user's own message.
    let TurnItemKind::UserMessage(user) = &projection.visible_turn_items[0].item.kind else {
        panic!("first row is the user message");
    };
    assert_eq!(user.message_id, "msg-user-1");
    assert_eq!(user.input_intent, UserMessageInputIntent::TurnStart);
}

#[test]
fn engine_mapping_text_answer() {
    let mut harness = Harness::new();
    harness.start("Reply with exactly the words: hello world.");
    harness.replay(include_str!("fixtures/omp/text.jsonl"), |_, _| {
        panic!("no questions")
    });
    let projection = harness.projection();
    assert_completed_run(&projection);
    assert_eq!(assistant_texts(&projection), ["hello world"]);
    let assistant = projection
        .messages
        .iter()
        .find(|message| message.role == MessageRole::Assistant)
        .unwrap();
    assert_eq!(assistant.text, "hello world");
    assert!(!assistant.streaming);
    let shell = harness.model.proj.shell();
    assert_eq!(shell.latest_visible_message.unwrap().text, "hello world");
    assert!(harness.model.is_settled());
    // Lifecycle events were emitted in order.
    let kinds: Vec<&str> = harness
        .events
        .iter()
        .filter_map(|body| match body {
            DomainEventBody::RunCreated(_) => Some("run.created"),
            DomainEventBody::RunAttemptCreated(_) => Some("run-attempt.created"),
            DomainEventBody::RunUpdated(run) if run.status == RunStatus::Completed => {
                Some("run.completed")
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        ["run.created", "run-attempt.created", "run.completed"]
    );
}

#[test]
fn engine_mapping_bash_tool() {
    let mut harness = Harness::new();
    harness.start("Use the bash tool to run exactly: echo hi-from-bash");
    harness.replay(include_str!("fixtures/omp/bash.jsonl"), |_, _| {
        panic!("no questions")
    });
    let projection = harness.projection();
    assert_completed_run(&projection);
    let command = projection
        .visible_turn_items
        .iter()
        .find_map(|row| match &row.item.kind {
            TurnItemKind::CommandExecution(command) => Some((row.item.clone(), command.clone())),
            _ => None,
        })
        .expect("bash maps to command_execution");
    assert_eq!(command.1.input, "echo hi-from-bash");
    assert_eq!(command.1.output.as_deref(), Some("hi-from-bash\n"));
    assert_eq!(command.0.status, TurnItemStatus::Completed);
    assert_eq!(command.0.title.as_deref(), Some("Echoing test string"));
    assert_eq!(assistant_texts(&projection), ["done"]);
}

#[test]
fn engine_mapping_ask_question() {
    let mut harness = Harness::new();
    harness.start("Use the ask tool to ask me which color I prefer.");
    let mut answered = None;
    harness.replay(
        include_str!("fixtures/omp/ask.jsonl"),
        |model, request_id| {
            let run = model.proj.active_run().unwrap();
            assert_eq!(run.status, RunStatus::Waiting);
            let answers = [("question".to_owned(), json!("red"))]
                .into_iter()
                .collect();
            answered = Some(model.answer(request_id, &answers).unwrap());
            assert_eq!(model.proj.active_run().unwrap().status, RunStatus::Running);
        },
    );
    assert_eq!(
        answered,
        Some(ExtensionUiResponse::AnswersUiResponse(AnswersUiResponse {
            id: "15bd2315c1daa63b".into(),
            answers: vec![AskAnswer {
                id: "question".into(),
                selected_options: vec!["red".into()],
                custom_input: None,
            }],
        }))
    );
    let projection = harness.projection();
    assert_completed_run(&projection);
    let request = &projection.runtime_requests[0];
    assert_eq!(request.status, RuntimeRequestStatus::Resolved);
    let (item, question) = projection
        .visible_turn_items
        .iter()
        .find_map(|row| match &row.item.kind {
            TurnItemKind::UserInputRequest(question) => Some((row.item.clone(), question.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(item.status, TurnItemStatus::Completed);
    assert_eq!(question.request_id, request.id);
    assert_eq!(question.questions[0].question, "Which color do you prefer?");
    assert_eq!(question.questions[0].header, "Question");
    assert_eq!(question.questions[0].options[0].label, "red");
    assert_eq!(question.questions[0].options[0].description, "red");
    let answer = question.question_answer.unwrap();
    assert_eq!(answer.answers["question"], json!("red"));
    assert_eq!(assistant_texts(&projection), ["red"]);
    // Answering twice is refused.
    let answers = [("question".to_owned(), json!("blue"))]
        .into_iter()
        .collect();
    assert!(harness.model.answer(&request.id, &answers).is_err());
}

#[test]
fn engine_mapping_todos() {
    let mut harness = Harness::new();
    harness.start("Use the todo tool to create a todo list with two tasks.");
    harness.replay(include_str!("fixtures/omp/todo.jsonl"), |_, _| {
        panic!("no questions")
    });
    let projection = harness.projection();
    assert_completed_run(&projection);
    let plan = &projection.plans[0];
    assert_eq!(plan.status, PlanArtifactStatus::Completed);
    let PlanArtifactKind::TodoList { steps, .. } = &plan.kind else {
        panic!("todo plan");
    };
    let texts: Vec<(&str, PlanStepStatus)> = steps
        .iter()
        .map(|step| (step.text.as_str(), step.status))
        .collect();
    assert_eq!(
        texts,
        [
            ("alpha", PlanStepStatus::Completed),
            ("beta", PlanStepStatus::Completed)
        ]
    );
    let todo_items: Vec<_> = projection
        .visible_turn_items
        .iter()
        .filter(|row| matches!(row.item.kind, TurnItemKind::TodoList(_)))
        .collect();
    assert_eq!(
        todo_items.len(),
        1,
        "one todo row per run, updated in place"
    );
    // Plan updates were emitted while the run progressed.
    let plan_updates = harness
        .events
        .iter()
        .filter(|body| matches!(body, DomainEventBody::PlanUpdated(_)))
        .count();
    assert!(plan_updates >= 2);
    // The thinking block became a reasoning row.
    assert!(projection.visible_turn_items.iter().any(
        |row| matches!(&row.item.kind, TurnItemKind::Reasoning(r) if r.text.contains("beta"))
    ));
}

#[test]
fn engine_mapping_streaming_text_is_coalescable() {
    let mut harness = Harness::new();
    harness.start("hello");
    let feed = |harness: &mut Harness, value: Value| {
        harness.model.apply(&Event::Notification(
            RpcNotification::from_value(value).unwrap(),
        ));
    };
    feed(&mut harness, json!({"type": "agent_start"}));
    feed(
        &mut harness,
        json!({"type": "message_update", "messageId": "m1", "message": {"role": "assistant"},
               "assistantMessageEvent": {"type": "text_start", "contentIndex": 0}}),
    );
    assert!(
        harness.model.proj.has_urgent(),
        "a new item is committed at once"
    );
    harness.commit();
    feed(
        &mut harness,
        json!({"type": "message_update", "messageId": "m1", "message": {"role": "assistant"},
               "assistantMessageEvent": {"type": "text_delta", "contentIndex": 0, "delta": "Hel"}}),
    );
    feed(
        &mut harness,
        json!({"type": "message_update", "messageId": "m1", "message": {"role": "assistant"},
               "assistantMessageEvent": {"type": "text_delta", "contentIndex": 0, "delta": "lo"}}),
    );
    assert!(harness.model.proj.has_pending());
    assert!(
        !harness.model.proj.has_urgent(),
        "deltas wait for the coalescing window"
    );
    let before = harness.events.len();
    harness.commit();
    // Two deltas → one item update plus one message update.
    let updates: Vec<_> = harness.events[before..].iter().collect();
    assert_eq!(updates.len(), 2);
    assert!(updates.iter().any(|body| matches!(body,
        DomainEventBody::TurnItemUpdated(item)
            if matches!(&item.kind, TurnItemKind::AssistantMessage(m) if m.text == "Hello" && m.streaming))));
}

#[test]
fn engine_mapping_prompt_error_fails_run_with_error_item() {
    let mut harness = Harness::new();
    harness.start("hello");
    harness.model.apply(&Event::Notification(
        RpcNotification::from_value(json!({"type": "agent_start"})).unwrap(),
    ));
    harness.model.apply(&Event::Notification(
        RpcNotification::from_value(json!({
            "type": "prompt_result", "agentInvoked": true, "status": "error",
            "sessionSettled": true,
            "error": {"message": "rate limited", "retryable": true, "httpStatus": 429}
        }))
        .unwrap(),
    ));
    harness.commit();
    let projection = harness.projection();
    assert_eq!(projection.runs[0].status, RunStatus::Failed);
    let error = projection
        .turn_items
        .iter()
        .find_map(|item| match &item.kind {
            TurnItemKind::Error(error) => Some(error.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(error.failure.message, "rate limited");
    assert_eq!(error.failure.retryable, Some(true));
    let shell = harness.model.proj.shell();
    assert_eq!(shell.last_error, Some(Some("rate limited".into())));
}

#[test]
fn engine_mapping_trailing_settle_does_not_end_the_next_run() {
    let mut harness = Harness::new();
    harness.start("first");
    let feed = |harness: &mut Harness, value: Value| {
        harness.model.apply(&Event::Notification(
            RpcNotification::from_value(value).unwrap(),
        ))
    };
    feed(&mut harness, json!({"type": "agent_start"}));
    let effects = feed(
        &mut harness,
        json!({"type": "prompt_result", "agentInvoked": true, "status": "completed", "sessionSettled": true}),
    );
    assert_eq!(effects, [j3_core::engine::model::Effect::RunSettled]);
    // The next (queued) run starts before omp's trailing `session_settled`.
    let second = harness.model.start_run(&UserInput {
        message_id: "msg-user-2".into(),
        text: "second".into(),
        attachments: Vec::new(),
        context: None,
        model_selection: selection(),
        created_by: Actor::User,
        creation_source: CreationSource::Mobile,
    });
    assert!(feed(&mut harness, json!({"type": "session_settled"})).is_empty());
    harness.commit();
    let projection = harness.projection();
    let run = projection.runs.iter().find(|run| run.id == second).unwrap();
    assert_eq!(run.status, RunStatus::Starting);
    assert_eq!(projection.runs[0].status, RunStatus::Completed);
}

/// The `subagent` row of `subagent_id` and the `task` tool row it hangs off.
fn subagent_row<'a>(
    projection: &'a ThreadProjection,
    subagent_id: &str,
) -> &'a j3_protocol::t3::orchestration::TurnItem {
    let row = projection
        .visible_turn_items
        .iter()
        .find(|row| {
            matches!(&row.item.kind, TurnItemKind::Subagent(body) if body.subagent_id == subagent_id)
        })
        .expect("a subagent feed row");
    &row.item
}

#[test]
fn engine_mapping_subagent() {
    let mut harness = Harness::new();
    harness.start("Use the task tool to run exactly one subagent that replies PONG.");
    harness.replay(include_str!("fixtures/omp/subagent.jsonl"), |_, _| {
        panic!("no questions")
    });
    assert!(harness.fetches.is_empty(), "the transcript was read");
    let projection = harness.projection();
    assert_completed_run(&projection);
    assert_eq!(projection.subagents.len(), 1);
    let subagent = &projection.subagents[0];
    assert_eq!(subagent.status, SubagentStatus::Completed);
    assert_eq!(subagent.origin, SubagentOrigin::ProviderNative);
    assert_eq!(subagent.driver, "pi");
    assert_eq!(subagent.run_id.as_ref(), Some(&projection.runs[0].id));
    assert_eq!(
        Some(&subagent.parent_node_id),
        projection.runs[0].root_node_id.as_ref()
    );
    assert_eq!(subagent.title.as_deref(), Some("task"));
    assert!(
        subagent
            .prompt
            .starts_with("Reply with exactly the word PONG")
    );
    assert_eq!(subagent.model.as_deref(), Some(MODEL));
    // The final answer comes from the subagent's transcript.
    assert_eq!(subagent.result.as_deref(), Some("PONG"));
    assert_eq!(subagent.progress.as_deref(), Some("13.9k tokens"));
    assert!(subagent.started_at.is_some() && subagent.completed_at.is_some());
    assert_eq!(
        subagent
            .native_task_ref
            .as_ref()
            .unwrap()
            .native_id
            .as_ref(),
        Some(&subagent.id)
    );

    // Its feed row mirrors it, under the `task` tool row that started it.
    let item = subagent_row(&projection, &subagent.id);
    assert_eq!(item.status, TurnItemStatus::Completed);
    assert_eq!(item.run_id, subagent.run_id);
    let TurnItemKind::Subagent(body) = &item.kind else {
        unreachable!()
    };
    assert_eq!(body.result.as_deref(), Some("PONG"));
    assert_eq!(body.prompt, subagent.prompt);
    let parent = projection
        .turn_items
        .iter()
        .find(|candidate| Some(&candidate.id) == item.parent_item_id.as_ref())
        .expect("the task tool row");
    assert!(
        matches!(&parent.kind, TurnItemKind::DynamicTool(tool) if tool.tool_name.as_deref() == Some("task"))
    );
    assert!(parent.ordinal < item.ordinal);

    // Live: the entity streamed as `subagent.updated` events.
    let updates = harness
        .events
        .iter()
        .filter(|body| matches!(body, DomainEventBody::SubagentUpdated(_)))
        .count();
    assert!(updates >= 3, "{updates} subagent updates");
}

#[test]
fn engine_mapping_subagent_cancelled_then_revived() {
    let mut harness = Harness::new();
    harness.start("Use the task tool to run one subagent that sleeps 60 seconds.");
    let mut statuses = Vec::new();
    for line in include_str!("fixtures/omp/subagent-cancel.jsonl").lines() {
        harness.replay(line, |_, _| panic!("no questions"));
        if let Some(subagent) = harness.model.proj.subagents.first()
            && statuses.last() != Some(&subagent.status)
        {
            statuses.push(subagent.status);
        }
    }
    assert_eq!(
        statuses,
        [
            SubagentStatus::Running,
            SubagentStatus::Cancelled,
            SubagentStatus::Running,
            SubagentStatus::Completed,
        ]
    );
    let projection = harness.projection();
    assert_completed_run(&projection);
    assert_eq!(
        projection.subagents.len(),
        1,
        "a revived subagent is the same one"
    );
    let subagent = &projection.subagents[0];
    assert_eq!(subagent.title.as_deref(), Some("Run sleep 60 via bash"));
    // No transcript read in this recording: the last output lines stand in.
    assert_eq!(subagent.result.as_deref(), Some("DONE"));
    assert_eq!(harness.fetches.len(), 1);
    assert!(
        subagent
            .progress
            .as_deref()
            .is_some_and(|line| line.starts_with("Waiting for sleep job to finish · 2 tool calls"))
    );
    let item = subagent_row(&projection, &subagent.id);
    assert_eq!(item.status, TurnItemStatus::Completed);
    assert_eq!(
        projection
            .visible_turn_items
            .iter()
            .filter(|row| matches!(row.item.kind, TurnItemKind::Subagent(_)))
            .count(),
        1
    );
}

#[test]
fn engine_mapping_subagents_end_with_the_omp_process() {
    let mut harness = Harness::new();
    harness.start("Use the task tool to run one subagent that sleeps 60 seconds.");
    let fixture = include_str!("fixtures/omp/subagent-cancel.jsonl");
    let until_running: String = fixture
        .lines()
        .take_while(|line| !line.contains("\"cancel_subagent\""))
        .map(|line| format!("{line}\n"))
        .collect();
    harness.replay(&until_running, |_, _| panic!("no questions"));
    assert_eq!(
        harness.model.proj.subagents[0].status,
        SubagentStatus::Running
    );
    assert!(harness.model.needs_recovery());
    // A server restart: nothing survives in omp.
    harness.model.recover();
    harness.commit();
    let projection = harness.projection();
    assert_eq!(projection.subagents[0].status, SubagentStatus::Interrupted);
    assert_eq!(
        subagent_row(&projection, &projection.subagents[0].id).status,
        TurnItemStatus::Interrupted
    );
    assert!(!harness.model.needs_recovery());
}

#[test]
fn engine_mapping_goal_lifecycle() {
    let mut harness = Harness::new();
    harness.start("Work on the goal now.");
    harness.replay(include_str!("fixtures/omp/goal.jsonl"), |_, _| {
        panic!("no questions")
    });
    let projection = harness.projection();
    assert_completed_run(&projection);
    // create → pause → resume → the agent completes it → a second goal is
    // created and dropped. omp's `get_state.goal` is null right after the
    // completion; j3 keeps showing the completed goal until the next one.
    let mut statuses = Vec::new();
    let mut completed = None;
    for body in &harness.events {
        let DomainEventBody::ProviderThreadUpdated(thread) = body else {
            continue;
        };
        let status = thread.goal.as_ref().map(|goal| goal.status);
        if status == Some(ProviderGoalStatus::Complete) {
            completed = thread.goal.clone();
        }
        if statuses.last() != Some(&status) {
            statuses.push(status);
        }
    }
    assert_eq!(
        statuses,
        [
            None,
            Some(ProviderGoalStatus::Active),
            Some(ProviderGoalStatus::Paused),
            Some(ProviderGoalStatus::Active),
            Some(ProviderGoalStatus::Complete),
            Some(ProviderGoalStatus::Active),
            None,
        ]
    );
    let completed = completed.unwrap();
    assert_eq!(
        completed.objective,
        "Create a file named goal.txt containing the word done"
    );
    assert_eq!(completed.tokens_used, Some(15950));
    assert_eq!(completed.token_budget, Some(Some(200_000)));
    assert_eq!(completed.time_used_seconds, Some(7));
    assert!(projection.provider_threads[0].goal.is_none());
    assert!(!harness.model.goal_active());
    let shell: ThreadShell = serde_json::from_value(harness.last_shell.clone().unwrap()).unwrap();
    assert_eq!(shell.goal, None);
}

#[test]
fn engine_mapping_goal_shows_on_the_shell_and_pauses_with_omp() {
    let mut harness = Harness::new();
    let fixture = include_str!("fixtures/omp/goal.jsonl");
    let created: String = fixture
        .lines()
        .take_while(|line| !line.contains("\"status\":\"paused\""))
        .map(|line| format!("{line}\n"))
        .collect();
    harness.replay(&created, |_, _| panic!("no questions"));
    assert!(harness.model.goal_active());
    let shell: ThreadShell = serde_json::from_value(harness.last_shell.clone().unwrap()).unwrap();
    let goal = shell
        .goal
        .flatten()
        .expect("the shell row carries the goal");
    assert_eq!(goal.status, ProviderGoalStatus::Active);
    assert_eq!(goal.tokens_used, Some(0));
    // omp restores an active goal paused when its process starts again.
    harness.model.session_closed();
    harness.commit();
    assert_eq!(
        harness.projection().provider_threads[0]
            .goal
            .as_ref()
            .map(|goal| goal.status),
        Some(ProviderGoalStatus::Paused)
    );
    // A completed goal is cleared by j3 alone.
    assert!(!harness.model.clear_completed_goal());
}
