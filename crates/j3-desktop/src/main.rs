//! `j3`: the j3 desktop app. Talks to its own in-process j3 server (the
//! local environment) and to remote j3 servers over the t3 wire protocol.

// The server-config fixtures in unit tests are large `json!` literals.
#![recursion_limit = "256"]

mod actions;
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
mod goto;
mod local_server;
mod models;
mod new_thread;
mod preferences;
mod services;
mod settings_view;
mod sidebar;
mod thread_view;
mod ui_settings;
#[cfg(test)]
mod ui_tests;
mod workspace;

use std::sync::Arc;

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
        titlebar: Some(TitlebarOptions {
            title: Some("j3".into()),
            ..Default::default()
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
