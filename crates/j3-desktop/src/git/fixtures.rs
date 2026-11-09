//! An offline git control for the `git` scene and the control's tests: the open thread's
//! project on a GitHub feature branch with uncommitted changes and no pull request, so the
//! quick action is "Commit, push & PR" and the commit dialog offers every action.

use j3_client::methods::{GitFile, GitStatus};

use super::GitControl;
use crate::{app::J3, sidebar::ThreadKey};

fn changed(path: &str, additions: u64, deletions: u64) -> GitFile {
    GitFile {
        path: path.to_owned(),
        additions,
        deletions,
    }
}

/// A feature branch with five changed files, pushed upstream, without a pull request.
pub fn status() -> GitStatus {
    let files = vec![
        changed("crates/j3-desktop/src/files/mod.rs", 412, 38),
        changed("crates/j3-desktop/src/files/tree.rs", 365, 0),
        changed("crates/j3-desktop/src/files/preview.rs", 318, 0),
        changed("crates/j3-desktop/src/git/mod.rs", 296, 22),
        changed("crates/j3-desktop/src/app/fixtures.rs", 14, 3),
    ];
    let additions = files.iter().map(|file| file.additions).sum();
    let deletions = files.iter().map(|file| file.deletions).sum();
    GitStatus {
        is_repo: true,
        branch: Some("rune-desktop-filesgit".to_owned()),
        on_default_branch: false,
        has_remote: true,
        github: true,
        has_upstream: true,
        ahead: 0,
        behind: 0,
        dirty: true,
        files,
        additions,
        deletions,
        pull_request: None,
    }
}

/// Gives `key`'s control [`status`] for its project, read just now (the next read is due
/// in 30 s); `commit_dialog` opens the commit dialog over it.
pub fn install(s: &mut J3, key: &ThreadKey, commit_dialog: bool) {
    let project_id = s.envs.get(&key.env).and_then(|env| {
        let shell = &env.state().shell;
        let thread = shell.threads.get(&key.thread_id)?;
        shell
            .projects
            .contains_key(&thread.project_id)
            .then(|| thread.project_id.clone())
    });
    let running = s
        .threads
        .get(key)
        .is_some_and(|thread| thread.state.active_turn().is_some());
    s.git.insert(
        key.clone(),
        GitControl {
            project_id,
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
