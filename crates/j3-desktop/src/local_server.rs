//! The in-process j3 server that backs the local environment.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::Mutex,
    time::Duration,
};

use j3_core::{
    app::{App, AppConfig},
    omp::OmpLauncher,
    serve::{DEFAULT_PORT, Server, tailscale_ipv4},
};
use tracing::{info, warn};

/// The URL the local environment connects to, whether or not this process
/// managed to serve it.
pub const LOCAL_URL: &str = "http://127.0.0.1:3780";

/// How starting the in-process server went. Shown in the local
/// environment's header; the local connection targets [`LOCAL_URL`] in
/// every case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalServerStatus {
    /// This process serves on these addresses.
    Serving(Vec<SocketAddr>),
    /// Binding failed, typically because a `j3-server` already listens
    /// there; the app uses that server instead.
    External(String),
    /// The server could not be opened at all (data directory, database).
    Failed(String),
}

impl LocalServerStatus {
    /// One line for the environment's status tooltip.
    pub fn summary(&self) -> String {
        match self {
            Self::Serving(addrs) => {
                let addrs: Vec<String> = addrs.iter().map(ToString::to_string).collect();
                format!("Serving on {}", addrs.join(", "))
            }
            Self::External(reason) => format!("Using the server already on port 3780 ({reason})"),
            Self::Failed(reason) => format!("Local server failed to start: {reason}"),
        }
    }
}

/// The running server, if any. Taken once at shutdown.
pub struct LocalServer {
    status: LocalServerStatus,
    /// What the local environment connects to.
    url: String,
    server: Mutex<Option<Server>>,
}

impl LocalServer {
    /// Opens the app in `dirs::data_dir()/j3` and binds `127.0.0.1:3780`
    /// plus the Tailscale IPv4 if there is one. Never fails: problems are
    /// reported through [`LocalServer::status`].
    pub async fn start() -> Self {
        let (status, server) = match Self::try_start().await {
            Ok(server) => (LocalServerStatus::Serving(server.addrs().to_vec()), Some(server)),
            Err(status) => (status, None),
        };
        match &status {
            LocalServerStatus::Serving(_) => info!("{}", status.summary()),
            _ => warn!("{}", status.summary()),
        }
        Self {
            status,
            url: LOCAL_URL.to_owned(),
            server: Mutex::new(server),
        }
    }

    /// Wraps a server bound elsewhere (tests bind an ephemeral port); the
    /// local environment connects to its first address.
    #[cfg(test)]
    pub fn from_server(server: Server) -> Self {
        let addrs = server.addrs().to_vec();
        Self {
            url: format!("http://{}", addrs[0]),
            status: LocalServerStatus::Serving(addrs),
            server: Mutex::new(Some(server)),
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    async fn try_start() -> Result<Server, LocalServerStatus> {
        let data_dir = dirs::data_dir()
            .map(|dir| dir.join("j3"))
            .ok_or_else(|| LocalServerStatus::Failed("no data directory".to_owned()))?;
        let logs = data_dir.join("logs");
        std::fs::create_dir_all(&logs).map_err(|e| {
            LocalServerStatus::Failed(format!("creating {}: {e}", logs.display()))
        })?;
        let label = gethostname::gethostname().to_string_lossy().into_owned();
        let app = App::open(AppConfig {
            data_dir: data_dir.clone(),
            label,
            omp: OmpLauncher::new(PathBuf::from("omp")).stderr_log(logs.join("omp.log")),
        })
        .await
        .map_err(|e| LocalServerStatus::Failed(format!("{}: {e}", data_dir.display())))?;

        let mut addrs = vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_PORT)];
        match tailscale_ipv4(Duration::ZERO).await {
            Ok(ip) => addrs.push(SocketAddr::new(ip, DEFAULT_PORT)),
            Err(e) => info!("not serving on Tailscale: {e}"),
        }
        match Server::bind(app.clone(), &addrs).await {
            Ok(server) => Ok(server),
            Err(e) => {
                // Nothing is served; stop the omp sessions `open` may recover.
                app.shutdown().await;
                Err(LocalServerStatus::External(e.to_string()))
            }
        }
    }

    pub fn status(&self) -> &LocalServerStatus {
        &self.status
    }

    /// Stops the listeners and every omp session. Later calls do nothing.
    pub async fn shutdown(&self) {
        let server = self
            .server
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(server) = server {
            server.shutdown().await;
            info!("local server stopped");
        }
    }
}
