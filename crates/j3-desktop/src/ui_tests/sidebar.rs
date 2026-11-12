//! The Inbox and herdr sidebars over real threads: shelves, settle-and-advance, pins and
//! drag and drop, snoozes, attention order, glyphs, toasts and the goto picker.

use j3_client::methods::SnoozeParams;
use rune::Key;
use serde_json::json;

use super::harness::{Harness, primary, primary_shift};
use crate::sidebar::iso;

/// A thread row's element key (its sidebar row in Inbox mode).
fn thread_row(thread_id: &str) -> String {
    format!("thread-local-{thread_id}")
}

#[test]
fn inbox_sections_settle_and_advance_pin_and_reorder() {
    let mut app = Harness::start("inbox");
    let project_id = app.project();
    app.until("the empty list says so", |h| h.has_label("No threads yet"));

    // Three finished threads: unread ("Done") in Active, newest first.
    let alpha = app.completed(&project_id, "Alpha");
    let beta = app.completed(&project_id, "Beta");
    let gamma = app.completed(&project_id, "Gamma");
    app.until("the three show as Done", |h| {
        ["Alpha, Done", "Beta, Done", "Gamma, Done"]
            .iter()
            .all(|label| h.in_scope("inbox-active", label).is_some())
    });
    app.until("the rows are newest first", |h| {
        h.rows_in_order(&["Gamma, Done", "Beta, Done", "Alpha, Done"])
    });

    // Opening a thread reads it.
    app.click_sidebar("Gamma, Done");
    app.until("Gamma opens, read", |h| {
        h.in_sidebar("Gamma") && h.feed_row("Gamma").is_some()
    });

    // ⌘⇧S settles it and moves on to the next unsettled row.
    app.key(primary_shift('s'));
    app.until("Gamma settles and Beta opens", |h| {
        h.in_scope("inbox-settled", "Gamma, Settled").is_some()
            && h.feed_row("Beta").is_some()
            && h.in_sidebar("Beta")
    });
    // The Settled shelf sits below Active.
    let shelf = app.element("shelf-settled").expect("the Settled shelf");
    assert!(shelf.rect[1] > app.top_of("Alpha, Done"));
    assert_eq!(shelf.label.as_deref(), Some("Settled"));
    let settled = app.shell_thread(&gamma);
    assert!(settled.is_some_and(|thread| thread.triage.settled_at.is_some()));

    // ⌘⇧S on a settled thread un-settles it, in place.
    app.click_sidebar("Gamma, Settled");
    app.until("Gamma opens", |h| h.feed_row("Gamma").is_some());
    app.key(primary_shift('s'));
    app.until("Gamma is back in Active", |h| {
        h.in_scope("inbox-active", "Gamma").is_some()
    });

    // Pin two threads from the context menu.
    for (id, label) in [(&alpha, "Alpha, Done"), (&beta, "Beta")] {
        app.right_click(&thread_row(id));
        app.until("the row menu opens", |h| h.has_label("Pin thread"));
        app.click_label("Pin thread");
        app.until(&format!("{label} is pinned"), |h| {
            h.in_scope("inbox-pinned", label).is_some()
        });
    }
    // Keyless pins: the newest first.
    app.until("the pins are newest first", |h| {
        h.rows_in_order(&["Beta", "Alpha, Done", "Gamma"])
    });

    // Dragging Alpha onto Beta puts it first and gives both an order key.
    app.drag_to(&thread_row(&alpha), &thread_row(&beta));
    app.until("Alpha moves above Beta", |h| {
        h.rows_in_order(&["Alpha, Done", "Beta"])
    });
    app.until_shell("Beta gets an order key", &beta, |thread| {
        thread.triage.pin_order.is_some()
    });

    // Dropping a pinned row on the Settled shelf settles it.
    app.drag_to(&thread_row(&beta), "shelf-settled");
    app.until("Beta settles", |h| {
        h.in_scope("inbox-settled", "Beta, Settled").is_some()
            && h.label_of("shelf-settled").as_deref() == Some("Settled")
    });
}

#[test]
fn inbox_snooze_hides_a_thread_until_it_wakes() {
    let mut app = Harness::start("snooze");
    let project_id = app.project();
    let nap = app.completed(&project_id, "Nap");
    app.until("Nap is Done", |h| {
        h.in_scope("inbox-active", "Nap, Done").is_some()
    });

    // Snooze ▸ Custom… for an hour: the row moves to the Snoozed shelf.
    app.right_click(&thread_row(&nap));
    app.until("the row menu opens", |h| h.has_label("Snooze"));
    app.click_label("Snooze");
    app.until("the submenu opens", |h| h.has_label("Custom…"));
    app.click_label("Custom…");
    app.until("the dialog opens", |h| h.exists("dialog-confirm"));
    app.click("dialog-confirm");
    app.until("Nap is snoozed", |h| {
        h.in_scope("inbox-snoozed", "Nap, Snoozed").is_some()
            && h.in_scope("inbox-active", "Nap, Done").is_none()
    });
    assert_eq!(app.label_of("shelf-snoozed").as_deref(), Some("Snoozed 1"));

    // Wake thread brings it back at once.
    app.right_click(&thread_row(&nap));
    app.until("the row menu opens", |h| h.has_label("Wake thread"));
    app.click_label("Wake thread");
    app.until("Nap is awake", |h| {
        h.in_scope("inbox-active", "Nap, Done").is_some()
    });

    // A short snooze hides the row until its wake time, then the sidebar brings it back
    // by itself, marked as woken.
    let wake = time::OffsetDateTime::now_utc() + time::Duration::seconds(3);
    let connection = app.connection.clone();
    let params = SnoozeParams {
        thread_id: nap.clone(),
        until: iso(wake),
        command_id: None,
    };
    app.block_on(async move { connection.triage_snooze(params).await })
        .unwrap();
    app.until("Nap is snoozed", |h| {
        h.in_scope("inbox-snoozed", "Nap, Snoozed").is_some()
    });
    app.until("Nap wakes by itself", |h| {
        h.in_scope("inbox-active", "Nap, Woke").is_some() && !h.exists("inbox-snoozed")
    });
    assert!(time::OffsetDateTime::now_utc() >= wake);
}

