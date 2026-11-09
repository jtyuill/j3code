//! The hub's view: the roster (300 pt, left) and the selected subagent's detail — its
//! header with Stop, then the transcript: the assignment, assistant markdown, thinking,
//! tool calls and results that expand on click, and errors.

use std::hash::{DefaultHasher, Hash, Hasher};

use j3_client::{
    ThreadView,
    entities::{Item, ItemStatus, SubagentItem},
    methods::TranscriptEntry,
};
use j3_text::math;
use rune::{
    Color, Cx, El, Icon, KeyEvent, ScrollSpec, TextStyle, Tx, col, icon, memo, row, spacer, text,
};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit, Markdown, Spinner};

use super::{AgentHub, TRANSCRIPT_KEY, is_running, roster};
use crate::{
    app::{J3, Ui},
    icons, mention,
    sidebar::ThreadKey,
    theme::Status,
};

/// The roster column's width.
const ROSTER_WIDTH: f32 = 300.0;

/// `key`'s hub, filling the chat's place.
pub fn view(s: &J3, _cx: &mut Cx<J3>, ui: &mut Ui, key: &ThreadKey) -> El {
    let k = ui.k;
    let (Some(thread), Some(hub)) = (s.threads.get(key), s.hubs.get(key)) else {
        return col().key("agent-hub").grow(1.0);
    };
    let roster_column = roster_column(k, key, hub, &thread.state);
    let detail = detail(s, ui, key, hub, &thread.state);
    let keys = key.clone();
    row()
        .key("agent-hub")
        .label("Agents")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .items_stretch()
        .focusable(true)
        .on_key(move |s: &mut J3, ev: &KeyEvent, tx: &mut Tx| s.on_hub_key(&keys, ev, tx))
        .child(roster_column)
        .child(detail)
}

/// "Agents" and the count, the rows, and the key hints.
fn roster_column(k: &Kit, key: &ThreadKey, hub: &AgentHub, state: &ThreadView) -> El {
    let roster = roster(state);
    let count = match roster.len() {
        1 => "1 agent".to_owned(),
        n => format!("{n} agents"),
    };
    let mut rows = col()
        .key("hub-roster-rows")
        .w_full()
        .gap(2.0)
        .px(8.0)
        .pb(8.0);
    for (item, subagent) in roster {
        rows = rows.child(roster_row(k, key, hub, item, subagent));
    }
    col()
        .key("hub-roster-column")
        .w(ROSTER_WIDTH)
        .h_full()
        .border_sides([0.0, 1.0, 0.0, 0.0], k.c.border_muted)
        .child(
            row()
                .key("hub-roster-header")
                .w_full()
                .px(12.0)
                .py(8.0)
                .gap(8.0)
                .child(
                    text("Agents")
                        .font(k.ty.text_xsmall_medium)
                        .color(k.c.text_muted),
                )
                .child(spacer())
                .child(text(count).font(k.ty.text_xsmall).color(k.c.text_muted)),
        )
        .child(
            col()
                .key("hub-roster")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .child(rows),
        )
        .child(
            row()
                .key("hub-hints")
                .w_full()
                .px(12.0)
                .py(8.0)
                .border_sides([1.0, 0.0, 0.0, 0.0], k.c.border_muted)
                .child(
                    text("↑↓ select · PgUp/PgDn scroll · x stop · Esc or ←← close")
                        .font(k.ty.text_xsmall)
                        .color(k.c.text_muted)
                        .wrap(),
                ),
        )
}

