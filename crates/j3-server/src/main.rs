//! `j3-server serve`: the headless j3 server (shieldy runs it under systemd).

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    process::ExitCode,
    sync::Arc,
    time::Duration,
};

use clap::{Parser, Subcommand};
use j3_core::{
    app::{App, AppConfig},
    http::{HttpState, router},
    omp::OmpLauncher,
    rpc::RpcHandler,
};
use tokio::{net::TcpListener, process::Command, signal, task::JoinSet};
use tracing::{error, info, warn};

/// Not t3code's 3773, so both servers can run on one host.
const DEFAULT_PORT: u16 = 3780;
/// How long `--tailscale` waits for tailscaled to report an address (boot ordering).
const TAILSCALE_WAIT: Duration = Duration::from_secs(120);
/// How long in-flight HTTP requests get after a shutdown signal.
const SHUTDOWN_DRAIN: Duration = Duration::from_secs(5);

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve j3 over HTTP/WebSocket.
    Serve(ServeArgs),
}

#[derive(clap::Args)]
struct ServeArgs {
    /// Port used for 127.0.0.1 and the Tailscale address.
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,
    /// Also listen on this host's Tailscale IPv4 address (`tailscale ip -4`).
    #[arg(long)]
    tailscale: bool,
    /// Extra listen addresses (`ip:port`), repeatable.
    #[arg(long = "listen")]
    listen: Vec<SocketAddr>,
    /// State directory [default: $XDG_DATA_HOME/j3 or ~/.local/share/j3].
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// omp binary.
    #[arg(long, default_value = "omp")]
    omp: PathBuf,
    /// Environment label shown in clients [default: hostname].
    #[arg(long)]
    label: Option<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let Cli {
        command: Cmd::Serve(args),
    } = Cli::parse();
    match serve(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            error!("{message}");
            ExitCode::FAILURE
        }
    }
}

async fn serve(args: ServeArgs) -> Result<(), String> {
    let data_dir = match args.data_dir {
        Some(dir) => dir,
        None => default_data_dir()?,
    };
    let logs = data_dir.join("logs");
    std::fs::create_dir_all(&logs).map_err(|e| format!("creating {}: {e}", logs.display()))?;
    let label = args
        .label
        .unwrap_or_else(|| gethostname::gethostname().to_string_lossy().into_owned());

    let mut addrs = vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), args.port)];
    if args.tailscale {
        addrs.push(SocketAddr::new(tailscale_ipv4().await?, args.port));
    }
    addrs.extend(args.listen);
    addrs.dedup();

    let app = App::open(AppConfig {
        data_dir: data_dir.clone(),
        label,
        omp: OmpLauncher::new(args.omp).stderr_log(logs.join("omp.log")),
    })
    .await
    .map_err(|e| format!("opening j3 state in {}: {e}", data_dir.display()))?;

    let rpc: Arc<dyn RpcHandler> = app.clone();
    let routes = router(HttpState {
        descriptor: app.descriptor(),
        rpc,
        attachments: app.attachments(),
    });

    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let mut servers = JoinSet::new();
    for addr in addrs {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| format!("binding {addr}: {e}"))?;
        info!("listening on http://{addr}");
        let routes = routes.clone();
        let mut stop = stop_rx.clone();
        servers.spawn(async move {
            axum::serve(listener, routes)
                .with_graceful_shutdown(async move {
                    let _ = stop.wait_for(|stop| *stop).await;
                })
                .await
                .map_err(|e| format!("server on {addr}: {e}"))
        });
    }

    let mut result = Ok(());
    tokio::select! {
        () = shutdown_signal() => info!("shutdown requested"),
        Some(joined) = servers.join_next() => {
            result = joined.map_err(|e| e.to_string()).and_then(|served| served);
            if result.is_ok() {
                result = Err("a listener stopped unexpectedly".into());
            }
        }
    }
    let _ = stop_tx.send(true);
    // Open WebSockets never finish on their own; give requests a moment, then cut them.
    if tokio::time::timeout(SHUTDOWN_DRAIN, async {
        while servers.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        servers.abort_all();
    }
    info!("shutting down omp sessions");
    app.shutdown().await;
    result
}

fn default_data_dir() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir).join("j3"));
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set; pass --data-dir")?;
    Ok(PathBuf::from(home).join(".local/share/j3"))
}

/// Polls `tailscale ip -4` until tailscaled reports an address.
async fn tailscale_ipv4() -> Result<IpAddr, String> {
    let deadline = tokio::time::Instant::now() + TAILSCALE_WAIT;
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
            return Err(format!(
                "no Tailscale address after {TAILSCALE_WAIT:?}: {reason}"
            ));
        }
        warn!("waiting for Tailscale: {reason}");
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn shutdown_signal() {
    let interrupt = signal::ctrl_c();
    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(e) => {
                warn!("SIGTERM handler unavailable: {e}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = interrupt => {}
        _ = terminate => {}
    }
}
