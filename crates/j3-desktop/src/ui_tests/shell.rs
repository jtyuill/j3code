//! The window frame, Settings, usage and environments.

use std::sync::Arc;

use j3_client::Status;
use rune::Key;
use serde_json::{Value, json};

use super::{
    harness::{Harness, bottom, create_project, primary, serve, temp_dir, top},
    omp::install_fake_omp,
};
use crate::{
    local_server::LocalServer,
    preferences::Preferences,
    theme::Metrics,
    ui_settings::{DEFAULT_TEXT_MEASURE, Workflow},
};

#[test]
fn the_titlebar_and_text_measure_frame_the_thread() {
    let mut app = Harness::start("frame");
    // macOS draws its traffic lights over the panes' header row.
    let lights = 80.0;
    app.rt_mut().set_titlebar_inset(lights);
    let project_id = app.project();
    app.open_running_thread(&project_id, "framed");
    app.at_rest("the header band", |h| h.element("thread-actions"));
    // The panes' header row (rune-kit's shell band) is the top of the window (on macOS, its
    // titlebar): the thread's title is in it, and the sidebar's band control clears the
    // traffic lights. Rune's shell puts the sidebar's own header (the filter) right under
    // the band, so the list follows that header.
    let row = rune_kit::shell::TOOLBAR_HEIGHT;
    let title = app.element("thread-title").expect("the thread title");
    assert!(
        top(&title) >= 0.0 && bottom(&title) <= row,
        "{:?}",
        title.rect
    );
    let toggle = app.element("sidebar-toggle").expect("the sidebar toggle");
    assert!(toggle.rect[0] >= lights, "{:?}", toggle.rect);
    assert!(bottom(&toggle) <= row);
    let header = app.element("inbox-header").expect("the Inbox header");
    assert_eq!(top(&header), row);
    let filter = app.element("project-filter").expect("the project filter");
    assert!(top(&filter) >= row && bottom(&filter) <= bottom(&header));
    let list = app.element("inbox-list").expect("the Inbox list");
    assert!(top(&list) >= bottom(&header));
    // The Inbox header offers adding a project next to the new-thread button.
    assert!(app.exists("add-project"));
    // The conversation uses the (tighter) default measure.
    let composer = app.element("composer").expect("the composer");
    let rem = Metrics::new(app.state().prefs.ui()).rem;
    assert_eq!(composer.rect[2], rem * DEFAULT_TEXT_MEASURE);
    // The build stamp is a strip under the content in the bottom-right corner, clear of
    // the composer.
    let stamp = app.element("version-bar").expect("the version bar").rect;
    let (width, height) = crate::app::testing::SIZE;
    assert_eq!(stamp[1] + stamp[3], height);
    assert_eq!(stamp[0] + stamp[2], width);
    let c = composer.rect;
    let overlaps = stamp[0] < c[0] + c[2]
        && c[0] < stamp[0] + stamp[2]
        && stamp[1] < c[1] + c[3]
        && c[1] < stamp[1] + stamp[3];
    assert!(!overlaps, "{stamp:?} {c:?}");
}

#[test]
fn usage_shows_the_model_provider_refreshes_on_press_and_keeps_the_cache_on_failure() {
    let mut app = Harness::start("usage");
    let cache = app.config_dir.join("usage.json");
    let omp_usage = app.data_dir.join("usage.json");
    let limit = |id: &str, label: &str, used: f64| {
        json!({"id": id, "label": label, "status": "ok",
            "window": {"id": "5h", "label": "5 Hour", "resetsAt": 1_793_573_042_000_i64},
            "amount": {"usedFraction": used, "unit": "percent"}})
    };
    std::fs::write(
        &omp_usage,
        json!({"generatedAt": 1_793_566_275_810_i64, "reports": [
            {"provider": "anthropic", "metadata": {"email": "me@example.com"},
             "limits": [limit("anthropic:5h", "Claude 5 Hour", 0.5)]},
            {"provider": "opencode-go", "metadata": {},
             "limits": [limit("go:5h", "5 Hour limit", 0.78)]},
        ]})
        .to_string(),
    )
    .unwrap();
    app.project();
    app.until_models();
    // The button sits beside the composer's model picker.
    app.click("new-thread");
    app.until("the usage button shows", |h| h.exists("usage"));
    // Showing the gauge fetches at once (nothing is cached yet); the gauge then shows the
    // model provider's peak.
    app.until("the gauge shows the peak", |h| {
        cache.exists() && h.has_label("78%")
    });

    app.click("usage");
    // Only the model's provider (opencode-go, account 1) is listed.
    app.until("the popover lists the provider", |h| {
        h.exists("usage-local-1-0")
    });
    assert!(!app.exists("usage-local-0-0"));
    let cached: Value = serde_json::from_slice(&std::fs::read(&cache).unwrap()).unwrap();
    assert_eq!(
        cached["local"]["accounts"][1]["limits"][0]["used"]
            .as_f64()
            .map(f64::round),
        Some(78.0)
    );

    // A failed refresh shows its error next to the cached snapshot.
    std::fs::remove_file(&omp_usage).unwrap();
    app.click("usage");
    app.until("the popover closes", |h| !h.exists("usage-panel"));
    app.click("usage");
    app.until("the error shows", |h| h.exists("usage-error-local"));
    assert!(app.exists("usage-local-1-0"));
    let after: Value = serde_json::from_slice(&std::fs::read(&cache).unwrap()).unwrap();
    assert_eq!(after, cached);
}

