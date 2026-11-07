//! Commands and key bindings (spec §9). Each command is one [`Action`]; `J3::dispatch` runs
//! it, so keys, menus and buttons agree. Bindings are `rune::Keybind`s where `mod` is ⌘ on
//! macOS and Ctrl elsewhere (GPUI's `secondary-`); [`shortcut_label`] names an action's keys
//! the platform's way for tooltips (⌘N vs Ctrl+N).
//!
//! `app::on_key` calls [`action_for`] with the key the focused control passed on (or every
//! key when nothing has focus) and the [`KeyContext`] derived from state and focus. To add a
//! binding: add the `Action` variant (if new), a row to [`BINDINGS`], and its arm in
//! `J3::dispatch`.

use std::sync::LazyLock;

use rune::{KeyEvent, KeyPlatform, Keybind, Tx};

use crate::app::{J3, Main};

/// Every command. `JumpToThread(n)` opens the sidebar's nth visible row (1-based).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
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
    /// Stop the open thread's run.
    StopRun,
    /// Rename the open thread.
    RenameThread,
    /// Archive the open thread.
    ArchiveThread,
    /// Delete the open thread.
    DeleteThread,
    /// Settle the open thread and move on to the next unsettled one (or un-settle it if it
    /// is settled).
    SettleThread,
    /// Show archived threads (in Settings).
    ShowArchived,
    /// Open the goto picker over every agent.
    GotoAgent,
    /// Move keyboard focus to the composer.
    FocusComposer,
    /// Move keyboard focus to the sidebar.
    FocusSidebar,
    /// Show or hide the open thread's file pane.
    ToggleFiles,
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
    /// Open the sidebar's Nth thread (1-based), as t3's `thread.jump.N`.
    JumpToThread(u8),
}

/// Where a key arrives, as GPUI's key contexts named it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyContext {
    /// The main window (thread, new thread, empty screen).
    Workspace,
    /// The thread sidebar has focus (workspace bindings apply too).
    Sidebar,
    /// Settings is open.
    Settings,
    /// The goto picker dialog is open.
    GotoPicker,
    /// A terminal has focus: it takes Tab, Ctrl-letters and its own copy/paste keys.
    Terminal,
}

/// One key binding: the keys, the command, and the context it needs (`None`: everywhere).
pub struct Binding {
    pub keys: &'static str,
    pub action: Action,
    pub context: Option<KeyContext>,
}

const fn bind(keys: &'static str, action: Action, context: Option<KeyContext>) -> Binding {
    Binding {
        keys,
        action,
        context,
    }
}

const WS: Option<KeyContext> = Some(KeyContext::Workspace);

/// The binding table (spec §9). Keybind syntax: `mod` is ⌘/Ctrl, alternatives are
/// comma-separated (so a literal comma is `comma`).
pub const BINDINGS: &[Binding] = &[
    bind("mod+q", Action::Quit, None),
    bind("mod+comma", Action::OpenSettings, WS),
    bind("mod+comma", Action::OpenSettings, Some(KeyContext::Settings)),
    bind("escape", Action::CloseSettings, Some(KeyContext::Settings)),
    bind("mod+n", Action::NewThread, WS),
    bind("mod+o", Action::AddProject, WS),
    bind("mod+.", Action::StopRun, WS),
    bind("mod+shift+r", Action::RenameThread, WS),
    bind("mod+shift+a", Action::ArchiveThread, WS),
    bind("mod+shift+backspace", Action::DeleteThread, WS),
    bind("mod+shift+s", Action::SettleThread, WS),
    bind("mod+g", Action::GotoAgent, WS),
    bind("mod+l", Action::FocusComposer, WS),
    bind("mod+0", Action::FocusSidebar, WS),
    bind("mod+shift+e", Action::ToggleFiles, WS),
    bind("mod+shift+]", Action::NextThread, WS),
    bind("mod+shift+[", Action::PreviousThread, WS),
    bind("down", Action::NextThread, Some(KeyContext::Sidebar)),
    bind("up", Action::PreviousThread, Some(KeyContext::Sidebar)),
    // `+` is shift-= on most layouts; both zoom in.
    bind("mod+=, mod+plus", Action::ZoomIn, WS),
    bind("mod+minus", Action::ZoomOut, WS),
    bind(TOGGLE_TERMINAL_KEYS, Action::ToggleTerminal, WS),
    bind("mod+1", Action::JumpToThread(1), WS),
    bind("mod+2", Action::JumpToThread(2), WS),
    bind("mod+3", Action::JumpToThread(3), WS),
    bind("mod+4", Action::JumpToThread(4), WS),
    bind("mod+5", Action::JumpToThread(5), WS),
    bind("mod+6", Action::JumpToThread(6), WS),
    bind("mod+7", Action::JumpToThread(7), WS),
    bind("mod+8", Action::JumpToThread(8), WS),
    bind("mod+9", Action::JumpToThread(9), WS),
];

