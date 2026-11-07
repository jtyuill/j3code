//! The thread sidebar (spec §4): its shared shell — thread and project keys, thread
//! commands, navigation order, the sidebar pane's state — and its view. The T3 and herdr
//! workflows (§4.2, §4.3) render their rows here.
//!
//! Navigation (`J3::thread_order`, `adjacent`, `J3::nth_thread`) drives ⌘⇧] / ⌘⇧[ / ⌘1…⌘9
//! / ↓↑; thread mutations go through `J3::apply_thread_commands`.

use std::time::Duration;

use j3_client::workflow::is_listed;
use rune::{Cx, El, Tx, col, text};
use rune_kit::{
    IconButton, IconButtonVariant, PaneState,
    shell::{Sidebar, SidebarRow},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    actions::{Action, shortcut_label},
    app::{J3, Main, Ui},
    icons,
};

/// One thread, across environments.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ThreadKey {
    pub env: String,
    pub thread_id: String,
}

impl ThreadKey {
    pub fn new(env: impl Into<String>, thread_id: impl Into<String>) -> Self {
        Self {
            env: env.into(),
            thread_id: thread_id.into(),
        }
    }

    /// A stable element key for this thread's control `part`: `{part}-{env}-{thread_id}`.
    pub fn element_id(&self, part: &str) -> String {
        format!("{part}-{}-{}", self.env, self.thread_id)
    }
}

/// A project, across environments.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectKey {
    pub env: String,
    pub project_id: String,
}

/// One thread mutation (t3's sidebar commands).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadCommand {
    Settle,
    Unsettle,
    Snooze(OffsetDateTime),
    Wake,
    Pin(Option<String>),
    Unpin,
    ReorderPinned(String),
    ReorderActive(String),
    MarkUnread,
    AutoSettle(bool),
}

/// The sidebar's own state.
pub struct SidebarState {
    /// The sidebar pane's width and open state (17 rem, 12.5–30 rem).
    pub pane: PaneState,
    /// T3's project filter (`None`: all projects).
    pub project_filter: Option<ProjectKey>,
    /// When time-dependent rows next change (`clock.t` seconds); `None` until the first poll.
    pub tick_at: Option<f64>,
}

impl Default for SidebarState {
    fn default() -> Self {
        Self {
            pane: PaneState::new(272.0).range(200.0, 480.0),
            project_filter: None,
            tick_at: None,
        }
    }
}

impl SidebarState {
    /// The project "new thread" and "add project" target: the filter, if any.
    pub fn project_scope(&self) -> Option<&ProjectKey> {
        self.project_filter.as_ref()
    }
}

/// Whether the focused element `key` belongs to the sidebar (its rows and list), so ↑/↓ move
/// through threads there.
pub fn owns_focus(key: &str) -> bool {
    key.starts_with("thread-") || key.starts_with("sidebar")
}

/// The thread after (`delta` > 0) or before `current` in `order`, wrapping; from the ends
/// when nothing (or something not listed) is open.
pub fn adjacent<'k>(order: &'k [ThreadKey], current: Option<&ThreadKey>, delta: isize) -> Option<&'k ThreadKey> {
    if order.is_empty() {
        return None;
    }
    let next = match current.and_then(|key| order.iter().position(|k| k == key)) {
        Some(pos) => (pos as isize + delta).rem_euclid(order.len() as isize) as usize,
        None if delta >= 0 => 0,
        None => order.len() - 1,
    };
    order.get(next)
}

/// Parses an ISO-8601 timestamp from the wire.
pub fn parse_time(iso: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(iso, &Rfc3339).ok()
}

/// How long until the next whole minute of `now`, so relative times like `5m` stay current.
pub fn until_next_minute(now: OffsetDateTime) -> Duration {
    let into_minute = now.second() as u64 * 1_000 + now.millisecond() as u64;
    Duration::from_millis(60_000 - into_minute)
}

impl J3 {
    /// The visible threads in display order, for keyboard navigation: every listed thread
    /// of the distinct environments (within the project filter), newest first.
    pub fn thread_order(&self) -> Vec<ThreadKey> {
        let filter = self.sidebar.project_filter.as_ref();
        let mut rows: Vec<(&str, ThreadKey)> = self
            .envs
            .distinct()
            .into_iter()
            .flat_map(|env| {
                let key = env.key();
                env.state()
                    .shell
                    .threads
                    .iter()
                    .filter(|thread| is_listed(thread))
                    .filter(move |thread| {
                        filter.is_none_or(|f| f.env == key && f.project_id == thread.project_id)
                    })
                    .map(move |thread| (thread.updated_at.as_str(), ThreadKey::new(key, &thread.id)))
            })
            .collect();
        rows.sort_by(|a, b| b.0.cmp(a.0));
        rows.into_iter().map(|(_, key)| key).collect()
    }

    /// The sidebar's `n`th visible thread (1-based).
    pub fn nth_thread(&self, n: usize) -> Option<ThreadKey> {
        n.checked_sub(1).and_then(|ix| self.thread_order().into_iter().nth(ix))
    }

    /// Moves keyboard focus to the sidebar's list.
    pub fn focus_sidebar(&mut self, _: &mut Tx) {
        self.sidebar.pane.set_open(true);
    }