/// One roster row: glyph, id and status, then title and live progress.
fn roster_row(
    k: &Kit,
    key: &ThreadKey,
    hub: &AgentHub,
    item: &Item,
    subagent: &SubagentItem,
) -> El {
    let selected = hub.selected.as_deref() == Some(subagent.subagent_id.as_str());
    let title = subagent_title(subagent);
    let status = crate::feed::subagent_status_label(item.status);
    let secondary = if selected {
        k.c.text_base
    } else {
        k.c.text_muted
    };
    let (select_key, id) = (key.clone(), subagent.subagent_id.clone());
    let mut row_el = col()
        .key(format!("hub-agent-{}", subagent.subagent_id))
        .role("button")
        .label(format!("{}, {title}, {status}", subagent.subagent_id))
        .w_full()
        .gap(2.0)
        .px(8.0)
        .py(6.0)
        .radius(k.radius.md)
        .on_press_tx(move |s: &mut J3, tx: &mut Tx| s.select_subagent(&select_key, &id, tx))
        .child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(status_glyph(k, item.status, 14.0))
                .child(
                    text(&subagent.subagent_id)
                        .font(k.ty.text_small_medium)
                        .color(k.c.text_base)
                        .truncate()
                        .grow(1.0)
                        .min_w(0.0),
                )
                .child(text(status).font(k.ty.text_xsmall).color(secondary)),
        )
        .child(
            text(title)
                .font(k.ty.text_xsmall)
                .color(k.c.text_base)
                .truncate(),
        );
    if let Some(progress) = subagent.progress.as_ref().filter(|_| is_running(item)) {
        row_el = row_el.child(
            text(progress)
                .font(k.ty.text_xsmall)
                .color(secondary)
                .truncate(),
        );
    }
    if selected {
        row_el.fill(k.c.layer_02)
    } else {
        k.interactive(row_el)
    }
}

/// The selected subagent: header, then its transcript (or what j3 knows without one).
fn detail(s: &J3, ui: &Ui, key: &ThreadKey, hub: &AgentHub, state: &ThreadView) -> El {
    let k = ui.k;
    let Some((item, subagent)) = hub.selected.as_ref().and_then(|id| state.subagent(id)) else {
        return col().key("hub-detail").grow(1.0).center().child(
            text("No subagents")
                .font(k.ty.text_small)
                .color(k.c.text_muted),
        );
    };
    let active = is_running(item);
    let model = subagent.model.as_deref().map(|model| {
        // The catalog's label when the slug is listed.
        s.envs
            .get(&key.env)
            .and_then(|env| mention::find_choice(env.models(), model))
            .map_or_else(|| model.to_owned(), |choice| choice.label.text().to_owned())
    });
    let agent = Some(subagent.agent.clone()).filter(|agent| !agent.trim().is_empty());
    let meta = [
        Some(subagent.subagent_id.clone()),
        agent,
        model,
        subagent.progress.clone(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    let mut header = row()
        .key("hub-detail-header")
        .w_full()
        .gap(8.0)
        .px(16.0)
        .py(8.0)
        .center_y()
        .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_muted)
        .child(status_glyph(k, item.status, 16.0))
        .child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .child(
                    text(subagent_title(subagent))
                        .font(k.ty.text_small_medium)
                        .color(k.c.text_base)
                        .truncate(),
                )
                .child(
                    text(meta)
                        .font(k.ty.text_xsmall)
                        .color(k.c.text_muted)
                        .truncate(),
                ),
        );
    if active {
        let stop_key = key.clone();
        header = header.child(Spinner::new().size(14.0).build(k)).child(
            Button::new("Stop")
                .variant(ButtonVariant::Outline)
                .control_size(ControlSize::Small)
                .icon(&icons::SQUARE)
                .on_press(move |s: &mut J3| s.stop_selected_subagent(&stop_key))
                .build(k)
                .key("hub-stop"),
        );
    }
    let mut body = col()
        .key("hub-entries")
        .w_full()
        .gap(12.0)
        .px(16.0)
        .py(12.0);
    match hub.transcript(&subagent.subagent_id) {
        Some(transcript) if transcript.loaded => {
            if transcript.entries.is_empty() {
                body = body.child(note(k, "Nothing in the transcript yet"));
            }
            for (ix, entry) in transcript.entries.iter().enumerate() {
                let id = format!("{}/{ix}", subagent.subagent_id);
                body = body.child(entry_view(s, ui, key, hub, entry, id));
            }
        }
        Some(transcript) if transcript.error.is_some() => {
            // No transcript yet: what j3 already knows.
            body = body
                .child(note(k, transcript.error.clone().unwrap_or_default()))
                .child(labelled(k, "Assignment", &subagent.prompt));
            if let Some(result) = &subagent.result {
                body = body.child(labelled(k, "Result", result));
            }
        }
        _ => {
            body = body.child(
                row()
                    .key("hub-loading")
                    .gap(8.0)
                    .center_y()
                    .child(Spinner::new().size(14.0).build(k))
                    .child(
                        text("Loading transcript…")
                            .font(k.ty.text_small)
                            .color(k.c.text_muted),
                    ),
            );
        }
    }
    col()
        .key("hub-detail")
        .grow(1.0)
        .min_w(0.0)
        .h_full()
        .child(header)
        .child(
            col()
                .key(TRANSCRIPT_KEY)
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec { stick_bottom: true })
                .child(body),
        )
}

