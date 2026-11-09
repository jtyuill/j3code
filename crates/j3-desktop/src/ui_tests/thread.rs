//! An open thread against the real server: questions, queueing and steering, stopping,
//! the goal chip, subagents and the agents hub, images, the feed following a reply, and a
//! sent message held at the top while its reply writes down under it.

use std::{ops::Range, sync::Arc, time::Duration};

use j3_client::{
    entities::{Delivery, ItemKind, TurnStatus},
    methods::ImageUrlParams,
};
use rune::{ElementInfo, Key, KeyEvent, Mods};
use serde_json::json;

use super::harness::{Harness, bottom, centre, primary, top};
use crate::sidebar::ThreadKey;

/// The streaming reply `m1` (the feed row labelled with its text).
fn reply_row(app: &Harness) -> Option<ElementInfo> {
    labelled_from(app, "Paragraph 0:")
}

/// The feed row whose label starts with `prefix`.
fn labelled_from(app: &Harness, prefix: &str) -> Option<ElementInfo> {
    app.elements().into_iter().find(|row| {
        row.path.split('/').any(|part| part == "feed-rows")
            && row
                .label
                .as_deref()
                .is_some_and(|label| label.starts_with(prefix))
    })
}

/// Streams `{word} {i}: …` for each `i` into reply `message` and waits until the feed shows
/// the last one.
fn stream(app: &mut Harness, message: &str, word: &str, range: Range<usize>) {
    let marker = format!("{word} {}:", range.end - 1);
    for i in range {
        let delta = format!(
            "{word} {i}: enough words to wrap across the column at least once or twice.\n\n"
        );
        app.script.text(
            message,
            json!({"type": "text_delta", "contentIndex": 0, "delta": delta}),
        );
    }
    let first = format!("{word} 0:");
    app.until("the last paragraph shows", |h| {
        labelled_from(h, &first).is_some_and(|row| row.label.unwrap().contains(&marker))
    });
}

/// Streams `Paragraph {i}: …` into reply `m1`.
fn stream_paragraphs(app: &mut Harness, range: Range<usize>) {
    stream(app, "m1", "Paragraph", range);
}

#[test]
fn a_pending_question_is_answered_from_its_card() {
    let mut app = Harness::start("question");
    let project_id = app.project();
    let thread_id = app.open_running_thread(&project_id, "Ask me");
    assert!(!app.exists("question-card"));

    app.script.emit(
        json!({"type": "extension_ui_request", "id": "ui-1", "method": "select",
        "title": "Pick one", "options": ["alpha", "beta"]}),
    );
    app.until("the card shows", |h| {
        h.exists("question-card") && h.has_label("beta")
    });
    // The sidebar row follows the shell stream, which may trail the thread's own.
    app.until("the sidebar asks for input", |h| {
        h.in_sidebar("Ask me, Input")
    });
    assert!(app.has_label("alpha"));

    app.click_label("beta");
    // Element info carries no checked state: the card's form says what is ticked.
    let key = ThreadKey::new("local", &thread_id);
    let chosen = |app: &Harness, value: &str| {
        app.state().threads[&key]
            .chrome
            .question
            .as_ref()
            .and_then(|card| card.form.as_ref().ok())
            .is_some_and(|form| form.items[0].is_chosen(value))
    };
    assert!(chosen(&app, "beta"));
    assert!(!chosen(&app, "alpha"));
    // Choosing the answer of the last (only) question confirms the questionnaire once the
    // selection has shown, which submits it.
    let script = Arc::clone(&app.script);
    app.until("the answer is sent", move |_| {
        !script.ui_responses.lock().unwrap().is_empty()
    });
    let responses = app.script.ui_responses.lock().unwrap().clone();
    assert_eq!(
        serde_json::to_value(&responses[0]).unwrap(),
        json!({"type": "extension_ui_response", "id": "ui-1", "value": "beta"})
    );
    app.until_thread("the turn continues", &thread_id, |state| {
        state.open_requests().next().is_none()
            && state
                .active_turn()
                .is_some_and(|turn| turn.status == TurnStatus::Running)
    });
    app.until("the card goes", |h| {
        !h.exists("question-card") && h.in_sidebar("Ask me, Working")
    });
    assert_eq!(
        app.script.ui_responses.lock().unwrap().len(),
        1,
        "answered once"
    );
}

