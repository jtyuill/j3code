//! The feed in the thread scene: every row kind draws, toggles are remembered, and the list
//! follows a streaming reply.

use std::sync::Arc;

use j3_client::entities::{ItemKind, ItemStatus, ToolDetail};

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
fn item_id(app: &TestApp, pick: impl Fn(&ItemKind) -> bool) -> String {
    let key = open_thread_key();
    app.rt.state.threads[&key]
        .state
        .items
        .iter()
        .find(|item| pick(&item.kind))
        .map(|item| item.id.clone())
        .expect("the scene has the item")
}

/// Scrolls row `key` into view and lets the list mount it.
fn reveal(app: &mut TestApp, key: &str) {
    let key = key.to_owned();
    app.rt.update(move |_, tx| tx.scroll_into_view(&key));
    app.settle(30);
}

#[test]
fn a_thread_loaded_on_open_shows_its_end_without_gliding_there() {
    let mut app = TestApp::with_scene(Scene::Thread);
    let key = open_thread_key();
    // Not cached: the subscription has nothing yet, and the feed's images aren't decoded.
    app.rt.state.threads.get_mut(&key).unwrap().state =
        Arc::new(j3_client::ThreadView::new(key.thread_id.clone()));
    app.settle(30);
    let loaded = Arc::new(thread_state());
    app.rt
        .update(move |s, tx| s.thread_changed(&key, loaded, tx));
    for frame in 0..40 {
        app.frame();
        let (drawn, target) = app
            .rt
            .elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some("feed-rows"))
            .map(|e| (e.rect, e.target))
            .expect("the feed draws");
        assert!(
            (drawn[3] - target[3]).abs() < 0.5,
            "frame {frame}: the rows are at their height ({} of {})",
            drawn[3],
            target[3]
        );
        assert_eq!(app.rt.scroll_at_bottom("feed"), Some(true), "frame {frame}");
    }
}

#[test]
fn every_row_kind_draws_in_the_thread_scene() {
    let mut app = scene();
    let key = open_thread_key();
    let ids: Vec<String> = app.rt.state.threads[&key]
        .state
        .items
        .iter()
        .filter(|item| !crate::feed::is_hidden(item))
        .map(|item| item.id.clone())
        .collect();
    assert!(ids.len() > 20, "the scene holds every kind");
    for id in &ids {
        let row = format!("feed-{id}");
        reveal(&mut app, &row);
        assert!(app.rt.find(&row).is_some(), "{row} draws");
    }
    // The card's key is inside its row: mount the row first.
    let subagent = item_id(
        &app,
        |kind| matches!(kind, ItemKind::Subagent(s) if s.subagent_id == "sa-bench"),
    );
    reveal(&mut app, &format!("feed-{subagent}"));
    reveal(&mut app, "subagent-sa-bench");
    assert!(
        app.rt.find("stop-subagent-sa-bench").is_some(),
        "a running subagent has Stop"
    );
}

#[test]
fn a_folded_tool_body_opens_from_its_header_and_the_choice_is_remembered() {
    let mut app = scene();
    let key = open_thread_key();
    // The finished feed tests: folded until asked.
    let id = item_id(&app, |kind| {
        matches!(kind, ItemKind::Tool(tool)
            if matches!(tool.detail, Some(ToolDetail::Command { .. }))
                && tool.output.as_ref().and_then(|output| output.exit_code) == Some(0))
    });
    let output = |app: &TestApp| {
        app.rt
            .elements()
            .into_iter()
            .filter_map(|e| e.text)
            .any(|text| text.contains("test result: ok. 14 passed"))
    };
    // Fold the plan card: with the question and queue cards it covers the top of the feed.
    let open = key.clone();
    app.rt
        .update(move |s, _| s.threads.get_mut(&open).unwrap().chrome.todo_open = false);
    let toggle = format!("tool-toggle-{id}");
    reveal(&mut app, &toggle);
    assert!(!output(&app), "a finished command starts folded");
    let (x, y) = app.rt.find(&toggle).expect("the header draws");
    app.rt.pointer_down(x, y);
    app.rt.pointer_up(x, y);
    app.settle(30);
    assert!(app.rt.state.threads[&key].feed.is_expanded(&id));
    reveal(&mut app, &toggle);
    assert!(output(&app), "expanded shows the output");
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
        .items
        .iter()
        .find(|item| matches!(item.kind, ItemKind::Text(_)))
        .cloned()
        .unwrap();
    for text in [
        "Streaming the reply.",
        "Streaming the reply.\n\nIt keeps growing, line after line, so the list must move with it.\n\nOne more paragraph to push the end down past the old bottom of the viewport.\n\nAnd another.",
    ] {
        let mut reply = template.clone();
        reply.id = "ti-streaming".into();
        reply.status = ItemStatus::Running;
        reply.ended_at = None;
        reply.ordinal = state.items.last().map_or(1, |last| last.ordinal + 1);
        if let ItemKind::Text(message) = &mut reply.kind {
            message.text = text.into();
        }
        state.items.retain(|item| item.id != "ti-streaming");
        state.items.push(reply);
        let next = Arc::new(state.clone());
        let key = key.clone();
        app.rt.update(move |s, tx| s.thread_changed(&key, next, tx));
        app.settle(90);
        let row = app
            .rt
            .presented("feed-ti-streaming")
            .expect("the reply is mounted")
            .0;
        // The list's end padding sits under the last row.
        let row_bottom = row[1] + row[3] + super::END_PAD;
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
    let id = item_id(
        &app,
        |kind| matches!(kind, ItemKind::Tool(tool) if matches!(tool.detail, Some(ToolDetail::Command { .. }))),
    );
    let other = item_id(&app, |kind| matches!(kind, ItemKind::Thinking(_)));
    let (k, i, o) = (key.clone(), id.clone(), other.clone());
    app.rt.update(move |s, _| {
        s.toggle_feed_row(&k, &i);
        s.toggle_feed_row(&k, &o);
    });
    app.settle(10);

    let mut state = thread_state();
    state.items.retain(|item| item.id != id);
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
