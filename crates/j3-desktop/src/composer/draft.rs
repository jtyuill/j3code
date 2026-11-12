//! The draft's editing rules: `^model` mentions as chips (spec §7.7), magic keywords painted
//! with their gradient (§7.8), omp's word prediction as ghost text (§7.9), and the keys the
//! composer takes before the editor (the completion menu, the ghost, ←← for the agents hub).

use std::ops::Range;

use rune::{Key, KeyEvent, Mods, Span, TextEdit, Tx};

use super::{ComposerKey, ComposerState};
use crate::{
    app::J3,
    hub, keywords, mention,
    models::ModelChoice,
    prediction::{self, Follow, Ghost, Provisional},
};

/// The id of the chip a `^slug` mention becomes.
pub(super) fn mention_id(slug: &str) -> String {
    format!("model:{slug}")
}

/// The draft as it is sent: each model chip as the `^slug` it stands for.
pub fn plain_text(edit: &TextEdit) -> String {
    let mut out = String::with_capacity(edit.text.len());
    let mut at = 0;
    for (range, id) in edit.pills() {
        let Some(slug) = id.strip_prefix("model:") else {
            continue;
        };
        out.push_str(&edit.text[at..range.start]);
        out.push('^');
        out.push_str(slug);
        at = range.end;
    }
    out.push_str(&edit.text[at..]);
    out
}

/// The byte ranges of the draft's chips.
fn pill_ranges(edit: &TextEdit) -> Vec<Range<usize>> {
    edit.pills().map(|(range, _)| range).collect()
}

fn overlaps(taken: &[Range<usize>], range: &Range<usize>) -> bool {
    taken
        .iter()
        .any(|t| t.start < range.end && range.start < t.end)
}

/// The magic keywords drawn with their gradient: standalone prose occurrences that a
/// character follows (so `orchestrate` doesn't flicker while it is the last word), outside
/// the chips.
pub(crate) fn keyword_tokens(edit: &TextEdit) -> Vec<j3_text::keywords::Found> {
    let text = &edit.text;
    let pills = pill_ranges(edit);
    j3_text::keywords::find(text)
        .into_iter()
        .filter(|found| found.range.end < text.len() && !overlaps(&pills, &found.range))
        .collect()
}

/// Whether a keyword token shows (the shimmer runs only then).
pub(super) fn shows_keyword(edit: &TextEdit) -> bool {
    !keyword_tokens(edit).is_empty()
}

/// One colour span per character of each keyword token, at shimmer `phase`.
pub(super) fn keyword_spans(edit: &TextEdit, phase: f32) -> Vec<Span> {
    let mut spans = Vec::new();
    for found in keyword_tokens(edit) {
        let word = &edit.text[found.range.clone()];
        let colors = keywords::colors(found.keyword, phase);
        for ((offset, ch), color) in word.char_indices().zip(colors) {
            let start = found.range.start + offset;
            spans.push(Span::new(start, start + ch.len_utf8()).color(color));
        }
    }
    spans
}

/// Where byte `at` lands after `range` was replaced by `len` bytes.
fn shifted(at: usize, range: &Range<usize>, len: usize) -> usize {
    if at <= range.start {
        at
    } else if at >= range.end {
        at - range.len() + len
    } else {
        range.start + len
    }
}

/// Replaces `range` of the draft with `with` (as a chip `(id, label)` when given), keeping
/// the selection where it was.
pub(super) fn splice(edit: &mut TextEdit, range: Range<usize>, with: &str, pill: Option<&str>) {
    let (anchor, focus) = (edit.anchor, edit.focus);
    match pill {
        Some(id) => edit.replace_with_pill(range.clone(), id, with),
        None => {
            // An unwrapped chip leaves its mark behind; drop it first.
            let marks = edit
                .marks()
                .iter()
                .filter(|m| {
                    !(m.mark.pill_id().is_some() && m.start >= range.start && m.end <= range.end)
                })
                .cloned()
                .collect();
            edit.set_marks(marks);
            edit.set_selection(range.start, range.end);
            edit.insert(with);
        }
    }
    edit.set_selection(
        shifted(anchor, &range, with.len()),
        shifted(focus, &range, with.len()),
    );
}

