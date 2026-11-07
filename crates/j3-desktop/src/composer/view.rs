//! The composer's layout (spec §7): the completion menu above, the image strip, the field
//! with its ghost text, the error and catalog notes, then the model picker, the usage gauge,
//! the microphone and the send button.

use std::time::Duration;

use rune::{
    Cx, El, PillStyle, TextEdit, TextStyle, Tx, col, el, icon, row, spacer, text, text_input,
};
use rune_kit::{Kit, Spinner};

use super::{ComposerKey, draft, images, menu, mic, picker, usage};
use crate::{
    actions::{Action, shortcut_label},
    app::{J3, Ui},
    icons, keywords,
    theme::Status,
};

/// What the round button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SendAction {
    /// Starts a run.
    Send,
    /// Holds the draft until the current run ends.
    Queue,
    /// Interrupts the current run; there is no draft to queue.
    Stop,
}

/// The field's type: the kit's control text on the line `TextArea` uses.
fn field_style(k: &Kit) -> TextStyle {
    TextStyle {
        line_height: (k.ty.ui.size * 1.35 * 2.0).round() / 2.0,
        ..k.ty.ui
    }
}

/// The composer of `key`.
pub fn view(s: &J3, cx: &mut Cx<J3>, ui: &mut Ui, key: &ComposerKey) -> El {
    let k = ui.k;
    let blank = TextEdit::new("");
    let composer = s.composers.get(key);
    let edit = composer.map_or(&blank, |c| &c.edit);
    let target = s.composer_target(key);
    let running = target.running;
    let has_models = target.env.is_some_and(|env| !env.models().is_empty());
    // An empty catalog is nearly always a server problem (omp not found, say): say what the
    // server reported.
    let catalog_note = (!has_models)
        .then(|| target.env?.state().config.as_ref()?.providers.first())
        .flatten()
        .map(|provider| {
            provider
                .message
                .clone()
                .unwrap_or_else(|| "Loading models…".to_owned())
        });
    // The poll notices focus changes (prediction warm-up, shimmer); ask for one.
    if composer.is_some_and(|c| c.focused != c.edit.focused) {
        s.inbox.wake();
    }

    let mut root = col()
        .key("composer")
        .w_full()
        .fill(k.c.bg_base)
        .border(1.0, k.c.border_base)
        .radius(k.radius.lg);
    if let Some(strip) = images::view(s, ui, key) {
        root = root.child(strip);
    }
    root = root.child(field(s, cx, ui, key, edit));
    if let Some(error) = composer.and_then(|c| c.error.as_deref()) {
        root = root.child(
            text(error)
                .key("composer-error")
                .font(k.ty.text_xsmall)
                .color(Status::of(k).danger)
                .wrap()
                .px(12.0),
        );
    }
    if let Some(note) = catalog_note {
        root = root.child(
            text(note)
                .key("catalog-note")
                .font(k.ty.text_xsmall)
                .color(k.c.text_muted)
                .wrap()
                .px(12.0)
                .pb(4.0),
        );
    }
    let bottom = row()
        .key("composer-actions")
        .w_full()
        .gap(4.0)
        .px(8.0)
        .pb(8.0)
        .pt(4.0)
        .center_y()
        .child(picker::view(s, ui, key))
        .child(usage::view(s, ui, key))
        .child(spacer())
        .child(mic::view(s, ui, key))
        .child(send_button(s, ui, key, running));
    let width = cx.layout_size("composer").map_or(480.0, |[w, _]| w);
    menu::view(s, ui, key, "composer", width);
    let drop = key.clone();
    root.child(bottom)
        .on_drop_files(move |s: &mut J3, paths, tx: &mut Tx| {
            s.composer_drop(&drop, paths);
            tx.instant();
        })
}

