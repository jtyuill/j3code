//! T3 mode: t3code's web sidebar (`apps/web/src/components/Sidebar.tsx`,
//! `sidebar/SidebarThreadRow.tsx`). One list across environments: Pinned
//! and Active at the top; the Working, Snoozed and Settled shelves anchored
//! at the bottom, count-labelled and collapsible; Settled paged.

use std::{collections::HashMap, rc::Rc, time::Duration};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, Theme,
    button::{Button, ButtonVariants as _},
    h_flex,
    list::ListItem,
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, WeakEntity, Window, div,
};
use j3_client::{
    t3::orchestration::{SettledOverride, ThreadShell},
    workflow::{
        DropInput, DropPlan, EnvThread, ListItem as Item, ListMarker, T3Section, ThreadStatus,
        TimeZone, can_snooze, drop_target, format_working_duration, has_unseen_completion,
        pick_snooze_preset, plan_drop, resolve_snooze_presets, settled_timestamp, should_recede,
        snooze_wake_label, t3_layout, thread_status, woke_unseen_at, working_started_at,
    },
};
use time::OffsetDateTime;

use super::{
    EnvSnapshot, ProjectKey, Rows, Sidebar, SidebarEvent, ThreadCommand, ThreadKey,
    herdr::display_title, parse_time, until_next_minute,
};
use crate::{
    actions::AddProject,
    feed::age_since,
    ui_settings::{Shelf, UiSettings},
};

/// Settled rows shown at first (`SETTLED_TAIL_INITIAL_COUNT`,
/// Sidebar.tsx:328).
pub const SETTLED_INITIAL_COUNT: usize = 10;
/// Rows each "Show N more" adds (`SETTLED_TAIL_PAGE_COUNT`, Sidebar.tsx:329).
pub const SETTLED_PAGE_COUNT: usize = 25;

/// One thread row.
pub struct T3Row {
    pub key: ThreadKey,
    pub section: T3Section,
    pub title: SharedString,
    pub project: SharedString,
    /// The environment, when more than one is connected.
    pub env_label: Option<SharedString>,
    pub status: ThreadStatus,
    pub unread: bool,
    pub woke: bool,
    pub recede: bool,
    pub working_since: Option<OffsetDateTime>,
    /// The row's time: when it settled for settled rows, else its latest
    /// user message (or update).
    pub time: Option<OffsetDateTime>,
    pub snoozed_until: Option<OffsetDateTime>,
    pub pinned: bool,
    pub settled: bool,
    pub snoozable: bool,
    pub auto_settle: bool,
    pub has_completion: bool,
}

impl T3Row {
    /// The status word the row shows, if any (also its accessible state).
    fn badge(&self) -> Option<&'static str> {
        if self.section == T3Section::Snoozed {
            return Some("Snoozed");
        }
        if self.woke {
            return Some("Woke");
        }
        if let Some(label) = self.status.label(self.unread) {
            return Some(label);
        }
        (self.section == T3Section::Settled).then_some("Settled")
    }

    fn accessibility_label(&self) -> String {
        match self.badge() {
            Some(badge) => format!("{}, {badge}", self.title),
            None => self.title.to_string(),
        }
    }
}

/// A row being dragged.
#[derive(Clone)]
pub struct DraggedThread {
    key: ThreadKey,
    title: SharedString,
}

/// What follows the pointer while a row is dragged.
struct DragPreview(SharedString);

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_2()
            .py_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.drag_border)
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .text_sm()
            .shadow_md()
            .child(self.0.clone())
    }
}

#[derive(Default)]
pub struct T3Rows {
    pub pinned: Vec<T3Row>,
    pub active: Vec<T3Row>,
    pub working: Vec<T3Row>,
    pub snoozed: Vec<T3Row>,
    pub settled: Vec<T3Row>,
    pub working_count: usize,
    pub snoozed_count: usize,
    pub settled_count: usize,
    pub expanded: HashMap<Shelf, bool>,
    pub active_time_ordered: bool,
    pub next_wake_at: Option<OffsetDateTime>,
    /// The rendered list (rows and markers) for drop targets.
    pub items: Vec<Item<ThreadKey>>,
    pub pinned_keys: HashMap<ThreadKey, String>,
    pub active_keys: HashMap<ThreadKey, String>,
    /// The filter menu: every project, with its label.
    pub projects: Vec<(ProjectKey, SharedString)>,
    pub filter_label: SharedString,
    pub filter: Option<ProjectKey>,
    pub has_projects: bool,
    pub note: Option<SharedString>,
}

fn flat<T>(value: &Option<Option<T>>) -> Option<&T> {
    value.as_ref().and_then(Option::as_ref)
}