/// One transcript entry; `id` is `subagent/index`.
fn entry_view(
    s: &J3,
    ui: &Ui,
    key: &ThreadKey,
    hub: &AgentHub,
    entry: &TranscriptEntry,
    id: String,
) -> El {
    let k = ui.k;
    match entry {
        TranscriptEntry::User { text: body } => col()
            .key(format!("hub-entry-{id}"))
            .w_full()
            .gap(4.0)
            .px(12.0)
            .py(8.0)
            .radius(k.radius.lg)
            .fill(k.c.layer_01)
            .child(
                text("Assignment")
                    .font(k.ty.text_xsmall)
                    .color(k.c.text_muted),
            )
            .child(markdown(s, ui, key, &id, body)),
        TranscriptEntry::Assistant { text: body } => col()
            .key(format!("hub-entry-{id}"))
            .w_full()
            .child(markdown(s, ui, key, &id, body)),
        TranscriptEntry::Thinking { text: body } => text(body)
            .key(format!("hub-entry-{id}"))
            .font(TextStyle {
                italic: true,
                ..k.ty.text_xsmall
            })
            .color(k.c.text_muted)
            .wrap(),
        TranscriptEntry::ToolCall {
            name,
            intent,
            input,
        } => {
            let title = match intent {
                Some(intent) => format!("{name} · {intent}"),
                None => name.clone(),
            };
            let arguments = match input {
                serde_json::Value::String(text) => text.clone(),
                other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
            };
            tool_row(
                k,
                key,
                hub,
                &id,
                &icons::SQUARE_TERMINAL,
                title,
                &arguments,
                false,
            )
        }
        TranscriptEntry::ToolResult {
            name,
            text: body,
            failed,
        } => {
            let glyph = if *failed {
                &icons::CIRCLE_X
            } else {
                &icons::CHECK
            };
            tool_row(
                k,
                key,
                hub,
                &id,
                glyph,
                format!("{name} result"),
                body,
                *failed,
            )
        }
        TranscriptEntry::Error { text: body } => text(body)
            .key(format!("hub-entry-{id}"))
            .font(k.ty.text_small)
            .color(Status::of(k).danger)
            .wrap(),
    }
}

/// A one-line tool row; a click shows `body` in full.
#[allow(clippy::too_many_arguments)]
fn tool_row(
    k: &Kit,
    key: &ThreadKey,
    hub: &AgentHub,
    id: &str,
    glyph: &'static Icon,
    title: String,
    body: &str,
    is_error: bool,
) -> El {
    let expanded = hub.is_expanded(id);
    let color = if is_error {
        Status::of(k).danger
    } else {
        k.c.text_muted
    };
    let mut head = row()
        .w_full()
        .gap(8.0)
        .center_y()
        .child(icon(glyph, 12.0).color(color))
        .child(text(title).font(k.ty.text_xsmall_medium).color(color));
    if !expanded {
        head = head.child(
            text(crate::feed::one_line(body))
                .font(k.ty.text_xsmall)
                .color(k.c.text_muted)
                .truncate()
                .grow(1.0)
                .min_w(0.0),
        );
    }
    let (toggle_key, entry) = (key.clone(), id.to_owned());
    let mut row_el = col()
        .key(format!("hub-entry-{id}"))
        .role("button")
        .expanded(expanded)
        .w_full()
        .gap(4.0)
        .on_press(move |s: &mut J3| s.toggle_hub_entry(&toggle_key, &entry))
        .child(head);
    if expanded {
        row_el = row_el.child(
            col()
                .w_full()
                .px(12.0)
                .py(8.0)
                .radius(k.radius.md)
                .fill(k.c.layer_01)
                .child(
                    text(body)
                        .font(k.ty.mono)
                        .color(k.c.text_base)
                        .wrap_anywhere()
                        .selectable(true),
                ),
        );
    }
    row_el
}