#[test]
fn settings_switch_the_workflow_and_persist() {
    let mut app = Harness::start("settings");
    // Inbox is the default.
    app.until("the Inbox list shows", |h| h.exists("inbox-list"));
    assert!(!app.exists("herdr-spaces"));
    assert!(app.exists("project-filter"));

    // ⌘, opens Settings over the window; herdr is one click away.
    app.key(primary(','));
    app.until("Settings opens", |h| h.exists("close-settings"));
    assert!(!app.exists("inbox-list"));
    app.click("workflow-herdr");
    app.until_config("herdr is saved", |config| {
        config["ui"]["workflow"] == "herdr"
    });
    // A switch saves too.
    app.until("the Working shelf switch shows", |h| {
        h.has_label("Working shelf")
    });
    app.click_label("Working shelf");
    app.until_config("the Working shelf is saved", |config| {
        config["ui"]["workingShelf"] == false
    });

    // Escape leaves Settings onto the herdr sidebar.
    app.press(Key::Escape);
    app.until("the herdr sidebar shows", |h| {
        !h.exists("close-settings") && h.exists("herdr-spaces") && !h.exists("inbox-list")
    });

    // And back to Inbox with the Done button.
    app.key(primary(','));
    app.until("Settings opens", |h| h.exists("workflow-inbox"));
    app.click("workflow-inbox");
    app.until_config("Inbox is saved", |config| {
        config["ui"]["workflow"] == "inbox"
    });
    app.click("close-settings");
    app.until("the Inbox list shows", |h| h.exists("inbox-list"));

    // What was saved is what the next launch loads.
    let path = app.config_dir.join("desktop.json");
    let preferences = Preferences::load(Some(path), "test-host");
    assert_eq!(preferences.ui().workflow, Workflow::Inbox);
    assert!(!preferences.ui().working_shelf);
    assert!(!preferences.ui().sounds);
}

#[test]
fn a_remote_that_is_the_local_server_is_listed_once() {
    // A second, separate server for an "other" remote.
    let other_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let other_dir = temp_dir("dedupe-other");
    let other_omp = install_fake_omp(&other_dir);
    let (other_server, other_port) = serve(&other_runtime, &other_dir, &other_omp, Arc::default());
    let other = LocalServer::from_server(other_server);

    // "twin" reaches the local server under another host name.
    let mut app = Harness::start_with(
        "dedupe",
        |port| {
            json!([
                {"label": "twin", "url": format!("http://localhost:{port}")},
                {"label": "other", "url": format!("http://127.0.0.1:{other_port}")},
            ])
        },
        json!({"workflow": "herdr", "sounds": false}),
    );

    let connections = app.state().envs.connections();
    assert_eq!(connections.len(), 3, "local, twin and other connect");
    let live = connections.clone();
    app.until("every environment is live", move |_| {
        live.iter().all(|connection| {
            let state = connection.state();
            let state = state.borrow();
            state.status == Status::Connected && state.shell.synced
        })
    });
    // A project on each server: the local one is listed once, not again under the twin;
    // the other server's after it.
    app.project();
    let remote = connections[2].clone();
    let dir = app.project_dir.to_string_lossy().into_owned();
    app.block_on(async move { create_project(&remote, "Remote", dir).await })
        .unwrap();
    app.until("both spaces show", |h| {
        h.in_sidebar("Demo, unknown") && h.in_sidebar("Remote, unknown")
    });
    assert_eq!(app.count_in_scope("herdr-spaces", "Demo, unknown"), 1);
    let demo = app.in_scope("herdr-spaces", "Demo, unknown").unwrap();
    let remote = app.in_scope("herdr-spaces", "Remote, unknown").unwrap();
    assert!(top(&remote) > top(&demo));

    // Hidden, not forgotten: the config entry stays.
    let config: Value = serde_json::from_str(
        &std::fs::read_to_string(app.config_dir.join("desktop.json")).unwrap(),
    )
    .unwrap();
    let labels: Vec<&str> = config["environments"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|env| env["label"].as_str())
        .collect();
    assert_eq!(labels, ["twin", "other"]);

    drop(app);
    other_runtime.block_on(other.shutdown());
    other_runtime.shutdown_background();
    let _ = std::fs::remove_dir_all(other_dir);
}
