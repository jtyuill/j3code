//! The header's git control (spec §8.7): the thread project's repository status, a quick
//! action, the git menu and the commit dialog; refreshed on environment change, on run end
//! and every 30 s. Hidden unless a status loaded and the project is a repository.

use j3_protocol::t3::git::VcsStatusResult;
use rune::{Cx, El, row};

use crate::{
    app::{J3, Ui},
    sidebar::ThreadKey,
};

/// One thread's git control.
#[derive(Default)]
pub struct GitControl {
    /// The latest status of the thread's project.
    pub status: Option<VcsStatusResult>,
    /// An action is running.
    pub busy: bool,
}

/// `key`'s git control in the thread header; empty until a repository status is known.
pub fn view(s: &J3, _cx: &mut Cx<J3>, _ui: &mut Ui, key: &ThreadKey) -> El {
    let shown = s
        .git
        .get(key)
        .and_then(|git| git.status.as_ref())
        .is_some_and(|status| status.is_repo);
    row().key("git-control").children(shown.then(row))
}
