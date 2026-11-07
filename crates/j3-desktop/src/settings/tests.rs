//! Settings save what the user picks to `desktop.json` and give the window back.

use rune::{Key, KeyEvent, Mods};

use super::Page;
use crate::{
    app::{
        Main,
        fixtures::{Scene, open_thread_key},
        testing::TestApp,
    },
    preferences::Preferences,
    ui_settings::Workflow,
};

/// Scrolls `key` into view, then presses and releases it.
fn tap(app: &mut TestApp, key: &str) {
    let target = key.to_owned();
    app.rt.update(move |_, tx| tx.scroll_into_view(target));
    app.settle(60);
    let (x, y) = app.rt.find(key).unwrap_or_else(|| panic!("{key} is drawn"));
    app.rt.pointer_move(x, y);
    app.rt.pointer_down(x, y);
    app.rt.pointer_up(x, y);
    app.frame();
}

#[test]
fn general_choices_are_saved_to_desktop_json() {
    let dir = std::env::temp_dir().join(format!("j3-settings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("desktop.json");
    let mut app = TestApp::new();
    app.rt.state.prefs = Preferences::load(Some(path.clone()), "host");
    app.rt.update(|s, tx| s.open_settings(Page::General, tx));
    app.frame();

    tap(&mut app, "workflow-herdr");
    tap(&mut app, "working-shelf");
    tap(&mut app, "interface-font-size-increment");
    let ui = app.rt.state.prefs.ui();
    assert_eq!(ui.workflow, Workflow::Herdr);
    assert!(!ui.working_shelf);
    assert_eq!(ui.interface_font_size, 17.0);

    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("desktop.json was written"))
            .expect("desktop.json is JSON");
    assert_eq!(saved["ui"]["workflow"], "herdr");
    assert_eq!(saved["ui"]["workingShelf"], false);
    assert_eq!(saved["ui"]["interfaceFontSize"], 17.0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn steppers_stop_at_their_bounds() {
    let mut app = TestApp::new();
    app.rt.update(|s, tx| {
        s.update_ui(|ui| ui.text_measure = 62.0, tx);
        s.open_settings(Page::General, tx);
    });
    app.frame();
    tap(&mut app, "text-width-increment");
    assert_eq!(app.rt.state.prefs.ui().text_measure, 64.0);
    tap(&mut app, "text-width-increment");
    assert_eq!(
        app.rt.state.prefs.ui().text_measure,
        64.0,
        "64 is the widest"
    );
}

#[test]
fn done_and_escape_give_the_thread_back() {
    let mut app = TestApp::with_scene(Scene::Thread);
    app.frame();
    app.rt.update(|s, tx| s.open_settings(Page::Projects, tx));
    app.frame();
    assert!(app.rt.find("remove-project-local-prj-j3code").is_some());
    tap(&mut app, "close-settings");
    assert!(!app.rt.state.route.settings);
    assert_eq!(app.rt.state.route.main, Main::Thread(open_thread_key()));

    app.rt
        .update(|s, tx| s.open_settings(Page::Notifications, tx));
    app.frame();
    tap(&mut app, "sounds");
    assert!(!app.rt.state.prefs.ui().sounds);
    app.key(KeyEvent {
        key: Key::Escape,
        mods: Mods::default(),
    });
    assert!(!app.rt.state.route.settings);
    assert!(
        app.rt
            .find(&open_thread_key().element_id("composer-input"))
            .is_some()
    );
}

#[test]
fn a_project_with_threads_is_not_removed() {
    let mut app = TestApp::with_scene(Scene::Settings);
    app.rt.update(|s, tx| s.open_settings(Page::Projects, tx));
    app.frame();
    tap(&mut app, "remove-project-local-prj-j3code");
    assert!(app.rt.state.dialogs.open.is_none());
    let threads = app.rt.state.envs.get("local").map_or(0, |env| {
        env.state()
            .shell
            .threads_of(crate::app::fixtures::J3CODE_PROJECT)
            .len()
    });
    let toast = app
        .rt
        .state
        .toasts
        .iter()
        .next()
        .map(|(_, t)| t.title.clone());
    assert_eq!(
        toast,
        Some(format!(
            "“j3code” still has {threads} thread(s). Archive or delete them first."
        )),
        "the refusal is a notification"
    );
}

#[test]
fn archived_threads_list_and_ask_before_deleting() {
    let mut app = TestApp::with_scene(Scene::SettingsArchived);
    app.frame();
    for key in [
        "archived-thr-archived-recent",
        "unarchive-thr-archived-old",
        "archived-http://shieldy:3780",
    ] {
        assert!(app.rt.find(key).is_some(), "{key} is drawn");
    }
    tap(&mut app, "delete-thr-archived-week");
    let deleting = matches!(
        &app.rt.state.dialogs.open,
        Some(crate::dialogs::Dialog::DeleteThread { key, archived: true, .. })
            if key.thread_id == "thr-archived-week"
    );
    assert!(deleting, "Delete… asks first");
    assert!(app.rt.find("dialog-confirm").is_some());
}

#[test]
fn an_environment_added_while_archived_shows_loads_its_archive() {
    use std::sync::Arc;

    use crate::{
        archived::Listing,
        environment::{Environment, EnvironmentKind},
    };

    let mut app = TestApp::with_scene(Scene::SettingsArchived);
    app.frame();
    let url = "http://revived:3780";
    let runtime = app.tokio.handle().clone();
    app.rt.update(move |s, _| {
        s.envs.push(Environment::with_state(
            url,
            "revived",
            EnvironmentKind::Remote,
            j3_client::Connection::offline(url, runtime),
            Arc::new(j3_client::ConnectionState::default()),
            None,
        ));
    });
    app.settle(60);
    let archive = app.rt.state.settings.archived.get(url);
    assert!(
        archive.is_some_and(|a| !matches!(a.listing, Listing::Loading)),
        "the new environment's archive was asked for and answered"
    );
    assert!(app.rt.find(&format!("archived-{url}")).is_some());
}