impl T3Rows {
    pub fn build(
        snapshots: &[EnvSnapshot],
        ui: &UiSettings,
        filter: Option<&ProjectKey>,
        selected: Option<&ThreadKey>,
        settled_shown: usize,
        now: OffsetDateTime,
    ) -> Self {
        let connected: Vec<&str> = snapshots
            .iter()
            .filter(|env| env.state.status.is_connected())
            .map(|env| env.key.as_ref())
            .collect();
        let multi_env = snapshots.len() > 1 && connected.len() > 1;
        let mut projects = Vec::new();
        let mut project_titles: HashMap<(&str, &str), SharedString> = HashMap::new();
        let mut pinned_keys = HashMap::new();
        let mut active_keys = HashMap::new();
        let mut threads = Vec::new();
        for env in snapshots {
            let shell = &env.state.shell;
            for project in &shell.projects {
                let title: SharedString = project.title.clone().into();
                project_titles.insert((env.key.as_ref(), project.id.as_str()), title.clone());
                let label = if multi_env {
                    format!("{} · {}", project.title, env.label).into()
                } else {
                    title
                };
                projects.push((
                    ProjectKey {
                        env: env.key.clone(),
                        project_id: project.id.clone(),
                    },
                    label,
                ));
            }
            for thread in &shell.threads {
                let key = ThreadKey::new(env.key.clone(), thread.id.clone());
                // Hidden rows' keys stay reserved when rearranging.
                if let Some(order) = flat(&thread.pin_order_key) {
                    pinned_keys.insert(key.clone(), order.clone());
                }
                if let Some(order) = flat(&thread.active_order_key) {
                    active_keys.insert(key, order.clone());
                }
                let in_filter = filter.is_none_or(|filter| {
                    filter.env == env.key && filter.project_id == thread.project_id
                });
                if in_filter {
                    threads.push(EnvThread {
                        environment: env.key.as_ref(),
                        thread,
                    });
                }
            }
        }
        let layout = t3_layout(threads, now, ui.working_shelf, |env| {
            connected.contains(&env)
        });
        let labels: HashMap<&str, &SharedString> = snapshots
            .iter()
            .map(|env| (env.key.as_ref(), &env.label))
            .collect();
        let row = |entry: &EnvThread, section: T3Section| -> T3Row {
            let thread: &ThreadShell = entry.thread;
            let key = ThreadKey::new(
                SharedString::from(entry.environment.to_owned()),
                thread.id.clone(),
            );
            let status = thread_status(thread);
            let unread = has_unseen_completion(thread);
            let woke = woke_unseen_at(thread, now).is_some();
            let is_selected = selected == Some(&key);
            let settled = thread.settled_override == Some(SettledOverride::Settled);
            T3Row {
                section,
                title: display_title(&thread.title),
                project: project_titles
                    .get(&(entry.environment, thread.project_id.as_str()))
                    .cloned()
                    .unwrap_or_default(),
                env_label: multi_env
                    .then(|| labels.get(entry.environment).map(|label| (*label).clone()))
                    .flatten(),
                status,
                unread,
                woke,
                recede: should_recede(status, unread, woke, false, is_selected),
                working_since: matches!(status, ThreadStatus::Working | ThreadStatus::Waiting)
                    .then(|| working_started_at(thread))
                    .flatten(),
                time: if section == T3Section::Settled {
                    settled_timestamp(thread)
                } else {
                    thread
                        .latest_user_message_at
                        .as_deref()
                        .and_then(parse_time)
                        .or_else(|| parse_time(&thread.updated_at))
                },
                snoozed_until: flat(&thread.snoozed_until).and_then(|until| parse_time(until)),
                pinned: flat(&thread.pinned_at).is_some(),
                settled,
                snoozable: can_snooze(thread, now),
                auto_settle: flat(&thread.auto_settle_disabled_at).is_none(),
                has_completion: flat(&thread.latest_run_completed_at).is_some(),
                key,
            }
        };
        let rows = |entries: &[EnvThread], section: T3Section| -> Vec<T3Row> {
            entries.iter().map(|entry| row(entry, section)).collect()
        };
        // A collapsed shelf still shows the open thread (threadListV2.ts:738-798).
        let shelf = |entries: &[EnvThread], section: T3Section, shelf: Shelf| -> Vec<T3Row> {
            if ui.is_expanded(shelf) {
                rows(entries, section)
            } else {
                entries
                    .iter()
                    .filter(|entry| {
                        selected.is_some_and(|selected| {
                            selected.env == entry.environment
                                && selected.thread_id == entry.thread.id
                        })
                    })
                    .map(|entry| row(entry, section))
                    .collect()
            }
        };
        let settled_rows: Vec<T3Row> = if ui.is_expanded(Shelf::Settled) {
            layout
                .settled
                .iter()
                .take(settled_shown)
                .map(|entry| row(entry, T3Section::Settled))
                .collect()
        } else {
            shelf(&layout.settled, T3Section::Settled, Shelf::Settled)
        };

        let mut rows = Self {
            pinned: rows(&layout.pinned, T3Section::Pinned),
            active: rows(&layout.active, T3Section::Active),
            working: shelf(&layout.working, T3Section::Working, Shelf::Working),
            snoozed: shelf(&layout.snoozed, T3Section::Snoozed, Shelf::Snoozed),
            settled: settled_rows,
            working_count: layout.working.len(),
            snoozed_count: layout.snoozed.len(),
            settled_count: layout.settled.len(),
            expanded: [Shelf::Working, Shelf::Snoozed, Shelf::Settled]
                .into_iter()
                .map(|shelf| (shelf, ui.is_expanded(shelf)))
                .collect(),
            active_time_ordered: layout.active_time_ordered,
            next_wake_at: layout.next_wake_at,
            items: Vec::new(),
            pinned_keys,
            active_keys,
            filter_label: filter
                .and_then(|filter| {
                    projects
                        .iter()
                        .find(|(key, _)| key == filter)
                        .map(|(_, label)| label.clone())
                })
                .unwrap_or_else(|| "All projects".into()),
            filter: filter.cloned(),
            has_projects: !projects.is_empty(),
            projects,
            note: (!snapshots.iter().any(|env| env.state.shell.loaded))
                .then(|| "Connecting…".into()),
        };
        rows.items = rows.list_items();
        rows
    }

