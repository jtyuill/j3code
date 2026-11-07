//! The plan and queue cards above the composer (spec §5.2, items 1–2).
//!
//! The plan card shows the latest todo list (`todo-toggle` folds it). The queue card lists
//! messages waiting behind the active run: each can steer the run (when it can be steered),
//! start now, be edited in place or leave the queue; a held queue says "Paused" and offers
//! Resume.

use j3_client::{ThreadState, t3::orchestration::PlanStepStatus};
use rune::{El, TextEdit, Tx, col, icon, row, spacer, text};
use rune_kit::{Button, ButtonVariant, ControlSize, IconButton, IconButtonVariant, Kit, TextInput};

use super::{ChromeUi, chrome_edit};
use crate::{
    app::{J3, Ui},
    icons,
    sidebar::ThreadKey,
    theme::Status,
};

/// A queued message being edited in place.
pub struct QueueEdit {
    pub run_id: String,
    pub edit: TextEdit,
}

impl J3 {
    /// Folds or unfolds `key`'s plan card.
    pub fn toggle_todo(&mut self, key: &ThreadKey) {
        if let Some(thread) = self.threads.get_mut(key) {
            thread.chrome.todo_open = !thread.chrome.todo_open;
        }
    }

    /// Sends queued run `run_id` into the active run as a steer.
    pub fn steer_queued(&mut self, key: &ThreadKey, run_id: &str) {
        let Some(target) = self
            .threads
            .get(key)
            .and_then(|thread| thread.state.active_run())
            .map(|run| run.id.clone())
        else {
            return;
        };
        let Some(connection) = self.thread_connection(key) else {
            return;
        };
        let (thread_id, run_id) = (key.thread_id.clone(), run_id.to_owned());
        self.run_command(
            "steer the run",
            async move {
                connection
                    .promote_to_steer(&thread_id, &run_id, &target)
                    .await
            },
            |_, _, _| {},
        );
    }

    /// Stops the active run and starts queued run `run_id`.
    pub fn send_queued_now(&mut self, key: &ThreadKey, run_id: &str) {
        let Some(connection) = self.thread_connection(key) else {
            return;
        };
        let (thread_id, run_id) = (key.thread_id.clone(), run_id.to_owned());
        self.run_command(
            "send the message now",
            async move { connection.start_queued(&thread_id, &run_id).await },
            |_, _, _| {},
        );
    }

    /// Takes queued run `run_id` out of the queue.
    pub fn cancel_queued(&mut self, key: &ThreadKey, run_id: &str) {
        let Some(connection) = self.thread_connection(key) else {
            return;
        };
        let (thread_id, run_id) = (key.thread_id.clone(), run_id.to_owned());
        self.run_command(
            "remove the message from the queue",
            async move { connection.cancel_queued(&thread_id, &run_id).await },
            |_, _, _| {},
        );
    }

    /// Releases `key`'s held queue.
    pub fn resume_queue(&mut self, key: &ThreadKey) {
        let Some(connection) = self.thread_connection(key) else {
            return;
        };
        let thread_id = key.thread_id.clone();
        self.run_command(
            "resume the queue",
            async move { connection.resume_queue(&thread_id).await },
            |_, _, _| {},
        );
    }

    /// Edits queued run `run_id`'s `text` in place, focused with the caret at the end.
    pub fn edit_queued(&mut self, key: &ThreadKey, run_id: &str, text: &str) {
        let Some(thread) = self.threads.get_mut(key) else {
            return;
        };
        let mut edit = TextEdit::new(text);
        edit.focused = true;
        edit.anchor = text.len();
        edit.focus = text.len();
        thread.chrome.queue_edit = Some(QueueEdit {
            run_id: run_id.to_owned(),
            edit,
        });
    }

    /// Saves the queued message being edited (`connection.edit_queued`).
    pub fn save_queued_edit(&mut self, key: &ThreadKey) {
        let Some(edit) = self
            .threads
            .get_mut(key)
            .and_then(|thread| thread.chrome.queue_edit.take())
        else {
            return;
        };
        let Some(connection) = self.thread_connection(key) else {
            return;
        };
        let thread_id = key.thread_id.clone();
        let text = edit.edit.text.clone();
        self.run_command(
            "edit the queued message",
            async move {
                connection
                    .edit_queued(&thread_id, &edit.run_id, &text)
                    .await
            },
            |_, _, _| {},
        );
    }

