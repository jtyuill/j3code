//! Modal dialogs (spec §8.4): at most one is open, as a [`Dialog`] in `s.dialogs`, drawn
//! into the overlay layer by [`view`] with rune-kit's `Dialog`. Each variant owns its form
//! state; opening one replaces any other. The shared footer is Cancel (`dialog-cancel`) and
//! the primary action (`dialog-confirm`).
//!
//! To add a dialog: a variant here (with its state), a constructor, its arm in [`view`], and
//! a `J3` command that opens it with `s.open_dialog(..)`.

mod project;
mod snooze;

use rune::{Cx, El, TextEdit, Tx, col, row, spacer, text};
use rune_kit::{Button, ButtonVariant, Kit, TextInput};

pub use project::{Listing as FolderListing, ProjectPicker, title_from_path};
pub use snooze::{Schedule as SnoozeSchedule, SnoozeForm};

use crate::{
    app::{J3, Ui},
    sidebar::ThreadKey,
    theme::Status,
};

/// The open dialog and its form state. At most one exists, so variant sizes don't matter.
#[allow(clippy::large_enum_variant)]
pub enum Dialog {
    /// Add a project on a live environment through a folder browser; a new thread opens in
    /// it afterwards when the picker's `then_new_thread`.
    AddProject(ProjectPicker),
    /// Name + "Host or URL" for a remote environment.
    AddEnvironment {
        label: TextEdit,
        url: TextEdit,
        error: Option<String>,
    },
    /// Rename a thread; empty titles are refused.
    RenameThread { key: ThreadKey, title: TextEdit },
    /// Confirm deleting a thread (`archived`: from Settings' archive, which reloads after).
    DeleteThread {
        key: ThreadKey,
        title: String,
        archived: bool,
    },
    /// Confirm forgetting a remote environment.
    RemoveEnvironment { key: String, label: String },
    /// Confirm removing a project with no threads.
    RemoveProject {
        env: String,
        project_id: String,
        title: String,
    },
    /// Snooze a thread until a chosen time.
    CustomSnooze(SnoozeForm),
    /// Go to any agent across environments.
    Goto,
}

impl Dialog {
    pub fn add_project(env: String, then_new_thread: bool) -> Self {
        Self::AddProject(ProjectPicker::new(env, then_new_thread))
    }

    pub fn add_environment() -> Self {
        let mut label = TextEdit::new("");
        label.focused = true;
        Self::AddEnvironment {
            label,
            url: TextEdit::new(""),
            error: None,
        }
    }

    pub fn rename_thread(s: &J3, key: ThreadKey) -> Self {
        let title = s.thread_title(&key);
        let mut edit = TextEdit::new(title.clone());
        edit.focused = true;
        edit.set_selection(0, title.len());
        Self::RenameThread { key, title: edit }
    }

    pub fn delete_thread(s: &J3, key: ThreadKey) -> Self {
        let title = s.thread_title(&key);
        Self::DeleteThread {
            key,
            title,
            archived: false,
        }
    }

    /// Deleting an archived thread (titled `title`) from Settings.
    pub fn delete_archived(key: ThreadKey, title: String) -> Self {
        Self::DeleteThread {
            key,
            title,
            archived: true,
        }
    }

    pub fn remove_environment(s: &J3, key: String) -> Self {
        let label = s
            .envs
            .get(&key)
            .map_or_else(|| key.clone(), |env| env.label().to_owned());
        Self::RemoveEnvironment { key, label }
    }

    pub fn custom_snooze(key: ThreadKey) -> Self {
        Self::CustomSnooze(SnoozeForm::new(key))
    }

    pub fn goto() -> Self {
        Self::Goto
    }
}

/// Which text field of the open dialog a `TextInput` edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Label,
    Url,
    Title,
    Path,
    ProjectTitle,
    SnoozeAmount,
    SnoozeDate,
    SnoozeTime,
}

#[derive(Default)]
pub struct DialogState {
    pub open: Option<Dialog>,
    /// What a field accessor edits when the dialog it named has closed meanwhile.
    scratch: TextEdit,
    /// Numbers dialog openings and their requests app-wide, so a late reply can tell whether
    /// it is still the one asked for.
    serial: u64,
}

impl DialogState {
    /// A number no earlier call returned.
    pub fn next_serial(&mut self) -> u64 {
        self.serial += 1;
        self.serial
    }

    pub fn is_goto(&self) -> bool {
        matches!(self.open, Some(Dialog::Goto))
    }

