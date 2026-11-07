//! An offline git control for the `git` scene and the control's tests: the open thread's
//! project on a GitHub feature branch with uncommitted changes and no pull request, so the
//! quick action is "Commit, push & PR" and the commit dialog offers every action.

use j3_protocol::t3::git::{
    SourceControlProviderInfo, VcsStatusResult, VcsWorkingTree, VcsWorkingTreeFile,
};

use super::GitControl;
use crate::{app::J3, sidebar::ThreadKey};

fn changed(path: &str, insertions: u64, deletions: u64) -> VcsWorkingTreeFile {
    VcsWorkingTreeFile {
        path: path.to_owned(),
        insertions,
        deletions,
    }
}

/// A feature branch with five changed files, pushed upstream, without a pull request.
pub fn status() -> VcsStatusResult {
    let files = vec![
        changed("crates/j3-desktop/src/files/mod.rs", 412, 38),
        changed("crates/j3-desktop/src/files/tree.rs", 365, 0),
        changed("crates/j3-desktop/src/files/preview.rs", 318, 0),
        changed("crates/j3-desktop/src/git/mod.rs", 296, 22),
        changed("crates/j3-desktop/src/app/fixtures.rs", 14, 3),
    ];
    let insertions = files.iter().map(|file| file.insertions).sum();
    let deletions = files.iter().map(|file| file.deletions).sum();
    VcsStatusResult {
        is_repo: true,
        source_control_provider: Some(SourceControlProviderInfo::github()),
        has_primary_remote: true,
        is_default_ref: false,
        ref_name: Some("rune-desktop-filesgit".to_owned()),
        has_working_tree_changes: true,
        working_tree: VcsWorkingTree {
            files,
            insertions,
            deletions,
        },
        has_upstream: true,
        ahead_count: 0,
        behind_count: 0,
        ..VcsStatusResult::not_a_repo()
    }
}

/// Gives `key`'s control [`status`] for its project, read just now (the next read is due
/// in 30 s); `commit_dialog` opens the commit dialog over it.
pub fn install(s: &mut J3, key: &ThreadKey, commit_dialog: bool) {
    let cwd = s.envs.get(&key.env).and_then(|env| {
        let shell = &env.state().shell;
        let thread = shell.thread(&key.thread_id)?;
        shell
            .project(&thread.project_id)
            .map(|project| project.workspace_root.clone())
    });
    let running = s
        .threads
        .get(key)
        .is_some_and(|thread| thread.state.active_run().is_some());
    s.git.insert(
        key.clone(),
        GitControl {
            cwd,
            status: Some(status()),
            next_poll: Some(s.clock.t + super::POLL_INTERVAL),
            run_active: running,
            ..GitControl::default()
        },
    );
    if commit_dialog {
        s.open_commit_dialog(key);
    }
}
