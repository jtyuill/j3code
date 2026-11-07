//! One open thread (spec §5): its live state — a `ThreadHandle` subscription forwarded into
//! the app by a watch task — and its view: the header (in the shell's toolbar band, see
//! [`header`]), the feed (§5.1, §6), the cards above the composer (§5.2: [`cards`],
//! [`question`], [`goal`]) and the layout with the terminal drawer and file pane ([`layout`],
//! §5.4). The agents hub (§5.3) is `crate::hub`.
//!
//! `J3::open_thread` subscribes, routes the main pane to the thread and marks it visited.
//! Only the open thread stays subscribed: when another opens (or the route leaves it) its
//! subscription ends but its last state, feed and cards stay cached for the
//! [`RECENT_THREADS`] most recently open threads, so reopening one draws at once while the
//! new subscription catches up. An archived or deleted thread is forgotten.

pub mod cards;
pub mod feed;
pub mod goal;
pub mod header;
pub mod layout;
pub mod question;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use j3_client::{Connection, ThreadHandle, ThreadState, ThreadSync};
use rune::{TextEdit, Tx};

pub use header::toolbar;
pub use layout::view;
pub use question::build_answers;

use crate::{
    app::{J3, Main},
    sidebar::ThreadKey,
};

/// Threads kept cached (state, feed, cards) besides the open one.
pub const RECENT_THREADS: usize = 8;

/// A thread's UI state: live while it is open, cached for a while after.
pub struct ThreadUi {
    /// The latest state: live from the subscription while open, the last one seen after.
    pub state: Arc<ThreadState>,
    /// The feed's own UI state.
    pub feed: feed::FeedUi,
    /// The header menu and the cards' own state.
    pub chrome: ChromeUi,
    /// The server subscription, while the thread is open.
    live: Option<Subscription>,
    /// When the thread was last open (clock seconds), for evicting the least recent.
    seen: f64,
}

impl ThreadUi {
    /// Whether the server streams this thread's changes (it is the open one).
    pub fn is_subscribed(&self) -> bool {
        self.live.is_some()
    }
}

/// A `ThreadHandle` and the task forwarding its state into the inbox; ends on drop.
struct Subscription {
    /// Keeps the server subscription alive.
    _handle: ThreadHandle,
    watch: tokio::task::AbortHandle,
}

impl Drop for Subscription {
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
/// `key`'s hub's sink instead; a hub entry left without its thread is dropped by the next
/// poll ([`J3::retire_threads`]).
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

    /// Subscribes to `key`'s thread unless it already is; a cached thread keeps its state
    /// until the new subscription has some. Returns its environment's connection, None for
    /// an unknown environment.
    pub fn subscribe_thread(&mut self, key: &ThreadKey) -> Option<Connection> {
        let connection = self.envs.get(&key.env)?.connection().clone();
        if self.threads.get(key).is_some_and(ThreadUi::is_subscribed) {
            return Some(connection);
        }
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
        let live = Subscription {
            _handle: handle,
            watch: watch.abort_handle(),
        };
        let seen = self.clock.t;
        match self.threads.get_mut(key) {
            Some(thread) => {
                thread.live = Some(live);
                thread.seen = seen;
                if !is_blank(&state) {
                    thread.state = state;
                }
            }
            None => {
                self.threads.insert(
                    key.clone(),
                    ThreadUi {
                        state,
                        feed: feed::FeedUi::default(),
                        chrome: ChromeUi::default(),
                        live: Some(live),
                        seen,
                    },
                );
            }
        }
        Some(connection)
    }

    /// A newer state for `key`'s thread, from its watch-forwarding task. A resubscription's
    /// first, empty state doesn't replace the cached one.
    pub fn thread_changed(&mut self, key: &ThreadKey, state: Arc<ThreadState>, _: &mut Tx) {
        if let Some(thread) = self.threads.get_mut(key)
            && !(is_blank(&state) && thread.state.projection.is_some())
        {
            thread.state = state;
        }
    }

    /// Drops everything kept for `key`'s thread (it was archived or deleted).
    pub fn forget_thread(&mut self, key: &ThreadKey) {
        self.threads.remove(key);
        self.hubs.remove(key);
    }

    /// Only the open thread stays subscribed (it is subscribed here when the route reached
    /// it some other way than `open_thread`); of the others the [`RECENT_THREADS`] most
    /// recently open stay cached and the rest are forgotten, with any hub left without its
    /// thread.
    fn retire_threads(&mut self, open: Option<&ThreadKey>) {
        if let Some(open) = open
            && !self.threads.get(open).is_some_and(ThreadUi::is_subscribed)
        {
            self.subscribe_thread(open);
        }
        let settled = self
            .threads
            .iter()
            .all(|(key, thread)| !thread.is_subscribed() || Some(key) == open)
            && self.threads.len() <= RECENT_THREADS + 1
            && self.hubs.keys().all(|key| self.threads.contains_key(key));
        if settled {
            return;
        }
        for (key, thread) in &mut self.threads {
            if Some(key) != open {
                thread.live = None;
            }
        }
        let mut cached: Vec<(f64, ThreadKey)> = self
            .threads
            .iter()
            .filter(|(key, _)| Some(*key) != open)
            .map(|(key, thread)| (thread.seen, key.clone()))
            .collect();
        if cached.len() > RECENT_THREADS {
            cached.sort_by(|a, b| b.0.total_cmp(&a.0));
            for (_, key) in cached.drain(RECENT_THREADS..) {
                self.threads.remove(&key);
            }
        }
        let threads = &self.threads;
        self.hubs.retain(|key, _| threads.contains_key(key));
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
        let open = self.open_thread_key();
        self.retire_threads(open.as_ref());
        let Some(key) = open else {
            return false;
        };
        let mut changed = false;
        let now = self.clock.t;
        if let Some(thread) = self.threads.get_mut(&key) {
            thread.seen = now;
            let state = thread.state.clone();
            changed |= thread.chrome.sync(&state);
        }
        changed |= self.poll_question(&key, tx);
        changed |= self.poll_hub(&key, tx);
        changed
    }
}

/// A subscription's first state: nothing received yet.
fn is_blank(state: &ThreadState) -> bool {
    state.projection.is_none() && state.sync == ThreadSync::Loading
}
