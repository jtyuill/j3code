//! The feed in the thread scene: every row kind draws, toggles are remembered, and the list
//! follows a streaming reply.

use std::sync::Arc;

use j3_client::entities::{ItemKind, ItemStatus, ToolDetail};

use crate::{
    app::{
        Main,
        fixtures::{Scene, open_thread_key, thread_state},
        testing::TestApp,
    },
    settings::Page,
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
fn streamed_words_fade_in_while_the_row_is_already_at_its_height() {
    let mut app = scene();
    let key = open_thread_key();
    let mut state = thread_state();
    let template = state
        .items
        .iter()
        .find(|item| matches!(item.kind, ItemKind::Text(_)))
        .cloned()
        .unwrap();
    let mut grow = |app: &mut TestApp, text: &str| {
        let mut reply = template.clone();
        reply.id = "ti-streaming".into();
        reply.ordinal = state.items.last().map_or(1, |last| last.ordinal + 1);
        if let ItemKind::Text(message) = &mut reply.kind {
            message.text = text.into();
        }
        state.items.retain(|item| item.id != "ti-streaming");
        state.items.push(reply);
        let (key, next) = (key.clone(), Arc::new(state.clone()));
        app.rt.update(move |s, tx| s.thread_changed(&key, next, tx));
    };
    grow(&mut app, "The reply");
    // A picture finishing its decode lands its commit at once (`poll_feed`), words with it.
    for _ in 0..600 {
        app.settle(90);
        if app.rt.state.threads[&key].feed.wait.borrow().is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(!app.rt.animating, "nothing moves at rest");

    // Mid-word and mid-paragraph: the text shows as far as it has arrived.
    let longer = format!(
        "The reply {}and an unfinished wo",
        "keeps going ".repeat(40)
    );
    grow(&mut app, &longer);
    app.frame();
    let row = |app: &TestApp| {
        app.rt
            .elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some("feed-ti-streaming"))
            .expect("the reply is mounted")
    };
    let first = row(&app);
    assert!(
        (first.rect[3] - first.target[3]).abs() < 0.5 && first.rect[3] > 60.0,
        "the row is at its full height in the frame the text arrives: {} of {}",
        first.rect[3],
        first.target[3]
    );
    assert!(
        app.rt.elements().into_iter().any(|e| e
            .text
            .as_deref()
            .is_some_and(|shown| shown.ends_with("and an unfinished wo"))),
        "the unfinished word shows"
    );
    assert!(app.rt.animating, "the new words are fading in");
    app.settle(240);
    assert!(!app.rt.animating, "and the fade ends");
    assert_eq!(row(&app).rect[3], first.rect[3], "nothing moved meanwhile");
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

/// The row at the top of the view (its key) and where its top stands under the viewport's.
fn top_row(app: &TestApp) -> (String, f32) {
    let all = app.rt.elements();
    let view = all
        .iter()
        .find(|e| e.key.as_deref() == Some("feed"))
        .expect("the feed scrolls")
        .rect;
    all.into_iter()
        .filter(|e| {
            e.path
                .ends_with(&format!("feed-rows/{}", e.key.as_deref().unwrap_or("?")))
        })
        .filter(|e| e.rect[1] + e.rect[3] > view[1] + 1.0)
        .min_by(|a, b| a.rect[1].total_cmp(&b.rect[1]))
        .map(|e| (e.key.unwrap(), e.rect[1] - view[1]))
        .expect("a row is in view")
}

/// The thread scene in a window tall enough that the cards above the composer leave the
/// feed a few hundred points.
fn tall_scene() -> TestApp {
    let mut app = TestApp::with_scene(Scene::Thread);
    app.rt.set_viewport((1280.0, 2000.0), 1.0);
    app
}

/// Frames with pauses: the scene's pictures decode on other threads.
fn rest(app: &mut TestApp) {
    for _ in 0..60 {
        app.frame();
        std::thread::sleep(std::time::Duration::from_millis(3));
    }
}

#[test]
fn a_feed_that_leaves_the_screen_comes_back_where_the_reader_was() {
    for zoom in [1.0, 1.25] {
        let mut app = tall_scene();
        app.rt
            .update(move |s, tx| s.update_ui(|ui| ui.zoom = zoom, tx));
        rest(&mut app);
        let key = open_thread_key();
        let view = app.rt.presented("feed").expect("the feed scrolls").0;
        let (x, y) = (view[0] + view[2] / 2.0, view[1] + view[3] / 2.0);
        // Up the feed, a row running out of the top of the view.
        app.rt.wheel(x, y, 700.0);
        rest(&mut app);
        let before = top_row(&app);
        assert_eq!(app.rt.scroll_at_bottom("feed"), Some(false));
        assert!(
            before.1 < -1.0,
            "zoom {zoom}: a row is cut by the top: {before:?}"
        );

        type Leave = fn(&mut crate::app::J3, &mut rune::Tx);
        let ways: [(&str, Leave, Leave); 2] = [
            (
                "Settings",
                |s, tx| s.open_settings(Page::General, tx),
                |s, tx| s.close_settings(tx),
            ),
            (
                "another surface",
                |s, _| s.route.main = Main::NewThread,
                |s, tx| s.open_thread(open_thread_key(), tx),
            ),
        ];
        for (way, leave, back) in ways {
            app.rt.update(leave);
            rest(&mut app);
            assert!(app.rt.find("feed-rows").is_none(), "{way} covers the feed");
            app.rt.update(back);
            rest(&mut app);
            let after = top_row(&app);
            assert_eq!(
                after.0, before.0,
                "zoom {zoom}, {way}: the same row is on top"
            );
            assert!(
                (after.1 - before.1).abs() <= 1.0,
                "zoom {zoom}, {way}: it stands where it did: {} then, {} now",
                before.1,
                after.1
            );
        }

        // A reader at the end comes back to the end.
        app.rt
            .update(move |s, tx| s.jump_to_feed_end(&open_thread_key(), tx));
        rest(&mut app);
        assert_eq!(app.rt.scroll_at_bottom("feed"), Some(true));
        app.rt.update(|s, tx| s.open_settings(Page::General, tx));
        rest(&mut app);
        app.rt.update(|s, tx| s.close_settings(tx));
        rest(&mut app);
        assert_eq!(app.rt.scroll_at_bottom("feed"), Some(true), "zoom {zoom}");
        assert!(app.rt.state.threads[&key].feed.place.is_none());
    }
}

#[test]
fn the_jump_button_shows_away_from_the_end_and_goes_there_at_once() {
    let mut app = tall_scene();
    rest(&mut app);
    assert!(app.rt.find("feed-jump").is_none(), "at the end: no button");
    let view = app.rt.presented("feed").expect("the feed scrolls").0;
    let (x, y) = (view[0] + view[2] / 2.0, view[1] + view[3] / 2.0);
    app.rt.wheel(x, y, 900.0);
    rest(&mut app);
    assert_eq!(app.rt.scroll_at_bottom("feed"), Some(false));

    // The strip the button sits in doesn't take the wheel from the feed under it.
    let (bx, by) = app
        .rt
        .find("feed-jump")
        .expect("away from the end: the button");
    let before = top_row(&app);
    app.rt.wheel(bx + 120.0, by, 300.0);
    rest(&mut app);
    assert_ne!(top_row(&app), before, "the feed scrolls under the strip");

    let (bx, by) = app.rt.find("feed-jump").expect("still away");
    app.rt.pointer_down(bx, by);
    app.rt.pointer_up(bx, by);
    app.frame();
    app.frame();
    assert_eq!(
        app.rt.scroll_at_bottom("feed"),
        Some(true),
        "one press, no glide"
    );
    rest(&mut app);
    assert!(app.rt.find("feed-jump").is_none());
}
