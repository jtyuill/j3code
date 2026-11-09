//! What a [`crate::Connection`] publishes, and the pure reducers that apply
//! `shell.watch` and `threads.watch` items to it (`docs/protocol/wire.md` §6).

use std::collections::BTreeMap;

use j3_protocol::v2::{
    entities::{
        Item, ItemKind, Project, QueueEntry, Request, RequestStatus, SubagentItem, Thread,
        TodoItem, Turn,
    },
    methods::Catalog,
    streams::{DeltaField, ShellItem, ThreadItem},
};
use serde::{Deserialize, Serialize};

/// `GET /api/v1/info` (wire.md §2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    /// Stable for the life of the server's database; key cached state by it.
    pub server_id: String,
    #[serde(default)]
    pub name: String,
    pub protocol: u32,
    #[serde(default)]
    pub version: String,
    /// Optional method groups the server serves (`terminal`, `git`, …).
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub home: String,
}

/// The connection's lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Status {
    /// The first attempt is under way.
    #[default]
    Connecting,
    /// The socket is open and the catalog arrived. [`Shell::synced`] says
    /// whether the shell has caught up.
    Connected,
    /// The last attempt (or the connection) failed; the next attempt is
    /// `attempt + 1`.
    Reconnecting { attempt: u32, error: String },
    /// The server speaks another protocol version. The supervisor keeps
    /// checking at its slowest backoff, in case the server is updated.
    Incompatible { message: String },
    /// [`crate::Connection::close`] was called.
    Closed,
}

/// Status, server identity, catalog and shell of one connection.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ConnectionState {
    pub status: Status,
    pub info: Option<Info>,
    pub catalog: Option<Catalog>,
    pub shell: Shell,
}

/// Every project and every thread row that is not deleted, archived ones
/// included (wire.md §5.2); filter on `archived_at`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Shell {
    /// The highest sequence applied.
    pub sequence: u64,
    /// Caught up with the server on a live socket; false while
    /// (re)subscribing.
    pub synced: bool,
    pub projects: BTreeMap<String, Project>,
    pub threads: BTreeMap<String, Thread>,
}

/// Where a [`ThreadView`] stands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ThreadSync {
    /// Not caught up: before the first snapshot, or resubscribing after a
    /// reconnect (the last state stays in place meanwhile).
    #[default]
    Loading,
    /// Caught up with the server on a live socket.
    Synced,
    /// The thread does not exist or was deleted; the subscription ended.
    Missing,
    /// The subscription failed and is being retried.
    Error { message: String },
}

/// One thread with everything its feed shows.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ThreadView {
    pub thread_id: String,
    pub sync: ThreadSync,
    /// The highest sequence applied.
    pub sequence: u64,
    /// `None` until the first snapshot.
    pub thread: Option<Thread>,
    /// By `ordinal`.
    pub turns: Vec<Turn>,
    /// By `ordinal`.
    pub items: Vec<Item>,
    /// Open requests and those of the latest turn, in arrival order.
    pub requests: Vec<Request>,
    /// By `position`.
    pub queue: Vec<QueueEntry>,
}

impl ThreadView {
    pub fn new(thread_id: impl Into<String>) -> Self {
        Self {
            thread_id: thread_id.into(),
            ..Self::default()
        }
    }

    /// The running turn (the thread row's `activeTurnId`).
    pub fn active_turn(&self) -> Option<&Turn> {
        let id = self.thread.as_ref()?.active_turn_id.as_deref()?;
        self.turns.iter().rev().find(|turn| turn.id == id)
    }

    /// The requests omp is waiting on, oldest first.
    pub fn open_requests(&self) -> impl Iterator<Item = &Request> {
        self.requests
            .iter()
            .filter(|request| request.status == RequestStatus::Open)
    }

    /// Whether a stop held the queue (wire.md §4.6): entries wait for
    /// `queue.resume`.
    pub fn queue_held(&self) -> bool {
        self.queue.iter().any(|entry| entry.held)
    }

    /// The newest todo list.
    pub fn latest_todo(&self) -> Option<(&Item, &TodoItem)> {
        self.items.iter().rev().find_map(|item| match &item.kind {
            ItemKind::Todo(todo) => Some((item, todo)),
            _ => None,
        })
    }

    /// Every subagent item, in feed order.
    pub fn subagents(&self) -> impl Iterator<Item = (&Item, &SubagentItem)> {
        self.items.iter().filter_map(|item| match &item.kind {
            ItemKind::Subagent(subagent) => Some((item, subagent)),
            _ => None,
        })
    }

