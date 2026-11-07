//! The thread's header and cards, driven like a window over the offline fixture thread. Its
//! connection fails every command at once, so a command a control runs shows up as its error
//! toast ("Couldn’t steer the run: …").

use std::{sync::Arc, time::Duration};

use j3_client::{ThreadState, t3::orchestration::ProviderGoalStatus};

use crate::{
    actions::{Action, shortcut_label},
    app::{
        J3,
        fixtures::{Scene, open_thread_key},
        testing::TestApp,
    },
};

/// The fixture thread open, settled.
fn thread_app() -> TestApp {
    let mut app = TestApp::with_scene(Scene::Thread);
    app.settle(30);
    app
}

fn click(app: &mut TestApp, key: &str) {
    let (x, y) = app.rt.find(key).unwrap_or_else(|| panic!("{key} is drawn"));
    app.rt.pointer_down(x, y);
    app.rt.pointer_up(x, y);
    app.frame();
}

/// Clicks the element with accessible label `label`.
fn click_label(app: &mut TestApp, label: &str) {
    let rect = app
        .rt
        .elements()
        .into_iter()
        .find(|e| e.label.as_deref() == Some(label))
        .unwrap_or_else(|| panic!("“{label}” is drawn"))
        .rect;
    let (x, y) = (rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0);
    app.rt.pointer_down(x, y);
    app.rt.pointer_up(x, y);
    app.frame();
}

fn label_of(app: &TestApp, key: &str) -> Option<String> {
    app.rt
        .elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some(key))
        .and_then(|e| e.label)
}

fn text_of(app: &TestApp, key: &str) -> Option<String> {
    app.rt
        .elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some(key))
        .and_then(|e| e.text)
}

/// Frames until a toast titled `Couldn’t {what}: …` shows (the command ran and failed).
fn until_failed(app: &mut TestApp, what: &str) {
    let prefix = format!("Couldn’t {what}: ");
    for _ in 0..400 {
        app.frame();
        if app
            .rt
            .state
            .toasts
            .iter()
            .any(|(_, toast)| toast.title.starts_with(&prefix))
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let shown: Vec<_> = app
        .rt
        .state
        .toasts
        .iter()
        .map(|(_, t)| t.title.clone())
        .collect();
    panic!("no “{prefix}…” toast; shown: {shown:?}");
}

/// Replaces the open thread's state with `edit` applied to it.
fn edit_state(app: &mut TestApp, edit: impl FnOnce(&mut ThreadState)) {
    let key = open_thread_key();
    let mut state = ThreadState::clone(&app.rt.state.threads[&key].state);
    edit(&mut state);
    app.rt
        .update(|s: &mut J3, tx| s.thread_changed(&key, Arc::new(state), tx));
    app.frame();
}

fn set_goal_status(app: &mut TestApp, status: ProviderGoalStatus) {
    edit_state(app, |state| {
        let projection = state.projection.as_mut().unwrap();
        projection.provider_threads[0].goal.as_mut().unwrap().status = status;
    });
}

#[test]
fn the_header_toggles_the_panes_with_their_shortcut_tooltips() {
    let mut app = thread_app();
    let help = |app: &TestApp, key: &str| {
        app.rt
            .elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some(key))
            .and_then(|e| e.help)
    };
    assert_eq!(
        help(&app, "toggle-files"),
        Some(format!("Files ({})", shortcut_label(Action::ToggleFiles)))
    );
    assert_eq!(
        help(&app, "toggle-terminal"),
        Some(format!(
            "Terminal ({})",
            shortcut_label(Action::ToggleTerminal)
        ))
    );
    assert_eq!(
        text_of(&app, "thread-title").as_deref(),
        Some("Port the feed renderer to Rune")
    );
    click(&mut app, "toggle-files");
    let key = open_thread_key();
    assert!(app.rt.state.files[&key].open);
    assert!(app.rt.find("file-pane").is_some() && app.rt.find("thread-files-split").is_some());
    click(&mut app, "toggle-terminal");
    assert!(app.rt.state.terminals[&key].open);
    assert!(app.rt.find("terminal-split").is_some());
    click(&mut app, "toggle-files");
    assert!(app.rt.find("file-pane").is_none());
}