    /// The rendered list as t3 orders it (Sidebar.tsx:1944-1962).
    fn list_items(&self) -> Vec<Item<ThreadKey>> {
        let thread = |row: &T3Row| Item::Thread {
            key: row.key.clone(),
            section: row.section,
        };
        let mut items = vec![Item::Marker(ListMarker::PinnedHeader)];
        items.extend(self.pinned.iter().map(thread));
        items.push(Item::Marker(ListMarker::PinnedDivider));
        items.push(Item::Marker(ListMarker::ActivePlaceholder));
        items.extend(self.active.iter().map(thread));
        if self.working_count > 0 {
            items.push(Item::Marker(ListMarker::WorkingHeader));
            items.extend(self.working.iter().map(thread));
        }
        if self.snoozed_count > 0 {
            items.push(Item::Marker(ListMarker::SnoozedHeader));
            items.extend(self.snoozed.iter().map(thread));
        }
        items.push(Item::Marker(ListMarker::SettledHeader));
        items.push(Item::Marker(ListMarker::SettledPlaceholder));
        items.extend(self.settled.iter().map(thread));
        items
    }

    fn rows(&self) -> impl Iterator<Item = &T3Row> {
        self.pinned
            .iter()
            .chain(&self.active)
            .chain(&self.working)
            .chain(&self.snoozed)
            .chain(&self.settled)
    }

    fn row(&self, key: &ThreadKey) -> Option<&T3Row> {
        self.rows().find(|row| &row.key == key)
    }

    fn marker_index(&self, marker: ListMarker) -> Option<usize> {
        self.items
            .iter()
            .position(|item| matches!(item, Item::Marker(m) if *m == marker))
    }

    fn thread_index(&self, key: &ThreadKey) -> Option<usize> {
        self.items
            .iter()
            .position(|item| matches!(item, Item::Thread { key: k, .. } if k == key))
    }

    /// Visible rows in display order.
    pub fn order(&self) -> Vec<ThreadKey> {
        self.rows().map(|row| row.key.clone()).collect()
    }

    fn order_of(rows: &[T3Row]) -> Vec<ThreadKey> {
        rows.iter().map(|row| row.key.clone()).collect()
    }

    /// When the list next changes on its own: the next snooze wake, the
    /// working timers (every second) or the relative times (each minute).
    pub fn next_tick(&self, now: OffsetDateTime) -> Option<Duration> {
        let timers = self.rows().any(|row| row.working_since.is_some());
        let tick = if timers {
            Duration::from_secs(1)
        } else {
            until_next_minute(now)
        };
        let wake = self.next_wake_at.map(|wake| {
            // Just past the wake, so the row has moved when the list rebuilds.
            Duration::try_from(wake - now).unwrap_or_default() + Duration::from_millis(50)
        });
        Some(wake.map_or(tick, |wake| wake.min(tick)))
    }

