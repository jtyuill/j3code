//! The Inbox's rows (`docs/triage.md`): one list across environments, Pinned and Active at
//! the top; the Working, Snoozed and Settled shelves anchored at the bottom, count-labelled
//! and collapsible; Settled paged. Status, sections and drop plans come from
//! `j3_client::inbox`; [`super::inbox_view`] draws the rows.

use std::{collections::HashMap, time::Duration};

use j3_client::{
    EnvThread,
    entities::{Settled, Thread},
    inbox::{
        DropInput, DropPlan, ListItem as Item, ListMarker, Section, Status, can_settle, can_snooze,
        completed_at, drop_target, is_listed, is_unread, layout, plan_drop, recedes, settled_time,
        status, woke_at, working_since,
    },
};
use time::OffsetDateTime;

use super::{
    EnvSnapshot, ProjectKey, ThreadCommand, ThreadKey, display_title, parse_time, until_next_minute,
};
use crate::ui_settings::{Shelf, UiSettings};

/// Settled rows shown at first (`docs/triage.md` §5.5).
pub const SETTLED_INITIAL_COUNT: usize = 10;
/// Rows each "Show N more" adds.
pub const SETTLED_PAGE_COUNT: usize = 25;

/// One thread row.
#[derive(Debug, Clone)]
pub struct InboxRow {
    pub key: ThreadKey,
    pub section: Section,
    pub title: String,
    pub project: String,
    /// The environment, when more than one is connected.
    pub env_label: Option<String>,
    pub status: Status,
    pub unread: bool,
    pub woke: bool,
    pub recede: bool,
    pub working_since: Option<OffsetDateTime>,
    /// The row's time: when it settled for settled rows, else its latest user message (or
    /// update).
    pub time: Option<OffsetDateTime>,
    pub snoozed_until: Option<OffsetDateTime>,
    pub pinned: bool,
    pub settled: bool,
    /// The server would accept a snooze (`docs/triage.md` §12.1(1)).
    pub snoozable: bool,
    /// The server would accept a settle (§12.1(3)): Settle is offered only then.
    pub settleable: bool,
    pub auto_settle: bool,
    pub has_completion: bool,
}

impl InboxRow {
    /// The status word the row shows, if any (also its accessible state).
    pub fn badge(&self) -> Option<&'static str> {
        if self.section == Section::Snoozed {
            return Some("Snoozed");
        }
        if self.woke {
            return Some("Woke");
        }
        if let Some(label) = self.status.label(self.unread) {
            return Some(label);
        }
        (self.section == Section::Settled).then_some("Settled")
    }

    /// `"{title}, {badge}"`, or the title alone.
    pub fn accessibility_label(&self) -> String {
        match self.badge() {
            Some(badge) => format!("{}, {badge}", self.title),
            None => self.title.clone(),
        }
    }
}

/// The Inbox list, built from the distinct environments' shells.
#[derive(Debug, Default)]
pub struct InboxRows {
    pub pinned: Vec<InboxRow>,
    pub active: Vec<InboxRow>,
    pub working: Vec<InboxRow>,
    pub snoozed: Vec<InboxRow>,
    pub settled: Vec<InboxRow>,
    pub working_count: usize,
    pub snoozed_count: usize,
    pub settled_count: usize,
    pub expanded: HashMap<Shelf, bool>,
    pub active_time_ordered: bool,
    pub next_wake_at: Option<OffsetDateTime>,
    /// The rendered list (rows and markers), for drop targets.
    pub items: Vec<Item<ThreadKey>>,
    pub pinned_keys: HashMap<ThreadKey, String>,
    pub active_keys: HashMap<ThreadKey, String>,
    /// The filter menu: every project, with its label.
    pub projects: Vec<(ProjectKey, String)>,
    /// The filtered-to project's label; `None` shows every project.
    pub filter_label: Option<String>,
    pub filter: Option<ProjectKey>,
    pub has_projects: bool,
    /// "Connecting…" while no environment has synced.
    pub note: Option<&'static str>,
}

