//! The sidebar's shell logic, and headless runs over the fixture scenes: T3's order and
//! settle-and-advance, the row context menu, drag and drop, the clock, herdr's ordering and
//! glyphs, and the goto picker.

use std::{sync::Arc, time::Duration};

use j3_client::workflow::{AgentOrder, AgentStatus, StatusIndicators};
use rune::{Key, KeyEvent, Mods};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::*;
use crate::{
    actions::Action,
    app::{
        Main,
        fixtures::{self, NOW, OPEN_THREAD, SHIELDY_URL, Scene},
        testing::TestApp,
    },
    dialogs::Dialog,
    environment::LOCAL_KEY,
    ui_settings::{UiSettings, Workflow},
};

#[test]
fn adjacent_wraps_and_starts_at_the_ends() {
    let order: Vec<ThreadKey> = ["a", "b", "c"]
        .into_iter()
        .map(|id| ThreadKey::new("local", id))
        .collect();
    let key = |id: &str| ThreadKey::new("local", id);
    assert_eq!(adjacent(&order, Some(&key("a")), 1), Some(&key("b")));
    assert_eq!(adjacent(&order, Some(&key("c")), 1), Some(&key("a")));
    assert_eq!(adjacent(&order, Some(&key("a")), -1), Some(&key("c")));
    assert_eq!(adjacent(&order, None, 1), Some(&key("a")));
    assert_eq!(adjacent(&order, None, -1), Some(&key("c")));
    assert_eq!(adjacent(&order, Some(&key("x")), 1), Some(&key("a")));
    assert_eq!(adjacent(&[], None, 1), None);
}

#[test]
fn element_ids_are_namespaced_per_environment() {
    assert_ne!(
        ThreadKey::new("local", "t1").element_id("thread"),
        ThreadKey::new("http://x:1", "t1").element_id("thread")
    );
}

#[test]
fn the_clock_ticks_on_the_minute() {
    let now = OffsetDateTime::parse("2026-11-01T12:00:45.250Z", &Rfc3339).unwrap();
    assert_eq!(until_next_minute(now), Duration::from_millis(14_750));
}

fn local(id: &str) -> ThreadKey {
    ThreadKey::new(LOCAL_KEY, id)
}

fn scene(scene: Scene, ui: UiSettings) -> TestApp {
    let mut app = TestApp::with_ui(ui);
    fixtures::install(&mut app.rt.state, scene);
    app.settle(30);
    app
}

fn open(app: &mut TestApp, key: ThreadKey) {
    app.rt.update(move |s, tx| s.open_thread(key, tx));
    app.settle(10);
}

fn dispatch(app: &mut TestApp, action: Action) {
    app.rt.update(move |s, tx| s.dispatch(action, tx));
    app.settle(10);
}

fn tap_at(app: &mut TestApp, (x, y): (f32, f32)) {
    app.rt.pointer_down(x, y);
    app.rt.pointer_up(x, y);
    app.settle(10);
}

fn tap(app: &mut TestApp, key: &str) {
    let at = app.rt.find(key).unwrap_or_else(|| panic!("{key} is drawn"));
    tap_at(app, at);
}

/// The centre of the element whose accessible label is `label`.
fn labelled(app: &TestApp, label: &str) -> Option<(f32, f32)> {
    app.rt
        .elements()
        .into_iter()
        .find(|e| e.label.as_deref() == Some(label))
        .map(|e| (e.rect[0] + e.rect[2] / 2.0, e.rect[1] + e.rect[3] / 2.0))
}

/// Taps the topmost element labelled `label` (an open menu's item over a row's button).
fn tap_label(app: &mut TestApp, label: &str) {
    let at = app
        .rt
        .elements()
        .into_iter()
        .rfind(|e| e.label.as_deref() == Some(label))
        .map(|e| (e.rect[0] + e.rect[2] / 2.0, e.rect[1] + e.rect[3] / 2.0))
        .unwrap_or_else(|| panic!("“{label}” is drawn"));
    tap_at(app, at);
}

