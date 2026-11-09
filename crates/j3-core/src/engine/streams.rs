//! `shell.watch` and `threads.watch` (`docs/protocol/wire.md` §6).
//!
//! Each stream subscribes to the hub broadcast first, then reads its snapshot
//! and the high-water mark under the store lock, emits `snapshot` and
//! `synced`, then forwards live commits whose sequence is above the last one
//! emitted. Every subscription starts with a snapshot, whatever its `after`
//! (wire.md §6.2 allows that). A lagging receiver gets a fresh snapshot.

use std::{collections::VecDeque, sync::Arc};

use futures_util::stream::{self, BoxStream, StreamExt};
use j3_protocol::v2::{
    envelope::Error,
    streams::{ShellItem, ShellSnapshot, ThreadItem},
};
use serde_json::Value;
use tokio::sync::broadcast::{self, error::RecvError};

use crate::{
    hub::{Hub, ShellEvent, ThreadEvent},
    store::{self, Store},
    v2::driver::StreamEvent,
};

/// Reads a snapshot item from a consistent store view.
type SnapshotFn = Arc<dyn Fn(&Store) -> Result<Value, Error> + Send + Sync>;

fn encode<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("v2 stream items always encode")
}

fn store_error(error: store::Error) -> Error {
    tracing::error!(%error, "reading a stream snapshot");
    Error::internal("The server could not read its state.")
}

/// One broadcast commit.
trait Live: Clone + Send + Sync + 'static {
    fn sequence(&self) -> u64;
    fn items(&self) -> &[Value];
    fn wanted(&self, thread: Option<&str>) -> bool;
}

impl Live for ThreadEvent {
    fn sequence(&self) -> u64 {
        self.sequence
    }
    fn items(&self) -> &[Value] {
        &self.items
    }
    fn wanted(&self, thread: Option<&str>) -> bool {
        thread == Some(&*self.thread_id)
    }
}

impl Live for ShellEvent {
    fn sequence(&self) -> u64 {
        self.sequence
    }
    fn items(&self) -> &[Value] {
        &self.items
    }
    fn wanted(&self, _: Option<&str>) -> bool {
        true
    }
}

struct State<E> {
    hub: Arc<Hub>,
    rx: broadcast::Receiver<E>,
    snapshot: SnapshotFn,
    thread: Option<String>,
    last: u64,
    queue: VecDeque<StreamEvent>,
    done: bool,
}

impl<E: Live> State<E> {
    /// Queues a fresh snapshot (and `synced` on the first one).
    fn snapshot(&mut self, first: bool) {
        let read = self
            .hub
            .read(|store| Ok::<_, Error>((store.high_water(), (self.snapshot)(store)?)));
        match read {
            Ok((sequence, item)) => {
                self.last = sequence;
                self.queue.push_back(StreamEvent::Item(item));
                if first {
                    self.queue
                        .push_back(StreamEvent::Item(encode(&ShellItem::Synced)));
                }
            }
            Err(error) => {
                self.queue.push_back(StreamEvent::Fail(error));
                self.done = true;
            }
        }
    }
}

fn live_stream<E: Live>(
    hub: Arc<Hub>,
    rx: broadcast::Receiver<E>,
    snapshot: SnapshotFn,
    thread: Option<String>,
) -> BoxStream<'static, StreamEvent> {
    let mut state = State {
        hub,
        rx,
        snapshot,
        thread,
        last: 0,
        queue: VecDeque::new(),
        done: false,
    };
    state.snapshot(true);
    stream::unfold(state, |mut state| async move {
        loop {
            if let Some(item) = state.queue.pop_front() {
                return Some((item, state));
            }
            if state.done {
                return None;
            }
            match state.rx.recv().await {
                Ok(event) => {
                    if event.wanted(state.thread.as_deref()) && event.sequence() > state.last {
                        state.last = event.sequence();
                        for item in event.items() {
                            // A removed thread's stream ends after saying so.
                            if item["type"] == "thread.removed" && state.thread.is_some() {
                                state.done = true;
                            }
                            state.queue.push_back(StreamEvent::Item(item.clone()));
                        }
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    // Jump to the live tail before reading the snapshot, so
                    // stale buffered items cannot lag the receiver again.
                    state.rx = state.rx.resubscribe();
                    state.snapshot(false);
                }
                Err(RecvError::Closed) => return None,
            }
        }
    })
    .boxed()
}

