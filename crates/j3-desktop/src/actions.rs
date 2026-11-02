//! Commands, key bindings and the menu bar. Each command is one action; the
//! workspace owns their handlers so menus, keys and buttons agree.

use std::rc::Rc;

use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::{
    Action, App, KeyBinding, KeyBindingContextPredicate, Keystroke, Menu, MenuItem, NoAction,
    OperatingSystem, OsAction, actions,
};

use crate::{services::Services, terminal_drawer, workspace::Workspace};

/// Key context of the workspace root.
pub const WORKSPACE: &str = "Workspace";
/// Key context of the thread sidebar.
pub const SIDEBAR: &str = "Sidebar";
/// Key context of the settings view.
pub const SETTINGS: &str = "Settings";
/// Key context of the goto picker.
pub const GOTO: &str = "GotoPicker";
/// Key context of a focused terminal.
pub const TERMINAL: &str = "Terminal";

actions!(
    j3,
    [
        /// Quit, stopping the local server's omp sessions first.
        Quit,
        /// Open (or close) Settings.
        OpenSettings,
        /// Leave Settings.
        CloseSettings,
        /// Start a new thread in the selected project.
        NewThread,
        /// Add a project to the selected environment.
        AddProject,
        /// Add a remote environment.
        AddEnvironment,
        /// Stop the selected thread's run.
        StopRun,
        /// Rename the selected thread.
        RenameThread,
        /// Archive the selected thread.
        ArchiveThread,
        /// Delete the selected thread.
        DeleteThread,
        /// Settle the open thread and move on to the next unsettled one (or
        /// un-settle it if it is settled).
        SettleThread,
        /// Show archived threads (in Settings).
        ShowArchived,
        /// Open the goto picker over every agent.
        GotoAgent,
        /// Move keyboard focus to the composer.
        FocusComposer,
        /// Move keyboard focus to the sidebar.
        FocusSidebar,
        /// Open the next thread of the sidebar.
        NextThread,
        /// Open the previous thread of the sidebar.
        PreviousThread,
        /// Make the whole interface larger.
        ZoomIn,
        /// Make the whole interface smaller.
        ZoomOut,
        /// Back to 100%.
        ResetZoom,
        /// Open (or hide) the open thread's terminal drawer.
        ToggleTerminal,
    ]
);

/// Open the sidebar's Nth thread (1-based), as t3's `thread.jump.N`.
#[derive(Clone, Debug, PartialEq, Eq, Action)]
#[action(namespace = j3, no_json)]
pub struct JumpToThread(pub usize);

/// A binding written with GPUI's `secondary-` modifier: ⌘ on macOS, Ctrl
/// elsewhere.
fn bind(keys: &str, action: impl Action, context: Option<&str>, cx: &App) -> KeyBinding {
    let predicate = context.map(|context| {
        Rc::new(KeyBindingContextPredicate::parse(context).expect("static key context"))
    });
    KeyBinding::load(keys, Box::new(action), predicate, false, None, cx)
        .expect("static key bindings parse")
}

