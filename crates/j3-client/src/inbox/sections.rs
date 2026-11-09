//! Sections, their order, drag and drop, and forward navigation
//! (`docs/triage.md` §5, §6.3, §6.4, with §12.1).

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use time::OffsetDateTime;

use super::order::{OrderAssignment, plan_reorder};
use super::status::{is_snoozed, is_working, parse_time, settled_time};
use crate::EnvThread;
use crate::entities::{Settled, Thread};

/// Where a listed thread sits in the Inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Pinned,
    Active,
    Working,
    Snoozed,
    Settled,
}

/// The thread's section (triage §5.2), first match wins: effectively snoozed
/// → Snoozed; settled → Settled; pinned → Pinned; working while the Working
/// shelf is on and its environment is `connected` → Working; else Active.
///
/// So a snoozed pinned thread returns to Pinned on wake, a settled pinned
/// thread is in Settled, and a working pinned thread stays in Pinned.
pub fn section(
    thread: &Thread,
    now: OffsetDateTime,
    working_shelf: bool,
    connected: bool,
) -> Section {
    if is_snoozed(thread, now) {
        Section::Snoozed
    } else if thread.triage.settled == Some(Settled::Settled) {
        Section::Settled
    } else if thread.triage.pinned_at.is_some() {
        Section::Pinned
    } else if working_shelf && connected && is_working(thread) {
        Section::Working
    } else {
        Section::Active
    }
}

/// The thread gets a row: it is not archived.
pub fn is_listed(thread: &Thread) -> bool {
    thread.archived_at.is_none()
}

/// The Inbox's sections, each in display order.
#[derive(Debug, Clone)]
pub struct Layout<'a> {
    pub pinned: Vec<EnvThread<'a>>,
    pub active: Vec<EnvThread<'a>>,
    pub working: Vec<EnvThread<'a>>,
    pub snoozed: Vec<EnvThread<'a>>,
    pub settled: Vec<EnvThread<'a>>,
    /// Active sorted by return time (working shelf on) rather than by keys.
    pub active_time_ordered: bool,
    /// Earliest snoozed_until still in the future across the listed threads (§5.6).
    pub next_wake_at: Option<OffsetDateTime>,
}

/// The latest parseable time among `values`.
fn latest<'v>(values: impl IntoIterator<Item = Option<&'v str>>) -> Option<OffsetDateTime> {
    values.into_iter().map(parse_time).max().flatten()
}

fn key(value: Option<&str>) -> Option<&str> {
    value.filter(|key| !key.is_empty())
}

/// Lays out the listed threads (triage §5.3, with §12.1(6)):
///
/// - Pinned: keyed rows by pin key, then keyless rows by `pinned_at`, newest first.
/// - Active, time ordered (`working_shelf` on): newest return first, where
///   return = latest of creation, unsettle and the latest turn's request and end.
/// - Active, manually ordered: keyless rows first, newest of creation and
///   unsettle first, then keyed rows by active key.
/// - Working: newest of creation and the last user send first.
/// - Snoozed: soonest wake first.
/// - Settled: most recently settled first ([`settled_time`]).
///
/// Missing or unparseable times sort as oldest. Ties break by thread id,
/// then environment.
pub fn layout<'a>(
    threads: Vec<EnvThread<'a>>,
    now: OffsetDateTime,
    working_shelf: bool,
    connected: impl Fn(&str) -> bool,
) -> Layout<'a> {
    let mut layout = Layout {
        pinned: Vec::new(),
        active: Vec::new(),
        working: Vec::new(),
        snoozed: Vec::new(),
        settled: Vec::new(),
        active_time_ordered: working_shelf,
        next_wake_at: None,
    };
    for row in threads {
        let thread = row.thread;
        if !is_listed(thread) {
            continue;
        }
        if let Some(until) =
            parse_time(thread.triage.snoozed_until.as_deref()).filter(|until| *until > now)
        {
            layout.next_wake_at = Some(layout.next_wake_at.map_or(until, |next| next.min(until)));
        }
        let rows = match section(thread, now, working_shelf, connected(row.environment)) {
            Section::Pinned => &mut layout.pinned,
            Section::Active => &mut layout.active,
            Section::Working => &mut layout.working,
            Section::Snoozed => &mut layout.snoozed,
            Section::Settled => &mut layout.settled,
        };
        rows.push(row);
    }

    layout.pinned.sort_by_cached_key(|&row| {
        let triage = &row.thread.triage;
        let pin = key(triage.pin_order.as_deref());
        let pinned_at = if pin.is_some() {
            None
        } else {
            parse_time(triage.pinned_at.as_deref())
        };
        (pin.is_none(), pin, Reverse(pinned_at), tie(row))
    });
    if working_shelf {
        layout.active.sort_by_cached_key(|&row| {
            let thread = row.thread;
            let turn = thread.latest_turn.as_ref();
            let returned = latest([
                Some(thread.created_at.as_str()),
                thread.triage.unsettled_at.as_deref(),
                turn.map(|turn| turn.requested_at.as_str()),
                turn.and_then(|turn| turn.ended_at.as_deref()),
            ]);
            (Reverse(returned), tie(row))
        });
    } else {
        layout.active.sort_by_cached_key(|&row| {
            let thread = row.thread;
            let active = key(thread.triage.active_order.as_deref());
            let anchor = if active.is_some() {
                None
            } else {
                latest([
                    Some(thread.created_at.as_str()),
                    thread.triage.unsettled_at.as_deref(),
                ])
            };
            (active.is_some(), Reverse(anchor), active, tie(row))
        });
    }
    layout.working.sort_by_cached_key(|&row| {
        let thread = row.thread;
        let sent = latest([
            Some(thread.created_at.as_str()),
            thread.latest_user_message_at.as_deref(),
            thread
                .latest_turn
                .as_ref()
                .map(|turn| turn.requested_at.as_str()),
        ]);
        (Reverse(sent), tie(row))
    });
    layout.snoozed.sort_by_cached_key(|&row| {
        (
            parse_time(row.thread.triage.snoozed_until.as_deref()),
            tie(row),
        )
    });
    layout
        .settled
        .sort_by_cached_key(|&row| (Reverse(settled_time(row.thread)), tie(row)));
    layout
}