fn right_click(app: &mut TestApp, key: &str) {
    let (x, y) = app.rt.find(key).unwrap_or_else(|| panic!("{key} is drawn"));
    app.rt.secondary_down(x, y);
    app.rt.secondary_up(x, y);
    app.settle(10);
}

fn sent(app: &mut TestApp) -> Vec<(ThreadKey, ThreadCommand)> {
    std::mem::take(&mut app.rt.state.sidebar.sent)
}

fn close_dialog(app: &mut TestApp) {
    app.rt.update(|s, _| s.close_dialog());
    app.settle(60);
}

fn plain(key: Key) -> KeyEvent {
    KeyEvent {
        key,
        mods: Mods::default(),
    }
}

#[test]
fn t3_lists_pinned_then_active_by_return_and_navigation_wraps() {
    let mut app = scene(Scene::Thread, UiSettings::default());
    let order = app.rt.state.thread_order();
    let ids: Vec<&str> = order.iter().map(|k| k.thread_id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "thr-release",
            OPEN_THREAD,
            "thr-dictation",
            "thr-math",
            "thr-layout",
            "thr-caddy",
            "thr-acl",
            "thr-tokens",
        ],
        "collapsed shelves show none of their rows"
    );
    assert_eq!(app.rt.state.nth_thread(1), Some(local("thr-release")));
    dispatch(&mut app, Action::PreviousThread);
    assert_eq!(app.rt.state.route.main, Main::Thread(local("thr-release")));
    dispatch(&mut app, Action::PreviousThread);
    assert_eq!(app.rt.state.route.main, Main::Thread(local("thr-tokens")));
    dispatch(&mut app, Action::NextThread);
    assert_eq!(app.rt.state.route.main, Main::Thread(local("thr-release")));
}

#[test]
fn rows_say_their_status_and_collapsed_shelves_keep_the_open_thread() {
    let mut app = scene(Scene::Thread, UiSettings::default());
    assert!(labelled(&app, "Typeset display math in markdown, Done").is_some());
    assert!(labelled(&app, "Dictation drops the first word, Failed").is_some());
    assert!(labelled(&app, "Working 3").is_some());
    assert!(
        app.rt
            .find(&local("thr-zoom").element_id("thread"))
            .is_none()
    );
    open(&mut app, local("thr-zoom"));
    assert!(
        labelled(&app, "Zoom shortcuts on Linux, Settled").is_some(),
        "the collapsed Settled shelf still shows the open thread"
    );
    // Unfolding saves the shelf.
    tap(&mut app, "shelf-snoozed");
    assert!(app.rt.state.prefs.ui().is_expanded(Shelf::Snoozed));
    assert!(labelled(&app, "Follow up on the notarization ticket, Snoozed").is_some());
}

#[test]
fn settle_and_advance_follows_the_sidebar_order() {
    let mut app = scene(Scene::Thread, UiSettings::default());
    dispatch(&mut app, Action::SettleThread);
    assert_eq!(
        sent(&mut app),
        [(local(OPEN_THREAD), ThreadCommand::Settle)]
    );
    assert_eq!(
        app.rt.state.route.main,
        Main::Thread(local("thr-dictation"))
    );

    // From the last row it wraps to the first that isn't settled or snoozed.
    open(&mut app, local("thr-tokens"));
    dispatch(&mut app, Action::SettleThread);
    assert_eq!(
        sent(&mut app),
        [(local("thr-tokens"), ThreadCommand::Settle)]
    );
    assert_eq!(app.rt.state.route.main, Main::Thread(local("thr-release")));

    // A settled thread un-settles in place.
    open(&mut app, local("thr-zoom"));
    dispatch(&mut app, Action::SettleThread);
    assert_eq!(
        sent(&mut app),
        [(local("thr-zoom"), ThreadCommand::Unsettle)]
    );
    assert_eq!(app.rt.state.route.main, Main::Thread(local("thr-zoom")));
}

