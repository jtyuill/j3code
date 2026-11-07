//! The thread sidebar (spec §4): its shared shell — thread and project keys, thread
//! commands, navigation order, the clock, the sidebar pane's state — and its view, in the
//! workflow the preferences choose:
//!
//! - **T3** ([`t3`], [`t3_view`]): t3code's settle workflow. One list across environments:
//!   Pinned and Active at the top; the Working, Snoozed and Settled shelves anchored at the
//!   bottom.
//! - **herdr** ([`herdr`]): herdr's agent workflow. Spaces (projects) with their rolled-up
//!   attention, then every agent (thread) with its state.
//!
//! Navigation (`J3::thread_order`, `adjacent`, `J3::nth_thread`) drives ⌘⇧] / ⌘⇧[ / ⌘1…⌘9
//! / ↓↑ over the visible rows; thread mutations go through `J3::apply_thread_commands`.

mod dnd;
mod herdr;
mod menu;
mod t3;
mod t3_view;
#[cfg(test)]
mod tests;

use std::{
    cell::RefCell,
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use j3_client::{ConnectionState, workflow::AgentOrder};
use rune::{Cx, El, Tx};
use rune_kit::PaneState;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub use dnd::DragState;
pub use herdr::{AgentRow, HerdrRows, SpaceRow, display_title, glyph, status_color};
pub use menu::{RowMenu, Submenu};
pub use t3::{SETTLED_INITIAL_COUNT, SETTLED_PAGE_COUNT, T3Row, T3Rows};
pub use t3_view::status_look;

use crate::{
    actions::{Action, KeyContext},
    app::{J3, Ui},
    goto::GotoState,
    ui_settings::{Shelf, Workflow},
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

/// One distinct environment's latest state, for building rows.
pub struct EnvSnapshot<'a> {
    pub key: &'a str,
    pub label: &'a str,
    pub state: &'a ConnectionState,
}

/// The sidebar's own state.
pub struct SidebarState {
    /// The sidebar pane's width and open state (17 rem, 12.5–30 rem).
    pub pane: PaneState,
    /// T3's project filter (`None`: all projects).
    pub project_filter: Option<ProjectKey>,
    /// Settled rows shown (t3's paged tail).
    pub settled_shown: usize,
    /// When time-dependent rows next change (`clock.t` seconds); `None` while nothing on
    /// screen depends on the time.
    pub tick_at: Option<f64>,
    /// What the clock was last armed for (environments, settings, filter, open thread).
    armed_for: Option<u64>,
    /// How many times the clock came due: time-dependent rows change then.
    ticks: u64,
    /// The rows of the current inputs and tick, built once for every reader.
    cache: RefCell<RowsCache>,
    /// The project filter's dropdown is open.
    pub filter_menu: bool,
    /// The open row context menu.
    pub row_menu: Option<RowMenu>,
    /// The row being dragged.
    pub drag: Option<DragState>,
    /// Where each drop target was last drawn, in window points.
    pub drop_rects: HashMap<String, [f32; 4]>,
    /// ⌘0 gave the sidebar the keyboard (until another control takes focus).
    pub keyboard: bool,
    /// The goto picker's search, filter and selection.
    pub goto: GotoState,
    /// Every thread command sent, for tests (fixture connections are offline).
    #[cfg(test)]
    pub sent: Vec<(ThreadKey, ThreadCommand)>,
}

impl Default for SidebarState {
    fn default() -> Self {
        Self {
            pane: PaneState::new(272.0).range(200.0, 480.0),
            project_filter: None,
            settled_shown: SETTLED_INITIAL_COUNT,
            tick_at: None,
            armed_for: None,
            ticks: 0,
            cache: RefCell::default(),
            filter_menu: false,
            row_menu: None,
            drag: None,
            drop_rects: HashMap::new(),
            keyboard: false,
            goto: GotoState::default(),
            #[cfg(test)]
            sent: Vec::new(),
        }
    }
}

impl SidebarState {
    /// The project "new thread" and "add project" target: the filter, if any.
    pub fn project_scope(&self) -> Option<&ProjectKey> {
        self.project_filter.as_ref()
    }
}

/// The rows built for one [`J3::sidebar_rows_key`], each workflow's when first asked for.
#[derive(Default)]
struct RowsCache {
    key: u64,
    t3: Option<Rc<T3Rows>>,
    herdr: Option<Rc<HerdrRows>>,
    goto: Option<Rc<HerdrRows>>,
}

/// The thread after (`delta` > 0) or before `current` in `order`, wrapping; from the ends
/// when nothing (or something not listed) is open.
pub fn adjacent<'k>(
    order: &'k [ThreadKey],
    current: Option<&ThreadKey>,
    delta: isize,
) -> Option<&'k ThreadKey> {
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
    /// The distinct environments' latest states.
    pub fn env_snapshots(&self) -> Vec<EnvSnapshot<'_>> {
        self.envs
            .distinct()
            .into_iter()
            .map(|env| EnvSnapshot {
                key: env.key(),
                label: env.label(),
                state: env.state(),
            })
            .collect()
    }

    /// The cache for the current inputs and clock tick, emptied when either moved.
    fn rows_cache(&self) -> std::cell::RefMut<'_, RowsCache> {
        let key = self.sidebar_rows_key();
        let mut cache = self.sidebar.cache.borrow_mut();
        if cache.key != key {
            *cache = RowsCache {
                key,
                ..RowsCache::default()
            };
        }
        cache
    }

    /// T3 mode's rows now (built once per change of inputs or clock tick).
    pub fn t3_rows(&self) -> Rc<T3Rows> {
        let mut cache = self.rows_cache();
        cache
            .t3
            .get_or_insert_with(|| {
                Rc::new(T3Rows::build(
                    &self.env_snapshots(),
                    self.prefs.ui(),
                    self.sidebar.project_filter.as_ref(),
                    self.open_thread_key().as_ref(),
                    self.sidebar.settled_shown,
                    self.clock.now,
                ))
            })
            .clone()
    }

    /// herdr mode's rows now, in the agent order Settings chose.
    pub fn herdr_rows(&self) -> Rc<HerdrRows> {
        let mut cache = self.rows_cache();
        cache
            .herdr
            .get_or_insert_with(|| {
                let ui = self.prefs.ui();
                Rc::new(HerdrRows::build(
                    &self.env_snapshots(),
                    ui.status_indicators,
                    ui.agent_order,
                ))
            })
            .clone()
    }

    /// Every agent in space order, whatever the workflow and order: the goto picker's list.
    pub fn goto_rows(&self) -> Rc<HerdrRows> {
        let mut cache = self.rows_cache();
        cache
            .goto
            .get_or_insert_with(|| {
                Rc::new(HerdrRows::build(
                    &self.env_snapshots(),
                    self.prefs.ui().status_indicators,
                    AgentOrder::Spaces,
                ))
            })
            .clone()
    }

    /// The visible threads in display order, for keyboard navigation.
    pub fn thread_order(&self) -> Vec<ThreadKey> {
        match self.prefs.ui().workflow {
            Workflow::T3 => self.t3_rows().order(),
            Workflow::Herdr => self.herdr_rows().order(),
        }
    }

    /// The sidebar's `n`th visible thread (1-based).
    pub fn nth_thread(&self, n: usize) -> Option<ThreadKey> {
        let ix = n.checked_sub(1)?;
        match self.prefs.ui().workflow {
            Workflow::T3 => self.t3_rows().rows().nth(ix).map(|row| row.key.clone()),
            Workflow::Herdr => self
                .herdr_rows()
                .agents
                .get(ix)
                .map(|agent| agent.key.clone()),
        }
    }

    /// Gives the sidebar the keyboard: ↑/↓ then move through its threads.
    pub fn focus_sidebar(&mut self, tx: &mut Tx) {
        self.sidebar.pane.set_open(true);
        self.sidebar.keyboard = true;
        tx.blur();
    }

    /// Whether keys arriving with `focused` focused belong to the sidebar: one of its rows
    /// has focus, or ⌘0 gave it the keyboard and nothing has focus since.
    pub fn sidebar_owns_keys(&self, focused: Option<&str>) -> bool {
        match focused {
            Some(key) => self.is_sidebar_row(key),
            None => self.sidebar.keyboard,
        }
    }

    /// Whether element `key` is one of the sidebar's rows (a T3 thread, a herdr space or
    /// agent). Other elements share their prefixes (the header's `thread-actions`).
    fn is_sidebar_row(&self, key: &str) -> bool {
        if self.route.settings {
            return false;
        }
        match self.prefs.ui().workflow {
            Workflow::T3 => {
                key.starts_with("thread-")
                    && self
                        .t3_rows()
                        .rows()
                        .any(|row| row.key.element_id("thread") == key)
            }
            Workflow::Herdr => {
                let rows = self.herdr_rows();
                (key.starts_with("agent-")
                    && rows.agents.iter().any(|a| a.key.element_id("agent") == key))
                    || (key.starts_with("space-")
                        && rows.spaces.iter().any(|s| s.element_id() == key))
            }
        }
    }

    /// A new thread in `project`, or where "new thread" goes (the filter, the open thread's
    /// project, the local environment).
    pub fn new_thread_in(&mut self, project: Option<ProjectKey>, tx: &mut Tx) {
        match project {
            Some(project) => self.new_thread(project.env, Some(project.project_id), tx),
            None => self.dispatch(Action::NewThread, tx),
        }
    }

    /// Shows `filter`'s threads only (`None`: every project's).
    pub fn set_project_filter(&mut self, filter: Option<ProjectKey>) {
        self.sidebar.project_filter = filter;
    }

    /// Folds or unfolds `shelf` (saved in `ui.expandedShelves`); Settled pages from the
    /// start again.
    pub fn toggle_shelf(&mut self, shelf: Shelf, tx: &mut Tx) {
        if shelf == Shelf::Settled {
            self.sidebar.settled_shown = SETTLED_INITIAL_COUNT;
        }
        self.update_ui(|ui| ui.set_expanded(shelf, !ui.is_expanded(shelf)), tx);
    }

    /// The next page of settled rows.
    pub fn show_more_settled(&mut self) {
        self.sidebar.settled_shown += SETTLED_PAGE_COUNT;
    }

    /// Runs thread commands in order on their servers, stopping at the first failure (a later
    /// command can depend on an earlier one, e.g. un-settle before reorder).
    pub fn apply_thread_commands(&mut self, commands: Vec<(ThreadKey, ThreadCommand)>) {
        #[cfg(test)]
        self.sidebar.sent.extend(commands.iter().cloned());
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
                        ThreadCommand::ReorderPinned(key) => {
                            connection.reorder_pinned(id, &key).await
                        }
                        ThreadCommand::ReorderActive(key) => {
                            connection.reorder_active(id, &key).await
                        }
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

    /// What the sidebar's rows depend on besides the time: the environments' states, the
    /// workflow settings, the filter, the paging and the open thread.
    fn sidebar_inputs(&self) -> u64 {
        let mut h = DefaultHasher::new();
        for env in self.envs.distinct() {
            env.key().hash(&mut h);
            (Arc::as_ptr(env.state()) as usize).hash(&mut h);
        }
        let ui = self.prefs.ui();
        (ui.workflow == Workflow::T3, ui.working_shelf).hash(&mut h);
        (ui.status_indicators, ui.agent_order).hash(&mut h);
        ui.expanded_shelves.hash(&mut h);
        self.sidebar.project_filter.hash(&mut h);
        self.sidebar.settled_shown.hash(&mut h);
        self.open_thread_key().hash(&mut h);
        h.finish()
    }

    /// [`Self::sidebar_inputs`] at the clock's current tick: the rows cache's key.
    fn sidebar_rows_key(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.sidebar_inputs().hash(&mut h);
        self.sidebar.ticks.hash(&mut h);
        h.finish()
    }

    /// Whether T3's list is on screen (its menus, drag and drop targets live only then).
    fn t3_shown(&self) -> bool {
        !self.route.settings
            && self.sidebar.pane.is_open()
            && self.prefs.ui().workflow == Workflow::T3
    }

    /// The sidebar's clock: time-dependent rows (working timers every second, ages on the
    /// minute, snoozes when they wake) re-render on time. True when that time came. Also
    /// drops a project filter whose project is gone, menus and drags of a list no longer
    /// shown, the drop targets of rows that left, and the sidebar's keyboard once another
    /// control has focus.
    pub(crate) fn poll_sidebar(&mut self, tx: &mut Tx) -> bool {
        if tx.focused().is_some_and(|key| !self.is_sidebar_row(key)) {
            self.sidebar.keyboard = false;
        }
        let mut changed = false;
        if !self.t3_shown() {
            let open = self.sidebar.row_menu.is_some()
                || self.sidebar.filter_menu
                || self.sidebar.drag.is_some();
            if open {
                self.sidebar.row_menu = None;
                self.sidebar.filter_menu = false;
                self.sidebar.drag = None;
                changed = true;
            }
            self.sidebar.drop_rects.clear();
        }
        let t = self.clock.t;
        let due = self.sidebar.tick_at.is_some_and(|at| t >= at);
        if due {
            self.sidebar.ticks += 1;
        }
        let inputs = self.sidebar_inputs();
        if !due && self.sidebar.armed_for == Some(inputs) {
            return changed;
        }
        self.sidebar.armed_for = Some(inputs);
        if let Some(filter) = &self.sidebar.project_filter {
            let gone = !self.env_snapshots().iter().any(|env| {
                env.key == filter.env
                    && (!env.state.shell.loaded
                        || env.state.shell.project(&filter.project_id).is_some())
            });
            if gone {
                self.sidebar.project_filter = None;
            }
        }
        let wait = match self.prefs.ui().workflow {
            Workflow::T3 => {
                let rows = self.t3_rows();
                // Rows that left the list report where they are again when they return.
                self.sidebar
                    .drop_rects
                    .retain(|target, _| rows.target_slot(target).is_some());
                Some(rows.next_tick(self.clock.now))
            }
            Workflow::Herdr => None,
        };
        self.sidebar.tick_at = wait.map(|wait| t + wait.as_secs_f64());
        if let Some(wait) = wait {
            tx.wake_after(wait);
        }
        changed || due
    }
}

/// The sidebar pane's content in the chosen workflow. ↑/↓ on a focused row move to the
/// previous / next thread, wrapping, as everywhere the sidebar has the keyboard.
pub fn view(s: &J3, cx: &mut Cx<J3>, ui: &mut Ui) -> El {
    let body = match s.prefs.ui().workflow {
        Workflow::T3 => t3_view::view(s, cx, ui),
        Workflow::Herdr => herdr::view(s, ui),
    };
    body.on_key(|s: &mut J3, ev, tx: &mut Tx| {
        match crate::actions::action_for(ev, KeyContext::Sidebar, tx) {
            Some(action @ (Action::NextThread | Action::PreviousThread)) => {
                s.dispatch(action, tx);
                true
            }
            _ => false,
        }
    })
}