#[test]
fn enter_queues_while_running_and_the_queued_message_can_steer() {
    let mut app = Harness::start("steer");
    let project_id = app.project();
    let thread_id = app.open_running_thread(&project_id, "first");

    // One button: Stop with nothing typed, Queue once there is a draft.
    app.click_composer();
    assert_eq!(app.label_of("send-message").as_deref(), Some("Stop"));
    // Shift+Enter breaks the line and sends nothing.
    app.typed("queue me");
    assert_eq!(app.label_of("send-message").as_deref(), Some("Queue"));
    app.key(KeyEvent {
        key: Key::Enter,
        mods: Mods {
            shift: true,
            ..Mods::default()
        },
    });
    assert_eq!(app.draft(), "queue me\n");
    // Enter queues the (trimmed) draft; it does not steer the running turn.
    app.press(Key::Enter);
    app.until_thread("the message is queued", &thread_id, |state| {
        state.queue.len() == 1
    });
    let queued = {
        let handle = app.connection.thread(&thread_id);
        let state = handle.state();
        let state = state.borrow();
        let queued = &state.queue[0];
        assert_eq!(queued.text, "queue me");
        queued.id.clone()
    };
    let row = format!("queued-{queued}");
    app.until("the queue panel lists it", |h| {
        h.label_of(&row).as_deref() == Some("queue me")
    });
    assert_eq!(app.draft(), "");
    assert!(app.exists(&format!("send-now-{queued}")));
    assert!(app.exists(&format!("steer-{queued}")));
    // The draft is gone, so the button offers Stop again.
    assert_eq!(app.label_of("send-message").as_deref(), Some("Stop"));
    assert!(!app.script.called("steer:queue me"));
    assert!(!app.script.called("prompt:queue me:0"));

    // Steer on the queued message sends it into the running turn.
    app.click(&format!("steer-{queued}"));
    app.until_called("steer:queue me");
    app.until_thread("the message steers", &thread_id, |state| {
        state.queue.is_empty()
            && state.items.iter().any(|item| {
                matches!(&item.kind, ItemKind::User(user)
                    if user.delivery == Delivery::Steer && user.text == "queue me")
            })
    });
}

#[test]
fn stop_interrupts_the_run() {
    let mut app = Harness::start("stop");
    let project_id = app.project();
    let thread_id = app.open_running_thread(&project_id, "long task");

    // The Stop button.
    app.click("send-message");
    app.until_called("abort");
    app.until_thread("the turn ends", &thread_id, |state| {
        state.active_turn().is_none()
    });
    app.until("the button offers Send", |h| {
        !h.stoppable()
            && h.label_of("send-message").as_deref() == Some("Send")
            && h.in_sidebar("long task")
    });

    // A new message starts another run; the StopRun shortcut stops it too.
    app.click_composer();
    app.typed("again");
    app.press(Key::Enter);
    app.until_called("prompt:again:0");
    app.script.emit(json!({"type": "agent_start"}));
    app.until("the button offers Stop", |h| h.stoppable());
    app.key(primary('.'));
    app.until("the run stops", |h| !h.stoppable());
    assert_eq!(app.script.count("abort"), 2);
    app.until_thread("two turns were stopped", &thread_id, |state| {
        state.active_turn().is_none()
            && state
                .turns
                .iter()
                .filter(|turn| turn.status == TurnStatus::Stopped)
                .count()
                == 2
    });
}

