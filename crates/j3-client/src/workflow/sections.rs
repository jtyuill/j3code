//! T3 mode's list: section resolution, per-section order, the layout of
//! one merged list across environments, and drag & drop planning.
//!
//! Ports of `apps/web/src/components/Sidebar.logic.ts:175-534`,
//! `Sidebar.tsx:627-731` (`partitionSidebarThreads`) and
//! `Sidebar.tsx:1594-1616` (`planForwardNavigation`), plus the sorts in
//! `threadSort.ts:49-64,263-331` and `threadInbox.ts:38-89`.

use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    hash::Hash,
};

use j3_protocol::t3::orchestration::{SettledOverride, ThreadShell};
use time::OffsetDateTime;

use super::{
    EnvThread,
    order::{OrderAssignment, plan_reorder},
    status::{self, next_snooze_wake_after},
    view::{View, flat, now_ms, opt_ms},
};

/// Where a thread sits in T3 mode (`SidebarSection`, Sidebar.logic.ts:175).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum T3Section {
    Pinned,
    Active,
    Working,
    Snoozed,
    Settled,
}

/// The thread's section, or `None` when it gets no row (subagents, side
/// chats, archived, deleted). Snoozed beats Settled beats Pinned; the
/// Working shelf takes otherwise-Active threads that are working
/// (`resolveSidebarThreadSection`, Sidebar.logic.ts:177-186, and
/// `partitionSidebarThreads`, Sidebar.tsx:644-692).
pub fn t3_section(
    thread: &ThreadShell,
    now: OffsetDateTime,
    working_shelf: bool,
) -> Option<T3Section> {
    section_of(&View::of(thread), now_ms(now), working_shelf)
}

fn section_of(view: &View<'_>, now: i64, working_shelf: bool) -> Option<T3Section> {
    let thread = view.shell;
    if !status::is_listed(thread) {
        return None;
    }
    Some(if status::snoozed(view, now) {
        T3Section::Snoozed
    } else if thread.settled_override == Some(SettledOverride::Settled) {
        T3Section::Settled
    } else if flat(&thread.pinned_at).is_some() {
        T3Section::Pinned
    } else if working_shelf && status::working(view) {
        T3Section::Working
    } else {
        T3Section::Active
    })
}

/// Thread ids are unique only within an environment and the list merges
/// environments, so ties fall back to both parts.
fn identity(left: &EnvThread<'_>, right: &EnvThread<'_>) -> Ordering {
    left.thread
        .id
        .cmp(&right.thread.id)
        .then_with(|| left.environment.cmp(right.environment))
}

fn ts(iso: Option<&str>) -> i64 {
    opt_ms(iso).unwrap_or(0)
}

/// Keyed rows by `pinOrderKey`, then keyless rows newest first
/// (`sortPinnedThreadsByOrderKey`, threadSort.ts:263-296).
pub fn sort_pinned(threads: &mut [EnvThread<'_>]) {
    threads.sort_by(|left, right| {
        let keys = (
            flat(&left.thread.pin_order_key),
            flat(&right.thread.pin_order_key),
        );
        match keys {
            (Some(l), Some(r)) => l.cmp(r),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => {
                ts(Some(&right.thread.created_at)).cmp(&ts(Some(&left.thread.created_at)))
            }
        }
        .then_with(|| identity(left, right))
    });
}

/// `max(createdAt, unsettledAt)` (threadSort.ts:116-124).
fn active_anchor(thread: &ThreadShell) -> i64 {
    ts(Some(&thread.created_at)).max(ts(flat(&thread.unsettled_at)))
}

/// Keyless rows first, newest anchor (creation or reopening) first, then
/// keyed rows by `activeOrderKey` (`sortActiveThreadsByOrderKey`,
/// threadSort.ts:298-331). The Active order while the Working shelf is off.
pub fn sort_active(threads: &mut [EnvThread<'_>]) {
    threads.sort_by(|left, right| {
        let keys = (
            flat(&left.thread.active_order_key),
            flat(&right.thread.active_order_key),
        );
        match keys {
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(l), Some(r)) => l.cmp(r),
            (None, None) => active_anchor(right.thread).cmp(&active_anchor(left.thread)),
        }
        .then_with(|| identity(left, right))
    });
}

fn sort_newest_first(threads: &mut [EnvThread<'_>], timestamp: impl Fn(&ThreadShell) -> i64) {
    threads.sort_by(|left, right| {
        timestamp(right.thread)
            .cmp(&timestamp(left.thread))
            .then_with(|| identity(left, right))
    });
}

/// The thread that most recently came back to the inbox (created, reopened,
/// sent, finished) first (`sortInboxThreadsByReturn`, threadInbox.ts:38-55).
/// The Active order while the Working shelf is on.
pub fn sort_active_by_return(threads: &mut [EnvThread<'_>]) {
    sort_newest_first(threads, |thread| {
        let run = View::of(thread).latest_run;
        ts(Some(&thread.created_at))
            .max(ts(flat(&thread.unsettled_at)))
            .max(ts(run.and_then(|run| run.requested_at)))
            .max(ts(run.and_then(|run| run.completed_at)))
    });
}

/// By the last message the user sent, not later runs
/// (`sortWorkingThreadsBySend`, threadInbox.ts:62-77).
pub fn sort_working(threads: &mut [EnvThread<'_>]) {
    sort_newest_first(threads, |thread| {
        let sent = match &thread.latest_user_authored_message_at {
            None => View::of(thread).latest_run.and_then(|run| run.requested_at),
            Some(authored) => authored.as_deref(),
        };
        ts(Some(&thread.created_at)).max(ts(sent))
    });
}

/// Soonest wake first (Sidebar.tsx:722-726).
pub fn sort_snoozed(threads: &mut [EnvThread<'_>]) {
    threads.sort_by(|left, right| {
        ts(flat(&left.thread.snoozed_until))
            .cmp(&ts(flat(&right.thread.snoozed_until)))
            .then_with(|| identity(left, right))
    });
}

/// Most recently settled first (`sortSettledThreads`, threadSort.ts:49-64).
pub fn sort_settled(threads: &mut [EnvThread<'_>]) {
    sort_newest_first(threads, |thread| {
        status::settled_ms(&View::of(thread)).unwrap_or(0)
    });
}

/// T3 mode's merged list, section by section, each in t3's order.
#[derive(Debug, Clone, Default)]
pub struct T3Layout<'a> {
    pub pinned: Vec<EnvThread<'a>>,
    pub active: Vec<EnvThread<'a>>,
    pub working: Vec<EnvThread<'a>>,
    pub snoozed: Vec<EnvThread<'a>>,
    pub settled: Vec<EnvThread<'a>>,
    /// Active is ordered by return (the Working shelf is on), so rows in it
    /// cannot be rearranged; drops into it only change the lifecycle.
    pub active_time_ordered: bool,
    /// When the earliest snooze wakes; re-lay the list then.
    pub next_wake_at: Option<OffsetDateTime>,
}

/// Partitions and sorts the threads of every environment
/// (`partitionSidebarThreads`, Sidebar.tsx:627-731). With `working_shelf`
/// on (t3's default), working threads fold into the Working shelf, except
/// those of environments `connected` rejects, and Active is ordered by
/// return; off, Active follows `activeOrderKey`.
pub fn t3_layout<'a>(
    threads: impl IntoIterator<Item = EnvThread<'a>>,
    now: OffsetDateTime,
    working_shelf: bool,
    connected: impl Fn(&str) -> bool,
) -> T3Layout<'a> {
    let now_ms = now_ms(now);
    let mut layout = T3Layout {
        active_time_ordered: working_shelf,
        ..T3Layout::default()
    };
    let mut listed = Vec::new();
    for entry in threads {
        let view = View::of(entry.thread);
        let shelf = working_shelf && connected(entry.environment);
        let Some(section) = section_of(&view, now_ms, shelf) else {
            continue;
        };
        listed.push(entry.thread);
        match section {
            T3Section::Pinned => &mut layout.pinned,
            T3Section::Active => &mut layout.active,
            T3Section::Working => &mut layout.working,
            T3Section::Snoozed => &mut layout.snoozed,
            T3Section::Settled => &mut layout.settled,
        }
        .push(entry);
    }
    sort_pinned(&mut layout.pinned);
    if working_shelf {
        sort_active_by_return(&mut layout.active);
    } else {
        sort_active(&mut layout.active);
    }
    sort_working(&mut layout.working);
    sort_snoozed(&mut layout.snoozed);
    sort_settled(&mut layout.settled);
    layout.next_wake_at = next_snooze_wake_after(listed, now);
    layout
}

/// What a drop from one section into another does
/// (`resolveSidebarDropVerb`, Sidebar.logic.ts:306-318).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DropVerb {
    Pin,
    Unpin,
    Settle,
    Unsettle,
    Wake,
}

/// `None` for no move or a drop into Working or Snoozed, which take no
/// drops.
pub fn drop_verb(from: T3Section, to: Option<T3Section>) -> Option<DropVerb> {
    let to = to?;
    if to == from || matches!(to, T3Section::Working | T3Section::Snoozed) {
        return None;
    }
    Some(match (from, to) {
        (_, T3Section::Pinned) => DropVerb::Pin,
        (_, T3Section::Settled) => DropVerb::Settle,
        (T3Section::Pinned, _) => DropVerb::Unpin,
        (T3Section::Settled, _) => DropVerb::Unsettle,
        _ => DropVerb::Wake,
    })
}

/// A non-row entry of the rendered list (`SidebarListMarker`,
/// Sidebar.logic.ts:213-220). Headers start sections; the pinned divider
/// ends Pinned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListMarker {
    PinnedHeader,
    ActivePlaceholder,
    SettledPlaceholder,
    PinnedDivider,
    WorkingHeader,
    SnoozedHeader,
    SettledHeader,
}

/// One entry of the rendered list, in order (`SidebarListItem`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListItem<K> {
    Thread { key: K, section: T3Section },
    Marker(ListMarker),
}

