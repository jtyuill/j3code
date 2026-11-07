//! The window (spec §3): the sidebar and the main pane in a rune-kit `SplitView`, or Settings
//! as a full-window page; the version bar along the bottom; dialogs and toasts in the
//! overlay layer; the whole drawn at the user's zoom.

use rune::{Cx, El, col, el, row, text};
use rune_kit::{
    Kit,
    shell::{SplitView, Toolbar},
};

use super::{J3, Main};
use crate::{
    actions::{Action, shortcut_label},
    theme::{self, Metrics},
};

/// What every feature view gets besides the state and `Cx`: the kit, the overlay layer
/// (popovers, menus, dialogs, toasts) and the metrics. Feature views are
/// `fn …(s: &J3, cx: &mut Cx<J3>, ui: &mut Ui, …) -> El`.
pub struct Ui<'a> {
    pub k: &'a Kit,
    pub layer: &'a mut Vec<El>,
    pub m: &'a Metrics,
}

/// `v0.1.0 · d2112d3`: the build's version and source commit.
pub const VERSION_LABEL: &str =
    concat!("v", env!("CARGO_PKG_VERSION"), " · ", env!("J3_GIT_COMMIT"));

pub fn view(s: &J3, cx: &mut Cx<J3>) -> El {
    let k = theme::kit(cx.system_dark(), s.prefs.ui());
    let m = Metrics::new(s.prefs.ui());
    cx.foreground(k.c.text_base);
    let mut layer = Vec::new();
    let mut ui = Ui {
        k: &k,
        layer: &mut layer,
        m: &m,
    };
    let content = if s.route.settings {
        crate::settings::view(s, cx, &mut ui)
    } else {
        shell(s, cx, &mut ui)
    };
    crate::dialogs::view(s, cx, &mut ui);
    crate::toasts::view(s, &mut ui);
    let body = col()
        .key("workspace")
        .w_full()
        .h_full()
        .fill(k.c.bg_deep)
        .child(
            el().key("content")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .child(content),
        )
        .child(version_bar(&k));
    zoomed(rune_kit::root(body, layer), m.zoom, cx)
}

/// The sidebar pane and the main pane (thread, new thread or the empty screen).
fn shell(s: &J3, cx: &mut Cx<J3>, ui: &mut crate::app::Ui) -> El {
    let (toolbar, main) = match &s.route.main {
        Main::Thread(key) => (
            crate::thread::toolbar(s, cx, ui, key),
            crate::thread::view(s, cx, ui, key),
        ),
        Main::NewThread => (
            Toolbar::new().title("New thread"),
            crate::new_thread::view(s, cx, ui),
        ),
        Main::Empty => (Toolbar::new(), empty(s, ui.k)),
    };
    let sidebar = crate::sidebar::view(s, cx, ui);
    SplitView::new(main)
        .sidebar(&s.sidebar.pane, |s: &mut J3| &mut s.sidebar.pane, sidebar)
        .toolbar(toolbar)
        .titlebar_inset(cx.titlebar_inset())
        .reduced_motion(cx.is_reduced_motion())
        .build(ui.k)
}

/// `Main::Empty`: "No thread open", a shortcut cheat sheet, and why `desktop.json` couldn't
/// be read, if it couldn't.
fn empty(s: &J3, k: &Kit) -> El {
    let key = |action| shortcut_label(action);
    let hints = [
        (key(Action::NewThread), "New thread"),
        (key(Action::AddProject), "Add project"),
        (
            format!(
                "{}  {}",
                key(Action::PreviousThread),
                key(Action::NextThread)
            ),
            "Previous / next thread",
        ),
        (key(Action::SettleThread), "Settle thread"),
        (key(Action::GotoAgent), "Go to agent"),
        (key(Action::OpenSettings), "Settings"),
    ];
    let mut column = col()
        .key("empty")
        .w_full()
        .h_full()
        .center()
        .gap(8.0)
        .child(
            text("No thread open")
                .key("empty-title")
                .font(k.ty.text_small_medium)
                .color(k.c.text_base),
        );
    for (ix, (keys, what)) in hints.into_iter().enumerate() {
        column = column.child(
            row()
                .key(format!("empty-hint-{ix}"))
                .gap(12.0)
                .child(
                    row()
                        .w(140.0)
                        .justify_end()
                        .child(text(keys).font(k.ty.text_small).color(k.c.text_muted)),
                )
                .child(
                    row()
                        .w(160.0)
                        .child(text(what).font(k.ty.text_small).color(k.c.text_muted)),
                ),
        );
    }
    if let Some(error) = s.prefs.error() {
        column = column.child(
            text(error)
                .key("config-error")
                .font(k.ty.text_xsmall)
                .color(theme::Status::of(k).danger)
                .wrap()
                .max_w(480.0),
        );
    }
    column
}

/// The build stamp: a thin strip under the content with the label at its right end. It
/// takes its own row, so it never covers the composer or Settings.
fn version_bar(k: &Kit) -> El {
    row()
        .key("version-bar")
        .w_full()
        .justify_end()
        .center_y()
        .px(12.0)
        .py(2.0)
        .border_sides([1.0, 0.0, 0.0, 0.0], k.c.border_muted)
        .child(
            text(VERSION_LABEL)
                .key("version-label")
                .font(k.ty.text_xsmall)
                .color(k.c.text_muted),
        )
}

/// `root` drawn at `zoom`: laid out at the window's size divided by the zoom and scaled up
/// from the top-left, so everything (text, padding, panes) grows together.
fn zoomed(root: El, zoom: f32, cx: &mut Cx<J3>) -> El {
    let frame = el().key("j3-window").w_full().h_full();
    if (zoom - 1.0).abs() < f32::EPSILON {
        return frame.child(root);
    }
    let [w, h] = cx.layout_size("j3-window").unwrap_or([1280.0, 820.0]);
    frame.child(
        el().key("j3-zoom")
            .size(w / zoom, h / zoom)
            .scale(zoom)
            .origin(0.0, 0.0)
            .child(root),
    )
}