#[test]
fn the_row_menu_runs_thread_commands() {
    let mut app = scene(Scene::Fixtures, UiSettings::default());
    let math = local("thr-math");
    let row = math.element_id("thread");

    right_click(&mut app, &row);
    tap_label(&mut app, "Pin thread");
    assert_eq!(sent(&mut app), [(math.clone(), ThreadCommand::Pin(None))]);
    assert!(
        app.rt.state.sidebar.row_menu.is_none(),
        "a command closes the menu"
    );

    right_click(&mut app, &row);
    tap_label(&mut app, "Mark unread");
    assert_eq!(sent(&mut app), [(math.clone(), ThreadCommand::MarkUnread)]);

    right_click(&mut app, &row);
    tap_label(&mut app, "Auto-settle");
    tap_label(&mut app, "Disabled");
    assert_eq!(
        sent(&mut app),
        [(math.clone(), ThreadCommand::AutoSettle(false))]
    );

    right_click(&mut app, &row);
    tap_label(&mut app, "Snooze");
    // The first preset of the submenu (its labels follow the clock and time zone).
    tap(&mut app, "thread-menu-item2-sub-item0");
    match sent(&mut app).as_slice() {
        [(key, ThreadCommand::Snooze(until))] => {
            assert_eq!(key, &math);
            assert!(*until > NOW, "a preset wakes later");
        }
        other => panic!("one snooze, not {other:?}"),
    }

    right_click(&mut app, &row);
    tap_label(&mut app, "Snooze");
    tap_label(&mut app, "Custom…");
    assert!(
        matches!(&app.rt.state.dialogs.open, Some(Dialog::CustomSnooze(form)) if form.key == math)
    );
    close_dialog(&mut app);

    right_click(&mut app, &row);
    tap_label(&mut app, "Rename thread…");
    assert!(
        matches!(&app.rt.state.dialogs.open, Some(Dialog::RenameThread { key, .. }) if *key == math)
    );
    close_dialog(&mut app);

    right_click(&mut app, &row);
    tap_label(&mut app, "Delete…");
    assert!(
        matches!(&app.rt.state.dialogs.open, Some(Dialog::DeleteThread { key, .. }) if *key == math)
    );
    close_dialog(&mut app);

    // A settled row offers Un-settle; a snoozed one Wake.
    app.rt.update(|s, tx| {
        s.toggle_shelf(Shelf::Settled, tx);
        s.toggle_shelf(Shelf::Snoozed, tx);
    });
    app.settle(10);
    right_click(&mut app, &local("thr-zoom").element_id("thread"));
    tap_label(&mut app, "Un-settle thread");
    assert_eq!(
        sent(&mut app),
        [(local("thr-zoom"), ThreadCommand::Unsettle)]
    );
    right_click(&mut app, &local("thr-notarize").element_id("thread"));
    tap_label(&mut app, "Wake thread");
    assert_eq!(
        sent(&mut app),
        [(local("thr-notarize"), ThreadCommand::Wake)]
    );
}

#[test]
fn the_hover_action_settles_and_dropping_on_settled_settles() {
    let mut app = scene(Scene::Fixtures, UiSettings::default());
    let math = local("thr-math");
    let (x, y) = app.rt.find(&math.element_id("thread")).unwrap();
    app.rt.pointer_move(x, y);
    app.settle(5);
    tap(&mut app, &math.element_id("row-action"));
    assert_eq!(sent(&mut app), [(math.clone(), ThreadCommand::Settle)]);

    let (x, y) = app.rt.find(&math.element_id("thread")).unwrap();
    app.rt.pointer_down(x, y);
    app.rt.pointer_move(x, y + 12.0);
    app.settle(5);
    assert!(
        app.rt.find("drag-preview").is_some(),
        "a card follows the pointer"
    );
    assert!(
        app.rt.find("drop-pinned").is_some(),
        "the Pinned boundary shows while dragging"
    );
    let (sx, sy) = app.rt.find("shelf-settled").unwrap();
    app.rt.pointer_move(sx, sy);
    app.settle(5);
    app.rt.pointer_up(sx, sy);
    app.settle(5);
    assert_eq!(sent(&mut app), [(math.clone(), ThreadCommand::Settle)]);
    assert!(app.rt.state.sidebar.drag.is_none());
    assert_eq!(app.rt.state.route.main, Main::Empty, "a drag is no click");

    // Onto the Pinned boundary: pin (first, so before the pinned row).
    let (x, y) = app.rt.find(&math.element_id("thread")).unwrap();
    app.rt.pointer_down(x, y);
    app.rt.pointer_move(x, y - 12.0);
    app.settle(5);
    let (px, py) = app.rt.find("drop-pinned").unwrap();
    app.rt.pointer_move(px, py);
    app.settle(5);
    app.rt.pointer_up(px, py);
    app.settle(5);
    match sent(&mut app).as_slice() {
        [(key, ThreadCommand::Pin(Some(_))), ..] => assert_eq!(key, &math),
        other => panic!("a pin, not {other:?}"),
    }
}