/// ⌃\` on every platform (a literal Control, also on macOS).
pub const TOGGLE_TERMINAL_KEYS: &str = "ctrl+`";

/// [`BINDINGS`] parsed once.
static KEYBINDS: LazyLock<Vec<(Keybind, Action, Option<KeyContext>)>> = LazyLock::new(|| {
    BINDINGS
        .iter()
        .map(|b| (Keybind::new(b.keys), b.action, b.context))
        .collect()
});

/// Whether a binding for `context` applies where the key arrived. Workspace bindings also
/// apply in the sidebar, which sits in the workspace (as GPUI's context stack did).
fn applies(binding: Option<KeyContext>, at: KeyContext) -> bool {
    match binding {
        None => true,
        Some(KeyContext::Workspace) => matches!(at, KeyContext::Workspace | KeyContext::Sidebar),
        Some(context) => context == at,
    }
}

/// The action `ev` is bound to in `context`, if any. A focused terminal keeps Tab and, off
/// macOS, every Ctrl-letter for the shell (so Ctrl-C is SIGINT, not a shortcut).
pub fn action_for(ev: &KeyEvent, context: KeyContext, tx: &Tx) -> Option<Action> {
    if context == KeyContext::Terminal && terminal_keeps(ev, KeyPlatform::current()) {
        return None;
    }
    let at = if context == KeyContext::Terminal {
        KeyContext::Workspace
    } else {
        context
    };
    KEYBINDS
        .iter()
        .find(|(keys, _, binding)| applies(*binding, at) && keys.matches_tx(ev, tx))
        .map(|(_, action, _)| *action)
}

/// Keys a focused terminal sends to its shell instead of the app.
pub fn terminal_keeps(ev: &KeyEvent, platform: KeyPlatform) -> bool {
    use rune::Key;
    match ev.key {
        Key::Tab => true,
        Key::Command(c) if platform != KeyPlatform::Mac => {
            c.is_ascii_alphabetic() && ev.mods.ctrl && !ev.mods.alt && !ev.mods.shift
        }
        _ => false,
    }
}

/// The first binding's keys for `action` the platform's way (`⌘N`, `Ctrl+N`); "" if unbound.
pub fn shortcut_label(action: Action) -> String {
    shortcut_label_on(action, KeyPlatform::current())
}

/// [`shortcut_label`] for `platform`.
pub fn shortcut_label_on(action: Action, platform: KeyPlatform) -> String {
    KEYBINDS
        .iter()
        .find(|(_, a, _)| *a == action)
        .map(|(keys, _, _)| keys.label_on(platform))
        .unwrap_or_default()
}

