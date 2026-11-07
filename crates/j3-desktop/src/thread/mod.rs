//! One open thread (spec §5): its live state — a `ThreadHandle` subscription forwarded into
//! the app by a watch task — and its view: the header (in the shell's toolbar band), the feed
//! (§5.1, §6) and the cards above the composer (§5.2).
//!
//! `J3::open_thread` subscribes (once per thread; the state stays while the app runs, so a
//! reopened thread renders at once), routes the main pane to it and marks it visited.

use std::sync::Arc;

use j3_client::{ThreadHandle, ThreadState, ThreadSync, t3::orchestration::TurnItemKind};
use rune::{Cx, El, ScrollSpec, Tx, col, el, row, text};
use rune_kit::shell::Toolbar;

use crate::{
    app::{J3, Main, Ui},
    composer::ComposerKey,
    sidebar::ThreadKey,
};

/// A thread's UI state, kept while the app runs.
pub struct ThreadUi {
    /// Keeps the server subscription alive.
    pub handle: ThreadHandle,
    /// The latest state from the subscription.
    pub state: Arc<ThreadState>,
    /// The task forwarding `handle.state()` into the inbox; stopped on drop.
    watch: tokio::task::AbortHandle,
}

impl Drop for ThreadUi {
    fn drop(&mut self) {
        self.watch.abort();
    }
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
    pub fn subscribe_thread(&mut self, key: &ThreadKey) -> Option<j3_client::Connection> {
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
        let Some(connection) = self.envs.get(&key.env).map(|env| env.connection().clone()) else {
            return;
        };
        let id = key.thread_id.clone();
        self.run_command(
            "stop the run",
            async move { connection.stop(&id).await },
            |_, _, _| {},
        );
    }
}

/// The thread's header in the shell's toolbar band: its title.
pub fn toolbar(s: &J3, cx: &mut Cx<J3>, ui: &mut Ui, key: &ThreadKey) -> Toolbar {
    let title = s
        .threads
        .get(key)
        .and_then(|thread| thread.state.projection.as_ref())
        .map(|projection| projection.thread.title.clone())
        .or_else(|| {
            s.envs
                .get(&key.env)
                .and_then(|env| env.state().shell.thread(&key.thread_id))
                .map(|thread| thread.title.clone())
        })
        .unwrap_or_else(|| "Thread".to_owned());
    Toolbar::new()
        .title(title)
        .trailing(crate::git::view(s, cx, ui, key))
}

/// The open thread: its messages, then the composer.
pub fn view(s: &J3, cx: &mut Cx<J3>, ui: &mut Ui, key: &ThreadKey) -> El {
    let k = ui.k;
    let note = |message: String| {
        el().key("feed-note")
            .w_full()
            .grow(1.0)
            .center()
            .child(text(message).font(k.ty.text_small).color(k.c.text_muted))
    };
    let feed = match s.threads.get(key).map(|thread| &thread.state) {
        None => note("Loading…".into()),
        Some(state) => match &state.sync {
            ThreadSync::Deleted => note("This thread was deleted.".into()),
            ThreadSync::Failed(e) if state.projection.is_none() => {
                note(format!("Couldn’t load the thread: {e}"))
            }
            _ if state.projection.is_none() => note("Loading…".into()),
            _ => messages(state, ui),
        },
    };
    let hub_open = s.hubs.get(key).is_some_and(|hub| hub.open);
    let chat = if hub_open {
        crate::hub::view(s, cx, ui, key).grow(1.0).min_h(0.0)
    } else {
        let composer = crate::composer::view(s, cx, ui, &ComposerKey::Thread(key.clone()));
        col().key("chat").w_full().grow(1.0).min_h(0.0).child(feed).child(
            col()
                .key("thread-bottom")
                .w_full()
                .max_w(ui.m.measure)
                .self_center()
                .px(16.0)
                .pt(16.0)
                .pb(12.0)
                .child(composer),
        )
    };
    let drawer = crate::terminal::view(s, cx, ui, key);
    let files = crate::files::view(s, cx, ui, key);
    row()
        .key("thread")
        .w_full()
        .h_full()
        .min_h(0.0)
        .child(
            col()
                .key("thread-main")
                .grow(1.0)
                .min_w(0.0)
                .h_full()
                .child(chat)
                .child(drawer),
        )
        .child(files)
}

/// User and assistant messages as plain text, in a list that follows its end.
fn messages(state: &ThreadState, ui: &Ui) -> El {
    let k = ui.k;
    let rows: Vec<El> = state
        .feed()
        .iter()
        .filter(|projected| !crate::feed::is_hidden(&projected.item))
        .filter_map(|projected| {
            let item = &projected.item;
            let (text_of, user) = match &item.kind {
                TurnItemKind::UserMessage(message) => (message.text.clone(), true),
                TurnItemKind::AssistantMessage(message) => (message.text.clone(), false),
                _ => return None,
            };
            let body = text(text_of.clone())
                .font(k.ty.body)
                .color(k.c.text_base)
                .wrap()
                .selectable(true)
                .selection_color(k.selection());
            let bubble = if user {
                row().w_full().justify_end().child(
                    el().max_w(ui.m.measure * 0.8)
                        .padding(12.0)
                        .radius(k.radius.lg)
                        .fill(k.c.layer_02)
                        .child(body),
                )
            } else {
                row().w_full().child(body)
            };
            Some(
                el().key(format!("feed-{}", item.id))
                    .w_full()
                    .px(16.0)
                    .pt(24.0)
                    .label(text_of)
                    .child(bubble),
            )
        })
        .collect();
    if rows.is_empty() {
        return el()
            .key("feed")
            .w_full()
            .grow(1.0)
            .center()
            .child(text("No messages yet.").font(k.ty.text_small).color(k.c.text_muted));
    }
    col()
        .key("feed")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .scroll(ScrollSpec { stick_bottom: true })
        .child(
            col()
                .key("feed-rows")
                .w_full()
                .max_w(ui.m.measure)
                .self_center()
                .pb(16.0)
                .children(rows),
        )
}
