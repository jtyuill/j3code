//! The commit dialog (t3's, minus the per-file checkboxes), drawn by the control into the
//! overlay layer: the branch, the changed files with their line counts and totals, an
//! optional message, and Commit, Commit & push, or Commit, push & PR.

use j3_protocol::t3::git::{GitStackedAction, VcsChangeRequestState, VcsStatusResult};
use rune::{El, TextEdit, TextStyle, col, row, spacer, text};
use rune_kit::{Button, ButtonVariant, Kit, TextArea};

use super::GitControl;
use crate::{
    app::{J3, Ui},
    sidebar::ThreadKey,
    theme::Status,
};

/// The open dialog: the status it was opened over.
pub(super) struct CommitForm {
    status: VcsStatusResult,
}

impl CommitForm {
    /// A GitHub feature branch without an open pull request can open one.
    fn offers_pr(&self) -> bool {
        let status = &self.status;
        status.source_control_provider.is_some()
            && !status.is_default_ref
            && status.has_primary_remote
            && status
                .pr
                .as_ref()
                .is_none_or(|pr| pr.state != VcsChangeRequestState::Open)
    }

    /// Commit & push needs somewhere to push to.
    fn can_push(&self) -> bool {
        self.status.has_upstream || self.status.has_primary_remote
    }
}

impl GitControl {
    /// The commit dialog's message field, for its `on_edit`.
    fn message(&mut self) -> &mut TextEdit {
        &mut self.message
    }
}

impl J3 {
    /// Opens the commit dialog over `key`'s current status, the message focused; nothing
    /// while an action runs or before a status loaded.
    pub fn open_commit_dialog(&mut self, key: &ThreadKey) {
        let Some(git) = self.git.get_mut(key) else {
            return;
        };
        let Some(status) = git.status.clone().filter(|_| !git.busy()) else {
            return;
        };
        git.menu_open = false;
        git.commit = Some(CommitForm { status });
        git.message = TextEdit::default();
        git.message.focused = true;
    }

    pub fn close_commit_dialog(&mut self, key: &ThreadKey) {
        if let Some(git) = self.git.get_mut(key) {
            git.commit = None;
        }
    }

    /// Runs `action` from the dialog with its message (empty: generated) and closes it.
    fn commit_from_dialog(&mut self, key: &ThreadKey, action: GitStackedAction) {
        let Some(git) = self.git.get_mut(key) else {
            return;
        };
        if git.commit.take().is_none() {
            return;
        }
        let text = git.message.text.trim();
        let message = (!text.is_empty()).then(|| text.to_owned());
        self.run_git_action(key, action, message);
    }
}

/// `key`'s commit dialog into the overlay layer, while open.
pub(super) fn view(git: &GitControl, ui: &mut Ui, key: &ThreadKey) {
    let Some(form) = &git.commit else {
        return;
    };
    let k = ui.k;
    let body = col()
        .key("git-commit-body")
        .w_full()
        .px(16.0)
        .gap(12.0)
        .child(
            row()
                .gap(8.0)
                .child(text("Branch").font(k.ty.text_small).color(k.c.text_muted))
                .child(
                    text(
                        form.status
                            .ref_name
                            .clone()
                            .unwrap_or_else(|| "(detached HEAD)".to_owned()),
                    )
                    .key("git-commit-branch")
                    .font(k.ty.text_small_medium)
                    .color(k.c.text_base)
                    .min_w(0.0)
                    .truncate(),
                ),
        )
        .child(
            col()
                .gap(4.0)
                .child(text("Files").font(k.ty.text_small).color(k.c.text_muted))
                .child(files(&form.status, ui.m.rems(11.0), k)),
        )
        .child(
            col()
                .gap(4.0)
                .child(
                    text("Commit message (optional)")
                        .font(k.ty.text_small_medium)
                        .color(k.c.text_base),
                )
                .child({
                    let key = key.clone();
                    TextArea::new(&git.message)
                        .placeholder("Leave empty to auto-generate")
                        .label("Commit message")
                        .width(None)
                        .auto_grow(3, 8)
                        .on_edit(move |s: &mut J3| s.git.entry(key.clone()).or_default().message())
                        .build(k)
                        .key("git-commit-message")
                }),
        )
        .child(footer(form, k, key));
    let close = key.clone();
    rune_kit::Dialog::new("git-commit-dialog", "Commit changes")
        .open(true)
        .fit()
        .width(ui.m.rems(32.0))
        .on_close(move |s: &mut J3| s.close_commit_dialog(&close))
        .child(body)
        .build(k, ui.layer);
}