    /// The open dialog's `field`, for `TextInput::on_edit`.
    pub fn field(&mut self, field: Field) -> &mut TextEdit {
        match (&mut self.open, field) {
            (Some(Dialog::AddEnvironment { label, .. }), Field::Label) => label,
            (Some(Dialog::AddEnvironment { url, .. }), Field::Url) => url,
            (Some(Dialog::RenameThread { title, .. }), Field::Title) => title,
            (Some(Dialog::AddProject(picker)), Field::Path) => &mut picker.path,
            (Some(Dialog::AddProject(picker)), Field::ProjectTitle) => &mut picker.title,
            (Some(Dialog::CustomSnooze(form)), Field::SnoozeAmount) => &mut form.amount,
            (Some(Dialog::CustomSnooze(form)), Field::SnoozeDate) => &mut form.date,
            (Some(Dialog::CustomSnooze(form)), Field::SnoozeTime) => &mut form.time,
            _ => &mut self.scratch,
        }
    }
}

impl J3 {
    /// Opens `dialog`, replacing any open one.
    pub fn open_dialog(&mut self, dialog: Dialog, _: &mut Tx) {
        let picker = matches!(dialog, Dialog::AddProject(_));
        self.dialogs.open = Some(dialog);
        if picker {
            self.project_picker_opened();
        }
    }

    pub fn close_dialog(&mut self) {
        self.dialogs.open = None;
    }

    /// The open dialog's primary action. A refused form stays open with its reason.
    pub fn confirm_dialog(&mut self, tx: &mut Tx) {
        match &self.dialogs.open {
            Some(Dialog::AddProject(_)) => return self.create_project(),
            Some(Dialog::CustomSnooze(_)) => return self.confirm_snooze(),
            Some(Dialog::Goto) | None => return,
            Some(_) => {}
        }
        let Some(dialog) = self.dialogs.open.take() else {
            return;
        };
        match dialog {
            Dialog::RenameThread { key, title } => {
                if !self.rename_thread(&key, &title.text) {
                    self.dialogs.open = Some(Dialog::RenameThread { key, title });
                }
            }
            Dialog::DeleteThread {
                key,
                archived: true,
                ..
            } => self.delete_archived(key),
            Dialog::DeleteThread { key, .. } => self.delete_thread(key),
            Dialog::AddEnvironment { label, url, .. } => {
                if let Err(error) = self.add_environment(&label.text, &url.text) {
                    self.dialogs.open = Some(Dialog::AddEnvironment {
                        label,
                        url,
                        error: Some(error),
                    });
                }
            }
            Dialog::RemoveEnvironment { key, .. } => {
                if let Err(error) = self.remove_environment(&key, tx) {
                    self.toasts.error(error);
                }
            }
            Dialog::RemoveProject {
                env, project_id, ..
            } => self.remove_project(&env, project_id),
            dialog @ (Dialog::AddProject(_) | Dialog::CustomSnooze(_) | Dialog::Goto) => {
                self.dialogs.open = Some(dialog);
            }
        }
    }

    /// Asks to remove a project: refused with a notification while it has threads.
    pub fn ask_remove_project(&mut self, env: &str, project_id: &str, tx: &mut Tx) {
        let Some(state) = self.envs.get(env).map(|e| e.state().clone()) else {
            return;
        };
        let Some(project) = state.shell.project(project_id) else {
            return;
        };
        let threads = state.shell.threads_of(project_id).len();
        if threads > 0 {
            self.toasts.error(format!(
                "“{}” still has {threads} thread(s). Archive or delete them first.",
                project.title
            ));
            return;
        }
        let dialog = Dialog::RemoveProject {
            env: env.to_owned(),
            project_id: project_id.to_owned(),
            title: project.title.clone(),
        };
        self.open_dialog(dialog, tx);
    }

    fn remove_project(&mut self, env: &str, project_id: String) {
        let Some(connection) = self.envs.get(env).map(|e| e.connection().clone()) else {
            return;
        };
        self.run_command(
            "remove the project",
            async move { connection.delete_project(&project_id).await },
            |_, _, _| {},
        );
    }
}

/// Cancel and the primary action (disabled while `busy`).
fn footer(k: &Kit, confirm: &str, danger: bool, busy: bool) -> El {
    row()
        .key("dialog-footer")
        .w_full()
        .gap(8.0)
        .padding(16.0)
        .child(spacer())
        .child(
            Button::new("Cancel")
                .variant(ButtonVariant::Outline)
                .on_press(|s: &mut J3| s.close_dialog())
                .build(k)
                .key("dialog-cancel"),
        )
        .child(
            Button::new(confirm)
                .variant(if danger {
                    ButtonVariant::Danger
                } else {
                    ButtonVariant::Contrast
                })
                .disabled(busy)
                .on_press_tx(|s: &mut J3, tx: &mut Tx| s.confirm_dialog(tx))
                .build(k)
                .key("dialog-confirm"),
        )
}