impl J3 {
    /// Runs `action`. Commands that need an open thread do nothing without one.
    pub fn dispatch(&mut self, action: Action, tx: &mut Tx) {
        match action {
            Action::Quit => self.quit(tx),
            Action::OpenSettings => {
                if self.route.settings {
                    self.close_settings(tx);
                } else {
                    self.open_settings(crate::settings::Page::General, tx);
                }
            }
            Action::CloseSettings => self.close_settings(tx),
            Action::ShowArchived => self.open_settings(crate::settings::Page::Archived, tx),
            Action::NewThread => {
                let (env, project) = self.target_context();
                self.new_thread(env, project, tx);
            }
            Action::AddProject => {
                let (env, _) = self.target_context();
                self.open_dialog(crate::dialogs::Dialog::add_project(env, true), tx);
            }
            Action::AddEnvironment => {
                self.open_dialog(crate::dialogs::Dialog::add_environment(), tx)
            }
            Action::StopRun => {
                if let Some(key) = self.open_thread_key() {
                    self.stop_run(&key);
                }
            }
            Action::RenameThread => {
                if let Some(key) = self.open_thread_key() {
                    self.open_dialog(crate::dialogs::Dialog::rename_thread(self, key), tx);
                }
            }
            Action::ArchiveThread => {
                if let Some(key) = self.open_thread_key() {
                    self.archive_thread(key);
                }
            }
            Action::DeleteThread => {
                if let Some(key) = self.open_thread_key() {
                    self.open_dialog(crate::dialogs::Dialog::delete_thread(self, key), tx);
                }
            }
            Action::SettleThread => self.settle_open_thread(tx),
            Action::GotoAgent => self.open_goto(tx),
            Action::FocusComposer => self.focus_composer(tx),
            Action::FocusSidebar => {
                self.close_settings(tx);
                self.focus_sidebar(tx);
            }
            Action::ToggleFiles => {
                if let Some(key) = self.open_thread_key() {
                    self.toggle_files(key, tx);
                }
            }
            Action::ToggleTerminal => {
                if let Some(key) = self.open_thread_key() {
                    self.toggle_terminal(key, tx);
                }
            }
            Action::NextThread => self.open_adjacent(1, tx),
            Action::PreviousThread => self.open_adjacent(-1, tx),
            Action::JumpToThread(n) => {
                if let Some(key) = self.nth_thread(n as usize) {
                    self.open_thread(key, tx);
                }
            }
            Action::ZoomIn => self.zoom(1, tx),
            Action::ZoomOut => self.zoom(-1, tx),
            Action::ResetZoom => self.zoom(0, tx),
        }
    }

    /// Where a key arrives: Settings, the goto picker, a focused terminal or the sidebar, else
    /// the workspace.
    pub fn key_context(&self, tx: &Tx) -> KeyContext {
        let focused = tx.focused();
        if self.route.settings {
            KeyContext::Settings
        } else if self.dialogs.is_goto() {
            KeyContext::GotoPicker
        } else if focused.is_some_and(crate::terminal::owns_focus) {
            KeyContext::Terminal
        } else if self.sidebar_owns_keys(focused) {
            KeyContext::Sidebar
        } else {
            KeyContext::Workspace
        }
    }

    /// The thread on screen, if the main pane shows one.
    pub fn open_thread_key(&self) -> Option<crate::sidebar::ThreadKey> {
        match &self.route.main {
            Main::Thread(key) => Some(key.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_follow_the_platform() {
        assert_eq!(shortcut_label_on(Action::NewThread, KeyPlatform::Mac), "⌘N");
        assert_eq!(shortcut_label_on(Action::NewThread, KeyPlatform::Linux), "Ctrl+N");
        assert_eq!(
            shortcut_label_on(Action::SettleThread, KeyPlatform::Mac),
            "⇧⌘S"
        );
        assert_eq!(
            shortcut_label_on(Action::SettleThread, KeyPlatform::Linux),
            "Ctrl+Shift+S"
        );
        assert_eq!(shortcut_label_on(Action::ResetZoom, KeyPlatform::Mac), "");
    }

    #[test]
    fn every_binding_parses() {
        assert_eq!(KEYBINDS.len(), BINDINGS.len());
    }

    #[test]
    fn workspace_bindings_reach_the_sidebar_but_not_settings() {
        assert!(applies(WS, KeyContext::Sidebar));
        assert!(!applies(WS, KeyContext::Settings));
        assert!(applies(None, KeyContext::Settings));
        assert!(!applies(Some(KeyContext::Sidebar), KeyContext::Workspace));
    }

    #[test]
    fn a_terminal_keeps_tab_and_ctrl_letters_off_macos() {
        use rune::{Key, Mods};
        let ctrl_c = KeyEvent {
            key: Key::Command('c'),
            mods: Mods {
                cmd: true,
                ctrl: true,
                ..Mods::default()
            },
        };
        assert!(terminal_keeps(&ctrl_c, KeyPlatform::Linux));
        assert!(!terminal_keeps(&ctrl_c, KeyPlatform::Mac));
        let tab = KeyEvent {
            key: Key::Tab,
            mods: Mods::default(),
        };
        assert!(terminal_keeps(&tab, KeyPlatform::Mac));
    }
}
