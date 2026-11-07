//! Archived threads (spec §8.3): each environment's archive, loaded with
//! `connection.archived()` when Settings opens, newest first, with Unarchive (which opens the
//! restored thread) and Delete… (behind an alert). Shown on Settings' "Archived threads"
//! page, one group per environment.

use std::collections::HashMap;

use j3_client::t3::methods::ArchivedShellSnapshot;
use rune::{El, Tx, col, row, text};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit, Spinner};

use crate::{
    app::{J3, Ui},
    dialogs::Dialog,
    feed::relative_age,
    icons,
    sidebar::ThreadKey,
    theme::Status,
};

/// One environment's archive.
pub enum Listing {
    Loading,
    Loaded(ArchivedShellSnapshot),
    Failed(String),
}

/// One environment's archive and the last unarchive/delete failure.
pub struct Archive {
    pub listing: Listing,
    pub error: Option<String>,
}

/// Every environment's archive, by environment key.
#[derive(Default)]
pub struct Archives {
    by_env: HashMap<String, Archive>,
}

impl Archives {
    pub fn get(&self, env: &str) -> Option<&Archive> {
        self.by_env.get(env)
    }

    /// Shows `env`'s archive as `listing` (fixtures; [`J3::load_archived`] otherwise).
    pub fn set(&mut self, env: &str, listing: Listing) {
        self.by_env.insert(
            env.to_owned(),
            Archive {
                listing,
                error: None,
            },
        );
    }

    pub fn clear(&mut self) {
        self.by_env.clear();
    }
}

impl J3 {
    /// (Re)loads environment `env`'s archived threads.
    pub fn load_archived(&mut self, env: &str) {
        let Some(connection) = self.envs.get(env).map(|e| e.connection().clone()) else {
            return;
        };
        let archives = &mut self.settings.archived.by_env;
        match archives.get_mut(env) {
            // A reload keeps showing the list it has until the new one arrives.
            Some(archive) => archive.error = None,
            None => self.settings.archived.set(env, Listing::Loading),
        }
        let key = env.to_owned();
        self.spawn(async move {
            let result = connection.archived().await;
            move |s: &mut J3, _: &mut Tx| {
                if let Some(archive) = s.settings.archived.by_env.get_mut(&key) {
                    archive.listing = match result {
                        Ok(snapshot) => Listing::Loaded(snapshot),
                        Err(e) => Listing::Failed(e.to_string()),
                    };
                }
            }
        });
    }

    /// Restores `key`'s thread, then opens it.
    pub fn unarchive_thread(&mut self, key: ThreadKey) {
        let Some(connection) = self.envs.get(&key.env).map(|e| e.connection().clone()) else {
            return;
        };
        let id = key.thread_id.clone();
        self.spawn(async move {
            let result = connection.unarchive(&id).await;
            move |s: &mut J3, tx: &mut Tx| match result {
                Ok(_) => {
                    s.load_archived(&key.env);
                    s.open_thread(key, tx);
                }
                Err(e) => s.archive_failed(&key.env, e.to_string()),
            }
        });
    }

    /// Deletes archived thread `key` (after the alert confirmed) and reloads the archive.
    pub fn delete_archived(&mut self, key: ThreadKey) {
        let Some(connection) = self.envs.get(&key.env).map(|e| e.connection().clone()) else {
            return;
        };
        let id = key.thread_id.clone();
        self.spawn(async move {
            let result = connection.delete_thread(&id).await;
            move |s: &mut J3, _: &mut Tx| match result {
                Ok(_) => s.load_archived(&key.env),
                Err(e) => s.archive_failed(&key.env, e.to_string()),
            }
        });
    }

    fn archive_failed(&mut self, env: &str, error: String) {
        if let Some(archive) = self.settings.archived.by_env.get_mut(env) {
            archive.error = Some(error);
        }
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
    let Some(archive) = s.settings.archived.get(env) else {
        return column.child(Spinner::new().size(16.0).build(k));
    };
    if let Some(error) = &archive.error {
        column = column.child(
            text(error.clone())
                .font(k.ty.text_xsmall)
                .color(Status::of(k).danger)
                .wrap(),
        );
    }
    match &archive.listing {
        Listing::Loading => column.child(Spinner::new().size(16.0).build(k)),
        Listing::Failed(error) => column.child(
            text(format!("Couldn’t load archived threads: {error}"))
                .font(k.ty.text_small)
                .color(Status::of(k).danger)
                .wrap(),
        ),
        Listing::Loaded(snapshot) if snapshot.threads.is_empty() => column.child(
            text("No archived threads")
                .font(k.ty.text_small)
                .color(k.c.text_muted),
        ),
        Listing::Loaded(snapshot) => {
            for thread in newest_first(snapshot) {
                let project = snapshot
                    .projects
                    .iter()
                    .find(|p| p.id == thread.project_id)
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
    }
}

/// The snapshot's threads, most recently archived first.
fn newest_first(
    snapshot: &ArchivedShellSnapshot,
) -> Vec<&j3_client::t3::orchestration::ThreadShell> {
    let mut threads: Vec<_> = snapshot.threads.iter().collect();
    threads.sort_by(|a, b| b.archived_at.cmp(&a.archived_at));
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
        let snapshot = crate::app::fixtures::archived_snapshot();
        let ids: Vec<&str> = newest_first(&snapshot)
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