/// `+n / −n` in the success and danger colours.
fn counts(insertions: u64, deletions: u64, font: TextStyle, k: &Kit) -> El {
    let status = Status::of(k);
    row()
        .child(
            text(format!("+{insertions}"))
                .font(font)
                .color(status.success),
        )
        .child(text(" / ").font(font).color(k.c.text_muted))
        .child(
            text(format!("\u{2212}{deletions}"))
                .font(font)
                .color(status.danger),
        )
}

/// The changed files with their counts (`git-commit-files`), then the totals; "none" when
/// the tree is clean.
fn files(status: &VcsStatusResult, max_h: f32, k: &Kit) -> El {
    let files = &status.working_tree.files;
    if files.is_empty() {
        return text("none")
            .font(k.ty.text_small_medium)
            .color(k.c.text_base);
    }
    let mono = TextStyle {
        size: k.ty.text_xsmall.size,
        ..k.ty.mono
    };
    let list = col()
        .key("git-commit-files")
        .label(format!("{} changed files", files.len()))
        .w_full()
        .max_h(max_h)
        .padding(4.0)
        .radius(k.radius.sm)
        .border(1.0, k.c.border_muted)
        .scroll(rune::ScrollSpec {
            stick_bottom: false,
        })
        .children(files.iter().map(|file| {
            row()
                .key(format!("git-commit-file-{}", file.path))
                .w_full()
                .gap(12.0)
                .px(8.0)
                .py(2.0)
                .center_y()
                .child(
                    text(file.path.clone())
                        .font(mono)
                        .color(k.c.text_base)
                        .grow(1.0)
                        .min_w(0.0)
                        .truncate(),
                )
                .child(counts(file.insertions, file.deletions, mono, k))
        }));
    let noun = if files.len() == 1 { "file" } else { "files" };
    col().w_full().gap(4.0).child(list).child(
        row()
            .key("git-commit-totals")
            .w_full()
            .center_y()
            .child(
                text(format!("{} {noun}", files.len()))
                    .font(k.ty.text_xsmall)
                    .color(k.c.text_muted),
            )
            .child(spacer())
            .child(counts(
                status.working_tree.insertions,
                status.working_tree.deletions,
                mono,
                k,
            )),
    )
}

/// Cancel, then Commit, push & PR (when it applies), Commit & push and Commit.
fn footer(form: &CommitForm, k: &Kit, key: &ThreadKey) -> El {
    let action = |label: &str, variant, run: GitStackedAction| {
        let key = key.clone();
        Button::new(label)
            .variant(variant)
            .on_press(move |s: &mut J3| s.commit_from_dialog(&key, run))
    };
    let cancel = key.clone();
    let mut footer = row()
        .key("dialog-footer")
        .w_full()
        .gap(8.0)
        .padding(16.0)
        .child(spacer())
        .child(
            Button::new("Cancel")
                .variant(ButtonVariant::Ghost)
                .on_press(move |s: &mut J3| s.close_commit_dialog(&cancel))
                .build(k)
                .key("dialog-cancel"),
        );
    if form.offers_pr() {
        footer = footer.child(
            action(
                "Commit, push & PR",
                ButtonVariant::Outline,
                GitStackedAction::CommitPushPr,
            )
            .build(k)
            .key("git-commit-push-pr"),
        );
    }
    footer
        .child(
            action(
                "Commit & push",
                ButtonVariant::Outline,
                GitStackedAction::CommitPush,
            )
            .disabled(!form.can_push())
            .build(k)
            .key("git-commit-push"),
        )
        .child(
            action("Commit", ButtonVariant::Contrast, GitStackedAction::Commit)
                .build(k)
                .key("git-commit"),
        )
}

/// Whether the dialog over `status` offers Commit, push & PR / Commit & push, for tests.
#[cfg(test)]
pub(super) fn offers(status: VcsStatusResult) -> (bool, bool) {
    let form = CommitForm { status };
    (form.offers_pr(), form.can_push())
}
