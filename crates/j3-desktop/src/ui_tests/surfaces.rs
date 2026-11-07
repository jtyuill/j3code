//! The thread's side surfaces on the real server: the terminal drawer, the git control
//! and the file pane.

use std::path::Path;

use rune::{Key, KeyEvent, Mods};

use super::harness::{Harness, top};
use crate::{actions::Action, sidebar::ThreadKey};

/// ⌃` (the terminal toggle).
fn ctrl_backtick() -> KeyEvent {
    KeyEvent {
        key: Key::Command('`'),
        mods: Mods {
            ctrl: true,
            ..Mods::default()
        },
    }
}

/// The visible text of terminal `id` of thread `key` (its element's accessibility value,
/// which element info doesn't carry: read from the screen it is made of).
fn terminal_text(app: &Harness, key: &ThreadKey, id: &str) -> String {
    app.state()
        .terminals
        .get(key)
        .and_then(|drawer| drawer.terminal(id))
        .map(|terminal| terminal.screen().text())
        .unwrap_or_default()
}

#[test]
fn the_terminal_drawer_runs_a_shell_on_the_server() {
    let mut app = Harness::start("terminal");
    let project_id = app.project();
    let thread_id = app.open_running_thread(&project_id, "shell");
    let key = ThreadKey::new("local", &thread_id);

    // ctrl-` opens the drawer with the focus in its first terminal, whose shell runs on
    // the server and answers typed input.
    app.key(ctrl_backtick());
    app.until("term-1 has the keyboard", |h| {
        h.focused().as_deref() == Some("terminal-term-1")
    });
    app.typed("echo hi-$((40+2))\n");
    app.until("the shell answers", |h| {
        terminal_text(h, &key, "term-1").contains("hi-42")
    });

    // The header button hides it and the composer gets the focus back.
    app.click("toggle-terminal");
    app.until("the drawer hides", |h| !h.exists("terminal-drawer"));
    assert!(app.composer_focused());

    // Reopened, the same shell is there with its output.
    app.key(ctrl_backtick());
    app.until("the drawer shows the output again", |h| {
        h.exists("terminal-drawer") && terminal_text(h, &key, "term-1").contains("hi-42")
    });

    // `+` opens a second terminal; its × ends it.
    app.click("terminal-new");
    app.until("term-2 has the keyboard", |h| {
        h.focused().as_deref() == Some("terminal-term-2")
    });
    app.click("terminal-close-term-2");
    app.until("term-2 closes", |h| {
        !h.exists("terminal-tab-term-2") && h.exists("terminal-term-1")
    });

    // An exited shell shows why, and Restart starts a new one. (`terminal-term-1` is the
    // terminal's hidden focus field; a press on its view focuses it.)
    app.click("terminal-view-term-1");
    app.until("term-1 has the keyboard", |h| {
        h.focused().as_deref() == Some("terminal-term-1")
    });
    app.typed("exit 7\n");
    app.until("the status says how the shell ended", |h| {
        h.label_of("terminal-status").as_deref() == Some("Shell exited with code 7")
    });
    app.click("terminal-restart");
    app.until("the status goes", |h| !h.exists("terminal-status"));
    app.until("the new shell has the keyboard", |h| {
        h.focused().as_deref() == Some("terminal-term-1")
    });
    app.typed("echo back-$((1+1))\n");
    app.until("the new shell answers", |h| {
        terminal_text(h, &key, "term-1").contains("back-2")
    });
}

/// Runs `git` in `dir`; returns its trimmed stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[test]
fn the_header_commits_uncommitted_changes() {
    let mut app = Harness::start("git");
    // A repository without a remote, with one uncommitted file.
    let dir = app.project_dir.clone();
    git(&dir, &["init", "-q"]);
    git(&dir, &["config", "user.name", "j3 test"]);
    git(&dir, &["config", "user.email", "test@j3.invalid"]);
    std::fs::write(dir.join("notes.txt"), "one\ntwo\n").unwrap();
    let project_id = app.project();
    app.open_running_thread(&project_id, "write notes");

    // Changes and no remote: the quick action commits.
    app.until("the quick action is Commit", |h| {
        h.label_of("git-quick-action").as_deref() == Some("Commit")
    });

    // Commit… opens the dialog listing the file; a typed message is used as is.
    app.click_label("Git actions");
    app.until("the menu opens", |h| h.has_label("Commit…"));
    app.click_label("Commit…");
    app.until("the dialog opens", |h| h.exists("git-commit"));
    assert!(app.exists("git-commit-files"));
    app.typed("Add notes");
    app.click("git-commit");
    let repo = dir.clone();
    app.until("the commit lands", move |_| {
        std::process::Command::new("git")
            .args(["log", "-1", "--format=%s"])
            .current_dir(&repo)
            .output()
            .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "Add notes")
    });
    assert_eq!(git(&dir, &["status", "--porcelain"]), "");
    // The action ended: the button is back, with nothing left to commit.
    app.until("the quick action is back", |h| {
        h.label_of("git-quick-action").as_deref() == Some("Commit") && !h.has_label("Commit…")
    });
}

#[test]
fn the_file_pane_browses_and_previews_the_project() {
    let mut app = Harness::start("files");
    let dir = app.project_dir.clone();
    git(&dir, &["init", "-q"]);
    std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("target")).unwrap();
    std::fs::write(
        dir.join("src/main.rs"),
        "fn main() {\n    println!(\"hi\");\n}\n",
    )
    .unwrap();
    std::fs::write(dir.join("logo.png"), b"\x89PNG\r\n\x1a\n\0\0").unwrap();
    let project_id = app.project();
    let thread_id = app.open_running_thread(&project_id, "browse files");

    // The header button opens the pane on the project root, focused, with directories
    // first.
    app.click("toggle-files");
    app.until("the root lists", |h| {
        h.has_label("target") && h.has_label("logo.png")
    });
    // The tree takes the keyboard through its focus field.
    assert_eq!(app.focused().as_deref(), Some("file-tree-focus"));
    let row_top = |app: &Harness, label: &str| top(&app.labelled(label).unwrap());
    assert!(row_top(&app, "src") < row_top(&app, "target"));
    assert!(row_top(&app, "target") < row_top(&app, ".gitignore"));
    assert!(row_top(&app, ".gitignore") < row_top(&app, "logo.png"));

    // A click expands a directory; its entries load beneath it.
    app.click_label("src");
    app.until("src lists", |h| h.has_label("src/main.rs"));

    // ↓ moves to the file and Enter previews it, line numbers and all.
    app.press(Key::Down);
    app.press(Key::Enter);
    app.until("the preview shows the file", |h| {
        h.has_label("Contents of src/main.rs")
            && h.elements().into_iter().any(|e| {
                e.path.split('/').any(|part| part == "file-preview-scroll")
                    && e.text
                        .as_deref()
                        .is_some_and(|text| text.contains("println!(\"hi\")"))
            })
    });

    // A binary file gets a notice instead of a preview.
    app.click_label("logo.png");
    app.until("the binary notice shows", |h| {
        h.has_label("Binary file; there is no preview.")
    });

    // The shortcut's action closes the pane and returns to the composer.
    app.rt_mut()
        .update(|s, tx| s.dispatch(Action::ToggleFiles, tx));
    app.until("the pane closes", |h| !h.exists("file-pane"));
    assert!(app.composer_focused());
    assert!(!app.state().files[&ThreadKey::new("local", &thread_id)].open);
}
