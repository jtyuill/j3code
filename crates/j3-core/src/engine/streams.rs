//! `orchestration.subscribeShell` / `orchestration.subscribeThread` streams
//! (ios-surface §4.1–§4.2).
//!
//! Each stream subscribes to the hub broadcast first, then reads its snapshot
//! and the high-water mark under the store lock, emits the snapshot and
//! `synchronized`, then forwards live items whose sequence is above the last
//! one emitted. A lagging receiver gets a fresh snapshot, which clients accept
//! at any time.

use std::{collections::VecDeque, sync::Arc};

use futures_util::stream::{self, BoxStream, StreamExt};
use serde_json::{Value, json};
use tokio::sync::broadcast::{self, error::RecvError};

use crate::{
    hub::{Hub, ShellEvent, ThreadEvent},
    rpc::StreamItem,
    store::Store,
};

/// Builds a snapshot body from a consistent store view; `Err` is the typed
/// error JSON that ends the stream.
pub type SnapshotFn = Arc<dyn Fn(&Store) -> Result<Value, Value> + Send + Sync>;

fn synchronized() -> StreamItem {
    StreamItem::Value(json!({"kind": "synchronized"}))
}

/// Reads `(high water, snapshot body)` atomically.
fn read_snapshot(hub: &Hub, snapshot: &SnapshotFn) -> Result<(u64, Value), Value> {
    hub.read(|store| Ok((store.high_water(), snapshot(store)?)))
}

struct State<E> {
    hub: Arc<Hub>,
    rx: broadcast::Receiver<E>,
    snapshot: SnapshotFn,
    wrap: fn(u64, Value) -> Value,
    last: u64,
    queue: VecDeque<StreamItem>,
    done: bool,
}

trait Live: Clone + Send + Sync + 'static {
    fn sequence(&self) -> u64;
    fn item(&self) -> &Value;
    fn wanted(&self, filter: &Option<Arc<str>>) -> bool;
}

impl Live for ThreadEvent {
    fn sequence(&self) -> u64 {
        self.sequence
    }
    fn item(&self) -> &Value {
        &self.item
    }
    fn wanted(&self, filter: &Option<Arc<str>>) -> bool {
        filter.as_deref() == Some(&*self.thread_id)
    }
}

impl Live for ShellEvent {
    fn sequence(&self) -> u64 {
        self.sequence
    }
    fn item(&self) -> &Value {
        &self.item
    }
    fn wanted(&self, _: &Option<Arc<str>>) -> bool {
        true
    }
}

fn live_stream<E: Live>(
    hub: Arc<Hub>,
    rx: broadcast::Receiver<E>,
    snapshot: SnapshotFn,
    wrap: fn(u64, Value) -> Value,
    filter: Option<Arc<str>>,
) -> BoxStream<'static, StreamItem> {
    let mut state = State {
        hub,
        rx,
        snapshot,
        wrap,
        last: 0,
        queue: VecDeque::new(),
        done: false,
    };
    match read_snapshot(&state.hub, &state.snapshot) {
        Ok((sequence, body)) => {
            state.last = sequence;
            state
                .queue
                .push_back(StreamItem::Value((state.wrap)(sequence, body)));
            state.queue.push_back(synchronized());
        }
        Err(error) => {
            state.queue.push_back(StreamItem::Fail(error));
            state.done = true;
        }
    }
    stream::unfold((state, filter), |(mut state, filter)| async move {
        loop {
            if let Some(item) = state.queue.pop_front() {
                return Some((item, (state, filter)));
            }
            if state.done {
                return None;
            }
            match state.rx.recv().await {
                Ok(event) => {
                    if event.wanted(&filter) && event.sequence() > state.last {
                        state.last = event.sequence();
                        state
                            .queue
                            .push_back(StreamItem::Value(event.item().clone()));
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    // Jump to the live tail before reading the snapshot, so
                    // stale buffered items cannot lag the receiver again.
                    state.rx = state.rx.resubscribe();
                    match read_snapshot(&state.hub, &state.snapshot) {
                        Ok((sequence, body)) => {
                            state.last = sequence;
                            state
                                .queue
                                .push_back(StreamItem::Value((state.wrap)(sequence, body)));
                        }
                        Err(error) => {
                            state.queue.push_back(StreamItem::Fail(error));
                            state.done = true;
                        }
                    }
                }
                Err(RecvError::Closed) => return None,
            }
        }
    })
    .boxed()
}