/// The sections that take drops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DropSection {
    Pinned,
    Active,
    Settled,
}

/// Where a dragged row lands, with the Pinned and Active orders after the
/// move (`SidebarDropTarget`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropTarget<K> {
    pub section: DropSection,
    pub pinned_order: Vec<K>,
    pub active_order: Vec<K>,
}

fn section_at_slot<K>(items: &[ListItem<K>], index: usize) -> T3Section {
    let mut section = T3Section::Pinned;
    for item in items.iter().take(index) {
        if let ListItem::Marker(marker) = item {
            section = match marker {
                ListMarker::PinnedDivider => T3Section::Active,
                ListMarker::WorkingHeader => T3Section::Working,
                ListMarker::SnoozedHeader => T3Section::Snoozed,
                ListMarker::SettledHeader => T3Section::Settled,
                _ => section,
            };
        }
    }
    section
}

/// The drop target for moving `items[moved]` to `items[over]`'s slot;
/// `None` unless `moved` is a row, or when the slot is in Working or
/// Snoozed (`resolveSidebarDropTarget`, Sidebar.logic.ts:253-281).
pub fn drop_target<K: Clone>(
    items: &[ListItem<K>],
    moved: usize,
    over: usize,
) -> Option<DropTarget<K>> {
    if over >= items.len() || !matches!(items.get(moved), Some(ListItem::Thread { .. })) {
        return None;
    }
    let mut rearranged: Vec<&ListItem<K>> = items
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != moved)
        .map(|(_, item)| item)
        .collect();
    rearranged.insert(over, &items[moved]);
    let rearranged: Vec<ListItem<K>> = rearranged.into_iter().cloned().collect();
    let section = match section_at_slot(&rearranged, over) {
        T3Section::Pinned => DropSection::Pinned,
        T3Section::Active => DropSection::Active,
        T3Section::Settled => DropSection::Settled,
        T3Section::Working | T3Section::Snoozed => return None,
    };
    let mut pinned_order = Vec::new();
    let mut active_order = Vec::new();
    let mut in_pinned = true;
    for item in rearranged {
        match item {
            ListItem::Marker(ListMarker::PinnedDivider) => in_pinned = false,
            ListItem::Marker(
                ListMarker::WorkingHeader | ListMarker::SnoozedHeader | ListMarker::SettledHeader,
            ) => break,
            ListItem::Marker(_) => {}
            ListItem::Thread { key, .. } if in_pinned => pinned_order.push(key),
            ListItem::Thread { key, .. } => active_order.push(key),
        }
    }
    Some(DropTarget {
        section,
        pinned_order,
        active_order,
    })
}