#[test]
fn dragging_the_dividers_resizes_drawer_and_pane_within_their_ranges() {
    let mut app = thread_app();
    let key = open_thread_key();
    app.rt.update(|s: &mut J3, tx| {
        s.toggle_terminal(key.clone(), tx);
        s.toggle_files(key.clone(), tx);
    });
    app.frame();
    app.frame();
    let drag = |app: &mut TestApp, grip: &str, dx: f32, dy: f32| {
        let (x, y) = app.rt.find(grip).unwrap();
        app.rt.pointer_down(x, y);
        app.rt.pointer_move(x + dx / 2.0, y + dy / 2.0);
        app.frame();
        app.rt.pointer_move(x + dx, y + dy);
        app.frame();
        app.rt.pointer_up(x + dx, y + dy);
        app.frame();
    };
    let height = app.rt.state.terminals[&key].height;
    drag(&mut app, "terminal-split", 0.0, -60.0);
    assert_eq!(app.rt.state.terminals[&key].height, height + 60.0);
    drag(&mut app, "terminal-split", 0.0, 5_000.0);
    assert_eq!(
        app.rt.state.terminals[&key].height, 120.0,
        "the drawer keeps 120 pt"
    );
    let width = app.rt.state.files[&key].width;
    drag(&mut app, "thread-files-split", -40.0, 0.0);
    assert_eq!(app.rt.state.files[&key].width, width + 40.0);
    drag(&mut app, "thread-files-split", 5_000.0, 0.0);
    assert_eq!(
        app.rt.state.files[&key].width,
        15.0 * 16.0,
        "the pane keeps 15 rem"
    );
}

#[test]
fn the_actions_menu_offers_a_goal_only_without_one() {
    let mut app = thread_app();
    click(&mut app, "thread-actions");
    let items = |app: &TestApp| -> Vec<String> {
        app.rt
            .elements()
            .into_iter()
            .filter(|e| {
                e.key
                    .as_deref()
                    .is_some_and(|k| k.starts_with("thread-actions-item"))
            })
            .filter_map(|e| e.label)
            .collect()
    };
    assert_eq!(items(&app), ["Rename…", "Archive", "Delete…"]);
    click_label(&mut app, "Rename…");
    assert!(
        app.rt.state.dialogs.open.is_some(),
        "Rename… opens its dialog"
    );
    assert!(!app.rt.state.threads[&open_thread_key()].chrome.actions_open);
    app.rt.state.close_dialog();

    edit_state(&mut app, |state| {
        state.projection.as_mut().unwrap().provider_threads[0].goal = None;
    });
    assert!(app.rt.find("goal").is_none());
    click(&mut app, "thread-actions");
    assert_eq!(items(&app), ["Rename…", "Set goal…", "Archive", "Delete…"]);
    click_label(&mut app, "Set goal…");
    assert!(app.rt.find("goal-objective").is_some());

    // Neither an empty objective nor a budget that isn't a number creates it.
    goal_dialog(&mut app).budget.replace_text("lots");
    click(&mut app, "dialog-confirm");
    let dialog = goal_dialog(&mut app);
    assert!(dialog.objective_missing && dialog.budget_invalid);
    dialog.objective.replace_text("Ship the release");
    dialog.budget.replace_text("200,000");
    click(&mut app, "dialog-confirm");
    assert!(
        app.rt.find("goal-objective").is_none(),
        "Set Goal closes the dialog"
    );
    until_failed(&mut app, "set the goal");
}

