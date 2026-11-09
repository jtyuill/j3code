//! The file pane: rows from listings (notes, inherited ignores), the tree's keyboard, the
//! preview's line splitting and notices, and the pane in the window.

use j3_client::{
    Error,
    envelope::{self, ErrorCode},
    methods::{EntriesResult, ReadFileResult},
};
use rune::{Key, KeyEvent, Mods};
use rune_kit::syntax::{Paint, Syntax};

use super::{
    Effect, FilePane, fixtures,
    preview::{self, Body},
    tree::{self, Note, Row},
};
use crate::{
    actions::Action,
    app::{
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

/// The visible rows as `path` / `path (ignored)` / `[dir: note]`.
fn shown(pane: &FilePane) -> Vec<String> {
    pane.rows
        .iter()
        .map(|row| match row {
            Row::Entry { path, ignored, .. } if *ignored => format!("{path} (ignored)"),
            Row::Entry { path, .. } => path.clone(),
            Row::Note { dir, note, .. } => format!("[{dir}: {note:?}]"),
        })
        .collect()
}

#[test]
fn rows_follow_listings_with_notes_and_inherited_ignores() {
    let mut pane = fixtures::pane(
        "/p",
        "p",
        &["crates", "docs", "target"],
        &["crates/j3-desktop"],
    );
    assert_eq!(
        shown(&pane),
        [
            ".github",
            "crates",
            "crates/j3-client",
            "crates/j3-desktop",
            "[crates/j3-desktop: Loading]",
            "crates/j3-protocol",
            "docs",
            "[docs: Empty]",
            "target (ignored)",
            "target/debug (ignored)",
            "target/.rustc_info.json (ignored)",
            ".gitignore",
            "Cargo.lock",
            "Cargo.toml",
            "README.md",
            "flake.nix",
            "logo.png",
        ]
    );
    let depth = |path: &str| {
        pane.rows.iter().find_map(|row| match row {
            Row::Entry { path: p, depth, .. } if p == path => Some(*depth),
            Row::Note { dir, depth, .. } if dir == path => Some(*depth),
            _ => None,
        })
    };
    assert_eq!(depth("crates/j3-client"), Some(1));

    // A truncated listing says how many it shows; a failed one shows its error.
    let Effect::Load { request, .. } = pane.begin_load("docs") else {
        unreachable!()
    };
    let truncated = EntriesResult {
        entries: fixtures::listings()[1].1.clone(),
        truncated: true,
    };
    pane.listed("docs", request, Ok(truncated));
    assert!(shown(&pane).contains(&"[docs: Truncated(3)]".to_owned()));
    let Effect::Load { request, .. } = pane.begin_load("crates/j3-desktop") else {
        unreachable!()
    };
    pane.listed("crates/j3-desktop", request, Err(Error::Disconnected));
    assert!(pane.rows.iter().any(|row| matches!(
        row,
        Row::Note { dir, note: Note::Failed(_), key, .. }
            if dir == "crates/j3-desktop" && key == "file-note-crates/j3-desktop"
    )));
}

#[test]
fn an_answer_to_an_older_request_is_dropped() {
    let mut pane = FilePane::new("/p".into(), "p".into(), 16.0);
    let Effect::Load { request: first, .. } = pane.begin_load("") else {
        unreachable!()
    };
    let Effect::Load {
        request: second, ..
    } = pane.begin_load("")
    else {
        unreachable!()
    };
    pane.listed("", first, Err(Error::Disconnected));
    assert_eq!(shown(&pane), ["[: Loading]"]);
    pane.listed(
        "",
        second,
        Ok(EntriesResult {
            entries: Vec::new(),
            truncated: false,
        }),
    );
    assert_eq!(shown(&pane), ["[: Empty]"]);
}

#[test]
fn arrows_move_over_entries_expand_enter_collapse_and_climb() {
    let mut pane = fixtures::pane("/p", "p", &[], &[]);
    let key = |pane: &mut FilePane, k: Key| pane.key(&press(k));
    assert_eq!(key(&mut pane, Key::Up), (true, None));
    assert_eq!(
        pane.selected.as_deref(),
        Some("logo.png"),
        "↑ with nothing selected: last"
    );
    pane.selected = None;
    key(&mut pane, Key::Down);
    assert_eq!(pane.selected.as_deref(), Some(".github"));
    key(&mut pane, Key::Up);
    assert_eq!(
        pane.selected.as_deref(),
        Some(".github"),
        "stops at the top"
    );

    // → on a directory never listed expands it and lists it; ↓ skips its note.
    let (taken, effect) = key(&mut pane, Key::Right);
    assert!(taken);
    assert!(matches!(effect, Some(Effect::Load { ref dir, .. }) if dir == ".github"));
    assert_eq!(
        &shown(&pane)[..3],
        [".github", "[.github: Loading]", "crates"]
    );
    key(&mut pane, Key::Down);
    assert_eq!(pane.selected.as_deref(), Some("crates"));

    // → expands, → again enters the first child, ← climbs to the parent, ← collapses.
    let Some(Effect::Load { request, .. }) = key(&mut pane, Key::Right).1 else {
        panic!("crates lists")
    };
    pane.listed(
        "crates",
        request,
        Ok(EntriesResult {
            entries: fixtures::listings()[1].1.clone(),
            truncated: false,
        }),
    );
    assert_eq!(key(&mut pane, Key::Right), (true, None));
    assert_eq!(pane.selected.as_deref(), Some("crates/j3-client"));
    key(&mut pane, Key::Left);
    assert_eq!(pane.selected.as_deref(), Some("crates"));
    key(&mut pane, Key::Left);
    assert!(!pane.expanded.contains("crates"));
    assert!(!shown(&pane).contains(&"crates/j3-client".to_owned()));
    // Expanding a listed directory again needs no request.
    assert_eq!(key(&mut pane, Key::Right), (true, None));
    assert!(shown(&pane).contains(&"crates/j3-client".to_owned()));

    // Enter opens a file once; typing is swallowed; modified keys pass.
    pane.selected = Some("Cargo.toml".into());
    let (_, effect) = key(&mut pane, Key::Enter);
    assert!(matches!(effect, Some(Effect::Read { ref path, .. }) if path == "Cargo.toml"));
    assert_eq!(
        pane.preview.as_ref().map(|p| p.path.as_str()),
        Some("Cargo.toml")
    );
    assert_eq!(key(&mut pane, Key::Enter), (true, None), "already open");
    assert_eq!(key(&mut pane, Key::Text("x".into())), (true, None));
    let shortcut = KeyEvent {
        key: Key::Command('e'),
        mods: Mods {
            cmd: true,
            shift: true,
            ..Mods::default()
        },
    };
    assert_eq!(pane.key(&shortcut), (false, None));
}

#[test]
fn highlights_split_by_line_relative_to_each_line() {
    let (a, b) = (Paint::Kind(Syntax::String), Paint::Kind(Syntax::Comment));
    // "ab\r\n" "/* c" "d */ e" "" with a comment across lines 1-2.
    let text = "ab\r\n/* c\nd */ e\n";
    let highlights = [(0..1, a), (2..3, a), (4..13, b), (14..15, a)];
    let (lines, spans) = preview::split_lines(text, &highlights);
    assert_eq!(lines, [0..2, 4..8, 9..15, 16..16], "\\r stays out");
    assert_eq!(spans[0], [(0..1, a)]);
    assert_eq!(spans[1], [(0..4, b)]);
    assert_eq!(spans[2], [(0..4, b), (5..6, a)]);
    assert!(spans[3].is_empty());
}

#[test]
fn a_highlight_ending_on_a_newline_stays_on_its_line() {
    let a = Paint::Kind(Syntax::Keyword);
    let (_, spans) = preview::split_lines("x\ny", &[(0..2, a), (2..3, a)]);
    assert_eq!(spans, [vec![(0..1, a)], vec![(0..1, a)]]);
}

#[test]
fn reads_become_code_or_notices() {
    let file = |text: &str, size, truncated| ReadFileResult {
        path: "a.rs".into(),
        text: text.into(),
        size,
        truncated,
        exists: true,
    };
    let Body::Text(_) = Body::read("a.rs", Ok(file("fn main() {}\n", 13, false))) else {
        panic!("text")
    };
    let binary = Error::Server(envelope::Error::new(
        ErrorCode::Rejected,
        "Workspace file 'logo.png' is binary and cannot be previewed as text.",
    ));
    assert!(matches!(Body::read("logo.png", Err(binary)), Body::Binary));
    assert!(matches!(
        Body::read("a.rs", Err(Error::Disconnected)),
        Body::Failed(e) if e == Error::Disconnected.to_string()
    ));
    let big = "x".repeat(3 * 1024 + 410);
    let mut pane = FilePane::new("/p".into(), "p".into(), 16.0);
    let Effect::Read { request, .. } = pane.begin_read("big.txt".into()) else {
        unreachable!()
    };
    pane.file_read(
        "big.txt",
        request,
        Body::read("big.txt", Ok(file(&big, 1 << 20, true))),
    );
    assert_eq!(
        pane.preview.as_ref().and_then(|p| p.truncated()),
        Some("Showing the first 3.4 KB of 1.0 MB.")
    );
}

#[test]
fn icons_and_sizes() {
    let name = |file: &str| tree::file_icon(file).name;
    assert_eq!(name("Cargo.lock"), crate::icons::FILE_LOCK.name);
    assert_eq!(name("Cargo.toml"), crate::icons::FILE_COG.name);
    assert_eq!(name("lib.rs"), crate::icons::FILE_CODE.name);
    assert_eq!(name("logo.PNG"), crate::icons::FILE_IMAGE.name);
    assert_eq!(name("LICENSE"), crate::icons::FILE.name);
    assert_eq!(preview::format_size(512), "512 B");
    assert_eq!(preview::format_size(3 * 1024 + 410), "3.4 KB");
    assert_eq!(preview::format_size(1 << 20), "1.0 MB");
}

/// The key of the focused element.
fn focused(app: &TestApp) -> Option<String> {
    app.rt
        .elements()
        .into_iter()
        .find(|e| e.focused)
        .and_then(|e| e.key)
}

/// Frames until `done` holds (async answers land in the inbox from tokio), at most ~2 s.
fn until(app: &mut TestApp, done: impl Fn(&TestApp) -> bool) {
    for _ in 0..400 {
        app.frame();
        if done(app) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("timed out");
}

fn click(app: &mut TestApp, key: &str) {
    let (x, y) = app.rt.find(key).unwrap_or_else(|| panic!("{key} is drawn"));
    app.rt.pointer_down(x, y);
    app.rt.pointer_up(x, y);
    app.frame();
}

#[test]
fn the_pane_opens_focused_lists_and_closes_back_to_the_composer() {
    let mut app = TestApp::with_scene(Scene::Thread);
    let key = open_thread_key();
    app.settle(10);
    let open = key.clone();
    app.rt.update(move |s, tx| s.toggle_files(open, tx));
    app.settle(10);
    assert!(app.rt.find("file-pane").is_some());
    assert_eq!(focused(&app).as_deref(), Some("file-tree-focus"));
    // Offline, the root's listing fails and says so under the (empty) root.
    until(&mut app, |app| {
        app.rt.state.files[&key].rows.iter().any(
            |row| matches!(row, Row::Note { dir, note: Note::Failed(_), .. } if dir.is_empty()),
        )
    });
    assert!(app.rt.find("file-note-").is_some());
    assert!(app.rt.find("refresh-files").is_some());

    app.rt.update(|s, tx| s.dispatch(Action::ToggleFiles, tx));
    app.frame();
    assert!(app.rt.find("file-pane").is_none());
    assert!(
        app.rt.state.composers[&ComposerKey::Thread(key)]
            .edit
            .focused,
        "closing returns to the composer"
    );
}

#[test]
fn keys_and_clicks_drive_the_tree_in_the_window() {
    let mut app = TestApp::with_scene(Scene::Files);
    let key = open_thread_key();
    app.settle(10);
    assert_eq!(focused(&app).as_deref(), Some("file-tree-focus"));
    assert!(app.rt.find("file-preview-scroll").is_some());
    let selected = |app: &TestApp| app.rt.state.files[&key].selected.clone();

    app.key(press(Key::Down));
    assert_eq!(
        selected(&app).as_deref(),
        Some("crates/j3-desktop/Cargo.toml")
    );
    app.key(press(Key::Left));
    assert_eq!(selected(&app).as_deref(), Some("crates/j3-desktop"));
    app.key(press(Key::Left));
    assert!(
        app.rt.find("file-row-crates/j3-desktop/src").is_none(),
        "collapsed"
    );
    app.key(press(Key::Enter));
    assert!(
        app.rt.find("file-row-crates/j3-desktop/src").is_some(),
        "expanded again"
    );

    // A click on a directory toggles it and keeps the tree focused.
    let lib = "file-row-crates/j3-desktop/src/lib.rs";
    assert!(app.rt.find(lib).is_some());
    click(&mut app, "file-row-crates/j3-desktop/src");
    assert!(app.rt.find(lib).is_none());
    assert_eq!(selected(&app).as_deref(), Some("crates/j3-desktop/src"));
    app.frame();
    assert_eq!(focused(&app).as_deref(), Some("file-tree-focus"));

    // The preview's close button leaves the tree alone.
    click(&mut app, "close-file-preview");
    assert!(app.rt.find("file-preview").is_none());
    assert!(app.rt.find("file-tree").is_some());
}
