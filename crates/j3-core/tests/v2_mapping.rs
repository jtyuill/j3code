//! The omp event → v2 thread model over recorded omp frames
//! (`tests/fixtures/omp/*.jsonl`, captured from `omp --mode rpc` with
//! `messageUpdates:"delta"`). Every item a commit broadcasts is decoded back
//! through `j3_protocol::v2::streams::ThreadItem`.

use j3_core::{
    engine::{
        actor::commit_thread,
        model::{Effect, NewThread, ThreadModel, UserInput},
    },
    hub::{Hub, ThreadEvent},
    omp::omp_rpc::{
        AnswersUiResponse, AskAnswer, Event, ExtensionUiResponse, Goal as OmpGoal, RpcNotification,
        SessionState, SubagentMessages,
    },
    store::Store,
};
use j3_protocol::v2::{
    entities::{
        Answer, Delivery, GoalState, Item, ItemKind, ItemStatus, MessageRole, ModelRef,
        RequestKind, RequestStatus, SubagentItem, ThinkingLevel, Thread, ThreadMode, ThreadStatus,
        TodoStatus, ToolDetail, TurnStatus,
    },
    envelope::ErrorCode,
    streams::ThreadItem,
};
use serde_json::Value;
use tokio::sync::broadcast;

const MODEL: &str = "opencode-go/glm-5.3-flash";

fn model() -> ModelRef {
    ModelRef {
        provider: "opencode-go".into(),
        id: "glm-5.3-flash".into(),
        thinking: Some(ThinkingLevel::High),
    }
}

struct Harness {
    hub: Hub,
    model: ThreadModel,
    rx: broadcast::Receiver<ThreadEvent>,
    last_sequence: u64,
    /// The thread row of the latest broadcast, as wire JSON.
    last_row: Option<Value>,
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
            model: model(),
            mode: ThreadMode::Default,
        });
        let mut harness = Self {
            hub,
            model,
            rx,
            last_sequence: 0,
            last_row: None,
            fetches: Vec::new(),
        };
        harness.commit();
        harness
    }

    /// Commits pending changes and decodes every broadcast item strictly.
    fn commit(&mut self) {
        commit_thread(&self.hub, &mut self.model, None, false, None).unwrap();
        while let Ok(event) = self.rx.try_recv() {
            assert!(
                event.sequence > self.last_sequence,
                "sequences increase: {} after {}",
                event.sequence,
                self.last_sequence
            );
            self.last_sequence = event.sequence;
            for value in event.items.iter() {
                let item: ThreadItem = serde_json::from_value(value.clone())
                    .unwrap_or_else(|error| panic!("bad stream item {error}: {value}"));
                assert_eq!(
                    item.sequence(),
                    Some(event.sequence),
                    "the item carries its commit sequence"
                );
                assert_eq!(
                    serde_json::to_value(&item).unwrap(),
                    value.clone(),
                    "the stream item re-encodes"
                );
                match &item {
                    ThreadItem::Item { item, .. } => assert!(
                        !matches!(item.kind, ItemKind::Unknown),
                        "known item kind: {value}"
                    ),
                    ThreadItem::Thread { thread, .. } => {
                        self.last_row = Some(serde_json::to_value(thread.as_ref()).unwrap());
                    }
                    _ => {}
                }
            }
        }
    }

    fn start(&mut self, text: &str) -> String {
        let turn = self.model.start_turn(&UserInput {
            text: text.into(),
            attachments: Vec::new(),
            model: model(),
        });
        self.commit();
        turn
    }

    /// Feeds a fixture; `answer` handles each newly pending request.
    fn replay(&mut self, fixture: &str, mut answer: impl FnMut(&mut ThreadModel, &str)) {
        for line in fixture.lines().filter(|line| !line.trim().is_empty()) {
            let value: Value = serde_json::from_str(line).unwrap();
            if value["type"] == "response" {
                match value["command"].as_str() {
                    Some("get_state") if value["success"] == true => {
                        let state: SessionState =
                            serde_json::from_value(value["data"].clone()).unwrap();
                        self.model.apply_state(&state);
                    }
                    Some("goal") if value["success"] == true => {
                        let goal: Option<OmpGoal> =
                            serde_json::from_value(value["data"]["goal"].clone()).unwrap();
                        self.model.apply_goal(goal.as_ref());
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
            .requests
            .iter()
            .filter(|request| request.status == RequestStatus::Open)
            .map(|request| request.id.clone())
            .collect()
    }

    fn row(&self) -> Thread {
        serde_json::from_value(self.last_row.clone().expect("a committed row")).unwrap()
    }
}

fn assistant_texts(model: &ThreadModel) -> Vec<String> {
    model
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::Text(text) => {
                assert_eq!(
                    item.status,
                    ItemStatus::Completed,
                    "an assistant answer is complete"
                );
                Some(text.text.clone())
            }
            _ => None,
        })
        .collect()
}

fn subagent_item<'a>(
    model: &'a ThreadModel,
    subagent_id: &str,
) -> Option<(&'a Item, &'a SubagentItem)> {
    model.items.iter().rev().find_map(|item| match &item.kind {
        ItemKind::Subagent(body) if body.subagent_id == subagent_id => Some((item, body)),
        _ => None,
    })
}