#[test]
fn time_moves_rows_by_itself() {
    let mut app = scene(Scene::Fixtures, UiSettings::default());
    let t = app.rt.state.clock.t;
    let wait = app.rt.state.sidebar.tick_at.unwrap() - t;
    assert!(
        wait > 1.0 && wait <= 60.0,
        "no timer shows: the minute ({wait})"
    );

    // A thread snoozed for three seconds wakes on its own and says so.
    let mut state = (**app.rt.state.envs.get(LOCAL_KEY).unwrap().state()).clone();
    let math = state
        .shell
        .threads
        .iter_mut()
        .find(|thread| thread.id == "thr-math")
        .unwrap();
    let iso = |at: OffsetDateTime| at.format(&Rfc3339).unwrap();
    let now = app.rt.state.clock.now;
    math.snoozed_until = Some(Some(iso(now + time::Duration::seconds(3))));
    math.snoozed_at = Some(Some(iso(now)));
    app.rt.update(move |s, _| {
        s.envs
            .get_mut(LOCAL_KEY)
            .unwrap()
            .set_state(Arc::new(state));
    });
    app.settle(5);
    let row = local("thr-math").element_id("thread");
    assert!(
        app.rt.find(&row).is_none(),
        "snoozed into the collapsed shelf"
    );
    let wait = app.rt.state.sidebar.tick_at.unwrap() - app.rt.state.clock.t;
    assert!(wait <= 3.1, "the clock wakes with the snooze ({wait})");
    for _ in 0..240 {
        app.frame();
    }
    assert!(labelled(&app, "Typeset display math in markdown, Woke").is_some());

    // Working timers tick every second.
    app.rt.update(|s, tx| s.toggle_shelf(Shelf::Working, tx));
    app.settle(5);
    let wait = app.rt.state.sidebar.tick_at.unwrap() - app.rt.state.clock.t;
    assert!(wait <= 1.0, "a working timer shows ({wait})");
}

#[test]
fn the_project_filter_scopes_rows_and_new_threads() {
    let mut app = scene(Scene::Fixtures, UiSettings::default());
    tap(&mut app, "project-filter");
    tap_label(&mut app, "rune · workstation");
    let filter = app.rt.state.sidebar.project_filter.clone().unwrap();
    assert_eq!(filter.project_id, "prj-rune");
    assert!(app.rt.find("project-filter-chip").is_some());
    let order = app.rt.state.thread_order();
    assert!(
        order
            .iter()
            .all(|k| ["thr-layout", "thr-tokens"].contains(&k.thread_id.as_str())),
        "{order:?}"
    );
    tap(&mut app, "new-thread");
    assert_eq!(app.rt.state.route.main, Main::NewThread);
    assert_eq!(app.rt.state.new_thread.project.as_deref(), Some("prj-rune"));
    tap(&mut app, "clear-project-filter");
    assert!(app.rt.state.sidebar.project_filter.is_none());
}

#[test]
fn up_and_down_move_through_the_sidebar_once_it_has_the_keyboard() {
    let mut app = scene(Scene::Thread, UiSettings::default());
    dispatch(&mut app, Action::FocusSidebar);
    app.key(plain(Key::Down));
    assert_eq!(
        app.rt.state.route.main,
        Main::Thread(local("thr-dictation"))
    );
    app.key(plain(Key::Up));
    app.key(plain(Key::Up));
    assert_eq!(app.rt.state.route.main, Main::Thread(local("thr-release")));
}

