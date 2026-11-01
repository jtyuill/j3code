//! j3 protocol client: one [`Connection`] per j3 server, speaking the
//! t3-compatible wire protocol (`docs/protocol/effect-rpc.md`,
//! `docs/protocol/ios-surface.md`).
//!
//! GPUI-agnostic and executor-agnostic: sockets and HTTP run on the tokio
//! runtime handed to [`Connection::open`]; every public `async fn` can be
//! awaited from any executor, and state is published through
//! [`tokio::sync::watch`].

mod connection;
mod error;
mod projection;
mod rpc;
mod shell;
mod supervisor;
mod thread;

pub use connection::{Answer, Connection, SendMode, ThreadHandle};
pub use error::Error;
/// The wire types (`j3_protocol::t3`).
pub use j3_protocol::t3;
pub use shell::{ConnectionState, ConnectionStatus, ShellState, ThreadActivity, WorkspaceCommands};
pub use thread::{PendingUserInput, QueuedRun, ThreadState, ThreadSync, TodoPlan};