    /// The subagent item of `subagent_id`.
    pub fn subagent(&self, subagent_id: &str) -> Option<(&Item, &SubagentItem)> {
        self.subagents()
            .find(|(_, subagent)| subagent.subagent_id == subagent_id)
    }
}

/// The sequence bookkeeping of one subscription (wire.md §6.2).
///
/// One commit may produce several items with the same sequence (e.g. a
/// `turn` and an `item`, or an `item` and the `queue`), so a delta is
/// dropped only when its sequence is lower than the highest applied (or
/// than the snapshot's, or the `after` the subscription resumed from).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Cursor {
    applied: Option<u64>,
}

impl Cursor {
    /// A subscription that passed `after` (none: expects a snapshot).
    pub(crate) fn resume(after: Option<u64>) -> Self {
        Self { applied: after }
    }

    fn snapshot(&mut self, sequence: u64) {
        self.applied = Some(sequence);
    }

    /// Whether a delta at `sequence` is current; records it if so.
    fn admit(&mut self, sequence: u64) -> bool {
        let current = self.applied.is_none_or(|applied| sequence >= applied);
        if current {
            self.applied = Some(sequence);
        }
        current
    }
}

impl Shell {
    /// The projects oldest first (ties by id): the order lists show them in.
    pub fn projects_in_order(&self) -> Vec<&Project> {
        let mut projects: Vec<&Project> = self.projects.values().collect();
        projects.sort_by(|a, b| (&a.created_at, &a.id).cmp(&(&b.created_at, &b.id)));
        projects
    }

    /// Applies one `shell.watch` item; whether anything changed.
    pub(crate) fn apply(&mut self, cursor: &mut Cursor, item: ShellItem) -> bool {
        if let Some(sequence) = item.sequence() {
            if !cursor.admit(sequence) {
                return false;
            }
            self.sequence = self.sequence.max(sequence);
        }
        match item {
            ShellItem::Snapshot(snapshot) => {
                cursor.snapshot(snapshot.sequence);
                self.sequence = snapshot.sequence;
                self.projects = snapshot
                    .projects
                    .into_iter()
                    .map(|project| (project.id.clone(), project))
                    .collect();
                self.threads = snapshot
                    .threads
                    .into_iter()
                    .map(|thread| (thread.id.clone(), thread))
                    .collect();
            }
            ShellItem::Synced => {
                if self.synced {
                    return false;
                }
                self.synced = true;
            }
            ShellItem::Project { project, .. } => {
                self.projects.insert(project.id.clone(), project);
            }
            ShellItem::ProjectRemoved { project_id, .. } => {
                self.projects.remove(&project_id);
            }
            ShellItem::Thread { thread, .. } => {
                self.threads.insert(thread.id.clone(), *thread);
            }
            ShellItem::ThreadRemoved { thread_id, .. } => {
                self.threads.remove(&thread_id);
            }
        }
        true
    }
}

impl ThreadView {
    /// Applies one `threads.watch` item; whether anything changed.
    pub(crate) fn apply(&mut self, cursor: &mut Cursor, item: ThreadItem) -> bool {
        if let Some(sequence) = item.sequence() {
            if !cursor.admit(sequence) {
                return false;
            }
            self.sequence = self.sequence.max(sequence);
        }
        match item {
            ThreadItem::Snapshot(snapshot) => {
                let snapshot = *snapshot;
                cursor.snapshot(snapshot.sequence);
                self.sequence = snapshot.sequence;
                self.thread = Some(snapshot.thread);
                self.turns = snapshot.turns;
                self.turns.sort_by_key(|turn| turn.ordinal);
                self.items = snapshot.items;
                self.items.sort_by_key(|item| item.ordinal);
                self.requests = snapshot.requests;
                self.queue = snapshot.queue;
                self.queue.sort_by_key(|entry| entry.position);
            }
            ThreadItem::Synced => {
                if self.sync == ThreadSync::Synced {
                    return false;
                }
                self.sync = ThreadSync::Synced;
            }
            ThreadItem::Thread { thread, .. } => self.thread = Some(*thread),
            ThreadItem::ItemDelta {
                item_id,
                field,
                append,
                ..
            } => {
                // A delta for an item this view lacks still moved the sequence.
                if let Some(item) = self.items.iter_mut().rev().find(|item| item.id == item_id) {
                    append_to(item, field, &append);
                }
            }
            ThreadItem::Turn { turn, .. } => {
                upsert_ordered(
                    &mut self.turns,
                    turn,
                    |turn| &turn.id,
                    |turn| u64::from(turn.ordinal),
                );
            }
            ThreadItem::Item { item, .. } => {
                upsert_ordered(&mut self.items, *item, |item| &item.id, |item| item.ordinal);
            }
            ThreadItem::Request { request, .. } => {
                match self
                    .requests
                    .iter_mut()
                    .find(|known| known.id == request.id)
                {
                    Some(known) => *known = request,
                    None => self.requests.push(request),
                }
            }
            ThreadItem::Queue { mut queue, .. } => {
                queue.sort_by_key(|entry| entry.position);
                self.queue = queue;
            }
            ThreadItem::ThreadRemoved { .. } => self.sync = ThreadSync::Missing,
        }
        true
    }
}

