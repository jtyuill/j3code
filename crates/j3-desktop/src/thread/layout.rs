//! The open thread's body (spec §5, §5.4): the chat column — the feed, then the cards above
//! the composer and the composer — with the terminal drawer under it and the file pane to
//! its right, each behind a draggable divider. The agents hub takes the chat's place (and the
//! drawer's) while open; the header and the file pane stay.
//!
//! The drawer's height lives in `terminal::Drawer::height` (280 pt by default, 120–2000) and
//! the pane's width in `files::FilePane::width` (22.5 rem, 15–60 rem); a double click on a
//! divider restores the default.

use rune::{Cursor, Cx, Drag, El, Phase, ScrollSpec, Tx, col, el, row};

use super::{cards, goal, question};
use crate::{
    app::{J3, Ui},
    composer::ComposerKey,
    sidebar::ThreadKey,
    theme::Metrics,
};

/// The dividers' grab strip.
const GRIP: f32 = 5.0;
const DRAWER_DEFAULT: f32 = 280.0;
const DRAWER_MIN: f32 = 120.0;
const DRAWER_MAX: f32 = 2000.0;
/// What the chat keeps when a divider is dragged towards it.
const CHAT_MIN_HEIGHT: f32 = 120.0;
const CHAT_MIN_WIDTH: f32 = 320.0;
const FILES_DEFAULT_REM: f32 = 22.5;
const FILES_MIN_REM: f32 = 15.0;
const FILES_MAX_REM: f32 = 60.0;

/// The thread's body under the header.
pub fn view(s: &J3, cx: &mut Cx<J3>, ui: &mut Ui, key: &ThreadKey) -> El {
    let hub_open = s.hubs.get(key).is_some_and(|hub| hub.open);
    let chat = if hub_open {
        crate::hub::view(s, cx, ui, key)
    } else {
        chat(s, cx, ui, key)
    };
    let zoom = ui.m.zoom;
    let mut main = col()
        .key("thread-main")
        .grow(1.0)
        .min_w(0.0)
        .h_full()
        .child(chat);
    if !hub_open && s.terminals.get(key).is_some_and(|drawer| drawer.open) {
        let room = cx.layout("thread-main").map(|rect| rect[3] / zoom);
        main = main
            .child(drawer_grip(key, zoom, room))
            .child(crate::terminal::view(s, cx, ui, key));
    }
    let mut body = row().key("thread").w_full().h_full().min_h(0.0).child(main);
    if s.files.get(key).is_some_and(|pane| pane.open) {
        let room = cx.layout("thread").map(|rect| rect[2] / zoom);
        body = body
            .child(files_grip(key, ui.m, room))
            .child(crate::files::view(s, cx, ui, key));
    }
    if let Some(dialog) = s
        .threads
        .get(key)
        .and_then(|thread| thread.chrome.goal_dialog.as_ref())
    {
        goal::dialog(ui, key, dialog);
    }
    body
}

/// The feed, then the cards and the composer in a column at the text measure. When the
/// chat is short (a tall drawer, a long questionnaire) the feed gives way first, then the
/// cards scroll; the composer keeps its size.
fn chat(s: &J3, cx: &mut Cx<J3>, ui: &mut Ui, key: &ThreadKey) -> El {
    let feed = super::feed::view(s, cx, ui, key);
    let mut bottom = col()
        .key("thread-bottom")
        .w_full()
        .max_w(ui.m.measure)
        .self_center()
        .shrink()
        .min_h(0.0)
        .px(16.0)
        .pt(16.0)
        .pb(12.0)
        .gap(8.0)
        // ←← in the composer opens the hub in this column's place: the keyboard follows.
        .focus_return("agent-hub");
    if let Some(thread) = s.threads.get(key) {
        let (state, chrome) = (&thread.state, &thread.chrome);
        let cards: Vec<El> = [
            cards::todo(ui, key, state, chrome),
            cards::queue(ui, key, state, chrome),
            chrome
                .question
                .as_ref()
                .map(|card| question::card(ui, key, card)),
            state.goal().map(|goal| goal::chip(ui, key, goal)),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !cards.is_empty() {
            bottom = bottom.child(
                col()
                    .key("thread-cards")
                    .w_full()
                    .gap(8.0)
                    .scroll(ScrollSpec { stick_bottom: true })
                    .shrink()
                    .min_h(0.0)
                    .children(cards),
            );
        }
    }
    let composer = crate::composer::view(s, cx, ui, &ComposerKey::Thread(key.clone()));
    col()
        .key("chat")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .child(
            col()
                .key("chat-feed")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .pt(12.0)
                .child(feed),
        )
        .child(bottom.child(composer))
}

/// The divider above the terminal drawer: dragging it up grows the drawer. `room` is the
/// chat column's height, which keeps [`CHAT_MIN_HEIGHT`] for the chat.
fn drawer_grip(key: &ThreadKey, zoom: f32, room: Option<f32>) -> El {
    let max = room.map_or(DRAWER_MAX, |room| {
        (room - CHAT_MIN_HEIGHT).clamp(DRAWER_MIN, DRAWER_MAX)
    });
    let (drag_key, reset_key) = (key.clone(), key.clone());
    el().key("terminal-split")
        .role("separator")
        .label("Resize terminal")
        .w_full()
        .h(GRIP)
        .cursor(Cursor::ResizeRow)
        .on_drag(move |s: &mut J3, d: &mut Drag, tx: &mut Tx| {
            let Some(drawer) = s.terminals.get_mut(&drag_key) else {
                return;
            };
            match d.phase {
                Phase::Down => d.data = drawer.height,
                Phase::Move => {
                    tx.instant();
                    drawer.height = (d.data - d.dy / zoom).clamp(DRAWER_MIN, max);
                }
                Phase::Up => {
                    if d.cancelled() {
                        drawer.height = d.data;
                    }
                }
            }
        })
        .on_double_press(move |s: &mut J3| {
            if let Some(drawer) = s.terminals.get_mut(&reset_key) {
                drawer.height = DRAWER_DEFAULT;
            }
        })
}

/// The divider left of the file pane: dragging it left widens the pane. `room` is the
/// thread's width, which keeps [`CHAT_MIN_WIDTH`] for the chat.
fn files_grip(key: &ThreadKey, m: &Metrics, room: Option<f32>) -> El {
    let (min, default) = (m.rems(FILES_MIN_REM), m.rems(FILES_DEFAULT_REM));
    let max = room.map_or(m.rems(FILES_MAX_REM), |room| {
        (room - CHAT_MIN_WIDTH).clamp(min, m.rems(FILES_MAX_REM))
    });
    let zoom = m.zoom;
    let (drag_key, reset_key) = (key.clone(), key.clone());
    el().key("thread-files-split")
        .role("separator")
        .label("Resize file pane")
        .h_full()
        .w(GRIP)
        .cursor(Cursor::ResizeColumn)
        .on_drag(move |s: &mut J3, d: &mut Drag, tx: &mut Tx| {
            let Some(pane) = s.files.get_mut(&drag_key) else {
                return;
            };
            match d.phase {
                Phase::Down => d.data = pane.width,
                Phase::Move => {
                    tx.instant();
                    pane.width = (d.data - d.dx / zoom).clamp(min, max);
                }
                Phase::Up => {
                    if d.cancelled() {
                        pane.width = d.data;
                    }
                }
            }
        })
        .on_double_press(move |s: &mut J3| {
            if let Some(pane) = s.files.get_mut(&reset_key) {
                pane.width = default;
            }
        })
}
