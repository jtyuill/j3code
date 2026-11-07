//! The per-thread terminal drawer (spec §5.4, §8.6): tabs of shells on the thread's server
//! under the chat, kept per thread for the session (tabs, scrollback and height survive
//! reopening the thread). ⌃` toggles it; hiding it returns focus to the composer.

use rune::{Cx, El, Tx, col, row, text};

use crate::{
    app::{J3, Ui},
    sidebar::ThreadKey,
};

/// One thread's drawer.
pub struct Drawer {
    pub open: bool,
    /// Terminal ids (`term-N`) in tab order.
    pub tabs: Vec<String>,
    pub active: Option<String>,
    /// The next tab's number.
    pub next: u32,
    /// The drawer's height in points (default 280, 120–2000).
    pub height: f32,
}

impl Default for Drawer {
    fn default() -> Self {
        Self {
            open: false,
            tabs: Vec::new(),
            active: None,
            next: 1,
            height: 280.0,
        }
    }
}

impl Drawer {
    /// Adds a tab `term-N` and makes it active.
    pub fn add_tab(&mut self) -> String {
        let id = format!("term-{}", self.next);
        self.next += 1;
        self.tabs.push(id.clone());
        self.active = Some(id.clone());
        id
    }
}

/// Whether the focused element `key` is a terminal, which takes Tab and Ctrl-letters.
pub fn owns_focus(key: &str) -> bool {
    key.starts_with("terminal-")
}

impl J3 {
    /// Opens `key`'s drawer (with a first terminal) or hides it, refocusing the composer.
    pub fn toggle_terminal(&mut self, key: ThreadKey, tx: &mut Tx) {
        let drawer = self.terminals.entry(key).or_default();
        drawer.open = !drawer.open;
        if drawer.open && drawer.tabs.is_empty() {
            drawer.add_tab();
        }
        if !drawer.open {
            self.focus_composer(tx);
        }
    }
}

/// `key`'s drawer: its tabs, labelled "Terminal N".
pub fn view(s: &J3, _cx: &mut Cx<J3>, ui: &mut Ui, key: &ThreadKey) -> El {
    let k = ui.k;
    let Some(drawer) = s.terminals.get(key).filter(|d| d.open) else {
        return col();
    };
    let mut tabs = row().key("terminal-tabs").w_full().gap(8.0).px(12.0).py(6.0);
    for id in &drawer.tabs {
        let n = id.trim_start_matches("term-");
        let color = if drawer.active.as_deref() == Some(id) { k.c.text_base } else { k.c.text_muted };
        tabs = tabs.child(text(format!("Terminal {n}")).key(format!("terminal-tab-{id}")).font(k.ty.text_small).color(color));
    }
    col()
        .key("terminal-drawer")
        .w_full()
        .h(drawer.height)
        .border_sides([1.0, 0.0, 0.0, 0.0], k.c.border_muted)
        .child(tabs)
}
