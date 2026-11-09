//! The completion menu (spec §7.6): `/` commands and skills at the start of the draft, `^`
//! models anywhere a mention may go and `@` files and directories (searched on the server, as
//! omp's editor does), in a rune-kit `PopupList` above the field. The field keeps focus; ↑ ↓
//! Tab Enter Escape reach it through `draft::composer_key`.

use j3_client::methods::{EntryKind, SearchEntriesParams};
use rune::{Align, Side, Tx, text};
use rune_kit::{PopupItem, PopupList};

use super::{ComposerKey, ComposerState, draft};
use crate::{
    app::{J3, Ui},
    completion::{self, Completion, CompletionKind},
    mention,
};

/// Rows shown at once; the list scrolls with the selection.
pub const MAX_COMPLETIONS: usize = 8;
/// Models a `^` mention lists.
pub const MAX_MODEL_COMPLETIONS: usize = 20;
/// Rows kept from a search.
const KEPT: usize = MAX_COMPLETIONS * 4;
/// The popup list's row height.
const ROW: f32 = 28.0;

impl ComposerState {
    /// Closes the menu; a search still on its way no longer opens it.
    pub(super) fn close_completions(&mut self) {
        self.completions.clear();
        self.path_query = None;
    }
}

impl J3 {
    /// Recomputes `key`'s menu from its draft and caret, selecting the first row. An `@`
    /// reference's rows come from the server.
    pub(super) fn refresh_completions(&mut self, key: &ComposerKey) {
        let composer = self.composer(key);
        let (text, cursor) = (composer.edit.text.clone(), composer.edit.focus);
        let (a, b) = composer.edit.range();
        if a == b
            && j3_text::mention::allowed(&text)
            && let Some(path) = completion::path_query(&text, cursor)
        {
            self.search_paths(key, path.query.to_owned());
            return;
        }
        let mut rows = if a != b {
            Vec::new()
        } else if let Some(query) = completion::slash_query(&text, cursor) {
            self.composer_catalog(key).complete(query)
        } else {
            self.model_completions(key, &text, cursor)
        };
        rows.truncate(KEPT);
        let composer = self.composer(key);
        composer.path_query = None;
        composer.completions = rows;
        composer.completion_ix = 0;
    }

    /// Asks `key`'s environment for the entries `@query` names in its project, unless that
    /// answer shows or is on its way. The menu keeps the paths it shows until the answer
    /// replaces them.
    fn search_paths(&mut self, key: &ComposerKey, query: String) {
        let composer = self.composer(key);
        composer
            .completions
            .retain(|row| matches!(row.kind, CompletionKind::Path { .. }));
        if composer.path_query.as_ref() == Some(&query) {
            return;
        }
        composer.path_query = Some(query.clone());
        let target = self.composer_target(key);
        let search = target
            .env
            .filter(|env| env.is_live())
            .zip(target.project_id)
            .map(|(env, project_id)| (env.connection().clone(), project_id));
        let Some((connection, project_id)) = search else {
            self.composer(key).completions.clear();
            return;
        };
        let params = SearchEntriesParams {
            project_id,
            query: query.clone(),
            limit: Some(KEPT as u32),
            kind: None,
            images_only: None,
        };
        let key = key.clone();
        self.spawn(async move {
            let project_id = params.project_id.clone();
            let found = connection.projects_search_entries(params).await;
            move |s: &mut J3, tx: &mut Tx| {
                let Some(composer) = s.composers.get_mut(&key) else {
                    return;
                };
                if composer.path_query.as_ref() != Some(&query) {
                    return;
                }
                composer.completions = match found {
                    Ok(found) => found
                        .entries
                        .into_iter()
                        .map(|entry| {
                            let directory = entry.kind == EntryKind::Dir;
                            completion::path_completion(entry.path, directory)
                        })
                        .collect(),
                    Err(e) => {
                        tracing::debug!("searching project {project_id} for @{query}: {e}");
                        Vec::new()
                    }
                };
                composer.completion_ix = 0;
                tx.instant();
            }
        });
    }

    /// The models a `^` mention being typed at `cursor` offers.
    fn model_completions(&self, key: &ComposerKey, text: &str, cursor: usize) -> Vec<Completion> {
        let Some((_, query)) = j3_text::mention::query_at(text, cursor) else {
            return Vec::new();
        };
        let Some(env) = self.envs.get(key.env()) else {
            return Vec::new();
        };
        let choices = env.models();
        let favorites = &self.prefs.ui().favorite_models;
        mention::rank(choices, favorites, query, MAX_MODEL_COMPLETIONS)
            .into_iter()
            .filter_map(|ix| choices.get(ix))
            .map(|choice| Completion {
                kind: CompletionKind::Model(choice.slug().to_owned()),
                label: choice.label.text().to_owned(),
                description: None,
                hint: None,
            })
            .collect()
    }