/// `subscribeShell`. `snapshot` returns the `OrchestrationV2ShellSnapshot`
/// body without `snapshotSequence`, which is filled in here.
pub fn shell_stream(hub: Arc<Hub>, snapshot: SnapshotFn) -> BoxStream<'static, StreamItem> {
    let rx = hub.subscribe_shell();
    live_stream(
        hub,
        rx,
        snapshot,
        |sequence, mut body| {
            body["snapshotSequence"] = json!(sequence);
            json!({"kind": "snapshot", "snapshot": body})
        },
        None,
    )
}

/// `subscribeThread`. `snapshot` returns the `OrchestrationV2ThreadProjection`.
pub fn thread_stream(
    hub: Arc<Hub>,
    thread_id: &str,
    snapshot: SnapshotFn,
) -> BoxStream<'static, StreamItem> {
    let rx = hub.subscribe_threads();
    live_stream(
        hub,
        rx,
        snapshot,
        |sequence, projection| json!({"kind": "snapshot", "snapshotSequence": sequence, "projection": projection}),
        Some(Arc::from(thread_id)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::{Commit, PendingEvent, ThreadWrite};

    fn commit_event(hub: &Hub, thread_id: &str) -> u64 {
        hub.commit(Commit {
            thread: Some(ThreadWrite {
                thread_id: thread_id.into(),
                events: vec![PendingEvent {
                    event_type: "message.updated",
                    payload: json!({}),
                    run_id: None,
                    node_id: None,
                }],
                ..ThreadWrite::default()
            }),
            ..Commit::default()
        })
        .unwrap()
    }

    fn value(item: StreamItem) -> Value {
        match item {
            StreamItem::Value(value) => value,
            StreamItem::Fail(error) => panic!("unexpected failure {error}"),
        }
    }

    fn counting_snapshot() -> SnapshotFn {
        Arc::new(|store: &Store| Ok(json!({"at": store.high_water()})))
    }

    #[tokio::test]
    async fn thread_stream_snapshot_then_filtered_live_events() {
        let hub = Arc::new(Hub::new(Store::open_in_memory().unwrap()));
        commit_event(&hub, "t1");
        let mut stream = thread_stream(Arc::clone(&hub), "t1", counting_snapshot());
        let snapshot = value(stream.next().await.unwrap());
        assert_eq!(snapshot["kind"], "snapshot");
        assert_eq!(snapshot["snapshotSequence"], 1);
        assert_eq!(snapshot["projection"]["at"], 1);
        assert_eq!(value(stream.next().await.unwrap())["kind"], "synchronized");
        commit_event(&hub, "other");
        let third = commit_event(&hub, "t1");
        let live = value(stream.next().await.unwrap());
        assert_eq!(live["kind"], "event");
        assert_eq!(live["sequence"], third);
    }

    #[tokio::test]
    async fn lagged_subscriber_gets_fresh_snapshot() {
        let hub = Arc::new(Hub::with_capacity(Store::open_in_memory().unwrap(), 2, 2));
        let mut stream = thread_stream(Arc::clone(&hub), "t1", counting_snapshot());
        stream.next().await.unwrap();
        stream.next().await.unwrap();
        for _ in 0..5 {
            commit_event(&hub, "t1");
        }
        let item = value(stream.next().await.unwrap());
        assert_eq!(item["kind"], "snapshot");
        assert_eq!(item["snapshotSequence"], 5);
        // Nothing older than the snapshot follows it.
        let next = commit_event(&hub, "t1");
        let live = value(stream.next().await.unwrap());
        assert_eq!(live["sequence"], next);
    }

    #[tokio::test]
    async fn failing_snapshot_ends_with_typed_failure() {
        let hub = Arc::new(Hub::new(Store::open_in_memory().unwrap()));
        let mut stream = thread_stream(
            hub,
            "missing",
            Arc::new(|_: &Store| Err(json!({"_tag": "OrchestrationV2GetThreadProjectionError"}))),
        );
        assert!(matches!(stream.next().await, Some(StreamItem::Fail(_))));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn shell_stream_fills_snapshot_sequence() {
        let hub = Arc::new(Hub::new(Store::open_in_memory().unwrap()));
        commit_event(&hub, "t1");
        let mut stream = shell_stream(Arc::clone(&hub), counting_snapshot());
        let snapshot = value(stream.next().await.unwrap());
        assert_eq!(snapshot["snapshot"]["snapshotSequence"], 1);
        assert_eq!(value(stream.next().await.unwrap())["kind"], "synchronized");
    }
}
