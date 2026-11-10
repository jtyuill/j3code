//! The composer headless, on the offline fixtures: what a user sees and sends.

use j3_client::{Status, entities::ThinkingLevel};
use rune::{ClipboardContent, ClipboardImage, Key, KeyEvent, Mods};

use super::{ComposerKey, draft, fixture::PNG, plain_text};
use crate::{
    app::{
        fixtures::{J3CODE_PROJECT, Scene, open_thread_key},
        testing::TestApp,
    },
    speech::{Phase, SpeechEvent},
};

const OPUS: &str = "anthropic/claude-opus-4-5";

fn new_thread_key() -> ComposerKey {
    ComposerKey::NewThread {
        env: crate::environment::LOCAL_KEY.to_owned(),
        project: Some(J3CODE_PROJECT.to_owned()),
    }
}

/// `scene` with the main composer focused.
fn focused(scene: Scene) -> (TestApp, ComposerKey) {
    let mut app = TestApp::with_scene(scene);
    app.frame();
    app.rt.update(|s, tx| s.focus_composer(tx));
    app.frame();
    let key = app.rt.state.main_composer_key().expect("a composer shows");
    (app, key)
}

fn press(app: &mut TestApp, key: Key, mods: Mods) {
    app.key(KeyEvent { key, mods });
}

fn typed(app: &mut TestApp, text: &str) {
    for c in text.chars() {
        let key = if c == '\n' {
            Key::Enter
        } else {
            Key::Text(c.to_string())
        };
        press(app, key, Mods::default());
    }
}

fn draft(app: &TestApp, key: &ComposerKey) -> String {
    plain_text(&app.rt.state.composers[key].edit)
}

fn label(app: &TestApp, key: &str) -> Option<String> {
    app.rt
        .elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some(key))
        .and_then(|e| e.label)
}

fn chips(app: &TestApp, key: &ComposerKey) -> Vec<(String, String)> {
    let edit = &app.rt.state.composers[key].edit;
    edit.pills()
        .map(|(range, id)| (id.to_owned(), edit.text[range].to_owned()))
        .collect()
}

#[test]
fn a_caret_mention_completes_to_a_model_chip_and_sends_its_selector() {
    let (mut app, key) = focused(Scene::NewThread);
    assert_eq!(key, new_thread_key());
    typed(&mut app, "Have ^");
    let row = format!("completion-option-{OPUS}");
    assert!(app.rt.find(&row).is_some(), "the menu lists the models");
    press(&mut app, Key::Enter, Mods::default());
    assert!(app.rt.find(&row).is_none(), "accepting closes the menu");
    assert_eq!(
        chips(&app, &key),
        [(format!("model:{OPUS}"), "Claude Opus 4.5".to_owned())]
    );
    assert_eq!(draft(&app, &key), format!("Have ^{OPUS} "));

    // A typed selector of a listed model becomes a chip once something follows it.
    typed(&mut app, "or ^anthropic/claude-haiku-4-5");
    assert_eq!(chips(&app, &key).len(), 1, "still typing: no chip");
    typed(&mut app, " please");
    assert_eq!(chips(&app, &key).len(), 2);
    assert_eq!(
        draft(&app, &key),
        format!("Have ^{OPUS} or ^anthropic/claude-haiku-4-5 please")
    );
}

#[test]
fn a_bang_draft_shows_no_model_menu_and_no_model_chip() {
    let (mut app, key) = focused(Scene::NewThread);
    let text = format!("!ls ^{OPUS} ");
    typed(&mut app, &text);
    assert!(app.rt.state.composers[&key].completions.is_empty());
    assert!(chips(&app, &key).is_empty());
    assert_eq!(draft(&app, &key), text);

    // A chip goes back to its selector once the draft becomes a `$` command.
    let (mut app, key) = focused(Scene::NewThread);
    typed(&mut app, &format!("^{OPUS} x"));
    assert_eq!(chips(&app, &key).len(), 1);
    press(&mut app, Key::Home, Mods::default());
    typed(&mut app, "$");
    assert!(chips(&app, &key).is_empty());
    assert_eq!(
        app.rt.state.composers[&key].edit.text,
        format!("$^{OPUS} x")
    );
}

