//! The composer against the real server: launching a new thread, `^model` chips, `@path`
//! completion, magic keywords, omp's word prediction and dictation.

use std::time::Duration;

use rune::Key;
use serde_json::json;

use super::{harness::Harness, omp::MODEL};
use crate::composer::keyword_tokens;

impl Harness {
    /// The main composer's chips as `(id, label)`.
    fn chips(&self) -> Vec<(String, String)> {
        let composer = &self.state().composers[&self.composer_key()];
        let edit = &composer.edit;
        edit.pills()
            .map(|(range, id)| (id.to_owned(), edit.text[range].to_owned()))
            .collect()
    }

    /// The magic keywords the main composer paints as tokens. (Tokens are colour spans of
    /// the field, not elements with a `kw:` key and label as they were.)
    fn keyword_tokens(&self) -> Vec<String> {
        let edit = &self.state().composers[&self.composer_key()].edit;
        keyword_tokens(edit)
            .into_iter()
            .map(|found| edit.text[found.range].to_owned())
            .collect()
    }
}

#[test]
fn new_thread_launches_from_the_composer_and_streams_the_reply() {
    let mut app = Harness::start("launch");
    let project_id = app.project();
    // The composer needs the model catalog the fake omp serves.
    app.until_models();

    // The sidebar's new-thread button opens a new thread in the (only) project, with the
    // composer focused.
    app.click("new-thread");
    app.until("the composer is focused", |h| h.composer_focused());
    assert!(!app.stoppable());
    // An empty message is not sent.
    app.press(Key::Enter);
    app.typed("Say hi");
    assert_eq!(app.draft(), "Say hi");
    app.press(Key::Enter);
    app.until_called("prompt:Say hi:0");
    assert_eq!(app.script.count("prompt:"), 1);

    // The launched thread opens with the first message; the draft clears.
    app.until("the thread opens", |h| h.feed_row("Say hi").is_some());
    assert_eq!(app.draft(), "");

    // Streaming text reaches the feed a block at a time: a paragraph shows once it has ended,
    // the rest once the reply does.
    app.script.emit(json!({"type": "agent_start"}));
    app.until("the row says Working", |h| h.in_sidebar("Say hi, Working"));
    app.script
        .text("m1", json!({"type": "text_start", "contentIndex": 0}));
    app.script.text(
        "m1",
        json!({"type": "text_delta", "contentIndex": 0, "delta": "Hello there.\n\nAnd mo"}),
    );
    app.until("the first paragraph shows, the second waits", |h| {
        h.feed_shows("Hello there.") && !h.feed_shows("And mo")
    });
    app.script.text(
        "m1",
        json!({"type": "text_delta", "contentIndex": 0, "delta": "re."}),
    );
    app.script.text(
        "m1",
        json!({"type": "text_end", "contentIndex": 0, "content": "Hello there.\n\nAnd more."}),
    );
    app.until("the whole reply shows", |h| h.feed_shows("And more."));
    let reply = app.feed_row("Hello there.\n\nAnd more.").unwrap();
    assert!(reply.rect[1] > app.feed_row("Say hi").unwrap().rect[1]);

    app.script.settle("completed");
    app.until("the run ends", |h| h.in_sidebar("Say hi") && !h.stoppable());
    // The launch carried the composer's model (the one the fake omp lists).
    let state = app.connection.state();
    let state = state.borrow();
    let thread = state
        .shell
        .threads
        .values()
        .find(|thread| thread.project_id == project_id)
        .expect("the launched thread");
    assert_eq!(
        format!("{}/{}", thread.model.provider, thread.model.id),
        MODEL
    );
}

#[test]
fn a_caret_mention_completes_to_a_model_chip_and_sends_its_selector() {
    let mut app = Harness::start("mention");
    app.focused_new_thread_composer();
    let name = app
        .state()
        .envs
        .local()
        .unwrap()
        .models()
        .iter()
        .find(|choice| choice.slug() == MODEL)
        .map(|choice| choice.label.text().to_owned())
        .expect("the fake omp's model");
    let row = format!("completion-option-{MODEL}");

    app.typed("Have ^");
    app.until("the menu lists the model", |h| h.exists(&row));
    app.press(Key::Enter);
    // The chip is a pill of the draft (its label is the text it shows).
    app.until("the chip shows", |h| !h.chips().is_empty());
    assert_eq!(app.draft(), format!("Have ^{MODEL} "));
    assert_eq!(app.chips(), [(format!("model:{MODEL}"), name)]);
    assert!(!app.exists(&row));

    app.press(Key::Enter);
    app.until_called(&format!("prompt:Have ^{MODEL}:0"));
}

