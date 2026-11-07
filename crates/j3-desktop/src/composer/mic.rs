//! Dictation into the draft (spec §7.10): the microphone button left of Send, and the
//! transcripts of this composer's capture landing at the caret it started from.

use rune::{El, Tx, icon, row};
use rune_kit::Spinner;

use super::ComposerKey;
use crate::{
    app::{J3, Ui},
    icons,
    speech::{Phase, SpeechEvent},
    theme::Status,
};

/// A capture dictating into the draft, and where its words are.
pub(super) struct Dictation {
    pub phase: Phase,
    /// Byte offset of the transcript in the draft: the caret when the capture started.
    start: usize,
    /// The transcript as it stands in the draft, its separating space included.
    inserted: String,
    /// The draft has a word right before `start`, so the transcript starts with a space.
    separate: bool,
    /// Cleared once the user edits the transcript's text; later updates then leave the
    /// draft alone.
    tracking: bool,
}

impl Dictation {
    /// A capture starting at `edit`'s caret.
    pub(super) fn at(edit: &rune::TextEdit) -> Self {
        let start = edit.focus.min(edit.text.len());
        let before = edit
            .text
            .get(..start)
            .and_then(|head| head.chars().next_back());
        Self {
            phase: Phase::Starting,
            start,
            inserted: String::new(),
            separate: before.is_some_and(|c| !c.is_whitespace()),
            tracking: true,
        }
    }
}

impl J3 {
    /// The microphone button: start dictating at the caret, cancel a capture still
    /// starting, stop one that records; nothing while the last words are transcribed.
    fn toggle_dictation(&mut self, key: &ComposerKey, tx: &mut Tx) {
        match self.composer(key).dictation.as_ref().map(|d| d.phase) {
            Some(Phase::Transcribing) => {}
            // The button took the press; the keyboard goes back to the draft.
            Some(Phase::Starting) => {
                self.cancel_dictation(tx);
                self.composer(key).edit.focused = true;
            }
            Some(Phase::Recording) => {
                self.stop_dictation(tx);
                self.composer(key).edit.focused = true;
            }
            None => {
                self.start_dictation(key.clone(), tx);
                let started = self
                    .speech
                    .capture()
                    .is_some_and(|(owner, phase)| owner == key && phase == Phase::Starting);
                let composer = self.composer(key);
                if !started {
                    composer.error = Some("Another composer is dictating.".to_owned());
                    return;
                }
                let dictation = Dictation::at(&composer.edit);
                composer.ghost = None;
                composer.provisional = None;
                composer.error = None;
                composer.dictation = Some(dictation);
                // The button took the press; the words land at the caret.
                composer.edit.focused = true;
            }
        }
    }

    /// What the capture `owner` started reports: its phase, its transcript so far, its end
    /// ("Dictation failed: …", or "No speech detected." for a capture that heard nothing).
    pub fn on_speech_event(&mut self, event: SpeechEvent, _: &mut Tx) {
        let owner = match &event {
            SpeechEvent::Phase { owner, .. }
            | SpeechEvent::Transcript { owner, .. }
            | SpeechEvent::End { owner, .. } => owner.clone(),
        };
        let composer = self.composer(&owner);
        let Some(dictation) = composer.dictation.as_mut() else {
            return;
        };
        match event {
            SpeechEvent::Phase { phase, .. } => dictation.phase = phase,
            SpeechEvent::Transcript { text, .. } => write_transcript(composer, &text),
            SpeechEvent::End { text, error, .. } => {
                if !text.trim().is_empty() {
                    write_transcript(composer, &text);
                }
                // A start cancelled before the microphone opened heard nothing, and says
                // nothing.
                let silent = composer
                    .dictation
                    .take()
                    .is_some_and(|d| d.phase != Phase::Starting && d.inserted.is_empty());
                composer.error = match error {
                    Some(error) => Some(format!("Dictation failed: {error}")),
                    None if silent => Some("No speech detected.".to_owned()),
                    None => None,
                };
            }
        }
    }
}

/// Puts the capture's transcript so far in place of the previous one. Once the user has
/// edited that text, the draft is theirs: later transcripts are dropped.
fn write_transcript(composer: &mut super::ComposerState, text: &str) {
    let Some(dictation) = composer.dictation.as_mut().filter(|d| d.tracking) else {
        return;
    };
    let edit = &mut composer.edit;
    let end = dictation.start + dictation.inserted.len();
    if edit.text.get(dictation.start..end) != Some(dictation.inserted.as_str()) {
        dictation.tracking = false;
        return;
    }
    let text = text.trim();
    let replacement = match text {
        "" => String::new(),
        _ if dictation.separate => format!(" {text}"),
        _ => text.to_owned(),
    };
    if replacement == dictation.inserted {
        return;
    }
    edit.set_selection(dictation.start, end);
    edit.insert(&replacement);
    dictation.inserted = replacement;
}

/// The microphone, pill-shaped: ghost Mic at rest (disabled while another composer
/// dictates), a spinner while the microphone opens, red while recording, loading while
/// the recording is transcribed. Its tooltip follows the speech model's download or load.
pub(super) fn view(s: &J3, ui: &mut Ui, key: &ComposerKey) -> El {
    let k = ui.k;
    let danger = Status::of(k).danger;
    let elsewhere = s.speech.capture().is_some_and(|(owner, _)| owner != key);
    let note = s.speech.model_note();
    let phase = s
        .composers
        .get(key)
        .and_then(|c| c.dictation.as_ref())
        .map(|d| d.phase);
    let mic = |color| icon(&icons::MIC, 14.0).color(color);
    let (label, help, content, fill) = match phase {
        None => (
            "Dictate",
            if elsewhere {
                "Another composer is dictating".to_owned()
            } else {
                "Dictate".to_owned()
            },
            mic(k.c.icon_muted),
            None,
        ),
        Some(Phase::Starting) => (
            "Cancel dictation",
            "Starting the microphone… (click to cancel)".to_owned(),
            Spinner::new().size(14.0).build(k),
            None,
        ),
        Some(Phase::Recording) => (
            "Stop dictation",
            match &note {
                Some(note) => format!("Stop dictation · {note}"),
                None => "Stop dictation".to_owned(),
            },
            mic(danger),
            Some(k.alpha(danger, 0.18)),
        ),
        Some(Phase::Transcribing) => (
            "Transcribing",
            match &note {
                Some(note) => format!("Transcribing · {note}…"),
                None => "Transcribing…".to_owned(),
            },
            Spinner::new().size(14.0).build(k),
            None,
        ),
    };
    let mut button = row()
        .key("dictate")
        .role("button")
        .label(label)
        .help(help)
        .size(28.0, 28.0)
        .center()
        .radius(999.0)
        .child(content);
    if let Some(fill) = fill {
        button = button.fill(fill);
    }
    let disabled = elsewhere && phase.is_none();
    if disabled {
        return button.disabled(true).opacity(0.5);
    }
    let press = key.clone();
    button
        .hover(k.c.hover)
        .on_press_tx(move |s: &mut J3, tx: &mut Tx| s.toggle_dictation(&press, tx))
}