fn tie(row: EnvThread<'_>) -> (&str, &str) {
    (row.thread.id.as_str(), row.environment)
}

/// A non-row entry of the rendered list (triage §5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListMarker {
    PinnedHeader,
    PinnedDivider,
    ActivePlaceholder,
    WorkingHeader,
    SnoozedHeader,
    SettledHeader,
    SettledPlaceholder,
}

/// One entry of the rendered list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListItem<K> {
    Thread { key: K, section: Section },
    Marker(ListMarker),
}

/// The sections a row can be dropped into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DropSection {
    Pinned,
    Active,
    Settled,
}

/// Where a dragged row lands, with the Pinned and Active rows after the move.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DropTarget<K> {
    pub section: DropSection,
    pub pinned_order: Vec<K>,
    pub active_order: Vec<K>,
}

/// The section that starts after a marker, if the marker starts one.
fn marker_section(marker: ListMarker) -> Option<Section> {
    match marker {
        ListMarker::PinnedDivider => Some(Section::Active),
        ListMarker::WorkingHeader => Some(Section::Working),
        ListMarker::SnoozedHeader => Some(Section::Snoozed),
        ListMarker::SettledHeader => Some(Section::Settled),
        ListMarker::PinnedHeader
        | ListMarker::ActivePlaceholder
        | ListMarker::SettledPlaceholder => None,
    }
}

/// The drop target for moving `items[moved]` to slot `over` (triage §6.4):
/// `over` indexes the list with the moved row taken out and re-inserted
/// there. A slot belongs to the section of the last marker before it (the
/// list starts in Pinned; the Pinned divider starts Active; each shelf header
/// starts its shelf).
///
/// None when `moved` is not a row, `over` is out of range, or the slot is in
/// Working or Snoozed: nothing drops there. The target's orders are the rows
/// before the Pinned divider (pinned) and between it and the first shelf
/// header (active), after the move.
pub fn drop_target<K: Clone>(
    items: &[ListItem<K>],
    moved: usize,
    over: usize,
) -> Option<DropTarget<K>> {
    let dragged = items.get(moved)?;
    if !matches!(dragged, ListItem::Thread { .. }) {
        return None;
    }
    if over >= items.len() {
        return None;
    }
    let mut list: Vec<&ListItem<K>> = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| (index != moved).then_some(item))
        .collect();
    list.insert(over, dragged);

    let mut current = Section::Pinned;
    let mut slot = Section::Pinned;
    let mut pinned_order = Vec::new();
    let mut active_order = Vec::new();
    for (index, item) in list.into_iter().enumerate() {
        match item {
            ListItem::Marker(marker) => current = marker_section(*marker).unwrap_or(current),
            ListItem::Thread { key, .. } => {
                if index == over {
                    slot = current;
                }
                match current {
                    Section::Pinned => pinned_order.push(key.clone()),
                    Section::Active => active_order.push(key.clone()),
                    Section::Working | Section::Snoozed | Section::Settled => {}
                }
            }
        }
    }
    let section = match slot {
        Section::Pinned => DropSection::Pinned,
        Section::Active => DropSection::Active,
        Section::Settled => DropSection::Settled,
        Section::Working | Section::Snoozed => return None,
    };
    Some(DropTarget {
        section,
        pinned_order,
        active_order,
    })
}

/// The lifecycle change a drop implies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DropVerb {
    Pin,
    Unpin,
    Settle,
    Unsettle,
    Wake,
}

/// The verb for a drop from `from` into `to`: into Pinned pins, into Settled
/// settles; into Active, from Pinned unpins, from Settled unsettles and from
/// Snoozed wakes. None for a same-section move or a drop into Working or
/// Snoozed (`to` None).
pub fn drop_verb(from: Section, to: Option<Section>) -> Option<DropVerb> {
    match (from, to?) {
        (from, to) if from == to => None,
        (_, Section::Pinned) => Some(DropVerb::Pin),
        (_, Section::Settled) => Some(DropVerb::Settle),
        (Section::Pinned, Section::Active) => Some(DropVerb::Unpin),
        (Section::Settled, Section::Active) => Some(DropVerb::Unsettle),
        (Section::Snoozed, Section::Active) => Some(DropVerb::Wake),
        _ => None,
    }
}

/// Everything [`plan_drop`] needs.
///
/// `*_order` are the sections' visible rows before the move; `*_keys` hold
/// the current keys of every keyed row of the section, hidden ones included.
/// `*_writable` are the rows of that section whose server can store order
/// (after the move, so including the moved row where it lands); `None` means
/// all of them.
#[derive(Debug, Clone, Copy)]
pub struct DropInput<'a, K> {
    pub moved: &'a K,
    pub from: Section,
    /// The moved row is pinned (it may be shown in Snoozed).
    pub from_pinned: bool,
    /// The moved row is settled.
    pub from_settled: bool,
    /// Its server can settle and unsettle.
    pub supports_settlement: bool,
    pub target: &'a DropTarget<K>,
    pub pinned_order: &'a [K],
    pub pinned_keys: &'a HashMap<K, String>,
    pub pinned_writable: Option<&'a HashSet<K>>,
    pub active_order: &'a [K],
    pub active_keys: &'a HashMap<K, String>,
    pub active_writable: Option<&'a HashSet<K>>,
    pub active_time_ordered: bool,
}