/// How `keys` (GPUI keystroke syntax, `secondary-` allowed) reads on `os`,
/// e.g. `secondary-n` is `⌘N` on macOS and `Ctrl+N` on Linux.
pub fn format_shortcut(keys: &str, os: OperatingSystem) -> String {
    keys.split_whitespace()
        .map(|stroke| match Keystroke::parse_with_os(stroke, os) {
            Ok(keystroke) => Kbd::format(&keystroke),
            Err(_) => stroke.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// [`format_shortcut`] for the running platform.
pub fn shortcut(keys: &str, cx: &App) -> String {
    format_shortcut(keys, cx.platform_info().os)
}

pub fn init(cx: &mut App) {
    // Bindings first: `set_menus` bakes shortcuts in when it is called.
    let ws = Some(WORKSPACE);
    let mut bindings = vec![
        bind("secondary-q", Quit, None, cx),
        bind("secondary-,", OpenSettings, ws, cx),
        bind("escape", CloseSettings, Some(SETTINGS), cx),
        bind("secondary-n", NewThread, ws, cx),
        bind("secondary-o", AddProject, ws, cx),
        bind("secondary-.", StopRun, ws, cx),
        bind("secondary-shift-r", RenameThread, ws, cx),
        bind("secondary-shift-a", ArchiveThread, ws, cx),
        bind("secondary-shift-backspace", DeleteThread, ws, cx),
        bind("secondary-shift-s", SettleThread, ws, cx),
        bind("secondary-g", GotoAgent, ws, cx),
        bind("secondary-l", FocusComposer, ws, cx),
        bind("secondary-0", FocusSidebar, ws, cx),
        bind("secondary-shift-]", NextThread, ws, cx),
        bind("secondary-shift-[", PreviousThread, ws, cx),
        bind("down", NextThread, Some(SIDEBAR), cx),
        bind("up", PreviousThread, Some(SIDEBAR), cx),
        // `+` is shift-= on most layouts; both zoom in.
        bind("secondary-=", ZoomIn, ws, cx),
        bind("secondary-+", ZoomIn, ws, cx),
        bind("secondary--", ZoomOut, ws, cx),
        bind(terminal_drawer::TOGGLE_KEYS, ToggleTerminal, ws, cx),
    ];
    bindings.extend((1..=9).map(|n| bind(&format!("secondary-{n}"), JumpToThread(n), ws, cx)));
    bindings.extend(terminal_bindings(cx));
    cx.bind_keys(bindings);
    cx.on_action(|_: &Quit, cx| quit(cx));
    cx.on_window_closed(|cx, _| {
        if cx.windows().is_empty() {
            quit(cx);
        }
    })
    .detach();
    cx.set_menus([
        Menu::new("j3").items([
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::action("Quit j3", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Thread", NewThread),
            MenuItem::action("Add Project…", AddProject),
            MenuItem::separator(),
            MenuItem::action("Add Environment…", AddEnvironment),
            MenuItem::action("Archived Threads", ShowArchived),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("Thread").items([
            MenuItem::action("Settle", SettleThread),
            MenuItem::action("Stop", StopRun),
            MenuItem::action("Rename…", RenameThread),
            MenuItem::action("Archive", ArchiveThread),
            MenuItem::action("Delete…", DeleteThread),
            MenuItem::separator(),
            MenuItem::action("Go to Agent…", GotoAgent),
            MenuItem::action("Next Thread", NextThread),
            MenuItem::action("Previous Thread", PreviousThread),
        ]),
        Menu::new("View").items([
            MenuItem::action("Focus Composer", FocusComposer),
            MenuItem::action("Focus Sidebar", FocusSidebar),
            MenuItem::action("Toggle Terminal", ToggleTerminal),
            MenuItem::separator(),
            MenuItem::action("Zoom In", ZoomIn),
            MenuItem::action("Zoom Out", ZoomOut),
            MenuItem::action("Actual Size", ResetZoom),
        ]),
    ]);
}

/// A focused terminal takes the keys a shell needs: Tab and Shift-Tab
/// (otherwise focus traversal) and, where `secondary-` is Ctrl, every
/// Ctrl-letter (otherwise app shortcuts). Copy and paste are ⌘C/⌘V on
/// macOS and Ctrl-Shift-C/V elsewhere, since Ctrl-C interrupts.
fn terminal_bindings(cx: &App) -> Vec<KeyBinding> {
    let terminal = Some(TERMINAL);
    let mac = cx.platform_info().os == OperatingSystem::Mac;
    let mut bindings = vec![
        bind("tab", NoAction, terminal, cx),
        bind("shift-tab", NoAction, terminal, cx),
    ];
    if mac {
        bindings.push(bind("cmd-c", Copy, terminal, cx));
        bindings.push(bind("cmd-v", Paste, terminal, cx));
    } else {
        bindings.push(bind("ctrl-shift-c", Copy, terminal, cx));
        bindings.push(bind("ctrl-shift-v", Paste, terminal, cx));
        bindings
            .extend(('a'..='z').map(|key| bind(&format!("ctrl-{key}"), NoAction, terminal, cx)));
    }
    bindings
}

/// Closes connections, stops the local server (bounded) and quits.
pub fn quit(cx: &mut App) {
    let connections = Workspace::global_connections(cx);
    Services::quit(connections, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secondary_shortcuts_follow_the_platform() {
        // `Kbd::format` renders with this build's conventions; `secondary-`
        // resolves to ⌘ for macOS and Ctrl for Linux.
        if cfg!(target_os = "macos") {
            assert_eq!(format_shortcut("secondary-n", OperatingSystem::Mac), "⌘N");
            assert_eq!(format_shortcut("secondary-n", OperatingSystem::Linux), "⌃N");
            assert_eq!(format_shortcut("shift-enter", OperatingSystem::Mac), "⇧⏎");
        } else {
            assert_eq!(
                format_shortcut("secondary-n", OperatingSystem::Linux),
                "Ctrl+N"
            );
            assert_eq!(
                format_shortcut("shift-enter", OperatingSystem::Linux),
                "Shift+Enter"
            );
        }
        assert_eq!(
            format_shortcut("not-a-valid--key", OperatingSystem::Mac),
            "not-a-valid--key"
        );
    }
}
