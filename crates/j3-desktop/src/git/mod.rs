//! The header's git control (spec §8.7), ported from t3's `GitActionsControl`: the thread
//! project's repository status, a quick action button with a menu (Commit…, Push, Pull, and on
//! GitHub Create PR / View PR), the commit dialog, and outcome toasts. It follows the open
//! thread's project root and rereads the status when the root changes, when the thread's run
//! ends and every 30 s. Hidden unless a status loaded and the project is a repository, or
//! an action is running.
//!
//! An action runs on tokio and outlives the control: leaving the thread does not interrupt a
//! commit; its phases label the button and its outcome is a toast.

mod commit;
pub mod fixtures;
#[cfg(test)]
mod tests;
mod view;

use std::time::Duration;

use j3_client::{
    GitActionUpdate,
    t3::git::{
        GitActionProgress, GitStackedAction, GitToast, GitToastCta, VcsPullResult, VcsPullStatus,
        VcsStatusResult,
    },
};
use rune::{TextEdit, Tx};

use crate::{
    app::{J3, Main},
    sidebar::ThreadKey,
    toasts::{Toast, ToastKind},
};

pub use view::view;

/// Seconds between status reads while the thread is open.
const POLL_INTERVAL: f64 = 30.0;

/// One thread's git control.
#[derive(Default)]
pub struct GitControl {
    /// The thread's project directory, once the shell knows it.
    cwd: Option<String>,
    /// The latest status read of `cwd`; `None` until one succeeds. A failed read keeps it.
    pub status: Option<VcsStatusResult>,
    /// The running action's current phase ("Committing..."); the control is busy while set.
    running: Option<String>,
    /// A status read is in flight.
    refreshing: bool,
    /// A read was asked for while one was in flight: it runs when that one ends.
    refresh_again: bool,
    /// When the next periodic read is due (`clock.t`).
    next_poll: Option<f64>,
    /// The thread had a run in flight at the last poll.
    run_active: bool,
    /// The quick action's menu is open.
    menu_open: bool,
    /// The commit dialog, while open.
    commit: Option<commit::CommitForm>,
    /// The commit dialog's message.
    message: TextEdit,
}

impl GitControl {
    pub fn busy(&self) -> bool {
        self.running.is_some()
    }
}

/// The button's label before the server names the first phase.
fn first_phase(action: GitStackedAction, generates: bool) -> &'static str {
    match (action.commits(), generates) {
        (true, true) => "Generating commit message...",
        (true, false) => "Committing...",
        (false, _) => "Pushing...",
    }
}

/// The toast for a pull's outcome: its title and description.
fn pulled(pull: &VcsPullResult) -> (&'static str, String) {
    match pull.status {
        VcsPullStatus::Pulled => (
            "Pulled",
            format!(
                "Updated {} from {}",
                pull.ref_name,
                pull.upstream_ref.as_deref().unwrap_or("upstream")
            ),
        ),
        VcsPullStatus::SkippedUpToDate => (
            "Already up to date",
            format!("{} is already synchronized.", pull.ref_name),
        ),
    }
}

impl J3 {
    /// Before every frame: follows the open thread's project root and rereads its status on
    /// a new root, at the end of a run and every [`POLL_INTERVAL`] seconds.
    pub(crate) fn poll_git(&mut self, tx: &mut Tx) -> bool {
        let Main::Thread(key) = &self.route.main else {
            return false;
        };
        let Some(env) = self.envs.get(&key.env) else {
            return false;
        };
        let shell = &env.state().shell;
        let root = shell
            .thread(&key.thread_id)
            .and_then(|thread| shell.project(&thread.project_id))
            .map(|project| project.workspace_root.as_str());
        let running = self
            .threads
            .get(key)
            .is_some_and(|thread| thread.state.active_run().is_some());
        if !self.git.contains_key(key) {
            self.git.insert(key.clone(), GitControl::default());
        }
        let Some(git) = self.git.get_mut(key) else {
            return false;
        };
        let t = self.clock.t;
        let mut changed = false;
        let mut refresh = git.next_poll.is_none_or(|at| t >= at);
        if git.cwd.as_deref() != root {
            git.cwd = root.map(str::to_owned);
            git.status = None;
            changed = true;
            refresh = true;
        }
        // A finished run may have changed files.
        refresh |= git.run_active && !running;
        git.run_active = running;
        if refresh {
            git.next_poll = Some(t + POLL_INTERVAL);
        }
        if let Some(at) = git.next_poll {
            tx.wake_after(Duration::from_secs_f64((at - t).max(0.0)));
        }
        if refresh {
            let key = key.clone();
            self.refresh_git(&key);
        }
        changed
    }

