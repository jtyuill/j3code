//! The side-question card above the composer: omp's `/btw`. A draft that starts with
//! `/btw` is not sent to the thread; omp answers it beside whatever the thread is doing
//! (`btw.ask`) and the conversation keeps no trace of it. The card shows the question and
//! the answer as it streams. Another `/btw` while the card shows a finished answer follows
//! up in the same topic. Closing the card (its button, or Escape in the composer) cancels a
//! question still being answered.

use j3_client::{
    Error, apply_btw,
    methods::{BtwAskParams, BtwItem, BtwRecord, BtwStatus},
};
use rune::{El, Tx, col, icon, row, spacer, text};
use rune_kit::{ControlSize, IconButton, IconButtonVariant, Spinner};

use super::cards::card;
use crate::{
    app::{J3, Ui},
    icons,
    sidebar::ThreadKey,
    theme::Status,
};

/// The capability of a server that answers `btw.ask`.
pub const CAPABILITY: &str = "btw";

/// One thread's side question: the topic, and the ask still streaming into it.
pub struct BtwCard {
    /// The question on its way, shown until the topic carries it.
    asked: Option<String>,
    topic: Option<BtwRecord>,
    /// Why the last question could not be answered.
    error: Option<String>,
    /// The task reading the ask's stream; aborting it cancels the question.
    run: Option<tokio::task::AbortHandle>,
    /// Counts asks, so a late item of an earlier one is dropped.
    generation: u64,
}

impl Drop for BtwCard {
    fn drop(&mut self) {
        if let Some(run) = &self.run {
            run.abort();
        }
    }
}

impl BtwCard {
    /// The topic a new question follows up on: the one shown, once its last answer is
    /// complete.
    fn follow_up(&self) -> Option<String> {
        let topic = self.topic.as_ref()?;
        let complete = topic.turns.last()?.status == BtwStatus::Complete;
        (complete && self.error.is_none()).then(|| topic.id.clone())
    }
}

/// The question of a `/btw` draft: the text after the command. None for any other draft.
pub fn question(draft: &str) -> Option<&str> {
    let rest = draft.strip_prefix("/btw")?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| rest.trim())
}

impl J3 {
    /// Asks omp `question` beside `key`'s thread, following up on the topic the card shows.
    pub fn ask_btw(&mut self, key: &ThreadKey, question: String) {
        let Some(connection) = self.thread_connection(key) else {
            return;
        };
        let runtime = self.services.runtime.clone();
        let inbox = self.inbox.clone();
        let Some(thread) = self.threads.get_mut(key) else {
            return;
        };
        let record_id = thread.chrome.btw.as_ref().and_then(BtwCard::follow_up);
        let previous = thread.chrome.btw.take();
        let generation = previous.as_ref().map_or(0, |card| card.generation + 1);
        // A follow-up keeps the turns already answered on screen.
        let topic = record_id
            .is_some()
            .then(|| previous.and_then(|mut card| card.topic.take()))
            .flatten();
        let mut run = connection.btw_ask(BtwAskParams {
            thread_id: key.thread_id.clone(),
            question: question.clone(),
            record_id,
        });
        let forwarded = key.clone();
        let task = runtime.spawn(async move {
            loop {
                let next = run.next().await;
                let done = !matches!(next, Some(Ok(_)));
                let key = forwarded.clone();
                inbox.post(move |s: &mut J3, tx: &mut Tx| {
                    s.btw_item(&key, generation, next);
                    tx.instant();
                });
                if done {
                    return;
                }
            }
        });
        thread.chrome.btw = Some(BtwCard {
            asked: Some(question),
            topic,
            error: None,
            run: Some(task.abort_handle()),
            generation,
        });
    }