/// Keeps the chips in step with the text: a `^selector` of a listed model that something
/// follows becomes a chip; chips go back to their `^slug` text once mentions aren't allowed
/// (`!` and `$` drafts). Nothing changes while an IME composition is open. True when the
/// draft changed.
pub(super) fn sync_mentions(edit: &mut TextEdit, choices: &[ModelChoice]) -> bool {
    if edit.preedit().is_some() {
        return false;
    }
    if !j3_text::mention::allowed(&edit.text) {
        let chips: Vec<(Range<usize>, String)> = edit
            .pills()
            .filter_map(|(range, id)| Some((range, format!("^{}", id.strip_prefix("model:")?))))
            .collect();
        for (range, text) in chips.iter().rev() {
            splice(edit, range.clone(), text, None);
        }
        return !chips.is_empty();
    }
    let taken = pill_ranges(edit);
    let wrap: Vec<(Range<usize>, String, String)> = j3_text::mention::mentions(&edit.text)
        .into_iter()
        .filter(|m| m.range.end < edit.text.len() && !overlaps(&taken, &m.range))
        .filter_map(|m| {
            let choice = mention::find_choice(choices, m.selector)?;
            Some((
                m.range,
                mention_id(m.selector),
                choice.label.text().to_owned(),
            ))
        })
        .collect();
    for (range, id, label) in wrap.iter().rev() {
        splice(edit, range.clone(), label, Some(id));
    }
    !wrap.is_empty()
}

/// The ghost to paint: one that still follows the draft and caret, with nothing else
/// claiming the spot.
pub(super) fn shown_ghost(composer: &ComposerState) -> Option<&Ghost> {
    let ghost = composer.ghost.as_ref()?;
    let edit = &composer.edit;
    let (a, b) = edit.range();
    (composer.dictation.is_none()
        && composer.completions.is_empty()
        && a == b
        && ghost.matches(&edit.text, edit.focus))
    .then_some(ghost)
}

impl J3 {
    /// After an edit of `key`'s draft: settles a provisional space, follows the ghost,
    /// keeps the chips in step, refreshes the completion menu and asks for a prediction.
    /// Drafting a new thread closes the popover picking where it starts.
    pub(super) fn composer_changed(&mut self, key: &ComposerKey, tx: &mut Tx) {
        if matches!(key, ComposerKey::NewThread { .. }) {
            self.close_new_thread_picker();
        }
        let choices = self
            .envs
            .get(key.env())
            .map(|env| env.models().clone())
            .unwrap_or_default();
        let composer = self.composer(key);
        composer.error = None;
        if let Some(provisional) = composer.provisional.take() {
            let (text, cursor) = (&composer.edit.text, composer.edit.focus);
            if provisional.text == *text && provisional.cursor == cursor {
                // The accept's own edit.
                composer.provisional = Some(provisional);
            } else if let Some((range, with)) = provisional.settle(text, cursor) {
                let end = range.start + with.len();
                composer.edit.set_selection(range.start, range.end);
                composer.edit.insert(&with);
                composer.edit.set_caret(end);
            }
        }
        // A ghost still showing is omp's answer for the word at the caret: asking again
        // could only race it, and a `None` would drop the rest being typed through.
        let mut showing = false;
        if let Some(ghost) = composer.ghost.take() {
            match ghost.follow(&composer.edit.text, composer.edit.focus) {
                Follow::Unchanged => {
                    showing = true;
                    composer.ghost = Some(ghost);
                }
                Follow::TypedThrough(rest) => {
                    showing = rest.is_some();
                    composer.ghost = rest;
                }
                Follow::Diverged => {
                    let at = prediction::utf16_offset(&ghost.text, ghost.cursor);
                    self.predict_word_feedback(ghost.text, at, ghost.suffix, false);
                }
                Follow::Unrelated => {}
            }
        }
        let composer = self.composer(key);
        sync_mentions(&mut composer.edit, &choices);
        self.refresh_completions(key);
        tx.instant();
        if !showing {
            self.request_prediction(key);
        }
    }

