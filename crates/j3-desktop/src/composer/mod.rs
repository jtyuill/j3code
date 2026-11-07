//! The composer (spec §7): the message draft of one thread or of the new-thread surface,
//! sending (or queueing behind the active run), and dictation transcripts landing in the
//! draft. The full composer (images, model picker, completions, tokens, prediction, the
//! usage button) builds on this state and these commands.

use j3_client::SendMode;
use rune::{Cx, El, TextEdit, Tx, col, text};
use rune_kit::TextArea;

use crate::{
    app::{J3, Main, Ui},
    sidebar::ThreadKey,
    speech::SpeechEvent,
};

/// Whose draft: an open thread's, or the new-thread surface's for one environment and
/// project (a composer is reused only for the same pair).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ComposerKey {
    Thread(ThreadKey),
    NewThread {
        env: String,
        project: Option<String>,
    },
}

impl ComposerKey {
    /// The element key of this composer's text area.
    pub fn input_key(&self) -> String {
        match self {
            Self::Thread(key) => key.element_id("composer-input"),
            Self::NewThread { env, project } => {
                format!("composer-input-new-{env}-{}", project.as_deref().unwrap_or(""))
            }
        }
    }
}

/// One composer's state.
#[derive(Default)]
pub struct ComposerState {
    pub edit: TextEdit,
    /// Why the last submit was refused or failed.
    pub error: Option<String>,
    /// A send is on its way.
    pub busy: bool,
}

impl J3 {
    /// The composer for `key`, created on first use.
    pub fn composer(&mut self, key: &ComposerKey) -> &mut ComposerState {
        self.composers.entry(key.clone()).or_default()
    }

    /// Moves keyboard focus to the main pane's composer.
    pub fn focus_composer(&mut self, _: &mut Tx) {
        let key = match &self.route.main {
            Main::Thread(thread) => ComposerKey::Thread(thread.clone()),
            Main::NewThread => self.new_thread.composer_key(),
            Main::Empty => return,
        };
        self.composer(&key).edit.focused = true;
    }

    /// Sends `key`'s draft: to its thread (queued behind an active run), or as the first
    /// message of a new thread.
    pub fn submit_composer(&mut self, key: &ComposerKey, tx: &mut Tx) {
        let composer = self.composer(key);
        let text = composer.edit.text.trim().to_owned();
        if text.is_empty() || composer.busy {
            return;
        }
        match key {
            ComposerKey::Thread(thread) => {
                let Some(connection) = self.envs.get(&thread.env).map(|e| e.connection().clone())
                else {
                    return;
                };
                let running = self
                    .threads
                    .get(thread)
                    .is_some_and(|t| t.state.active_run().is_some());
                let mode = if running { SendMode::Queue } else { SendMode::Start };
                let id = thread.thread_id.clone();
                let done = key.clone();
                self.composer(key).busy = true;
                self.spawn(async move {
                    let result = connection.send(&id, &text, Vec::new(), None, mode).await;
                    move |s: &mut J3, _: &mut Tx| {
                        let composer = s.composer(&done);
                        composer.busy = false;
                        match result {
                            Ok(_) => {
                                composer.edit.clear();
                                composer.error = None;
                            }
                            Err(e) => composer.error = Some(format!("Couldn’t send: {e}")),
                        }
                    }
                });
            }
            ComposerKey::NewThread { .. } => self.launch_new_thread(text, tx),
        }
    }

    /// Dictation events for the composer that owns the capture: transcripts replace the
    /// draft's dictated text; a failure shows under the draft.
    pub fn on_speech_event(&mut self, event: SpeechEvent, _: &mut Tx) {
        match event {
            SpeechEvent::Phase { .. } => {}
            SpeechEvent::Transcript { owner, text } => {
                self.composer(&owner).edit.replace_text(text);
            }
            SpeechEvent::End { owner, text, error } => {
                let composer = self.composer(&owner);
                if !text.is_empty() {
                    composer.edit.replace_text(text);
                }
                composer.error = error.map(|e| format!("Dictation failed: {e}"));
            }
        }
    }
}

/// The composer of `key`: the draft and its error line.
pub fn view(s: &J3, _cx: &mut Cx<J3>, ui: &mut Ui, key: &ComposerKey) -> El {
    let k = ui.k;
    let blank = TextEdit::new("");
    let state = s.composers.get(key);
    let edit = state.map_or(&blank, |c| &c.edit);
    let running = match key {
        ComposerKey::Thread(thread) => s
            .threads
            .get(thread)
            .is_some_and(|t| t.state.active_run().is_some()),
        ComposerKey::NewThread { .. } => false,
    };
    let edit_key = key.clone();
    let submit_key = key.clone();
    let input = TextArea::new(edit)
        .placeholder(if running { "Queue a message…" } else { "Message omp…" })
        .label("Message")
        .auto_grow(1, 12)
        .on_edit(move |s: &mut J3| &mut s.composer(&edit_key).edit)
        .on_submit(move |s: &mut J3, tx: &mut Tx| s.submit_composer(&submit_key, tx))
        .build(k)
        .key(key.input_key());
    let mut column = col().key("composer").w_full().gap(4.0).child(input);
    if let Some(error) = state.and_then(|c| c.error.as_deref()) {
        column = column.child(
            text(error)
                .key("composer-error")
                .font(k.ty.text_xsmall)
                .color(crate::theme::Status::of(k).danger),
        );
    }
    column
}
