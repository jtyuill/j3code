//! UniFFI bindings over the protocol v2 client (`j3_client`) and the
//! feed text preparation (`j3_text`) for the SwiftUI iOS app
//! (`docs/m8.md` D1).
//!
//! # Surface
//!
//! [`Host`] is one server: `watch` and `watch_thread` push immutable
//! snapshots ([`HostSnapshot`], [`ThreadSnapshot`]) to Swift listeners, and
//! its `async` methods are the app's commands. Errors are [`ClientError`].
//! The free functions in [`text`] prepare message text and [`markdown`]
//! parses assistant markdown into blocks.
//!
//! # Runtime
//!
//! The library owns one multi-threaded tokio runtime with two workers,
//! started on first use and kept for the life of the process. Every
//! [`Host`] passes its handle to `Connection::open`, so sockets, timers,
//! HTTP and the reconnect supervisor all run there.
//!
//! Exported `async` functions are plain UniFFI futures, polled by Swift's
//! executor: they do not use UniFFI's `tokio` feature. That feature wraps
//! each future in `async_compat::Compat`, which enters a tokio context on
//! every poll and, on threads without one (all of Swift's), lazily starts a
//! second runtime of its own. It is not needed: every `j3_client` call
//! spawns its work onto the connection's runtime and awaits the join
//! handle, which any executor can do.
//!
//! # Callbacks
//!
//! [`HostListener`] and [`ThreadListener`] are foreign traits that Swift
//! implements. They are called synchronously on a runtime worker thread,
//! never the main thread, so the Swift side hops to the main actor (e.g.
//! `Task { @MainActor in model.apply(snapshot) }`) and returns at once.
//!
//! # JSON fields
//!
//! Values whose shape the protocol leaves open cross as JSON text: a tool
//! item's `input_json`, a transcript tool call's `input_json`, and a server
//! error's `data_json`. Everything else is a typed record or enum.

uniffi::setup_scaffolding!();

mod error;
mod host;
pub mod markdown;
pub mod text;
mod types;

use std::sync::LazyLock;

pub use error::ClientError;
pub use host::{BtwListener, Host, HostListener, Subscription, ThreadListener};
pub use types::*;

/// Worker threads of the library runtime: the socket, its supervisor and
/// the occasional upload; listener calls borrow one while they run.
const WORKER_THREADS: usize = 2;

/// The library's runtime, started on first use.
static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .thread_name("j3-mobile")
        .enable_all()
        .build()
        .expect("cannot start the j3-mobile tokio runtime")
});