/// The inputs of [`plan_drop`] (`planSidebarThreadDrop`'s input,
/// Sidebar.logic.ts:369-383).
#[derive(Debug, Clone, Copy)]
pub struct DropInput<'a, K> {
    /// The dragged row's key.
    pub moved: &'a K,
    /// The dragged row's section.
    pub from: T3Section,
    /// The dragged thread is pinned (also true for a pinned snoozed thread).
    pub from_pinned: bool,
    /// The dragged thread is settled.
    pub from_settled: bool,
    /// The dragged thread's server supports settling.
    pub supports_settlement: bool,
    pub target: &'a DropTarget<K>,
    /// Pinned's order before the move.
    pub pinned_order: &'a [K],
    /// Pin order keys of keyed rows, hidden rows included.
    pub pinned_keys: &'a HashMap<K, String>,
    /// Rows whose server can store a pin order now (connected); `None`: all.
    pub pinned_writable: Option<&'a HashSet<K>>,
    /// Active's order before the move.
    pub active_order: &'a [K],
    /// Active order keys of keyed rows, hidden rows included.
    pub active_keys: &'a HashMap<K, String>,
    /// Rows whose server can store an active order now; `None`: all.
    pub active_writable: Option<&'a HashSet<K>>,
    /// [`T3Layout::active_time_ordered`].
    pub active_time_ordered: bool,
}

/// What a drop does (`SidebarThreadDropPlan`, Sidebar.logic.ts:283-304).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropPlan<K> {
    None,
    /// Rearrange Pinned: write `assignments` (`thread.pin.reorder`).
    ReorderPinned {
        order: Vec<K>,
        assignments: Vec<OrderAssignment<K>>,
    },
    /// Pin the row with `order_key` (`thread.pin`), and write the other
    /// rows' keys (`thread.pin.reorder`).
    Pin {
        order: Vec<K>,
        order_key: Option<String>,
        extra_assignments: Vec<OrderAssignment<K>>,
    },
    /// Into Active: unpin / unsettle / wake as flagged, then write
    /// `assignments` (`thread.active.reorder`). `order` is `None` when
    /// Active is time-ordered.
    MoveActive {
        order: Option<Vec<K>>,
        assignments: Vec<OrderAssignment<K>>,
        unpin: bool,
        unsettle: bool,
        unsnooze: bool,
    },
    /// Settle the row.
    Settle,
}

/// Plans a drop (`planSidebarThreadDrop`, Sidebar.logic.ts:369-497):
/// Working rows never move; reorders write one midpoint key when they can,
/// else rewrite the writable rows, and refuse a placement that a row whose
/// server cannot store order (a frozen keyless row) would undo.
pub fn plan_drop<K: Eq + Hash + Clone>(input: DropInput<'_, K>) -> DropPlan<K> {
    let DropInput {
        moved,
        from,
        from_pinned,
        from_settled,
        target,
        ..
    } = input;
    if from == T3Section::Working {
        return DropPlan::None;
    }
    if !input.supports_settlement && (target.section == DropSection::Settled || from_settled) {
        return DropPlan::None;
    }
    let arrange = |order: &[K], keys: &HashMap<K, String>, writable: Option<&HashSet<K>>| {
        let Some(writable) = writable else {
            return Some(plan_reorder(order, keys, moved));
        };
        if !writable.contains(moved) {
            return None;
        }
        let candidates: Vec<K> = order
            .iter()
            .filter(|key| writable.contains(*key) || keys.contains_key(*key))
            .cloned()
            .collect();
        let assignments = plan_reorder(&candidates, keys, moved);
        if assignments.iter().any(|a| !writable.contains(&a.id)) {
            return None;
        }
        // Keyless Active rows sort before keyed rows; keyless Pinned rows
        // after them. Materializing a row across a frozen keyless boundary
        // cannot persist that position.
        let assigned: HashSet<&K> = assignments.iter().map(|a| &a.id).collect();
        let mut frozen_before = false;
        let mut assigned_before = false;
        for key in order {
            let frozen = !writable.contains(key) && !keys.contains_key(key);
            let is_assigned = assigned.contains(key);
            if (target.section == DropSection::Active && frozen && assigned_before)
                || (target.section == DropSection::Pinned && is_assigned && frozen_before)
            {
                return None;
            }
            frozen_before |= frozen;
            assigned_before |= is_assigned;
        }
        Some(assignments)
    };
    match target.section {
        DropSection::Active => {
            if input.active_time_ordered {
                return if from == T3Section::Active {
                    DropPlan::None
                } else {
                    DropPlan::MoveActive {
                        order: None,
                        assignments: Vec::new(),
                        unpin: from_pinned,
                        unsettle: from_settled,
                        unsnooze: from == T3Section::Snoozed,
                    }
                };
            }
            let order = &target.active_order;
            if from == T3Section::Active && order.as_slice() == input.active_order {
                return DropPlan::None;
            }
            let Some(assignments) = arrange(order, input.active_keys, input.active_writable) else {
                return DropPlan::None;
            };
            DropPlan::MoveActive {
                order: Some(order.clone()),
                assignments,
                unpin: from_pinned,
                unsettle: from_settled,
                unsnooze: from == T3Section::Snoozed,
            }
        }
        DropSection::Settled if from == T3Section::Settled => DropPlan::None,
        DropSection::Settled => DropPlan::Settle,
        DropSection::Pinned => {
            let order = &target.pinned_order;
            if from == T3Section::Pinned && order.as_slice() == input.pinned_order {
                return DropPlan::None;
            }
            let Some(assignments) = arrange(order, input.pinned_keys, input.pinned_writable) else {
                return DropPlan::None;
            };
            if from == T3Section::Pinned {
                return if assignments.is_empty() {
                    DropPlan::None
                } else {
                    DropPlan::ReorderPinned {
                        order: order.clone(),
                        assignments,
                    }
                };
            }
            let order_key = assignments
                .iter()
                .find(|a| &a.id == moved)
                .map(|a| a.order_key.clone());
            let extra_assignments = if from_pinned {
                assignments
            } else {
                assignments.into_iter().filter(|a| &a.id != moved).collect()
            };
            DropPlan::Pin {
                order: order.clone(),
                order_key,
                extra_assignments,
            }
        }
    }
}

