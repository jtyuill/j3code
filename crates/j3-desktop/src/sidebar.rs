//! The thread sidebar, in the workflow the preferences choose:
//!
//! - **T3** ([`t3`]): t3code's settle workflow. One list across
//!   environments: Pinned and Active at the top; the Working, Snoozed and
//!   Settled shelves anchored at the bottom.
//! - **herdr** ([`herdr`]): herdr's agent workflow. Spaces (projects) with
//!   their rolled-up attention, then every agent (thread) with its state.
//!
//! The sidebar owns presentation and navigation order; thread commands are
//! requests the workspace carries out.

mod herdr;
mod t3;

use std::{rc::Rc, sync::Arc, time::Duration};

use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription, Task,
    TestSupportExt as _, Window, div,
};
use j3_client::ConnectionState;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    actions::SIDEBAR,
    environment::{Environment, distinct_environments},
    preferences::Preferences,
    ui_settings::Workflow,
};

pub use herdr::{HerdrRows, glyph as herdr_glyph};
pub use t3::T3Rows;

/// One thread, across environments.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ThreadKey {
    pub env: SharedString,
    pub thread_id: String,
}

impl ThreadKey {
    pub fn new(env: impl Into<SharedString>, thread_id: impl Into<String>) -> Self {
        Self {
            env: env.into(),
            thread_id: thread_id.into(),
        }
    }

    /// A stable element id for this thread's control `part`.
    pub fn element_id(&self, part: &str) -> SharedString {
        format!("{part}-{}-{}", self.env, self.thread_id).into()
    }
}

/// A project, across environments.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectKey {
    pub env: SharedString,
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

/// A request from the sidebar; the workspace carries it out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarEvent {
    OpenThread(ThreadKey),
    /// The new-thread composer, preset to a project when one is given.
    NewThread(Option<ProjectKey>),
    /// Add a project (and open a new thread in it).
    AddProject,
    /// Commands to run in order (a drop can touch several threads).
    Commands(Vec<(ThreadKey, ThreadCommand)>),
    /// Ask for a custom wake time, then snooze.
    CustomSnooze(ThreadKey),
    RenameThread(ThreadKey),
    ArchiveThread(ThreadKey),
    DeleteThread(ThreadKey),
}

/// One connected environment's latest state, for building rows.
pub(crate) struct EnvSnapshot {
    pub key: SharedString,
    pub label: SharedString,
    pub state: Arc<ConnectionState>,
}

enum Rows {
    T3(Rc<T3Rows>),
    Herdr(Rc<HerdrRows>),
}

pub struct Sidebar {
    environments: Vec<Entity<Environment>>,
    preferences: Entity<Preferences>,
    /// The open thread.
    selected: Option<ThreadKey>,
    /// T3's project filter (`None`: all projects).
    project_filter: Option<ProjectKey>,
    /// Settled rows shown (t3's paged tail).
    settled_shown: usize,
    rows: Rows,
    /// Visible threads in display order, for keyboard navigation.
    order: Rc<Vec<ThreadKey>>,
    focus_handle: FocusHandle,
    /// Re-renders when time-dependent rows change (a snooze wakes, a
    /// working timer or a relative time ticks).
    _clock: Task<()>,
    _environment_subscriptions: Vec<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SidebarEvent> for Sidebar {}

impl Sidebar {
    pub fn new(
        environments: Vec<Entity<Environment>>,
        preferences: Entity<Preferences>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![cx.observe(&preferences, |this, _, cx| this.rebuild(cx))];
        let mut sidebar = Self {
            environments: Vec::new(),
            preferences,
            selected: None,
            project_filter: None,
            settled_shown: t3::SETTLED_INITIAL_COUNT,
            rows: Rows::T3(Rc::default()),
            order: Rc::default(),
            focus_handle: cx.focus_handle(),
            _clock: Task::ready(()),
            _environment_subscriptions: Vec::new(),
            _subscriptions: subscriptions,
        };
        sidebar.set_environments(environments, cx);
        sidebar
    }

    pub fn set_environments(
        &mut self,
        environments: Vec<Entity<Environment>>,
        cx: &mut Context<Self>,
    ) {
        self._environment_subscriptions = environments
            .iter()
            .map(|env| cx.observe(env, |this, _, cx| this.rebuild(cx)))
            .collect();
        self.environments = environments;
        self.rebuild(cx);
    }

    /// Highlights the open thread without emitting.
    pub fn select_thread(&mut self, key: Option<ThreadKey>, cx: &mut Context<Self>) {
        if self.selected != key {
            self.selected = key;
            self.rebuild(cx);
        }
    }

