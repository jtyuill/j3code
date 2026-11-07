//! Workspace commands (spec §3, §4.1, §8.8): which environment and project "new thread" and
//! "add project" target, thread navigation, settle-and-advance, archive and delete,
//! attention alerts, and leaving whatever showed a removed environment.

use j3_client::{
    t3::orchestration::SettledOverride,
    workflow::{AgentNotification, effective_snoozed, forward_target, has_unseen_completion},
};
use rune::Tx;

use super::{J3, Main};
use crate::{
    environment::LOCAL_KEY,
    sidebar::{ThreadCommand, ThreadKey},
    toasts::ToastKind,
};

impl J3 {
    /// Environment and project for "new thread"/"add project": the sidebar's project filter,
    /// else the open thread's, else the local environment.
    pub fn target_context(&self) -> (String, Option<String>) {
        if let Some(scope) = self.sidebar.project_scope()
            && self.envs.get(&scope.env).is_some()
        {
            return (scope.env.clone(), Some(scope.project_id.clone()));
        }
        if let Some(key) = self.open_thread_key()
            && let Some(env) = self.envs.get(&key.env)
        {
            let project = env
                .state()
                .shell
                .thread(&key.thread_id)
                .map(|thread| thread.project_id.clone());
            return (key.env, project);
        }
        let env = self
            .envs
            .iter()
            .next()
            .map_or(LOCAL_KEY.to_owned(), |env| env.key().to_owned());
        (env, None)
    }

    /// Opens the sidebar's next (`delta` > 0) or previous thread, wrapping around.
    pub fn open_adjacent(&mut self, delta: isize, tx: &mut Tx) {
        let order = self.thread_order();
        let open = self.open_thread_key();
        if let Some(key) = crate::sidebar::adjacent(&order, open.as_ref(), delta).cloned() {
            self.open_thread(key, tx);
        }
    }

    /// t3's settle-and-advance (⌘⇧S): un-settles a settled open thread; otherwise settles it
    /// and opens the next row that is not settled or snoozed, else a new thread in its project.
    pub fn settle_open_thread(&mut self, tx: &mut Tx) {
        let Some(key) = self.open_thread_key() else {
            return;
        };
        let Some(thread) = self
            .envs
            .get(&key.env)
            .and_then(|env| env.state().shell.thread(&key.thread_id).cloned())
        else {
            return;
        };
        if thread.settled_override == Some(SettledOverride::Settled) {
            self.apply_thread_commands(vec![(key, ThreadCommand::Unsettle)]);
            return;
        }
        let now = self.clock.now;
        let order = self.thread_order();
        let parked = |candidate: &ThreadKey| {
            candidate == &key
                || self
                    .envs
                    .get(&candidate.env)
                    .and_then(|env| {
                        env.state().shell.thread(&candidate.thread_id).map(|thread| {
                            thread.settled_override == Some(SettledOverride::Settled)
                                || effective_snoozed(thread, now)
                        })
                    })
                    .unwrap_or(true)
        };
        let next = forward_target(&order, &key, parked).cloned();
        let env = key.env.clone();
        self.apply_thread_commands(vec![(key, ThreadCommand::Settle)]);
        match next {
            Some(next) => self.open_thread(next, tx),
            None => self.new_thread(env, Some(thread.project_id), tx),
        }
    }

    /// The title of `key`'s thread, or "this thread".
    pub fn thread_title(&self, key: &ThreadKey) -> String {
        self.envs
            .get(&key.env)
            .and_then(|env| env.state().shell.thread(&key.thread_id))
            .map(|thread| thread.title.clone())
            .unwrap_or_else(|| "this thread".to_owned())
    }

    /// Archives `key`'s thread and leaves it if it is open.
    pub fn archive_thread(&mut self, key: ThreadKey) {
        let Some(connection) = self.envs.get(&key.env).map(|env| env.connection().clone()) else {
            return;
        };
        let id = key.thread_id.clone();
        self.run_command(
            "archive the thread",
            async move { connection.archive(&id).await },
            move |s, _, _| s.leave_thread(&key),
        );
    }