    /// The commands a drop of `drag` onto list slot `over` makes.
    fn plan(&self, drag: &DraggedThread, over: usize) -> Vec<(ThreadKey, ThreadCommand)> {
        let Some(row) = self.row(&drag.key) else {
            return Vec::new();
        };
        let Some(moved) = self.thread_index(&drag.key) else {
            return Vec::new();
        };
        let Some(target) = drop_target(&self.items, moved, over) else {
            return Vec::new();
        };
        let pinned_order = Self::order_of(&self.pinned);
        let active_order = Self::order_of(&self.active);
        let plan = plan_drop(DropInput {
            moved: &drag.key,
            from: row.section,
            from_pinned: row.pinned,
            from_settled: row.settled,
            supports_settlement: true,
            target: &target,
            pinned_order: &pinned_order,
            pinned_keys: &self.pinned_keys,
            pinned_writable: None,
            active_order: &active_order,
            active_keys: &self.active_keys,
            active_writable: None,
            active_time_ordered: self.active_time_ordered,
        });
        let moved = drag.key.clone();
        match plan {
            DropPlan::None => Vec::new(),
            DropPlan::Settle => vec![(moved, ThreadCommand::Settle)],
            DropPlan::ReorderPinned { assignments, .. } => assignments
                .into_iter()
                .map(|a| (a.id, ThreadCommand::ReorderPinned(a.order_key)))
                .collect(),
            DropPlan::Pin {
                order_key,
                extra_assignments,
                ..
            } => std::iter::once((moved, ThreadCommand::Pin(order_key)))
                .chain(
                    extra_assignments
                        .into_iter()
                        .map(|a| (a.id, ThreadCommand::ReorderPinned(a.order_key))),
                )
                .collect(),
            DropPlan::MoveActive {
                assignments,
                unpin,
                unsettle,
                unsnooze,
                ..
            } => [
                unpin.then_some(ThreadCommand::Unpin),
                unsettle.then_some(ThreadCommand::Unsettle),
                unsnooze.then_some(ThreadCommand::Wake),
            ]
            .into_iter()
            .flatten()
            .map(|command| (moved.clone(), command))
            .chain(
                assignments
                    .into_iter()
                    .map(|a| (a.id, ThreadCommand::ReorderActive(a.order_key))),
            )
            .collect(),
        }
    }
}

fn emit(weak: &WeakEntity<Sidebar>, event: SidebarEvent, cx: &mut App) {
    let _ = weak.update(cx, |_, cx| cx.emit(event));
}

/// The pill color of a status (t3's `SidebarThreadRow` classes, mapped to
/// theme roles).
fn status_color(status: ThreadStatus, theme: &Theme) -> Hsla {
    match status {
        ThreadStatus::Working => theme.info,
        ThreadStatus::Waiting => theme.muted_foreground,
        ThreadStatus::Approval | ThreadStatus::Input | ThreadStatus::Request => theme.warning,
        ThreadStatus::Limited => theme.warning,
        ThreadStatus::Failed => theme.danger,
        ThreadStatus::Ready => theme.success,
    }
}

fn status_icon(status: ThreadStatus) -> Icon {
    match status {
        ThreadStatus::Working => Icon::new(Lucide::CircleDashed),
        ThreadStatus::Waiting => Icon::new(Lucide::CircleDotDashed),
        ThreadStatus::Approval | ThreadStatus::Input | ThreadStatus::Request => {
            Icon::new(Lucide::MessageCircleQuestionMark)
        }
        ThreadStatus::Failed | ThreadStatus::Limited => Icon::new(IconName::CircleAlert),
        ThreadStatus::Ready => Icon::new(IconName::CircleCheck),
    }
}

impl Sidebar {
    fn toggle_shelf(&mut self, shelf: Shelf, cx: &mut Context<Self>) {
        if shelf == Shelf::Settled {
            self.settled_shown = SETTLED_INITIAL_COUNT;
        }
        self.preferences.update(cx, |preferences, cx| {
            preferences.update_ui(|ui| ui.set_expanded(shelf, !ui.is_expanded(shelf)), cx)
        });
    }

    fn show_more_settled(&mut self, cx: &mut Context<Self>) {
        self.settled_shown += SETTLED_PAGE_COUNT;
        self.rebuild(cx);
    }

    fn set_project_filter(&mut self, filter: Option<ProjectKey>, cx: &mut Context<Self>) {
        self.project_filter = filter;
        self.rebuild(cx);
    }

    fn drop_thread(&mut self, drag: &DraggedThread, over: usize, cx: &mut Context<Self>) {
        let Rows::T3(rows) = &self.rows else {
            return;
        };
        let commands = rows.plan(drag, over);
        if !commands.is_empty() {
            cx.emit(SidebarEvent::Commands(commands));
        }
    }

