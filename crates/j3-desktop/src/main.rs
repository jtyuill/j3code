//! `j3`: the j3 desktop app. Talks to its own in-process j3 server (the
//! local environment) and to remote j3 servers over the t3 wire protocol.

use std::{path::PathBuf, sync::Arc};

use j3_desktop::{
    assist::{self, AssistOmp},
    device, environments,
    local_server::LocalServer,
    preferences::Preferences,
    services::Services,
    speech::SpeechEngine,
};
use tracing_subscriber::EnvFilter;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    // Before any other thread exists (`set_var`) and anything spawns `omp` or `tailscale`.
    extend_path();
    // The GPU device and pipeline build on their own thread while the rest starts.
    rune::prewarm_gpu();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("j3-tokio")
        .build()
        .expect("the tokio runtime starts");
    // Headless renders (`--snap`, `--tree`, scripts) never start a server; connections to
    // the local port then fail quietly. A `--scene` runs on offline fixtures
    // (`app::fixtures`) and needs no server either.
    let headless = rune::is_headless();
    let scene = std::env::args().any(|arg| arg == "--scene");
    let config_path = environments::config_path()
        .inspect_err(|e| tracing::warn!("{e}"))
        .ok();
    // The server reports this computer's name, so the setting is read before it starts.
    let hostname = gethostname::gethostname().to_string_lossy().into_owned();
    let label = device::name(
        Preferences::load(config_path.clone(), &hostname)
            .ui()
            .device_name
            .as_deref(),
    );
    // Starting is fast (SQLite open, bind); doing it before the window means the local
    // connection's first attempt already finds the server.
    let local =
        (!headless && !scene).then(|| Arc::new(runtime.block_on(LocalServer::start(label))));
    let data_dir = dirs::data_dir().map(|dir| dir.join("j3"));
    let assist = AssistOmp::new(
        assist::launcher(data_dir.as_deref()),
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
    );
    let speech = SpeechEngine::local(
        data_dir
            .as_deref()
            .unwrap_or(&std::env::temp_dir().join("j3"))
            .join("models"),
        runtime.handle().clone(),
    );
    if !headless {
        // Probing the GPU takes a moment; Settings and the first dictation then find it done.
        let probe = Arc::clone(&speech);
        std::thread::spawn(move || {
            tracing::info!("dictation runs on {}", probe.hardware().describe());
        });
    }
    let services = Arc::new(Services::new(
        runtime.handle().clone(),
        local,
        assist,
        speech,
        config_path,
        data_dir,
    ));

    let app = j3_desktop::app::app(Arc::clone(&services));
    // SIGTERM (logout, systemd) and SIGINT take the same orderly path as ⌘Q, so omp
    // sessions are stopped instead of orphaned.
    let handle = app.handle();
    runtime.block_on(async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate())
            .and_then(|term| Ok((term, signal(SignalKind::interrupt())?)))
        {
            Ok((mut term, mut int)) => {
                tokio::spawn(async move {
                    tokio::select! {
                        _ = term.recv() => {}
                        _ = int.recv() => {}
                    }
                    tracing::info!("termination signal received; quitting");
                    handle.quit();
                });
            }
            Err(e) => tracing::warn!("signal handlers unavailable: {e}"),
        }
    });
    if let Err(e) = app.try_run() {
        tracing::error!("opening the window failed: {e}");
        services.shutdown();
        std::process::exit(1);
    }
    // Closes connections, stops the local server's omp sessions and the assist omp
    // (bounded); then never waits on a stuck blocking task (an omp call) at exit.
    services.shutdown();
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
}

/// Finder (and launchd) start an app bundle with a minimal `PATH` that
/// misses where user tools live. j3 spawns `omp` and `tailscale` by name, so
/// without this the local server could not list models (an empty model
/// picker) or serve on the tailnet. Add the usual install locations to this
/// process's `PATH`, which every child inherits.
fn extend_path() {
    let Some(path) = std::env::var_os("PATH") else {
        return;
    };
    let home = dirs::home_dir();
    let joined = extend_paths(&path, home.as_deref(), |dir| dir.is_dir());
    if joined == path {
        return;
    }
    // SAFETY: called at the top of `main`, before the tokio runtime or any
    // other thread of ours exists, so nothing is reading the environment.
    unsafe { std::env::set_var("PATH", &joined) };
    tracing::info!("PATH extended for omp and tailscale");
}

/// `path` with the user tool directories that exist and are missing from it.
/// `exists` is the filesystem check, kept out so tests are deterministic.
fn extend_paths(
    path: &std::ffi::OsStr,
    home: Option<&std::path::Path>,
    exists: impl Fn(&std::path::Path) -> bool,
) -> std::ffi::OsString {
    let mut dirs: Vec<PathBuf> = std::env::split_paths(path).collect();
    let mut extras: Vec<PathBuf> = Vec::new();
    if let Some(home) = home {
        extras.push(home.join(".local/bin"));
        extras.push(home.join(".bun/bin"));
    }
    #[cfg(target_os = "macos")]
    extras.extend([
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ]);
    #[cfg(target_os = "linux")]
    extras.push(PathBuf::from("/usr/local/bin"));

    for extra in extras {
        if exists(&extra) && !dirs.contains(&extra) {
            dirs.push(extra);
        }
    }
    std::env::join_paths(&dirs).unwrap_or_else(|_| path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn the_path_gains_the_user_bin_directories() {
        let home = PathBuf::from("/Users/jacob");
        let path = OsString::from("/usr/bin:/bin");
        let extended = extend_paths(&path, Some(&home), |_| true);
        let dirs: Vec<PathBuf> = std::env::split_paths(&extended).collect();
        assert_eq!(
            &dirs[..2],
            &[PathBuf::from("/usr/bin"), PathBuf::from("/bin")]
        );
        assert!(dirs.contains(&home.join(".local/bin")));
        assert!(dirs.contains(&home.join(".bun/bin")));
        // Idempotent: an already-extended path gains nothing.
        let again = extend_paths(&extended, Some(&home), |_| true);
        assert_eq!(again, extended);
    }

    #[test]
    fn missing_directories_are_skipped() {
        let extended = extend_paths(
            &OsString::from("/bin"),
            Some(&PathBuf::from("/nope")),
            |_| false,
        );
        let dirs: Vec<PathBuf> = std::env::split_paths(&extended).collect();
        assert_eq!(dirs, [PathBuf::from("/bin")]);
    }
}
