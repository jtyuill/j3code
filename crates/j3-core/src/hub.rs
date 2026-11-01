//! Sequenced commits and live fan-out.
//!
//! One [`Commit`] is one SQLite transaction: entity upserts, the thread index
//! row, project rows, an optional command receipt, and the domain events. Each
//! event gets its own increasing global sequence; derived shell items carry the
//! commit's last sequence. The store lock is held from the transaction through
//! the broadcast, so broadcast order equals sequence order and a reader that
//! subscribed before taking the lock sees every later commit exactly once.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use j3_protocol::t3;
use serde_json::{Map, Value, json};
use tokio::sync::broadcast;

use crate::store::{self, EntityRow, Store, ThreadRow};

/// Live thread events kept for slow subscribers before they lag.
const THREAD_CAPACITY: usize = 4096;
/// Live shell items kept for slow subscribers before they lag.
const SHELL_CAPACITY: usize = 1024;

/// A domain event before the hub assigns its id, sequence and time.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingEvent {
    /// `OrchestrationV2DomainEvent.type`, e.g. `turn-item.updated`.
    pub event_type: &'static str,
    pub payload: Value,
    pub run_id: Option<String>,
    pub node_id: Option<String>,
}

/// A shell stream change; the hub adds `sequence`.
#[derive(Debug, Clone, PartialEq)]
pub enum ShellChange {
    /// `thread.updated`; `location` is `active` or `archive`.
    ThreadUpdated {
        location: &'static str,
        thread: Value,
    },
    /// `thread.removed`.
    ThreadRemoved {
        location: &'static str,
        thread_id: String,
    },
    /// `project.updated` with an `OrchestrationProjectShell`.
    ProjectUpdated(Value),
    /// `project.removed`.
    ProjectRemoved(String),
}

impl ShellChange {
    fn item(&self, sequence: u64) -> Value {
        match self {
            Self::ThreadUpdated { location, thread } => json!({
                "kind": "thread.updated", "sequence": sequence,
                "location": location, "thread": thread,
            }),
            Self::ThreadRemoved {
                location,
                thread_id,
            } => json!({
                "kind": "thread.removed", "sequence": sequence,
                "location": location, "threadId": thread_id,
            }),
            Self::ProjectUpdated(project) => json!({
                "kind": "project.updated", "sequence": sequence, "project": project,
            }),
            Self::ProjectRemoved(project_id) => json!({
                "kind": "project.removed", "sequence": sequence, "projectId": project_id,
            }),
        }
    }
}

/// Writes for one thread inside a commit.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThreadWrite {
    pub thread_id: String,
    pub entities: Vec<EntityRow>,
    pub events: Vec<PendingEvent>,
    /// New thread index row (session file, archived flag, shell).
    pub index: Option<ThreadRow>,
    /// Drop every entity and mark the index row deleted (after `events`).
    pub delete: bool,
}

/// Builds a command receipt from the commit's last sequence.
pub type ReceiptFn = Box<dyn FnOnce(u64) -> Value + Send>;

/// Everything written atomically by one [`Hub::commit`].
#[derive(Default)]
pub struct Commit {
    pub thread: Option<ThreadWrite>,
    /// `(project id, Some(Project json))` upserts, `None` deletes.
    pub projects: Vec<(String, Option<Value>)>,
    pub shell: Vec<ShellChange>,
    /// `(command id, receipt builder)`.
    pub receipt: Option<(String, ReceiptFn)>,
}

impl Commit {
    pub fn is_empty(&self) -> bool {
        self.thread.as_ref().is_none_or(|thread| {
            thread.entities.is_empty()
                && thread.events.is_empty()
                && thread.index.is_none()
                && !thread.delete
        }) && self.projects.is_empty()
            && self.shell.is_empty()
            && self.receipt.is_none()
    }
}

/// One live thread stream item (`{"kind":"event",...}`).
#[derive(Debug, Clone)]
pub struct ThreadEvent {
    pub thread_id: Arc<str>,
    pub sequence: u64,
    pub item: Arc<Value>,
}

/// One live shell stream item.
#[derive(Debug, Clone)]
pub struct ShellEvent {
    pub sequence: u64,
    pub item: Arc<Value>,
}

pub struct Hub {
    store: Mutex<Store>,
    threads: broadcast::Sender<ThreadEvent>,
    shell: broadcast::Sender<ShellEvent>,
}

impl Hub {
    pub fn new(store: Store) -> Self {
        Self::with_capacity(store, THREAD_CAPACITY, SHELL_CAPACITY)
    }

