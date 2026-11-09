//! The plan and queue cards above the composer (spec §5.2, items 1–2).
//!
//! The plan card shows the latest todo list's tasks, by phase (`todo-toggle` folds it). The
//! queue card lists messages waiting behind the active turn: each can steer the turn (while
//! one runs), start now, be edited in place or leave the queue; a held queue says "Paused"
//! and offers Resume.

use j3_client::{
    ThreadView,
    entities::TodoStatus,
    methods::{QueueEditParams, QueueEntryParams, ThreadParams},
};
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
    pub entry_id: String,
    pub edit: TextEdit,
}

/// What a queue entry's button asks of the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueueCommand {
    Steer,
    Start,
    Cancel,
}

impl J3 {
    /// Folds or unfolds `key`'s plan card.
    pub fn toggle_todo(&mut self, key: &ThreadKey) {
        if let Some(thread) = self.threads.get_mut(key) {
            thread.chrome.todo_open = !thread.chrome.todo_open;
        }
    }

    /// Sends queue entry `entry_id` into the active turn as a steer.
    pub fn steer_queued(&mut self, key: &ThreadKey, entry_id: &str) {
        self.queue_command(key, entry_id, QueueCommand::Steer);
    }

    /// Stops the active turn and starts queue entry `entry_id`.
    pub fn send_queued_now(&mut self, key: &ThreadKey, entry_id: &str) {
        self.queue_command(key, entry_id, QueueCommand::Start);
    }

    /// Takes queue entry `entry_id` out of the queue.
    pub fn cancel_queued(&mut self, key: &ThreadKey, entry_id: &str) {
        self.queue_command(key, entry_id, QueueCommand::Cancel);
    }

    fn queue_command(&mut self, key: &ThreadKey, entry_id: &str, command: QueueCommand) {
        let Some(connection) = self.thread_connection(key) else {
            return;
        };
        let params = QueueEntryParams {
            thread_id: key.thread_id.clone(),
            entry_id: entry_id.to_owned(),
            command_id: None,
        };
        let what = match command {
            QueueCommand::Steer => "steer the turn",
            QueueCommand::Start => "send the message now",
            QueueCommand::Cancel => "remove the message from the queue",
        };
        self.run_command(
            what,
            async move {
                match command {
                    QueueCommand::Steer => connection.queue_steer(params).await,
                    QueueCommand::Start => connection.queue_start(params).await,
                    QueueCommand::Cancel => connection.queue_cancel(params).await,
                }
            },
            |_, _, _| {},
        );
    }

    /// Releases `key`'s held queue.
    pub fn resume_queue(&mut self, key: &ThreadKey) {
        let Some(connection) = self.thread_connection(key) else {
            return;
        };
        let params = ThreadParams {
            thread_id: key.thread_id.clone(),
            command_id: None,
        };
        self.run_command(
            "resume the queue",
            async move { connection.queue_resume(params).await },
            |_, _, _| {},
        );
    }

    /// Edits queue entry `entry_id`'s `text` in place, focused with the caret at the end.
    pub fn edit_queued(&mut self, key: &ThreadKey, entry_id: &str, text: &str) {
        let Some(thread) = self.threads.get_mut(key) else {
            return;
        };
        let mut edit = TextEdit::new(text);
        edit.focused = true;
        edit.anchor = text.len();
        edit.focus = text.len();
        thread.chrome.queue_edit = Some(QueueEdit {
            entry_id: entry_id.to_owned(),
            edit,
        });
    }

