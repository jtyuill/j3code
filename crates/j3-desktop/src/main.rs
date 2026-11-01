//! `j3`: the j3 desktop app. Talks to its own in-process j3 server (the
//! local environment) and to remote j3 servers over the t3 wire protocol.

// The server-config fixtures in unit tests are large `json!` literals.
#![recursion_limit = "256"]

mod actions;
mod archived;
mod completion;
mod composer;
mod dialogs;
mod environment;
mod environments;
mod feed;
mod feed_view;
mod local_server;
mod models;
mod new_thread;
mod services;
mod sidebar;
mod thread_view;
mod workspace;

use std::sync::Arc;

use gpui_kit::assets::Assets;
use gpui_kit::component::Theme;
use gpui_kit::{App, AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};
use tracing_subscriber::EnvFilter;

use crate::{local_server::LocalServer, services::Services, workspace::Workspace};

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
    let services = Services::new(runtime.handle().clone(), local);

    gpui_kit::application()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            cx.set_global(services);
            actions::init(cx);
            open_main_window(cx);
            cx.activate(true);
        });
    drop(runtime);
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
        cx.quit();
    }
}