    /// The project new threads go to from the sidebar's point of view: the
    /// T3 project filter.
    pub fn project_scope(&self) -> Option<&ProjectKey> {
        self.project_filter.as_ref()
    }

    /// Visible threads in display order.
    pub fn order(&self) -> Rc<Vec<ThreadKey>> {
        self.order.clone()
    }

    /// The thread `delta` rows away from the open one, wrapping around.
    pub fn adjacent_thread(&self, delta: isize) -> Option<ThreadKey> {
        adjacent(&self.order, self.selected.as_ref(), delta).cloned()
    }

    /// The Nth visible thread (1-based).
    pub fn nth_thread(&self, n: usize) -> Option<ThreadKey> {
        n.checked_sub(1).and_then(|ix| self.order.get(ix)).cloned()
    }

    /// Every agent in space order, for the goto picker (in either mode).
    pub fn goto_rows(&self, cx: &App) -> HerdrRows {
        let mut ui = self.preferences.read(cx).ui().clone();
        ui.agent_order = j3_client::workflow::AgentOrder::Spaces;
        HerdrRows::build(&self.snapshots(cx), &ui)
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.focus_handle.focus(window, cx);
    }

    /// The distinct environments' latest states.
    fn snapshots(&self, cx: &App) -> Vec<EnvSnapshot> {
        distinct_environments(&self.environments, cx)
            .into_iter()
            .map(|env| {
                let env = env.read(cx);
                EnvSnapshot {
                    key: env.key().clone(),
                    label: env.label().clone(),
                    state: env.state().clone(),
                }
            })
            .collect()
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let now = OffsetDateTime::now_utc();
        let snapshots = self.snapshots(cx);
        let ui = self.preferences.read(cx).ui().clone();
        let next_tick = match ui.workflow {
            Workflow::T3 => {
                if let Some(filter) = &self.project_filter
                    && !snapshots.iter().any(|env| {
                        env.key == filter.env
                            && (!env.state.shell.loaded
                                || env.state.shell.project(&filter.project_id).is_some())
                    })
                {
                    self.project_filter = None;
                }
                let rows = T3Rows::build(
                    &snapshots,
                    &ui,
                    self.project_filter.as_ref(),
                    self.selected.as_ref(),
                    self.settled_shown,
                    now,
                );
                self.order = Rc::new(rows.order());
                let tick = rows.next_tick(now);
                self.rows = Rows::T3(Rc::new(rows));
                tick
            }
            Workflow::Herdr => {
                let rows = HerdrRows::build(&snapshots, &ui);
                self.order = Rc::new(rows.order());
                self.rows = Rows::Herdr(Rc::new(rows));
                None
            }
        };
        self._clock = match next_tick {
            Some(delay) => cx.spawn(async move |this, cx| {
                cx.background_executor().timer(delay).await;
                let _ = this.update(cx, |this, cx| this.rebuild(cx));
            }),
            None => Task::ready(()),
        };
        cx.notify();
    }
}

/// The key `delta` places after `current` in `order`, wrapping; the first
/// (or last) one when nothing is current.
fn adjacent<'a>(
    order: &'a [ThreadKey],
    current: Option<&ThreadKey>,
    delta: isize,
) -> Option<&'a ThreadKey> {
    if order.is_empty() {
        return None;
    }
    let position = current.and_then(|current| order.iter().position(|key| key == current));
    let next = match position {
        Some(pos) => (pos as isize + delta).rem_euclid(order.len() as isize) as usize,
        None if delta >= 0 => 0,
        None => order.len() - 1,
    };
    order.get(next)
}

/// Parses an ISO-8601 timestamp from the wire.
pub(crate) fn parse_time(iso: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(iso, &Rfc3339).ok()
}

/// How long until the next whole minute of `since`'s age, so relative
/// times like `5m` stay current.
pub(crate) fn until_next_minute(now: OffsetDateTime) -> Duration {
    let into_minute = now.second() as u64 * 1_000 + now.millisecond() as u64;
    Duration::from_millis(60_000 - into_minute)
}

impl Focusable for Sidebar {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Sidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The sidebar is the window's left edge: its header clears the
        // traffic lights.
        let inset = crate::titlebar::traffic_lights_inset(window);
        let body = match &self.rows {
            Rows::T3(rows) => self.render_t3(rows.clone(), inset, cx),
            Rows::Herdr(rows) => self.render_herdr(rows.clone(), inset, cx),
        };
        let theme = cx.theme();
        v_flex()
            .id("sidebar")
            .test_support()
            .key_context(SIDEBAR)
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .child(div().flex_1().min_h_0().child(body))
    }
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