#[test]
fn goal_chip_controls_and_subagent_progress() {
    let mut app = Harness::start("extras");
    let project_id = app.project();
    app.open_running_thread(&project_id, "delegate");

    // Set a goal from the thread-actions menu and its dialog.
    assert!(!app.exists("goal"));
    app.click("thread-actions");
    app.until("the menu opens", |h| h.has_label("Set goal…"));
    app.click_label("Set goal…");
    app.until("the dialog opens", |h| h.exists("dialog-confirm"));
    app.typed("Ship the release");
    app.click("dialog-confirm");
    app.until("the goal chip shows", |h| {
        h.label_of("goal").as_deref() == Some("Ship the release, Active")
    });
    assert!(!app.exists("goal-resume"));

    // Pause, resume, drop.
    app.click("goal-pause");
    app.until("the goal pauses", |h| {
        h.label_of("goal").as_deref() == Some("Ship the release, Paused") && h.exists("goal-resume")
    });
    app.click("goal-resume");
    app.until("the goal resumes", |h| {
        h.label_of("goal").as_deref() == Some("Ship the release, Active")
    });
    app.click("goal-drop");
    app.until("the goal goes", |h| !h.exists("goal"));
    let goals: Vec<String> = app
        .script
        .calls()
        .into_iter()
        .filter(|call| call.starts_with("goal:"))
        .collect();
    assert_eq!(
        goals,
        ["goal:create", "goal:pause", "goal:resume", "goal:drop"]
    );

    // A subagent row shows live progress, then its result.
    let script = Arc::clone(&app.script);
    script.emit(
        json!({"type": "tool_execution_start", "toolCallId": "call_task", "toolName": "task",
        "args": {"tasks": [{"agent": "task", "task": "Reply PONG."}]}}),
    );
    script.emit(json!({"type": "tool_execution_end", "toolCallId": "call_task", "toolName": "task",
        "result": {"content": [{"type": "text", "text": "Spawned agent `Pinger`."}]}, "isError": false}));
    script.subagent_lifecycle("Pinger", "started");
    script.subagent_progress("Pinger", "running", &[], 1200);
    app.until("the subagent runs", |h| {
        h.label_of("subagent-Pinger").as_deref()
            == Some("Answer PONG, Running, Thinking about PONG · 1.2k tokens")
    });
    assert!(app.exists("stop-subagent-Pinger"));

    script.subagent_progress("Pinger", "completed", &["PONG"], 2400);
    script.subagent_lifecycle("Pinger", "completed");
    app.until("the subagent is done", |h| {
        h.label_of("subagent-Pinger").as_deref()
            == Some("Answer PONG, Done, PONG, from the transcript")
    });
    assert!(!app.exists("stop-subagent-Pinger"));

    // ←← in the empty composer opens the agents hub with the transcript omp wrote next to
    // the thread's session; Esc closes it.
    let dir = app.data_dir.join("fake-session");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Pinger.jsonl"),
        concat!(
            r#"{"type":"message","message":{"role":"user","content":"Reply PONG."}}"#,
            "\n",
            r#"{"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","id":"c1","name":"bash","arguments":{"command":"echo PONG"}}]}}"#,
            "\n",
        ),
    )
    .unwrap();
    app.click_composer();
    app.press(Key::Left);
    app.pause(Duration::from_millis(60));
    app.press(Key::Left);
    app.until("the hub opens", |h| h.exists("agent-hub"));
    app.until("the hub lists the subagent", |h| {
        h.label_of("hub-agent-Pinger").as_deref() == Some("Pinger, Answer PONG, Done")
    });
    app.until("the transcript loads", |h| h.exists("hub-entry-Pinger/1"));
    assert!(!app.exists("composer"));
    app.press(Key::Escape);
    app.until("the hub closes", |h| {
        !h.exists("agent-hub") && h.exists("composer")
    });
}

/// A 1×1 PNG.
const PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

#[test]
fn tool_and_host_images_come_from_the_threads_server() {
    use base64::Engine as _;

    let mut app = Harness::start("images");
    let png = base64::engine::general_purpose::STANDARD
        .decode(PNG_BASE64)
        .unwrap();
    std::fs::write(app.project_dir.join("plot.png"), &png).unwrap();
    std::fs::write(app.project_dir.join("notes.txt"), "text").unwrap();
    let project_id = app.project();
    let thread_id = app.open_running_thread(&project_id, "look");

    // A tool that returns an image: the row draws it, fetched from the server as an
    // attachment.
    app.script
        .emit(json!({"type": "tool_execution_start", "toolCallId": "c1",
        "toolName": "read", "args": {"path": "plot.png"}}));
    app.script.emit(
        json!({"type": "tool_execution_end", "toolCallId": "c1", "toolName": "read",
        "result": {"content": [
            {"type": "text", "text": "Read image file [image/png]"},
            {"type": "image", "data": PNG_BASE64, "mimeType": "image/png"},
        ]}, "isError": false}),
    );
    app.until("the image shows", |h| h.has_label("Tool output image"));

    let image = app
        .connection
        .thread(&thread_id)
        .state()
        .borrow()
        .items
        .iter()
        .find_map(|item| match &item.kind {
            ItemKind::Tool(tool) => tool.output.as_ref()?.images.first().cloned(),
            _ => None,
        })
        .expect("the tool's output image");
    let get = |url: String| {
        app.block_on(async move {
            let response = reqwest::get(url).await.ok()?.error_for_status().ok()?;
            Some(response.bytes().await.ok()?.to_vec())
        })
    };
    assert_eq!(
        get(app.connection.attachment_url(&image.attachment_id)),
        Some(png.clone())
    );

    // Markdown images name files in the thread's project on its host
    // (`projects.imageUrl`, project-relative): images in the project are served; other
    // files and escapes are not.
    let fetch = |path: &str| {
        let connection = app.connection.clone();
        let params = ImageUrlParams {
            project_id: project_id.clone(),
            path: path.to_owned(),
        };
        let url = app.block_on(async move { connection.projects_image_url(params).await });
        get(app.connection.url(&url.ok()?.url))
    };
    assert_eq!(fetch("plot.png"), Some(png));
    assert_eq!(fetch("notes.txt"), None);
    assert_eq!(fetch("../plot.png"), None);
}

