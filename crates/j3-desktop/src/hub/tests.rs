//! The agents hub over the offline fixture thread (two subagents: `sa-bench` running,
//! `sa-audit` done).

use std::{sync::Arc, time::Duration};

use j3_client::ThreadState;
use rune::{Key, KeyEvent, Mods};

use crate::{
    app::{
        J3,
        fixtures::{Scene, open_thread_key},
        testing::TestApp,
    },
    composer::ComposerKey,
};

fn press(key: Key) -> KeyEvent {
    KeyEvent {
        key,
        mods: Mods::default(),
    }
}

fn typed(c: &str) -> KeyEvent {
    press(Key::Text(c.to_owned()))
}

fn hub_app() -> TestApp {
    let mut app = TestApp::with_scene(Scene::Thread);
    app.settle(30);
    let key = open_thread_key();
    app.rt.update(move |s: &mut J3, tx| s.open_hub(key, tx));
    app.frame();
    app
}

fn is_open(app: &TestApp) -> bool {
    app.rt
        .state
        .hubs
        .get(&open_thread_key())
        .is_some_and(|hub| hub.open)
}

fn selected(app: &TestApp) -> Option<String> {
    app.rt.state.hubs[&open_thread_key()].selected.clone()
}

fn label_of(app: &TestApp, key: &str) -> Option<String> {
    app.rt
        .elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some(key))
        .and_then(|e| e.label)
}

#[test]
fn the_hub_replaces_the_chat_and_lists_running_subagents_first() {
    let mut app = hub_app();
    assert!(is_open(&app));
    assert!(app.rt.find("agent-hub").is_some());
    assert!(
        app.rt.find("composer").is_none(),
        "the hub replaces the composer"
    );
    assert!(app.rt.find("feed").is_none(), "and the feed");
    assert!(app.rt.find("thread-title").is_some(), "the header stays");
    assert_eq!(
        label_of(&app, "hub-agent-sa-bench").as_deref(),
        Some("sa-bench, BenchFeed, Running")
    );
    assert_eq!(selected(&app).as_deref(), Some("sa-bench"));
    let y = |app: &TestApp, key: &str| app.rt.find(key).unwrap().1;
    assert!(y(&app, "hub-agent-sa-bench") < y(&app, "hub-agent-sa-audit"));
    assert!(
        app.rt.find("hub-stop").is_some(),
        "a running subagent can be stopped"
    );

    // ↓/↑ and j/k move the selection; the detail follows.
    app.key(press(Key::Down));
    assert_eq!(selected(&app).as_deref(), Some("sa-audit"));
    assert!(app.rt.find("hub-stop").is_none(), "a finished one can't");
    app.key(typed("k"));
    assert_eq!(selected(&app).as_deref(), Some("sa-bench"));
    app.key(typed("j"));
    assert_eq!(selected(&app).as_deref(), Some("sa-audit"));
    app.key(press(Key::Down));
    assert_eq!(
        selected(&app).as_deref(),
        Some("sa-audit"),
        "the roster's end holds"
    );
}

#[test]
fn left_left_closes_the_hub_and_the_composer_takes_the_keyboard() {
    let mut app = hub_app();
    // Key-repeat speed (16 ms apart) is no double tap.
    app.key(press(Key::Left));
    app.key(press(Key::Left));
    assert!(is_open(&app));
    // Another key between taps cancels the pair.
    for _ in 0..3 {
        app.frame();
    }
    app.key(typed("q"));
    app.key(press(Key::Left));
    assert!(is_open(&app));
    // A second tap 40–500 ms after the first closes.
    for _ in 0..3 {
        app.frame();
    }
    app.key(press(Key::Left));
    assert!(!is_open(&app));
    assert!(app.rt.find("agent-hub").is_none());
    assert!(app.rt.find("composer").is_some());
    let input = ComposerKey::Thread(open_thread_key()).input_key();
    app.frame();
    assert!(
        app.rt
            .elements()
            .into_iter()
            .any(|e| e.focused && e.path.contains(&input)),
        "the composer has focus again"
    );
}

#[test]
fn escape_closes_the_hub() {
    let mut app = hub_app();
    app.key(press(Key::Escape));
    assert!(!is_open(&app));
    assert!(app.rt.find("composer").is_some());
}

#[test]
fn a_transcript_that_cannot_be_read_shows_what_j3_knows() {
    let mut app = hub_app();
    let shows_assignment = |app: &TestApp| {
        app.rt
            .elements()
            .into_iter()
            .any(|e| e.text.as_deref() == Some("Assignment"))
    };
    for _ in 0..400 {
        app.frame();
        if shows_assignment(&app) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        shows_assignment(&app),
        "the assignment shows without a transcript"
    );
    assert_eq!(
        app.rt.state.toasts.iter().count(),
        0,
        "a failed read is no toast"
    );
}

#[test]
fn the_hub_opens_only_with_subagents() {
    let mut app = TestApp::with_scene(Scene::Thread);
    app.settle(30);
    let key = open_thread_key();
    let thread = app.rt.state.threads.get_mut(&key).unwrap();
    let mut state = ThreadState::clone(&thread.state);
    state.projection.as_mut().unwrap().subagents.clear();
    thread.state = Arc::new(state);
    let open = key.clone();
    app.rt.update(move |s: &mut J3, tx| s.open_hub(open, tx));
    app.frame();
    assert!(!is_open(&app));
    assert!(app.rt.find("composer").is_some());
}
