//! Sequenced commits and live fan-out.
//!
//! One [`Commit`] is one SQLite transaction (thread records, the thread
//! index row, projects, a command result) and takes one new sequence
//! (`docs/protocol/wire.md` §6.1): every stream item of the commit carries
//! it. The store lock is held from the transaction through the broadcast,
//! so broadcast order equals sequence order and a reader that subscribed
//! before taking the lock sees every later commit exactly once.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use j3_protocol::v2::entities::Project;
use serde_json::{Map, Value, json};
use tokio::sync::broadcast;

use crate::store::{self, Record, Store};

/// Live thread commits kept for slow subscribers before they lag.
const THREAD_CAPACITY: usize = 4096;
/// Live shell commits kept for slow subscribers before they lag.
const SHELL_CAPACITY: usize = 1024;

/// The writes for one thread inside a commit.
#[derive(Debug, Default)]
pub struct ThreadWrite {
    pub thread_id: String,
    pub project_id: String,
    pub session_file: Option<String>,
    pub records: Vec<Record>,
    /// `threads.watch` items without their `sequence`.
    pub items: Vec<Map<String, Value>>,
    /// The new wire row; also sent as a `thread` item on both streams.
    pub row: Option<Value>,
    /// The thread was deleted: its records go and both streams get
    /// `thread.removed`.
    pub removed: bool,
}

/// A project change.
#[derive(Debug, Clone)]
pub enum ProjectWrite {
    Upsert(Project),
    Remove(String),
}

/// Builds a command result from the commit's sequence.
pub type ResultFn = Box<dyn FnOnce(u64) -> Value + Send>;

/// Everything written atomically by one [`Hub::commit`].
#[derive(Default)]
pub struct Commit {
    pub thread: Option<ThreadWrite>,
    pub projects: Vec<ProjectWrite>,
    /// `(command id, result builder)`, kept for repeats of the command.
    pub result: Option<(String, ResultFn)>,
}

/// One commit's `threads.watch` items for one thread.
#[derive(Debug, Clone)]
pub struct ThreadEvent {
    pub thread_id: Arc<str>,
    pub sequence: u64,
    pub items: Arc<[Value]>,
}

/// One commit's `shell.watch` items.
#[derive(Debug, Clone)]
pub struct ShellEvent {
    pub sequence: u64,
    pub items: Arc<[Value]>,
}

pub struct Hub {
    store: Mutex<Store>,
    threads: broadcast::Sender<ThreadEvent>,
    shell: broadcast::Sender<ShellEvent>,
}

/// `item` with the commit's `sequence`.
fn sequenced(mut item: Map<String, Value>, sequence: u64) -> Value {
    item.insert("sequence".into(), json!(sequence));
    Value::Object(item)
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

    /// Applies `commit` atomically under a new sequence and broadcasts it.
    /// Returns that sequence.
    pub fn commit(&self, commit: Commit) -> store::Result<u64> {
        let mut store = self.lock();
        let mut tx = store.transaction()?;
        let sequence = tx.next_sequence();
        let mut shell_items = Vec::new();
        let mut thread_event = None;
        if let Some(thread) = commit.thread {
            for record in &thread.records {
                tx.upsert_record(&thread.thread_id, record)?;
            }
            if let Some(row) = &thread.row {
                tx.upsert_thread(
                    &thread.thread_id,
                    &thread.project_id,
                    thread.session_file.as_deref(),
                    row,
                )?;
            }
            let mut items: Vec<Value> = thread
                .items
                .into_iter()
                .map(|item| sequenced(item, sequence))
                .collect();
            if thread.removed {
                tx.delete_thread(&thread.thread_id)?;
                items.push(json!({"type": "thread.removed", "sequence": sequence}));
                shell_items.push(json!({"type": "thread.removed", "sequence": sequence,
                    "threadId": thread.thread_id}));
            } else if let Some(row) = thread.row {
                let item = json!({"type": "thread", "sequence": sequence, "thread": row});
                items.push(item.clone());
                shell_items.push(item);
            }
            if !items.is_empty() {
                thread_event = Some(ThreadEvent {
                    thread_id: Arc::from(thread.thread_id.as_str()),
                    sequence,
                    items: items.into(),
                });
            }
        }
        for project in commit.projects {
            match project {
                ProjectWrite::Upsert(project) => {
                    tx.upsert_project(&project)?;
                    shell_items
                        .push(json!({"type": "project", "sequence": sequence, "project": project}));
                }
                ProjectWrite::Remove(id) => {
                    tx.delete_project(&id)?;
                    shell_items.push(json!({"type": "project.removed", "sequence": sequence,
                        "projectId": id}));
                }
            }
        }
        if let Some((command_id, result)) = commit.result {
            tx.put_receipt(&command_id, &result(sequence))?;
        }
        tx.commit()?;
        // No receivers is fine: nobody is watching.
        if let Some(event) = thread_event {
            let _ = self.threads.send(event);
        }
        if !shell_items.is_empty() {
            let _ = self.shell.send(ShellEvent {
                sequence,
                items: shell_items.into(),
            });
        }
        Ok(sequence)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(n: u64) -> Map<String, Value> {
        let Value::Object(item) = json!({"type": "turn", "turn": {"n": n}}) else {
            unreachable!()
        };
        item
    }

    #[tokio::test]
    async fn one_commit_is_one_sequence_on_every_stream() {
        let hub = Hub::new(Store::open_in_memory().unwrap());
        let mut threads = hub.subscribe_threads();
        let mut shell = hub.subscribe_shell();
        let sequence = hub
            .commit(Commit {
                thread: Some(ThreadWrite {
                    thread_id: "t1".into(),
                    project_id: "p".into(),
                    items: vec![item(1), item(2)],
                    row: Some(json!({"id": "t1"})),
                    ..ThreadWrite::default()
                }),
                result: Some((
                    "c1".into(),
                    Box::new(|sequence| json!({"sequence": sequence})),
                )),
                ..Commit::default()
            })
            .unwrap();
        assert_eq!(sequence, 1);
        let event = threads.recv().await.unwrap();
        assert_eq!(event.sequence, 1);
        assert_eq!(event.items.len(), 3, "two items and the row");
        assert!(event.items.iter().all(|item| item["sequence"] == 1));
        let shell_event = shell.recv().await.unwrap();
        assert_eq!(shell_event.items[0]["type"], "thread");
        assert_eq!(
            hub.read(|store| store.receipt("c1").unwrap()),
            Some(json!({"sequence": 1}))
        );
        // A commit with nothing in it still takes its own sequence.
        assert_eq!(hub.commit(Commit::default()).unwrap(), 2);
        assert_eq!(hub.high_water(), 2);
    }
}
