//! The feed in the thread scene: every row kind draws, toggles are remembered, and the list
//! follows a streaming reply.

use std::sync::Arc;

use j3_client::t3::orchestration::{TurnItemKind, TurnItemStatus};

use crate::app::{
    fixtures::{Scene, open_thread_key, thread_state},
    testing::TestApp,
};

fn scene() -> TestApp {
    let mut app = TestApp::with_scene(Scene::Thread);
    app.settle(30);
    app
}

/// The id of the first feed item `pick` accepts.
fn item_id(app: &TestApp, pick: impl Fn(&TurnItemKind) -> bool) -> String {
    let key = open_thread_key();
    app.rt.state.threads[&key]
        .state
        .feed()
        .iter()
        .find(|projected| pick(&projected.item.kind))
        .map(|projected| projected.item.id.clone())
        .expect("the scene has the item")
}

/// Scrolls row `key` into view and lets the list mount it.
fn reveal(app: &mut TestApp, key: &str) {
    let key = key.to_owned();
    app.rt.update(move |_, tx| tx.scroll_into_view(&key));
    app.settle(30);
}

#[test]
fn every_row_kind_draws_in_the_thread_scene() {
    let mut app = scene();
    let key = open_thread_key();
    let ids: Vec<String> = app.rt.state.threads[&key]
        .state
        .feed()
        .iter()
        .filter(|projected| !crate::feed::is_hidden(&projected.item))
        .map(|projected| projected.item.id.clone())
        .collect();
    assert!(ids.len() > 20, "the scene holds every kind");
    for id in &ids {
        let row = format!("feed-{id}");
        reveal(&mut app, &row);
        assert!(app.rt.find(&row).is_some(), "{row} draws");
    }
    let hidden = item_id(&app, |kind| {
        matches!(kind, TurnItemKind::RunInterruptRequest(_))
    });
    assert!(!ids.contains(&hidden), "stop requests stay out of the feed");
    let subagent = "subagent-sa-bench";
    reveal(&mut app, subagent);
    assert!(
        app.rt.find("stop-subagent-sa-bench").is_some(),
        "a running subagent has Stop"
    );
}

#[test]
fn a_tool_body_expands_and_the_choice_is_remembered() {
    let mut app = scene();
    let key = open_thread_key();
    let id = item_id(&app, |kind| {
        matches!(kind, TurnItemKind::CommandExecution(_))
    });
    let more = format!("tool-more-{id}");
    reveal(&mut app, &more);
    let (x, y) = app.rt.find(&more).expect("a long output offers more lines");
    app.rt.pointer_down(x, y);
    app.rt.pointer_up(x, y);
    app.settle(30);
    assert!(app.rt.state.threads[&key].feed.is_expanded(&id));
    reveal(&mut app, &more);
    let label: String = app
        .rt
        .elements()
        .into_iter()
        .filter(|e| e.path.contains(more.as_str()))
        .filter_map(|e| e.text)
        .collect();
    assert!(
        label.contains("Show less"),
        "expanded shows the whole output: {label}"
    );
}

#[test]
fn the_feed_keeps_following_the_reply_after_a_wheel_scroll_at_the_bottom() {
    let mut app = scene();
    let key = open_thread_key();
    let viewport = app.rt.presented("feed").expect("the feed scrolls").0;
    let bottom = viewport[1] + viewport[3];
    let (x, y) = (
        viewport[0] + viewport[2] / 2.0,
        viewport[1] + viewport[3] / 2.0,
    );
    // A wheel scroll down at the bottom (negative moves content up, toward the end).
    app.rt.wheel(x, y, -40.0);
    app.settle(60);

    // A reply starts streaming after the last row and grows twice.
    let mut state = thread_state();
    let template = state
        .feed()
        .iter()
        .find(|projected| matches!(projected.item.kind, TurnItemKind::AssistantMessage(_)))
        .cloned()
        .unwrap();
    for text in [
        "Streaming the reply.",
        "Streaming the reply.\n\nIt keeps growing, line after line, so the list must move with it.\n\nOne more paragraph to push the end down past the old bottom of the viewport.\n\nAnd another.",
    ] {
        let mut reply = template.clone();
        reply.item.id = "ti-streaming".into();
        reply.item.status = TurnItemStatus::Running;
        if let TurnItemKind::AssistantMessage(message) = &mut reply.item.kind {
            message.text = text.into();
            message.streaming = true;
        }
        let projection = state.projection.as_mut().unwrap();
        projection
            .visible_turn_items
            .retain(|projected| projected.item.id != "ti-streaming");
        projection.visible_turn_items.push(reply);
        let next = Arc::new(state.clone());
        let key = key.clone();
        app.rt.update(move |s, tx| s.thread_changed(&key, next, tx));
        app.settle(90);
        let row = app
            .rt
            .presented("feed-ti-streaming")
            .expect("the reply is mounted")
            .0;
        let row_bottom = row[1] + row[3];
        assert!(
            (row_bottom - bottom).abs() < 2.0,
            "the reply's end stays at the viewport's bottom: {row_bottom} vs {bottom}"
        );
    }
}

#[test]
fn an_expanded_row_is_forgotten_once_its_item_leaves_the_feed() {
    let mut app = scene();
    let key = open_thread_key();
    let id = item_id(&app, |kind| {
        matches!(kind, TurnItemKind::CommandExecution(_))
    });
    let other = item_id(&app, |kind| matches!(kind, TurnItemKind::Reasoning(_)));
    let (k, i, o) = (key.clone(), id.clone(), other.clone());
    app.rt.update(move |s, _| {
        s.toggle_feed_row(&k, &i);
        s.toggle_feed_row(&k, &o);
    });
    app.settle(10);

    let mut state = thread_state();
    state
        .projection
        .as_mut()
        .unwrap()
        .visible_turn_items
        .retain(|projected| projected.item.id != id);
    let next = Arc::new(state);
    let k = key.clone();
    app.rt.update(move |s, tx| s.thread_changed(&k, next, tx));
    app.settle(10);
    let feed = &app.rt.state.threads[&key].feed;
    assert!(
        !feed.is_expanded(&id),
        "a row that left the feed is forgotten"
    );
    assert!(
        feed.is_expanded(&other),
        "rows still in the feed stay expanded"
    );

    // Coming back, it starts collapsed.
    let next = Arc::new(thread_state());
    let k = key.clone();
    app.rt.update(move |s, tx| s.thread_changed(&k, next, tx));
    app.settle(10);
    assert!(!app.rt.state.threads[&key].feed.is_expanded(&id));
}
