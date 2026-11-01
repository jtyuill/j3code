//! `j3-server serve`: the headless j3 server (shieldy runs it under systemd).

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    process::ExitCode,
    time::Duration,
};

use clap::{Parser, Subcommand};
use j3_core::{
    app::{App, AppConfig},
    omp::OmpLauncher,
    serve::{DEFAULT_PORT, Server, tailscale_ipv4},
};
use tokio::signal;
use tracing::{error, info, warn};

/// How long `--tailscale` waits for tailscaled to report an address (boot ordering).
const TAILSCALE_WAIT: Duration = Duration::from_secs(120);

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
        addrs.push(SocketAddr::new(
            tailscale_ipv4(TAILSCALE_WAIT).await?,
            args.port,
        ));
    }
    addrs.extend(args.listen);

    let app = App::open(AppConfig {
        data_dir: data_dir.clone(),
        label,
        omp: OmpLauncher::new(args.omp).stderr_log(logs.join("omp.log")),
    })
    .await
    .map_err(|e| format!("opening j3 state in {}: {e}", data_dir.display()))?;

    let mut server = Server::bind(app, &addrs).await.map_err(|e| e.to_string())?;
    let result = tokio::select! {
        () = shutdown_signal() => {
            info!("shutdown requested");
            Ok(())
        }
        reason = server.failed() => Err(reason),
    };
    server.shutdown().await;
    result
}

fn default_data_dir() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir).join("j3"));
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set; pass --data-dir")?;
    Ok(PathBuf::from(home).join(".local/share/j3"))
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
