//! The git control: labels, the commit dialog's choices, and the control in the window
//! (shown for a repository, menu, dialog, a running action, rereads at run end).

use std::sync::Arc;

use j3_protocol::t3::{
    git::{VcsChangeRequest, VcsChangeRequestState, VcsPullResult, VcsPullStatus, VcsStatusResult},
    orchestration::RunStatus,
};

use super::{commit, fixtures, pulled};
use crate::app::{
    fixtures::{Scene, open_thread_key},
    testing::TestApp,
};

#[test]
fn pull_outcomes_read_as_toasts() {
    let pull = |status| VcsPullResult {
        status,
        ref_name: "main".into(),
        upstream_ref: None,
    };
    assert_eq!(
        pulled(&pull(VcsPullStatus::Pulled)),
        ("Pulled", "Updated main from upstream".to_owned())
    );
    assert_eq!(
        pulled(&pull(VcsPullStatus::SkippedUpToDate)),
        (
            "Already up to date",
            "main is already synchronized.".to_owned()
        )
    );
}

#[test]
fn the_commit_dialog_offers_what_the_branch_allows() {
    let feature = fixtures::status();
    assert_eq!(commit::offers(feature.clone()), (true, true));
    let default = VcsStatusResult {
        is_default_ref: true,
        ..feature.clone()
    };
    assert_eq!(commit::offers(default), (false, true));
    let with_pr = VcsStatusResult {
        pr: Some(VcsChangeRequest {
            number: 7,
            title: "Files".into(),
            url: "https://github.com/o/r/pull/7".into(),
            base_ref: "main".into(),
            head_ref: "rune-desktop-filesgit".into(),
            state: VcsChangeRequestState::Open,
            is_draft: None,
            updated_at: None,
        }),
        ..feature.clone()
    };
    assert_eq!(commit::offers(with_pr), (false, true));
    let local = VcsStatusResult {
        source_control_provider: None,
        has_primary_remote: false,
        has_upstream: false,
        ..feature
    };
    assert_eq!(commit::offers(local), (false, false));
}

/// Frames until `done` holds (async answers land in the inbox from tokio), at most ~2 s.
fn until(app: &mut TestApp, done: impl Fn(&TestApp) -> bool) {
    for _ in 0..400 {
        app.frame();
        if done(app) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("timed out");
}

fn click(app: &mut TestApp, key: &str) {
    let (x, y) = app.rt.find(key).unwrap_or_else(|| panic!("{key} is drawn"));
    app.rt.pointer_down(x, y);
    app.rt.pointer_up(x, y);
    app.frame();
}

fn label(app: &TestApp, key: &str) -> Option<String> {
    app.rt
        .elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some(key))
        .and_then(|e| e.label)
}

#[test]
fn the_control_shows_for_a_repository_and_opens_the_commit_dialog() {
    let mut app = TestApp::with_scene(Scene::Files);
    app.settle(10);
    let key = open_thread_key();
    assert_eq!(
        label(&app, "git-quick-action").as_deref(),
        Some("Commit, push & PR")
    );

    click(&mut app, "git-menu");
    assert!(app.rt.state.git[&key].menu_open);
    assert!(app.rt.find("git-control-menu-item0").is_some(), "Commit…");
    click(&mut app, "git-control-menu-item0");
    assert!(!app.rt.state.git[&key].menu_open);
    assert!(app.rt.state.git[&key].commit.is_some());
    app.frame();
    for part in [
        "git-commit-files",
        "git-commit-push-pr",
        "git-commit-push",
        "git-commit",
        "dialog-cancel",
    ] {
        assert!(app.rt.find(part).is_some(), "{part} is drawn");
    }
    assert_eq!(
        label(&app, "git-commit-files").as_deref(),
        Some("5 changed files")
    );
    assert!(app.rt.state.git[&key].message.focused);

    click(&mut app, "dialog-cancel");
    assert!(app.rt.state.git[&key].commit.is_none());
    assert!(app.rt.find("git-commit-files").is_none());

    // Not a repository: nothing shows.
    let hide = key.clone();
    app.rt.update(move |s, _| {
        s.git.get_mut(&hide).unwrap().status = Some(VcsStatusResult::not_a_repo());
    });
    app.frame();
    assert!(app.rt.find("git-quick-action").is_none());
}

#[test]
fn a_commit_runs_under_one_working_label_and_reports_its_failure() {
    let mut app = TestApp::with_scene(Scene::GitCommit);
    app.settle(10);
    let key = open_thread_key();
    click(&mut app, "git-commit");
    assert!(app.rt.state.git[&key].commit.is_none(), "the dialog closes");
    assert!(app.rt.state.git[&key].running);
    assert_eq!(
        label(&app, "git-quick-action").as_deref(),
        Some("Working...")
    );
    // Offline, the action fails at once: an error toast, and the control is free again.
    until(&mut app, |app| !app.rt.state.git[&key].running);
    assert_eq!(app.rt.state.toasts.iter().count(), 1);
}

#[test]
fn the_end_of_a_run_rereads_the_status_and_a_failed_read_keeps_the_last() {
    let mut app = TestApp::with_scene(Scene::Files);
    app.settle(10);
    let key = open_thread_key();
    assert!(
        app.rt.state.git[&key].run_active,
        "the fixture thread is mid-run"
    );
    assert!(app.rt.find("git-quick-action").is_some());
    // The run ends: the next poll rereads the status.
    let ended = key.clone();
    app.rt.update(move |s, _| {
        let thread = s.threads.get_mut(&ended).unwrap();
        let mut state = (*thread.state).clone();
        for run in &mut state.projection.as_mut().unwrap().runs {
            if run.status.is_active() {
                run.status = RunStatus::Completed;
            }
        }
        thread.state = Arc::new(state);
    });
    app.frame();
    assert!(!app.rt.state.git[&key].run_active);
    assert!(app.rt.state.git[&key].refreshing, "a read is in flight");
    // Offline the read fails; the last good status stays and so does the control.
    until(&mut app, |app| !app.rt.state.git[&key].refreshing);
    assert_eq!(app.rt.state.git[&key].status, Some(fixtures::status()));
    assert_eq!(
        label(&app, "git-quick-action").as_deref(),
        Some("Commit, push & PR")
    );
}

#[test]
fn a_running_action_keeps_the_control_without_a_status() {
    let mut app = TestApp::with_scene(Scene::Files);
    app.settle(10);
    let key = open_thread_key();
    let busy = key.clone();
    app.rt.update(move |s, _| {
        let git = s.git.get_mut(&busy).unwrap();
        git.status = None;
        git.running = true;
    });
    app.frame();
    assert_eq!(
        label(&app, "git-quick-action").as_deref(),
        Some("Working...")
    );
    let idle = key.clone();
    app.rt
        .update(move |s, _| s.git.get_mut(&idle).unwrap().running = false);
    app.frame();
    assert!(app.rt.find("git-quick-action").is_none());
}