    /// Runs thread commands in order on their servers, stopping at the first failure (a later
    /// command can depend on an earlier one, e.g. un-settle before reorder).
    pub fn apply_thread_commands(&mut self, commands: Vec<(ThreadKey, ThreadCommand)>) {
        let commands: Vec<_> = commands
            .into_iter()
            .filter_map(|(key, command)| {
                let connection = self.envs.get(&key.env)?.connection().clone();
                Some((connection, key.thread_id, command))
            })
            .collect();
        if commands.is_empty() {
            return;
        }
        self.run_command(
            "update the thread",
            async move {
                for (connection, id, command) in commands {
                    let id = id.as_str();
                    match command {
                        ThreadCommand::Settle => connection.settle(id).await,
                        ThreadCommand::Unsettle => connection.unsettle(id).await,
                        ThreadCommand::Snooze(until) => connection.snooze(id, until).await,
                        ThreadCommand::Wake => connection.unsnooze(id).await,
                        ThreadCommand::Pin(key) => connection.pin(id, key.as_deref()).await,
                        ThreadCommand::Unpin => connection.unpin(id).await,
                        ThreadCommand::ReorderPinned(key) => connection.reorder_pinned(id, &key).await,
                        ThreadCommand::ReorderActive(key) => connection.reorder_active(id, &key).await,
                        ThreadCommand::MarkUnread => connection.mark_unread(id).await,
                        ThreadCommand::AutoSettle(enabled) => {
                            connection.set_auto_settle(id, enabled).await
                        }
                    }?;
                }
                Ok(())
            },
            |_, _, _| {},
        );
    }

    /// The sidebar's clock: time-dependent rows (ages, snooze wakes) re-render on the
    /// minute. True when the minute turned.
    pub(crate) fn poll_sidebar(&mut self, tx: &mut Tx) -> bool {
        let t = self.clock.t;
        let due = self.sidebar.tick_at.is_some_and(|at| t >= at);
        if due || self.sidebar.tick_at.is_none() {
            let wait = until_next_minute(self.clock.now);
            self.sidebar.tick_at = Some(t + wait.as_secs_f64());
            tx.wake_after(wait);
        }
        due
    }
}

/// The sidebar pane's content.
pub fn view(s: &J3, _cx: &mut Cx<J3>, ui: &mut Ui) -> El {
    let k = ui.k;
    let open = match &s.route.main {
        Main::Thread(key) => Some(key),
        _ => None,
    };
    let new_thread = IconButton::new(&icons::SQUARE_PEN)
        .variant(IconButtonVariant::GhostMuted)
        .label("New thread")
        .help(format!("New thread ({})", shortcut_label(Action::NewThread)))
        .on_press_tx(|s: &mut J3, tx: &mut Tx| s.dispatch(Action::NewThread, tx))
        .build(k)
        .key("new-thread");
    let mut sidebar = Sidebar::new().label("Threads").header(new_thread);
    let order = s.thread_order();
    if order.is_empty() {
        let connecting = s.envs.iter().all(|env| !env.state().shell.loaded);
        let note = if connecting { "Connecting…" } else { "No threads yet" };
        sidebar = sidebar.child(
            col()
                .key("sidebar-empty")
                .w_full()
                .py(12.0)
                .child(text(note).font(k.ty.text_small).color(k.c.text_muted)),
        );
    }
    for key in order {
        let title = s
            .envs
            .get(&key.env)
            .and_then(|env| env.state().shell.thread(&key.thread_id))
            .map_or_else(String::new, |thread| thread.title.clone());
        let row_key = key.element_id("thread");
        let selected = open == Some(&key);
        sidebar = sidebar.child(
            SidebarRow::new(title)
                .selected(selected)
                .on_select(move |s: &mut J3, tx: &mut Tx| s.open_thread(key.clone(), tx))
                .build(k)
                .key(row_key),
        );
    }
    sidebar.build(k)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjacent_wraps_and_starts_at_the_ends() {
        let order: Vec<ThreadKey> = ["a", "b", "c"]
            .into_iter()
            .map(|id| ThreadKey::new("local", id))
            .collect();
        let key = |id: &str| ThreadKey::new("local", id);
        assert_eq!(adjacent(&order, Some(&key("a")), 1), Some(&key("b")));
        assert_eq!(adjacent(&order, Some(&key("c")), 1), Some(&key("a")));
        assert_eq!(adjacent(&order, Some(&key("a")), -1), Some(&key("c")));
        assert_eq!(adjacent(&order, None, 1), Some(&key("a")));
        assert_eq!(adjacent(&order, None, -1), Some(&key("c")));
        assert_eq!(adjacent(&order, Some(&key("x")), 1), Some(&key("a")));
        assert_eq!(adjacent(&[], None, 1), None);
    }

    #[test]
    fn element_ids_are_namespaced_per_environment() {
        assert_ne!(
            ThreadKey::new("local", "t1").element_id("thread"),
            ThreadKey::new("http://x:1", "t1").element_id("thread")
        );
    }

    #[test]
    fn the_clock_ticks_on_the_minute() {
        let now = OffsetDateTime::parse("2026-11-01T12:00:45.250Z", &Rfc3339).unwrap();
        assert_eq!(until_next_minute(now), Duration::from_millis(14_750));
    }
}
