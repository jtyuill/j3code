//! One open thread (spec §5): its live state — a `ThreadHandle` subscription forwarded into
//! the app by a watch task — and its view: the header (in the shell's toolbar band, see
//! [`header`]), the feed (§5.1, §6), the cards above the composer (§5.2: [`cards`],
//! [`question`], [`goal`]) and the layout with the terminal drawer and file pane ([`layout`],
//! §5.4). The agents hub (§5.3) is `crate::hub`.
//!
//! `J3::open_thread` subscribes (once per thread; the state stays while the app runs, so a
//! reopened thread renders at once), routes the main pane to it and marks it visited.

pub mod cards;
pub mod feed;
pub mod goal;
pub mod header;
pub mod layout;
pub mod question;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use j3_client::{Connection, ThreadHandle, ThreadState};
use rune::{TextEdit, Tx};

pub use header::toolbar;
pub use layout::view;
pub use question::build_answers;

use crate::{
    app::{J3, Main},
    sidebar::ThreadKey,
};

/// A thread's UI state, kept while the app runs.
pub struct ThreadUi {
    /// Keeps the server subscription alive.
    pub handle: ThreadHandle,
    /// The latest state from the subscription.
    pub state: Arc<ThreadState>,
    /// The feed's own UI state.
    pub feed: feed::FeedUi,
    /// The header menu and the cards' own state.
    pub chrome: ChromeUi,
    /// The task forwarding `handle.state()` into the inbox; stopped on drop.
    watch: tokio::task::AbortHandle,
}

impl Drop for ThreadUi {
    fn drop(&mut self) {
        self.watch.abort();
    }
}

/// What the header and the cards above the composer keep for one thread.
pub struct ChromeUi {
    /// The thread actions menu is open.
    pub actions_open: bool,
    /// The plan card is unfolded (it starts so).
    pub todo_open: bool,
    /// A queued message being edited in place.
    pub queue_edit: Option<cards::QueueEdit>,
    /// The first pending question's card.
    pub question: Option<question::QuestionCard>,
    /// The "Set goal" dialog, while open.
    pub goal_dialog: Option<goal::GoalDialog>,
    /// The state the cards were last brought in line with.
    synced: Option<Arc<ThreadState>>,
}

impl Default for ChromeUi {
    fn default() -> Self {
        Self {
            actions_open: false,
            todo_open: true,
            queue_edit: None,
            question: None,
            goal_dialog: None,
            synced: None,
        }
    }
}

impl ChromeUi {
    /// Brings the cards in line with `state`: the question card follows the first pending
    /// request (a new one starts a fresh questionnaire), and an edit of a queued message
    /// that left the queue ends. True when anything changed.
    fn sync(&mut self, state: &Arc<ThreadState>) -> bool {
        if self
            .synced
            .as_ref()
            .is_some_and(|synced| Arc::ptr_eq(synced, state))
        {
            return false;
        }
        self.synced = Some(state.clone());
        let mut changed = false;
        let pending = state.pending_user_inputs();
        let first = pending.first();
        let shown = self.question.as_ref().map(|card| card.request_id.as_str());
        if first.map(|pending| pending.request.id.as_str()) != shown {
            self.question = first.map(question::QuestionCard::new);
            changed = true;
        }
        if self.queue_edit.as_ref().is_some_and(|edit| {
            !state
                .queued_runs()
                .iter()
                .any(|queued| queued.run.id == edit.run_id)
        }) {
            self.queue_edit = None;
            changed = true;
        }
        changed
    }
}

/// The text field `pick` finds in `key`'s chrome, for a `TextInput::on_edit` accessor. When
/// the field closed (or the thread went) since the frame was built, the keystroke lands in
/// the thread's hub entry's sink instead.
pub(crate) fn chrome_edit<'a>(
    s: &'a mut J3,
    key: &ThreadKey,
    pick: impl FnOnce(&mut ChromeUi) -> Option<&mut TextEdit>,
) -> &'a mut TextEdit {
    if let Some(edit) = s
        .threads
        .get_mut(key)
        .and_then(|thread| pick(&mut thread.chrome))
    {
        return edit;
    }
    s.hubs.entry(key.clone()).or_default().edit_sink()
}

impl J3 {
    /// Opens `key`'s thread in the main pane (closing Settings), subscribing to it on first
    /// use and telling its server it was visited.
    pub fn open_thread(&mut self, key: ThreadKey, tx: &mut Tx) {
        self.close_settings(tx);
        if self.open_thread_key().as_ref() == Some(&key) {
            return;
        }
        let Some(connection) = self.subscribe_thread(&key) else {
            return;
        };
        let id = key.thread_id.clone();
        self.services.runtime.spawn(async move {
            if let Err(e) = connection.visit(&id).await {
                tracing::debug!("visit {id}: {e}");
            }
        });
        self.route.main = Main::Thread(key);
    }

    /// Subscribes to `key`'s thread unless it already is (its `ThreadUi` then exists);
    /// returns its environment's connection, None for an unknown environment.
    pub fn subscribe_thread(&mut self, key: &ThreadKey) -> Option<Connection> {
        let connection = self.envs.get(&key.env)?.connection().clone();
        if !self.threads.contains_key(key) {
            let handle = connection.thread(&key.thread_id);
            let mut states = handle.state();
            let state = states.borrow_and_update().clone();
            let inbox = self.inbox.clone();
            let forwarded = key.clone();
            let watch = self.services.runtime.spawn(async move {
                while states.changed().await.is_ok() {
                    let state = states.borrow_and_update().clone();
                    let key = forwarded.clone();
                    inbox.post(move |s, tx| s.thread_changed(&key, state, tx));
                }
            });
            self.threads.insert(
                key.clone(),
                ThreadUi {
                    handle,
                    state,
                    feed: feed::FeedUi::default(),
                    chrome: ChromeUi::default(),
                    watch: watch.abort_handle(),
                },
            );
        }
        Some(connection)
    }

    /// A newer state for `key`'s thread, from its watch-forwarding task.
    pub fn thread_changed(&mut self, key: &ThreadKey, state: Arc<ThreadState>, _: &mut Tx) {
        if let Some(thread) = self.threads.get_mut(key) {
            thread.state = state;
        }
    }

    /// Stops `key`'s active run (⌘.); nothing when no run is active.
    pub fn stop_run(&mut self, key: &ThreadKey) {
        let Some(thread) = self.threads.get(key) else {
            return;
        };
        if thread.state.active_run().is_none() {
            return;
        }
        let Some(connection) = self.thread_connection(key) else {
            return;
        };
        let id = key.thread_id.clone();
        self.run_command(
            "stop the run",
            async move { connection.stop(&id).await },
            |_, _, _| {},
        );
    }

    /// `key`'s environment's connection, None for an unknown environment.
    pub fn thread_connection(&self, key: &ThreadKey) -> Option<Connection> {
        self.envs.get(&key.env).map(|env| env.connection().clone())
    }

    /// Before each frame, for the open thread: brings its cards in line with its state (a
    /// state replaced by any path, not only [`J3::thread_changed`]), confirms a picked
    /// answer whose moment has passed and reads the agents hub's transcript when due. True
    /// when anything shown changed.
    pub(crate) fn poll_thread(&mut self, tx: &mut Tx) -> bool {
        let Some(key) = self.open_thread_key() else {
            return false;
        };
        let mut changed = false;
        if let Some(thread) = self.threads.get_mut(&key) {
            let state = thread.state.clone();
            changed |= thread.chrome.sync(&state);
        }
        changed |= self.poll_question(&key, tx);
        changed |= self.poll_hub(&key, tx);
        changed
    }
}
