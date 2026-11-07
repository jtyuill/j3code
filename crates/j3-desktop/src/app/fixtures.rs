//! Offline fixtures and the window's named scenes (`--scene NAME`, `AppSpec::scenes`): two
//! live environments — the local one and the remote `shieldy` — with a server config, three
//! projects and fifteen threads across every sidebar state, plus one open thread whose feed
//! holds every turn item kind the feed renders, all timed relative to [`NOW`].
//!
//! Nothing here touches the network: each environment runs over
//! [`Connection::offline`], whose calls fail at once, and `setup` opens no environments
//! when a scene installed these. Tests install a scene with [`install`]
//! (`TestApp::with_scene`).

use std::{collections::BTreeMap, sync::Arc};

use j3_client::{
    Connection, ConnectionState, ConnectionStatus, ShellState, ThreadState, ThreadSync,
    WorkspaceCommands,
    t3::{
        self,
        filesystem::FilesystemBrowseEntry,
        methods::ArchivedShellSnapshot,
        orchestration::{ThreadProjection, ThreadShell},
        project::ProjectShell,
        server::{ServerConfig, ServerProviderSkill, ServerProviderSlashCommand},
    },
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use time::{Duration, OffsetDateTime, macros::datetime};

use super::{Clock, J3, Main};
use crate::{
    archived::Listing,
    dialogs::{Dialog, FolderListing},
    environment::{Environment, EnvironmentKind, Environments, LOCAL_KEY},
    environments::{DesktopConfig, RemoteEnvironment},
    local_server::LocalServerStatus,
    new_thread::NewThreadState,
    preferences::Preferences,
    settings::Page,
    sidebar::ThreadKey,
    ui_settings::{UiSettings, Workflow},
};

/// The wall clock every fixture timestamp is relative to; scenes start their clock here.
pub const NOW: OffsetDateTime = datetime!(2026-11-07 16:30 UTC);
/// The local environment's label (its hostname in a real run).
pub const LOCAL_LABEL: &str = "workstation";
/// The remote environment's key and URL.
pub const SHIELDY_URL: &str = "http://shieldy:3780";
/// The local `j3code` project, the new-thread scene's target.
pub const J3CODE_PROJECT: &str = "prj-j3code";
/// The thread the `thread` and `herdr` scenes open (local environment).
pub const OPEN_THREAD: &str = "thr-feed-renderer";

/// A named starting state of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scene {
    /// The populated sidebar (T3 workflow) and the empty main pane.
    Fixtures,
    /// [`OPEN_THREAD`] open.
    Thread,
    /// The new-thread screen on the local `j3code` project.
    NewThread,
    /// Settings on its General page.
    Settings,
    /// The herdr workflow's sidebar with [`OPEN_THREAD`] open.
    Herdr,
    /// Settings on Archived threads, with the local archive loaded (shieldy's empty).
    SettingsArchived,
    /// The Add project dialog over the sidebar, listing `~/repos/` on the local device.
    AddProject,
    /// The Custom snooze dialog for [`OPEN_THREAD`].
    CustomSnooze,
}

impl Scene {
    pub const ALL: [Scene; 8] = [
        Scene::Fixtures,
        Scene::Thread,
        Scene::NewThread,
        Scene::Settings,
        Scene::Herdr,
        Scene::SettingsArchived,
        Scene::AddProject,
        Scene::CustomSnooze,
    ];

    /// The `--scene` name.
    pub fn name(self) -> &'static str {
        match self {
            Scene::Fixtures => "fixtures",
            Scene::Thread => "thread",
            Scene::NewThread => "new-thread",
            Scene::Settings => "settings",
            Scene::Herdr => "herdr",
            Scene::SettingsArchived => "settings-archived",
            Scene::AddProject => "add-project",
            Scene::CustomSnooze => "custom-snooze",
        }
    }
}