    /// Saves the queued message being edited (`queue.edit`; its attachments stay).
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
        let params = QueueEditParams {
            thread_id: key.thread_id.clone(),
            entry_id: edit.entry_id,
            text: edit.edit.text.clone(),
            attachments: None,
            command_id: None,
        };
        self.run_command(
            "edit the queued message",
            async move { connection.queue_edit(params).await },
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

/// The plan card: the latest todo list's tasks under their phases (a single phase goes
/// unnamed), folded by `todo-toggle`; None without tasks.
pub fn todo(ui: &Ui, key: &ThreadKey, state: &ThreadView, chrome: &ChromeUi) -> Option<El> {
    let k = ui.k;
    let (_, todo) = state.latest_todo()?;
    let tasks = || todo.phases.iter().flat_map(|phase| &phase.tasks);
    let total = tasks().count();
    if total == 0 {
        return None;
    }
    let done = tasks()
        .filter(|task| task.status == TodoStatus::Completed)
        .count();
    let open = chrome.todo_open;
    let summary = format!("{done}/{total} done");
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
    if !open {
        return Some(card);
    }
    let status = Status::of(k);
    let named = todo.phases.len() > 1;
    for (pix, phase) in todo.phases.iter().enumerate() {
        if named && !phase.tasks.is_empty() {
            card = card.child(
                text(&phase.name)
                    .key(format!("todo-phase-{pix}"))
                    .font(k.ty.text_xsmall_medium)
                    .color(k.c.text_muted)
                    .pt(4.0),
            );
        }
        for (tix, task) in phase.tasks.iter().enumerate() {
            let (glyph, color) = match task.status {
                TodoStatus::Completed => (&icons::CIRCLE_CHECK, status.success),
                TodoStatus::InProgress => (&icons::LOADER_CIRCLE, k.c.icon_accent),
                TodoStatus::Pending => (&icons::MINUS, k.c.icon_muted),
                TodoStatus::Blocked => (&icons::MINUS, status.warning),
                TodoStatus::Abandoned => (&icons::X, k.c.icon_muted),
            };
            let finished = matches!(task.status, TodoStatus::Completed | TodoStatus::Abandoned);
            let mut label = text(&task.text)
                .font(k.ty.text_small)
                .color(if finished {
                    k.c.text_muted
                } else {
                    k.c.text_base
                })
                .wrap();
            if finished {
                label = label.strike(0, task.text.len(), None);
            }
            card = card.child(
                row()
                    .key(format!("todo-step-{pix}-{tix}"))
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
pub fn queue(ui: &Ui, key: &ThreadKey, state: &ThreadView, chrome: &ChromeUi) -> Option<El> {
    let k = ui.k;
    if state.queue.is_empty() {
        return None;
    }
    let steerable = state.active_turn().is_some();
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
    for entry in &state.queue {
        let entry_id = entry.id.as_str();
        let editing = chrome
            .queue_edit
            .as_ref()
            .filter(|edit| edit.entry_id == entry_id);
        let body = match editing {
            Some(edit) => editing_row(k, key, entry_id, &edit.edit),
            None => queued_row(k, key, entry_id, &entry.text, steerable),
        };
        card = card.child(
            row()
                .key(format!("queued-{entry_id}"))
                .label(&entry.text)
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
fn queued_row(k: &Kit, key: &ThreadKey, entry_id: &str, message: &str, steerable: bool) -> El {
    let action =
        |label: &str, id: &str, help: Option<&str>, command: fn(&mut J3, &ThreadKey, &str)| {
            let (key, entry) = (key.clone(), entry_id.to_owned());
            let mut button = Button::new(label)
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small);
            if let Some(help) = help {
                button = button.help(help);
            }
            button
                .on_press(move |s: &mut J3| command(s, &key, &entry))
                .build(k)
                .key(format!("{id}-{entry_id}"))
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
    let (edit_entry, edit_text) = (entry_id.to_owned(), message.to_owned());
    let cancel_key = key.clone();
    let cancel_entry = entry_id.to_owned();
    body.child(action(
        "Send now",
        "send-now",
        Some("Stop the current turn and start this one"),
        J3::send_queued_now,
    ))
    .child(
        Button::new("Edit")
            .variant(ButtonVariant::Ghost)
            .control_size(ControlSize::Small)
            .on_press(move |s: &mut J3| s.edit_queued(&edit_key, &edit_entry, &edit_text))
            .build(k)
            .key(format!("edit-{entry_id}")),
    )
    .child(
        IconButton::new(&icons::X)
            .variant(IconButtonVariant::Ghost)
            .control_size(ControlSize::Small)
            .label("Remove from queue")
            .help("Remove from queue")
            .on_press(move |s: &mut J3| s.cancel_queued(&cancel_key, &cancel_entry))
            .build(k)
            .key(format!("cancel-{entry_id}")),
    )
}

/// The message in a field, with Save (or Enter) and Cancel.
fn editing_row(k: &Kit, key: &ThreadKey, entry_id: &str, edit: &TextEdit) -> El {
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
                    .key(format!("queue-input-{entry_id}")),
            ),
        )
        .child(
            Button::new("Save")
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small)
                .on_press(move |s: &mut J3| s.save_queued_edit(&save_key))
                .build(k)
                .key(format!("save-{entry_id}")),
        )
        .child(
            Button::new("Cancel")
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small)
                .on_press(move |s: &mut J3| s.cancel_queued_edit(&cancel_key))
                .build(k)
                .key(format!("cancel-edit-{entry_id}")),
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