fn assert_completed_turn(model: &ThreadModel, user_text: &str) {
    assert_eq!(model.turns.len(), 1, "one turn");
    let turn = &model.turns[0];
    assert_eq!(turn.status, TurnStatus::Completed);
    assert!(turn.started_at.is_some());
    assert!(turn.ended_at.is_some());
    assert!(turn.error.is_none());
    assert!(model.active_turn().is_none(), "no turn is active");
    assert!(model.is_idle());
    assert!(
        model
            .items
            .iter()
            .all(|item| item.status != ItemStatus::Running),
        "every item ended"
    );
    assert!(
        model
            .items
            .windows(2)
            .all(|pair| pair[0].ordinal < pair[1].ordinal),
        "feed order is strictly increasing"
    );
    // The first feed row is the user's own message.
    let ItemKind::User(user) = &model.items[0].kind else {
        panic!("the first item is the user message");
    };
    assert_eq!(user.text, user_text);
    assert_eq!(user.delivery, Delivery::Prompt);
}

/// Replays `text.jsonl`: one completed `text` item carries the final answer
/// and the turn ends completed.
#[test]
fn fixture_text_finishes_with_one_answer_item() {
    let mut harness = Harness::new();
    let prompt = "Reply with exactly the words: hello world.";
    harness.start(prompt);
    harness.replay(include_str!("fixtures/omp/text.jsonl"), |_, _| {
        panic!("no questions")
    });
    assert_completed_turn(&harness.model, prompt);
    assert_eq!(assistant_texts(&harness.model), ["hello world"]);
    assert_eq!(
        harness
            .model
            .items
            .iter()
            .filter(|item| matches!(item.kind, ItemKind::Text(_)))
            .count(),
        1
    );
    let row = harness.model.row();
    assert_eq!(row.status, ThreadStatus::Idle);
    let last = row
        .last_message
        .as_ref()
        .expect("the row carries the reply");
    assert_eq!(last.role, MessageRole::Assistant);
    assert_eq!(last.text, "hello world");
    assert_eq!(harness.row(), row);
}

/// Replays `bash.jsonl`: the call is a `tool` item named `bash` with a
/// command detail, the recorded output and its exit code.
#[test]
fn fixture_bash_maps_to_a_command_tool_item() {
    let mut harness = Harness::new();
    let prompt = "Use the bash tool to run exactly: echo hi-from-bash";
    harness.start(prompt);
    harness.replay(include_str!("fixtures/omp/bash.jsonl"), |_, _| {
        panic!("no questions")
    });
    assert_completed_turn(&harness.model, prompt);
    let (item, tool) = harness
        .model
        .items
        .iter()
        .find_map(|item| match &item.kind {
            ItemKind::Tool(tool) if tool.name == "bash" => Some((item, tool)),
            _ => None,
        })
        .expect("the bash call");
    assert_eq!(item.status, ItemStatus::Completed);
    assert_eq!(
        tool.detail,
        Some(ToolDetail::Command {
            command: "echo hi-from-bash".into()
        })
    );
    assert_eq!(tool.intent.as_deref(), Some("Echoing test string"));
    let output = tool.output.as_ref().expect("the command output");
    assert_eq!(output.text, "hi-from-bash\n");
    assert_eq!(assistant_texts(&harness.model), ["done"]);
}