/// The text field, auto-growing from one to twelve lines, with the ghost after the caret.
fn field(s: &J3, cx: &mut Cx<J3>, ui: &Ui, key: &ComposerKey, edit: &TextEdit) -> El {
    let k = ui.k;
    let style = field_style(k);
    let input_key = key.input_key();
    let composer = s.composers.get(key);
    let running = s.composer_target(key).running;
    let phase = if edit.focused {
        keywords::shimmer_phase(Duration::from_secs_f64(s.clock.t))
    } else {
        0.0
    };
    let info = Status::of(k).info;
    let (chip, chip_selected) = (k.alpha(info, 0.15), k.selection());
    let (a, b) = edit.range();
    let selected: Vec<String> = edit
        .pills()
        .filter(|(range, _)| a < b && range.start >= a && range.end <= b)
        .map(|(_, id)| id.to_owned())
        .collect();
    let (edit_key, submit_key, change_key, key_key, paste_key) = (
        key.clone(),
        key.clone(),
        key.clone(),
        key.clone(),
        key.clone(),
    );
    let input = text_input(edit, style)
        .key(input_key.clone())
        .w_full()
        .color(k.c.text_base)
        .placeholder(
            if running {
                "Queue a message…"
            } else {
                "Message omp…"
            },
            k.c.text_faint,
        )
        .caret(k.c.text_base, k.selection())
        .label("Message")
        .decorations(draft::keyword_spans(edit, phase))
        .pill_style(move |id| {
            let bg = if selected.iter().any(|s| s == id) {
                chip_selected
            } else {
                chip
            };
            PillStyle::new().bg(bg).fg(info).icon(&icons::BOT)
        })
        .on_edit(move |s: &mut J3| &mut s.composer(&edit_key).edit)
        .on_submit(move |s: &mut J3, tx: &mut Tx| s.submit_composer(&submit_key, tx))
        .on_input_change(move |s: &mut J3, tx: &mut Tx| s.composer_changed(&change_key, tx))
        .on_input_key(move |s: &mut J3, ev, tx: &mut Tx| s.composer_key(&key_key, ev, tx))
        .on_input_paste(move |s: &mut J3, content, tx: &mut Tx| {
            s.composer_paste(&paste_key, content, tx)
        });
    let mut wrapper = col()
        .key("composer-field")
        .w_full()
        .px(12.0)
        .pt(10.0)
        .child(rune::auto_grow(input, 1, 12));
    let ghost = composer
        .and_then(draft::shown_ghost)
        .filter(|_| edit.focused);
    if let Some(ghost) = ghost
        && let (Some(caret), Some(area)) = (cx.caret(&input_key), cx.layout("composer-field"))
    {
        // Set like the draft, from the caret's own geometry, clipped to the text area.
        let x = caret[0] + caret[2] - area[0];
        let width = area[2] - 12.0 - x;
        if width > 0.0 {
            wrapper = wrapper.child(
                el().key("composer-ghost")
                    .label(&ghost.suffix)
                    .absolute(x, caret[1] - area[1])
                    .w(width)
                    .h(caret[3])
                    .clip_soft(0.0)
                    .child(
                        text(&ghost.suffix)
                            .font(style)
                            .color(k.alpha(k.c.text_muted, 0.6))
                            .a11y_hidden(),
                    ),
            );
        }
    }
    wrapper
}

/// Send, Queue or Stop: grey with nothing to send, the info tint when ready, the danger
/// tint for Stop; a spinner while sending.
fn send_button(s: &J3, ui: &Ui, key: &ComposerKey, running: bool) -> El {
    let k = ui.k;
    let status = Status::of(k);
    let has_draft = s.composer_has_draft(key);
    let action = match (running, has_draft) {
        (false, _) => SendAction::Send,
        (true, true) => SendAction::Queue,
        (true, false) => SendAction::Stop,
    };
    let enter = rune::Keybind::new("enter").label();
    let (glyph, name, help) = match action {
        SendAction::Send => (&icons::ARROW_UP, "Send", format!("Send ({enter})")),
        SendAction::Queue => (
            &icons::INBOX,
            "Queue",
            format!("Queue after the current run ({enter})"),
        ),
        SendAction::Stop => (
            &icons::SQUARE,
            "Stop",
            format!("Stop the run ({})", shortcut_label(Action::StopRun)),
        ),
    };
    let (fill, ink) = match action {
        SendAction::Stop => (k.alpha(status.danger, 0.18), status.danger),
        _ if has_draft => (k.alpha(status.info, 0.22), status.info),
        _ => (k.c.layer_02, k.c.text_muted),
    };
    let busy = s.composers.get(key).is_some_and(|c| c.busy);
    let content = if busy {
        Spinner::new().size(14.0).color(ink).build(k)
    } else {
        icon(glyph, 14.0).color(ink)
    };
    let press = key.clone();
    row()
        .key("send-message")
        .role("button")
        .label(name)
        .help(help)
        .size(28.0, 28.0)
        .center()
        .radius(999.0)
        .fill(fill)
        .hover(k.alpha(ink, 0.3))
        .child(content)
        .on_press_tx(move |s: &mut J3, tx: &mut Tx| match action {
            SendAction::Stop => {
                if let ComposerKey::Thread(thread) = &press {
                    s.stop_run(thread);
                }
            }
            SendAction::Send | SendAction::Queue => s.send_halting_dictation(&press, tx),
        })
}