#[test]
fn a_magic_keyword_becomes_a_token_once_a_character_follows_and_stops_when_extended() {
    let (mut app, key) = focused(Scene::NewThread);
    let tokens = |app: &TestApp| draft::keyword_tokens(&app.rt.state.composers[&key].edit).len();
    typed(&mut app, "please orchestrate");
    assert_eq!(tokens(&app), 0, "the last word stays plain");
    typed(&mut app, " this");
    assert_eq!(tokens(&app), 1);
    typed(&mut app, " orchestrate ");
    assert_eq!(tokens(&app), 2);
    press(&mut app, Key::Left, Mods::default());
    typed(&mut app, "d");
    assert_eq!(draft(&app, &key), "please orchestrate this orchestrated ");
    assert_eq!(tokens(&app), 1);
}

#[test]
fn the_keyword_shimmer_runs_only_while_focused_with_a_keyword() {
    let (mut app, key) = focused(Scene::NewThread);
    typed(&mut app, "plain words");
    app.frame();
    assert!(app.rt.state.composers[&key].shimmer_at.is_none());
    typed(&mut app, " ultrathink now");
    app.frame();
    assert!(app.rt.state.composers[&key].shimmer_at.is_some());
    app.rt
        .update(|s, _| s.composer(&new_thread_key()).edit.focused = false);
    app.frame();
    app.frame();
    assert!(
        app.rt.state.composers[&key].shimmer_at.is_none(),
        "no timer after blur"
    );
}