#[test]
fn herdr_glyphs_attention_notifications_and_goto() {
    let mut app = Harness::start_ui(
        "herdr",
        json!({"workflow": "herdr", "agentOrder": "attention", "sounds": false}),
    );
    let project_id = app.project();
    app.until("the space shows", |h| h.in_sidebar("Demo, unknown"));

    // Idle: finished and read (opened).
    let idle = app.completed(&project_id, "Calm");
    app.until("Calm is done", |h| h.in_sidebar("Calm, done"));
    app.click_sidebar("Calm, done");
    app.until("Calm is idle", |h| h.in_sidebar("Calm, idle"));

    // Done: finished while another thread is open; it says so.
    let done = app.completed(&project_id, "Finish");
    app.until("Finish is done, with a toast", |h| {
        h.in_sidebar("Finish, done") && h.has_label("Finish finished")
    });

    // Blocked on a question.
    let blocked = app.launch(&project_id, "Ask");
    app.script.emit(
        json!({"type": "extension_ui_request", "id": "ui-1", "method": "select",
        "title": "Pick one", "options": ["alpha", "beta"]}),
    );
    app.until("Ask is blocked, with a toast", |h| {
        h.in_sidebar("Ask, blocked") && h.has_label("Ask needs attention")
    });

    // Working.
    let working = app.launch(&project_id, "Busy");
    app.until("Busy is working", |h| h.in_sidebar("Busy, working"));

    // Dots, and attention order: blocked, done, working, idle.
    let glyph = |app: &Harness, id: &str| app.label_of(&format!("glyph-local-{id}"));
    assert_eq!(glyph(&app, &blocked).as_deref(), Some("●"));
    assert_eq!(glyph(&app, &done).as_deref(), Some("●"));
    assert_eq!(glyph(&app, &working).as_deref(), Some("●"));
    assert_eq!(glyph(&app, &idle).as_deref(), Some("○"));
    // Rows glide to their new places: wait for them to arrive.
    app.until("the rows are in attention order", |h| {
        h.rows_in_order(&[
            "Ask, blocked",
            "Finish, done",
            "Busy, working",
            "Calm, idle",
        ])
    });
    // The space rolls up its most urgent agent.
    assert!(app.in_sidebar("Demo, blocked"));

    // Symbols, from Settings.
    app.key(primary(','));
    app.until("Settings opens", |h| h.exists("status-indicators-symbols"));
    app.click("status-indicators-symbols");
    app.click("close-settings");
    app.until("the glyphs are symbols", |h| {
        glyph(h, &blocked).as_deref() == Some("×")
    });
    assert_eq!(glyph(&app, &done).as_deref(), Some("✓"));
    assert_eq!(glyph(&app, &working).as_deref(), Some("◐"));
    assert_eq!(glyph(&app, &idle).as_deref(), Some("○"));

    // The goto picker: everything, then only what a filter key keeps.
    app.key(primary('g'));
    app.until("the picker lists Busy", |h| {
        h.in_scope("goto-picker", "Busy, working").is_some()
    });
    let picked = |app: &Harness| {
        [
            "Ask, blocked",
            "Finish, done",
            "Busy, working",
            "Calm, idle",
        ]
        .iter()
        .filter(|label| app.in_scope("goto-picker", label).is_some())
        .count()
    };
    assert_eq!(picked(&app), 4);
    app.typed("b");
    assert_eq!(picked(&app), 1);
    assert!(app.in_scope("goto-picker", "Ask, blocked").is_some());
    app.typed("a");
    assert_eq!(picked(&app), 4);
    app.typed("d");
    assert_eq!(picked(&app), 1);
    // Enter jumps; opening the done thread reads it.
    app.press(Key::Enter);
    app.until("Finish opens and is read", |h| {
        !h.exists("goto-picker") && h.feed_row("Finish").is_some() && h.in_sidebar("Finish, idle")
    });

    // Search narrows by name.
    app.key(primary('g'));
    app.until("the picker opens", |h| h.exists("goto-picker"));
    app.typed("/busy");
    app.until("the search keeps one", |h| picked(h) == 1);
    app.press(Key::Enter);
    app.until("Busy opens", |h| h.feed_row("Busy").is_some());

    // A thread that finishes while open was seen: idle, and no toast.
    app.script.emit_to(
        3,
        json!({"type": "prompt_result", "agentInvoked": true, "status": "completed",
            "sessionSettled": true}),
    );
    app.until("Busy is idle", |h| h.in_sidebar("Busy, idle"));
    assert!(!app.has_label("Busy finished"));
}