    /// Asks omp for the rest of the word at the caret. omp predicts only at the end of a
    /// line, never into a selection, a completion menu, an `@` reference or a dictation; only
    /// an answer for the draft and caret as they still are shows.
    fn request_prediction(&mut self, key: &ComposerKey) {
        let composer = self.composer(key);
        let edit = &composer.edit;
        let (a, b) = edit.range();
        let wanted = composer.dictation.is_none()
            && composer.completions.is_empty()
            && composer.path_query.is_none()
            && !edit.text.trim().is_empty()
            && prediction::at_line_end(&edit.text, edit.focus)
            && a == b;
        if !wanted {
            composer.ghost = None;
            return;
        }
        let (text, cursor) = (edit.text.clone(), edit.focus);
        composer.predicting += 1;
        let key = key.clone();
        let at = prediction::utf16_offset(&text, cursor);
        self.predict_word(text.clone(), at, move |s, suffix, tx| {
            if let Some(composer) = s.composers.get_mut(&key) {
                composer.predicting -= 1;
                s.prediction_answered(&key, text, cursor, suffix);
                tx.instant();
            }
        });
    }

    /// omp's answer `suffix` for `key`'s draft `text` with the caret at byte `cursor`:
    /// shown when the draft and caret are still those.
    pub fn prediction_answered(
        &mut self,
        key: &ComposerKey,
        text: String,
        cursor: usize,
        suffix: Option<String>,
    ) {
        let Some(composer) = self.composers.get_mut(key) else {
            return;
        };
        if composer.edit.focus != cursor || composer.edit.text != text {
            return;
        }
        composer.ghost = suffix.map(|suffix| Ghost {
            text,
            cursor,
            suffix,
        });
    }

    /// Types the shown ghost: Tab with a provisional space after it, → without one. False
    /// when none shows.
    fn accept_ghost(&mut self, key: &ComposerKey, space: bool) -> bool {
        let composer = self.composer(key);
        let Some(ghost) = shown_ghost(composer).cloned() else {
            return false;
        };
        composer.ghost = None;
        let insert = if space {
            format!("{} ", ghost.suffix)
        } else {
            ghost.suffix.clone()
        };
        composer.edit.insert(&insert);
        composer.provisional = space.then(|| Provisional {
            text: composer.edit.text.clone(),
            cursor: composer.edit.focus,
        });
        let at = prediction::utf16_offset(&ghost.text, ghost.cursor);
        self.predict_word_feedback(ghost.text, at, ghost.suffix, true);
        true
    }

    /// The keys `key`'s field hears before its editor: ←← in the empty draft opens the
    /// agents hub; while the completion menu is open ↑ ↓ move, Tab and Enter accept and
    /// Escape closes; while a ghost shows Tab and → take it; Escape otherwise closes the
    /// thread's side-question card. True when taken.
    pub(super) fn composer_key(&mut self, key: &ComposerKey, ev: &KeyEvent, tx: &mut Tx) -> bool {
        if matches!(ev.key, Key::Modifier { .. }) {
            return false;
        }
        let now = hub::tap_time(tx, self.clock.t);
        let composer = self.composer(key);
        let plain = ev.mods == Mods::default();
        let left =
            ev.key == Key::Left && plain && !tx.key_repeat() && composer.edit.text.is_empty();
        if !left {
            composer.left.reset();
        } else if composer.left.tap(now) {
            if let ComposerKey::Thread(thread) = key {
                self.open_hub(thread.clone(), tx);
            }
            return true;
        }
        let composer = self.composer(key);
        if !composer.completions.is_empty() {
            let len = composer.completions.len();
            match ev.key {
                Key::Down if plain => composer.completion_ix = (composer.completion_ix + 1) % len,
                Key::Up if plain => {
                    composer.completion_ix = (composer.completion_ix + len - 1) % len
                }
                Key::Tab if plain => self.accept_completion(key, tx),
                Key::Enter if !ev.mods.shift => self.accept_completion(key, tx),
                Key::Escape => composer.close_completions(),
                _ => return false,
            }
            tx.instant();
            return true;
        }
        let taken = match ev.key {
            Key::Tab if plain => self.accept_ghost(key, true),
            Key::Right if plain => self.accept_ghost(key, false),
            Key::Escape if plain => match key {
                ComposerKey::Thread(thread) => self.close_btw(thread),
                ComposerKey::NewThread { .. } => false,
            },
            _ => false,
        };
        if taken {
            tx.instant();
        }
        taken
    }
}
