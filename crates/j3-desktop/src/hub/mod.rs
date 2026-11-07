//! The agents hub (spec §5.3): the thread's subagent roster and the selected subagent's
//! transcript, replacing the feed and composer while open. ←← in an empty composer opens it;
//! Esc or ←← closes it.

use rune::{Cx, El, Tx, col};

use crate::{
    app::{J3, Ui},
    sidebar::ThreadKey,
};

mod double_tap;
pub use double_tap::{DoubleTap, tap_time};

/// One thread's hub.
#[derive(Default)]
pub struct AgentHub {
    pub open: bool,
    /// The selected subagent's id.
    pub selected: Option<String>,
}

impl J3 {
    /// Opens `key`'s hub when the thread has subagents.
    pub fn open_hub(&mut self, key: ThreadKey, _: &mut Tx) {
        let has_subagents = self
            .threads
            .get(&key)
            .is_some_and(|thread| !thread.state.subagents().is_empty());
        if has_subagents {
            self.hubs.entry(key).or_default().open = true;
        }
    }

    /// Closes `key`'s hub and returns focus to the composer.
    pub fn close_hub(&mut self, key: &ThreadKey, tx: &mut Tx) {
        if let Some(hub) = self.hubs.get_mut(key) {
            hub.open = false;
        }
        self.focus_composer(tx);
    }
}

/// `key`'s hub, when open.
pub fn view(s: &J3, _cx: &mut Cx<J3>, _ui: &mut Ui, key: &ThreadKey) -> El {
    let open = s.hubs.get(key).is_some_and(|hub| hub.open);
    col().key("agent-hub").children(open.then(col))
}
