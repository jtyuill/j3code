//! Commands, key bindings and the menu bar. Each command is one action; the
//! workspace owns their handlers so menus, keys and buttons agree.

use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
use gpui_kit::{App, KeyBinding, Menu, MenuItem, OsAction, actions};

use crate::{services::Services, workspace::Workspace};

/// Key context of the workspace root.
pub const WORKSPACE: &str = "Workspace";

actions!(
    j3,
    [
        /// Quit, stopping the local server's omp sessions first.
        Quit,
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
        /// Show the selected environment's archived threads.
        ShowArchived,
        /// Move keyboard focus to the composer.
        FocusComposer,
        /// Move keyboard focus to the sidebar.
        FocusSidebar,
        /// Select the next thread in the sidebar.
        NextThread,
        /// Select the previous thread in the sidebar.
        PreviousThread,
    ]
);

pub fn init(cx: &mut App) {
    // Bindings first: `set_menus` bakes shortcuts in when it is called.
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-n", NewThread, Some(WORKSPACE)),
        KeyBinding::new("cmd-o", AddProject, Some(WORKSPACE)),
        KeyBinding::new("cmd-.", StopRun, Some(WORKSPACE)),
        KeyBinding::new("cmd-shift-r", RenameThread, Some(WORKSPACE)),
        KeyBinding::new("cmd-shift-a", ArchiveThread, Some(WORKSPACE)),
        KeyBinding::new("cmd-shift-backspace", DeleteThread, Some(WORKSPACE)),
        KeyBinding::new("cmd-l", FocusComposer, Some(WORKSPACE)),
        KeyBinding::new("cmd-0", FocusSidebar, Some(WORKSPACE)),
        KeyBinding::new("cmd-]", NextThread, Some(WORKSPACE)),
        KeyBinding::new("cmd-[", PreviousThread, Some(WORKSPACE)),
    ]);
    cx.on_action(|_: &Quit, cx| quit(cx));
    cx.on_window_closed(|cx, _| {
        if cx.windows().is_empty() {
            quit(cx);
        }
    })
    .detach();
    cx.set_menus([
        Menu::new("j3").items([MenuItem::action("Quit j3", Quit)]),
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
            MenuItem::action("Stop", StopRun),
            MenuItem::action("Rename…", RenameThread),
            MenuItem::action("Archive", ArchiveThread),
            MenuItem::action("Delete…", DeleteThread),
            MenuItem::separator(),
            MenuItem::action("Next Thread", NextThread),
            MenuItem::action("Previous Thread", PreviousThread),
        ]),
        Menu::new("View").items([
            MenuItem::action("Focus Composer", FocusComposer),
            MenuItem::action("Focus Sidebar", FocusSidebar),
        ]),
    ]);
}

fn quit(cx: &mut App) {
    let connections = Workspace::global_connections(cx);
    Services::quit(connections, cx);
}