#[test]
fn herdr_orders_agents_and_rolls_up_spaces() {
    let ui = UiSettings {
        agent_order: AgentOrder::Attention,
        status_indicators: StatusIndicators::Symbols,
        ..UiSettings::default()
    };
    let mut app = scene(Scene::Herdr, ui);
    assert_eq!(app.rt.state.prefs.ui().workflow, Workflow::Herdr);
    let rows = app.rt.state.herdr_rows();
    let priorities: Vec<u8> = rows.agents.iter().map(|a| a.status.priority()).collect();
    assert!(
        priorities.windows(2).all(|w| w[0] >= w[1]),
        "{priorities:?}"
    );
    assert_eq!(rows.agents[0].status, AgentStatus::Blocked);
    assert_eq!(app.rt.state.thread_order(), rows.order());
    assert!(
        labelled(&app, "j3code, blocked").is_some(),
        "a space rolls up its most urgent agent"
    );
    let glyph = |app: &TestApp, id: &str| {
        app.rt
            .elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some(&local(id).element_id("glyph")))
            .and_then(|e| e.label)
    };
    assert_eq!(glyph(&app, "thr-sidebar-dnd").as_deref(), Some("◐"));
    assert_eq!(glyph(&app, "thr-tokens").as_deref(), Some("○"));
    app.rt.update(|s, tx| {
        s.update_ui(
            |ui| {
                ui.status_indicators = StatusIndicators::Dots;
                ui.agent_order = AgentOrder::Spaces;
            },
            tx,
        )
    });
    app.settle(5);
    assert_eq!(glyph(&app, "thr-sidebar-dnd").as_deref(), Some("●"));
    let rows = app.rt.state.herdr_rows();
    let projects: Vec<&str> = rows.agents.iter().map(|a| a.project.as_str()).collect();
    let mut grouped = projects.clone();
    grouped.dedup();
    assert_eq!(
        grouped,
        ["j3code", "rune", "infra"],
        "by space, in project order"
    );

    // A space opens its most urgent agent.
    tap(&mut app, "space-local-prj-rune");
    assert_eq!(app.rt.state.route.main, Main::Thread(local("thr-layout")));
}

#[test]
fn the_goto_picker_filters_searches_and_jumps() {
    let mut app = scene(Scene::Thread, UiSettings::default());
    dispatch(&mut app, Action::GotoAgent);
    assert!(app.rt.state.dialogs.is_goto());
    let count = |app: &TestApp| {
        app.rt
            .elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some("goto-count"))
            .and_then(|e| e.text)
    };
    let agents = app.rt.state.goto_rows().agents.clone();
    let with = |status| {
        let n = agents.iter().filter(|a| a.status == status).count();
        format!("{n} agents")
    };
    assert_eq!(count(&app), Some(format!("{} agents", agents.len())));
    app.key(plain(Key::Text("b".into())));
    assert_eq!(app.rt.state.sidebar.goto.filter, Some(AgentStatus::Blocked));
    assert_eq!(count(&app), Some(with(AgentStatus::Blocked)));
    app.key(plain(Key::Text("d".into())));
    assert_eq!(count(&app), Some(with(AgentStatus::Done)));
    app.key(plain(Key::Text("a".into())));
    assert_eq!(count(&app), Some(format!("{} agents", agents.len())));
    app.key(plain(Key::Text("/".into())));
    app.settle(3);
    app.key(plain(Key::Text("tailscale".into())));
    assert_eq!(count(&app).as_deref(), Some("1 agent"));
    app.key(plain(Key::Enter));
    assert!(!app.rt.state.dialogs.is_goto(), "Enter closes the picker");
    assert_eq!(
        app.rt.state.route.main,
        Main::Thread(ThreadKey::new(SHIELDY_URL, "thr-acl"))
    );
}

