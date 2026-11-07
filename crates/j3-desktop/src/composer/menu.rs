//! The completion menu (spec §7.6): `/` commands and skills at the start of the draft, `^`
//! models anywhere a mention may go, in a rune-kit `PopupList` above the field. The field
//! keeps focus; ↑ ↓ Tab Enter Escape reach it through `draft::composer_key`.

use rune::{Align, Side, Tx, text};
use rune_kit::{PopupItem, PopupList};

use super::{ComposerKey, draft};
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

impl J3 {
    /// Recomputes `key`'s menu from its draft and caret, selecting the first row.
    pub(super) fn refresh_completions(&mut self, key: &ComposerKey) {
        let composer = self.composer(key);
        let (text, cursor) = (composer.edit.text.clone(), composer.edit.focus);
        let (a, b) = composer.edit.range();
        let mut rows = if a != b {
            Vec::new()
        } else if let Some(query) = completion::slash_query(&text, cursor) {
            self.composer_catalog(key).complete(query)
        } else {
            self.model_completions(key, &text, cursor)
        };
        rows.truncate(KEPT);
        let composer = self.composer(key);
        composer.completions = rows;
        composer.completion_ix = 0;
    }

    /// The models a `^` mention being typed at `cursor` offers.
    fn model_completions(&self, key: &ComposerKey, text: &str, cursor: usize) -> Vec<Completion> {
        let Some((_, query)) = mention::query_at(text, cursor) else {
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
    /// end), a model replaces the `^query` with its chip and a space.
    pub(super) fn accept_completion(&mut self, key: &ComposerKey, tx: &mut Tx) {
        let composer = self.composer(key);
        let Some(row) = composer.completions.get(composer.completion_ix).cloned() else {
            return;
        };
        composer.completions.clear();
        composer.ghost = None;
        if let CompletionKind::Model(slug) = &row.kind {
            self.accept_model(key, slug);
            return;
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
        let Some((range, _)) = mention::query_at(&edit.text, edit.focus) else {
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

/// A row's id: the label for commands and skills, the slug for models.
fn row_id(row: &Completion) -> &str {
    match &row.kind {
        CompletionKind::Model(slug) => slug,
        _ => &row.label,
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
                CompletionKind::Command => {}
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
