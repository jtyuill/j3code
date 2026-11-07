//! App-wide services (spec §2.3): the tokio runtime that drives networking and the
//! in-process server, this machine's assist omp and dictation engine, and orderly shutdown.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use j3_client::Connection;
use tokio::runtime::Handle;

use crate::{assist::AssistOmp, local_server::LocalServer, speech::SpeechEngine};

/// How long quitting waits for the connections, the local server's omp sessions and the
/// assist omp to stop.
pub const SHUTDOWN_WAIT: Duration = Duration::from_secs(8);

pub struct Services {
    pub runtime: Handle,
    /// The in-process server; `None` in headless runs, which never start one.
    pub local: Option<Arc<LocalServer>>,
    /// The omp behind the composer's word prediction.
    pub assist: Arc<AssistOmp>,
    /// The composer's dictation.
    pub speech: Arc<SpeechEngine>,
    /// `desktop.json`; `None` when the system has no config directory.
    pub config_path: Option<PathBuf>,
    /// `<data>/j3`; `None` when the system has no data directory.
    pub data_dir: Option<PathBuf>,
    /// Every environment's connection, closed on shutdown. Environments register theirs
    /// when they open and remove it when they are dropped from the list.
    connections: Mutex<Vec<Connection>>,
}

impl Services {
    pub fn new(
        runtime: Handle,
        local: Option<Arc<LocalServer>>,
        assist: Arc<AssistOmp>,
        speech: Arc<SpeechEngine>,
        config_path: Option<PathBuf>,
        data_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            runtime,
            local,
            assist,
            speech,
            config_path,
            data_dir,
            connections: Mutex::new(Vec::new()),
        }
    }

    /// The local environment's URL: this process's server, or the one already on its port.
    pub fn local_url(&self) -> &str {
        crate::local_server::LOCAL_URL
    }

    pub(crate) fn register(&self, connection: &Connection) {
        self.lock().push(connection.clone());
    }

    pub(crate) fn unregister(&self, connection: &Connection) {
        self.lock().retain(|c| c.http_base() != connection.http_base());
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Connection>> {
        self.connections.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Closes every connection, stops the local server and the assist omp, bounded by
    /// [`SHUTDOWN_WAIT`]. Blocks the calling thread (main, after the window's loop ends).
    pub fn shutdown(&self) {
        let connections = std::mem::take(&mut *self.lock());
        let local = self.local.clone();
        let assist = Arc::clone(&self.assist);
        self.runtime.block_on(async move {
            let stop = async move {
                for connection in &connections {
                    connection.close().await;
                }
                let local = async {
                    if let Some(local) = local {
                        local.shutdown().await;
                    }
                };
                tokio::join!(local, assist.close());
            };
            if tokio::time::timeout(SHUTDOWN_WAIT, stop).await.is_err() {
                tracing::warn!("shutdown timed out after {SHUTDOWN_WAIT:?}");
            }
        });
    }
}