    /// Custom broadcast capacities (lag tests).
    pub fn with_capacity(store: Store, threads: usize, shell: usize) -> Self {
        Self {
            store: Mutex::new(store),
            threads: broadcast::channel(threads).0,
            shell: broadcast::channel(shell).0,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `read` against a consistent view of the store.
    pub fn read<T>(&self, read: impl FnOnce(&Store) -> T) -> T {
        read(&self.lock())
    }

    pub fn high_water(&self) -> u64 {
        self.lock().high_water()
    }

    pub fn subscribe_threads(&self) -> broadcast::Receiver<ThreadEvent> {
        self.threads.subscribe()
    }

    pub fn subscribe_shell(&self) -> broadcast::Receiver<ShellEvent> {
        self.shell.subscribe()
    }

    /// Applies `commit` atomically and broadcasts it. Returns the commit's
    /// last sequence (the current high water when it emitted nothing).
    pub fn commit(&self, commit: Commit) -> store::Result<u64> {
        let mut store = self.lock();
        let occurred_at = t3::now_iso();
        let mut tx = store.transaction()?;
        let mut thread_items = Vec::new();
        if let Some(thread) = &commit.thread {
            for row in &thread.entities {
                tx.upsert_entity(&thread.thread_id, row)?;
            }
            if let Some(index) = &thread.index {
                tx.upsert_thread(index)?;
            }
            for event in &thread.events {
                let sequence = tx.next_sequence();
                thread_items.push((
                    sequence,
                    event_item(&thread.thread_id, sequence, &occurred_at, event),
                ));
            }
            if thread.delete {
                tx.delete_thread(&thread.thread_id)?;
            }
        }
        for (id, project) in &commit.projects {
            match project {
                Some(json) => tx.upsert_project(id, json)?,
                None => tx.delete_project(id)?,
            }
        }
        if !commit.shell.is_empty() && thread_items.is_empty() {
            tx.next_sequence();
        }
        let last = tx.last_sequence();
        if let Some((command_id, receipt)) = commit.receipt {
            tx.put_receipt(&command_id, &receipt(last))?;
        }
        tx.commit()?;

        if let Some(thread) = &commit.thread {
            let thread_id: Arc<str> = Arc::from(thread.thread_id.as_str());
            for (sequence, item) in thread_items {
                // No receivers is fine: nobody is watching this thread.
                let _ = self.threads.send(ThreadEvent {
                    thread_id: Arc::clone(&thread_id),
                    sequence,
                    item: Arc::new(item),
                });
            }
        }
        for change in &commit.shell {
            let _ = self.shell.send(ShellEvent {
                sequence: last,
                item: Arc::new(change.item(last)),
            });
        }
        Ok(last)
    }
}

/// `{"kind":"event","sequence":n,"event":OrchestrationV2DomainEvent}`.
fn event_item(thread_id: &str, sequence: u64, occurred_at: &str, event: &PendingEvent) -> Value {
    let mut body = Map::new();
    body.insert("id".into(), Value::String(format!("evt_{sequence}")));
    body.insert("threadId".into(), Value::String(thread_id.to_owned()));
    if let Some(run_id) = &event.run_id {
        body.insert("runId".into(), Value::String(run_id.clone()));
    }
    if let Some(node_id) = &event.node_id {
        body.insert("nodeId".into(), Value::String(node_id.clone()));
    }
    body.insert("occurredAt".into(), Value::String(occurred_at.to_owned()));
    body.insert("type".into(), Value::String(event.event_type.to_owned()));
    body.insert("payload".into(), event.payload.clone());
    json!({"kind": "event", "sequence": sequence, "event": body})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(n: u64) -> PendingEvent {
        PendingEvent {
            event_type: "message.updated",
            payload: json!({"n": n}),
            run_id: None,
            node_id: None,
        }
    }

    #[tokio::test]
    async fn engine_hub_events_get_increasing_sequences_and_shell_gets_last() {
        let hub = Hub::new(Store::open_in_memory().unwrap());
        let mut threads = hub.subscribe_threads();
        let mut shell = hub.subscribe_shell();
        let last = hub
            .commit(Commit {
                thread: Some(ThreadWrite {
                    thread_id: "t1".into(),
                    events: vec![event(1), event(2)],
                    ..ThreadWrite::default()
                }),
                shell: vec![ShellChange::ProjectRemoved("p".into())],
                receipt: Some((
                    "c1".into(),
                    Box::new(|sequence| json!({"sequence": sequence})),
                )),
                ..Commit::default()
            })
            .unwrap();
        assert_eq!(last, 2);
        assert_eq!(threads.recv().await.unwrap().sequence, 1);
        let second = threads.recv().await.unwrap();
        assert_eq!(second.sequence, 2);
        assert_eq!(second.item["event"]["id"], "evt_2");
        assert_eq!(second.item["event"]["threadId"], "t1");
        assert_eq!(shell.recv().await.unwrap().item["sequence"], 2);
        assert_eq!(
            hub.read(|store| store.receipt("c1").unwrap()),
            Some(json!({"sequence": 2}))
        );
        // Shell-only commits still advance the sequence.
        let shell_only = hub
            .commit(Commit {
                shell: vec![ShellChange::ProjectRemoved("p".into())],
                ..Commit::default()
            })
            .unwrap();
        assert_eq!(shell_only, 3);
        // Empty commits do not.
        assert_eq!(hub.commit(Commit::default()).unwrap(), 3);
    }
}