/// A scene's name and the change it makes to the initial state.
pub type SceneSetup = (&'static str, fn(&mut J3));

/// `AppSpec::scenes`, in [`Scene::ALL`] order.
pub const SCENES: &[SceneSetup] = &[
    ("fixtures", |s: &mut J3| start(s, Scene::Fixtures)),
    ("thread", |s: &mut J3| start(s, Scene::Thread)),
    ("new-thread", |s: &mut J3| start(s, Scene::NewThread)),
    ("settings", |s: &mut J3| start(s, Scene::Settings)),
    ("herdr", |s: &mut J3| start(s, Scene::Herdr)),
    ("settings-archived", |s: &mut J3| start(s, Scene::SettingsArchived)),
    ("add-project", |s: &mut J3| start(s, Scene::AddProject)),
    ("custom-snooze", |s: &mut J3| start(s, Scene::CustomSnooze)),
];

/// A scene from the command line: default UI settings (not the user's `desktop.json`), so
/// snapshots are the same on every machine, then [`install`].
fn start(s: &mut J3, scene: Scene) {
    s.prefs = Preferences::in_memory(DesktopConfig {
        environments: Vec::new(),
        ui: UiSettings::default(),
    });
    install(s, scene);
}

/// The key of [`OPEN_THREAD`].
pub fn open_thread_key() -> ThreadKey {
    ThreadKey::new(LOCAL_KEY, OPEN_THREAD)
}

/// Replaces `s`'s environments with the offline fixtures, keeps its UI settings (the
/// `herdr` scene switches the workflow) in memory-only preferences, sets the clock to
/// [`NOW`] and routes to `scene`.
pub fn install(s: &mut J3, scene: Scene) {
    let mut ui = s.prefs.ui().clone();
    if scene == Scene::Herdr {
        ui.workflow = Workflow::Herdr;
    }
    s.prefs = Preferences::in_memory(DesktopConfig {
        environments: vec![RemoteEnvironment {
            label: "shieldy".into(),
            url: SHIELDY_URL.into(),
        }],
        ui,
    });
    s.clock = Clock::new(NOW);
    s.envs = Environments::default();
    let runtime = s.services.runtime.clone();
    let local_url = s.services.local_url().to_owned();
    s.envs.push(Environment::with_state(
        LOCAL_KEY,
        LOCAL_LABEL,
        EnvironmentKind::Local(LocalServerStatus::Serving(vec![
            ([127, 0, 0, 1], 3780).into(),
        ])),
        Connection::offline(local_url, runtime.clone()),
        Arc::new(local_state()),
        None,
    ));
    s.envs.push(Environment::with_state(
        SHIELDY_URL,
        "shieldy",
        EnvironmentKind::Remote,
        Connection::offline(SHIELDY_URL, runtime),
        Arc::new(shieldy_state()),
        None,
    ));
    match scene {
        Scene::Fixtures => {}
        Scene::Thread | Scene::Herdr | Scene::CustomSnooze => {
            let key = open_thread_key();
            if s.subscribe_thread(&key).is_some()
                && let Some(thread) = s.threads.get_mut(&key)
            {
                thread.state = Arc::new(thread_state());
            }
            s.route.main = Main::Thread(key.clone());
            if scene == Scene::CustomSnooze {
                s.dialogs.open = Some(Dialog::custom_snooze(key));
            }
        }
        Scene::NewThread => {
            s.new_thread = NewThreadState {
                env: LOCAL_KEY.to_owned(),
                project: Some(J3CODE_PROJECT.to_owned()),
                ..NewThreadState::default()
            };
            s.route.main = Main::NewThread;
        }
        Scene::Settings => {
            s.settings.page = Page::General;
            s.route.settings = true;
            s.load_fonts();
        }
        Scene::SettingsArchived => {
            s.settings.page = Page::Archived;
            s.settings.archived.set(LOCAL_KEY, Listing::Loaded(archived_snapshot()));
            let empty = ArchivedShellSnapshot {
                threads: Vec::new(),
                projects: Vec::new(),
                ..archived_snapshot()
            };
            s.settings.archived.set(SHIELDY_URL, Listing::Loaded(empty));
            s.route.settings = true;
        }
        Scene::AddProject => s.dialogs.open = Some(add_project_dialog()),
    }
}

/// Three archived local threads, not in archive order.
pub fn archived_snapshot() -> ArchivedShellSnapshot {
    let archived = |minutes: i64| json!({"archivedAt": ago(minutes)});
    ArchivedShellSnapshot {
        schema_version: 1,
        snapshot_sequence: 1,
        projects: local_state().shell.projects,
        threads: vec![
            row("thr-archived-week", J3CODE_PROJECT, "Try GPUI's text system for the feed",
                60 * 24 * 9, archived(60 * 24 * 7)),
            row("thr-archived-old", "prj-rune", "Sketch the settings layout",
                60 * 24 * 40, archived(60 * 24 * 30)),
            row("thr-archived-recent", J3CODE_PROJECT, "Profile the sidebar's drag reordering",
                60 * 5, archived(90)),
        ],
    }
}

/// The Add project dialog on the local device with `~/repos/` listed.
fn add_project_dialog() -> Dialog {
    let mut dialog = Dialog::add_project(LOCAL_KEY.to_owned(), true);
    if let Dialog::AddProject(picker) = &mut dialog {
        picker.path.replace_text("~/repos/");
        picker.path.set_caret(picker.path.text.len());
        picker.title.replace_text("repos");
        let entry = |name: &str| FilesystemBrowseEntry {
            name: name.to_owned(),
            full_path: format!("/home/jacob/repos/{name}"),
        };
        picker.listing = FolderListing::Loaded {
            parent: "/home/jacob/repos/".into(),
            entries: ["dotfiles", "herdr", "j3code", "j3code-wt", "omp", "rune", "t3code"]
                .map(entry)
                .into(),
        };
    }
    dialog
}

// ---------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------

fn iso(at: OffsetDateTime) -> String {
    t3::iso_from_unix_ms(at.unix_timestamp() * 1_000)
}

/// `minutes` before [`NOW`].
pub fn ago(minutes: i64) -> String {
    iso(NOW - Duration::minutes(minutes))
}

/// `minutes` after [`NOW`].
pub fn from_now(minutes: i64) -> String {
    iso(NOW + Duration::minutes(minutes))
}

fn decode<T: DeserializeOwned>(what: &str, value: Value) -> T {
    serde_json::from_value(value).unwrap_or_else(|e| panic!("fixture {what} decodes: {e}"))
}

/// `base` with `patch`'s keys set.
fn patched(mut base: Value, patch: Value) -> Value {
    if let (Some(base), Value::Object(patch)) = (base.as_object_mut(), patch) {
        base.extend(patch);
    }
    base
}

// ---------------------------------------------------------------------------
// Environments
// ---------------------------------------------------------------------------

/// The local environment: a live server with the `j3code` and `rune` projects.
pub fn local_state() -> ConnectionState {
    let projects = vec![
        project(J3CODE_PROJECT, "j3code", "/home/jacob/repos/j3code", 60 * 24 * 40),
        project("prj-rune", "rune", "/home/jacob/repos/rune", 60 * 24 * 90),
    ];
    connection_state("env-workstation", LOCAL_LABEL, projects, local_threads())
}

/// The `shieldy` environment: a live server with the `infra` project.
pub fn shieldy_state() -> ConnectionState {
    let projects = vec![project("prj-infra", "infra", "/srv/infra", 60 * 24 * 120)];
    connection_state("env-shieldy", "shieldy", projects, shieldy_threads())
}

fn connection_state(
    environment_id: &str,
    label: &str,
    projects: Vec<ProjectShell>,
    threads: Vec<ThreadShell>,
) -> ConnectionState {
    let workspaces: BTreeMap<String, WorkspaceCommands> = projects
        .iter()
        .map(|project| {
            let commands = WorkspaceCommands {
                slash_commands: slash_commands(),
                skills: skills(),
            };
            (project.workspace_root.clone(), commands)
        })
        .collect();
    ConnectionState {
        status: ConnectionStatus::Live,
        descriptor: Some(decode("descriptor", descriptor(environment_id, label))),
        config: Some(server_config(environment_id, label)),
        shell: ShellState {
            loaded: true,
            synchronized: true,
            snapshot_sequence: 4_210,
            projects,
            threads,
            archived_threads: Vec::new(),
        },
        workspaces,
    }
}

fn descriptor(environment_id: &str, label: &str) -> Value {
    json!({
        "environmentId": environment_id, "label": label,
        "platform": {"os": "linux", "arch": "x64"},
        "serverVersion": "0.9.0", "orchestrationProtocolVersion": 2,
        "capabilities": {"repositoryIdentity": true, "attachmentUploads": true}
    })
}

fn project(id: &str, title: &str, root: &str, created_minutes_ago: i64) -> ProjectShell {
    decode(
        "project",
        json!({
            "id": id, "title": title, "workspaceRoot": root,
            "defaultModelSelection": null, "scripts": [],
            "createdAt": ago(created_minutes_ago), "updatedAt": ago(created_minutes_ago)
        }),
    )
}

/// A `Thinking` select over `off` and `levels`, `default` preselected.
fn thinking(levels: &[&str], default: &str) -> Value {
    let options: Vec<Value> = std::iter::once("off")
        .chain(levels.iter().copied())
        .map(|id| {
            let mut label = id.to_owned();
            label[..1].make_ascii_uppercase();
            let mut option = json!({"id": id, "label": label});
            if id == default {
                option["isDefault"] = json!(true);
            }
            option
        })
        .collect();
    json!({"optionDescriptors": [
        {"type": "select", "id": "thinking", "label": "Thinking", "options": options}
    ]})
}

fn model(slug: &str, name: &str, capabilities: Value) -> Value {
    let provider = slug.split_once('/').map_or(slug, |(provider, _)| provider);
    json!({
        "slug": slug, "name": name, "subProvider": provider, "isCustom": false,
        "capabilities": capabilities
    })
}

/// The server config j3 sends: one `omp` provider whose models come from Anthropic and
/// OpenAI (grouped by provider in the picker), with thinking levels, slash commands and
/// skills.
pub fn server_config(environment_id: &str, label: &str) -> ServerConfig {
    let levels = ["low", "medium", "high"];
    let openai_levels = ["minimal", "low", "medium", "high"];
    let mut opus = model("anthropic/claude-opus-4-5", "Claude Opus 4.5", thinking(&levels, "high"));
    opus["isDefault"] = json!(true);
    let models = json!([
        opus,
        model("anthropic/claude-sonnet-4-5", "Claude Sonnet 4.5", thinking(&levels, "medium")),
        model("anthropic/claude-haiku-4-5", "Claude Haiku 4.5", json!({"optionDescriptors": []})),
        model("openai/gpt-5.1", "GPT-5.1", thinking(&openai_levels, "medium")),
        model("openai/gpt-5.1-codex", "GPT-5.1 Codex", thinking(&openai_levels, "high")),
        model("openai/gpt-5-mini", "GPT-5 mini", json!({"optionDescriptors": []})),
    ]);
    decode(
        "server config",
        json!({
            "environment": descriptor(environment_id, label),
            "auth": {"policy": "remote-reachable", "bootstrapMethods": [], "sessionMethods": [],
                     "sessionCookieName": "j3"},
            "cwd": "/home/jacob", "keybindingsConfigPath": "/home/jacob/.config/j3/keybindings.json",
            "keybindings": [], "issues": [], "availableEditors": [],
            "observability": {"logsDirectoryPath": "/home/jacob/.local/share/j3/logs",
                              "localTracingEnabled": false, "otlpTracesEnabled": false,
                              "otlpMetricsEnabled": false},
            "settings": {},
            "providers": [{
                "instanceId": "omp", "driver": "pi", "displayName": "omp",
                "enabled": true, "installed": true, "version": null, "status": "ready",
                "auth": {"status": "authenticated"}, "checkedAt": ago(30),
                "models": models,
                "slashCommands": slash_commands(),
                "skills": skills()
            }]
        }),
    )
}

pub fn slash_commands() -> Vec<ServerProviderSlashCommand> {
    decode(
        "slash commands",
        json!([
            {"name": "compact", "description": "Summarize the conversation to free context",
             "input": {"hint": "focus instructions"}},
            {"name": "model", "description": "Switch the model for this thread"},
            {"name": "plan", "description": "Draft a plan before making changes"},
            {"name": "review", "description": "Review the working tree changes",
             "input": {"hint": "base branch"}},
            {"name": "goal", "description": "Set a goal the agent works towards",
             "input": {"hint": "objective"}}
        ]),
    )
}

pub fn skills() -> Vec<ServerProviderSkill> {
    decode(
        "skills",
        json!([
            {"name": "unslop", "description": "Rewrite text so it reads like a person wrote it",
             "path": "skill://unslop", "enabled": true, "userInvocable": true},
            {"name": "visualize", "description": "Turn an idea into a diagram or plot",
             "path": "skill://visualize", "enabled": true, "userInvocable": true},
            {"name": "reverse-engineer-anything",
             "description": "Investigate binaries, firmware and runtime evidence",
             "path": "skill://reverse-engineer-anything", "enabled": true, "userInvocable": true}
        ]),
    )
}

// ---------------------------------------------------------------------------
// Thread rows
// ---------------------------------------------------------------------------

fn selection(slug: &str, level: Option<&str>) -> Value {
    match level {
        Some(level) => json!({"instanceId": "omp", "model": slug,
                              "options": [{"id": "thinking", "value": level}]}),
        None => json!({"instanceId": "omp", "model": slug}),
    }
}

fn opus() -> Value {
    selection("anthropic/claude-opus-4-5", Some("high"))
}

fn codex() -> Value {
    selection("openai/gpt-5.1-codex", Some("high"))
}

/// A thread row created `created` minutes ago whose last run completed then and was seen,
/// with `patch` applied.
fn row(id: &str, project: &str, title: &str, created: i64, patch: Value) -> ThreadShell {
    let base = json!({
        "createdBy": "user", "creationSource": "web",
        "id": id, "projectId": project, "title": title,
        "providerInstanceId": "omp", "modelSelection": opus(),
        "runtimeMode": "full-access", "interactionMode": "default",
        "branch": "main", "worktreePath": null,
        "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": id},
        "forkedFrom": null,
        "activeProviderThreadId": format!("pt-{id}"),
        "latestRunId": format!("run-{id}"),
        "latestRunRequestedAt": ago(created),
        "latestRunStartedAt": ago(created),
        "latestRunCompletedAt": ago(created - 3),
        "activeRunId": null,
        "status": "completed",
        "pendingRuntimeRequest": null,
        "latestVisibleMessage": {"id": format!("msg-{id}"), "role": "assistant",
                                 "text": "Done. The changes are in the working tree.",
                                 "updatedAt": ago(created - 3)},
        "latestUserMessageAt": ago(created),
        "hasActionableProposedPlan": false,
        "itemCount": 24, "visibleItemCount": 18,
        "createdAt": ago(created), "updatedAt": ago(created - 3),
        "archivedAt": null, "settledOverride": null, "settledAt": null,
        "lastVisitedAt": ago(created - 4),
        "deletedAt": null
    });
    decode("thread row", patched(base, patch))
}