/// User and assistant text as markdown, with TeX math typeset when the setting is on.
/// A memo keyed by thread and entry, versioned by the source (a reset transcript reuses
/// indices), keeps all but entries with formulas (whose images load later).
fn markdown(s: &J3, ui: &Ui, key: &ThreadKey, id: &str, source: &str) -> El {
    let k = ui.k;
    let math_on = s.prefs.ui().math_rendering;
    let build = || {
        if !math_on {
            return Markdown::new(source).selectable(true).build(k);
        }
        let prepared = math::prepare(source);
        if prepared.formulas.is_empty() {
            return Markdown::new(prepared.source).selectable(true).build(k);
        }
        let ink = k.c.text_base;
        let size = k.ty.body.size;
        let images: Vec<Option<rune::Image>> = prepared
            .formulas
            .iter()
            .map(|formula| {
                s.math
                    .borrow_mut()
                    .image(s.images.cache(), formula, ink, size, 1.0)
            })
            .collect();
        Markdown::new(prepared.source)
            .images(move |src| images.get(math::parse_src(src)?)?.clone())
            .selectable(true)
            .build(k)
    };
    let may_have_math =
        math_on && (source.contains('$') || source.contains("\\(") || source.contains("\\["));
    if may_have_math {
        return build();
    }
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    memo(
        format!("hub-md-{}-{}-{id}", key.env, key.thread_id),
        (
            hasher.finish(),
            math_on,
            k.dark,
            ui.m.rem.to_bits(),
            ui.m.code.to_bits(),
        ),
        build,
    )
}

fn note(k: &Kit, message: impl Into<String>) -> El {
    text(message.into())
        .font(k.ty.text_small)
        .color(k.c.text_muted)
        .wrap()
}

/// A labelled plain-text block (when no transcript reads).
fn labelled(k: &Kit, label: &str, body: &str) -> El {
    col()
        .w_full()
        .gap(4.0)
        .child(text(label).font(k.ty.text_xsmall).color(k.c.text_muted))
        .child(text(body).font(k.ty.text_small).color(k.c.text_base).wrap())
}

/// The subagent's title: omp's short description of its task, else its assignment's first
/// line, else its agent.
pub fn subagent_title(subagent: &SubagentItem) -> String {
    let first_line = |text: &str| crate::feed::one_line(text.lines().next().unwrap_or_default());
    [
        subagent.description.as_deref(),
        Some(subagent.prompt.as_str()),
    ]
    .into_iter()
    .flatten()
    .map(first_line)
    .find(|title| !title.is_empty())
    .unwrap_or_else(|| subagent.agent.clone())
}

/// The status glyph: running, done, failed or stopped.
fn status_glyph(k: &Kit, status: ItemStatus, size: f32) -> El {
    let palette = Status::of(k);
    let (glyph, color): (&'static Icon, Color) = match status {
        ItemStatus::Running => (&icons::LOADER_CIRCLE, k.c.icon_accent),
        ItemStatus::Completed => (&icons::CIRCLE_CHECK, palette.success),
        ItemStatus::Failed => (&icons::CIRCLE_X, palette.danger),
        ItemStatus::Cancelled => (&icons::CIRCLE_SLASH, palette.warning),
    };
    icon(glyph, size).color(color)
}