impl InboxRows {
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
            .filter(|env| env.state.status == j3_client::Status::Connected)
            .map(|env| env.key)
            .collect();
        let multi_env = snapshots.len() > 1 && connected.len() > 1;
        let mut projects = Vec::new();
        let mut project_titles: HashMap<(&str, &str), &str> = HashMap::new();
        let mut pinned_keys = HashMap::new();
        let mut active_keys = HashMap::new();
        let mut threads = Vec::new();
        for env in snapshots {
            let shell = &env.state.shell;
            for project in shell.projects_in_order() {
                project_titles.insert((env.key, project.id.as_str()), project.title.as_str());
                let label = if multi_env {
                    format!("{} · {}", project.title, env.label)
                } else {
                    project.title.clone()
                };
                let key = ProjectKey {
                    env: env.key.to_owned(),
                    project_id: project.id.clone(),
                };
                projects.push((key, label));
            }
            for thread in shell.threads.values().filter(|thread| is_listed(thread)) {
                let key = ThreadKey::new(env.key, &thread.id);
                // Hidden rows' keys stay reserved when rearranging.
                if let Some(order) = &thread.triage.pin_order {
                    pinned_keys.insert(key.clone(), order.clone());
                }
                if let Some(order) = &thread.triage.active_order {
                    active_keys.insert(key, order.clone());
                }
                let in_filter =
                    filter.is_none_or(|f| f.env == env.key && f.project_id == thread.project_id);
                if in_filter {
                    threads.push(EnvThread {
                        environment: env.key,
                        thread,
                    });
                }
            }
        }
        let layout = layout(threads, now, ui.working_shelf, |env| {
            connected.contains(&env)
        });
        let labels: HashMap<&str, &str> =
            snapshots.iter().map(|env| (env.key, env.label)).collect();
        let row = |entry: &EnvThread, section: Section| -> InboxRow {
            let thread: &Thread = entry.thread;
            let key = ThreadKey::new(entry.environment, &thread.id);
            let status = status(thread);
            let unread = is_unread(thread);
            let woke = woke_at(thread, now).is_some();
            let is_selected = selected == Some(&key);
            InboxRow {
                section,
                title: display_title(&thread.title),
                project: project_titles
                    .get(&(entry.environment, thread.project_id.as_str()))
                    .map_or_else(String::new, |title| (*title).to_owned()),
                env_label: multi_env
                    .then(|| {
                        labels
                            .get(entry.environment)
                            .map(|label| (*label).to_owned())
                    })
                    .flatten(),
                status,
                unread,
                woke,
                recede: recedes(status, unread, woke, is_selected),
                working_since: working_since(thread),
                time: if section == Section::Settled {
                    settled_time(thread)
                } else {
                    thread
                        .latest_user_message_at
                        .as_deref()
                        .and_then(parse_time)
                        .or_else(|| parse_time(&thread.updated_at))
                },
                snoozed_until: thread.triage.snoozed_until.as_deref().and_then(parse_time),
                pinned: thread.triage.pinned_at.is_some(),
                settled: thread.triage.settled == Some(Settled::Settled),
                snoozable: can_snooze(thread),
                settleable: can_settle(thread),
                auto_settle: thread.triage.auto_settle_off_at.is_none(),
                has_completion: completed_at(thread).is_some(),
                key,
            }
        };
        let rows = |entries: &[EnvThread], section: Section| -> Vec<InboxRow> {
            entries.iter().map(|entry| row(entry, section)).collect()
        };
        // A collapsed shelf still shows the open thread.
        let shelf = |entries: &[EnvThread], section: Section, shelf: Shelf| -> Vec<InboxRow> {
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
        let settled = if ui.is_expanded(Shelf::Settled) {
            layout
                .settled
                .iter()
                .take(settled_shown)
                .map(|entry| row(entry, Section::Settled))
                .collect()
        } else {
            shelf(&layout.settled, Section::Settled, Shelf::Settled)
        };
        let filter_label = filter.and_then(|filter| {
            projects
                .iter()
                .find(|(key, _)| key == filter)
                .map(|(_, label)| label.clone())
        });
        let mut list = Self {
            pinned: rows(&layout.pinned, Section::Pinned),
            active: rows(&layout.active, Section::Active),
            working: shelf(&layout.working, Section::Working, Shelf::Working),
            snoozed: shelf(&layout.snoozed, Section::Snoozed, Shelf::Snoozed),
            settled,
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
            filter_label,
            filter: filter.cloned(),
            has_projects: !projects.is_empty(),
            projects,
            note: (!snapshots.iter().any(|env| env.state.shell.synced)).then_some("Connecting…"),
        };
        list.items = list.list_items();
        list
    }

    /// Whether `shelf` is expanded.
    pub fn is_expanded(&self, shelf: Shelf) -> bool {
        self.expanded.get(&shelf).copied().unwrap_or(false)
    }