#[test]
fn the_feed_keeps_following_the_reply_after_a_wheel_scroll_at_the_bottom() {
    let mut app = Harness::start("follow");
    let project_id = app.project();
    app.open_running_thread(&project_id, "long");
    app.script.emit(json!({"type": "agent_start"}));
    app.script
        .text("m1", json!({"type": "text_start", "contentIndex": 0}));
    // The reply overflows the feed, which follows its bottom edge.
    stream_paragraphs(&mut app, 0..30);
    app.until("the reply overflows", |h| {
        reply_row(h).is_some_and(|row| top(&row) < 0.0)
    });
    let row = reply_row(&app).unwrap();
    let (first_top, pinned) = (top(&row), bottom(&row));

    // A wheel scroll down at the bottom, as a trackpad flick ending there, over the
    // visible end of the reply.
    let (x, _) = centre(&row);
    let y = pinned - 40.0;
    app.rt_mut().pointer_move(x, y);
    app.rt_mut().wheel(x, y, -80.0);
    app.frame();

    // More text: once it is laid out and the edge bounce has settled, the reply's bottom
    // edge is in view where it was.
    stream_paragraphs(&mut app, 30..40);
    app.until("the reply grows and stays pinned", |h| {
        reply_row(h).is_some_and(|row| top(&row) < first_top - 200.0 && bottom(&row) == pinned)
    });
}

/// Opens a running thread whose first reply has `paragraphs` paragraphs, lets it end, and
/// sends `next question` from the composer; returns once the message is held at the top, with
/// its top there.
fn send_and_hold(app: &mut Harness, paragraphs: usize) -> f32 {
    let project_id = app.project();
    app.open_running_thread(&project_id, "long");
    app.script
        .text("m1", json!({"type": "text_start", "contentIndex": 0}));
    stream_paragraphs(app, 0..paragraphs);
    app.script.settle("completed");
    app.until("the run ends", |h| !h.stoppable());

    app.click_composer();
    app.typed("next question");
    app.press(Key::Enter);
    app.until_called("prompt:next question:0");
    app.script.emit(json!({"type": "agent_start"}));

    // The message glides up until its bubble sits 24 points under the feed's top: its row
    // starts with the 44 points that open a new turn, so the row's top is 20 points above.
    let feed_top = top(&app.element("feed-area").unwrap());
    app.until("the message is held at the top", |h| {
        h.feed_row("next question")
            .is_some_and(|row| (top(&row) - (feed_top - 20.0)).abs() < 0.5)
    });
    app.script
        .text("m2", json!({"type": "text_start", "contentIndex": 0}));
    top(&app.feed_row("next question").unwrap())
}

#[test]
fn a_sent_message_is_held_at_the_top_while_its_reply_writes_down_under_it() {
    let mut app = Harness::start("anchor");
    let held = send_and_hold(&mut app, 30);

    // Its reply writes down under it, past the bottom of the feed, and nothing moves.
    for chunk in 0..6 {
        stream(&mut app, "m2", "Answer", chunk * 5..(chunk + 1) * 5);
        app.pause(Duration::from_millis(300));
        let message = app.feed_row("next question").unwrap();
        assert!(
            (top(&message) - held).abs() < 0.5,
            "the message moved from {held} to {} while its reply grew",
            top(&message)
        );
    }
    let answer = labelled_from(&app, "Answer 0:").unwrap();
    assert!(
        bottom(&answer) > app.rt().viewport.1,
        "the reply runs past the feed"
    );

    // The run ends; once the room below shrinks the message is still where it was.
    app.script.settle("completed");
    app.until("the run ends", |h| !h.stoppable());
    app.pause(Duration::from_millis(1500));
    let message = app.feed_row("next question").unwrap();
    assert!(
        (top(&message) - held).abs() < 0.5,
        "the message moved to {}",
        top(&message)
    );
}

#[test]
fn a_reader_who_scrolls_to_the_end_of_a_long_reply_is_followed() {
    let mut app = Harness::start("anchor-follow");
    send_and_hold(&mut app, 3);
    stream(&mut app, "m2", "Answer", 0..20);
    app.until("the reply runs past the feed", |h| {
        labelled_from(h, "Answer 0:").is_some_and(|row| bottom(&row) > h.rt().viewport.1)
    });

    // Scrolling down to its end, the reader lands on it and stays there as it grows.
    let (x, y) = centre(&app.element("feed-area").unwrap());
    app.rt_mut().pointer_move(x, y);
    for _ in 0..30 {
        app.rt_mut().wheel(x, y, -400.0);
        app.frame();
    }
    app.pause(Duration::from_millis(500));
    let end = bottom(&labelled_from(&app, "Answer 0:").unwrap());
    assert!(end < app.rt().viewport.1, "the reply's end is in view");
    stream(&mut app, "m2", "Answer", 20..30);
    app.until("the reply grows and its end stays in view", |h| {
        labelled_from(h, "Answer 0:").is_some_and(|row| {
            row.label
                .as_deref()
                .is_some_and(|label| label.contains("Answer 29:"))
                && (bottom(&row) - end).abs() < 0.5
        })
    });
}
