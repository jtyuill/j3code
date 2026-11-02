//! `j3`: the j3 desktop app. Talks to its own in-process j3 server (the
//! local environment) and to remote j3 servers over the t3 wire protocol.

// The server-config fixtures in unit tests are large `json!` literals.
#![recursion_limit = "256"]

mod actions;
mod agent_hub;
mod appearance;
mod archived;
mod assets;
mod attention;
mod completion;
mod composer;
mod dialogs;
mod environment;
mod environments;
mod feed;
mod feed_view;
mod file_pane;
mod git_control;
mod goto;
mod local_server;
mod logos;
mod models;
mod new_thread;
mod preferences;
mod services;
mod settings_view;
mod sidebar;
mod terminal_drawer;
mod terminal_view;
mod thread_view;
mod titlebar;
mod ui_settings;
#[cfg(test)]
mod ui_tests;
mod usage;
mod workspace;

use std::{path::PathBuf, sync::Arc};

use gpui_kit::component::Theme;
use gpui_kit::{
    App, AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size,
};
use tracing_subscriber::EnvFilter;

use crate::{
    assets::AppAssets, local_server::LocalServer, services::Services, workspace::Workspace,
};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    // Before the runtime starts and anything spawns `omp` or `tailscale`.
    extend_path();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("j3-tokio")
        .build()
        .expect("the tokio runtime starts");
    // Starting is fast (SQLite open, bind); doing it before the window means
    // the local connection's first attempt already finds the server.
    let local = Arc::new(runtime.block_on(LocalServer::start()));
    let config_path = environments::config_path()
        .inspect_err(|e| tracing::warn!("{e}"))
        .ok();
    let services = Services::new(runtime.handle().clone(), local, config_path);
    let termination = runtime.block_on(termination_signal());

    gpui_kit::application()
        .with_assets(AppAssets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            cx.set_global(services);
            actions::init(cx);
            // SIGTERM (logout, systemd) and SIGINT take the same orderly path
            // as ⌘Q, so omp sessions are stopped instead of orphaned.
            cx.spawn(async move |cx| {
                if termination.await.is_ok() {
                    tracing::info!("termination signal received; quitting");
                    cx.update(actions::quit);
                }
            })
            .detach();
            open_main_window(cx);
            cx.activate(true);
        });
    // Never wait on a stuck blocking task (e.g. an omp call) at exit.
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
}

/// Resolves when the process receives SIGTERM or SIGINT. The listeners are
/// installed before this returns, so no signal is missed while the app boots.
async fn termination_signal() -> tokio::sync::oneshot::Receiver<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let (tx, rx) = tokio::sync::oneshot::channel();
    let listeners = signal(SignalKind::terminate())
        .and_then(|term| Ok((term, signal(SignalKind::interrupt())?)));
    match listeners {
        Ok((mut term, mut int)) => {
            tokio::spawn(async move {
                tokio::select! {
                    _ = term.recv() => {}
                    _ = int.recv() => {}
                }
                let _ = tx.send(());
            });
        }
        Err(e) => tracing::warn!("signal handlers unavailable: {e}"),
    }
    rx
}

fn open_main_window(cx: &mut App) {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1280.), px(820.)),
            cx,
        ))),
        window_min_size: Some(size(px(720.), px(480.))),
        // No titlebar of its own: the system one is hidden and the traffic
        // lights sit on the panes' header row (see `titlebar`). The workspace
        // re-centres them when the row's height changes with the zoom.
        titlebar: Some(TitlebarOptions {
            title: Some("j3".into()),
            appears_transparent: true,
            traffic_light_position: Some(titlebar::traffic_light_position(
                Theme::global(cx).font_size * titlebar::ROW_HEIGHT,
            )),
        }),
        ..Default::default()
    };
    let opened = gpui_kit::open_window(options, cx, |window, cx| {
        Theme::sync_system_appearance(Some(window), cx);
        cx.new(|cx| Workspace::new(window, cx))
    });
    if let Err(e) = opened {
        tracing::error!("opening the window failed: {e:#}");
        let connections = Workspace::global_connections(cx);
        Services::exit(connections, 1, cx);
    }
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
    // other thread exists, so nothing is reading the environment.
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