/// A labelled one-line field of the open dialog.
fn labelled(
    k: &Kit,
    title: &str,
    edit: &TextEdit,
    field: Field,
    placeholder: &str,
    key: &str,
) -> El {
    col()
        .w_full()
        .gap(6.0)
        .child(
            text(title.to_owned())
                .font(k.ty.text_small_medium)
                .color(k.c.text_base),
        )
        .child(
            TextInput::new(edit)
                .width(None)
                .placeholder(placeholder)
                .label(title)
                .on_edit(move |s: &mut J3| s.dialogs.field(field))
                .on_submit(|s: &mut J3, tx: &mut Tx| s.confirm_dialog(tx))
                .build(k)
                .key(key.to_owned()),
        )
}

/// A form body over the footer.
fn form(body: El, footer: El) -> El {
    col()
        .w_full()
        .child(col().w_full().px(16.0).child(body))
        .child(footer)
}

fn error_line(k: &Kit, error: &Option<String>) -> Option<El> {
    error.as_ref().map(|error| {
        text(error.clone())
            .key("dialog-error")
            .font(k.ty.text_xsmall)
            .color(Status::of(k).danger)
            .wrap()
    })
}

/// Draws the open dialog into the overlay layer.
pub fn view(s: &J3, cx: &mut Cx<J3>, ui: &mut Ui) {
    let Some(dialog) = &s.dialogs.open else {
        return;
    };
    let k = ui.k;
    let close = |s: &mut J3| s.close_dialog();
    let (key, title, description, body, width): (&str, String, Option<String>, El, f32) =
        match dialog {
            Dialog::RenameThread { title, .. } => (
                "rename-thread",
                "Rename thread".into(),
                None,
                form(
                    TextInput::new(title)
                        .width(None)
                        .placeholder("Title")
                        .label("Title")
                        .on_edit(|s: &mut J3| s.dialogs.field(Field::Title))
                        .on_submit(|s: &mut J3, tx: &mut Tx| s.confirm_dialog(tx))
                        .build(k)
                        .key("rename-title"),
                    footer(k, "Rename", false, false),
                ),
                400.0,
            ),
            Dialog::DeleteThread { title, .. } => (
                "delete-thread",
                format!("Delete “{title}”?"),
                Some(
                    "The conversation is removed from j3. omp’s session file stays on disk.".into(),
                ),
                footer(k, "Delete", true, false),
                400.0,
            ),
            Dialog::RemoveEnvironment { label, .. } => (
                "remove-environment",
                format!("Remove “{label}”?"),
                Some("Its threads stay on that server.".into()),
                footer(k, "Remove", true, false),
                400.0,
            ),
            Dialog::RemoveProject { title, .. } => (
                "remove-project",
                format!("Remove “{title}”?"),
                Some("Files on disk aren’t deleted.".into()),
                footer(k, "Remove", true, false),
                400.0,
            ),
            Dialog::AddEnvironment { label, url, error } => {
                let mut fields = col()
                    .w_full()
                    .gap(12.0)
                    .child(labelled(
                        k,
                        "Name",
                        label,
                        Field::Label,
                        "revived",
                        "environment-label",
                    ))
                    .child(
                        col()
                            .w_full()
                            .gap(6.0)
                            .child(labelled(
                                k,
                                "Host or URL",
                                url,
                                Field::Url,
                                "revived or http://revived:3780",
                                "environment-url",
                            ))
                            .child(
                                text("Port 3780 is used when none is given.")
                                    .font(k.ty.text_xsmall)
                                    .color(k.c.text_muted),
                            ),
                    );
                if let Some(error) = error_line(k, error) {
                    fields = fields.child(error);
                }
                (
                    "add-environment",
                    "Add environment".into(),
                    None,
                    form(fields, footer(k, "Add", false, false)),
                    400.0,
                )
            }
            Dialog::AddProject(picker) => (
                "add-project",
                "Add project".into(),
                None,
                form(
                    project::body(s, k, picker),
                    footer(k, "Add Project", false, picker.creating),
                ),
                544.0,
            ),
            Dialog::CustomSnooze(snooze) => (
                "custom-snooze",
                "Custom snooze".into(),
                None,
                form(snooze::body(k, snooze), footer(k, "Snooze", false, false)),
                400.0,
            ),
            Dialog::Goto => (
                "goto",
                "Go to agent".into(),
                None,
                crate::goto::body(s, cx, ui),
                576.0,
            ),
        };
    let mut panel = rune_kit::Dialog::new(key, title)
        .open(true)
        .fit()
        .width(width)
        .on_close(close)
        .child(body);
    if let Some(description) = description {
        panel = panel.description(description);
    }
    panel.build(k, ui.layer);
}

#[cfg(test)]
mod tests;
