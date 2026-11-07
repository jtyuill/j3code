//! The project file pane (spec §5.4, §8.5): the thread's project tree, loaded a directory at
//! a time from its server, beside the chat, with a read-only preview. ⌘⇧E toggles it.

use rune::{Cx, El, Tx, col, row, text};

use crate::{
    app::{J3, Ui},
    icons,
    sidebar::ThreadKey,
};

/// One thread's file pane.
pub struct FilePane {
    pub open: bool,
    /// The pane's width in points (22.5 rem by default, 15–60 rem).
    pub width: f32,
}

impl Default for FilePane {
    fn default() -> Self {
        Self { open: false, width: 360.0 }
    }
}

impl J3 {
    /// Shows or hides `key`'s file pane; hiding refocuses the composer.
    pub fn toggle_files(&mut self, key: ThreadKey, tx: &mut Tx) {
        let pane = self.files.entry(key).or_default();
        pane.open = !pane.open;
        if !pane.open {
            self.focus_composer(tx);
        }
    }
}

/// `key`'s file pane: its header with the project's title.
pub fn view(s: &J3, _cx: &mut Cx<J3>, ui: &mut Ui, key: &ThreadKey) -> El {
    let k = ui.k;
    let Some(pane) = s.files.get(key).filter(|p| p.open) else {
        return col();
    };
    let title = s
        .envs
        .get(&key.env)
        .and_then(|env| {
            let shell = &env.state().shell;
            let project = &shell.thread(&key.thread_id)?.project_id;
            shell.project(project).map(|p| p.title.clone())
        })
        .unwrap_or_default();
    col()
        .key("file-pane")
        .w(pane.width)
        .h_full()
        .border_sides([0.0, 0.0, 0.0, 1.0], k.c.border_muted)
        .child(
            row()
                .key("file-pane-header")
                .w_full()
                .gap(8.0)
                .padding(12.0)
                .center_y()
                .child(rune::icon(&icons::FOLDER_TREE, 16.0).color(k.c.icon_muted))
                .child(text(title).font(k.ty.text_small_medium).color(k.c.text_base).truncate()),
        )
}