    /// The rendered list in display order, with its markers (`docs/triage.md` §5.5).
    fn list_items(&self) -> Vec<Item<ThreadKey>> {
        let thread = |row: &InboxRow| Item::Thread {
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

    /// Every visible row, in display order.
    pub fn rows(&self) -> impl Iterator<Item = &InboxRow> {
        self.pinned
            .iter()
            .chain(&self.active)
            .chain(&self.working)
            .chain(&self.snoozed)
            .chain(&self.settled)
    }

    pub fn row(&self, key: &ThreadKey) -> Option<&InboxRow> {
        self.rows().find(|row| &row.key == key)
    }

    pub fn marker_index(&self, marker: ListMarker) -> Option<usize> {
        self.items
            .iter()
            .position(|item| matches!(item, Item::Marker(m) if *m == marker))
    }

    fn thread_index(&self, key: &ThreadKey) -> Option<usize> {
        self.items
            .iter()
            .position(|item| matches!(item, Item::Thread { key: k, .. } if k == key))
    }

    /// The list slot a drop onto `row` lands in: its own for Pinned (and an Active that keeps
    /// its own order), Active's placeholder for a time-ordered Active, Settled's placeholder
    /// for settled rows; Working and Snoozed rows take no drops.
    pub fn drop_slot(&self, row: &InboxRow) -> Option<usize> {
        match row.section {
            Section::Pinned => self.thread_index(&row.key),
            Section::Active if self.active_time_ordered => {
                self.marker_index(ListMarker::ActivePlaceholder)
            }
            Section::Active => self.thread_index(&row.key),
            Section::Settled => self.marker_index(ListMarker::SettledPlaceholder),
            Section::Working | Section::Snoozed => None,
        }
    }

    /// Visible rows in display order, for keyboard navigation.
    pub fn order(&self) -> Vec<ThreadKey> {
        self.rows().map(|row| row.key.clone()).collect()
    }

    fn order_of(rows: &[InboxRow]) -> Vec<ThreadKey> {
        rows.iter().map(|row| row.key.clone()).collect()
    }

    /// When the list next changes on its own: the next snooze wake, the working timers
    /// (every second) or the relative times (each minute).
    pub fn next_tick(&self, now: OffsetDateTime) -> Duration {
        let timers = self.rows().any(|row| row.working_since.is_some());
        let tick = if timers {
            Duration::from_secs(1)
        } else {
            until_next_minute(now)
        };
        let wake = self.next_wake_at.map(|wake| {
            // Just past the wake, so the row has moved when the list is laid out again.
            Duration::try_from(wake - now).unwrap_or_default() + Duration::from_millis(50)
        });
        wake.map_or(tick, |wake| wake.min(tick))
    }

    /// The commands a drop of `moved` onto list slot `over` makes, in order.
    pub fn plan(&self, moved: &ThreadKey, over: usize) -> Vec<(ThreadKey, ThreadCommand)> {
        let Some(row) = self.row(moved) else {
            return Vec::new();
        };
        let Some(index) = self.thread_index(moved) else {
            return Vec::new();
        };
        let Some(target) = drop_target(&self.items, index, over) else {
            return Vec::new();
        };
        let pinned_order = Self::order_of(&self.pinned);
        let active_order = Self::order_of(&self.active);
        let plan = plan_drop(DropInput {
            moved,
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
        let moved = moved.clone();
        match plan {
            DropPlan::None => Vec::new(),
            DropPlan::Settle => vec![(moved, ThreadCommand::Settle)],
            DropPlan::ReorderPinned { assignments } => assignments
                .into_iter()
                .map(|a| (a.id, ThreadCommand::ReorderPinned(a.order)))
                .collect(),
            DropPlan::Pin { order, extra } => std::iter::once((moved, ThreadCommand::Pin(order)))
                .chain(
                    extra
                        .into_iter()
                        .map(|a| (a.id, ThreadCommand::ReorderPinned(a.order))),
                )
                .collect(),
            DropPlan::MoveActive {
                assignments,
                unpin,
                unsettle,
                wake,
            } => [
                unpin.then_some(ThreadCommand::Unpin),
                unsettle.then_some(ThreadCommand::Unsettle),
                wake.then_some(ThreadCommand::Wake),
            ]
            .into_iter()
            .flatten()
            .map(|command| (moved.clone(), command))
            .chain(
                assignments
                    .into_iter()
                    .map(|a| (a.id, ThreadCommand::ReorderActive(a.order))),
            )
            .collect(),
        }
    }
}
