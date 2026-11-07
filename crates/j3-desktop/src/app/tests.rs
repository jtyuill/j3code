//! The shell renders headless, keys route to actions, and inbox updates land in `poll`.

use rune::{Key, KeyEvent, Mods};

use super::{Main, testing::TestApp};
use crate::{sidebar::ThreadKey, ui_settings::UiSettings};

fn primary(c: char) -> KeyEvent {
    KeyEvent {
        key: Key::Command(c),
        mods: Mods {
            cmd: true,
            ctrl: !cfg!(target_os = "macos"),
            logo: cfg!(target_os = "macos"),
            ..Mods::default()
        },
    }
}

#[test]
fn the_shell_draws_the_sidebar_the_empty_screen_and_the_version_bar() {
    let mut app = TestApp::new();
    app.frame();
    for key in ["sidebar", "empty", "version-bar", "new-thread"] {
        assert!(app.rt.find(key).is_some(), "{key} is drawn");
    }
    let (_, bar_y) = app.rt.find("version-bar").unwrap();
    let (_, empty_y) = app.rt.find("empty").unwrap();
    assert!(bar_y > empty_y, "the version bar sits under the content");
}

#[test]
fn settings_open_and_close_with_their_keys() {
    let mut app = TestApp::new();
    app.frame();
    app.key(primary(','));
    assert!(app.rt.state.route.settings);
    assert!(app.rt.find("close-settings").is_some());
    app.key(KeyEvent {
        key: Key::Escape,
        mods: Mods::default(),
    });
    assert!(!app.rt.state.route.settings);
    assert!(app.rt.find("empty").is_some());
}

#[test]
fn inbox_updates_land_in_poll() {
    let mut app = TestApp::new();
    app.frame();
    let inbox = app.rt.state.inbox.clone();
    std::thread::spawn(move || inbox.post(|s, _| {
        s.toasts.info("hello");
    }))
        .join()
        .unwrap();
    app.frame();
    assert_eq!(app.rt.state.toasts.iter().count(), 1);
    assert!(app.rt.find("toast-region").is_some());
}

#[test]
fn spawned_work_comes_back_through_the_inbox() {
    let mut app = TestApp::new();
    app.frame();
    app.rt.state.spawn(async {
        |s: &mut super::J3, _: &mut rune::Tx| {
            s.toasts.error("late");
        }
    });
    for _ in 0..200 {
        app.frame();
        if app.rt.state.toasts.iter().count() == 1 {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the spawned update never landed");
}

#[test]
fn zoom_draws_the_whole_window_larger() {
    let mut app = TestApp::with_ui(UiSettings {
        zoom: 1.5,
        ..UiSettings::default()
    });
    app.frame();
    app.frame();
    assert!(app.rt.find("j3-zoom").is_some());
    assert!(app.rt.find("version-bar").is_some());
}

#[test]
fn a_thread_of_an_unknown_environment_does_not_open() {
    let mut app = TestApp::new();
    app.frame();
    app.rt
        .update(|s, tx| s.open_thread(ThreadKey::new("nowhere", "t1"), tx));
    app.frame();
    assert_eq!(app.rt.state.route.main, Main::Empty);
}

#[test]
fn fixture_scenes_render_offline() {
    use super::fixtures::{Scene, open_thread_key};
    use crate::ui_settings::Workflow;

    for scene in Scene::ALL {
        let mut app = TestApp::with_scene(scene);
        app.settle(30);
        let s = &app.rt.state;
        assert_eq!(s.envs.iter().count(), 2, "{scene:?}");
        assert!(s.envs.iter().all(|env| env.is_live()), "{scene:?}");
        assert_eq!(s.toasts.iter().count(), 0, "{scene:?}: no command failed");
        let open = open_thread_key();
        match scene {
            Scene::Fixtures => {
                assert_eq!(s.route.main, Main::Empty);
                assert!(app.rt.find(&open.element_id("thread")).is_some());
                assert!(app.rt.find("empty").is_some());
            }
            Scene::Thread | Scene::Herdr => {
                assert_eq!(s.route.main, Main::Thread(open.clone()));
                assert!(s.threads[&open].state.projection.is_some());
                assert!(app.rt.find("feed").is_some());
                assert_eq!(
                    s.prefs.ui().workflow == Workflow::Herdr,
                    scene == Scene::Herdr
                );
            }
            Scene::NewThread => {
                assert_eq!(s.route.main, Main::NewThread);
                assert!(app.rt.find("new-thread-view").is_some());
            }
            Scene::Settings => {
                assert!(s.route.settings);
                assert!(app.rt.find("close-settings").is_some());
            }
            Scene::Composer => {
                assert_eq!(s.route.main, Main::NewThread);
                assert!(app.rt.find("composer-ghost").is_some());
            }
            Scene::SettingsArchived => {
                assert!(s.route.settings);
                assert!(app.rt.find("archived-thr-archived-recent").is_some());
            }
            Scene::AddProject | Scene::CustomSnooze => {
                assert!(s.dialogs.open.is_some());
                assert!(app.rt.find("dialog-confirm").is_some());
            }
            Scene::Hub => {
                assert_eq!(s.route.main, Main::Thread(open.clone()));
                assert!(app.rt.find("agent-hub").is_some());
                assert!(app.rt.find("composer").is_none());
            }
        }
    }
}