/// A run that started `minutes` ago and is still running.
fn running(id: &str, minutes: i64) -> Value {
    json!({
        "status": "running", "activeRunId": format!("run-{id}"),
        "activityRunStatus": "running", "activityRunStartedAt": ago(minutes),
        "latestRunRequestedAt": ago(minutes), "latestRunStartedAt": ago(minutes),
        "latestRunCompletedAt": null, "latestUserMessageAt": ago(minutes),
        "updatedAt": ago(0), "lastVisitedAt": ago(minutes)
    })
}

fn local_threads() -> Vec<ThreadShell> {
    let j3 = J3CODE_PROJECT;
    let rune = "prj-rune";
    vec![
        // Waiting on the user's answer, with a queued message and a goal (the open thread).
        row(OPEN_THREAD, j3, "Port the feed renderer to Rune", 55, json!({
            "status": "waiting", "latestRunId": "run-3", "activeRunId": "run-3",
            "activeProviderThreadId": "pt-feed",
            "activityRunStatus": "waiting", "activityRunStartedAt": ago(6),
            "latestRunRequestedAt": ago(6), "latestRunStartedAt": ago(6),
            "latestRunCompletedAt": null, "latestUserMessageAt": ago(1),
            "pendingRuntimeRequest": {"id": "req-anchor", "kind": "user_input",
                                      "createdAt": ago(2), "blocking": true},
            "latestVisibleMessage": {"id": "msg-3", "role": "user",
                                     "text": "Keep the anchor, and orchestrate a bench run on both displays.",
                                     "updatedAt": ago(6)},
            "goal": goal(),
            "itemCount": 27, "visibleItemCount": 25,
            "updatedAt": ago(2), "lastVisitedAt": ago(2)
        })),
        // Working.
        row("thr-sidebar-dnd", j3, "Sidebar drag and drop between shelves", 48,
            patched(running("thr-sidebar-dnd", 4), json!({"modelSelection": codex(),
                                                          "branch": "sidebar-dnd"}))),
        // Failed, not seen since.
        row("thr-dictation", j3, "Dictation drops the first word", 95, json!({
            "status": "failed", "latestRunCompletedAt": ago(9),
            "lastError": "omp exited unexpectedly (exit code 1)",
            "lastErrorClass": "provider_error",
            "updatedAt": ago(9), "lastVisitedAt": ago(30)
        })),
        // Pinned, finished and seen.
        row("thr-release", j3, "Release checklist for 0.9", 60 * 24 * 3, json!({
            "pinnedAt": ago(60 * 24 * 3), "pinOrderKey": "a0",
            "latestRunCompletedAt": ago(180), "updatedAt": ago(180), "lastVisitedAt": ago(170)
        })),
        // Finished and unread.
        row("thr-math", j3, "Typeset display math in markdown", 70, json!({
            "latestRunCompletedAt": ago(12), "updatedAt": ago(12), "lastVisitedAt": ago(40)
        })),
        // Settled.
        row("thr-zoom", j3, "Zoom shortcuts on Linux", 60 * 24 * 2 + 30, json!({
            "settledOverride": "settled", "settledAt": ago(60 * 24 * 2)
        })),
        // Snoozed for three hours.
        row("thr-notarize", j3, "Follow up on the notarization ticket", 75, json!({
            "snoozedUntil": from_now(180), "snoozedAt": ago(60)
        })),
        // Working, on GPT-5.1 Codex.
        row("thr-scroll", rune, "Momentum scrolling stutters at 120 Hz", 40,
            patched(running("thr-scroll", 15), json!({"modelSelection": codex()}))),
        // Finished a day ago and seen.
        row("thr-tokens", rune, "Theme tokens from the opencode kit", 60 * 26, json!({})),
        // Stopped by a usage limit.
        row("thr-layout", rune, "Refactor the flex layout pass", 130, json!({
            "status": "failed", "latestRunCompletedAt": ago(20),
            "lastError": "You have reached your usage limit.",
            "lastErrorClass": "usage_limit", "usageLimitResetAt": from_now(120),
            "updatedAt": ago(20), "lastVisitedAt": ago(18)
        })),
    ]
}

