//! Archived threads (spec §8.3): each environment's archived threads, straight from its
//! shell (`thread.archived_at` set), newest first, with Unarchive (which opens the restored
//! thread) and Delete… (behind an alert). Shown on Settings' "Archived threads" page, one
//! group per environment.

use std::collections::HashMap;

use j3_client::{
    Shell,
    entities::Thread,
    methods::{DeleteThreadParams, ThreadParams},
};
use rune::{El, Tx, col, row, text};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit, Spinner};

use crate::{
    app::{J3, Ui},
    dialogs::Dialog,
    feed::relative_age,
    icons,
    sidebar::{ThreadKey, parse_time},
    theme::Status,
};

/// The last unarchive/delete failure of each environment, by environment key.
#[derive(Default)]
pub struct Archives {
    errors: HashMap<String, String>,
}

impl Archives {
    pub fn error(&self, env: &str) -> Option<&str> {
        self.errors.get(env).map(String::as_str)
    }

    pub fn clear(&mut self) {
        self.errors.clear();
    }
}

impl J3 {
    /// Restores `key`'s thread, then opens it.
    pub fn unarchive_thread(&mut self, key: ThreadKey) {
        let Some(connection) = self.envs.get(&key.env).map(|e| e.connection().clone()) else {
            return;
        };
        self.settings.archived.errors.remove(&key.env);
        let thread_id = key.thread_id.clone();
        self.spawn(async move {
            let result = connection
                .threads_unarchive(ThreadParams {
                    thread_id,
                    command_id: None,
                })
                .await;
            move |s: &mut J3, tx: &mut Tx| match result {
                Ok(_) => s.open_thread(key, tx),
                Err(e) => s.archive_failed(&key.env, e.to_string()),
            }
        });
    }

    /// Deletes archived thread `key` (after the alert confirmed); the shell drops its row.
    pub fn delete_archived(&mut self, key: ThreadKey) {
        let Some(connection) = self.envs.get(&key.env).map(|e| e.connection().clone()) else {
            return;
        };
        self.settings.archived.errors.remove(&key.env);
        let thread_id = key.thread_id.clone();
        self.spawn(async move {
            let result = connection
                .threads_delete(DeleteThreadParams {
                    thread_id,
                    only_if_unused: None,
                    command_id: None,
                })
                .await;
            move |s: &mut J3, _: &mut Tx| {
                if let Err(e) = result {
                    s.archive_failed(&key.env, e.to_string());
                }
            }
        });
    }

    fn archive_failed(&mut self, env: &str, error: String) {
        self.settings.archived.errors.insert(env.to_owned(), error);
    }
}

/// Environment `env`'s archived threads (`archived-{env}`).
pub fn view(s: &J3, ui: &Ui, env: &str) -> El {
    let k = ui.k;
    let mut column = col()
        .key(format!("archived-{env}"))
        .w_full()
        .gap(4.0)
        .px(16.0)
        .py(12.0);
    if let Some(error) = s.settings.archived.error(env) {
        column = column.child(
            text(error.to_owned())
                .font(k.ty.text_xsmall)
                .color(Status::of(k).danger)
                .wrap(),
        );
    }
    let Some(state) = s.envs.get(env).map(|e| e.state().clone()) else {
        return column;
    };
    let shell = &state.shell;
    // Until the shell first syncs, its absence of archived rows means nothing yet.
    if !shell.synced {
        return column.child(Spinner::new().size(16.0).build(k));
    }
    let threads = newest_first(shell);
    if threads.is_empty() {
        return column.child(
            text("No archived threads")
                .font(k.ty.text_small)
                .color(k.c.text_muted),
        );
    }
    for thread in threads {
        let project = shell
            .projects
            .get(&thread.project_id)
            .map(|p| p.title.as_str())
            .unwrap_or_default();
        let age = thread
            .archived_at
            .as_deref()
            .and_then(|at| relative_age(at, s.clock.now))
            .unwrap_or_default();
        let key = ThreadKey::new(env, thread.id.clone());
        column = column.child(archived_row(k, &key, &thread.title, project, &age));
    }
    column
}

/// The shell's archived threads, most recently archived first (ties by id).
fn newest_first(shell: &Shell) -> Vec<&Thread> {
    let mut threads: Vec<_> = shell
        .threads
        .values()
        .filter(|thread| thread.archived_at.is_some())
        .collect();
    threads.sort_by(|a, b| {
        let at = |t: &Thread| t.archived_at.as_deref().and_then(parse_time);
        at(b).cmp(&at(a)).then_with(|| a.id.cmp(&b.id))
    });
    threads
}

fn archived_row(k: &Kit, key: &ThreadKey, title: &str, project: &str, age: &str) -> El {
    let unarchive = key.clone();
    let delete = key.clone();
    let delete_title = title.to_owned();
    row()
        .key(format!("archived-{}", key.thread_id))
        .w_full()
        .gap(8.0)
        .py(4.0)
        .center_y()
        .child(
            text(title.to_owned())
                .font(k.ty.text_small)
                .color(k.c.text_base)
                .truncate()
                .grow(1.0)
                .shrink()
                .min_w(0.0),
        )
        .child(
            text(project.to_owned())
                .font(k.ty.text_xsmall)
                .color(k.c.text_muted),
        )
        .child(
            row().w(40.0).child(
                text(age.to_owned())
                    .font(k.ty.text_xsmall)
                    .color(k.c.text_muted),
            ),
        )
        .child(
            Button::new("Unarchive")
                .icon(&icons::UNDO_2)
                .variant(ButtonVariant::Outline)
                .control_size(ControlSize::Small)
                .on_press(move |s: &mut J3| s.unarchive_thread(unarchive.clone()))
                .build(k)
                .key(format!("unarchive-{}", key.thread_id)),
        )
        .child(
            Button::new("Delete…")
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small)
                .on_press_tx(move |s: &mut J3, tx: &mut Tx| {
                    let dialog = Dialog::delete_archived(delete.clone(), delete_title.clone());
                    s.open_dialog(dialog, tx);
                })
                .build(k)
                .key(format!("delete-{}", key.thread_id)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archived_threads_list_newest_first() {
        let state = crate::app::fixtures::local_state();
        let ids: Vec<&str> = newest_first(&state.shell)
            .iter()
            .map(|t| t.id.as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "thr-archived-recent",
                "thr-archived-week",
                "thr-archived-old"
            ]
        );
    }
}