    /// One item of ask `generation`'s stream (`None`: it ended).
    fn btw_item(&mut self, key: &ThreadKey, generation: u64, next: Option<Result<BtwItem, Error>>) {
        let Some(card) = self
            .threads
            .get_mut(key)
            .and_then(|thread| thread.chrome.btw.as_mut())
            .filter(|card| card.generation == generation)
        else {
            return;
        };
        match next {
            Some(Ok(item)) => {
                apply_btw(&mut card.topic, item);
                card.asked = None;
            }
            Some(Err(error)) => {
                card.error = Some(error.to_string());
                card.run = None;
            }
            None => card.run = None,
        }
    }

    /// Closes `key`'s side-question card, cancelling a question still being answered. False
    /// when there is none.
    pub fn close_btw(&mut self, key: &ThreadKey) -> bool {
        self.threads
            .get_mut(key)
            .and_then(|thread| thread.chrome.btw.take())
            .is_some()
    }
}

/// The card: each question of the topic over its answer, then the question on its way.
pub fn view(s: &J3, ui: &Ui, key: &ThreadKey, btw: &BtwCard) -> El {
    let k = ui.k;
    let status = Status::of(k);
    let running = btw.run.is_some();
    let close_key = key.clone();
    let mut header = row()
        .key("btw-header")
        .w_full()
        .gap(8.0)
        .center_y()
        .child(icon(&icons::MESSAGE_CIRCLE_QUESTION_MARK, 12.0).color(k.c.icon_muted))
        .child(
            text("Side question")
                .font(k.ty.text_xsmall_medium)
                .color(k.c.text_muted),
        );
    if running {
        header = header.child(Spinner::new().size(12.0).build(k).key("btw-running"));
    }
    header = header.child(spacer()).child(
        IconButton::new(&icons::X)
            .variant(IconButtonVariant::Ghost)
            .control_size(ControlSize::Small)
            .label("Close side question")
            .help(format!("Close ({})", rune::Keybind::new("escape").label()))
            .on_press(move |s: &mut J3| {
                s.close_btw(&close_key);
            })
            .build(k)
            .key("btw-close"),
    );
    let asked = |index: usize, question: &str| {
        text(question)
            .key(format!("btw-question-{index}"))
            .label(question)
            .font(k.ty.text_small_medium)
            .color(k.c.text_base)
            .wrap()
    };
    let note = |index: usize, message: &str, color| {
        text(message)
            .key(format!("btw-note-{index}"))
            .font(k.ty.text_xsmall)
            .color(color)
            .wrap()
    };
    let mut body = card(k, "btw").child(header);
    let turns = btw.topic.as_ref().map_or(&[][..], |topic| &topic.turns);
    for (index, turn) in turns.iter().enumerate() {
        body = body.child(asked(index, &turn.question));
        if !turn.answer.is_empty() {
            body = body.child(
                col()
                    .key(format!("btw-answer-{index}"))
                    .label(&turn.answer)
                    .w_full()
                    .child(crate::hub::view::markdown(
                        s,
                        ui,
                        key,
                        &format!("btw-{index}"),
                        &turn.answer,
                    )),
            );
        }
        match turn.status {
            BtwStatus::Error => {
                let message = turn.error.as_deref().unwrap_or("omp could not answer.");
                body = body.child(note(index, message, status.danger));
            }
            BtwStatus::Cancelled => body = body.child(note(index, "Cancelled", k.c.text_muted)),
            BtwStatus::Running | BtwStatus::Complete => {}
        }
    }
    if let Some(question) = &btw.asked {
        body = body.child(asked(turns.len(), question));
    }
    if let Some(error) = &btw.error {
        body = body.child(note(turns.len(), error, status.danger));
    }
    body
}

#[cfg(test)]
mod tests {
    use super::question;

    #[test]
    fn only_a_btw_draft_is_a_side_question() {
        assert_eq!(question("/btw why is it slow?"), Some("why is it slow?"));
        assert_eq!(question("/btw\n  two\nlines "), Some("two\nlines"));
        assert_eq!(question("/btw"), Some(""));
        assert_eq!(question("/btwx why"), None);
        assert_eq!(question("so /btw why"), None);
        assert_eq!(question("/compact"), None);
    }
}