fn shieldy_threads() -> Vec<ThreadShell> {
    let infra = "prj-infra";
    vec![
        // Working.
        row("thr-backup", infra, "Nightly backup restore drill", 30,
            running("thr-backup", 22)),
        // Waiting on an approval.
        row("thr-acl", infra, "Tailscale ACL for the new runners", 35, json!({
            "status": "waiting", "activeRunId": "run-thr-acl",
            "activityRunStatus": "waiting", "activityRunStartedAt": ago(8),
            "latestRunCompletedAt": null,
            "pendingRuntimeRequest": {"id": "req-acl", "kind": "command",
                                      "createdAt": ago(5), "blocking": true},
            "updatedAt": ago(5), "lastVisitedAt": ago(8)
        })),
        // Finished and unread.
        row("thr-caddy", infra, "Caddy TLS renewal failing", 50, json!({
            "modelSelection": selection("openai/gpt-5.1", Some("medium")),
            "latestRunCompletedAt": ago(25), "updatedAt": ago(25), "lastVisitedAt": ago(45)
        })),
        // Settled.
        row("thr-grafana", infra, "Grafana dashboard for omp latency", 60 * 24 * 5, json!({
            "settledOverride": "settled", "settledAt": ago(60 * 24 * 4)
        })),
        // Snoozed until tomorrow morning.
        row("thr-disk", infra, "Disk pressure alerts on shieldy", 200, json!({
            "snoozedUntil": from_now(16 * 60 + 30), "snoozedAt": ago(150)
        })),
    ]
}

fn goal() -> Value {
    json!({"objective": "Feed renders every turn item kind on Rune with no scroll jumps",
           "status": "active", "tokensUsed": 48_210, "tokenBudget": 200_000,
           "timeUsedSeconds": 1_260})
}

// ---------------------------------------------------------------------------
// The open thread
// ---------------------------------------------------------------------------

const ANSWER: &str = r#"## Why the feed jumps

The scroller measures a streaming reply **after** it lays out the rows under the anchor, so
every new line pushes the viewport down by its height. With rows of height $h_i$ above the
anchor, one frame drifts by