    /// Reads `key`'s status again (`vcs.refreshStatus`). A read asked for while one is in
    /// flight runs once that one ends.
    pub fn refresh_git(&mut self, key: &ThreadKey) {
        let Some(connection) = self.envs.get(&key.env).map(|env| env.connection().clone()) else {
            return;
        };
        let t = self.clock.t;
        let Some(git) = self.git.get_mut(key) else {
            return;
        };
        let Some(cwd) = git.cwd.clone() else {
            return;
        };
        if git.refreshing {
            git.refresh_again = true;
            return;
        }
        git.refreshing = true;
        git.next_poll = Some(t + POLL_INTERVAL);
        let key = key.clone();
        self.spawn(async move {
            let result = connection.git_status(&cwd).await;
            move |s: &mut J3, _: &mut Tx| {
                let Some(git) = s.git.get_mut(&key) else {
                    return;
                };
                git.refreshing = false;
                // A failed read keeps the last good status: one missed poll (a busy
                // repository mid-commit, say) must not hide the control.
                match result {
                    Ok(status) if git.cwd.as_deref() == Some(cwd.as_str()) => {
                        git.status = Some(status);
                    }
                    Ok(_) => {}
                    Err(e) => tracing::debug!("git status of {cwd}: {e}"),
                }
                if std::mem::take(&mut git.refresh_again) {
                    s.refresh_git(&key);
                }
            }
        });
    }

    /// Runs a stacked action in `key`'s project; its phases label the button and its outcome
    /// is a toast. `commit_message` `None` lets the server write one.
    pub fn run_git_action(
        &mut self,
        key: &ThreadKey,
        action: GitStackedAction,
        commit_message: Option<String>,
    ) {
        let Some(connection) = self.envs.get(&key.env).map(|env| env.connection().clone()) else {
            return;
        };
        let Some(git) = self.git.get_mut(key) else {
            return;
        };
        let Some(cwd) = git.cwd.clone().filter(|_| !git.busy()) else {
            return;
        };
        let generates = commit_message.is_none()
            && git
                .status
                .as_ref()
                .is_some_and(|status| status.has_working_tree_changes);
        git.running = Some(first_phase(action, generates).to_owned());
        let mut run = connection.run_git_action(&cwd, action, commit_message);
        let (inbox, key) = (self.inbox.clone(), key.clone());
        self.services.runtime.spawn(async move {
            let outcome = loop {
                match run.next().await {
                    Some(GitActionUpdate::Progress(event)) => {
                        if let GitActionProgress::PhaseStarted { label, .. } = event.progress {
                            let key = key.clone();
                            inbox.post(move |s: &mut J3, _: &mut Tx| {
                                if let Some(git) = s.git.get_mut(&key).filter(|git| git.busy()) {
                                    git.running = Some(label);
                                }
                            });
                        }
                    }
                    Some(GitActionUpdate::Finished(outcome)) => break outcome,
                    None => break Err(j3_client::Error::Closed),
                }
            };
            inbox.post(move |s: &mut J3, _: &mut Tx| {
                s.git_finished(&key);
                match outcome {
                    Ok(result) => s.git_toast(&key, result.toast),
                    Err(e) => {
                        s.toasts.error(e.to_string());
                    }
                }
            });
        });
    }

    /// Pulls (`vcs.pull`) and reports the outcome.
    pub fn git_pull(&mut self, key: &ThreadKey) {
        let Some(connection) = self.envs.get(&key.env).map(|env| env.connection().clone()) else {
            return;
        };
        let Some(git) = self.git.get_mut(key) else {
            return;
        };
        let Some(cwd) = git.cwd.clone().filter(|_| !git.busy()) else {
            return;
        };
        git.running = Some("Pulling...".to_owned());
        let key = key.clone();
        self.spawn(async move {
            let outcome = connection.pull(&cwd).await;
            move |s: &mut J3, _: &mut Tx| {
                s.git_finished(&key);
                match outcome {
                    Ok(pull) => {
                        let (title, description) = pulled(&pull);
                        s.toasts
                            .push(Toast::new(ToastKind::Success, title).description(description));
                    }
                    Err(e) => {
                        s.toasts.error(e.to_string());
                    }
                }
            }
        });
    }

    /// Opens the branch's pull request in the browser.
    pub fn open_git_pr(&mut self, key: &ThreadKey, tx: &mut Tx) {
        if let Some(pr) = self
            .git
            .get(key)
            .and_then(|git| git.status.as_ref())
            .and_then(|status| status.pr.as_ref())
            && let Err(e) = tx.open_url(&pr.url)
        {
            self.toasts.error(format!("Couldn’t open {}: {e}", pr.url));
        }
    }

    /// An action ended: the control is free and the status is read again.
    fn git_finished(&mut self, key: &ThreadKey) {
        if let Some(git) = self.git.get_mut(key) {
            git.running = None;
        }
        self.refresh_git(key);
    }

    /// The server's success toast. Its call to action becomes the toast's button: "View PR"
    /// opens the pull request; a follow-up (Push after Commit, Create PR after a push) runs.
    fn git_toast(&mut self, key: &ThreadKey, toast: GitToast) {
        let mut note = Toast::new(ToastKind::Success, toast.title);
        if let Some(description) = toast.description {
            note = note.description(description);
        }
        note = match toast.cta {
            GitToastCta::RunAction { label, action } => {
                let key = key.clone();
                note.action(label, move |s: &mut J3, _: &mut Tx| {
                    s.run_git_action(&key, action.kind, None)
                })
            }
            GitToastCta::OpenPr { label, url } => {
                note.action(label, move |s: &mut J3, tx: &mut Tx| {
                    if let Err(e) = tx.open_url(&url) {
                        s.toasts.error(format!("Couldn’t open {url}: {e}"));
                    }
                })
            }
            GitToastCta::None => note,
        };
        self.toasts.push(note);
    }
}
