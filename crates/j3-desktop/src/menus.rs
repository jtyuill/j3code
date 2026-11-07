//! The macOS menu bar (spec §9): j3, File, Thread and View, each item an [`Action`] with the
//! key equivalent its first [`BINDINGS`] row gives. Edit (Undo, Redo, Cut, Copy, Paste,
//! Select All) and Window are the standard menus Rune keeps on every macOS app, so they are
//! not listed here.
//!
//! [`MENUS`] is plain data and [`items`] builds Rune's items from it on every platform (so
//! both are tested everywhere); only macOS installs them, since Rune's native menus exist
//! only there. Items let the window see their keys first (`keys_first`), so a key still goes
//! through `on_key` and its context (a focused terminal keeps its keys) and the menu runs
//! the action only when nothing took the key.

use rune::{KeyPlatform, MenuItem, Shortcut, Tx};

use crate::{
    actions::{Action, BINDINGS},
    app::J3,
};

/// One line of a menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    Item(&'static str, Action),
    Separator,
}

use Entry::{Item, Separator};

/// The app's menus in menu-bar order; the first is the application menu.
pub const MENUS: &[(&str, &[Entry])] = &[
    (
        "j3",
        &[
            Item("Settings…", Action::OpenSettings),
            Separator,
            Item("Quit j3", Action::Quit),
        ],
    ),
    (
        "File",
        &[
            Item("New Thread", Action::NewThread),
            Item("Add Project…", Action::AddProject),
            Separator,
            Item("Add Environment…", Action::AddEnvironment),
            Item("Archived Threads", Action::ShowArchived),
        ],
    ),
    (
        "Thread",
        &[
            Item("Settle", Action::SettleThread),
            Item("Stop", Action::StopRun),
            Item("Rename…", Action::RenameThread),
            Item("Archive", Action::ArchiveThread),
            Item("Delete…", Action::DeleteThread),
            Separator,
            Item("Go to Agent…", Action::GotoAgent),
            Item("Next Thread", Action::NextThread),
            Item("Previous Thread", Action::PreviousThread),
        ],
    ),
    (
        "View",
        &[
            Item("Focus Composer", Action::FocusComposer),
            Item("Focus Sidebar", Action::FocusSidebar),
            Item("Toggle Terminal", Action::ToggleTerminal),
            Separator,
            Item("Files", Action::ToggleFiles),
            Separator,
            Item("Zoom In", Action::ZoomIn),
            Item("Zoom Out", Action::ZoomOut),
            Item("Actual Size", Action::ResetZoom),
        ],
    ),
];

/// The keys of `action`'s first binding (its first alternative) as a macOS key equivalent.
pub fn key_equivalent(action: Action) -> Option<Shortcut> {
    let keys = BINDINGS.iter().find(|b| b.action == action)?.keys;
    let first = keys.split(", ").next()?.trim();
    Shortcut::parse_on(first, KeyPlatform::Mac).ok()
}

/// What picking a menu item runs: `action` through `J3::dispatch`. Rune's items take plain
/// function pointers, hence one arm per menu action.
fn run(action: Action) -> fn(&mut J3, &mut Tx) {
    macro_rules! dispatch {
        ($($action:ident),* $(,)?) => {
            match action {
                $(Action::$action => |s: &mut J3, tx: &mut Tx| s.dispatch(Action::$action, tx),)*
                // Not in a menu: it needs its sidebar row number.
                Action::JumpToThread(_) => |_: &mut J3, _: &mut Tx| {},
            }
        };
    }
    dispatch!(
        Quit,
        OpenSettings,
        CloseSettings,
        NewThread,
        AddProject,
        AddEnvironment,
        StopRun,
        RenameThread,
        ArchiveThread,
        DeleteThread,
        SettleThread,
        ShowArchived,
        GotoAgent,
        FocusComposer,
        FocusSidebar,
        ToggleFiles,
        NextThread,
        PreviousThread,
        ZoomIn,
        ZoomOut,
        ResetZoom,
        ToggleTerminal,
    )
}

/// A menu item's stable id: `{menu}-{title}` lowercased, words dashed, `…` dropped.
fn item_id(menu: &str, title: &str) -> String {
    let title: String = title
        .trim_end_matches('…')
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-");
    format!("{}-{title}", menu.to_lowercase())
}