    /// Takes the selected row: a command or skill replaces the leading token (caret at the
    /// end), a model replaces the `^query` with its chip and a space, a path replaces the
    /// `@query` (a directory's entries listed next).
    pub(super) fn accept_completion(&mut self, key: &ComposerKey, tx: &mut Tx) {
        let composer = self.composer(key);
        let Some(row) = composer.completions.get(composer.completion_ix).cloned() else {
            return;
        };
        composer.close_completions();
        composer.ghost = None;
        match &row.kind {
            CompletionKind::Model(slug) => {
                self.accept_model(key, slug);
                return;
            }
            CompletionKind::Path { path, directory } => {
                self.accept_path(key, path, *directory);
                tx.instant();
                return;
            }
            CompletionKind::Command | CompletionKind::Skill => {}
        }
        let edit = &mut composer.edit;
        // `completion::apply` as an edit of the leading token (and the whitespace after it),
        // so the rest of the draft keeps its chips.
        let text = &edit.text;
        let end = text.find(char::is_whitespace).map_or(text.len(), |start| {
            start + text[start..].len() - text[start..].trim_start().len()
        });
        edit.set_selection(0, end);
        edit.insert(&format!("{} ", row.label));
        edit.set_caret(edit.text.len());
        tx.instant();
    }

    /// Replaces the `@query` before the caret with `path`'s reference; a directory's
    /// entries come next.
    fn accept_path(&mut self, key: &ComposerKey, path: &str, directory: bool) {
        let edit = &mut self.composer(key).edit;
        let Some(at) = completion::path_query(&edit.text, edit.focus) else {
            return;
        };
        let insert = completion::path_insert(path, directory, at.quoted);
        edit.set_selection(at.start, edit.focus);
        edit.insert(&insert);
        if directory {
            self.refresh_completions(key);
        }
    }

    /// Replaces the `^query` before the caret with model `slug`'s chip and a space.
    fn accept_model(&mut self, key: &ComposerKey, slug: &str) {
        let Some(label) = self
            .envs
            .get(key.env())
            .and_then(|env| mention::find_choice(env.models(), slug))
            .map(|choice| choice.label.text().to_owned())
        else {
            return;
        };
        let edit = &mut self.composer(key).edit;
        let Some((range, _)) = j3_text::mention::query_at(&edit.text, edit.focus) else {
            return;
        };
        edit.replace_with_pill(range, draft::mention_id(slug), &label);
        edit.insert(" ");
    }

    /// The row whose id is `id` was clicked.
    fn choose_completion(&mut self, key: &ComposerKey, id: &str, tx: &mut Tx) {
        let composer = self.composer(key);
        if let Some(ix) = composer
            .completions
            .iter()
            .position(|row| row_id(row) == id)
        {
            composer.completion_ix = ix;
            self.accept_completion(key, tx);
        }
        self.composer(key).edit.focused = true;
    }
}

/// A row's id: the label for commands and skills, the slug for models, the path for paths.
fn row_id(row: &Completion) -> &str {
    match &row.kind {
        CompletionKind::Model(slug) => slug,
        CompletionKind::Path { path, .. } => path,
        CompletionKind::Command | CompletionKind::Skill => &row.label,
    }
}

/// The open menu, pushed into the layer above the composer (`anchor`).
pub(super) fn view(s: &J3, ui: &mut Ui, key: &ComposerKey, anchor: &str, width: f32) {
    let Some(composer) = s.composers.get(key) else {
        return;
    };
    if composer.completions.is_empty() {
        return;
    }
    let k = ui.k;
    let tag = |t: &str| text(t).font(k.ty.text_xsmall).color(k.c.text_muted);
    let items: Vec<PopupItem> = composer
        .completions
        .iter()
        .map(|row| {
            let mut detail = Vec::new();
            detail.extend(row.hint.as_deref());
            detail.extend(row.description.as_deref());
            let mut item = PopupItem::new(row_id(row), &row.label).description(detail.join("  "));
            match row.kind {
                CompletionKind::Skill => item = item.trailing(tag("skill")),
                CompletionKind::Model(_) => item = item.trailing(tag("model")),
                CompletionKind::Command | CompletionKind::Path { .. } => {}
            }
            item
        })
        .collect();
    let active_key = key.clone();
    let choose_key = key.clone();
    PopupList::new("completion", anchor)
        .anchored(anchor, Side::Top, Align::Start, 4.0)
        .label("Completions")
        .width(width)
        .max_height(MAX_COMPLETIONS as f32 * ROW)
        .items(items)
        .active(Some(composer.completion_ix))
        .on_active(move |s: &mut J3, ix| s.composer(&active_key).completion_ix = ix)
        .on_choose(move |s: &mut J3, id, tx| s.choose_completion(&choose_key, id, tx))
        .build(k, ui.layer);
}