/// The commands a drop sends.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DropPlan<K> {
    /// Nothing to do, or the drop is refused.
    None,
    /// Pin reorders within Pinned.
    ReorderPinned {
        assignments: Vec<OrderAssignment<K>>,
    },
    /// Pin the row with `order` (None: no key), then reorder the other pins.
    Pin {
        order: Option<String>,
        extra: Vec<OrderAssignment<K>>,
    },
    /// Into Active: unpin, unsettle, wake (in that order, where set), then
    /// the active reorders.
    MoveActive {
        assignments: Vec<OrderAssignment<K>>,
        unpin: bool,
        unsettle: bool,
        wake: bool,
    },
    /// Settle the row.
    Settle,
}

/// Which side of a frozen (unwritable, keyless) row a key may not be written.
#[derive(Clone, Copy)]
enum Frozen {
    /// Active: keyless rows sort first, so no key above a frozen row.
    KeysBelow,
    /// Pinned: keyed rows sort first, so no key below a frozen row.
    KeysAbove,
}

fn writable<K: Eq + Hash>(set: Option<&HashSet<K>>, id: &K) -> bool {
    set.is_none_or(|set| set.contains(id))
}

/// The reorder plan for one section with its unwritable rows: frozen keyless
/// rows keep no key and are left out of planning, keyed ones stay as bounds.
/// None when a write would hit an unwritable row or cross a frozen row.
fn plan_section<K: Eq + Hash + Clone>(
    order: &[K],
    keys: &HashMap<K, String>,
    writable_rows: Option<&HashSet<K>>,
    moved: &K,
    frozen_rule: Frozen,
) -> Option<Vec<OrderAssignment<K>>> {
    let frozen: Vec<usize> = order
        .iter()
        .enumerate()
        .filter(|(_, id)| !writable(writable_rows, id) && !keys.contains_key(id))
        .map(|(index, _)| index)
        .collect();
    let assignments = if frozen.is_empty() {
        plan_reorder(order, keys, moved)
    } else {
        let plannable: Vec<K> = order
            .iter()
            .enumerate()
            .filter(|(index, _)| !frozen.contains(index))
            .map(|(_, id)| id.clone())
            .collect();
        plan_reorder(&plannable, keys, moved)
    };
    for assignment in &assignments {
        if !writable(writable_rows, &assignment.id) {
            return None;
        }
        let index = order.iter().position(|id| *id == assignment.id)?;
        let crosses = match frozen_rule {
            Frozen::KeysBelow => frozen.iter().any(|&row| index < row),
            Frozen::KeysAbove => frozen.iter().any(|&row| index > row),
        };
        if crosses {
            return None;
        }
    }
    Some(assignments)
}

/// The commands for a drop (triage §6.4):
///
/// - A Working row never moves.
/// - Into Settled: settle, unless already settled or the server cannot settle.
/// - Into Pinned from Pinned: the reorder plan of the new Pinned order, if it changed.
/// - Into Pinned from elsewhere: pin with the row's planned key, then the
///   other rows' reorders.
/// - Into Active, time ordered: lifecycle only (unpin, unsettle, wake), no
///   keys; a no-op from Active.
/// - Into Active, manually ordered: lifecycle plus the active reorders; a
///   no-op from Active when the order is unchanged.
/// - Unwritable rows: refused when the moved row is unwritable, when a write
///   hits an unwritable row, or when a key would be written above a frozen
///   keyless Active row or below a frozen keyless Pinned row.
/// - Drops into or out of Settled are refused on servers without settlement.
pub fn plan_drop<K: Eq + Hash + Clone>(input: DropInput<'_, K>) -> DropPlan<K> {
    let DropInput {
        moved,
        from,
        from_pinned,
        from_settled,
        supports_settlement,
        target,
        ..
    } = input;
    if from == Section::Working {
        return DropPlan::None;
    }
    if target.section == DropSection::Settled {
        return if from_settled || !supports_settlement {
            DropPlan::None
        } else {
            DropPlan::Settle
        };
    }
    if from_settled && !supports_settlement {
        return DropPlan::None;
    }
    match target.section {
        DropSection::Pinned => {
            let order = &target.pinned_order;
            if !order.contains(moved) || !writable(input.pinned_writable, moved) {
                return DropPlan::None;
            }
            if from == Section::Pinned && order.as_slice() == input.pinned_order {
                return DropPlan::None;
            }
            let Some(assignments) = plan_section(
                order,
                input.pinned_keys,
                input.pinned_writable,
                moved,
                Frozen::KeysAbove,
            ) else {
                return DropPlan::None;
            };
            if from == Section::Pinned {
                return if assignments.is_empty() {
                    DropPlan::None
                } else {
                    DropPlan::ReorderPinned { assignments }
                };
            }
            let (own, extra): (Vec<_>, Vec<_>) =
                assignments.into_iter().partition(|a| a.id == *moved);
            DropPlan::Pin {
                order: own.into_iter().next().map(|a| a.order),
                extra,
            }
        }
        DropSection::Active => {
            let order = &target.active_order;
            if !order.contains(moved) || !writable(input.active_writable, moved) {
                return DropPlan::None;
            }
            let from_active = from == Section::Active;
            if from_active && (input.active_time_ordered || order.as_slice() == input.active_order)
            {
                return DropPlan::None;
            }
            let assignments = if input.active_time_ordered {
                Vec::new()
            } else {
                let Some(assignments) = plan_section(
                    order,
                    input.active_keys,
                    input.active_writable,
                    moved,
                    Frozen::KeysBelow,
                ) else {
                    return DropPlan::None;
                };
                if from_active && assignments.is_empty() {
                    return DropPlan::None;
                }
                assignments
            };
            DropPlan::MoveActive {
                assignments,
                unpin: from_pinned,
                unsettle: from_settled,
                wake: from == Section::Snoozed,
            }
        }
        DropSection::Settled => DropPlan::None,
    }
}

