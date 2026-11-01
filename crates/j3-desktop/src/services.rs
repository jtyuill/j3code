//! App-wide services: the tokio runtime that drives networking and the
//! in-process server, and orderly shutdown.

use std::{
    cell::Cell,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use gpui_kit::{App, Global};
use j3_client::Connection;
use tokio::runtime::Handle;

use crate::local_server::LocalServer;

/// How long quitting waits for the local server to stop its omp sessions.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(8);

pub struct Services {
    runtime: Handle,
    local: Arc<LocalServer>,
    quitting: Rc<Cell<bool>>,
}

impl Global for Services {}

impl Services {
    pub fn new(runtime: Handle, local: Arc<LocalServer>) -> Self {
        Self {
            runtime,
            local,
            quitting: Rc::new(Cell::new(false)),
        }
    }

    pub fn global(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    pub fn runtime(&self) -> &Handle {
        &self.runtime
    }

    pub fn local(&self) -> &Arc<LocalServer> {
        &self.local
    }

    /// Runs `future` on the tokio runtime and resolves with its output on
    /// any executor (for reqwest and other reactor-bound work).
    pub fn run<F>(&self, future: F) -> impl Future<Output = Option<F::Output>> + use<F>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.runtime.spawn(async move {
            let _ = tx.send(future.await);
        });
        async move { rx.await.ok() }
    }

    /// Closes `connections`, stops the local server (bounded by
    /// [`SHUTDOWN_WAIT`]) and quits. Repeated calls while quitting do nothing.
    pub fn quit(connections: Vec<Connection>, cx: &mut App) {
        let services = Self::global(cx);
        if services.quitting.replace(true) {
            return;
        }
        let local = Arc::clone(&services.local);
        let done = services.run(async move {
            let stop = async move {
                for connection in &connections {
                    connection.close().await;
                }
                local.shutdown().await;
            };
            if tokio::time::timeout(SHUTDOWN_WAIT, stop).await.is_err() {
                tracing::warn!("shutdown timed out after {SHUTDOWN_WAIT:?}");
            }
        });
        cx.spawn(async move |cx| {
            done.await;
            cx.update(|cx| cx.quit());
        })
        .detach();
    }
}