/// Where settle-and-advance goes after parking `current`: the next row
/// after it, wrapping around, that is not `parked` (settled, snoozed, or
/// being parked with it). `None` opens a new thread instead
/// (`planForwardNavigation`, Sidebar.tsx:1594-1616).
pub fn forward_target<'k, K: PartialEq>(
    ordered: &'k [K],
    current: &K,
    parked: impl Fn(&K) -> bool,
) -> Option<&'k K> {
    let index = ordered.iter().position(|key| key == current)?;
    ordered[index + 1..]
        .iter()
        .chain(&ordered[..index])
        .find(|key| !parked(key))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::workflow::{fixtures::shell, view::instant};

    fn at(iso: &str) -> OffsetDateTime {
        instant(iso).unwrap()
    }

    fn env<'a>(environment: &'a str, thread: &'a ThreadShell) -> EnvThread<'a> {
        EnvThread {
            environment,
            thread,
        }
    }

    fn ids(threads: &[EnvThread<'_>]) -> Vec<String> {
        threads
            .iter()
            .map(|entry| entry.thread.id.clone())
            .collect()
    }

    fn sorted(threads: &[ThreadShell], sort: fn(&mut [EnvThread<'_>])) -> Vec<String> {
        let mut entries: Vec<EnvThread<'_>> = threads.iter().map(|t| env("env", t)).collect();
        sort(&mut entries);
        ids(&entries)
    }

    // --- sortSettledThreads (threadSort.test.ts:61-104) ---

    #[test]
    fn a_turn_completion_counts_as_activity_for_auto_settled_threads() {
        // threadSort.test.ts:76-94
        let base = json!({ "updatedAt": "2026-03-09T09:00:00.000Z" });
        let mut message_only = base.clone();
        message_only["latestUserMessageAt"] = json!("2026-03-09T10:04:00.000Z");
        let mut completed = base;
        completed["latestUserMessageAt"] = json!("2026-03-09T10:00:00.000Z");
        completed["latestRunId"] = json!("run-1");
        completed["status"] = json!("completed");
        completed["latestRunRequestedAt"] = json!("2026-03-09T10:00:00.000Z");
        completed["latestRunStartedAt"] = json!("2026-03-09T10:00:00.000Z");
        completed["latestRunCompletedAt"] = json!("2026-03-09T10:30:00.000Z");
        let threads = [
            shell("message-only", message_only),
            shell("completed-later", completed),
        ];
        assert_eq!(
            sorted(&threads, sort_settled),
            ["completed-later", "message-only"]
        );
    }

    #[test]
    fn settled_ties_break_by_id() {
        // threadSort.test.ts:96-103
        let stamp = json!({ "settledAt": "2026-03-09T10:00:00.000Z" });
        let threads = [shell("b", stamp.clone()), shell("a", stamp)];
        assert_eq!(sorted(&threads, sort_settled), ["a", "b"]);
    }

    // --- sortActiveThreadsByOrderKey (threadSort.test.ts:236-304) ---

    #[test]
    fn new_and_reopened_threads_lead_the_saved_order() {
        // threadSort.test.ts:237-266
        let threads = [
            shell(
                "arranged-first",
                json!({ "createdAt": "2026-03-09T09:00:00.000Z", "activeOrderKey": "f" }),
            ),
            shell(
                "new",
                json!({ "createdAt": "2026-03-09T11:00:00.000Z", "activeOrderKey": null }),
            ),
            shell(
                "arranged-last",
                json!({ "createdAt": "2026-03-09T12:00:00.000Z", "unsettledAt": "2026-03-09T13:00:00.000Z", "activeOrderKey": "t" }),
            ),
            shell(
                "reopened",
                json!({ "createdAt": "2026-03-01T09:00:00.000Z", "unsettledAt": "2026-03-09T12:00:00.000Z" }),
            ),
        ];
        assert_eq!(
            sorted(&threads, sort_active),
            ["reopened", "new", "arranged-first", "arranged-last"]
        );
    }

    #[test]
    fn equal_keys_and_times_break_by_thread_then_environment() {
        // threadSort.test.ts:268-285
        for key in [json!(null), json!("m")] {
            let b = shell("thread-b", json!({ "activeOrderKey": key }));
            let a = shell("thread-a", json!({ "activeOrderKey": key }));
            let mut entries = vec![env("env-a", &b), env("env-b", &a), env("env-a", &a)];
            sort_active(&mut entries);
            let order: Vec<String> = entries
                .iter()
                .map(|entry| format!("{}:{}", entry.thread.id, entry.environment))
                .collect();
            assert_eq!(
                order,
                ["thread-a:env-a", "thread-a:env-b", "thread-b:env-a"]
            );
        }
    }

    #[test]
    fn materializes_a_large_active_list_in_the_requested_order() {
        // threadSort.test.ts:287-303
        let ids: Vec<String> = (0..1_200).map(|index| index.to_string()).collect();
        let ordered: Vec<String> = ids.iter().rev().cloned().collect();
        let assignments = plan_reorder(&ordered, &HashMap::new(), &ordered[0]);
        let keys: HashMap<String, String> = assignments
            .into_iter()
            .map(|a| (a.id, a.order_key))
            .collect();
        let threads: Vec<ThreadShell> = ids
            .iter()
            .map(|id| shell(id, json!({ "activeOrderKey": keys.get(id) })))
            .collect();
        assert_eq!(sorted(&threads, sort_active), ordered);
    }

    // threadListV2.test.ts:219-228
    #[test]
    fn honors_a_saved_active_order_and_leaves_new_threads_above_it() {
        let threads = [
            shell(
                "newer-arranged",
                json!({ "createdAt": "2026-06-01T12:00:00.000Z", "activeOrderKey": "t" }),
            ),
            shell(
                "older-arranged",
                json!({ "createdAt": "2026-06-01T08:00:00.000Z", "activeOrderKey": "f" }),
            ),
            shell("new", json!({ "createdAt": "2026-06-01T13:00:00.000Z" })),
        ];
        assert_eq!(
            sorted(&threads, sort_active),
            ["new", "older-arranged", "newer-arranged"]
        );
    }

    // --- sortPinnedThreadsForSidebar (Sidebar.logic.test.ts:906-937) ---

    #[test]
    fn keyed_pinned_threads_lead_keyless_ones_in_creation_order() {
        let threads = [
            shell(
                "keyless-old",
                json!({ "createdAt": "2026-03-09T08:00:00.000Z" }),
            ),
            shell(
                "second",
                json!({ "createdAt": "2026-03-09T09:00:00.000Z", "pinOrderKey": "t" }),
            ),
            shell(
                "keyless-new",
                json!({ "createdAt": "2026-03-09T12:00:00.000Z" }),
            ),
            shell(
                "first",
                json!({ "createdAt": "2026-03-09T07:00:00.000Z", "pinOrderKey": "g" }),
            ),
        ];
        assert_eq!(
            sorted(&threads, sort_pinned),
            ["first", "second", "keyless-new", "keyless-old"]
        );
    }

    #[test]
    fn equal_pin_keys_break_by_id() {
        let threads = [
            shell(
                "b",
                json!({ "createdAt": "2026-03-09T10:00:00.000Z", "pinOrderKey": "m" }),
            ),
            shell(
                "a",
                json!({ "createdAt": "2026-03-09T11:00:00.000Z", "pinOrderKey": "m" }),
            ),
        ];
        assert_eq!(sorted(&threads, sort_pinned), ["a", "b"]);
    }

    // --- sortInboxThreadsByReturn (Sidebar.logic.test.ts:1126-1175) ---

    fn inbox(
        id: &str,
        created_at: &str,
        completed_at: Option<Option<&str>>,
        unsettled_at: Option<&str>,
    ) -> ThreadShell {
        let mut row = json!({ "createdAt": created_at, "unsettledAt": unsettled_at });
        if let Some(completed_at) = completed_at {
            row["latestRunId"] = json!("turn-1");
            row["status"] = json!(if completed_at.is_some() {
                "completed"
            } else {
                "running"
            });
            row["latestRunRequestedAt"] = json!(created_at);
            row["latestRunCompletedAt"] = json!(completed_at);
        }
        shell(id, row)
    }

    #[test]
    fn the_thread_that_finished_last_tops_the_inbox() {
        // Sidebar.logic.test.ts:1141-1155
        let threads = [
            inbox("new", "2026-03-09T11:00:00.000Z", None, None),
            inbox(
                "old-finished-now",
                "2026-03-01T09:00:00.000Z",
                Some(Some("2026-03-09T12:00:00.000Z")),
                None,
            ),
            inbox(
                "reopened",
                "2026-03-02T09:00:00.000Z",
                None,
                Some("2026-03-09T11:30:00.000Z"),
            ),
        ];
        assert_eq!(
            sorted(&threads, sort_active_by_return),
            ["old-finished-now", "reopened", "new"]
        );
    }

    #[test]
    fn a_return_without_a_server_stamp_sorts_by_its_last_stamp() {
        // Sidebar.logic.test.ts:1157-1175 (j3 keeps no per-device return
        // observations, so only the stamped order applies)
        let threads = [
            inbox(
                "asks-approval",
                "2026-03-09T09:00:00.000Z",
                Some(None),
                None,
            ),
            inbox(
                "finished",
                "2026-03-09T09:30:00.000Z",
                Some(Some("2026-03-09T11:00:00.000Z")),
                None,
            ),
        ];
        assert_eq!(
            sorted(&threads, sort_active_by_return),
            ["finished", "asks-approval"]
        );
    }

    // --- sortWorkingThreadsBySend (threadInbox.test.ts:220-242) ---

    #[test]
    fn working_threads_order_by_the_last_message_sent() {
        let running = |id: &str, extra: serde_json::Value| {
            let mut row = json!({
                "createdAt": "2026-06-01T00:00:00.000Z",
                "activeProviderThreadId": "provider",
                "status": "running",
            });
            for (key, value) in extra.as_object().unwrap() {
                row[key] = value.clone();
            }
            shell(id, row)
        };
        let threads = [
            running("launched", json!({ "latestUserAuthoredMessageAt": null })),
            running(
                "sent-first",
                json!({
                    "latestUserAuthoredMessageAt": "2026-06-01T01:00:00.000Z",
                    "latestRunId": "run:wake",
                    "latestRunRequestedAt": "2026-06-01T04:00:00.000Z",
                    "latestRunStartedAt": "2026-06-01T04:00:00.000Z",
                    "latestRunCompletedAt": null,
                }),
            ),
            running(
                "sent-last",
                json!({ "latestUserAuthoredMessageAt": "2026-06-01T02:00:00.000Z" }),
            ),
        ];
        assert_eq!(
            sorted(&threads, sort_working),
            ["sent-last", "sent-first", "launched"]
        );
    }

    // --- sections (threadListV2.test.ts:230-394) ---

    const NOW: &str = "2026-06-02T00:00:00.000Z";

    fn layout_ids(threads: &[ThreadShell], now: &str, working_shelf: bool) -> [Vec<String>; 5] {
        let layout = t3_layout(
            threads.iter().map(|t| env("env", t)),
            at(now),
            working_shelf,
            |_| true,
        );
        [
            ids(&layout.pinned),
            ids(&layout.active),
            ids(&layout.working),
            ids(&layout.snoozed),
            ids(&layout.settled),
        ]
    }

    #[test]
    fn ordered_sections_exclude_settled_snoozed_and_archived_rows() {
        // threadListV2.test.ts:230-277
        let snooze = json!({ "snoozedUntil": "2026-06-03T10:00:00.000Z", "snoozedAt": NOW });
        let threads = [
            shell("active-later", json!({ "activeOrderKey": "t" })),
            shell("active-first", json!({ "activeOrderKey": "f" })),
            shell(
                "active-new",
                json!({ "createdAt": "2026-06-01T00:00:00.000Z" }),
            ),
            shell(
                "pinned-later",
                json!({ "pinnedAt": NOW, "pinOrderKey": "t", "activeOrderKey": "f" }),
            ),
            shell(
                "pinned-first",
                json!({ "pinnedAt": NOW, "pinOrderKey": "f", "activeOrderKey": "t" }),
            ),
            shell("settled", json!({ "settledOverride": "settled" })),
            shell("archived", json!({ "archivedAt": NOW })),
            shell("snoozed", snooze.clone()),
            shell("pinned-snoozed", {
                let mut row = snooze;
                row["pinnedAt"] = json!(NOW);
                row
            }),
        ];
        let [pinned, active, _, snoozed, settled] = layout_ids(&threads, NOW, false);
        assert_eq!(active, ["active-new", "active-first", "active-later"]);
        assert_eq!(pinned, ["pinned-first", "pinned-later"]);
        assert_eq!(snoozed, ["pinned-snoozed", "snoozed"]);
        assert_eq!(settled, ["settled"]);
    }

    #[test]
    fn a_settled_pinned_thread_goes_to_the_settled_shelf() {
        // threadListV2.test.ts:306-326
        let threads = [
            shell("active", json!({})),
            shell(
                "pinned-settled",
                json!({ "pinnedAt": "2026-06-01T12:00:00.000Z", "settledOverride": "settled", "settledAt": "2026-06-01T12:00:00.000Z" }),
            ),
        ];
        let [pinned, active, _, _, settled] = layout_ids(&threads, NOW, false);
        assert!(pinned.is_empty());
        assert_eq!(active, ["active"]);
        assert_eq!(settled, ["pinned-settled"]);
    }

    #[test]
    fn snooze_hides_a_pinned_thread_and_wake_restores_it() {
        // threadListV2.test.ts:280-304,328-352
        let threads = [
            shell("active", json!({})),
            shell(
                "pinned-snoozed",
                json!({ "pinnedAt": "2026-06-01T12:00:00.000Z", "snoozedUntil": "2026-06-03T09:00:00.000Z", "snoozedAt": "2026-06-01T11:00:00.000Z" }),
            ),
            shell(
                "woken",
                json!({ "snoozedUntil": "2026-06-01T18:00:00.000Z", "snoozedAt": "2026-06-01T12:00:00.000Z" }),
            ),
        ];
        let [pinned, active, _, snoozed, _] = layout_ids(&threads, NOW, false);
        assert!(pinned.is_empty());
        assert_eq!(active, ["active", "woken"]);
        assert_eq!(snoozed, ["pinned-snoozed"]);
        let [pinned, _, _, snoozed, _] = layout_ids(&threads, "2026-06-03T10:00:00.000Z", false);
        assert_eq!(pinned, ["pinned-snoozed"]);
        assert!(snoozed.is_empty());
    }

    #[test]
    fn snoozed_rows_wake_soonest_first_and_set_the_next_wake() {
        // threadListV2.test.ts:354-392
        let threads = [
            shell("active", json!({})),
            shell(
                "settled",
                json!({ "settledOverride": "settled", "settledAt": NOW }),
            ),
            shell(
                "later",
                json!({ "snoozedUntil": "2026-06-03T09:00:00.000Z", "snoozedAt": "2026-06-01T12:00:00.000Z" }),
            ),
            shell(
                "sooner",
                json!({ "snoozedUntil": "2026-06-02T09:00:00.000Z", "snoozedAt": "2026-06-01T12:00:00.000Z" }),
            ),
        ];
        let layout = t3_layout(threads.iter().map(|t| env("env", t)), at(NOW), true, |_| {
            true
        });
        assert_eq!(ids(&layout.snoozed), ["sooner", "later"]);
        assert_eq!(layout.next_wake_at, Some(at("2026-06-02T09:00:00.000Z")));
        assert!(layout.active_time_ordered);
    }

    #[test]
    fn subagents_and_side_chats_get_no_row() {
        // orchestrationV2.ts:161-163
        let subagent = shell(
            "subagent",
            json!({ "lineage": { "parentThreadId": "parent", "relationshipToParent": "subagent", "rootThreadId": "parent" } }),
        );
        let side_chat = shell(
            "side",
            json!({ "lineage": { "parentThreadId": "parent", "relationshipToParent": "fork", "rootThreadId": "parent", "sideChat": true } }),
        );
        let fork = shell(
            "fork",
            json!({ "lineage": { "parentThreadId": "parent", "relationshipToParent": "fork", "rootThreadId": "parent" } }),
        );
        assert_eq!(t3_section(&subagent, at(NOW), true), None);
        assert_eq!(t3_section(&side_chat, at(NOW), true), None);
        assert_eq!(t3_section(&fork, at(NOW), true), Some(T3Section::Active));
    }

    #[test]
    fn working_threads_fold_into_the_shelf_unless_disconnected() {
        // Sidebar.logic.test.ts:1110-1124
        let running = shell(
            "running",
            json!({ "latestRunId": "turn-1", "status": "running", "latestRunCompletedAt": "2026-03-09T10:05:00.000Z" }),
        );
        let shelve = |connected: bool, enabled: bool| {
            t3_layout([env("env-local", &running)], at(NOW), enabled, |_| {
                connected
            })
            .working
            .len()
                == 1
        };
        assert!(shelve(true, true));
        assert!(!shelve(false, true));
        assert!(!shelve(true, false));
        // A pinned working thread stays pinned (Sidebar.tsx:680-692).
        let mut pinned = running.clone();
        pinned.pinned_at = Some(Some(NOW.to_owned()));
        assert_eq!(t3_section(&pinned, at(NOW), true), Some(T3Section::Pinned));
    }

    // --- dragging (Sidebar.logic.test.ts:1177-1544) ---

    fn row(key: &'static str, section: T3Section) -> ListItem<&'static str> {
        ListItem::Thread { key, section }
    }

    fn items() -> Vec<ListItem<&'static str>> {
        vec![
            ListItem::Marker(ListMarker::PinnedHeader),
            row("p1", T3Section::Pinned),
            ListItem::Marker(ListMarker::PinnedDivider),
            row("a1", T3Section::Active),
            row("a2", T3Section::Active),
            ListItem::Marker(ListMarker::WorkingHeader),
            row("w1", T3Section::Working),
            ListItem::Marker(ListMarker::SettledHeader),
            row("s1", T3Section::Settled),
        ]
    }

    fn index(items: &[ListItem<&'static str>], key: &str) -> usize {
        items
            .iter()
            .position(|item| matches!(item, ListItem::Thread { key: k, .. } if *k == key))
            .unwrap()
    }

    fn key_map(entries: &[(&'static str, &str)]) -> HashMap<&'static str, String> {
        entries
            .iter()
            .map(|(id, key)| (*id, (*key).to_owned()))
            .collect()
    }

    #[test]
    fn never_drops_into_the_working_shelf() {
        // Sidebar.logic.test.ts:1196-1204
        let items = items();
        assert_eq!(
            drop_target(&items, index(&items, "a1"), index(&items, "w1")),
            None
        );
        assert_eq!(
            drop_target(&items, index(&items, "p1"), index(&items, "a2")),
            Some(DropTarget {
                section: DropSection::Active,
                pinned_order: vec![],
                active_order: vec!["a1", "a2", "p1"],
            })
        );
        assert_eq!(drop_verb(T3Section::Active, Some(T3Section::Working)), None);
    }

    #[test]
    fn drop_verbs() {
        // Sidebar.logic.ts:306-318
        use T3Section::*;
        assert_eq!(drop_verb(Active, Some(Pinned)), Some(DropVerb::Pin));
        assert_eq!(drop_verb(Pinned, Some(Settled)), Some(DropVerb::Settle));
        assert_eq!(drop_verb(Pinned, Some(Active)), Some(DropVerb::Unpin));
        assert_eq!(drop_verb(Settled, Some(Active)), Some(DropVerb::Unsettle));
        assert_eq!(drop_verb(Snoozed, Some(Active)), Some(DropVerb::Wake));
        assert_eq!(drop_verb(Active, Some(Snoozed)), None);
        assert_eq!(drop_verb(Active, Some(Active)), None);
        assert_eq!(drop_verb(Active, None), None);
    }

    fn base_input<'a>(
        moved: &'a &'static str,
        from: T3Section,
        target: &'a DropTarget<&'static str>,
        pinned_order: &'a [&'static str],
        pinned_keys: &'a HashMap<&'static str, String>,
        active_order: &'a [&'static str],
        active_keys: &'a HashMap<&'static str, String>,
    ) -> DropInput<'a, &'static str> {
        DropInput {
            moved,
            from,
            from_pinned: from == T3Section::Pinned,
            from_settled: from == T3Section::Settled,
            supports_settlement: true,
            target,
            pinned_order,
            pinned_keys,
            pinned_writable: None,
            active_order,
            active_keys,
            active_writable: None,
            active_time_ordered: false,
        }
    }

    #[test]
    fn a_working_thread_never_moves() {
        // Sidebar.logic.test.ts:1206-1232
        let items = items();
        let pinned_keys = key_map(&[("p1", "m")]);
        let active_keys = key_map(&[("a1", "f"), ("a2", "t")]);
        for (section, over) in [
            (DropSection::Pinned, "p1"),
            (DropSection::Active, "a2"),
            (DropSection::Settled, "s1"),
        ] {
            let target = drop_target(&items, index(&items, "w1"), index(&items, over)).unwrap();
            assert_eq!(target.section, section);
            let mut input = base_input(
                &"w1",
                T3Section::Working,
                &target,
                &["p1"],
                &pinned_keys,
                &["a1", "a2"],
                &active_keys,
            );
            input.active_time_ordered = true;
            assert_eq!(plan_drop(input), DropPlan::None);
        }
    }

    #[test]
    fn arranges_the_rows_it_can_write() {
        // Sidebar.logic.test.ts:1234-1273
        let target = DropTarget {
            section: DropSection::Active,
            pinned_order: vec![],
            active_order: vec!["offline", "a2", "a1"],
        };
        let none = HashMap::new();
        let writable: HashSet<&'static str> = ["a1", "a2"].into();
        let mut input = base_input(
            &"a2",
            T3Section::Active,
            &target,
            &[],
            &none,
            &["offline", "a1", "a2"],
            &none,
        );
        input.active_writable = Some(&writable);
        let DropPlan::MoveActive { assignments, .. } = plan_drop(input) else {
            panic!("expected move-active");
        };
        assert_eq!(
            assignments.iter().map(|a| a.id).collect::<Vec<_>>(),
            ["a2", "a1"]
        );
        assert!(assignments[0].order_key < assignments[1].order_key);

        let above = DropTarget {
            section: DropSection::Active,
            pinned_order: vec![],
            active_order: vec!["a2", "offline"],
        };
        let keys = key_map(&[("offline", "m"), ("a2", "t")]);
        let writable: HashSet<&'static str> = ["a2"].into();
        let mut input = base_input(
            &"a2",
            T3Section::Active,
            &above,
            &[],
            &none,
            &["offline", "a2"],
            &keys,
        );
        input.active_writable = Some(&writable);
        let DropPlan::MoveActive { assignments, .. } = plan_drop(input) else {
            panic!("expected move-active");
        };
        assert!(assignments[0].order_key.as_str() < "m");
    }

    /// `planDrop` (Sidebar.logic.test.ts:1302-1366): `rows` are
    /// (key, order key, writable, created hour); returns the plan, its
    /// writes, and the order those writes produce.
    fn plan_offline(
        section: DropSection,
        rows: &[(&'static str, Option<&str>, bool, u8)],
        moved: &'static str,
        order: &[&'static str],
        from: Option<T3Section>,
    ) -> (
        DropPlan<&'static str>,
        Vec<OrderAssignment<&'static str>>,
        Vec<String>,
    ) {
        let shell_of = |(key, order_key, _, hour): &(&'static str, Option<&str>, bool, u8)| {
            shell(
                key,
                json!({
                    "createdAt": format!("2026-03-09T{hour:02}:00:00.000Z"),
                    "activeOrderKey": order_key,
                    "pinOrderKey": order_key,
                }),
            )
        };
        let sort = |threads: &[ThreadShell]| {
            let mut entries: Vec<EnvThread<'_>> = threads.iter().map(|t| env("env", t)).collect();
            if section == DropSection::Active {
                sort_active(&mut entries);
            } else {
                sort_pinned(&mut entries);
            }
            ids(&entries)
        };
        let shells: Vec<ThreadShell> = rows.iter().map(shell_of).collect();
        let original: Vec<&'static str> = sort(&shells)
            .iter()
            .map(|id| rows.iter().find(|row| row.0 == id).unwrap().0)
            .collect();
        let keys: HashMap<&'static str, String> = rows
            .iter()
            .filter_map(|(key, order_key, _, _)| order_key.map(|k| (*key, k.to_owned())))
            .collect();
        let writable: HashSet<&'static str> =
            rows.iter().filter(|row| row.2).map(|row| row.0).collect();
        let target = DropTarget {
            section,
            pinned_order: order.to_vec(),
            active_order: order.to_vec(),
        };
        let from = from.unwrap_or(match section {
            DropSection::Active => T3Section::Active,
            DropSection::Pinned => T3Section::Pinned,
            DropSection::Settled => T3Section::Settled,
        });
        let mut input = base_input(&moved, from, &target, &original, &keys, &original, &keys);
        input.pinned_writable = Some(&writable);
        input.active_writable = Some(&writable);
        let plan = plan_drop(input);
        let assignments = match &plan {
            DropPlan::MoveActive { assignments, .. }
            | DropPlan::ReorderPinned { assignments, .. } => assignments.clone(),
            DropPlan::Pin {
                order_key,
                extra_assignments,
                ..
            } => {
                let mut all = extra_assignments.clone();
                if let Some(order_key) = order_key {
                    all.push(OrderAssignment {
                        id: moved,
                        order_key: order_key.clone(),
                    });
                }
                all
            }
            _ => Vec::new(),
        };
        let updated: Vec<ThreadShell> = rows
            .iter()
            .map(|row| {
                let assigned = assignments
                    .iter()
                    .find(|a| a.id == row.0)
                    .map(|a| a.order_key.as_str());
                shell_of(&(row.0, assigned.or(row.1), row.2, row.3))
            })
            .collect();
        (plan, assignments, sort(&updated))
    }

    fn reorder_kind(section: DropSection, plan: &DropPlan<&'static str>) -> bool {
        match section {
            DropSection::Active => matches!(plan, DropPlan::MoveActive { .. }),
            _ => matches!(plan, DropPlan::ReorderPinned { .. }),
        }
    }

    #[test]
    fn replans_a_disconnected_drag_source() {
        // Sidebar.logic.test.ts:1384-1399
        for section in [DropSection::Active, DropSection::Pinned] {
            let rows = [
                ("offline", Some("m"), false, 12),
                ("online", Some("t"), true, 11),
            ];
            let (plan, _, _) = plan_offline(section, &rows, "online", &["online", "offline"], None);
            assert!(reorder_kind(section, &plan));
            let disconnected = [
                ("offline", Some("m"), false, 12),
                ("online", Some("t"), false, 11),
            ];
            let (plan, assignments, _) = plan_offline(
                section,
                &disconnected,
                "online",
                &["online", "offline"],
                None,
            );
            assert_eq!(plan, DropPlan::None);
            assert!(assignments.is_empty());
        }
    }

    #[test]
    fn preserves_a_keyed_offline_bound() {
        // Sidebar.logic.test.ts:1401-1409
        for section in [DropSection::Active, DropSection::Pinned] {
            let rows = [
                ("offline", Some("m"), false, 12),
                ("online", Some("t"), true, 11),
            ];
            let order = ["online", "offline"];
            let (plan, assignments, actual) = plan_offline(section, &rows, "online", &order, None);
            assert!(reorder_kind(section, &plan));
            assert_eq!(
                assignments.iter().map(|a| a.id).collect::<Vec<_>>(),
                ["online"]
            );
            assert_eq!(actual, order);
        }
    }

    #[test]
    fn rejects_rebalancing_a_keyed_offline_bound() {
        // Sidebar.logic.test.ts:1411-1426
        for section in [DropSection::Active, DropSection::Pinned] {
            let rows = [
                ("offline", Some("m"), false, 12),
                ("online", None, true, 11),
                ("neighbor", None, true, 10),
            ];
            let (plan, assignments, _) = plan_offline(
                section,
                &rows,
                "online",
                &["offline", "neighbor", "online"],
                None,
            );
            assert_eq!(plan, DropPlan::None);
            assert!(assignments.is_empty());
        }
    }

    #[test]
    fn rejects_crossing_the_frozen_keyless_boundary() {
        // Sidebar.logic.test.ts:1428-1442
        for section in [DropSection::Active, DropSection::Pinned] {
            let offline_hour = if section == DropSection::Active {
                12
            } else {
                10
            };
            let rows = [
                ("offline", None, false, offline_hour),
                ("online", None, true, 11),
            ];
            let order: &[&str] = if section == DropSection::Active {
                &["online", "offline"]
            } else {
                &["offline", "online"]
            };
            let (plan, assignments, _) = plan_offline(section, &rows, "online", order, None);
            assert_eq!(plan, DropPlan::None);
            assert!(assignments.is_empty());
        }
    }

    #[test]
    fn allows_realizable_moves_beside_a_frozen_keyless_row() {
        // Sidebar.logic.test.ts:1444-1462
        for section in [DropSection::Active, DropSection::Pinned] {
            let rows = [
                ("offline", None, false, 12),
                ("online", None, true, 11),
                ("neighbor", None, true, 10),
            ];
            let order: &[&str] = if section == DropSection::Active {
                &["offline", "neighbor", "online"]
            } else {
                &["online", "neighbor", "offline"]
            };
            let (plan, assignments, actual) = plan_offline(section, &rows, "online", order, None);
            assert!(reorder_kind(section, &plan));
            assert!(!assignments.is_empty());
            assert!(assignments.iter().all(|a| a.id != "offline"));
            assert_eq!(actual, order);
        }
    }

    #[test]
    fn checks_fallback_assignments_at_the_boundary() {
        // Sidebar.logic.test.ts:1464-1479
        for section in [DropSection::Active, DropSection::Pinned] {
            let rows = [
                ("offline", None, false, 12),
                ("online", None, true, 11),
                ("neighbor", None, true, 10),
            ];
            let order: &[&str] = if section == DropSection::Active {
                &["neighbor", "offline", "online"]
            } else {
                &["online", "offline", "neighbor"]
            };
            let (plan, _, _) = plan_offline(section, &rows, "online", order, None);
            assert_eq!(plan, DropPlan::None);
        }
    }

    #[test]
    fn rejects_impossible_pin_or_unpin_placement_across_the_boundary() {
        // Sidebar.logic.test.ts:1481-1496
        for section in [DropSection::Active, DropSection::Pinned] {
            let rows = [("offline", None, false, 12), ("online", None, true, 11)];
            let (order, from): (&[&str], T3Section) = if section == DropSection::Active {
                (&["online", "offline"], T3Section::Pinned)
            } else {
                (&["offline", "online"], T3Section::Active)
            };
            let (plan, _, _) = plan_offline(section, &rows, "online", order, Some(from));
            assert_eq!(plan, DropPlan::None);
        }
    }

    #[test]
    fn only_changes_lifecycle_when_the_inbox_is_time_ordered() {
        // Sidebar.logic.test.ts:1499-1531
        let pinned_keys = key_map(&[("p1", "m")]);
        let active_keys = key_map(&[("a1", "f"), ("a2", "t")]);
        let reorder = DropTarget {
            section: DropSection::Active,
            pinned_order: vec!["p1"],
            active_order: vec!["a2", "a1"],
        };
        let mut input = base_input(
            &"a1",
            T3Section::Active,
            &reorder,
            &["p1"],
            &pinned_keys,
            &["a1", "a2"],
            &active_keys,
        );
        input.active_time_ordered = true;
        assert_eq!(plan_drop(input), DropPlan::None);
        let unpin = DropTarget {
            section: DropSection::Active,
            pinned_order: vec![],
            active_order: vec!["a1", "p1", "a2"],
        };
        let mut input = base_input(
            &"p1",
            T3Section::Pinned,
            &unpin,
            &["p1"],
            &pinned_keys,
            &["a1", "a2"],
            &active_keys,
        );
        input.active_time_ordered = true;
        assert_eq!(
            plan_drop(input),
            DropPlan::MoveActive {
                order: None,
                assignments: vec![],
                unpin: true,
                unsettle: false,
                unsnooze: false,
            }
        );
    }

    #[test]
    fn settling_drops_and_unsupported_servers() {
        // Sidebar.logic.ts:397-400,469-470
        let target = DropTarget {
            section: DropSection::Settled,
            pinned_order: vec![],
            active_order: vec![],
        };
        let none = HashMap::new();
        let input = base_input(&"a1", T3Section::Active, &target, &[], &none, &[], &none);
        assert_eq!(plan_drop(input), DropPlan::Settle);
        let mut input = base_input(&"a1", T3Section::Active, &target, &[], &none, &[], &none);
        input.supports_settlement = false;
        assert_eq!(plan_drop(input), DropPlan::None);
        let input = base_input(&"s1", T3Section::Settled, &target, &[], &none, &[], &none);
        assert_eq!(plan_drop(input), DropPlan::None);
    }

    #[test]
    fn forward_navigation_skips_parked_rows_and_wraps() {
        // Sidebar.tsx:1603-1609
        let ordered = ["a", "b", "settled", "c"];
        let parked = |key: &&str| *key == "settled" || *key == "b";
        assert_eq!(forward_target(&ordered, &"a", parked), Some(&"c"));
        assert_eq!(forward_target(&ordered, &"c", parked), Some(&"a"));
        assert_eq!(forward_target(&["a"], &"a", |_| false), None);
        assert_eq!(forward_target(&ordered, &"gone", parked), None);
    }
}