    /// Deletes `key`'s thread (after the dialog confirmed) and leaves it if it is open.
    pub fn delete_thread(&mut self, key: ThreadKey) {
        let Some(connection) = self.envs.get(&key.env).map(|env| env.connection().clone()) else {
            return;
        };
        let id = key.thread_id.clone();
        self.run_command(
            "delete the thread",
            async move { connection.delete_thread(&id).await },
            move |s, _, _| s.leave_thread(&key),
        );
    }

    /// Renames `key`'s thread; an empty title is refused.
    pub fn rename_thread(&mut self, key: &ThreadKey, title: &str) -> bool {
        let title = title.trim().to_owned();
        if title.is_empty() {
            return false;
        }
        let Some(connection) = self.envs.get(&key.env).map(|env| env.connection().clone()) else {
            return false;
        };
        let id = key.thread_id.clone();
        self.run_command(
            "rename the thread",
            async move { connection.rename(&id, &title).await },
            |_, _, _| {},
        );
        true
    }

    /// After a thread leaves the list (archived or deleted), show nothing in its place and
    /// drop what was kept for it.
    pub fn leave_thread(&mut self, key: &ThreadKey) {
        if self.open_thread_key().as_ref() == Some(key) {
            self.route.main = Main::Empty;
        }
        self.forget_thread(key);
    }

    /// Environment `key` was removed: nothing of it stays on screen.
    pub(crate) fn forget_environment(&mut self, key: &str, _: &mut Tx) {
        let showing = match &self.route.main {
            Main::Thread(open) => open.env == key,
            Main::NewThread => self.new_thread.env == key,
            Main::Empty => false,
        };
        if showing {
            self.route.main = Main::Empty;
        }
        self.attention.forget(key);
        self.threads.retain(|thread, _| thread.env != key);
        self.terminals.retain(|thread, _| thread.env != key);
        self.files.retain(|thread, _| thread.env != key);
        self.git.retain(|thread, _| thread.env != key);
        self.hubs.retain(|thread, _| thread.env != key);
    }

    /// Raises attention toasts and, while the window is in the background, sounds for
    /// environment `key`'s status changes, and acknowledges a completion of the open thread
    /// (it was watched).
    pub(crate) fn observe_attention(&mut self, key: &str, _: &mut Tx) {
        let Some(env) = self.envs.get(key) else {
            return;
        };
        let state = env.state().clone();
        let connection = env.connection().clone();
        let open = self.open_thread_key().filter(|open| open.env == key);
        // The open thread is only watched while the window has focus: a run that finishes
        // behind another app alerts like any other.
        let focused = self.window_active;
        let watched = open
            .as_ref()
            .filter(|_| focused)
            .map(|open| open.thread_id.as_str());
        let alerts = self.attention.observe(key, &state.shell, watched);
        let ui = self.prefs.ui().clone();
        for alert in alerts {
            if ui.sounds && !focused {
                alert.sound().play(&self.services.runtime);
            }
            // The open thread's own run ending (heard in the background) needs no toast: it
            // is on screen.
            let on_screen = alert.kind == AgentNotification::Finished
                && open
                    .as_ref()
                    .is_some_and(|open| open.thread_id == alert.thread_id);
            if ui.toasts && !on_screen {
                let kind = match alert.kind {
                    AgentNotification::NeedsAttention => ToastKind::Warning,
                    AgentNotification::Finished => ToastKind::Success,
                };
                let thread = ThreadKey::new(alert.env.clone(), alert.thread_id.clone());
                self.toasts.push_thread(kind, alert.message(), thread);
            }
        }
        if let Some(open) = open
            && let Some(thread) = state.shell.thread(&open.thread_id)
            && has_unseen_completion(thread)
            && let Some(Some(completed)) = &thread.latest_run_completed_at
        {
            let mark = (open.clone(), completed.clone());
            if self.visited_completion.as_ref() != Some(&mark) {
                self.visited_completion = Some(mark);
                self.services.runtime.spawn(async move {
                    if let Err(e) = connection.visit(&open.thread_id).await {
                        tracing::debug!("visit {}: {e}", open.thread_id);
                    }
                });
            }
        }
    }
}