/// `shell.watch`: every project and every thread that is not deleted.
pub fn shell_stream(hub: Arc<Hub>) -> BoxStream<'static, StreamEvent> {
    let rx = hub.subscribe_shell();
    let snapshot: SnapshotFn = Arc::new(|store: &Store| {
        let projects = store.projects().map_err(store_error)?;
        let threads = store
            .threads()
            .map_err(store_error)?
            .into_iter()
            .map(|index| index.row)
            .collect();
        Ok(encode(&ShellItem::Snapshot(ShellSnapshot {
            sequence: store.high_water(),
            projects,
            threads,
        })))
    });
    live_stream(hub, rx, snapshot, None)
}

/// `threads.watch`: the thread's snapshot, then its commits. Ends after
/// `thread.removed`, or with `not_found` when the thread does not exist.
pub fn thread_stream(hub: Arc<Hub>, thread_id: &str) -> BoxStream<'static, StreamEvent> {
    let rx = hub.subscribe_threads();
    let id = thread_id.to_owned();
    let snapshot: SnapshotFn =
        Arc::new(
            move |store: &Store| match store.snapshot(&id).map_err(store_error)? {
                Some(snapshot) => Ok(encode(&ThreadItem::Snapshot(Box::new(snapshot)))),
                None => Err(Error::not_found("thread", &id)),
            },
        );
    live_stream(hub, rx, snapshot, Some(thread_id.to_owned()))
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, json};

    use super::*;
    use crate::hub::{Commit, ThreadWrite};

    fn commit(hub: &Hub, thread_id: &str) -> u64 {
        let Value::Object(item) = json!({"type": "queue", "queue": []}) else {
            unreachable!()
        };
        hub.commit(Commit {
            thread: Some(ThreadWrite {
                thread_id: thread_id.into(),
                items: vec![item],
                ..ThreadWrite::default()
            }),
            ..Commit::default()
        })
        .unwrap()
    }

    fn item(event: StreamEvent) -> Value {
        match event {
            StreamEvent::Item(item) => item,
            StreamEvent::Fail(error) => panic!("unexpected failure {error}"),
        }
    }

    #[tokio::test]
    async fn shell_snapshot_then_synced_then_live_commits() {
        let hub = Arc::new(Hub::new(Store::open_in_memory().unwrap()));
        commit(&hub, "t1");
        let mut stream = shell_stream(Arc::clone(&hub));
        let snapshot = item(stream.next().await.unwrap());
        assert_eq!(snapshot["type"], "snapshot");
        assert_eq!(snapshot["sequence"], 1);
        assert_eq!(
            item(stream.next().await.unwrap()),
            json!({"type": "synced"})
        );
        let sequence = hub
            .commit(Commit {
                projects: vec![crate::hub::ProjectWrite::Remove("p".into())],
                ..Commit::default()
            })
            .unwrap();
        let live = item(stream.next().await.unwrap());
        assert_eq!(live["type"], "project.removed");
        assert_eq!(live["sequence"], sequence);
    }

    #[tokio::test]
    async fn a_missing_thread_fails_not_found() {
        let hub = Arc::new(Hub::new(Store::open_in_memory().unwrap()));
        let mut stream = thread_stream(hub, "missing");
        match stream.next().await {
            Some(StreamEvent::Fail(error)) => assert_eq!(error.code.as_str(), "not_found"),
            _ => panic!("expected a failure"),
        }
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn a_lagging_subscriber_gets_a_fresh_snapshot() {
        let hub = Arc::new(Hub::with_capacity(Store::open_in_memory().unwrap(), 2, 2));
        let mut stream = shell_stream(Arc::clone(&hub));
        stream.next().await.unwrap();
        stream.next().await.unwrap();
        for _ in 0..5 {
            hub.commit(Commit {
                projects: vec![crate::hub::ProjectWrite::Remove("p".into())],
                ..Commit::default()
            })
            .unwrap();
        }
        let snapshot = item(stream.next().await.unwrap());
        assert_eq!(snapshot["type"], "snapshot");
        assert_eq!(snapshot["sequence"], 5);
        let _ = Map::<String, Value>::new();
    }
}