/// Replaces the element with `value`'s id, or inserts it, keeping `list`
/// sorted by `order` (stable among equal keys).
fn upsert_ordered<T>(
    list: &mut Vec<T>,
    value: T,
    id: impl Fn(&T) -> &str,
    order: impl Fn(&T) -> u64,
) {
    if let Some(index) = list.iter().rposition(|known| id(known) == id(&value)) {
        if order(&list[index]) == order(&value) {
            list[index] = value;
            return;
        }
        list.remove(index);
    }
    let key = order(&value);
    let at = list.partition_point(|known| order(known) <= key);
    list.insert(at, value);
}

/// `item.delta` (wire.md §6.3): `text` grows a text or thinking item's
/// `text`, `output` a tool item's `output.text`.
fn append_to(item: &mut Item, field: DeltaField, append: &str) {
    match (field, &mut item.kind) {
        (DeltaField::Text, ItemKind::Text(body) | ItemKind::Thinking(body)) => {
            body.text.push_str(append);
        }
        (DeltaField::Output, ItemKind::Tool(tool)) => {
            tool.output
                .get_or_insert_with(Default::default)
                .text
                .push_str(append);
        }
        // Another kind: nothing a delta can grow.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn thread_item(value: Value) -> ThreadItem {
        serde_json::from_value(value).unwrap()
    }

    fn item(id: &str, ordinal: u64, text: &str) -> Value {
        json!({"id": id, "threadId": "t1", "turnId": "u1", "ordinal": ordinal,
            "status": "running", "startedAt": "2026-11-09T11:18:00.000Z",
            "kind": "text", "text": text})
    }

    fn snapshot(sequence: u64, items: Vec<Value>) -> ThreadItem {
        thread_item(json!({"type": "snapshot", "sequence": sequence,
            "thread": {"id": "t1", "projectId": "p1", "title": "T",
                "createdAt": "2026-11-09T11:18:00.000Z", "updatedAt": "2026-11-09T11:18:00.000Z",
                "status": "running", "model": {"provider": "x", "id": "y"}, "mode": "default",
                "queued": 0, "triage": {"bumpedAt": "2026-11-09T11:18:00.000Z"}},
            "turns": [], "items": items, "requests": [], "queue": []}))
    }

    fn texts(view: &ThreadView) -> Vec<(&str, &str)> {
        view.items
            .iter()
            .map(|item| match &item.kind {
                ItemKind::Text(body) => (item.id.as_str(), body.text.as_str()),
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    #[test]
    fn items_keep_ordinal_order() {
        let mut view = ThreadView::new("t1");
        let mut cursor = Cursor::resume(None);
        assert!(view.apply(
            &mut cursor,
            snapshot(10, vec![item("b", 5, "B"), item("a", 2, "A")])
        ));
        assert_eq!(texts(&view), [("a", "A"), ("b", "B")]);
        // A new item between them, an update in place, one at the end.
        for (sequence, value) in [
            (11, item("m", 3, "M")),
            (12, item("a", 2, "A2")),
            (13, item("z", 9, "Z")),
        ] {
            assert!(view.apply(
                &mut cursor,
                thread_item(json!({"type": "item", "sequence": sequence, "item": value}))
            ));
        }
        assert_eq!(
            texts(&view),
            [("a", "A2"), ("m", "M"), ("b", "B"), ("z", "Z")]
        );
        assert_eq!(view.sequence, 13);
    }

    #[test]
    fn deltas_append_text_and_tool_output() {
        let mut view = ThreadView::new("t1");
        let mut cursor = Cursor::resume(None);
        let tool = json!({"id": "c", "threadId": "t1", "turnId": "u1", "ordinal": 2,
            "status": "running", "startedAt": "2026-11-09T11:18:00.000Z", "kind": "tool",
            "callId": "call_1", "name": "bash", "input": {}});
        view.apply(&mut cursor, snapshot(1, vec![item("a", 1, "Hel"), tool]));
        let delta = |sequence: u64, id: &str, field: &str, append: &str| {
            thread_item(
                json!({"type": "item.delta", "sequence": sequence, "itemId": id,
                "field": field, "append": append}),
            )
        };
        assert!(view.apply(&mut cursor, delta(2, "a", "text", "lo")));
        // Two items of one commit share its sequence.
        assert!(view.apply(&mut cursor, delta(2, "c", "output", "out")));
        assert!(view.apply(&mut cursor, delta(3, "c", "output", "put")));
        // An unknown item: nothing to grow, but the sequence moved.
        assert!(view.apply(&mut cursor, delta(4, "missing", "text", "x")));
        assert_eq!(view.sequence, 4);
        assert!(matches!(&view.items[0].kind, ItemKind::Text(body) if body.text == "Hello"));
        assert!(matches!(&view.items[1].kind,
            ItemKind::Tool(tool) if tool.output.as_ref().unwrap().text == "output"));
    }

    #[test]
    fn stale_sequences_are_dropped() {
        let mut view = ThreadView::new("t1");
        let mut cursor = Cursor::resume(None);
        view.apply(&mut cursor, snapshot(5, vec![item("a", 1, "A")]));
        let delta = |sequence: u64| {
            thread_item(
                json!({"type": "item.delta", "sequence": sequence, "itemId": "a",
                "field": "text", "append": "!"}),
            )
        };
        // Below the snapshot: already in it. Equal: another item of a commit.
        assert!(!view.apply(&mut cursor, delta(4)));
        assert!(view.apply(&mut cursor, delta(5)));
        assert!(view.apply(&mut cursor, delta(6)));
        assert!(!view.apply(&mut cursor, delta(3)));
        assert!(!view.apply(&mut cursor, delta(5)));
        assert_eq!(texts(&view), [("a", "A!!")]);
        assert_eq!(view.sequence, 6);

        // Resuming after 6: older deltas are dropped, 7 applies.
        let mut resumed = Cursor::resume(Some(view.sequence));
        assert!(!view.apply(&mut resumed, delta(5)));
        assert!(view.apply(&mut resumed, delta(7)));
        assert_eq!(texts(&view), [("a", "A!!!")]);

        // A snapshot replaces everything, whatever its sequence.
        assert!(view.apply(&mut resumed, snapshot(2, vec![item("b", 1, "B")])));
        assert_eq!(texts(&view), [("b", "B")]);
        assert_eq!(view.sequence, 2);
        assert_eq!(view.sync, ThreadSync::Loading);
        assert!(view.apply(&mut resumed, thread_item(json!({"type": "synced"}))));
        assert_eq!(view.sync, ThreadSync::Synced);
        assert!(view.apply(
            &mut resumed,
            thread_item(json!({"type": "thread.removed", "sequence": 3}))
        ));
        assert_eq!(view.sync, ThreadSync::Missing);
    }

    #[test]
    fn shell_snapshot_and_deltas() {
        let project = |id: &str, title: &str| {
            json!({"id": id, "title": title, "path": "/p", "createdAt": "2026-11-09T11:18:00.000Z",
                "updatedAt": "2026-11-09T11:18:00.000Z"})
        };
        let shell_item = |value: Value| -> ShellItem { serde_json::from_value(value).unwrap() };
        let mut shell = Shell::default();
        let mut cursor = Cursor::resume(None);
        shell.apply(
            &mut cursor,
            shell_item(json!({"type": "snapshot", "sequence": 4,
            "projects": [project("p1", "One")], "threads": []})),
        );
        shell.apply(&mut cursor, shell_item(json!({"type": "synced"})));
        assert!(!shell.apply(
            &mut cursor,
            shell_item(json!({"type": "project", "sequence": 3,
            "project": project("p1", "Stale")}))
        ));
        assert!(shell.apply(
            &mut cursor,
            shell_item(json!({"type": "project", "sequence": 5,
            "project": project("p2", "Two")}))
        ));
        assert!(shell.apply(
            &mut cursor,
            shell_item(json!({"type": "project.removed", "sequence": 6,
            "projectId": "p1"}))
        ));
        assert!(shell.synced);
        assert_eq!(shell.sequence, 6);
        assert_eq!(shell.projects.keys().collect::<Vec<_>>(), ["p2"]);
    }
}