/// Replays `ask.jsonl`: omp's ask becomes an `ask` request with its
/// questions; the v2 answer maps back to omp's answers response and the turn
/// waits, then runs again, then completes.
#[test]
fn fixture_ask_becomes_an_answered_request() {
    let mut harness = Harness::new();
    let prompt = "Use the ask tool to ask me which color I prefer.";
    harness.start(prompt);
    let mut answered = None;
    harness.replay(
        include_str!("fixtures/omp/ask.jsonl"),
        |model, request_id| {
            assert_eq!(
                model.active_turn().unwrap().status,
                TurnStatus::Waiting,
                "the turn waits for the answer"
            );
            let answer = Answer {
                question_id: "question".into(),
                values: vec!["red".into()],
                other: None,
            };
            answered = Some(model.answer(request_id, &[answer]).unwrap());
            assert_eq!(
                model.active_turn().unwrap().status,
                TurnStatus::Running,
                "answering resumes the turn"
            );
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
    assert_completed_turn(&harness.model, prompt);
    assert_eq!(harness.model.requests.len(), 1);
    let request = &harness.model.requests[0];
    assert_eq!(request.kind, RequestKind::Ask);
    assert_eq!(request.status, RequestStatus::Answered);
    let question = &request.questions[0];
    assert_eq!(question.id, "question");
    assert_eq!(question.text, "Which color do you prefer?");
    assert!(question.allow_other);
    assert!(!question.multiple);
    let labels: Vec<&str> = question
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect();
    assert_eq!(labels, ["red", "blue"]);
    assert_eq!(
        request.answers.as_ref().unwrap(),
        &[Answer {
            question_id: "question".into(),
            values: vec!["red".into()],
            other: None,
        }]
    );
    assert_eq!(assistant_texts(&harness.model), ["red"]);
    // Answering twice is refused.
    let request_id = request.id.clone();
    let error = harness
        .model
        .answer(
            &request_id,
            &[Answer {
                question_id: "question".into(),
                values: vec!["blue".into()],
                other: None,
            }],
        )
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
}

/// Replays `todo.jsonl`: todo calls are `todo` items whose phases keep omp's
/// task statuses, and the run completes.
#[test]
fn fixture_todo_maps_to_todo_items() {
    let mut harness = Harness::new();
    let prompt = "Use the todo tool to create a todo list with two tasks.";
    harness.start(prompt);
    harness.replay(include_str!("fixtures/omp/todo.jsonl"), |_, _| {
        panic!("no questions")
    });
    assert_completed_turn(&harness.model, prompt);
    let first = harness
        .model
        .items
        .iter()
        .find_map(|item| match &item.kind {
            ItemKind::Todo(todo) => Some(todo),
            _ => None,
        })
        .expect("the init call");
    assert_eq!(first.phases.len(), 1);
    assert_eq!(first.phases[0].name, "Tasks");
    let initial: Vec<(&str, TodoStatus)> = first.phases[0]
        .tasks
        .iter()
        .map(|task| (task.text.as_str(), task.status))
        .collect();
    assert_eq!(
        initial,
        [
            ("alpha", TodoStatus::InProgress),
            ("beta", TodoStatus::Pending)
        ]
    );
    let (item, todo) = harness
        .model
        .items
        .iter()
        .rev()
        .find_map(|item| match &item.kind {
            ItemKind::Todo(todo) => Some((item, todo)),
            _ => None,
        })
        .expect("the final todo call");
    assert_eq!(item.status, ItemStatus::Completed);
    let finished: Vec<(&str, TodoStatus)> = todo.phases[0]
        .tasks
        .iter()
        .map(|task| (task.text.as_str(), task.status))
        .collect();
    assert_eq!(
        finished,
        [
            ("alpha", TodoStatus::Completed),
            ("beta", TodoStatus::Completed)
        ]
    );
}

/// Replays `goal.jsonl`: the row's `goal` follows omp, goal items record
/// each objective/state change, and a completed goal is dropped with omp's.
#[test]
fn fixture_goal_updates_the_row_and_adds_goal_items() {
    let mut harness = Harness::new();
    let prompt = "Work on the goal now.";
    harness.start(prompt);
    harness.replay(include_str!("fixtures/omp/goal.jsonl"), |_, _| {
        panic!("no questions")
    });
    assert_completed_turn(&harness.model, prompt);
    let goals: Vec<&j3_protocol::v2::entities::Goal> = harness
        .model
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::Goal(goal) => Some(goal),
            _ => None,
        })
        .collect();
    let states: Vec<GoalState> = goals.iter().map(|goal| goal.state).collect();
    assert_eq!(
        states,
        [
            GoalState::Active,
            GoalState::Paused,
            GoalState::Active,
            GoalState::Complete,
            GoalState::Active,
            GoalState::Dropped,
        ]
    );
    let completed = goals
        .iter()
        .find(|goal| goal.state == GoalState::Complete)
        .expect("the completed goal");
    assert_eq!(
        completed.objective,
        "Create a file named goal.txt containing the word done"
    );
    assert_eq!(completed.tokens_used, Some(15950));
    assert_eq!(completed.budget, Some(200_000));
    // omp forgot the goal after completing it; the second goal was dropped.
    assert_eq!(harness.model.row().goal, None);
    assert!(!harness.model.goal_active());
    assert_eq!(harness.row().goal, None);
}

