//! Items of the `shell.watch` and `threads.watch` streams
//! (`docs/protocol/wire.md` §6).

use serde::{Deserialize, Serialize};

use super::entities::{Item, Project, QueueEntry, Request, Thread, Turn};

/// `shell.watch` (wire.md §6.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ShellItem {
    #[serde(rename = "snapshot")]
    Snapshot(ShellSnapshot),
    #[serde(rename = "synced")]
    Synced,
    #[serde(rename = "project")]
    Project { sequence: u64, project: Project },
    #[serde(rename = "project.removed", rename_all = "camelCase")]
    ProjectRemoved { sequence: u64, project_id: String },
    #[serde(rename = "thread")]
    Thread { sequence: u64, thread: Box<Thread> },
    #[serde(rename = "thread.removed", rename_all = "camelCase")]
    ThreadRemoved { sequence: u64, thread_id: String },
}

impl ShellItem {
    /// The commit sequence of a delta; `None` for `snapshot`/`synced`.
    pub fn sequence(&self) -> Option<u64> {
        match self {
            Self::Snapshot(_) | Self::Synced => None,
            Self::Project { sequence, .. }
            | Self::ProjectRemoved { sequence, .. }
            | Self::Thread { sequence, .. }
            | Self::ThreadRemoved { sequence, .. } => Some(*sequence),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShellSnapshot {
    pub sequence: u64,
    pub projects: Vec<Project>,
    pub threads: Vec<Thread>,
}

/// `threads.watch` (wire.md §6.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ThreadItem {
    #[serde(rename = "snapshot")]
    Snapshot(Box<ThreadSnapshot>),
    #[serde(rename = "synced")]
    Synced,
    #[serde(rename = "thread")]
    Thread { sequence: u64, thread: Box<Thread> },
    #[serde(rename = "turn")]
    Turn { sequence: u64, turn: Turn },
    #[serde(rename = "item")]
    Item { sequence: u64, item: Box<Item> },
    #[serde(rename = "item.delta", rename_all = "camelCase")]
    ItemDelta {
        sequence: u64,
        item_id: String,
        field: DeltaField,
        append: String,
    },
    #[serde(rename = "request")]
    Request { sequence: u64, request: Request },
    #[serde(rename = "queue")]
    Queue {
        sequence: u64,
        queue: Vec<QueueEntry>,
    },
    #[serde(rename = "thread.removed")]
    ThreadRemoved { sequence: u64 },
}

impl ThreadItem {
    /// The commit sequence of a delta; `None` for `snapshot`/`synced`.
    pub fn sequence(&self) -> Option<u64> {
        match self {
            Self::Snapshot(_) | Self::Synced => None,
            Self::Thread { sequence, .. }
            | Self::Turn { sequence, .. }
            | Self::Item { sequence, .. }
            | Self::ItemDelta { sequence, .. }
            | Self::Request { sequence, .. }
            | Self::Queue { sequence, .. }
            | Self::ThreadRemoved { sequence } => Some(*sequence),
        }
    }
}

/// The field an `item.delta` appends to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeltaField {
    Text,
    Output,
}

/// A thread with everything its feed shows (wire.md §6.6 `snapshot`, and
/// the result of `threads.get`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadSnapshot {
    pub sequence: u64,
    pub thread: Thread,
    pub turns: Vec<Turn>,
    /// By `ordinal`.
    pub items: Vec<Item>,
    /// Open requests and those of the latest turn.
    pub requests: Vec<Request>,
    /// By `position`.
    pub queue: Vec<QueueEntry>,
}