    pub(super) fn render_t3(&self, rows: Rc<T3Rows>, cx: &Context<Self>) -> AnyElement {
        let now = OffsetDateTime::now_utc();
        let dragging = cx.has_active_drag();
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let weak = cx.weak_entity();

        let header = self.render_t3_header(&rows, cx);
        let row = |row: &T3Row, over: Option<usize>| self.render_row(row, over, now, &weak, cx);
        let index = |key: &ThreadKey| rows.thread_index(key);

        let mut top = v_flex().id("t3-top").gap_0p5();
        // The Pinned boundary shows only while dragging (t3's drag-only
        // `SidebarDragBoundary`), and takes drops that pin.
        if dragging {
            top = top.child(self.render_boundary(
                "Pinned",
                "drop-pinned",
                rows.marker_index(ListMarker::PinnedHeader),
                cx,
            ));
        }
        let pinned: Vec<AnyElement> = rows.pinned.iter().map(|r| row(r, index(&r.key))).collect();
        top = top.child(
            v_flex()
                .id("t3-pinned")
                .test_support()
                .gap_0p5()
                .children(pinned),
        );
        if dragging {
            top = top.child(self.render_boundary(
                "Active",
                "drop-active",
                rows.marker_index(ListMarker::PinnedDivider),
                cx,
            ));
        } else if !rows.pinned.is_empty() {
            top = top.child(div().mx_2().my_1().h_px().bg(theme.sidebar_border));
        }
        let active: Vec<AnyElement> = rows
            .active
            .iter()
            .map(|r| {
                let over = if rows.active_time_ordered {
                    rows.marker_index(ListMarker::ActivePlaceholder)
                } else {
                    index(&r.key)
                };
                row(r, over)
            })
            .collect();
        top = top.child(
            v_flex()
                .id("t3-active")
                .test_support()
                .gap_0p5()
                .children(active),
        );

        let mut shelves = v_flex().id("t3-shelves").gap_0p5();
        if rows.working_count > 0 {
            shelves = shelves.child(self.render_shelf_header(
                Shelf::Working,
                "Working",
                rows.working_count,
                rows.expanded[&Shelf::Working],
                None,
                cx,
            ));
            let working: Vec<AnyElement> = rows.working.iter().map(|r| row(r, None)).collect();
            shelves = shelves.child(
                v_flex()
                    .id("t3-working")
                    .test_support()
                    .gap_0p5()
                    .children(working),
            );
        }
        if rows.snoozed_count > 0 {
            shelves = shelves.child(self.render_shelf_header(
                Shelf::Snoozed,
                "Snoozed",
                rows.snoozed_count,
                rows.expanded[&Shelf::Snoozed],
                None,
                cx,
            ));
            let snoozed: Vec<AnyElement> = rows.snoozed.iter().map(|r| row(r, None)).collect();
            shelves = shelves.child(
                v_flex()
                    .id("t3-snoozed")
                    .test_support()
                    .gap_0p5()
                    .children(snoozed),
            );
        }
        // Like t3's `settled-header`, the Settled shelf is always there (a
        // drop target even while empty).
        if rows.has_projects {
            shelves = shelves.child(self.render_shelf_header(
                Shelf::Settled,
                "Settled",
                rows.settled_count,
                rows.expanded[&Shelf::Settled],
                rows.marker_index(ListMarker::SettledHeader),
                cx,
            ));
            let settled_over = rows.marker_index(ListMarker::SettledPlaceholder);
            let settled: Vec<AnyElement> =
                rows.settled.iter().map(|r| row(r, settled_over)).collect();
            let hidden = if rows.expanded[&Shelf::Settled] {
                rows.settled_count.saturating_sub(rows.settled.len())
            } else {
                0
            };
            shelves = shelves.child(
                v_flex()
                    .id("t3-settled")
                    .test_support()
                    .gap_0p5()
                    .children(settled)
                    .when(hidden > 0, |this| {
                        let more = hidden.min(SETTLED_PAGE_COUNT);
                        this.child(
                            ListItem::new("settled-show-more")
                                .accessibility_label(format!("Show {more} more"))
                                .px_2()
                                .py_1()
                                .rounded(theme.radius)
                                .text_sm()
                                .text_color(muted)
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .child(Icon::new(IconName::ChevronDown).small())
                                        .child(format!("Show {more} more")),
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.show_more_settled(cx))),
                        )
                    }),
            );
        }

        let empty = rows.order().is_empty() && rows.settled_count == 0;
        let empty_state = empty.then(|| self.render_empty(&rows, cx));
        v_flex()
            .size_full()
            .child(header)
            .child(
                div()
                    .id("t3-list")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .min_h_full()
                            .px_1()
                            .py_1()
                            .when_some(rows.note.clone(), |this, note| {
                                this.child(
                                    div().px_2().py_1().text_xs().text_color(muted).child(note),
                                )
                            })
                            .when_some(empty_state, |this, empty| this.child(empty))
                            .child(top)
                            // Shelves sit at the bottom ("boring sidebar").
                            .child(div().flex_1().min_h_4())
                            .child(shelves),
                    ),
            )
            .into_any_element()
    }

    fn render_t3_header(&self, rows: &T3Rows, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let weak = cx.weak_entity();
        let projects = rows.projects.clone();
        let filter = rows.filter.clone();
        h_flex()
            .flex_shrink_0()
            .h_9()
            .px_2()
            .gap_1()
            .border_b_1()
            .border_color(theme.sidebar_border)
            .child(
                Button::new("project-filter")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(Lucide::ListFilter))
                    .label(rows.filter_label.clone())
                    .tooltip("Filter threads by project")
                    .dropdown_menu(move |menu, _, _| {
                        let item = |label: SharedString, value: Option<ProjectKey>| {
                            let weak = weak.clone();
                            let checked = value == filter;
                            PopupMenuItem::new(label)
                                .checked(checked)
                                .on_click(move |_, _, cx| {
                                    let value = value.clone();
                                    let _ = weak
                                        .update(cx, |this, cx| this.set_project_filter(value, cx));
                                })
                        };
                        let menu = menu.item(item("All projects".into(), None));
                        let menu = if projects.is_empty() {
                            menu
                        } else {
                            menu.separator()
                        };
                        projects.iter().fold(menu, |menu, (key, label)| {
                            menu.item(item(label.clone(), Some(key.clone())))
                        })
                    }),
            )
            .child(div().flex_1())
            .child(
                Button::new("new-thread")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(Lucide::SquarePen))
                    .tooltip("New thread")
                    .on_click(cx.listener(|this, _, _, cx| {
                        let scope = this.project_filter.clone();
                        cx.emit(SidebarEvent::NewThread(scope))
                    })),
            )
    }

    fn render_empty(&self, rows: &T3Rows, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let text: SharedString = if rows.note.is_some() {
            return div().into_any_element();
        } else if !rows.has_projects {
            "No projects yet".into()
        } else if rows.filter.is_some() {
            format!("No threads in {} yet", rows.filter_label).into()
        } else {
            "No threads yet".into()
        };
        v_flex()
            .items_center()
            .gap_2()
            .py_6()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(
                div()
                    .id("t3-empty")
                    .test_support()
                    .aria_label(text.clone())
                    .child(text),
            )
            .when(!rows.has_projects, |this| {
                this.child(
                    Button::new("empty-add-project")
                        .outline()
                        .xsmall()
                        .icon(IconName::Plus)
                        .label("Add project")
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(AddProject), cx)),
                )
            })
            .into_any_element()
    }

    /// A drop line shown while dragging, labelled with the section it
    /// leads into.
    fn render_boundary(
        &self,
        label: &'static str,
        id: &'static str,
        over: Option<usize>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .id(id)
            .gap_2()
            .px_2()
            .py_1()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(label)
            .child(div().flex_1().h_px().bg(theme.sidebar_border))
            .drag_over::<DraggedThread>(|style, _, _, cx| {
                style
                    .text_color(cx.theme().primary)
                    .bg(cx.theme().drop_target)
            })
            .when_some(over, |this, over| {
                this.on_drop(cx.listener(move |this, drag: &DraggedThread, _, cx| {
                    this.drop_thread(drag, over, cx)
                }))
            })
    }

    fn render_shelf_header(
        &self,
        shelf: Shelf,
        title: &'static str,
        count: usize,
        expanded: bool,
        over: Option<usize>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let id = match shelf {
            Shelf::Working => "shelf-working",
            Shelf::Snoozed => "shelf-snoozed",
            Shelf::Settled => "shelf-settled",
        };
        ListItem::new(id)
            .accessibility_label(format!("{title} {count}"))
            .mt_2()
            .px_2()
            .py_1()
            .rounded(theme.radius)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Icon::new(if expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .xsmall(),
                    )
                    .child(div().font_medium().child(title))
                    .child(div().child(count.to_string())),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_shelf(shelf, cx)))
            .when_some(over, |this, over| {
                this.drag_over::<DraggedThread>(|style, _, _, cx| style.bg(cx.theme().drop_target))
                    .on_drop(cx.listener(move |this, drag: &DraggedThread, _, cx| {
                        this.drop_thread(drag, over, cx)
                    }))
            })
    }

    /// The trailing status of a row: Woke, the snooze countdown, a status
    /// pill (with the working time), or the row's time.
    fn render_status(&self, row: &T3Row, now: OffsetDateTime, theme: &Theme) -> AnyElement {
        let slot = h_flex().gap_1().flex_shrink_0().text_xs();
        if row.section == T3Section::Snoozed
            && let Some(until) = row.snoozed_until
        {
            return slot
                .text_color(theme.info)
                .child(Icon::new(Lucide::AlarmClock).xsmall())
                .child(snooze_wake_label(until, now))
                .into_any_element();
        }
        if row.woke {
            return slot
                .text_color(theme.warning)
                .font_medium()
                .child(Icon::new(Lucide::AlarmClock).xsmall())
                .child("Woke")
                .into_any_element();
        }
        if row.section != T3Section::Settled
            && let Some(label) = row.status.label(row.unread)
        {
            return slot
                .text_color(status_color(row.status, theme))
                .font_medium()
                .child(status_icon(row.status).xsmall())
                .child(label)
                .when_some(row.working_since, |this, since| {
                    this.child(
                        div()
                            .font_normal()
                            .text_color(theme.muted_foreground)
                            .child(format_working_duration(now - since)),
                    )
                })
                .into_any_element();
        }
        slot.text_color(theme.muted_foreground)
            .when_some(row.time.map(|time| age_since(time, now)), |this, age| {
                this.child(age)
            })
            .into_any_element()
    }

    /// The hover action of a row: Settle, Un-settle or Wake (also in the
    /// context menu and on ⌘⇧S).
    fn render_row_action(&self, row: &T3Row, cx: &Context<Self>) -> Option<AnyElement> {
        let key = row.key.clone();
        let (command, icon, label, tooltip): (_, Icon, Option<&str>, &str) = match row.section {
            T3Section::Settled => (
                ThreadCommand::Unsettle,
                Icon::new(IconName::Undo2),
                None,
                "Un-settle thread",
            ),
            T3Section::Snoozed => (
                ThreadCommand::Wake,
                Icon::new(Lucide::AlarmClockOff),
                None,
                "Wake thread now",
            ),
            T3Section::Working => return None,
            T3Section::Pinned | T3Section::Active => (
                ThreadCommand::Settle,
                Icon::new(IconName::Check),
                Some("Settle"),
                "Settle thread",
            ),
        };
        Some(
            Button::new(key.element_id("row-action"))
                .ghost()
                .xsmall()
                .icon(icon)
                .when_some(label, |this, label| this.label(label))
                .tooltip(tooltip)
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.stop_propagation();
                    cx.emit(SidebarEvent::Commands(vec![(key.clone(), command.clone())]));
                }))
                .into_any_element(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_row(
        &self,
        row: &T3Row,
        over: Option<usize>,
        now: OffsetDateTime,
        weak: &WeakEntity<Self>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let selected = self.selected.as_ref() == Some(&row.key);
        let group: SharedString = row.key.element_id("row");
        let status = div()
            .flex_shrink_0()
            .group_hover(group.clone(), |style| style.invisible())
            .child(self.render_status(row, now, theme));
        let action = self.render_row_action(row, cx).map(|action| {
            div()
                .absolute()
                .right_0()
                .top_0()
                .bottom_0()
                .flex()
                .items_center()
                .invisible()
                .group_hover(group.clone(), |style| style.visible())
                .child(action)
        });
        let quiet = row.recede || row.section == T3Section::Settled;
        let title_color = if quiet {
            theme.muted_foreground
        } else {
            theme.sidebar_foreground
        };
        let body = if row.section == T3Section::Settled {
            // Settled rows are slim: one line, quiet.
            h_flex()
                .relative()
                .gap_2()
                .min_w_0()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(title_color)
                        .child(row.title.clone()),
                )
                .child(status)
                .children(action)
                .into_any_element()
        } else {
            v_flex()
                .relative()
                .min_w_0()
                .child(
                    h_flex()
                        .gap_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .when(row.pinned, |this| {
                            this.child(Icon::new(Lucide::Pin).xsmall())
                        })
                        .child(div().min_w_0().truncate().child(row.project.clone()))
                        .when_some(row.env_label.clone(), |this, env| {
                            this.child(div().flex_shrink_0().child(format!("· {env}")))
                        })
                        .child(div().flex_1())
                        .child(status),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(title_color)
                        .when(!quiet, |this| this.font_medium())
                        .child(row.title.clone()),
                )
                .children(action)
                .into_any_element()
        };
        let key = row.key.clone();
        let menu_row = MenuRow {
            key: row.key.clone(),
            pinned: row.pinned,
            settled: row.settled,
            snoozed: row.section == T3Section::Snoozed,
            snoozable: row.snoozable,
            auto_settle: row.auto_settle,
            has_completion: row.has_completion,
        };
        let menu_weak = weak.clone();
        let item = ListItem::new(row.key.element_id("thread"))
            .group(group)
            .accessibility_label(row.accessibility_label())
            .selected(selected)
            .px_2()
            .py_1()
            .rounded(theme.radius)
            .child(body)
            .on_click(
                cx.listener(move |_, _, _, cx| cx.emit(SidebarEvent::OpenThread(key.clone()))),
            );
        let item = if row.section == T3Section::Working {
            item
        } else {
            let drag = DraggedThread {
                key: row.key.clone(),
                title: row.title.clone(),
            };
            item.on_drag(drag, |drag, _, _, cx| {
                let title = drag.title.clone();
                cx.new(|_| DragPreview(title))
            })
        };
        let item = match over {
            Some(over) => item
                .drag_over::<DraggedThread>(|style, _, _, cx| style.bg(cx.theme().drop_target))
                .on_drop(cx.listener(move |this, drag: &DraggedThread, _, cx| {
                    this.drop_thread(drag, over, cx)
                })),
            None => item,
        };
        item.context_menu(move |menu, window, cx| menu_row.build(&menu_weak, menu, window, cx))
            .into_any_element()
    }
}