/// Replays `goal.jsonl` up to the pause: the committed row shows the active
/// goal, and closing the omp session pauses it.
#[test]
fn fixture_goal_shows_on_the_row_and_pauses_with_session_close() {
    let mut harness = Harness::new();
    let prompt = "Work on the goal now.";
    harness.start(prompt);
    let fixture = include_str!("fixtures/omp/goal.jsonl");
    let created: String = fixture
        .lines()
        .take_while(|line| !line.contains("\"status\":\"paused\""))
        .map(|line| format!("{line}\n"))
        .collect();
    harness.replay(&created, |_, _| panic!("no questions"));
    assert!(harness.model.goal_active());
    let goal = harness.model.row().goal.expect("the row carries the goal");
    assert_eq!(goal.state, GoalState::Active);
    assert_eq!(goal.tokens_used, Some(0));
    assert_eq!(goal.budget, Some(200_000));
    assert_eq!(
        goal.objective,
        "Create a file named goal.txt containing the word done"
    );
    let row = harness.row();
    assert_eq!(
        row.goal.as_ref().map(|goal| goal.state),
        Some(GoalState::Active)
    );
    // omp restores an active goal paused when its process starts again.
    harness.model.session_closed();
    harness.commit();
    assert_eq!(
        harness.model.row().goal.as_ref().map(|goal| goal.state),
        Some(GoalState::Paused)
    );
    let row = harness.row();
    assert_eq!(
        row.goal.as_ref().map(|goal| goal.state),
        Some(GoalState::Paused)
    );
    // A completed goal is what j3 alone clears; this one is only paused.
    assert!(!harness.model.clear_completed_goal());
}

/// Replays `subagent.jsonl`: the task's subagent is one `subagent` item with
/// its agent, prompt, completed status, progress and transcript result.
#[test]
fn fixture_subagent_maps_to_a_subagent_item() {
    let mut harness = Harness::new();
    let prompt = "Use the task tool to run exactly one subagent that replies PONG.";
    harness.start(prompt);
    harness.replay(include_str!("fixtures/omp/subagent.jsonl"), |_, _| {
        panic!("no questions")
    });
    assert!(harness.fetches.is_empty(), "the transcript was read");
    assert_completed_turn(&harness.model, prompt);
    let subagents: Vec<(&Item, &SubagentItem)> = harness
        .model
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::Subagent(body) => Some((item, body)),
            _ => None,
        })
        .collect();
    assert_eq!(subagents.len(), 1);
    let (item, subagent) = subagents[0];
    assert_eq!(item.status, ItemStatus::Completed);
    assert_eq!(subagent.subagent_id, "SaltyGibbon");
    assert_eq!(subagent.agent, "task");
    assert_eq!(
        subagent.prompt,
        "Reply with exactly the word PONG and nothing else. Do not use any tools."
    );
    assert_eq!(subagent.model.as_deref(), Some(MODEL));
    // The final answer comes from the subagent's transcript.
    assert_eq!(subagent.result.as_deref(), Some("PONG"));
    assert_eq!(subagent.progress.as_deref(), Some("13.9k tokens"));
    // The `task` call that spawned it is a tool row.
    assert!(
        harness
            .model
            .items
            .iter()
            .any(|item| { matches!(&item.kind, ItemKind::Tool(tool) if tool.name == "task") })
    );
}

/// Replays `subagent-cancel.jsonl`: a cancelled subagent is revived as the
/// same item and finishes completed; no transcript was recorded for it.
#[test]
fn fixture_cancelled_subagent_is_revived() {
    let mut harness = Harness::new();
    let prompt = "Use the task tool to run one subagent that sleeps 60 seconds.";
    harness.start(prompt);
    let mut statuses = Vec::new();
    for line in include_str!("fixtures/omp/subagent-cancel.jsonl")
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        harness.replay(line, |_, _| panic!("no questions"));
        if let Some((item, _)) = subagent_item(&harness.model, "HushedHawk")
            && statuses.last() != Some(&item.status)
        {
            statuses.push(item.status);
        }
    }
    assert_eq!(
        statuses,
        [
            ItemStatus::Running,
            ItemStatus::Cancelled,
            ItemStatus::Running,
            ItemStatus::Completed,
        ]
    );
    assert_completed_turn(&harness.model, prompt);
    let subagents: Vec<(&Item, &SubagentItem)> = harness
        .model
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::Subagent(body) => Some((item, body)),
            _ => None,
        })
        .collect();
    assert_eq!(subagents.len(), 1, "a revived subagent is the same one");
    let (item, subagent) = subagents[0];
    assert_eq!(item.status, ItemStatus::Completed);
    assert_eq!(subagent.agent, "task");
    assert_eq!(
        subagent.prompt,
        "Continue your assignment: run `sleep 60` via bash, then reply DONE."
    );
    // No transcript read in this recording: the last output lines stand in.
    assert_eq!(subagent.result.as_deref(), Some("DONE"));
    assert!(
        subagent
            .progress
            .as_deref()
            .is_some_and(|line| line.starts_with("Waiting for sleep job to finish · 2 tool calls")),
        "{:?}",
        subagent.progress
    );
    assert_eq!(
        harness.fetches.len(),
        1,
        "the completion asked for a transcript that was never recorded"
    );
}