#[test]
fn only_the_sidebar_rows_take_the_sidebar_keys() {
    let mut app = scene(Scene::Thread, UiSettings::default());
    let s = &app.rt.state;
    assert!(s.sidebar_owns_keys(Some(&local("thr-math").element_id("thread"))));
    assert!(
        !s.sidebar_owns_keys(Some("thread-actions")),
        "the header's thread menu only shares the prefix"
    );
    assert!(!s.sidebar_owns_keys(Some("thread-title")));
    assert!(
        !s.sidebar_owns_keys(None),
        "nothing gave the sidebar the keyboard"
    );
    app.rt
        .update(|s, tx| s.update_ui(|ui| ui.workflow = Workflow::Herdr, tx));
    app.settle(5);
    let s = &app.rt.state;
    assert!(s.sidebar_owns_keys(Some(&local("thr-math").element_id("agent"))));
    assert!(s.sidebar_owns_keys(Some("space-local-prj-rune")));
    assert!(!s.sidebar_owns_keys(Some(&local("thr-math").element_id("thread"))));
}

#[test]
fn rows_are_built_once_per_change() {
    let mut app = scene(Scene::Fixtures, UiSettings::default());
    let s = &app.rt.state;
    let rows = s.t3_rows();
    assert!(
        Rc::ptr_eq(&rows, &s.t3_rows()),
        "the view and navigation share one build"
    );
    let _ = s.thread_order();
    assert!(Rc::ptr_eq(&rows, &s.t3_rows()));
    app.rt.update(|s, _| {
        s.set_project_filter(Some(ProjectKey {
            env: LOCAL_KEY.into(),
            project_id: "prj-rune".into(),
        }))
    });
    app.settle(5);
    assert!(
        !Rc::ptr_eq(&rows, &app.rt.state.t3_rows()),
        "a new filter rebuilds them"
    );
}

#[test]
fn menus_close_when_the_list_is_hidden_and_keep_their_place_on_zoom() {
    let mut app = scene(Scene::Fixtures, UiSettings::default());
    let row = local("thr-math").element_id("thread");
    right_click(&mut app, &row);
    let at = app
        .rt
        .state
        .sidebar
        .row_menu
        .as_ref()
        .expect("the menu opened")
        .at;
    dispatch(&mut app, Action::ZoomIn);
    assert_eq!(
        app.rt.state.sidebar.row_menu.as_ref().map(|m| m.at),
        Some(at),
        "the menu keeps its point in the zoomed content"
    );
    dispatch(&mut app, Action::OpenSettings);
    assert!(
        app.rt.state.sidebar.row_menu.is_none(),
        "Settings hides the list"
    );
    dispatch(&mut app, Action::CloseSettings);
    assert!(
        app.rt.find("thread-menu-menu").is_none(),
        "it doesn't come back"
    );

    tap(&mut app, "project-filter");
    assert!(app.rt.state.sidebar.filter_menu);
    app.rt
        .update(|s, tx| s.update_ui(|ui| ui.workflow = Workflow::Herdr, tx));
    app.settle(5);
    app.rt
        .update(|s, tx| s.update_ui(|ui| ui.workflow = Workflow::T3, tx));
    app.settle(5);
    assert!(!app.rt.state.sidebar.filter_menu);
    assert!(app.rt.find("project-filter-menu-menu").is_none());
}

#[test]
fn drop_targets_of_rows_that_left_are_forgotten() {
    let mut app = scene(Scene::Fixtures, UiSettings::default());
    let math = local("thr-math").element_id("thread");
    assert!(app.rt.state.sidebar.drop_rects.contains_key(&math));
    app.rt.update(|s, _| {
        s.set_project_filter(Some(ProjectKey {
            env: LOCAL_KEY.into(),
            project_id: "prj-rune".into(),
        }))
    });
    app.settle(5);
    let rects = &app.rt.state.sidebar.drop_rects;
    assert!(
        !rects.contains_key(&math),
        "{:?}",
        rects.keys().collect::<Vec<_>>()
    );
    assert!(rects.contains_key(&local("thr-tokens").element_id("thread")));
}