/// What a row's context menu needs.
#[derive(Clone)]
struct MenuRow {
    key: ThreadKey,
    pinned: bool,
    settled: bool,
    snoozed: bool,
    snoozable: bool,
    auto_settle: bool,
    has_completion: bool,
}

impl MenuRow {
    /// t3's thread action menu (`buildThreadActionMenuItems`,
    /// threadActionMenu.logic.ts:117-216), without what j3 lacks (branches,
    /// guests, regeneration, copy, project settings).
    fn build(
        &self,
        weak: &WeakEntity<Sidebar>,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let key = &self.key;
        let command = |label: &'static str, command: ThreadCommand| {
            let weak = weak.clone();
            let key = key.clone();
            PopupMenuItem::new(label).on_click(move |_, _, cx| {
                emit(
                    &weak,
                    SidebarEvent::Commands(vec![(key.clone(), command.clone())]),
                    cx,
                )
            })
        };
        let event = |label: &'static str, event: SidebarEvent| {
            let weak = weak.clone();
            PopupMenuItem::new(label).on_click(move |_, _, cx| emit(&weak, event.clone(), cx))
        };
        let menu = menu
            .item(if self.pinned {
                command("Unpin thread", ThreadCommand::Unpin)
            } else {
                command("Pin thread", ThreadCommand::Pin(None))
            })
            .item(if self.settled {
                command("Un-settle thread", ThreadCommand::Unsettle)
            } else {
                command("Settle thread", ThreadCommand::Settle)
            });
        let menu = if self.snoozed {
            menu.item(command("Wake thread", ThreadCommand::Wake))
        } else if self.snoozable {
            let weak = weak.clone();
            let key = key.clone();
            menu.submenu("Snooze", window, cx, move |submenu, _, _| {
                let zone = TimeZone::system();
                let presets = resolve_snooze_presets(OffsetDateTime::now_utc(), &zone);
                let submenu = presets.iter().fold(submenu, |submenu, preset| {
                    let weak = weak.clone();
                    let key = key.clone();
                    let shown = presets.clone();
                    let id = preset.id.clone();
                    submenu.item(
                        PopupMenuItem::new(format!("{} ({})", preset.label, preset.when_label))
                            .on_click(move |_, _, cx| {
                                let zone = TimeZone::system();
                                let now = OffsetDateTime::now_utc();
                                if let Some(preset) = pick_snooze_preset(&shown, &id, now, &zone) {
                                    emit(
                                        &weak,
                                        SidebarEvent::Commands(vec![(
                                            key.clone(),
                                            ThreadCommand::Snooze(preset.snoozed_until),
                                        )]),
                                        cx,
                                    );
                                }
                            }),
                    )
                });
                let weak = weak.clone();
                let key = key.clone();
                submenu
                    .separator()
                    .item(PopupMenuItem::new("Custom…").on_click(move |_, _, cx| {
                        emit(&weak, SidebarEvent::CustomSnooze(key.clone()), cx)
                    }))
            })
        } else {
            menu.item(PopupMenuItem::new("Snooze").disabled(true))
        };
        let auto_settle = self.auto_settle;
        let auto_weak = weak.clone();
        let auto_key = key.clone();
        menu.separator()
            .item(event(
                "Rename thread…",
                SidebarEvent::RenameThread(key.clone()),
            ))
            .item(command("Mark unread", ThreadCommand::MarkUnread).disabled(!self.has_completion))
            .submenu("Auto-settle", window, cx, move |submenu, _, _| {
                let option = |label: &'static str, enabled: bool| {
                    let weak = auto_weak.clone();
                    let key = auto_key.clone();
                    PopupMenuItem::new(label)
                        .checked(auto_settle == enabled)
                        .on_click(move |_, _, cx| {
                            emit(
                                &weak,
                                SidebarEvent::Commands(vec![(
                                    key.clone(),
                                    ThreadCommand::AutoSettle(enabled),
                                )]),
                                cx,
                            )
                        })
                };
                submenu
                    .item(option("Enabled", true))
                    .item(option("Disabled", false))
            })
            .separator()
            .item(event(
                "Archive thread",
                SidebarEvent::ArchiveThread(key.clone()),
            ))
            .item(event("Delete…", SidebarEvent::DeleteThread(key.clone())))
    }
}
