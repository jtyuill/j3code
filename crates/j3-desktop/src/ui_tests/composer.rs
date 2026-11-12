//! The composer against the real server: launching a new thread, `^model` chips, `@path`
//! completion, file attachments, `/btw` side questions, magic keywords, omp's word prediction
//! and dictation.

use std::time::Duration;

use rune::{ClipboardContent, Key};
use serde_json::json;

use super::{
    harness::Harness,
    omp::{MODEL, btw_record},
};
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

    // Streaming text reaches the feed as it arrives, an unfinished paragraph and word too.
    app.script.emit(json!({"type": "agent_start"}));
    app.until("the row says Working", |h| h.in_sidebar("Say hi, Working"));
    app.script
        .text("m1", json!({"type": "text_start", "contentIndex": 0}));
    app.script.text(
        "m1",
        json!({"type": "text_delta", "contentIndex": 0, "delta": "Hello there.\n\nAnd mo"}),
    );
    app.until("the reply shows as far as it has arrived", |h| {
        h.feed_shows("Hello there.") && h.feed_shows("And mo") && !h.feed_shows("And more.")
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

#[test]
fn a_pasted_file_is_uploaded_and_reaches_omp_as_a_mention() {
    let mut app = Harness::start("file-attachment");
    app.focused_new_thread_composer();
    let file = app.data_dir.join("notes.txt");
    std::fs::write(&file, "remember the milk").unwrap();

    // Copied in the file manager, pasted into the field.
    app.rt_mut()
        .paste_content(ClipboardContent::new("").with_files([file]));
    app.until("the file is a chip", |h| {
        h.label_of("composer-attachment-0").as_deref() == Some("notes.txt")
    });
    app.typed("Read this");
    let key = app.composer_key();
    app.until("the upload is done", |h| {
        matches!(
            h.state().composers[&key].attachments[0].upload,
            crate::composer::Upload::Done(_)
        )
    });
    app.press(Key::Enter);

    // omp gets no image: the server names the uploaded file after the text, and omp reads it.
    app.until("omp is prompted", |h| h.script.count("prompt:") == 1);
    let calls = app.script.calls();
    let prompt = calls.iter().find(|c| c.starts_with("prompt:")).unwrap();
    let path = prompt
        .strip_prefix("prompt:Read this\n\n@\"")
        .and_then(|rest| rest.strip_suffix("/notes.txt\":0"))
        .unwrap_or_else(|| panic!("no mention in {prompt:?}"));
    assert_eq!(
        std::fs::read_to_string(format!("{path}/notes.txt")).unwrap(),
        "remember the milk"
    );
    // The message in the feed is the text as typed.
    app.until("the thread opens", |h| h.feed_row("Read this").is_some());
}

#[test]
fn a_btw_draft_asks_a_side_question_beside_the_run() {
    let mut app = Harness::start("btw");
    let project_id = app.project();
    app.until_models();
    app.open_running_thread(&project_id, "Work");
    app.click_composer();

    // The server lists `/btw` among omp's commands.
    app.typed("/bt");
    app.until("the menu offers /btw", |h| {
        h.exists("completion-option-/btw")
    });
    app.press(Key::Enter);
    assert_eq!(app.draft(), "/btw ");
    // Without a question nothing is asked.
    app.press(Key::Enter);
    assert_eq!(app.draft(), "/btw ");
    app.typed("why?");
    app.press(Key::Enter);
    app.until_called("btw:why?:");
    assert_eq!(app.draft(), "");
    app.until("the card shows the question", |h| {
        h.label_of("btw-question-0").as_deref() == Some("why?") && h.exists("btw-running")
    });

    // The answer streams into the card.
    app.script
        .emit(json!({"type": "btw_delta", "recordId": "btw-1", "delta": "Because."}));
    app.until("the answer shows", |h| {
        h.label_of("btw-answer-0").as_deref() == Some("Because.")
    });
    app.script
        .emit(json!({"type": "btw_record", "record": btw_record("why?", "Because.", "complete")}));
    app.until("the answer is complete", |h| !h.exists("btw-running"));

    // Another `/btw` follows up in the topic; Escape closes the card and cancels it.
    app.typed("/btw and then?");
    app.press(Key::Enter);
    app.until_called("btw:and then?:btw-1");
    app.until("the follow-up shows", |h| {
        h.has_label("and then?") && h.exists("btw-running")
    });
    app.press(Key::Escape);
    app.until_called("btw_cancel:btw-1");
    assert!(!app.exists("btw-header"));

    // The thread never heard of it: the run goes on and nothing was queued.
    assert_eq!(app.script.count("prompt:"), 1);
    assert!(app.stoppable());
}
