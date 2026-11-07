//! Dialog forms refuse what they can't use, with the reason inline, and close once done.

use super::{Dialog, Field, snooze::INVALID};
use crate::app::{
    fixtures::{LOCAL_LABEL, Scene, open_thread_key},
    testing::TestApp,
};

fn tap(app: &mut TestApp, key: &str) {
    let (x, y) = app.rt.find(key).unwrap_or_else(|| panic!("{key} is drawn"));
    app.rt.pointer_move(x, y);
    app.rt.pointer_down(x, y);
    app.rt.pointer_up(x, y);
    app.frame();
}

/// The text of the dialog's error line, if it shows one.
fn error_line(app: &TestApp) -> Option<String> {
    app.rt
        .elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some("dialog-error") && !e.exiting)
        .and_then(|e| e.text)
}

#[test]
fn add_environment_explains_what_is_missing_then_adds_it() {
    let mut app = TestApp::new();
    app.rt
        .update(|s, tx| s.open_dialog(Dialog::add_environment(), tx));
    app.frame();
    assert!(app.rt.find("dialog-cancel").is_some());

    tap(&mut app, "dialog-confirm");
    assert_eq!(error_line(&app).as_deref(), Some("Enter a name."));

    app.rt
        .update(|s, _| s.dialogs.field(Field::Label).replace_text("revived"));
    app.frame();
    tap(&mut app, "dialog-confirm");
    assert_eq!(error_line(&app).as_deref(), Some("Enter a host or URL."));

    app.rt
        .update(|s, _| s.dialogs.field(Field::Url).replace_text("ftp://revived"));
    app.frame();
    tap(&mut app, "dialog-confirm");
    assert_eq!(
        error_line(&app).as_deref(),
        Some("Use an http:// or https:// URL.")
    );

    app.rt
        .update(|s, _| s.dialogs.field(Field::Url).replace_text("revived"));
    app.frame();
    tap(&mut app, "dialog-confirm");
    assert!(
        app.rt.state.dialogs.open.is_none(),
        "a valid environment closes the dialog"
    );
    assert!(app.rt.state.envs.get("http://revived:3780").is_some());
    assert_eq!(app.rt.state.prefs.remotes().len(), 1);
}

#[test]
fn an_empty_title_is_not_a_rename() {
    let mut app = TestApp::with_scene(Scene::Thread);
    app.rt.update(|s, tx| {
        let dialog = Dialog::rename_thread(s, open_thread_key());
        s.open_dialog(dialog, tx);
    });
    app.frame();
    let title = app.rt.state.dialogs.field(Field::Title);
    assert_eq!(title.text, "Port the feed renderer to Rune");
    assert_eq!(
        (title.anchor, title.focus),
        (0, title.text.len()),
        "the title is selected"
    );
    app.rt
        .update(|s, _| s.dialogs.field(Field::Title).replace_text("   "));
    tap(&mut app, "dialog-confirm");
    assert!(matches!(
        app.rt.state.dialogs.open,
        Some(Dialog::RenameThread { .. })
    ));
    tap(&mut app, "dialog-cancel");
    assert!(app.rt.state.dialogs.open.is_none());
}

#[test]
fn a_garbled_snooze_is_refused_inline() {
    let mut app = TestApp::with_scene(Scene::CustomSnooze);
    app.frame();
    assert!(app.rt.find("snooze-unit-hours").is_some());
    app.rt
        .update(|s, _| s.dialogs.field(Field::SnoozeAmount).replace_text("later"));
    tap(&mut app, "dialog-confirm");
    assert_eq!(error_line(&app).as_deref(), Some(INVALID));
    tap(&mut app, "snooze-schedule-date-and-time");
    assert!(app.rt.find("snooze-date").is_some());
    assert_eq!(app.rt.state.dialogs.field(Field::SnoozeTime).text, "09:00");
    assert!(
        error_line(&app).is_none(),
        "switching schedule clears the error"
    );
}

#[test]
fn the_project_name_follows_the_folder_until_edited() {
    let mut app = TestApp::with_scene(Scene::AddProject);
    app.frame();
    assert!(app.rt.find("browse-/home/jacob/repos/j3code").is_some());
    let device = format!("project-device-{}", crate::environment::LOCAL_KEY);
    assert!(app.rt.find(&device).is_some(), "{LOCAL_LABEL} is a device");

    tap(&mut app, "browse-/home/jacob/repos/j3code");
    assert_eq!(
        app.rt.state.dialogs.field(Field::Path).text,
        "/home/jacob/repos/j3code/"
    );
    assert_eq!(
        app.rt.state.dialogs.field(Field::ProjectTitle).text,
        "j3code"
    );

    app.rt.update(|s, _| {
        if let Some(Dialog::AddProject(picker)) = &mut s.dialogs.open {
            picker.title_edited = true;
        }
        s.open_project_folder("/srv/app");
    });
    assert_eq!(
        app.rt.state.dialogs.field(Field::ProjectTitle).text,
        "j3code"
    );
}

#[test]
fn a_closed_add_project_dialogs_replies_reach_nothing() {
    let mut app = TestApp::with_scene(Scene::Fixtures);
    app.frame();
    let local = crate::environment::LOCAL_KEY.to_owned();
    // Create on the first picker (offline, so it fails), then cancel and open a second one
    // before the reply lands.
    let first = local.clone();
    app.rt.update(move |s, tx| {
        s.open_dialog(Dialog::add_project(first, true), tx);
        s.confirm_dialog(tx);
        s.close_dialog();
    });
    let second = local.clone();
    app.rt
        .update(move |s, tx| s.open_dialog(Dialog::add_project(second, true), tx));
    let revision = |app: &TestApp| match &app.rt.state.dialogs.open {
        Some(Dialog::AddProject(picker)) => (picker.id, picker.revision, picker.creating),
        _ => panic!("the second picker is open"),
    };
    let (id, browse, creating) = revision(&app);
    assert!(
        id > 1 && browse > id,
        "ids and browses come from one app-wide counter"
    );
    assert!(!creating);
    app.settle(60);
    match &app.rt.state.dialogs.open {
        Some(Dialog::AddProject(picker)) => {
            assert_eq!(picker.id, id);
            assert!(!picker.creating);
            assert_eq!(
                picker.error, None,
                "the first picker's failure isn't shown here"
            );
        }
        _ => panic!("the second picker stays open"),
    }
    assert_eq!(app.rt.state.route.main, crate::app::Main::Empty);
}