/// Frames until `key`'s prediction requests are answered, so a test's own answer is the
/// last one.
fn settle_predictions(app: &mut TestApp, key: &ComposerKey) {
    for _ in 0..3000 {
        app.frame();
        if app.rt.state.composers[key].predicting == 0 {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the prediction never answered");
}

/// omp suggests `suffix` for the draft as it is.
fn suggest(app: &mut TestApp, key: &ComposerKey, suffix: &str) {
    settle_predictions(app, key);
    let edit = &app.rt.state.composers[key].edit;
    let (text, cursor) = (edit.text.clone(), edit.focus);
    let key = key.clone();
    let suffix = suffix.to_owned();
    app.rt
        .update(move |s, _| s.prediction_answered(&key, text, cursor, Some(suffix)));
    app.frame();
}

#[test]
fn word_prediction_shows_as_ghost_text_and_tab_space_and_punctuation_follow_omp() {
    let (mut app, key) = focused(Scene::NewThread);
    typed(&mut app, "please refac");
    suggest(&mut app, &key, "tor");
    assert_eq!(label(&app, "composer-ghost").as_deref(), Some("tor"));

    // Tab takes it with a space…
    press(&mut app, Key::Tab, Mods::default());
    assert_eq!(draft(&app, &key), "please refactor ");
    assert_eq!(label(&app, "composer-ghost"), None);
    // …which punctuation replaces.
    typed(&mut app, ",");
    assert_eq!(draft(&app, &key), "please refactor,");

    // A typed space after a Tab accept is swallowed.
    typed(&mut app, " refac");
    suggest(&mut app, &key, "tor");
    press(&mut app, Key::Tab, Mods::default());
    typed(&mut app, " ");
    assert_eq!(draft(&app, &key), "please refactor, refactor ");

    // Typing through it keeps the rest; → takes it without a space.
    typed(&mut app, "refac");
    suggest(&mut app, &key, "tor");
    typed(&mut app, "t");
    // No answer arriving since takes the rest away.
    settle_predictions(&mut app, &key);
    assert_eq!(label(&app, "composer-ghost").as_deref(), Some("or"));
    press(&mut app, Key::Right, Mods::default());
    assert_eq!(draft(&app, &key), "please refactor, refactor refactor");

    // Typing past it drops it.
    typed(&mut app, " refac");
    suggest(&mut app, &key, "tor");
    typed(&mut app, "e");
    assert!(app.rt.state.composers[&key].ghost.is_none());
}

#[test]
fn enter_queues_while_running_and_the_button_follows_the_draft() {
    let (mut app, key) = focused(Scene::Thread);
    assert_eq!(key, ComposerKey::Thread(open_thread_key()));
    assert_eq!(label(&app, "send-message").as_deref(), Some("Stop"));
    typed(&mut app, "queue me");
    assert_eq!(label(&app, "send-message").as_deref(), Some("Queue"));
    // Shift+Enter breaks the line and sends nothing.
    press(
        &mut app,
        Key::Enter,
        Mods {
            shift: true,
            ..Mods::default()
        },
    );
    assert_eq!(app.rt.state.composers[&key].edit.text, "queue me\n");
    assert!(!app.rt.state.composers[&key].busy);
    // Enter sends (to the queue); offline, the send fails and the draft stays.
    press(&mut app, Key::Enter, Mods::default());
    assert!(app.rt.state.composers[&key].busy);
    for _ in 0..500 {
        app.frame();
        if !app.rt.state.composers[&key].busy {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let composer = &app.rt.state.composers[&key];
    assert!(composer.error.is_some());
    assert_eq!(composer.edit.text, "queue me\n");
}

#[test]
fn pasted_images_upload_at_once_and_block_sending_until_removed() {
    let (mut app, key) = focused(Scene::NewThread);
    let image = ClipboardImage::from_png(PNG.to_vec()).expect("a PNG");
    app.rt
        .paste_content(ClipboardContent::new("").with_image(image));
    app.frame();
    assert_eq!(
        label(&app, "composer-attachment-0").as_deref(),
        Some("pasted-1.png")
    );
    // Offline, the upload fails; the draft can't go until the image is removed.
    for _ in 0..500 {
        app.frame();
        if !matches!(
            app.rt.state.composers[&key].attachments[0].upload,
            super::attachments::Upload::Uploading
        ) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    typed(&mut app, "look");
    press(&mut app, Key::Enter, Mods::default());
    assert_eq!(
        app.rt.state.composers[&key].error.as_deref(),
        Some("Remove the attachments that failed to upload.")
    );
    let (x, y) = app.rt.find("remove-attachment-0").expect("a remove button");
    app.rt.pointer_down(x, y);
    app.rt.pointer_up(x, y);
    app.frame();
    assert!(app.rt.state.composers[&key].attachments.is_empty());

    let key2 = key.clone();
    app.rt
        .update(move |s, _| s.add_composer_image(&key2, b"%PDF-1.7".to_vec(), None));
    assert_eq!(
        app.rt.state.composers[&key].error.as_deref(),
        Some("Paste a PNG, JPEG, WebP or GIF image.")
    );
    let mut big = PNG.to_vec();
    big.resize(super::attachments::MAX_IMAGE_BYTES + 1, 0);
    let key2 = key.clone();
    app.rt
        .update(move |s, _| s.add_composer_image(&key2, big, None));
    assert_eq!(
        app.rt.state.composers[&key].error.as_deref(),
        Some("Images must be 10 MB or smaller.")
    );
}

#[test]
fn any_file_is_an_attachment_up_to_the_servers_limit() {
    let (mut app, key) = focused(Scene::NewThread);
    let key2 = key.clone();
    app.rt.update(move |s, _| {
        s.add_composer_file(&key2, b"%PDF-1.7".to_vec(), "paper.pdf".to_owned())
    });
    app.frame();
    assert_eq!(
        label(&app, "composer-attachment-0").as_deref(),
        Some("paper.pdf")
    );
    assert!(app.rt.state.composers[&key].error.is_none());

    let big = vec![0; super::attachments::MAX_FILE_BYTES as usize + 1];
    let key2 = key.clone();
    app.rt
        .update(move |s, _| s.add_composer_file(&key2, big, "big.bin".to_owned()));
    assert_eq!(
        app.rt.state.composers[&key].error.as_deref(),
        Some("Files must be 50 MB or smaller.")
    );
    assert_eq!(app.rt.state.composers[&key].attachments.len(), 1);
}

#[test]
fn the_model_picker_stars_without_choosing_and_sets_the_thinking_level() {
    let (mut app, key) = focused(Scene::NewThread);
    let tap = |app: &mut TestApp, element: &str| {
        let (x, y) = app
            .rt
            .find(element)
            .unwrap_or_else(|| panic!("{element} shows"));
        app.rt.pointer_down(x, y);
        app.rt.pointer_up(x, y);
        app.frame();
    };
    tap(&mut app, "model-picker");
    let sonnet = "anthropic/claude-sonnet-4-5";
    tap(&mut app, &format!("favorite-{sonnet}"));
    assert!(app.rt.state.prefs.ui().is_favorite(sonnet));
    assert!(app.rt.find("model-section-favorites").is_some());
    assert_eq!(
        app.rt
            .state
            .composer_choice(&key)
            .map(|c| c.slug().to_owned())
            .as_deref(),
        Some(OPUS),
        "starring does not choose"
    );
    tap(&mut app, "thinking-low");
    let selection = app.rt.state.composer_selection(&key).expect("a selection");
    assert_eq!(selection.thinking, Some(ThinkingLevel::Low));
    tap(&mut app, &format!("model-{sonnet}"));
    assert_eq!(
        app.rt
            .state
            .composer_choice(&key)
            .map(|c| c.slug().to_owned())
            .as_deref(),
        Some(sonnet)
    );
    assert!(
        app.rt.find("model-search").is_none(),
        "choosing closes the menu"
    );
}

#[test]
fn left_left_in_the_empty_draft_opens_the_agents_hub() {
    let (mut app, key) = focused(Scene::Thread);
    let ComposerKey::Thread(thread) = key else {
        unreachable!()
    };
    press(&mut app, Key::Left, Mods::default());
    for _ in 0..6 {
        app.frame();
    }
    press(&mut app, Key::Left, Mods::default());
    assert!(app.rt.state.hubs.get(&thread).is_some_and(|hub| hub.open));
}

#[test]
fn transcripts_land_at_the_caret_and_the_draft_sends_only_after_dictation() {
    let (mut app, key) = focused(Scene::NewThread);
    typed(&mut app, "Note:");
    let start = key.clone();
    app.rt.update(move |s, _| {
        let composer = s.composer(&start);
        composer.dictation = Some(super::mic::Dictation::at(&composer.edit));
    });
    let event = |app: &mut TestApp, event: SpeechEvent| {
        app.rt.update(move |s, tx| s.on_speech_event(event, tx));
        app.frame();
    };
    let owner = key.clone();
    event(
        &mut app,
        SpeechEvent::Phase {
            owner: owner.clone(),
            phase: Phase::Recording,
        },
    );
    assert_eq!(label(&app, "dictate").as_deref(), Some("Stop dictation"));
    event(
        &mut app,
        SpeechEvent::Transcript {
            owner: owner.clone(),
            text: "hello".into(),
        },
    );
    assert_eq!(draft(&app, &key), "Note: hello");
    event(
        &mut app,
        SpeechEvent::Transcript {
            owner: owner.clone(),
            text: " hello there ".into(),
        },
    );
    assert_eq!(draft(&app, &key), "Note: hello there");
    // Nothing is sent while the words are still coming.
    press(&mut app, Key::Enter, Mods::default());
    assert!(!app.rt.state.composers[&key].busy);

    // Once the user edits the transcript, later ones leave the draft alone.
    press(&mut app, Key::Backspace, Mods::default());
    event(
        &mut app,
        SpeechEvent::Transcript {
            owner: owner.clone(),
            text: "hello there you".into(),
        },
    );
    assert_eq!(draft(&app, &key), "Note: hello ther");
    event(
        &mut app,
        SpeechEvent::End {
            owner: owner.clone(),
            text: String::new(),
            error: None,
        },
    );
    assert_eq!(label(&app, "dictate").as_deref(), Some("Dictate"));
    assert_eq!(app.rt.state.composers[&key].error, None);

    // A capture that heard nothing says so.
    let start = key.clone();
    app.rt.update(move |s, _| {
        let composer = s.composer(&start);
        let mut dictation = super::mic::Dictation::at(&composer.edit);
        dictation.phase = Phase::Recording;
        composer.dictation = Some(dictation);
    });
    event(
        &mut app,
        SpeechEvent::End {
            owner,
            text: String::new(),
            error: None,
        },
    );
    assert_eq!(
        app.rt.state.composers[&key].error.as_deref(),
        Some("No speech detected.")
    );
}

#[test]
fn send_during_dictation_waits_for_the_last_words_then_sends() {
    let (mut app, key) = focused(Scene::NewThread);
    typed(&mut app, "Note:");
    let start = key.clone();
    app.rt.update(move |s, _| {
        let composer = s.composer(&start);
        let mut dictation = super::mic::Dictation::at(&composer.edit);
        dictation.phase = Phase::Recording;
        composer.dictation = Some(dictation);
    });
    tap(&mut app, "send-message");
    // Pressed mid-dictation: nothing sent yet, the send waits for the capture's end.
    assert!(!app.rt.state.composers[&key].busy);
    // The capture's end sends at once. Observed inside the same update: the offline
    // fixture's launch fails right away, and its reply may land by the next frame.
    let end = key.clone();
    let mut sent = None;
    app.rt.update(|s, tx| {
        s.on_speech_event(
            SpeechEvent::End {
                owner: end.clone(),
                text: "last words".into(),
                error: None,
            },
            tx,
        );
        let composer = &s.composers[&end];
        assert!(composer.dictation.is_none() && !composer.send_after_dictation);
        sent = composer.busy.then(|| plain_text(&composer.edit));
    });
    assert_eq!(
        sent.as_deref(),
        Some("Note: last words"),
        "the draft with the last words was sent"
    );
}

fn tap(app: &mut TestApp, element: &str) {
    let (x, y) = app
        .rt
        .find(element)
        .unwrap_or_else(|| panic!("{element} shows"));
    app.rt.pointer_down(x, y);
    app.rt.pointer_up(x, y);
    app.frame();
}

/// The picker rows drawn highlighted.
fn highlighted(app: &TestApp) -> Vec<String> {
    app.rt
        .elements()
        .into_iter()
        .filter(|e| {
            e.role == "option"
                && e.key.as_deref().is_some_and(|k| k.starts_with("model-"))
                && e.fill != rune::Color::TRANSPARENT
        })
        .filter_map(|e| e.key)
        .collect()
}

#[test]
fn arrow_keys_walk_every_picker_row_once_with_a_starred_model() {
    let (mut app, key) = focused(Scene::NewThread);
    let sonnet = "anthropic/claude-sonnet-4-5";
    let haiku = "anthropic/claude-haiku-4-5";
    app.rt
        .update(|s, tx| s.update_ui(|ui| ui.toggle_favorite("anthropic/claude-sonnet-4-5"), tx));
    tap(&mut app, "model-picker");
    // Favorites: Sonnet; Anthropic: Opus (chosen), Sonnet, Haiku; then OpenAI.
    let active = |app: &TestApp| app.rt.state.composers[&key].picker.active;
    assert_eq!(active(&app), Some(1), "the chosen model is highlighted");
    app.settle(30);
    assert_eq!(highlighted(&app), [format!("model-{OPUS}")]);
    press(&mut app, Key::Down, Mods::default());
    assert_eq!(active(&app), Some(2));
    app.settle(30);
    assert_eq!(
        highlighted(&app),
        [format!("model-{sonnet}")],
        "one copy only"
    );
    press(&mut app, Key::Down, Mods::default());
    assert_eq!(
        active(&app),
        Some(3),
        "past the starred model's second copy"
    );
    press(&mut app, Key::Down, Mods::default());
    assert_eq!(active(&app), Some(4), "on into the next provider");
    press(&mut app, Key::Up, Mods::default());
    press(&mut app, Key::Enter, Mods::default());
    assert_eq!(
        app.rt
            .state
            .composer_choice(&key)
            .map(|c| c.slug().to_owned())
            .as_deref(),
        Some(haiku)
    );

    // Searching highlights the first match; Enter takes it.
    tap(&mut app, "model-picker");
    typed(&mut app, "sonnet");
    assert_eq!(active(&app), Some(0));
    press(&mut app, Key::Enter, Mods::default());
    assert_eq!(
        app.rt
            .state
            .composer_choice(&key)
            .map(|c| c.slug().to_owned())
            .as_deref(),
        Some(sonnet)
    );
}

/// Environment `env`'s state with status `status`.
fn set_status(app: &mut TestApp, env: &str, status: Status) {
    let env = env.to_owned();
    app.rt.update(move |s, tx| {
        let mut state = (**s.envs.get(&env).unwrap().state()).clone();
        state.status = status;
        s.environment_changed(&env, std::sync::Arc::new(state), tx);
    });
    app.frame();
}

#[test]
fn project_commands_are_asked_for_once_the_environment_is_live() {
    let (mut app, key) = focused(Scene::NewThread);
    let env = key.env().to_owned();
    let requested = |app: &TestApp| app.rt.state.composers[&key].commands_requested;
    set_status(&mut app, &env, Status::Connecting);
    app.rt.update(|s, _| {
        s.composer(&new_thread_key()).commands_requested = false;
    });
    app.frame();
    app.frame();
    assert!(!requested(&app), "nothing to ask while connecting");
    // Still focused, no focus change: the poll asks once the environment is live.
    set_status(&mut app, &env, Status::Connected);
    app.frame();
    assert!(requested(&app));
}

#[test]
fn a_new_thread_without_a_project_has_no_composer() {
    let (mut app, _) = focused(Scene::NewThread);
    // The environment lists no projects (any more).
    app.rt.update(|s, tx| {
        let env = crate::environment::LOCAL_KEY;
        let mut state = (**s.envs.get(env).unwrap().state()).clone();
        state.shell.projects.clear();
        s.environment_changed(env, std::sync::Arc::new(state), tx);
    });
    app.settle(10);
    assert_eq!(app.rt.state.new_thread.project, None);
    app.rt.update(|s, tx| s.focus_composer(tx));
    app.settle(10);
    let s = &app.rt.state;
    assert_eq!(s.main_composer_key(), None);
    let projectless = ComposerKey::NewThread {
        env: crate::environment::LOCAL_KEY.to_owned(),
        project: None,
    };
    assert!(!s.composers.contains_key(&projectless));
    assert!(
        s.composers.values().all(|c| c.usage_watch.is_none()),
        "no usage watch for a composer nobody sees"
    );
}

#[test]
fn composers_of_threads_that_are_gone_are_dropped() {
    let (mut app, open) = focused(Scene::Thread);
    let env = open.env().to_owned();
    let gone = ComposerKey::Thread(crate::sidebar::ThreadKey::new(env.clone(), "thr-gone"));
    let image_key = gone.clone();
    app.rt.update(move |s, _| {
        s.composer(&image_key).edit.replace_text("draft");
        s.add_composer_image(&image_key, PNG.to_vec(), None);
    });
    set_status(&mut app, &env, Status::Connected);
    let s = &app.rt.state;
    assert!(
        !s.composers.contains_key(&gone),
        "an unlisted thread's draft goes"
    );
    assert!(s.composers.contains_key(&open), "the open thread's stays");

    // A removed environment takes its composers with it.
    let shieldy = crate::app::fixtures::SHIELDY_URL;
    let other = ComposerKey::NewThread {
        env: shieldy.to_owned(),
        project: None,
    };
    let made = other.clone();
    app.rt.update(move |s, _| {
        s.composer(&made);
    });
    app.rt
        .update(move |s, tx| s.remove_environment(shieldy, tx).unwrap());
    assert!(!app.rt.state.composers.contains_key(&other));
}