/// Rune's items for [`MENUS`]: the first menu's go in the application menu.
pub fn items() -> Vec<MenuItem<J3>> {
    let mut items = Vec::new();
    for (ix, (menu, entries)) in MENUS.iter().enumerate() {
        for entry in *entries {
            let mut item = match *entry {
                Separator => MenuItem::separator(),
                Item(title, action) => {
                    let mut item = MenuItem::new(title, run(action))
                        .id(item_id(menu, title))
                        .keys_first();
                    if let Some(keys) = key_equivalent(action) {
                        item = item.key_equivalent(keys);
                    }
                    item
                }
            };
            if ix > 0 {
                item = item.menu(*menu);
            }
            items.push(item);
        }
    }
    items
}

/// Installs the menu bar (macOS; other platforms have no native menu bar).
pub fn install(app: rune::App<J3>) -> rune::App<J3> {
    if !cfg!(target_os = "macos") {
        return app;
    }
    items().into_iter().fold(app, rune::App::menu_item)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(menu: &str) -> Vec<&'static str> {
        MENUS
            .iter()
            .find(|(title, _)| *title == menu)
            .map(|(_, entries)| {
                entries
                    .iter()
                    .map(|entry| match entry {
                        Item(title, _) => *title,
                        Separator => "-",
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn the_menu_bar_lists_the_old_apps_menus() {
        let menus: Vec<&str> = MENUS.iter().map(|(title, _)| *title).collect();
        assert_eq!(menus, ["j3", "File", "Thread", "View"]);
        assert_eq!(titles("j3"), ["Settings…", "-", "Quit j3"]);
        assert_eq!(
            titles("File"),
            [
                "New Thread",
                "Add Project…",
                "-",
                "Add Environment…",
                "Archived Threads"
            ]
        );
        assert_eq!(
            titles("Thread"),
            [
                "Settle",
                "Stop",
                "Rename…",
                "Archive",
                "Delete…",
                "-",
                "Go to Agent…",
                "Next Thread",
                "Previous Thread"
            ]
        );
        assert_eq!(
            titles("View"),
            [
                "Focus Composer",
                "Focus Sidebar",
                "Toggle Terminal",
                "-",
                "Files",
                "-",
                "Zoom In",
                "Zoom Out",
                "Actual Size"
            ]
        );
    }

    #[test]
    fn every_bound_menu_action_has_a_key_equivalent() {
        for (_, entries) in MENUS {
            for entry in *entries {
                if let Item(title, action) = entry {
                    let bound = BINDINGS.iter().any(|b| b.action == *action);
                    assert_eq!(key_equivalent(*action).is_some(), bound, "{title}");
                }
            }
        }
        assert!(
            key_equivalent(Action::ZoomIn).is_some(),
            "mod+= of `mod+=, mod+plus`"
        );
        assert!(key_equivalent(Action::ToggleTerminal).is_some(), "ctrl+`");
        assert!(key_equivalent(Action::AddEnvironment).is_none());
    }

    #[test]
    fn item_ids_are_unique_and_stable() {
        assert_eq!(item_id("File", "Add Project…"), "file-add-project");
        assert_eq!(item_id("j3", "Quit j3"), "j3-quit-j3");
        let mut ids: Vec<String> = MENUS
            .iter()
            .flat_map(|(menu, entries)| {
                entries.iter().filter_map(move |entry| match entry {
                    Item(title, _) => Some(item_id(menu, title)),
                    Separator => None,
                })
            })
            .collect();
        let count = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), count);
        assert_eq!(
            items().len(),
            MENUS.iter().map(|(_, e)| e.len()).sum::<usize>()
        );
    }

    #[test]
    fn a_menu_item_runs_its_action() {
        let mut app = crate::app::testing::TestApp::new();
        app.frame();
        app.rt.update(|s, tx| run(Action::OpenSettings)(s, tx));
        assert!(app.rt.state.route.settings);
        app.rt.update(|s, tx| run(Action::ZoomIn)(s, tx));
        assert_eq!(app.rt.state.prefs.ui().zoom, 1.1);
    }
}
