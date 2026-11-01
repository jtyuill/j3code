//! Serving an [`App`] over HTTP/WebSocket on several addresses, shared by
//! `j3-server` and the desktop app's in-process server.

use std::{
    io,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use tokio::{net::TcpListener, process::Command, sync::watch, task::JoinSet};
use tracing::{info, warn};

use crate::{
    app::App,
    http::{HttpState, router},
    rpc::RpcHandler,
};

/// Default port for j3 servers (t3code keeps 3773).
pub const DEFAULT_PORT: u16 = 3780;
/// How long in-flight HTTP requests get after shutdown starts.
const SHUTDOWN_DRAIN: Duration = Duration::from_secs(5);

/// Listeners serving one [`App`].
pub struct Server {
    app: Arc<App>,
    addrs: Vec<SocketAddr>,
    stop: watch::Sender<bool>,
    tasks: JoinSet<Result<(), String>>,
}

#[derive(Debug, thiserror::Error)]
#[error("binding {addr}: {source}")]
pub struct BindError {
    pub addr: SocketAddr,
    #[source]
    pub source: io::Error,
}

impl Server {
    /// Binds every address (duplicates ignored) and starts serving `app`.
    pub async fn bind(app: Arc<App>, addrs: &[SocketAddr]) -> Result<Self, BindError> {
        let rpc: Arc<dyn RpcHandler> = app.clone();
        let routes = router(HttpState {
            descriptor: app.descriptor(),
            rpc,
            attachments: app.attachments(),
        });
        let (stop, stop_rx) = watch::channel(false);
        let mut tasks = JoinSet::new();
        let mut bound = Vec::new();
        for &addr in addrs {
            if bound.contains(&addr) {
                continue;
            }
            let listener = TcpListener::bind(addr)
                .await
                .map_err(|source| BindError { addr, source })?;
            info!("listening on http://{addr}");
            bound.push(addr);
            let routes = routes.clone();
            let mut stop = stop_rx.clone();
            tasks.spawn(async move {
                axum::serve(listener, routes)
                    .with_graceful_shutdown(async move {
                        let _ = stop.wait_for(|stop| *stop).await;
                    })
                    .await
                    .map_err(|e| format!("server on {addr}: {e}"))
            });
        }
        Ok(Self {
            app,
            addrs: bound,
            stop,
            tasks,
        })
    }

    pub fn addrs(&self) -> &[SocketAddr] {
        &self.addrs
    }

    /// Resolves only if a listener stops on its own, with the reason.
    pub async fn failed(&mut self) -> String {
        match self.tasks.join_next().await {
            Some(Ok(Ok(()))) => "a listener stopped unexpectedly".into(),
            Some(Ok(Err(message))) => message,
            Some(Err(join)) => join.to_string(),
            None => std::future::pending().await,
        }
    }

    /// Stops the listeners (cutting open WebSockets after a short drain) and
    /// then every omp session.
    pub async fn shutdown(mut self) {
        let _ = self.stop.send(true);
        let drained = tokio::time::timeout(SHUTDOWN_DRAIN, async {
            while self.tasks.join_next().await.is_some() {}
        })
        .await;
        if drained.is_err() {
            self.tasks.abort_all();
        }
        info!("shutting down omp sessions");
        self.app.shutdown().await;
    }
}

/// This host's Tailscale IPv4 address from `tailscale ip -4`, polling until
/// tailscaled reports one or `wait` passes (boot ordering under systemd).
pub async fn tailscale_ipv4(wait: Duration) -> Result<IpAddr, String> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let reason = match Command::new("tailscale").args(["ip", "-4"]).output().await {
            Ok(out) if out.status.success() => {
                let text = String::from_utf8_lossy(&out.stdout);
                match text.lines().next().map(str::trim).map(str::parse::<IpAddr>) {
                    Some(Ok(ip)) => return Ok(ip),
                    _ => format!("unexpected `tailscale ip -4` output: {text:?}"),
                }
            }
            Ok(out) => String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            Err(e) => format!("running tailscale: {e}"),
        };
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("no Tailscale address: {reason}"));
        }
        warn!("waiting for Tailscale: {reason}");
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