#[test]
fn an_at_reference_completes_paths_the_server_finds_and_sends_them_typed() {
    let mut app = Harness::start("at-path");
    for path in ["src/main.rs", "src/lib.rs", "notes.md"] {
        let path = app.project_dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }
    app.focused_new_thread_composer();

    // A directory is taken as `@dir/`, and its entries come next.
    app.typed("Read @sr");
    app.until("the menu lists the directory", |h| {
        h.exists("completion-option-src")
    });
    app.press(Key::Enter);
    assert_eq!(app.draft(), "Read @src/");
    app.until("its entries follow", |h| {
        h.exists("completion-option-src/main.rs") && h.exists("completion-option-src/lib.rs")
    });

    // Typing narrows them; a file is taken with a space after it.
    app.typed("ma");
    app.until("the menu narrows", |h| {
        !h.exists("completion-option-src/lib.rs")
    });
    app.press(Key::Enter);
    assert_eq!(app.draft(), "Read @src/main.rs ");
    assert!(!app.exists("completion-option-src/main.rs"));

    // omp reads the file itself: the reference goes as typed.
    app.typed("now");
    app.press(Key::Enter);
    app.until_called("prompt:Read @src/main.rs now:0");
}

#[test]
fn a_magic_keyword_becomes_a_token_once_a_delimiter_follows_and_stops_when_extended() {
    let mut app = Harness::start("keyword");
    app.focused_new_thread_composer();

    // At the end of the draft the word stays plain text.
    app.typed("please orchestrate");
    assert_eq!(app.draft(), "please orchestrate");
    assert!(app.keyword_tokens().is_empty());

    // Once a character follows it, the word is one token and the text is untouched.
    app.typed(" this");
    app.until("one token shows", |h| h.keyword_tokens().len() == 1);
    assert_eq!(app.keyword_tokens(), ["orchestrate"]);
    assert_eq!(app.draft(), "please orchestrate this");

    // Typing straight after the word makes it `orchestrated`: plain again.
    app.typed(" orchestrate ");
    app.until("two tokens show", |h| h.keyword_tokens().len() == 2);
    // Back over the space: the caret sits at the end of the word's token.
    app.press(Key::Left);
    app.typed("d");
    app.until("the word is extended", |h| {
        h.draft() == "please orchestrate this orchestrated "
    });
    app.until("one token is left", |h| h.keyword_tokens().len() == 1);
    assert_eq!(app.draft(), "please orchestrate this orchestrated ");
}

#[test]
fn the_composer_offers_omps_word_prediction_as_ghost_text() {
    let mut app = Harness::start("predict");
    app.focused_new_thread_composer();
    let log = app.data_dir.join("predict-feedback.log");
    let feedback = move || std::fs::read_to_string(&log).unwrap_or_default();
    let ghost = |h: &Harness| h.label_of("composer-ghost");

    // omp's suggestion follows the caret; Tab takes it with a space.
    app.typed("please refac");
    app.until("the ghost shows", |h| ghost(h).as_deref() == Some("tor"));
    app.press(Key::Tab);
    app.until("Tab takes it", |h| h.draft() == "please refactor ");
    assert_eq!(ghost(&app), None);
    let log = feedback.clone();
    app.until("omp hears it was taken", move |_| {
        log().contains(r#""accepted":true"#)
    });
    assert!(feedback().contains(r#""suggestion":"tor""#));

    // That space is provisional: punctuation takes its place.
    app.typed(",");
    app.until("the comma replaces the space", |h| {
        h.draft() == "please refactor,"
    });

    // Typing through the suggestion keeps the rest; → takes it without a space.
    app.typed(" refac");
    app.until("the ghost shows", |h| ghost(h).as_deref() == Some("tor"));
    app.typed("t");
    assert_eq!(ghost(&app).as_deref(), Some("or"));
    app.press(Key::Right);
    app.until("→ takes it", |h| h.draft() == "please refactor, refactor");

    // Typing past it rejects it.
    app.typed(" refac");
    app.until("the ghost shows", |h| ghost(h).as_deref() == Some("tor"));
    app.typed("e");
    assert_eq!(ghost(&app), None);
    let log = feedback.clone();
    app.until("omp hears it was rejected", move |_| {
        log().contains(r#""accepted":false"#)
    });
}

#[test]
fn the_microphone_dictates_into_the_draft() {
    let mut app = Harness::start("dictate");
    app.focused_new_thread_composer();

    // The transcript lands at the caret, a space after the draft's word, and is replaced
    // as it grows.
    assert_eq!(app.label_of("dictate").as_deref(), Some("Dictate"));
    app.typed("Note:");
    app.click("dictate");
    app.until("the partial transcript shows", |h| {
        h.label_of("dictate").as_deref() == Some("Stop dictation") && h.draft() == "Note: hello"
    });
    // Nothing is sent while the words are still coming.
    app.press(Key::Enter);

    app.click("dictate");
    app.until("the final transcript shows", |h| {
        h.label_of("dictate").as_deref() == Some("Dictate") && h.draft() == "Note: hello there"
    });
    assert_eq!(app.script.count("prompt:"), 0);

    // Then the draft sends as typed.
    app.press(Key::Enter);
    app.until_called("prompt:Note: hello there:0");
}

#[test]
fn a_bang_draft_shows_no_model_menu_and_no_model_chip() {
    let mut app = Harness::start("bang");
    app.focused_new_thread_composer();
    let text = format!("!ls ^{MODEL} ");

    app.typed(&text);
    app.until("the draft is typed", |h| h.draft() == text);
    app.pause(Duration::from_millis(300));
    assert_eq!(app.draft(), text);
    assert!(!app.exists(&format!("completion-option-{MODEL}")));
    assert!(app.chips().is_empty());
}