    /// Leaves the queued message as it was.
    pub fn cancel_queued_edit(&mut self, key: &ThreadKey) {
        if let Some(thread) = self.threads.get_mut(key) {
            thread.chrome.queue_edit = None;
        }
    }
}

/// The plan card: the latest todo list's steps, folded by `todo-toggle`; None without steps.
pub fn todo(ui: &Ui, key: &ThreadKey, state: &ThreadState, chrome: &ChromeUi) -> Option<El> {
    let k = ui.k;
    let todo = state.latest_todo().filter(|todo| !todo.steps.is_empty())?;
    let done = todo
        .steps
        .iter()
        .filter(|step| step.status == PlanStepStatus::Completed)
        .count();
    let open = chrome.todo_open;
    let summary = format!("{done}/{} done", todo.steps.len());
    let toggle_key = key.clone();
    let toggle = row()
        .key("todo-toggle")
        .role("button")
        .label(format!("Plan, {summary}"))
        .expanded(open)
        .gap(8.0)
        .center_y()
        .on_press(move |s: &mut J3| s.toggle_todo(&toggle_key))
        .child(
            icon(
                if open {
                    &icons::CHEVRON_DOWN
                } else {
                    &icons::CHEVRON_RIGHT
                },
                12.0,
            )
            .color(k.c.icon_muted),
        )
        .child(
            text("Plan")
                .font(k.ty.text_xsmall_medium)
                .color(k.c.text_muted),
        )
        .child(text(summary).font(k.ty.text_xsmall).color(k.c.text_muted));
    let mut card = card(k, "todo").child(toggle);
    if open {
        let status = Status::of(k);
        for step in todo.steps {
            let (glyph, color) = match step.status {
                PlanStepStatus::Completed => (&icons::CIRCLE_CHECK, status.success),
                PlanStepStatus::Running => (&icons::LOADER_CIRCLE, k.c.icon_accent),
                PlanStepStatus::Pending => (&icons::MINUS, k.c.icon_muted),
            };
            let completed = step.status == PlanStepStatus::Completed;
            let mut label = text(&step.text)
                .font(k.ty.text_small)
                .color(if completed {
                    k.c.text_muted
                } else {
                    k.c.text_base
                })
                .wrap();
            if completed {
                label = label.strike(0, step.text.len(), None);
            }
            card = card.child(
                row()
                    .key(format!("todo-step-{}", step.id))
                    .w_full()
                    .gap(8.0)
                    .items_start()
                    .child(el_icon(glyph, color))
                    .child(col().grow(1.0).min_w(0.0).child(label)),
            );
        }
    }
    Some(card)
}

/// The queue card: one row per queued message; None while nothing waits.
pub fn queue(ui: &Ui, key: &ThreadKey, state: &ThreadState, chrome: &ChromeUi) -> Option<El> {
    let k = ui.k;
    let queued = state.queued_runs();
    if queued.is_empty() {
        return None;
    }
    let steerable = state.active_run().is_some() && state.can_steer();
    let mut header = row()
        .key("queue-header")
        .w_full()
        .gap(8.0)
        .center_y()
        .child(
            text("Queued")
                .font(k.ty.text_xsmall_medium)
                .color(k.c.text_muted),
        );
    if state.queue_held() {
        let resume_key = key.clone();
        header = header
            .child(text("Paused").font(k.ty.text_xsmall).color(k.c.text_muted))
            .child(spacer())
            .child(
                Button::new("Resume")
                    .variant(ButtonVariant::Outline)
                    .control_size(ControlSize::Small)
                    .icon(&icons::PLAY)
                    .on_press(move |s: &mut J3| s.resume_queue(&resume_key))
                    .build(k)
                    .key("resume-queue"),
            );
    }
    let mut card = card(k, "queue").child(header);
    for queued in &queued {
        let run_id = queued.run.id.as_str();
        let editing = chrome
            .queue_edit
            .as_ref()
            .filter(|edit| edit.run_id == run_id);
        let body = match editing {
            Some(edit) => editing_row(k, key, run_id, &edit.edit),
            None => queued_row(k, key, run_id, queued.text, steerable),
        };
        card = card.child(
            row()
                .key(format!("queued-{run_id}"))
                .label(queued.text)
                .w_full()
                .gap(8.0)
                .center_y()
                .child(icon(&icons::INBOX, 12.0).color(k.c.icon_muted))
                .child(body),
        );
    }
    Some(card)
}