/// The row to open after settling `current` (triage §6.3, settle-and-advance):
/// the next row after it in `ordered`, wrapping, that is neither `current`
/// nor `parked`. None when `current` is not in `ordered` or nothing qualifies.
pub fn forward_target<'k, K: PartialEq>(
    ordered: &'k [K],
    current: &K,
    parked: impl Fn(&K) -> bool,
) -> Option<&'k K> {
    let index = ordered.iter().position(|id| id == current)?;
    ordered[index + 1..]
        .iter()
        .chain(&ordered[..index])
        .find(|id| *id != current && !parked(id))
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::inbox::fixtures::{at, base, min, request, thread, turn};
    use crate::inbox::order::spread_orders;

    fn rows(threads: &[Thread]) -> Vec<EnvThread<'_>> {
        threads
            .iter()
            .map(|thread| EnvThread {
                environment: "e",
                thread,
            })
            .collect()
    }

    fn ids<'a>(rows: &[EnvThread<'a>]) -> Vec<&'a str> {
        rows.iter().map(|row| row.thread.id.as_str()).collect()
    }

    fn lay(threads: &[Thread], working_shelf: bool) -> Layout<'_> {
        layout(rows(threads), base(), working_shelf, |_| true)
    }

    #[test]
    fn archived_threads_get_no_row() {
        let threads = [
            thread("a", json!({"archivedAt": min(-1)})),
            thread("b", json!({})),
        ];
        assert!(!is_listed(&threads[0]));
        let layout = lay(&threads, true);
        assert_eq!(ids(&layout.active), ["b"]);
    }

    #[test]
    fn ordered_sections_exclude_settled_snoozed_and_archived_rows() {
        let threads = [
            thread("active", json!({})),
            thread(
                "settled",
                json!({"triage": {"settled": "settled", "settledAt": min(-1)}}),
            ),
            thread("snoozed", json!({"triage": {"snoozedUntil": min(60)}})),
            thread("archived", json!({"archivedAt": min(-1)})),
            thread("pinned", json!({"triage": {"pinnedAt": min(-1)}})),
            thread(
                "unsettled",
                json!({"triage": {"settled": "active", "unsettledAt": min(-1)}}),
            ),
        ];
        let layout = lay(&threads, true);
        assert_eq!(ids(&layout.pinned), ["pinned"]);
        assert_eq!(ids(&layout.active), ["unsettled", "active"]);
        assert_eq!(ids(&layout.snoozed), ["snoozed"]);
        assert_eq!(ids(&layout.settled), ["settled"]);
        assert!(layout.working.is_empty());
    }

    #[test]
    fn a_settled_pinned_thread_goes_to_the_settled_shelf() {
        let row = thread(
            "a",
            json!({"triage": {"pinnedAt": min(-5), "settled": "settled"}}),
        );
        assert_eq!(section(&row, base(), true, true), Section::Settled);
    }

    #[test]
    fn snooze_hides_a_pinned_thread_and_wake_restores_it() {
        let row = thread(
            "a",
            json!({"triage": {"pinnedAt": min(-5), "snoozedUntil": min(60), "snoozedAt": min(-1)}}),
        );
        assert_eq!(section(&row, base(), true, true), Section::Snoozed);
        assert_eq!(section(&row, at(61), true, true), Section::Pinned);
        let woken = thread(
            "a",
            json!({"pendingRequest": request(0), "triage": {"pinnedAt": min(-5), "snoozedUntil": min(60)}}),
        );
        assert_eq!(
            section(&woken, base(), true, true),
            Section::Pinned,
            "a raised hand wakes it"
        );
    }

    #[test]
    fn snoozed_rows_wake_soonest_first_and_set_the_next_wake() {
        let threads = [
            thread("late", json!({"triage": {"snoozedUntil": min(120)}})),
            thread("soon", json!({"triage": {"snoozedUntil": min(30)}})),
            thread("passed", json!({"triage": {"snoozedUntil": min(-30)}})),
            thread(
                "hidden",
                json!({"archivedAt": min(-1), "triage": {"snoozedUntil": min(10)}}),
            ),
        ];
        let layout = lay(&threads, true);
        assert_eq!(ids(&layout.snoozed), ["soon", "late"]);
        assert_eq!(layout.next_wake_at, Some(at(30)));
        assert_eq!(ids(&layout.active), ["passed"]);
        assert_eq!(lay(&threads[2..], true).next_wake_at, None);
    }

    #[test]
    fn working_threads_fold_into_the_shelf_unless_disconnected() {
        let running = thread(
            "a",
            json!({"status": "running", "latestTurn": turn("running", -1, None)}),
        );
        assert_eq!(section(&running, base(), true, true), Section::Working);
        assert_eq!(section(&running, base(), true, false), Section::Active);
        assert_eq!(section(&running, base(), false, true), Section::Active);

        let pinned = thread(
            "b",
            json!({"status": "running", "triage": {"pinnedAt": min(-1)}}),
        );
        assert_eq!(section(&pinned, base(), true, true), Section::Pinned);
        let failed = thread("c", json!({"status": "failed"}));
        assert_eq!(section(&failed, base(), true, true), Section::Active);
        let input = thread(
            "d",
            json!({"status": "waiting", "pendingRequest": request(-1)}),
        );
        assert_eq!(section(&input, base(), true, true), Section::Active);

        let threads = [running.clone()];
        let offline = [EnvThread {
            environment: "off",
            thread: &threads[0],
        }];
        let layout = layout(offline.to_vec(), base(), true, |environment| {
            environment != "off"
        });
        assert_eq!(ids(&layout.active), ["a"]);
    }

    #[test]
    fn keyed_pinned_threads_lead_keyless_ones_newest_pin_first() {
        let threads = [
            thread(
                "old-pin",
                json!({"createdAt": min(-1), "triage": {"pinnedAt": min(-10)}}),
            ),
            thread(
                "keyed-m",
                json!({"triage": {"pinnedAt": min(-1), "pinOrder": "m"}}),
            ),
            thread(
                "new-pin",
                json!({"createdAt": min(-500), "triage": {"pinnedAt": min(-5)}}),
            ),
            thread(
                "keyed-c",
                json!({"triage": {"pinnedAt": min(-2), "pinOrder": "c"}}),
            ),
        ];
        assert_eq!(
            ids(&lay(&threads, true).pinned),
            ["keyed-c", "keyed-m", "new-pin", "old-pin"]
        );
    }

    #[test]
    fn equal_pin_keys_break_by_id() {
        let threads = [
            thread(
                "b",
                json!({"triage": {"pinnedAt": min(-1), "pinOrder": "n"}}),
            ),
            thread(
                "a",
                json!({"triage": {"pinnedAt": min(-2), "pinOrder": "n"}}),
            ),
        ];
        assert_eq!(ids(&lay(&threads, true).pinned), ["a", "b"]);
    }

    #[test]
    fn new_and_reopened_threads_lead_the_saved_order() {
        let threads = [
            thread(
                "keyed",
                json!({"createdAt": min(-1), "triage": {"activeOrder": "n"}}),
            ),
            thread("old", json!({"createdAt": min(-100)})),
            thread(
                "reopened",
                json!({"createdAt": min(-1000), "triage": {"settled": "active", "unsettledAt": min(-10)}}),
            ),
            thread("new", json!({"createdAt": min(-5)})),
        ];
        let layout = lay(&threads, false);
        assert!(!layout.active_time_ordered);
        assert_eq!(ids(&layout.active), ["new", "reopened", "old", "keyed"]);
    }

    #[test]
    fn honors_a_saved_active_order_and_leaves_new_threads_above_it() {
        let threads = [
            thread("x", json!({"triage": {"activeOrder": "t"}})),
            thread("y", json!({"triage": {"activeOrder": "c"}})),
            thread("fresh", json!({"createdAt": min(-1)})),
            thread("z", json!({"triage": {"activeOrder": "m"}})),
        ];
        assert_eq!(ids(&lay(&threads, false).active), ["fresh", "y", "z", "x"]);
    }

    #[test]
    fn equal_keys_and_times_break_by_thread_then_environment() {
        let threads = [
            thread("b", json!({"triage": {"activeOrder": "n"}})),
            thread("a", json!({"triage": {"activeOrder": "n"}})),
            thread("d", json!({})),
            thread("c", json!({})),
        ];
        let manual = lay(&threads, false);
        assert_eq!(ids(&manual.active), ["c", "d", "a", "b"]);

        let same = thread("a", json!({}));
        let envs = vec![
            EnvThread {
                environment: "e2",
                thread: &same,
            },
            EnvThread {
                environment: "e1",
                thread: &same,
            },
        ];
        let layout = layout(envs, base(), true, |_| true);
        let order: Vec<&str> = layout.active.iter().map(|row| row.environment).collect();
        assert_eq!(order, ["e1", "e2"]);
    }

    #[test]
    fn the_thread_that_finished_last_tops_the_inbox() {
        let threads = [
            thread(
                "newer",
                json!({"createdAt": min(-5), "triage": {"activeOrder": "b"}}),
            ),
            thread(
                "finished",
                json!({"createdAt": min(-1000), "latestTurn": turn("completed", -30, Some(-1))}),
            ),
            thread(
                "reopened",
                json!({"createdAt": min(-1000), "triage": {"settled": "active", "unsettledAt": min(-3)}}),
            ),
        ];
        let layout = lay(&threads, true);
        assert!(layout.active_time_ordered);
        assert_eq!(ids(&layout.active), ["finished", "reopened", "newer"]);
    }

    #[test]
    fn a_return_without_a_completion_counts_its_request_time() {
        let threads = [
            thread(
                "older",
                json!({"latestTurn": turn("completed", -30, Some(-10))}),
            ),
            // Running and disconnected, so in Active with only a request time.
            thread(
                "running",
                json!({"status": "running", "latestTurn": turn("running", -2, None)}),
            ),
        ];
        let layout = layout(rows(&threads), base(), true, |_| false);
        assert_eq!(ids(&layout.active), ["running", "older"]);
    }

    #[test]
    fn working_threads_order_by_the_last_message_sent() {
        let running = |sent: Value, created: i64| json!({"status": "running", "createdAt": min(created), "latestUserMessageAt": sent});
        let threads = [
            thread("early", running(json!(min(-20)), -1000)),
            thread("late", running(json!(min(-2)), -1000)),
            thread("unsent", running(Value::Null, -10)),
        ];
        assert_eq!(
            ids(&lay(&threads, true).working),
            ["late", "unsent", "early"]
        );
    }

    #[test]
    fn a_turn_completion_counts_as_activity_for_settled_threads() {
        let threads = [
            thread(
                "stamped",
                json!({"triage": {"settled": "settled", "settledAt": min(-10)}}),
            ),
            thread(
                "auto",
                json!({"latestTurn": turn("completed", -30, Some(-5)), "triage": {"settled": "settled"}}),
            ),
        ];
        assert_eq!(ids(&lay(&threads, true).settled), ["auto", "stamped"]);
    }

    #[test]
    fn settled_ties_break_by_id() {
        let threads = [
            thread(
                "b",
                json!({"triage": {"settled": "settled", "settledAt": min(-10)}}),
            ),
            thread(
                "a",
                json!({"triage": {"settled": "settled", "settledAt": min(-10)}}),
            ),
        ];
        assert_eq!(ids(&lay(&threads, true).settled), ["a", "b"]);
    }

    fn row(key: u32, section: Section) -> ListItem<u32> {
        ListItem::Thread { key, section }
    }

    fn list() -> Vec<ListItem<u32>> {
        vec![
            ListItem::Marker(ListMarker::PinnedHeader),
            row(1, Section::Pinned),
            ListItem::Marker(ListMarker::PinnedDivider),
            row(2, Section::Active),
            row(3, Section::Active),
            ListItem::Marker(ListMarker::WorkingHeader),
            row(4, Section::Working),
            ListItem::Marker(ListMarker::SnoozedHeader),
            row(5, Section::Snoozed),
            ListItem::Marker(ListMarker::SettledHeader),
            row(6, Section::Settled),
        ]
    }

    #[test]
    fn drop_slots_follow_the_markers_before_them() {
        let items = list();
        let to_pinned = drop_target(&items, 4, 1).unwrap();
        assert_eq!(
            to_pinned,
            DropTarget {
                section: DropSection::Pinned,
                pinned_order: vec![3, 1],
                active_order: vec![2]
            }
        );
        let to_active = drop_target(&items, 1, 3).unwrap();
        assert_eq!(
            to_active,
            DropTarget {
                section: DropSection::Active,
                pinned_order: vec![],
                active_order: vec![2, 1, 3]
            }
        );
        let to_settled = drop_target(&items, 3, 10).unwrap();
        assert_eq!(to_settled.section, DropSection::Settled);
        assert_eq!(to_settled.active_order, vec![3]);
    }

    #[test]
    fn never_drops_into_the_working_or_snoozed_shelf() {
        let items = list();
        assert_eq!(drop_target(&items, 3, 6), None, "inside Working");
        assert_eq!(drop_target(&items, 3, 8), None, "inside Snoozed");
        assert_eq!(drop_target(&items, 0, 1), None, "a marker is not a row");
        assert_eq!(drop_target(&items, 3, items.len()), None, "out of range");
        assert_eq!(drop_target(&items, items.len(), 0), None);
    }

    #[test]
    fn drop_verbs() {
        use Section::*;
        assert_eq!(drop_verb(Active, Some(Pinned)), Some(DropVerb::Pin));
        assert_eq!(drop_verb(Snoozed, Some(Pinned)), Some(DropVerb::Pin));
        assert_eq!(drop_verb(Active, Some(Settled)), Some(DropVerb::Settle));
        assert_eq!(drop_verb(Pinned, Some(Active)), Some(DropVerb::Unpin));
        assert_eq!(drop_verb(Settled, Some(Active)), Some(DropVerb::Unsettle));
        assert_eq!(drop_verb(Snoozed, Some(Active)), Some(DropVerb::Wake));
        assert_eq!(drop_verb(Pinned, Some(Pinned)), None);
        assert_eq!(drop_verb(Active, Some(Active)), None);
        assert_eq!(drop_verb(Settled, Some(Settled)), None);
        assert_eq!(drop_verb(Active, Some(Working)), None);
        assert_eq!(drop_verb(Active, Some(Snoozed)), None);
        assert_eq!(drop_verb(Active, None), None);
    }

    /// The sections around a drop; `plan` fills in the per-drop fields.
    #[derive(Default)]
    struct Case {
        pinned_order: Vec<u32>,
        pinned_keys: HashMap<u32, String>,
        pinned_writable: Option<HashSet<u32>>,
        active_order: Vec<u32>,
        active_keys: HashMap<u32, String>,
        active_writable: Option<HashSet<u32>>,
        time_ordered: bool,
        unsupported: bool,
    }

    fn keys(pairs: &[(u32, &str)]) -> HashMap<u32, String> {
        pairs
            .iter()
            .map(|&(id, key)| (id, key.to_owned()))
            .collect()
    }

    fn set(ids: &[u32]) -> Option<HashSet<u32>> {
        Some(ids.iter().copied().collect())
    }

    fn target(section: DropSection, pinned: &[u32], active: &[u32]) -> DropTarget<u32> {
        DropTarget {
            section,
            pinned_order: pinned.to_vec(),
            active_order: active.to_vec(),
        }
    }

    fn assign(id: u32, order: &str) -> OrderAssignment<u32> {
        OrderAssignment {
            id,
            order: order.to_owned(),
        }
    }

    impl Case {
        fn plan(&self, moved: u32, from: Section, target: &DropTarget<u32>) -> DropPlan<u32> {
            self.plan_with(
                moved,
                from,
                from == Section::Pinned,
                from == Section::Settled,
                target,
            )
        }

        fn plan_with(
            &self,
            moved: u32,
            from: Section,
            from_pinned: bool,
            from_settled: bool,
            target: &DropTarget<u32>,
        ) -> DropPlan<u32> {
            plan_drop(DropInput {
                moved: &moved,
                from,
                from_pinned,
                from_settled,
                supports_settlement: !self.unsupported,
                target,
                pinned_order: &self.pinned_order,
                pinned_keys: &self.pinned_keys,
                pinned_writable: self.pinned_writable.as_ref(),
                active_order: &self.active_order,
                active_keys: &self.active_keys,
                active_writable: self.active_writable.as_ref(),
                active_time_ordered: self.time_ordered,
            })
        }
    }

    #[test]
    fn a_working_thread_never_moves() {
        let case = Case::default();
        let into_pinned = target(DropSection::Pinned, &[9], &[]);
        assert_eq!(case.plan(9, Section::Working, &into_pinned), DropPlan::None);
        let into_settled = target(DropSection::Settled, &[], &[]);
        assert_eq!(
            case.plan(9, Section::Working, &into_settled),
            DropPlan::None
        );
    }

    #[test]
    fn reorders_pinned_rows_only_when_the_order_changed() {
        let case = Case {
            pinned_order: vec![1, 2, 3],
            pinned_keys: keys(&[(1, "b"), (2, "d"), (3, "f")]),
            ..Case::default()
        };
        let moved = target(DropSection::Pinned, &[1, 3, 2], &[]);
        assert_eq!(
            case.plan(3, Section::Pinned, &moved),
            DropPlan::ReorderPinned {
                assignments: vec![assign(3, "c")]
            }
        );
        let unchanged = target(DropSection::Pinned, &[1, 2, 3], &[]);
        assert_eq!(case.plan(3, Section::Pinned, &unchanged), DropPlan::None);
    }

    #[test]
    fn pinning_by_drop_sends_the_planned_key_first() {
        let case = Case {
            pinned_order: vec![1, 2],
            pinned_keys: keys(&[(1, "b"), (2, "d")]),
            ..Case::default()
        };
        let between = target(DropSection::Pinned, &[1, 9, 2], &[]);
        assert_eq!(
            case.plan(9, Section::Active, &between),
            DropPlan::Pin {
                order: Some("c".to_owned()),
                extra: vec![]
            }
        );

        let keyless = Case {
            pinned_order: vec![1, 2],
            ..Case::default()
        };
        let top = target(DropSection::Pinned, &[9, 1, 2], &[]);
        let spread = spread_orders(3);
        assert_eq!(
            keyless.plan(9, Section::Settled, &top),
            DropPlan::Pin {
                order: Some(spread[0].clone()),
                extra: vec![assign(1, &spread[1]), assign(2, &spread[2])],
            }
        );
    }

    #[test]
    fn only_changes_lifecycle_when_the_inbox_is_time_ordered() {
        let case = Case {
            active_order: vec![1, 2],
            time_ordered: true,
            ..Case::default()
        };
        let into_active = target(DropSection::Active, &[], &[1, 9, 2]);
        let lifecycle = |unpin, unsettle, wake| DropPlan::MoveActive {
            assignments: vec![],
            unpin,
            unsettle,
            wake,
        };
        assert_eq!(
            case.plan(9, Section::Pinned, &into_active),
            lifecycle(true, false, false)
        );
        assert_eq!(
            case.plan(9, Section::Settled, &into_active),
            lifecycle(false, true, false)
        );
        assert_eq!(
            case.plan_with(9, Section::Snoozed, true, false, &into_active),
            lifecycle(true, false, true)
        );
        let within = target(DropSection::Active, &[], &[2, 1]);
        assert_eq!(case.plan(1, Section::Active, &within), DropPlan::None);
    }

    #[test]
    fn manual_active_drops_send_lifecycle_then_reorders() {
        let case = Case {
            active_order: vec![1, 2],
            active_keys: keys(&[(1, "b"), (2, "d")]),
            ..Case::default()
        };
        let unchanged = target(DropSection::Active, &[], &[1, 2]);
        assert_eq!(case.plan(1, Section::Active, &unchanged), DropPlan::None);
        let up = target(DropSection::Active, &[], &[2, 1]);
        assert_eq!(
            case.plan(2, Section::Active, &up),
            DropPlan::MoveActive {
                assignments: vec![assign(2, "an")],
                unpin: false,
                unsettle: false,
                wake: false
            }
        );
        let from_settled = target(DropSection::Active, &[], &[1, 9, 2]);
        assert_eq!(
            case.plan(9, Section::Settled, &from_settled),
            DropPlan::MoveActive {
                assignments: vec![assign(9, "c")],
                unpin: false,
                unsettle: true,
                wake: false
            }
        );
    }

    #[test]
    fn arranges_the_rows_it_can_write() {
        // Row 5 is offline but keyed and not touched by the move.
        let case = Case {
            active_order: vec![1, 2, 5],
            active_keys: keys(&[(1, "b"), (2, "d"), (5, "x")]),
            active_writable: set(&[1, 2, 9]),
            ..Case::default()
        };
        let into = target(DropSection::Active, &[], &[1, 9, 2, 5]);
        assert_eq!(
            case.plan(9, Section::Pinned, &into),
            DropPlan::MoveActive {
                assignments: vec![assign(9, "c")],
                unpin: true,
                unsettle: false,
                wake: false
            }
        );
    }

    #[test]
    fn refuses_an_offline_drag_source() {
        let case = Case {
            active_order: vec![1, 2],
            active_writable: set(&[1, 2]),
            ..Case::default()
        };
        let into = target(DropSection::Active, &[], &[1, 9, 2]);
        assert_eq!(case.plan(9, Section::Pinned, &into), DropPlan::None);
        let pinned = Case {
            pinned_order: vec![1],
            pinned_writable: set(&[1]),
            ..Case::default()
        };
        assert_eq!(
            pinned.plan(
                9,
                Section::Active,
                &target(DropSection::Pinned, &[9, 1], &[])
            ),
            DropPlan::None
        );
    }

    #[test]
    fn preserves_a_keyed_offline_bound() {
        let case = Case {
            active_order: vec![1, 2],
            active_keys: keys(&[(1, "b"), (2, "d")]),
            active_writable: set(&[1, 9]),
            ..Case::default()
        };
        let into = target(DropSection::Active, &[], &[1, 9, 2]);
        assert_eq!(
            case.plan(9, Section::Pinned, &into),
            DropPlan::MoveActive {
                assignments: vec![assign(9, "c")],
                unpin: true,
                unsettle: false,
                wake: false
            }
        );
    }

    #[test]
    fn rejects_rebalancing_a_keyed_offline_bound() {
        // Row 1 is keyless, so the move respreads, which would rewrite offline row 2.
        let case = Case {
            active_order: vec![1, 2],
            active_keys: keys(&[(2, "d")]),
            active_writable: set(&[1, 9]),
            ..Case::default()
        };
        let into = target(DropSection::Active, &[], &[1, 9, 2]);
        assert_eq!(case.plan(9, Section::Pinned, &into), DropPlan::None);
    }

    #[test]
    fn rejects_crossing_the_frozen_keyless_boundary() {
        // Row 7 is offline and keyless; keyless rows sort above keyed ones.
        let case = Case {
            active_order: vec![7, 1],
            active_keys: keys(&[(1, "n")]),
            active_writable: set(&[1, 9]),
            ..Case::default()
        };
        let above = target(DropSection::Active, &[], &[9, 7, 1]);
        assert_eq!(case.plan(9, Section::Settled, &above), DropPlan::None);
    }

    #[test]
    fn allows_realizable_moves_beside_a_frozen_keyless_row() {
        let case = Case {
            active_order: vec![7, 1],
            active_keys: keys(&[(1, "n")]),
            active_writable: set(&[1, 9]),
            ..Case::default()
        };
        let below = target(DropSection::Active, &[], &[7, 9, 1]);
        assert_eq!(
            case.plan(9, Section::Settled, &below),
            DropPlan::MoveActive {
                assignments: vec![assign(9, "h")],
                unpin: false,
                unsettle: true,
                wake: false
            }
        );
    }

    #[test]
    fn checks_fallback_assignments_at_the_boundary() {
        // Row 5 is keyless and writable above frozen row 7: the respread would key it.
        let case = Case {
            active_order: vec![5, 7, 1],
            active_keys: keys(&[(1, "n")]),
            active_writable: set(&[1, 5, 9]),
            ..Case::default()
        };
        let into = target(DropSection::Active, &[], &[5, 7, 9, 1]);
        assert_eq!(case.plan(9, Section::Settled, &into), DropPlan::None);
    }

    #[test]
    fn rejects_impossible_pin_or_unpin_placement_across_the_boundary() {
        // Pinned: keyed rows sort above keyless ones, so no key below frozen row 7.
        let pins = Case {
            pinned_order: vec![1, 7],
            pinned_keys: keys(&[(1, "n")]),
            pinned_writable: set(&[1, 9]),
            ..Case::default()
        };
        assert_eq!(
            pins.plan(
                9,
                Section::Active,
                &target(DropSection::Pinned, &[1, 7, 9], &[])
            ),
            DropPlan::None
        );
        assert!(matches!(
            pins.plan(
                9,
                Section::Active,
                &target(DropSection::Pinned, &[1, 9, 7], &[])
            ),
            DropPlan::Pin { order: Some(_), .. }
        ));

        let actives = Case {
            active_order: vec![7, 1],
            active_keys: keys(&[(1, "n")]),
            active_writable: set(&[1, 9]),
            ..Case::default()
        };
        assert_eq!(
            actives.plan(
                9,
                Section::Pinned,
                &target(DropSection::Active, &[], &[9, 7, 1])
            ),
            DropPlan::None
        );
    }

    #[test]
    fn settling_drops_and_unsupported_servers() {
        let case = Case::default();
        let settled = target(DropSection::Settled, &[], &[]);
        assert_eq!(case.plan(9, Section::Active, &settled), DropPlan::Settle);
        assert_eq!(case.plan(9, Section::Pinned, &settled), DropPlan::Settle);
        assert_eq!(case.plan(9, Section::Settled, &settled), DropPlan::None);

        let unsupported = Case {
            unsupported: true,
            ..Case::default()
        };
        assert_eq!(
            unsupported.plan(9, Section::Active, &settled),
            DropPlan::None
        );
        let out_to_active = target(DropSection::Active, &[], &[9]);
        assert_eq!(
            unsupported.plan(9, Section::Settled, &out_to_active),
            DropPlan::None
        );
        let out_to_pinned = target(DropSection::Pinned, &[9], &[]);
        assert_eq!(
            unsupported.plan(9, Section::Settled, &out_to_pinned),
            DropPlan::None
        );
    }

    #[test]
    fn forward_navigation_skips_parked_rows_and_wraps() {
        let ordered = [1, 2, 3, 4];
        let parked = |id: &u32| *id == 2 || *id == 4;
        assert_eq!(forward_target(&ordered, &1, parked), Some(&3));
        assert_eq!(forward_target(&ordered, &3, parked), Some(&1));
        assert_eq!(forward_target(&ordered, &4, parked), Some(&1));
        assert_eq!(forward_target(&[1], &1, |_| false), None, "the only row");
        assert_eq!(forward_target(&ordered, &9, |_| false), None, "not listed");
        assert_eq!(
            forward_target(&ordered, &1, |id| *id != 1),
            None,
            "all parked"
        );
    }
}