$$\Delta y = \sum_{i=1}^{n} \left(h_i' - h_i\right)$$

until the reply settles, which is the jump in your screenshot.

### What changed

| Change | File | Effect |
|---|---|---|
| Anchor to the last visible row | `src/thread/anchor.rs` | No drift while streaming |
| Remeasure only changed rows | `src/thread/mod.rs` | About 4× fewer layouts |
| Bench scenario | `benches/feed.rs` | Guards the 120 Hz budget |

```rust
/// Keeps `anchor` at the same screen offset across a relayout.
pub fn hold(anchor: &Anchor, before: f32, after: f32) -> f32 {
    let drift = after - before;
    anchor.offset - drift
}
```

#### Results

- Frame time while streaming: 6.8 ms → 2.1 ms
- Clippy is clean again after dropping the unused `delta`

![Frame time before and after](data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20width='240'%20height='72'%3E%3Crect%20x='0'%20y='8'%20width='220'%20height='22'%20rx='4'%20fill='%23e5484d'/%3E%3Crect%20x='0'%20y='42'%20width='68'%20height='22'%20rx='4'%20fill='%2330a46c'/%3E%3C/svg%3E)
"#;

/// The open thread's live state: a feed with every rendered turn item kind, a run waiting
/// on a two-question `ask`, a queued message, a goal and an active todo plan.
pub fn thread_state() -> ThreadState {
    ThreadState {
        thread_id: OPEN_THREAD.to_owned(),
        sync: ThreadSync::Live,
        sequence: 312,
        projection: Some(projection()),
    }
}

/// One feed item of the open thread: `(run, status, minutes ago, seconds long, title,
/// type-specific body)`.
type FeedItem = (&'static str, &'static str, i64, i64, Option<&'static str>, Value);

/// The open thread's feed items, in order.
fn feed_items() -> Vec<FeedItem> {
    let image = json!({"type": "image", "id": "att-feed-jump", "name": "feed-jump.png",
                       "mimeType": "image/png", "sizeBytes": 184_320});
    vec![
        // Run 1: the investigation, completed.
        ("run-1", "completed", 52, 0, None, json!({
            "type": "user_message", "createdBy": "user", "creationSource": "web",
            "messageId": "msg-1", "inputIntent": "turn_start",
            "text": "Use ^anthropic/claude-opus-4-5 and ultrathink this: why does the feed jump while a long reply streams in? Screenshot attached.",
            "attachments": [image]
        })),
        ("run-1", "completed", 52, 18, None, json!({
            "type": "reasoning", "streaming": false,
            "text": "The jump lines up with each streamed chunk. The virtual list keeps its scroll offset fixed, but the rows **above** the viewport are remeasured when the reply grows, so the content under the viewport shifts.\n\nCheck where `remeasure_items` runs and whether the anchor is the first or last visible row."
        })),
        ("run-1", "completed", 51, 1, Some("Finding where rows are remeasured"), json!({
            "type": "file_search", "pattern": "remeasure",
            "results": [
                {"fileName": "crates/j3-desktop/src/thread/mod.rs", "line": 142, "preview": "        self.remeasure(&changed);"},
                {"fileName": "crates/j3-desktop/src/thread/mod.rs", "line": 188, "preview": "    fn remeasure(&mut self, rows: &[usize]) {"},
                {"fileName": "crates/j3-desktop/src/feed.rs", "line": 61, "preview": "/// Rows whose item changed are remeasured; the rest keep their height."}
            ]
        })),
        ("run-1", "completed", 50, 1, Some("Reading the thread view"), json!({
            "type": "dynamic_tool", "toolName": "read",
            "input": {"path": "crates/j3-desktop/src/thread/mod.rs:125-131"},
            "output": "[crates/j3-desktop/src/thread/mod.rs#6163]\n125:pub fn view(s: &J3, cx: &mut Cx<J3>, ui: &mut Ui, key: &ThreadKey) -> El {\n126:    let k = ui.k;\n127:    let note = |message: String| {\n128:        el().key(\"feed-note\")\n129:            .w_full()\n130:            .grow(1.0)\n131:            .center()"
        })),
        ("run-1", "completed", 49, 3, Some("Searching for scroll anchoring"), json!({
            "type": "web_search", "patterns": ["virtual list scroll anchoring streaming content"],
            "results": [
                {"title": "CSS Scroll Anchoring", "url": "https://drafts.csswg.org/css-scroll-anchoring/",
                 "snippet": "Scroll anchoring aims to keep the visible content stable when content above it changes size."},
                {"title": "Building a chat list that does not jump", "url": "https://blog.example.dev/chat-scroll",
                 "snippet": "Anchor to the last fully visible message and correct the offset after layout."},
                {"title": "Virtualized lists and dynamic heights", "url": "https://github.com/anomalyco/rune/discussions/412",
                 "snippet": "Measure once, then correct by the delta of rows above the viewport."}
            ]
        })),
        ("run-1", "failed", 48, 4, None, json!({
            "type": "error",
            "failure": {"class": "provider_error", "message": "overloaded_error: Overloaded",
                        "code": "529", "retryable": true},
            "retry": {"attempt": 2, "maxAttempts": 5, "retryDelayMs": 4_000}
        })),
        ("run-1", "completed", 46, 38, Some("Running the feed tests"), json!({
            "type": "command_execution", "input": "cargo test -p j3-desktop feed",
            "output": "   Compiling j3-desktop v0.9.0 (/home/jacob/repos/j3code/crates/j3-desktop)\n    Finished `test` profile [unoptimized + debuginfo] target(s) in 31.84s\n     Running unittests src/lib.rs (target/debug/deps/j3_desktop-5c1f0e2a9b7d4e31)\n\nrunning 14 tests\ntest feed::tests::ages_are_compact ... ok\ntest feed::tests::diff_lines_split_omp_and_unified_forms ... ok\ntest feed::tests::dynamic_tools_get_their_omp_titles ... ok\ntest feed::tests::edits_writes_and_searches_read_like_omp ... ok\n\ntest result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 212 filtered out; finished in 0.04s\n",
            "exitCode": 0
        })),
        ("run-1", "completed", 45, 24, Some("Linting with clippy"), json!({
            "type": "command_execution", "input": "cargo clippy -p j3-desktop --all-targets -- -D warnings",
            "output": "    Checking j3-desktop v0.9.0 (/home/jacob/repos/j3code/crates/j3-desktop)\nerror: unused variable: `delta`\n  --> crates/j3-desktop/src/thread/anchor.rs:18:9\n   |\n18 |     let delta = after - before;\n   |         ^^^^^ help: if this is intentional, prefix it with an underscore: `_delta`\n   |\n   = note: `-D unused-variables` implied by `-D warnings`\n\nerror: could not compile `j3-desktop` (lib) due to 1 previous error\n",
            "exitCode": 101, "outputIndicatesFailure": true
        })),
        ("run-1", "completed", 43, 1, None, json!({
            "type": "file_change", "fileName": "crates/j3-desktop/src/thread/mod.rs",
            "additions": 6, "deletions": 2,
            "diffStr": "@@ -139,8 +139,12 @@\n 139|        let changed = self.changed_rows(&state);\n-140|        self.remeasure(&changed);\n-141|        self.scroll.keep_offset();\n+140|        let anchor = self.scroll.last_visible();\n+141|        let before = self.scroll.offset_of(anchor);\n+142|        self.remeasure(&changed);\n+143|        let after = self.scroll.offset_of(anchor);\n+144|        self.scroll.shift(anchor::hold(&anchor, before, after));\n+145|        self.scroll.settle();\n 146|    }"
        })),
        ("run-1", "completed", 42, 1, None, json!({
            "type": "dynamic_tool", "toolName": "write",
            "input": {"path": "crates/j3-desktop/src/thread/anchor.rs",
                      "content": "//! Keeps the feed still while rows above the viewport change height.\n\n/// The row the viewport is pinned to and its offset from the top.\npub struct Anchor {\n    pub row: usize,\n    pub offset: f32,\n}\n\n/// Keeps `anchor` at the same screen offset across a relayout.\npub fn hold(anchor: &Anchor, before: f32, after: f32) -> f32 {\n    let drift = after - before;\n    anchor.offset - drift\n}\n"},
            "output": "Wrote crates/j3-desktop/src/thread/anchor.rs (13 lines)"
        })),
        ("run-1", "completed", 41, 1, None, json!({
            "type": "dynamic_tool", "toolName": "edit",
            "input": {"path": "crates/j3-desktop/src/thread/mod.rs",
                      "input": "[crates/j3-desktop/src/thread/mod.rs#6163]\nPUT <8:\n+mod anchor;\n+"},
            "output": "Applied 1 edit to crates/j3-desktop/src/thread/mod.rs"
        })),
        ("run-1", "completed", 40, 2, None, json!({
            "type": "dynamic_tool", "toolName": "eval",
            "input": {"language": "py", "title": "Comparing frame times",
                      "code": "before = [6.9, 6.7, 6.8]\nafter = [2.2, 2.0, 2.1]\nprint(f\"{sum(before)/3:.1f} ms -> {sum(after)/3:.1f} ms\")"},
            "output": "6.8 ms -> 2.1 ms"
        })),
        ("run-1", "completed", 39, 0, None, json!({
            "type": "todo_list", "planId": "plan-feed",
            "explanation": "Fix the jump, then guard it with a bench.",
            "steps": todo_steps()
        })),
        ("run-1", "completed", 38, 2, None, json!({
            "type": "dynamic_tool", "toolName": "task",
            "input": {"agent": "task", "tasks": [
                {"name": "AuditScroll", "task": "Find every place the feed changes its scroll offset"},
                {"name": "BenchFeed", "task": "Measure frame time while a 4,000-line reply streams"}
            ]},
            "output": "Started 2 agents: AuditScroll, BenchFeed"
        })),
        ("run-1", "completed", 37, 140, None, json!({
            "type": "subagent", "subagentId": "sa-audit", "origin": "provider_native",
            "driver": "pi", "providerInstanceId": "omp", "childThreadId": null,
            "prompt": "Find every place the feed changes its scroll offset and list them with file:line.",
            "progress": "Checked 9 call sites",
            "result": "Three writers: `thread/mod.rs:144` (anchor shift), `thread/mod.rs:201` (jump to bottom on send) and `rune_kit::MessageScroller::follow`. Only the first runs while streaming."
        })),
        ("run-1", "completed", 34, 2, None, json!({
            "type": "dynamic_tool", "toolName": "mcp__github__search_issues",
            "input": {"query": "scroll jump is:open repo:anomalyco/rune"},
            "output": "#412 Virtualized lists and dynamic heights (open)\n#398 MessageScroller jumps when a row above grows (open)"
        })),
        ("run-1", "completed", 31, 25, None, json!({
            "type": "assistant_message", "messageId": "msg-1a", "text": ANSWER, "streaming": false
        })),
        // Between runs.
        ("run-1", "completed", 29, 0, None, json!({
            "type": "system_notice", "message": "Model changed to GPT-5.1 Codex · thinking high"
        })),
        ("run-1", "completed", 28, 12, None, json!({
            "type": "compaction", "driver": "pi",
            "summary": "Investigated the feed jump, anchored the scroll to the last visible row and added a bench.",
            "beforeTokenCount": 182_340, "afterTokenCount": 24_120
        })),
        // Run 2: stopped by the user.
        ("run-2", "completed", 20, 0, None, json!({
            "type": "user_message", "createdBy": "user", "creationSource": "web",
            "messageId": "msg-2", "inputIntent": "turn_start",
            "text": "Also try it on the 240 Hz display.", "attachments": []
        })),
        ("run-2", "interrupted", 19, 40, None, json!({
            "type": "reasoning", "streaming": false,
            "text": "The 240 Hz display is on the other machine; I need the bench binary there first."
        })),
        ("run-2", "completed", 18, 0, None, json!({
            "type": "run_interrupt_request", "message": "Stop"
        })),
        ("run-2", "completed", 18, 0, None, json!({
            "type": "run_interrupt_result", "message": "Stopped by the user."
        })),
        // Run 3: active, waiting on the user's answer.
        ("run-3", "completed", 6, 0, None, json!({
            "type": "user_message", "createdBy": "user", "creationSource": "web",
            "messageId": "msg-3", "inputIntent": "turn_start",
            "text": "Keep the anchor, and orchestrate a bench run on both displays.",
            "attachments": []
        })),
        ("run-3", "running", 5, 0, None, json!({
            "type": "subagent", "subagentId": "sa-bench", "origin": "provider_native",
            "driver": "pi", "providerInstanceId": "omp", "childThreadId": null,
            "prompt": "Measure frame time while a 4,000-line reply streams, at 60, 120 and 240 Hz.",
            "progress": "120 Hz: 2.1 ms median, running 240 Hz", "result": null
        })),
        ("run-3", "waiting", 2, 0, None, json!({
            "type": "user_input_request", "requestId": "req-anchor",
            "questions": [
                {"id": "anchor", "header": "Anchor",
                 "question": "Where should the feed stay anchored while a reply streams?",
                 "options": [
                     {"label": "Bottom", "description": "Follow new output as it arrives (recommended)"},
                     {"label": "Top of the reply", "description": "Keep the start of the reply in view"},
                     {"label": "Where I left it", "description": "Never scroll on its own"}
                 ],
                 "allowCustomAnswer": true},
                {"id": "displays", "header": "Displays",
                 "question": "Which refresh rates should the bench cover?",
                 "options": [
                     {"label": "60 Hz", "description": "The laptop panel"},
                     {"label": "120 Hz", "description": "ProMotion"},
                     {"label": "240 Hz", "description": "The external monitor"}
                 ],
                 "multiSelect": true}
            ]
        })),
        // Run 4: queued behind run 3 (shown in the queue card, hidden in the feed).
        ("run-4", "pending", 1, 0, None, json!({
            "type": "user_message", "createdBy": "user", "creationSource": "web",
            "messageId": "msg-4", "inputIntent": "queued_turn",
            "text": "Then write the changelog entry.", "attachments": []
        })),
    ]
}

fn todo_steps() -> Value {
    json!([
        {"id": "s1", "text": "Find where rows are remeasured", "status": "completed"},
        {"id": "s2", "text": "Anchor the scroll to the last visible row", "status": "completed"},
        {"id": "s3", "text": "Bench frame time while streaming", "status": "running"},
        {"id": "s4", "text": "Write the changelog entry", "status": "pending"}
    ])
}

fn turn_item(ordinal: usize, item: &FeedItem) -> Value {
    let (run, status, minutes, seconds, title, body) = item;
    let started = NOW - Duration::minutes(*minutes);
    let completed = matches!(*status, "completed" | "failed" | "interrupted")
        .then(|| iso(started + Duration::seconds(*seconds)));
    let base = json!({
        "id": format!("item-{ordinal}"), "threadId": OPEN_THREAD,
        "runId": run, "nodeId": format!("node-{run}"),
        "providerThreadId": "pt-feed", "providerTurnId": null, "nativeItemRef": null,
        "parentItemId": null, "ordinal": ordinal, "status": status, "title": title,
        "startedAt": iso(started), "completedAt": completed,
        "updatedAt": completed.clone().unwrap_or_else(|| iso(started))
    });
    patched(base, body.clone())
}

fn run(id: &str, ordinal: u64, status: &str, requested: i64, completed: Option<i64>) -> Value {
    json!({
        "id": id, "threadId": OPEN_THREAD, "ordinal": ordinal,
        "providerInstanceId": "omp", "modelSelection": codex(),
        "providerThreadId": "pt-feed", "userMessageId": format!("msg-{ordinal}"),
        "rootNodeId": format!("node-{id}"), "activeAttemptId": null, "status": status,
        "queuePosition": (status == "queued").then_some(1),
        "requestedAt": ago(requested),
        "startedAt": (status != "queued").then(|| ago(requested)),
        "completedAt": completed.map(ago),
        "checkpointId": null, "contextHandoffId": null
    })
}

fn message(ordinal: u64, run: &str, text: &str, minutes: i64) -> Value {
    json!({
        "createdBy": "user", "creationSource": "web", "id": format!("msg-{ordinal}"),
        "threadId": OPEN_THREAD, "runId": run, "nodeId": format!("node-{run}"),
        "role": "user", "text": text, "attachments": [], "streaming": false,
        "createdAt": ago(minutes), "updatedAt": ago(minutes)
    })
}

fn subagent(id: &str, title: &str, status: &str, started: i64, item: &Value) -> Value {
    json!({
        "id": id, "threadId": OPEN_THREAD, "runId": item["runId"], "parentNodeId": item["nodeId"],
        "origin": "provider_native", "createdBy": "agent", "driver": "pi",
        "providerInstanceId": "omp", "providerThreadId": null, "childThreadId": null,
        "nativeTaskRef": null, "prompt": item["prompt"], "title": title,
        "model": "anthropic/claude-sonnet-4-5", "status": status, "progress": item["progress"],
        "result": item["result"], "startedAt": ago(started),
        "completedAt": item["completedAt"], "updatedAt": item["updatedAt"]
    })
}

fn projection() -> ThreadProjection {
    let items: Vec<Value> = feed_items()
        .iter()
        .enumerate()
        .map(|(ix, item)| turn_item(ix + 1, item))
        .collect();
    let find = |subagent_id: &str| {
        items
            .iter()
            .find(|item| item["subagentId"] == subagent_id)
            .cloned()
            .unwrap_or_default()
    };
    let visible: Vec<Value> = items
        .iter()
        .enumerate()
        .map(|(ix, item)| {
            json!({"position": ix + 1, "visibility": "local", "sourceThreadId": OPEN_THREAD,
                   "sourceItemId": item["id"], "item": item})
        })
        .collect();
    decode(
        "thread projection",
        json!({
            "thread": {
                "createdBy": "user", "creationSource": "web", "id": OPEN_THREAD,
                "projectId": J3CODE_PROJECT, "title": "Port the feed renderer to Rune",
                "providerInstanceId": "omp", "modelSelection": codex(),
                "runtimeMode": "full-access", "interactionMode": "default",
                "branch": "feed-anchor", "worktreePath": null,
                "activeProviderThreadId": "pt-feed",
                "lineage": {"parentThreadId": null, "relationshipToParent": null,
                            "rootThreadId": OPEN_THREAD},
                "forkedFrom": null, "createdAt": ago(55), "updatedAt": ago(1),
                "archivedAt": null, "lastVisitedAt": ago(2), "deletedAt": null
            },
            "runs": [
                run("run-1", 1, "completed", 52, Some(31)),
                run("run-2", 2, "interrupted", 20, Some(18)),
                run("run-3", 3, "waiting", 6, None),
                run("run-4", 4, "queued", 1, None)
            ],
            "attempts": [], "nodes": [],
            "subagents": [
                subagent("sa-audit", "AuditScroll", "completed", 37, &find("sa-audit")),
                subagent("sa-bench", "BenchFeed", "running", 5, &find("sa-bench"))
            ],
            "providerSessions": [],
            "providerThreads": [{
                "id": "pt-feed", "driver": "pi", "providerInstanceId": "omp",
                "providerSessionId": null, "appThreadId": OPEN_THREAD, "ownerNodeId": null,
                "nativeThreadRef": null, "nativeConversationHeadRef": null, "status": "active",
                "firstRunOrdinal": 1, "lastRunOrdinal": 3, "handoffIds": [], "forkedFrom": null,
                "goal": goal(), "createdAt": ago(55), "updatedAt": ago(2)
            }],
            "providerTurns": [],
            "runtimeRequests": [{
                "id": "req-anchor", "nodeId": "node-run-3", "providerTurnId": null,
                "nativeRequestRef": null, "kind": "user_input", "status": "pending",
                "responseCapability": {"type": "message"},
                "createdAt": ago(2), "resolvedAt": null
            }],
            "messages": [
                message(1, "run-1", "Use ^anthropic/claude-opus-4-5 and ultrathink this: why does the feed jump while a long reply streams in? Screenshot attached.", 52),
                message(2, "run-2", "Also try it on the 240 Hz display.", 20),
                message(3, "run-3", "Keep the anchor, and orchestrate a bench run on both displays.", 6),
                message(4, "run-4", "Then write the changelog entry.", 1)
            ],
            "plans": [{
                "id": "plan-feed", "threadId": OPEN_THREAD, "runId": "run-1",
                "nodeId": "node-run-1", "status": "active", "kind": "todo_list",
                "steps": todo_steps(),
                "explanation": "Fix the jump, then guard it with a bench."
            }],
            "turnItems": items,
            "checkpointScopes": [], "checkpoints": [], "contextHandoffs": [],
            "contextTransfers": [],
            "visibleTurnItems": visible,
            "updatedAt": ago(1)
        }),
    )
}

#[cfg(test)]
mod tests {
    use j3_client::{
        t3::orchestration::TurnItemKind,
        workflow::{
            AgentStatus, T3Section, ThreadStatus, agent_status, t3_section, thread_status,
        },
    };

    use super::*;

    #[test]
    fn scene_names_match_the_scene_list() {
        let names: Vec<&str> = SCENES.iter().map(|(name, _)| *name).collect();
        let expected: Vec<&str> = Scene::ALL.iter().map(|scene| scene.name()).collect();
        assert_eq!(names, expected);
    }

    #[test]
    fn the_open_thread_covers_every_rendered_turn_item_kind() {
        let state = thread_state();
        let kinds: Vec<&str> = state
            .feed()
            .iter()
            .map(|row| match &row.item.kind {
                TurnItemKind::UserMessage(_) => "user_message",
                TurnItemKind::AssistantMessage(_) => "assistant_message",
                TurnItemKind::Reasoning(_) => "reasoning",
                TurnItemKind::TodoList(_) => "todo_list",
                TurnItemKind::UserInputRequest(_) => "user_input_request",
                TurnItemKind::FileChange(_) => "file_change",
                TurnItemKind::CommandExecution(_) => "command_execution",
                TurnItemKind::FileSearch(_) => "file_search",
                TurnItemKind::WebSearch(_) => "web_search",
                TurnItemKind::DynamicTool(_) => "dynamic_tool",
                TurnItemKind::RunInterruptRequest(_) => "run_interrupt_request",
                TurnItemKind::RunInterruptResult(_) => "run_interrupt_result",
                TurnItemKind::SystemNotice(_) => "system_notice",
                TurnItemKind::Error(_) => "error",
                TurnItemKind::Compaction(_) => "compaction",
                TurnItemKind::Subagent(_) => "subagent",
                TurnItemKind::Unknown(_) => "unknown",
            })
            .collect();
        for kind in [
            "user_message",
            "assistant_message",
            "reasoning",
            "todo_list",
            "user_input_request",
            "file_change",
            "command_execution",
            "file_search",
            "web_search",
            "dynamic_tool",
            "run_interrupt_result",
            "system_notice",
            "error",
            "compaction",
            "subagent",
        ] {
            assert!(kinds.contains(&kind), "the feed has a {kind}");
        }
        assert_eq!(state.queued_runs().len(), 1);
        assert_eq!(state.queued_runs()[0].text, "Then write the changelog entry.");
        let questions = state.pending_user_inputs();
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].questions.len(), 2);
        assert!(state.goal().is_some());
        assert_eq!(state.latest_todo().map(|todo| todo.steps.len()), Some(4));
        assert_eq!(state.active_run().map(|run| run.id.as_str()), Some("run-3"));
        assert_eq!(state.subagents().len(), 2);
    }

    #[test]
    fn the_sidebar_fixtures_span_every_status_and_section() {
        let local = local_state();
        let shieldy = shieldy_state();
        let threads: Vec<&ThreadShell> =
            local.shell.threads.iter().chain(&shieldy.shell.threads).collect();
        let statuses: Vec<ThreadStatus> = threads.iter().map(|t| thread_status(t)).collect();
        for status in [
            ThreadStatus::Working,
            ThreadStatus::Input,
            ThreadStatus::Approval,
            ThreadStatus::Failed,
            ThreadStatus::Limited,
            ThreadStatus::Ready,
        ] {
            assert!(statuses.contains(&status), "a thread is {status:?}");
        }
        let sections: Vec<Option<T3Section>> =
            threads.iter().map(|t| t3_section(t, NOW, true)).collect();
        for section in [
            T3Section::Pinned,
            T3Section::Active,
            T3Section::Working,
            T3Section::Snoozed,
            T3Section::Settled,
        ] {
            assert!(sections.contains(&Some(section)), "a thread is in {section:?}");
        }
        let agents: Vec<AgentStatus> = threads.iter().map(|t| agent_status(t)).collect();
        for status in [
            AgentStatus::Working,
            AgentStatus::Blocked,
            AgentStatus::Done,
            AgentStatus::Idle,
        ] {
            assert!(agents.contains(&status), "an agent is {status:?}");
        }
        assert_eq!(local.shell.projects.len() + shieldy.shell.projects.len(), 3);
        let config = local.config.as_ref().unwrap();
        let choices = crate::models::model_choices(config);
        let providers: Vec<&str> = crate::models::by_provider(&choices)
            .into_iter()
            .map(|(provider, _)| provider)
            .collect();
        assert_eq!(providers, ["anthropic", "openai"]);
    }
}