/// A queued message: its text, then Steer, Send now, Edit and remove.
fn queued_row(k: &Kit, key: &ThreadKey, run_id: &str, message: &str, steerable: bool) -> El {
    let action =
        |label: &str, id: &str, help: Option<&str>, run_fn: fn(&mut J3, &ThreadKey, &str)| {
            let (key, run) = (key.clone(), run_id.to_owned());
            let mut button = Button::new(label)
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small);
            if let Some(help) = help {
                button = button.help(help);
            }
            button
                .on_press(move |s: &mut J3| run_fn(s, &key, &run))
                .build(k)
                .key(format!("{id}-{run_id}"))
        };
    let mut body = row().grow(1.0).min_w(0.0).gap(4.0).center_y().child(
        text(message)
            .font(k.ty.text_small)
            .color(k.c.text_base)
            .truncate()
            .grow(1.0)
            .min_w(0.0),
    );
    if steerable {
        body = body.child(action(
            "Steer",
            "steer",
            Some("Send into the running turn"),
            J3::steer_queued,
        ));
    }
    let edit_key = key.clone();
    let (edit_run, edit_text) = (run_id.to_owned(), message.to_owned());
    let cancel_key = key.clone();
    let cancel_run = run_id.to_owned();
    body.child(action(
        "Send now",
        "send-now",
        Some("Stop the current run and start this one"),
        J3::send_queued_now,
    ))
    .child(
        Button::new("Edit")
            .variant(ButtonVariant::Ghost)
            .control_size(ControlSize::Small)
            .on_press(move |s: &mut J3| s.edit_queued(&edit_key, &edit_run, &edit_text))
            .build(k)
            .key(format!("edit-{run_id}")),
    )
    .child(
        IconButton::new(&icons::X)
            .variant(IconButtonVariant::Ghost)
            .control_size(ControlSize::Small)
            .label("Remove from queue")
            .help("Remove from queue")
            .on_press(move |s: &mut J3| s.cancel_queued(&cancel_key, &cancel_run))
            .build(k)
            .key(format!("cancel-{run_id}")),
    )
}

/// The message in a field, with Save (or Enter) and Cancel.
fn editing_row(k: &Kit, key: &ThreadKey, run_id: &str, edit: &TextEdit) -> El {
    let (edit_key, submit_key, save_key, cancel_key) =
        (key.clone(), key.clone(), key.clone(), key.clone());
    row()
        .grow(1.0)
        .min_w(0.0)
        .gap(4.0)
        .center_y()
        .child(
            col().grow(1.0).min_w(0.0).child(
                TextInput::new(edit)
                    .width(None)
                    .label("Queued message")
                    .on_edit(move |s: &mut J3| {
                        chrome_edit(s, &edit_key, |chrome| {
                            chrome.queue_edit.as_mut().map(|edit| &mut edit.edit)
                        })
                    })
                    .on_submit(move |s: &mut J3, _: &mut Tx| s.save_queued_edit(&submit_key))
                    .build(k)
                    .key(format!("queue-input-{run_id}")),
            ),
        )
        .child(
            Button::new("Save")
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small)
                .on_press(move |s: &mut J3| s.save_queued_edit(&save_key))
                .build(k)
                .key(format!("save-{run_id}")),
        )
        .child(
            Button::new("Cancel")
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small)
                .on_press(move |s: &mut J3| s.cancel_queued_edit(&cancel_key))
                .build(k)
                .key(format!("cancel-edit-{run_id}")),
        )
}

/// A bordered card column keyed `key`.
pub(super) fn card(k: &Kit, key: &str) -> El {
    col()
        .key(key.to_owned())
        .w_full()
        .gap(4.0)
        .px(12.0)
        .py(8.0)
        .radius(k.radius.lg)
        .border(1.0, k.c.border_muted)
}

/// A 14 pt status glyph, raised to sit on the first text line.
fn el_icon(glyph: &'static rune::Icon, color: rune::Color) -> El {
    row()
        .h(20.0)
        .center_y()
        .child(icon(glyph, 14.0).color(color))
}