fn goal_dialog(app: &mut TestApp) -> &mut super::goal::GoalDialog {
    let key = open_thread_key();
    let thread = app.rt.state.threads.get_mut(&key).unwrap();
    thread
        .chrome
        .goal_dialog
        .as_mut()
        .expect("the goal dialog is open")
}

#[test]
fn queued_messages_steer_send_now_and_edit_in_place() {
    let mut app = thread_app();
    assert_eq!(
        label_of(&app, "queued-run-4").as_deref(),
        Some("Then write the changelog entry.")
    );
    // The waiting run can't be steered.
    assert!(app.rt.find("steer-run-4").is_none());
    click(&mut app, "send-now-run-4");
    until_failed(&mut app, "send the message now");

    // A `/compact` run is steered by stopping it.
    edit_state(&mut app, |state| {
        state.projection.as_mut().unwrap().messages[2].text = "/compact".into();
    });
    click(&mut app, "steer-run-4");
    until_failed(&mut app, "steer the run");

    click(&mut app, "edit-run-4");
    assert!(app.rt.find("queue-input-run-4").is_some());
    assert!(app.rt.find("send-now-run-4").is_none());
    click(&mut app, "cancel-edit-run-4");
    assert!(app.rt.find("queue-input-run-4").is_none());
    click(&mut app, "edit-run-4");
    click(&mut app, "save-run-4");
    until_failed(&mut app, "edit the queued message");

    click(&mut app, "cancel-run-4");
    until_failed(&mut app, "remove the message from the queue");
}

#[test]
fn goal_controls_follow_its_status() {
    let mut app = thread_app();
    assert_eq!(
        label_of(&app, "goal").as_deref(),
        Some("Feed renders every turn item kind on Rune with no scroll jumps, Active")
    );
    assert!(app.rt.find("goal-resume").is_none());
    click(&mut app, "goal-pause");
    until_failed(&mut app, "pause the goal");

    set_goal_status(&mut app, ProviderGoalStatus::Paused);
    assert!(app.rt.find("goal-pause").is_none());
    click(&mut app, "goal-resume");
    until_failed(&mut app, "resume the goal");

    set_goal_status(&mut app, ProviderGoalStatus::BudgetLimited);
    assert!(app.rt.find("goal-resume").is_some());

    set_goal_status(&mut app, ProviderGoalStatus::Complete);
    assert!(app.rt.find("goal-resume").is_none());
    assert_eq!(label_of(&app, "goal-drop").as_deref(), Some("Clear"));
    click(&mut app, "goal-drop");
    until_failed(&mut app, "drop the goal");
}

#[test]
fn the_plan_card_folds() {
    let mut app = thread_app();
    assert_eq!(
        label_of(&app, "todo-toggle").as_deref(),
        Some("Plan, 2/4 done")
    );
    assert!(app.rt.find("todo-step-s1").is_some());
    click(&mut app, "todo-toggle");
    assert!(app.rt.find("todo-step-s1").is_none());
    click(&mut app, "todo-toggle");
    assert!(app.rt.find("todo-step-s1").is_some());
}

#[test]
fn the_question_card_walks_the_questions_and_answers() {
    let mut app = thread_app();
    assert!(app.rt.find("question-card").is_some());
    assert_eq!(
        text_of(&app, "question-progress").as_deref(),
        Some("Question 1 of 2")
    );
    click(&mut app, "question-next");
    assert_eq!(
        text_of(&app, "question-problem").as_deref(),
        Some("Choose an answer to continue.")
    );
    // A pick shows for a moment, then moves on.
    click_label(&mut app, "Bottom");
    assert!(app.rt.find("question-problem").is_none());
    for _ in 0..15 {
        app.frame();
    }
    assert_eq!(
        text_of(&app, "question-progress").as_deref(),
        Some("Question 2 of 2")
    );
    click(&mut app, "question-choice-displays-120 Hz");
    click(&mut app, "question-submit");
    until_failed(&mut app, "answer the question");
    click(&mut app, "dismiss-question");
    until_failed(&mut app, "dismiss the question");
}
