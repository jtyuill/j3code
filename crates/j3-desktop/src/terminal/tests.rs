//! Headless drawer and terminal behaviour on the offline fixtures: fixture connections fail
//! every call at once, so attaching ends in a failure and the screen is fed directly.

use j3_client::Error;
use rune::{Ime, Key, KeyEvent, Mods, WheelEvent};

use super::{CellMetrics, PADDING, TerminalStatus};
use crate::{
    app::{J3, fixtures::Scene, testing::TestApp},
    sidebar::ThreadKey,
};

fn key() -> ThreadKey {
    crate::app::fixtures::open_thread_key()
}

fn focused(app: &TestApp) -> Option<String> {
    app.rt
        .elements()
        .into_iter()
        .find(|e| e.focused)
        .and_then(|e| e.key)
}

fn rect(app: &TestApp, key: &str) -> [f32; 4] {
    app.rt
        .elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some(key))
        .unwrap_or_else(|| panic!("no element {key}"))
        .rect
}

/// Frames (letting tokio answer) until `done` holds.
fn wait_for(app: &mut TestApp, done: impl Fn(&J3) -> bool) {
    for _ in 0..400 {
        app.frame();
        if done(&app.rt.state) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("timed out");
}

fn terminal<'a>(s: &'a J3, id: &str) -> &'a super::Terminal {
    s.terminals[&key()].terminal(id).expect("the terminal")
}

fn terminal_mut<'a>(s: &'a mut J3, id: &str) -> &'a mut super::Terminal {
    s.terminals
        .get_mut(&key())
        .and_then(|d| d.terminal_mut(id))
        .expect("the terminal")
}

/// The thread scene with its drawer toggled open and term-1's attach settled (failed).
fn opened() -> TestApp {
    let mut app = TestApp::with_scene(Scene::Thread);
    app.frame();
    app.rt.update(|s, tx| s.toggle_terminal(key(), tx));
    wait_for(&mut app, |s| {
        !matches!(terminal(s, "term-1").status(), TerminalStatus::Connecting)
    });
    app
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent {
        key: Key::Command(c),
        mods: Mods {
            cmd: true,
            ctrl: true,
            ..Mods::default()
        },
    }
}

fn plain(key: Key) -> KeyEvent {
    KeyEvent {
        key,
        mods: Mods::default(),
    }
}

#[test]
fn toggling_opens_a_focused_terminal_and_hiding_returns_to_the_composer() {
    let mut app = opened();
    for part in [
        "terminal-tab-term-1",
        "terminal-close-term-1",
        "terminal-new",
        "terminal-hide",
    ] {
        assert!(app.rt.find(part).is_some(), "{part}");
    }
    let tab = app
        .rt
        .elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some("terminal-tab-term-1"))
        .unwrap();
    assert_eq!(tab.label.as_deref(), Some("Terminal 1"));
    let hide = app
        .rt
        .elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some("terminal-hide"))
        .unwrap();
    assert!(hide.help.unwrap().starts_with("Hide terminal ("));
    assert_eq!(focused(&app).as_deref(), Some("terminal-term-1"));
    // Offline, the attach fails at once: the strip says why and offers Restart.
    assert_eq!(
        terminal(&app.rt.state, "term-1").status(),
        &TerminalStatus::Failed(Error::Closed.to_string())
    );
    assert!(app.rt.find("terminal-status").is_some());
    assert!(app.rt.find("terminal-restart").is_some());
    // ⌃` reaches the app from the focused terminal and hides the drawer.
    app.key(ctrl('`'));
    app.frame();
    assert!(!app.rt.state.terminals[&key()].open);
    assert!(app.rt.find("terminal-drawer").is_none());
    let composer = focused(&app).unwrap_or_default();
    assert!(
        !super::owns_focus(&composer),
        "focus left the terminal: {composer}"
    );
    // Opening it again keeps the same terminal.
    app.rt.update(|s, tx| s.toggle_terminal(key(), tx));
    app.frame();
    app.frame();
    assert_eq!(app.rt.state.terminals[&key()].tabs.len(), 1);
    assert_eq!(focused(&app).as_deref(), Some("terminal-term-1"));
}

#[test]
fn tabs_open_close_and_the_last_one_hides_the_drawer() {
    let mut app = opened();
    let (x, y) = app.rt.find("terminal-new").unwrap();
    app.rt.pointer_down(x, y);
    app.rt.pointer_up(x, y);
    app.frame();
    app.frame();
    assert!(app.rt.find("terminal-tab-term-2").is_some());
    assert_eq!(focused(&app).as_deref(), Some("terminal-term-2"));
    // Closing term-1 keeps term-2; the next new terminal takes the free number 1.
    let (x, y) = app.rt.find("terminal-close-term-1").unwrap();
    app.rt.pointer_down(x, y);
    app.rt.pointer_up(x, y);
    app.frame();
    assert!(app.rt.find("terminal-tab-term-1").is_none());
    app.rt.update(|s, _| s.new_terminal(&key()));
    app.frame();
    let ids: Vec<String> = app.rt.state.terminals[&key()]
        .tabs
        .iter()
        .map(|t| t.id.clone())
        .collect();
    assert_eq!(ids, ["term-2", "term-1"]);
    // Closing the selected tab selects the one before it, as the old drawer did.
    app.rt.update(|s, _| s.new_terminal(&key()));
    app.rt.update(|s, _| s.select_terminal(&key(), "term-1"));
    app.rt
        .update(|s, tx| s.close_terminal(&key(), "term-1", tx));
    app.frame();
    let drawer = &app.rt.state.terminals[&key()];
    assert_eq!(drawer.active().map(|t| t.id.as_str()), Some("term-2"));
    for id in ["term-3", "term-2"] {
        app.rt.update(move |s, tx| s.close_terminal(&key(), id, tx));
        app.frame();
    }
    assert!(!app.rt.state.terminals[&key()].open);
    assert!(app.rt.find("terminal-drawer").is_none());
}

