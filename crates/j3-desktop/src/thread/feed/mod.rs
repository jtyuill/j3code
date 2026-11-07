//! A thread's feed (spec §5.1, §6): the scrolling list of turn items — messages, reasoning,
//! tool calls, subagents and notices — with the thread's sync states in its place while it
//! loads, fails or was deleted.

use j3_client::{ThreadState, ThreadSync, t3::orchestration::TurnItemKind};
use rune::{Cx, El, ScrollSpec, col, el, row, text};

use crate::{
    app::{J3, Ui},
    sidebar::ThreadKey,
};

/// A thread's feed UI state (expanded rows, markdown streaming state), kept in its `ThreadUi`.
#[derive(Default)]
pub struct FeedUi {}

/// The feed of `key`'s thread, or the note that stands in for it.
pub fn view(s: &J3, _cx: &mut Cx<J3>, ui: &mut Ui, key: &ThreadKey) -> El {
    let k = ui.k;
    let note = |message: String| {
        el().key("feed-note")
            .w_full()
            .grow(1.0)
            .center()
            .child(text(message).font(k.ty.text_small).color(k.c.text_muted))
    };
    match s.threads.get(key).map(|thread| &thread.state) {
        None => note("Loading…".into()),
        Some(state) => match &state.sync {
            ThreadSync::Deleted => note("This thread was deleted.".into()),
            ThreadSync::Failed(e) if state.projection.is_none() => {
                note(format!("Couldn’t load the thread: {e}"))
            }
            _ if state.projection.is_none() => note("Loading…".into()),
            _ => messages(state, ui),
        },
    }
}

/// User and assistant messages as plain text, in a list that follows its end.
fn messages(state: &ThreadState, ui: &Ui) -> El {
    let k = ui.k;
    let rows: Vec<El> = state
        .feed()
        .iter()
        .filter(|projected| !crate::feed::is_hidden(&projected.item))
        .filter_map(|projected| {
            let item = &projected.item;
            let (text_of, user) = match &item.kind {
                TurnItemKind::UserMessage(message) => (message.text.clone(), true),
                TurnItemKind::AssistantMessage(message) => (message.text.clone(), false),
                _ => return None,
            };
            let body = text(text_of.clone())
                .font(k.ty.body)
                .color(k.c.text_base)
                .wrap()
                .selectable(true)
                .selection_color(k.selection());
            let bubble = if user {
                row().w_full().justify_end().child(
                    el().max_w(ui.m.measure * 0.8)
                        .padding(12.0)
                        .radius(k.radius.lg)
                        .fill(k.c.layer_02)
                        .child(body),
                )
            } else {
                row().w_full().child(body)
            };
            Some(
                el().key(format!("feed-{}", item.id))
                    .w_full()
                    .px(16.0)
                    .pt(24.0)
                    .label(text_of)
                    .child(bubble),
            )
        })
        .collect();
    if rows.is_empty() {
        return el()
            .key("feed")
            .w_full()
            .grow(1.0)
            .center()
            .child(text("No messages yet.").font(k.ty.text_small).color(k.c.text_muted));
    }
    col()
        .key("feed")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .scroll(ScrollSpec { stick_bottom: true })
        .child(
            col()
                .key("feed-rows")
                .w_full()
                .max_w(ui.m.measure)
                .self_center()
                .pb(16.0)
                .children(rows),
        )
}
