//! The open thread's header in the shell's toolbar band (spec §5): its title, then the file
//! pane and terminal toggles, the git control and the thread actions menu (Rename…, Set
//! goal… while there's no goal, Archive, Delete…).

use rune::{Cx, Tx, row, spacer, text};
use rune_kit::{
    ControlSize, IconButton, IconButtonVariant, Menu, MenuItem, Placement, shell::Toolbar,
};

use crate::{
    actions::{Action, shortcut_label},
    app::{J3, Ui},
    icons,
    sidebar::ThreadKey,
};

impl J3 {
    /// Opens or closes `key`'s thread actions menu.
    pub fn set_thread_actions(&mut self, key: &ThreadKey, open: bool) {
        if let Some(thread) = self.threads.get_mut(key) {
            thread.chrome.actions_open = open;
        }
    }
}

/// The thread's title: its live state's, else its sidebar row's, else "Thread".
pub fn title(s: &J3, key: &ThreadKey) -> String {
    s.threads
        .get(key)
        .and_then(|thread| thread.state.thread.as_ref())
        .map(|thread| thread.title.clone())
        .or_else(|| {
            s.envs
                .get(&key.env)
                .and_then(|env| env.state().shell.threads.get(&key.thread_id))
                .map(|thread| thread.title.clone())
        })
        .unwrap_or_else(|| "Thread".to_owned())
}

/// A width no window reaches: the header's row asks for it and shrinks to the band.
const BAND: f32 = 100_000.0;
/// The gaps the kit's band keeps around its (here empty) spacer and trailing slot.
const BAND_GAPS: f32 = 16.0;

/// The header: the title, then Files, Terminal, git and the actions menu at the far end,
/// all in one row that fills the band from the kit's leading slot. Nothing in it moves
/// (`crate::motion`): the git control comes and goes with the repository's status and
/// changes width with its label, on its own time, and the buttons beside it land in their
/// new places with it. The kit's trailing slot would spring its own frame to each new
/// width and carry the controls along, so they aren't in it; this row's frame only follows
/// the window.
pub fn toolbar(s: &J3, cx: &mut Cx<J3>, ui: &mut Ui, key: &ThreadKey) -> Toolbar {
    let k = ui.k;
    let files_open = s.files.get(key).is_some_and(|pane| pane.open);
    let terminal_open = s.terminals.get(key).is_some_and(|drawer| drawer.open);
    let toggle = |glyph, id: &str, label: &str, action: Action, open: bool| {
        IconButton::new(glyph)
            .variant(IconButtonVariant::Ghost)
            .control_size(ControlSize::Small)
            .label(label)
            .help(format!("{label} ({})", shortcut_label(action)))
            .expanded(open)
            .on_press_tx(move |s: &mut J3, tx: &mut Tx| s.dispatch(action, tx))
            .build(k)
            .key(id.to_owned())
    };
    let files = toggle(
        &icons::FOLDER,
        "toggle-files",
        "Files",
        Action::ToggleFiles,
        files_open,
    );
    let terminal = toggle(
        &icons::SQUARE_TERMINAL,
        "toggle-terminal",
        "Terminal",
        Action::ToggleTerminal,
        terminal_open,
    );
    let git = crate::git::view(s, cx, ui, key);
    let actions = actions_menu(s, ui, key);
    Toolbar::new().leading(
        row()
            .key("thread-bar")
            .w(BAND)
            .shrink()
            .min_w(0.0)
            .mr(-BAND_GAPS)
            .center_y()
            .instant_changes()
            .child(
                text(title(s, key))
                    .key("thread-title")
                    .font(k.ty.ui_medium)
                    .color(k.c.text_base)
                    .truncate()
                    .min_w(0.0),
            )
            .child(spacer().min_w(BAND_GAPS))
            .child(
                row()
                    .key("thread-tools")
                    .gap(4.0)
                    .center_y()
                    .child(files)
                    .child(terminal)
                    .child(git)
                    .child(actions),
            ),
    )
}

/// What a thread action menu item runs.
type MenuRun = Box<dyn Fn(&mut J3, &ThreadKey, &mut Tx)>;

/// The Ellipsis button and its menu.
fn actions_menu(s: &J3, ui: &mut Ui, key: &ThreadKey) -> rune::El {
    let k = ui.k;
    let thread = s.threads.get(key);
    let open = thread.is_some_and(|thread| thread.chrome.actions_open);
    let has_goal = thread.is_some_and(|thread| {
        let goal = thread.state.thread.as_ref().and_then(|t| t.goal.as_ref());
        super::goal::current(goal).is_some()
    });
    let item = |label: &str, run: MenuRun| {
        let key = key.clone();
        MenuItem::new(label).on_select_tx(move |s: &mut J3, tx: &mut Tx| {
            s.set_thread_actions(&key, false);
            run(s, &key, tx);
        })
    };
    let dispatch = |action: Action| -> MenuRun {
        Box::new(move |s: &mut J3, _: &ThreadKey, tx: &mut Tx| s.dispatch(action, tx))
    };
    let (toggle_key, close_key) = (key.clone(), key.clone());
    let trigger = IconButton::new(&icons::ELLIPSIS)
        .variant(IconButtonVariant::Ghost)
        .control_size(ControlSize::Small)
        .label("Thread actions")
        .help("Thread actions")
        .on_press(move |s: &mut J3| {
            let open = s
                .threads
                .get(&toggle_key)
                .is_some_and(|thread| thread.chrome.actions_open);
            s.set_thread_actions(&toggle_key, !open);
        })
        .build(k)
        .key("thread-actions");
    let mut menu = Menu::new("thread-actions")
        .trigger(trigger)
        .open(open)
        .placement(Placement::BottomEnd)
        .on_close(move |s: &mut J3| s.set_thread_actions(&close_key, false))
        .item(item("Rename…", dispatch(Action::RenameThread)));
    // One goal at a time: the menu offers a new one only without one.
    if !has_goal {
        menu = menu.item(item(
            "Set goal…",
            Box::new(|s: &mut J3, key: &ThreadKey, _: &mut Tx| s.open_goal_dialog(key)),
        ));
    }
    menu.item(item("Archive", dispatch(Action::ArchiveThread)))
        .separator()
        .item(item("Delete…", dispatch(Action::DeleteThread)))
        .build(k, ui.layer)
}