#[test]
fn the_status_strip_names_how_the_shell_ended() {
    let exited = |code, signal| TerminalStatus::Exited { code, signal }.message();
    assert_eq!(
        exited(None, Some(9)).as_deref(),
        Some("Shell ended by signal 9")
    );
    assert_eq!(
        exited(Some(2), None).as_deref(),
        Some("Shell exited with code 2")
    );
    assert_eq!(exited(None, None).as_deref(), Some("Shell exited"));
    assert_eq!(
        TerminalStatus::Closed.message().as_deref(),
        Some("Terminal closed")
    );
    assert_eq!(TerminalStatus::Running.message(), None);
    assert_eq!(TerminalStatus::Connecting.message(), None);
}

#[test]
fn keys_and_pastes_reach_the_shell_and_wait_for_the_snapshot() {
    let mut app = opened();
    // Input typed while connecting waits for the snapshot.
    app.rt
        .update(|s, _| terminal_mut(s, "term-1").status = TerminalStatus::Connecting);
    app.key(plain(Key::Text("l".into())));
    app.key(plain(Key::Text("s".into())));
    app.key(plain(Key::Tab));
    app.key(plain(Key::Enter));
    app.key(ctrl('c'));
    app.key(plain(Key::Up));
    assert_eq!(
        focused(&app).as_deref(),
        Some("terminal-term-1"),
        "Tab stays in the terminal"
    );
    app.rt.paste("a\nb");
    app.frame();
    let pending = &terminal(&app.rt.state, "term-1").pending;
    if cfg!(target_os = "macos") {
        assert!(pending.starts_with("ls\t\r"));
    } else {
        assert_eq!(pending, "ls\t\r\x03\x1b[Aa\rb");
    }
    // The snapshot sends it (into the writer) and shows the history.
    app.rt.update(|s, tx| {
        let generation = terminal(s, "term-1").generation;
        let snapshot = serde_json::from_value(serde_json::json!({
            "type": "snapshot",
            "snapshot": {
                "threadId": "thr-feed-renderer", "terminalId": "term-1", "cwd": "/tmp",
                "worktreePath": null, "status": "running", "pid": 7,
                "history": "$ echo hi\r\nhi\r\n$ ", "exitCode": null, "exitSignal": null,
                "label": "bash", "updatedAt": "2026-11-07T16:30:00Z"
            }
        }))
        .unwrap();
        s.terminal_attached(&key(), "term-1", generation, Some(Ok(snapshot)), tx);
    });
    app.frame();
    let t = terminal(&app.rt.state, "term-1");
    assert_eq!(t.status(), &TerminalStatus::Running);
    assert!(t.pending.is_empty());
    assert_eq!(t.screen().text(), "$ echo hi\nhi\n$");
    assert!(app.rt.find("terminal-status").is_none());
}

#[test]
fn the_input_method_composes_at_the_cursor_and_commits_to_the_shell() {
    let mut app = opened();
    app.rt
        .update(|s, _| terminal_mut(s, "term-1").status = TerminalStatus::Connecting);
    let before = rect(&app, "terminal-term-1");
    app.rt.ime(Ime::Preedit("か".into(), Some((0, 3))));
    app.frame();
    app.frame();
    let composing = rect(&app, "terminal-term-1");
    assert_eq!(
        composing[..2],
        before[..2],
        "the composition sits at the cursor"
    );
    assert!(composing[2] > before[2], "and has room to show");
    app.rt.ime(Ime::Commit("家".into()));
    app.frame();
    assert_eq!(terminal(&app.rt.state, "term-1").pending, "家");
    assert!(terminal(&app.rt.state, "term-1").edit.text.is_empty());
}

#[test]
fn the_grid_follows_the_layout_and_the_pty_follows_after_a_pause() {
    let mut app = opened();
    let cell = CellMetrics::new(app.rt.state.prefs.ui().terminal_font_size());
    let view = rect(&app, "terminal-view-term-1");
    let size = terminal(&app.rt.state, "term-1").screen().size();
    assert_eq!(size, cell.grid([view[2], view[3]]));
    app.rt
        .update(|s, _| s.terminals.get_mut(&key()).unwrap().height = 400.0);
    app.frame();
    app.frame();
    let taller = terminal(&app.rt.state, "term-1").screen().size();
    assert!(taller.rows > size.rows, "{taller:?} vs {size:?}");
    assert!(terminal(&app.rt.state, "term-1").resize_at.is_some());
    for _ in 0..12 {
        app.frame();
    }
    assert!(
        terminal(&app.rt.state, "term-1").resize_at.is_none(),
        "sent after 120 ms"
    );
}

