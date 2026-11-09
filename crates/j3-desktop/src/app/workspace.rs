//! Workspace commands (spec §3, §4.1, §8.8): which environment and project "new thread" and
//! "add project" target, thread navigation, settle-and-advance, archive and delete,
//! attention alerts, and leaving whatever showed a removed environment.

use j3_client::{
    entities::Settled,
    herdr::AgentNotification,
    inbox::{can_settle, completed_at, forward_target, is_snoozed, is_unread},
    methods::{DeleteThreadParams, RenameThreadParams, ThreadParams, VisitParams},
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
                .threads
                .get(&key.thread_id)
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

    /// Settle-and-advance (⌘⇧S, `docs/triage.md` §6.3): un-settles a settled open thread;
    /// otherwise, where the server accepts a settle, settles it and opens the next row that is
    /// not settled or snoozed, else a new thread in its project.
    pub fn settle_open_thread(&mut self, tx: &mut Tx) {
        let Some(key) = self.open_thread_key() else {
            return;
        };
        let Some(thread) = self
            .envs
            .get(&key.env)
            .and_then(|env| env.state().shell.threads.get(&key.thread_id).cloned())
        else {
            return;
        };
        if thread.triage.settled == Some(Settled::Settled) {
            self.apply_thread_commands(vec![(key, ThreadCommand::Unsettle)]);
            return;
        }
        if !can_settle(&thread) {
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
                        env.state()
                            .shell
                            .threads
                            .get(&candidate.thread_id)
                            .map(|thread| {
                                thread.triage.settled == Some(Settled::Settled)
                                    || is_snoozed(thread, now)
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
            .and_then(|env| env.state().shell.threads.get(&key.thread_id))
            .map(|thread| thread.title.clone())
            .unwrap_or_else(|| "this thread".to_owned())
    }

    /// Archives `key`'s thread and leaves it if it is open.
    pub fn archive_thread(&mut self, key: ThreadKey) {
        let Some(connection) = self.envs.get(&key.env).map(|env| env.connection().clone()) else {
            return;
        };
        let thread_id = key.thread_id.clone();
        self.run_command(
            "archive the thread",
            async move {
                let params = ThreadParams {
                    thread_id,
                    command_id: None,
                };
                connection.threads_archive(params).await
            },
            move |s, _, _| s.leave_thread(&key),
        );
    }

    /// Deletes `key`'s thread (after the dialog confirmed) and leaves it if it is open.
    pub fn delete_thread(&mut self, key: ThreadKey) {
        let Some(connection) = self.envs.get(&key.env).map(|env| env.connection().clone()) else {
            return;
        };
        let thread_id = key.thread_id.clone();
        self.run_command(
            "delete the thread",
            async move {
                let params = DeleteThreadParams {
                    thread_id,
                    only_if_unused: None,
                    command_id: None,
                };
                connection.threads_delete(params).await
            },
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
        let thread_id = key.thread_id.clone();
        self.run_command(
            "rename the thread",
            async move {
                let params = RenameThreadParams {
                    thread_id,
                    title,
                    command_id: None,
                };
                connection.threads_rename(params).await
            },
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
    pub(crate) fn forget_environment(&mut self, key: &str, tx: &mut Tx) {
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
        self.prune_composers(tx);
    }

    /// Raises attention toasts and, while the window is in the background, sounds for
    /// environment `key`'s status changes, and visits the open thread once per newly finished
    /// turn while the window has focus (`docs/triage.md` §12.1(4)).
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
        if focused && let Some(open) = open {
            self.visit_finished_turn(open, &state.shell, connection);
        }
    }

    /// Visits the open thread when its latest turn finished since it was last looked at here
    /// and left the thread unread. Each finished turn is remembered (keyed by turn id and end
    /// time), so marking the open thread unread sticks: only a newly finished turn visits.
    fn visit_finished_turn(
        &mut self,
        open: ThreadKey,
        shell: &j3_client::Shell,
        connection: j3_client::Connection,
    ) {
        let Some(thread) = shell.threads.get(&open.thread_id) else {
            return;
        };
        let Some(turn) = thread.latest_turn.as_ref() else {
            return;
        };
        let (Some(completed), Some(ended_at)) = (completed_at(thread), turn.ended_at.as_deref())
        else {
            return;
        };
        let mark = (open.clone(), format!("{}@{ended_at}", turn.id));
        if self.visited_completion.as_ref() == Some(&mark) {
            return;
        }
        self.visited_completion = Some(mark);
        if !is_unread(thread) {
            return;
        }
        // Never before the turn's end, whatever the clocks say: the visit must read it.
        let params = VisitParams {
            thread_id: open.thread_id.clone(),
            visited_at: crate::sidebar::iso(self.clock.now.max(completed)),
            opened: None,
            command_id: None,
        };
        self.services.runtime.spawn(async move {
            if let Err(e) = connection.triage_visit(params).await {
                tracing::debug!("visit {}: {e}", open.thread_id);
            }
        });
    }
}