#[test]
fn a_dropped_connection_attaches_again_after_two_seconds() {
    let mut app = opened();
    let generation = terminal(&app.rt.state, "term-1").generation;
    app.rt.update(move |s, tx| {
        s.terminal_attached(
            &key(),
            "term-1",
            generation,
            Some(Err(Error::Disconnected("lost".into()))),
            tx,
        )
    });
    app.frame();
    assert!(terminal(&app.rt.state, "term-1").reattach_at.is_some());
    for _ in 0..60 {
        app.frame();
    }
    assert_eq!(terminal(&app.rt.state, "term-1").generation, generation);
    for _ in 0..70 {
        app.frame();
    }
    assert_eq!(terminal(&app.rt.state, "term-1").generation, generation + 1);
    // Events of the old stream no longer apply.
    app.rt
        .update(move |s, tx| s.terminal_attached(&key(), "term-1", generation, None, tx));
    assert_ne!(
        terminal(&app.rt.state, "term-1").status(),
        &TerminalStatus::Closed
    );
}

#[test]
fn the_scene_terminal_draws_its_coloured_output() {
    let mut app = TestApp::with_scene(Scene::Terminal);
    app.frame();
    app.frame();
    let t = terminal(&app.rt.state, "term-1");
    let text = t.screen().text();
    assert!(text.contains("Cargo.toml  README.md"), "{text}");
    assert!(app.rt.find("terminal-tab-term-2").is_some());
    let texts: Vec<String> = app
        .rt
        .elements()
        .into_iter()
        .filter_map(|e| e.text)
        .collect();
    assert!(
        texts
            .iter()
            .any(|t| t.starts_with("jacob@workstation:~/repos/j3code$ git status -sb")),
        "a row's text is one run: {texts:?}"
    );
    // The focused terminal draws its block cursor in its cell, not as an overlay.
    assert_eq!(focused(&app).as_deref(), Some("terminal-term-1"));
    assert!(app.rt.find("terminal-cursor-term-1").is_none());
}

#[test]
fn the_grid_is_measured_in_unzoomed_points() {
    let mut app = opened();
    app.rt.update(|s, tx| s.zoom(2, tx));
    app.settle(20);
    let zoom = app.rt.state.prefs.ui().zoom();
    assert!(zoom > 1.0);
    let cell = CellMetrics::new(app.rt.state.prefs.ui().terminal_font_size());
    let drawn = rect(&app, "terminal-view-term-1");
    let size = terminal(&app.rt.state, "term-1").screen().size();
    assert_eq!(size, cell.grid([drawn[2] / zoom, drawn[3] / zoom]));
}

#[test]
fn pointer_drags_select_and_the_wheel_scrolls_the_scrollback() {
    let mut app = opened();
    app.rt.update(|s, _| {
        let t = terminal_mut(s, "term-1");
        t.status = TerminalStatus::Running;
        let lines: String = (0..60).map(|n| format!("line {n}\r\n")).collect();
        t.screen
            .feed(format!("{lines}$ echo selected words").as_bytes());
    });
    // The status strip goes once the shell runs: the grid grows into its room.
    app.settle(10);
    let cell = CellMetrics::new(app.rt.state.prefs.ui().terminal_font_size());
    let view = rect(&app, "terminal-view-term-1");
    let rows = usize::from(terminal(&app.rt.state, "term-1").screen().size().rows);
    let at = |col: f32, line: usize| {
        (
            view[0] + PADDING + (col + 0.25) * cell.width,
            view[1] + PADDING + (line as f32 + 0.5) * cell.height,
        )
    };
    let last = rows - 1;
    let (x0, y0) = at(7.0, last);
    let (x1, y1) = at(15.0, last);
    app.rt.pointer_down(x0, y0);
    app.rt.pointer_move((x0 + x1) / 2.0, y1);
    app.rt.pointer_move(x1, y1);
    app.rt.pointer_up(x1, y1);
    app.frame();
    assert_eq!(
        terminal(&app.rt.state, "term-1")
            .screen()
            .selected_text()
            .as_deref(),
        Some("selected")
    );
    // Typing returns to the live screen and drops the selection; the wheel scrolls back.
    let (x, y) = at(2.0, 1);
    app.rt
        .wheel_event(WheelEvent::new(0.0, 3.0 * 40.0).at(x, y));
    app.frame();
    let text = terminal(&app.rt.state, "term-1").screen().text();
    assert!(!text.ends_with("selected words"), "scrolled back: {text}");
    app.key(plain(Key::Text("x".into())));
    let t = terminal(&app.rt.state, "term-1");
    assert!(t.screen().text().ends_with("selected words"));
    assert_eq!(t.screen().selected_text(), None);
}
