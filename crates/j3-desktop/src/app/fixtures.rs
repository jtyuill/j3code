//! Offline fixtures and the window's named scenes (`--scene NAME`, `AppSpec::scenes`): two
//! live environments — the local one and the remote `shieldy` — each with its server info and
//! catalog, three projects and fifteen threads across every Inbox state (plus three archived
//! ones), and one open thread whose view holds every item kind the feed renders, all timed
//! relative to [`NOW`].
//!
//! The data is protocol v2 as a j3 server sends it (`docs/protocol/wire.md` §4): wire JSON
//! decoded through the protocol types, so a fixture that drifts from the wire fails to load.
//!
//! Nothing here touches the network: each environment runs over
//! [`Connection::offline`], whose calls fail at once, and `setup` opens no environments
//! when a scene installed these. Tests install a scene with [`install`]
//! (`TestApp::with_scene`).

use std::sync::Arc;

use j3_client::{
    Connection, ConnectionState, Info, Shell, Status, ThreadSync, ThreadView,
    entities::{Item, Project, Thread},
    methods::{BrowseEntry, Catalog},
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use time::{Duration, OffsetDateTime, macros::datetime};

use super::{Clock, J3, Main};
use crate::{
    dialogs::{Dialog, FolderListing, FolderSiblings},
    environment::{Environment, EnvironmentKind, Environments, LOCAL_KEY},
    environments::{DesktopConfig, RemoteEnvironment},
    local_server::LocalServerStatus,
    new_thread::NewThreadState,
    preferences::Preferences,
    settings::Page,
    sidebar::{ThreadKey, iso},
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
    /// The populated sidebar (Inbox workflow) and the empty main pane.
    Fixtures,
    /// [`OPEN_THREAD`] open.
    Thread,
    /// The new-thread screen on the local `j3code` project.
    NewThread,
    /// Settings on its General page.
    Settings,
    /// The herdr workflow's sidebar with [`OPEN_THREAD`] open.
    Herdr,
    /// The new-thread composer focused with a draft: a model chip, a magic keyword, a pasted
    /// image and ghost text.
    Composer,
    /// Settings on Archived threads (the local shell's three; shieldy has none).
    SettingsArchived,
    /// The Add project dialog over the sidebar, listing `~/repos/` on the local device.
    AddProject,
    /// The Custom snooze dialog for [`OPEN_THREAD`].
    CustomSnooze,
    /// [`OPEN_THREAD`] open with its agents hub showing.
    Hub,
    /// [`OPEN_THREAD`] open with its file pane (a fixture tree and preview) and git control.
    Files,
    /// [`OPEN_THREAD`] open with its git control's commit dialog.
    GitCommit,
    /// [`OPEN_THREAD`] open with its terminal drawer showing a coloured shell session.
    Terminal,
}

impl Scene {
    pub const ALL: [Scene; 13] = [
        Scene::Fixtures,
        Scene::Thread,
        Scene::NewThread,
        Scene::Settings,
        Scene::Herdr,
        Scene::Composer,
        Scene::SettingsArchived,
        Scene::AddProject,
        Scene::CustomSnooze,
        Scene::Hub,
        Scene::Files,
        Scene::GitCommit,
        Scene::Terminal,
    ];

    /// The `--scene` name.
    pub fn name(self) -> &'static str {
        match self {
            Scene::Fixtures => "fixtures",
            Scene::Thread => "thread",
            Scene::NewThread => "new-thread",
            Scene::Settings => "settings",
            Scene::Herdr => "herdr",
            Scene::Composer => "composer",
            Scene::SettingsArchived => "settings-archived",
            Scene::AddProject => "add-project",
            Scene::CustomSnooze => "custom-snooze",
            Scene::Hub => "hub",
            Scene::Files => "files",
            Scene::GitCommit => "git-commit",
            Scene::Terminal => "terminal",
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
    ("composer", |s: &mut J3| start(s, Scene::Composer)),
    ("settings-archived", |s: &mut J3| {
        start(s, Scene::SettingsArchived)
    }),
    ("add-project", |s: &mut J3| start(s, Scene::AddProject)),
    ("custom-snooze", |s: &mut J3| start(s, Scene::CustomSnooze)),
    ("hub", |s: &mut J3| start(s, Scene::Hub)),
    ("files", |s: &mut J3| start(s, Scene::Files)),
    ("git-commit", |s: &mut J3| start(s, Scene::GitCommit)),
    ("terminal", |s: &mut J3| start(s, Scene::Terminal)),
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
    s.envs.push(environment(
        LOCAL_KEY,
        LOCAL_LABEL,
        EnvironmentKind::Local(LocalServerStatus::Serving(vec![
            ([127, 0, 0, 1], 3780).into(),
        ])),
        Connection::offline(local_url, runtime.clone()),
        local_state(),
    ));
    s.envs.push(environment(
        SHIELDY_URL,
        "shieldy",
        EnvironmentKind::Remote,
        Connection::offline(SHIELDY_URL, runtime),
        shieldy_state(),
    ));
    match scene {
        Scene::Fixtures => {}
        Scene::Thread
        | Scene::Herdr
        | Scene::CustomSnooze
        | Scene::Hub
        | Scene::Files
        | Scene::GitCommit
        | Scene::Terminal => {
            let key = open_thread_key();
            if s.subscribe_thread(&key).is_some()
                && let Some(thread) = s.threads.get_mut(&key)
            {
                thread.state = Arc::new(thread_state());
            }
            if scene == Scene::Files {
                crate::files::fixtures::install(s, &key);
            }
            if matches!(scene, Scene::Files | Scene::GitCommit) {
                crate::git::fixtures::install(s, &key, scene == Scene::GitCommit);
            }
            if scene == Scene::Terminal {
                crate::terminal::fixture::install(s, &key);
            }
            s.route.main = Main::Thread(key.clone());
            if scene == Scene::CustomSnooze {
                s.dialogs.open = Some(Dialog::custom_snooze(key.clone()));
            }
            if scene == Scene::Hub {
                let hub = s.hubs.entry(key).or_default();
                hub.open = true;
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
        Scene::Composer => {
            install(s, Scene::NewThread);
            crate::composer::fixture::install(s);
        }
        Scene::SettingsArchived => {
            s.settings.page = Page::Archived;
            s.route.settings = true;
        }
        Scene::AddProject => s.dialogs.open = Some(add_project_dialog()),
    }
}

/// An environment over `connection` showing `state`, with the project slash commands its
/// catalog carries already fetched (so nothing asks the offline server for them).
fn environment(
    key: &str,
    label: &str,
    kind: EnvironmentKind,
    connection: Connection,
    state: ConnectionState,
) -> Environment {
    let commands = state
        .catalog
        .as_ref()
        .map(|catalog| catalog.project_commands.clone())
        .unwrap_or_default();
    let mut env = Environment::with_state(key, label, kind, connection, Arc::new(state), None);
    env.set_project_commands(commands);
    env
}

/// The Add project dialog on the local device with `~/repos/` listed.
fn add_project_dialog() -> Dialog {
    let mut dialog = Dialog::add_project(LOCAL_KEY.to_owned(), true);
    if let Dialog::AddProject(picker) = &mut dialog {
        picker.path.replace_text("~/repos/");
        picker.path.set_caret(picker.path.text.len());
        picker.title.replace_text("repos");
        let entry = |name: &str| BrowseEntry {
            name: name.to_owned(),
            path: format!("/home/jacob/repos/{name}"),
        };
        let entries: Vec<BrowseEntry> = [
            "dotfiles",
            "herdr",
            "j3code",
            "j3code-wt",
            "omp",
            "opencode",
            "rune",
        ]
        .map(entry)
        .into();
        picker.siblings = Some(FolderSiblings {
            dir: "~/repos/".into(),
            entries: Some(entries.clone()),
        });
        picker.listing = FolderListing::Loaded {
            parent: "/home/jacob/repos/".into(),
            entries,
        };
    }
    dialog
}

// ---------------------------------------------------------------------------
// Time and JSON
// ---------------------------------------------------------------------------

/// `minutes` before [`NOW`], as a wire timestamp.
pub fn ago(minutes: i64) -> String {
    iso(NOW - Duration::minutes(minutes))
}

/// `minutes` after [`NOW`], as a wire timestamp.
pub fn from_now(minutes: i64) -> String {
    iso(NOW + Duration::minutes(minutes))
}

/// `seconds` before [`NOW`], as a wire timestamp.
fn seconds_ago(seconds: i64) -> String {
    iso(NOW - Duration::seconds(seconds))
}

fn decode<T: DeserializeOwned>(what: &str, value: Value) -> T {
    serde_json::from_value(value).unwrap_or_else(|e| panic!("fixture {what} decodes: {e}"))
}

/// `base` with `patch` merged in: objects key by key, `null` removing a key, any other value
/// replacing it.
fn merged(mut base: Value, patch: Value) -> Value {
    merge(&mut base, patch);
    base
}

fn merge(base: &mut Value, patch: Value) {
    match (base, patch) {
        (Value::Object(base), Value::Object(patch)) => {
            for (key, value) in patch {
                if value.is_null() {
                    base.remove(&key);
                    continue;
                }
                if let Some(slot) = base.get_mut(&key)
                    && slot.is_object()
                    && value.is_object()
                {
                    merge(slot, value);
                    continue;
                }
                base.insert(key, value);
            }
        }
        (base, patch) => *base = patch,
    }
}

// ---------------------------------------------------------------------------
// Environments
// ---------------------------------------------------------------------------

/// The local environment: a live server with the `j3code` and `rune` projects.
pub fn local_state() -> ConnectionState {
    let projects = vec![
        project(
            J3CODE_PROJECT,
            "j3code",
            "/home/jacob/repos/j3code",
            60 * 24 * 40,
        ),
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
    server_id: &str,
    name: &str,
    projects: Vec<Project>,
    threads: Vec<Thread>,
) -> ConnectionState {
    let project_ids: Vec<String> = projects.iter().map(|p| p.id.clone()).collect();
    ConnectionState {
        status: Status::Connected,
        info: Some(Info {
            server_id: server_id.to_owned(),
            name: name.to_owned(),
            protocol: 2,
            version: "0.9.0".to_owned(),
            capabilities: ["terminal", "git", "files", "usage", "goals", "btw"]
                .map(String::from)
                .into(),
            platform: "linux".to_owned(),
            home: "/home/jacob".to_owned(),
        }),
        catalog: Some(catalog(&project_ids)),
        shell: Shell {
            sequence: 4_210,
            synced: true,
            projects: projects.into_iter().map(|p| (p.id.clone(), p)).collect(),
            threads: threads.into_iter().map(|t| (t.id.clone(), t)).collect(),
        },
    }
}

fn project(id: &str, title: &str, path: &str, created_minutes_ago: i64) -> Project {
    decode(
        "project",
        json!({
            "id": id, "title": title, "path": path,
            "createdAt": ago(created_minutes_ago), "updatedAt": ago(created_minutes_ago)
        }),
    )
}

/// The catalog j3 sends: models from Anthropic and OpenAI (grouped by provider in the
/// picker), with thinking levels, slash commands and skills. `projects` have had their own
/// slash commands fetched (they have none beyond omp's).
pub fn catalog(projects: &[String]) -> Catalog {
    let levels = json!(["off", "low", "medium", "high"]);
    let openai_levels = json!(["off", "minimal", "low", "medium", "high"]);
    let project_commands: serde_json::Map<String, Value> =
        projects.iter().map(|id| (id.clone(), json!([]))).collect();
    decode(
        "catalog",
        json!({
            "models": [
                {"provider": "anthropic", "id": "claude-opus-4-5", "name": "Claude Opus 4.5",
                 "thinking": levels, "default": true},
                {"provider": "anthropic", "id": "claude-sonnet-4-5", "name": "Claude Sonnet 4.5",
                 "thinking": levels},
                {"provider": "anthropic", "id": "claude-haiku-4-5", "name": "Claude Haiku 4.5"},
                {"provider": "openai", "id": "gpt-5.1", "name": "GPT-5.1",
                 "thinking": openai_levels},
                {"provider": "openai", "id": "gpt-5.1-codex", "name": "GPT-5.1 Codex",
                 "thinking": openai_levels},
                {"provider": "openai", "id": "gpt-5-mini", "name": "GPT-5 mini"}
            ],
            "skills": [
                {"name": "unslop", "description": "Rewrite text so it reads like a person wrote it"},
                {"name": "visualize", "description": "Turn an idea into a diagram or plot"},
                {"name": "reverse-engineer-anything",
                 "description": "Investigate binaries, firmware and runtime evidence"}
            ],
            "commands": [
                {"name": "compact", "description": "Summarize the conversation to free context",
                 "hint": "focus instructions"},
                {"name": "model", "description": "Switch the model for this thread"},
                {"name": "plan", "description": "Draft a plan before making changes"},
                {"name": "review", "description": "Review the working tree changes",
                 "hint": "base branch"},
                {"name": "goal", "description": "Set a goal the agent works towards",
                 "hint": "objective"}
            ],
            "projectCommands": project_commands,
            "status": "ready"
        }),
    )
}

// ---------------------------------------------------------------------------
// Thread rows
// ---------------------------------------------------------------------------

fn opus() -> Value {
    json!({"provider": "anthropic", "id": "claude-opus-4-5", "thinking": "high"})
}

fn codex() -> Value {
    json!({"provider": "openai", "id": "gpt-5.1-codex", "thinking": "high"})
}

/// A thread row created `created` minutes ago whose last turn completed three minutes later
/// and was seen, with `patch` merged in.
fn row(id: &str, project: &str, title: &str, created: i64, patch: Value) -> Thread {
    let base = json!({
        "id": id, "projectId": project, "title": title,
        "createdAt": ago(created), "updatedAt": ago(created - 3),
        "status": "idle", "model": opus(), "mode": "default", "queued": 0,
        "lastMessage": {"role": "assistant", "text": "Done. The changes are in the working tree.",
                        "at": ago(created - 3)},
        "latestTurn": {"id": format!("turn-{id}"), "status": "completed",
                       "requestedAt": ago(created), "startedAt": ago(created),
                       "endedAt": ago(created - 3)},
        "latestUserMessageAt": ago(created),
        "triage": {"visitedAt": ago(created - 4), "bumpedAt": ago(created - 3)}
    });
    decode("thread row", merged(base, patch))
}

/// A turn that started `minutes` ago and is still running.
fn running(id: &str, minutes: i64) -> Value {
    json!({
        "status": "running", "activeTurnId": format!("turn-{id}"),
        "latestTurn": {"status": "running", "requestedAt": ago(minutes),
                       "startedAt": ago(minutes), "endedAt": null},
        "latestUserMessageAt": ago(minutes),
        "lastMessage": {"role": "user", "text": "Go ahead.", "at": ago(minutes)},
        "updatedAt": ago(0),
        "triage": {"visitedAt": ago(minutes), "bumpedAt": ago(minutes)}
    })
}

/// A latest turn that ended `minutes` ago, seen `visited` minutes ago.
fn finished(minutes: i64, visited: i64) -> Value {
    json!({
        "latestTurn": {"endedAt": ago(minutes)},
        "lastMessage": {"at": ago(minutes)},
        "updatedAt": ago(minutes),
        "triage": {"visitedAt": ago(visited), "bumpedAt": ago(minutes)}
    })
}

/// A latest turn that failed `minutes` ago with `kind`, seen `visited` minutes ago.
fn failed(minutes: i64, visited: i64, kind: &str) -> Value {
    merged(
        finished(minutes, visited),
        json!({"status": "failed", "latestTurn": {"status": "failed", "errorKind": kind}}),
    )
}

/// The open thread's last message: a steer into its running turn.
const STEER: &str = "Run the 240 Hz pass on the external monitor, not the laptop panel.";
const OPEN_TITLE: &str = "Port the feed renderer to Rune";

fn local_threads() -> Vec<Thread> {
    let j3 = J3CODE_PROJECT;
    let rune = "prj-rune";
    let archived = |minutes: i64| json!({"archivedAt": ago(minutes)});
    vec![
        // Waiting on the user's answer, with a queued message and a goal (the open thread).
        row(
            OPEN_THREAD,
            j3,
            OPEN_TITLE,
            55,
            json!({
                "status": "waiting", "model": codex(), "activeTurnId": "turn-3",
                "pendingRequest": {"id": "req-anchor", "kind": "ask", "openedAt": ago(2)},
                "queued": 1,
                "lastMessage": {"role": "user", "text": STEER, "at": ago(3)},
                "latestTurn": {"id": "turn-3", "status": "waiting", "requestedAt": ago(6),
                               "startedAt": ago(6), "endedAt": null},
                "latestUserMessageAt": ago(3),
                "goal": goal(),
                "updatedAt": ago(1),
                "triage": {"visitedAt": ago(2), "bumpedAt": ago(1)}
            }),
        ),
        // Working.
        row(
            "thr-sidebar-dnd",
            j3,
            "Sidebar drag and drop between shelves",
            48,
            merged(running("thr-sidebar-dnd", 4), json!({"model": codex()})),
        ),
        // Failed, not seen since.
        row(
            "thr-dictation",
            j3,
            "Dictation drops the first word",
            95,
            failed(9, 30, "crashed"),
        ),
        // Pinned, finished and seen.
        row(
            "thr-release",
            j3,
            "Release checklist for 0.9",
            60 * 24 * 3,
            merged(
                finished(180, 170),
                json!({"triage": {"pinnedAt": ago(60 * 24 * 3), "pinOrder": "a0"}}),
            ),
        ),
        // Finished and unread.
        row(
            "thr-math",
            j3,
            "Typeset display math in markdown",
            70,
            finished(12, 40),
        ),
        // Settled.
        row(
            "thr-zoom",
            j3,
            "Zoom shortcuts on Linux",
            60 * 24 * 2 + 30,
            json!({"triage": {"settled": "settled", "settledAt": ago(60 * 24 * 2)}}),
        ),
        // Snoozed for three hours.
        row(
            "thr-notarize",
            j3,
            "Follow up on the notarization ticket",
            75,
            json!({"triage": {"snoozedUntil": from_now(180), "snoozedAt": ago(60)}}),
        ),
        // Working, on GPT-5.1 Codex.
        row(
            "thr-scroll",
            rune,
            "Momentum scrolling stutters at 120 Hz",
            40,
            merged(running("thr-scroll", 15), json!({"model": codex()})),
        ),
        // Finished a day ago and seen.
        row(
            "thr-tokens",
            rune,
            "Theme tokens from the opencode kit",
            60 * 26,
            json!({}),
        ),
        // Stopped by a usage limit.
        row(
            "thr-layout",
            rune,
            "Refactor the flex layout pass",
            130,
            failed(20, 18, "usage_limit"),
        ),
        // Archived: newest archived first, recent, week, old.
        row(
            "thr-archived-week",
            j3,
            "Try GPUI's text system for the feed",
            60 * 24 * 9,
            archived(60 * 24 * 7),
        ),
        row(
            "thr-archived-old",
            rune,
            "Sketch the settings layout",
            60 * 24 * 40,
            archived(60 * 24 * 30),
        ),
        row(
            "thr-archived-recent",
            j3,
            "Profile the sidebar's drag reordering",
            60 * 5,
            archived(90),
        ),
    ]
}

fn shieldy_threads() -> Vec<Thread> {
    let infra = "prj-infra";
    vec![
        // Working.
        row(
            "thr-backup",
            infra,
            "Nightly backup restore drill",
            30,
            running("thr-backup", 22),
        ),
        // Waiting on a confirmation.
        row(
            "thr-acl",
            infra,
            "Tailscale ACL for the new runners",
            35,
            json!({
                "status": "waiting", "activeTurnId": "turn-thr-acl",
                "pendingRequest": {"id": "req-acl", "kind": "confirm",
                                   "title": "Apply the ACL change?", "openedAt": ago(5)},
                "latestTurn": {"status": "waiting", "requestedAt": ago(8),
                               "startedAt": ago(8), "endedAt": null},
                "latestUserMessageAt": ago(8),
                "updatedAt": ago(5),
                "triage": {"visitedAt": ago(8), "bumpedAt": ago(5)}
            }),
        ),
        // Finished and unread.
        row(
            "thr-caddy",
            infra,
            "Caddy TLS renewal failing",
            50,
            merged(
                finished(25, 45),
                json!({"model": {"provider": "openai", "id": "gpt-5.1", "thinking": "medium"}}),
            ),
        ),
        // Settled.
        row(
            "thr-grafana",
            infra,
            "Grafana dashboard for omp latency",
            60 * 24 * 5,
            json!({"triage": {"settled": "settled", "settledAt": ago(60 * 24 * 4)}}),
        ),
        // Snoozed until tomorrow morning.
        row(
            "thr-disk",
            infra,
            "Disk pressure alerts on shieldy",
            200,
            json!({"triage": {"snoozedUntil": from_now(16 * 60 + 30), "snoozedAt": ago(150)}}),
        ),
    ]
}

fn goal() -> Value {
    json!({"objective": "Feed renders every turn item kind on Rune with no scroll jumps",
           "state": "active", "tokensUsed": 48_210, "budget": 200_000})
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

const ANCHOR_RS: &str = "//! Keeps the feed still while rows above the viewport change height.\n\n/// The row the viewport is pinned to and its offset from the top.\npub struct Anchor {\n    pub row: usize,\n    pub offset: f32,\n}\n\n/// Keeps `anchor` at the same screen offset across a relayout.\npub fn hold(anchor: &Anchor, before: f32, after: f32) -> f32 {\n    let drift = after - before;\n    anchor.offset - drift\n}\n";

/// The open thread's live state: a feed with every rendered item kind, a turn waiting on a
/// two-question `ask`, a queued message, a goal and an active todo list.
pub fn thread_state() -> ThreadView {
    let thread = local_threads()
        .into_iter()
        .find(|thread| thread.id == OPEN_THREAD)
        .expect("the open thread's row");
    ThreadView {
        thread_id: OPEN_THREAD.to_owned(),
        sync: ThreadSync::Synced,
        sequence: 312,
        thread: Some(thread),
        turns: decode(
            "turns",
            json!([
                turn("turn-1", 1, "completed", opus(), 52, Some(27)),
                turn("turn-2", 2, "stopped", codex(), 20, Some(18)),
                turn("turn-3", 3, "waiting", codex(), 6, None)
            ]),
        ),
        items: feed_items(),
        requests: vec![decode("request", request())],
        queue: vec![decode(
            "queue entry",
            json!({
                "id": "queue-1", "threadId": OPEN_THREAD, "position": 1,
                "text": "Then write the changelog entry.", "model": codex(),
                "createdAt": ago(1)
            }),
        )],
    }
}

fn turn(
    id: &str,
    ordinal: u32,
    status: &str,
    model: Value,
    requested: i64,
    ended: Option<i64>,
) -> Value {
    let mut turn = json!({
        "id": id, "threadId": OPEN_THREAD, "ordinal": ordinal, "status": status,
        "model": model, "requestedAt": ago(requested), "startedAt": ago(requested)
    });
    if let Some(ended) = ended {
        turn["endedAt"] = json!(ago(ended));
    }
    turn
}

/// The open `ask` of turn 3: two questions, the second multiple choice.
fn request() -> Value {
    json!({
        "id": "req-anchor", "threadId": OPEN_THREAD, "turnId": "turn-3",
        "kind": "ask", "status": "open", "openedAt": ago(2),
        "questions": [
            {"id": "anchor", "header": "Anchor",
             "text": "Where should the feed stay anchored while a reply streams?",
             "options": [
                 {"label": "Bottom", "description": "Follow new output as it arrives (recommended)"},
                 {"label": "Top of the reply", "description": "Keep the start of the reply in view"},
                 {"label": "Where I left it", "description": "Never scroll on its own"}
             ],
             "allowOther": true},
            {"id": "displays", "header": "Displays",
             "text": "Which refresh rates should the bench cover?",
             "options": [
                 {"label": "60 Hz", "description": "The laptop panel"},
                 {"label": "120 Hz", "description": "ProMotion"},
                 {"label": "240 Hz", "description": "The external monitor"}
             ],
             "multiple": true}
        ]
    })
}

/// One feed item of the open thread: `(turn, status, seconds before NOW it started, seconds
/// long (None while running), kind and its fields)`.
type FeedItem = (&'static str, &'static str, i64, Option<i64>, Value);

/// A tool item's fields: `name`, omp's intent (also its `i` argument), its arguments and
/// output.
fn tool(call: &str, name: &str, intent: Option<&str>, mut input: Value, output: Value) -> Value {
    if let Some(intent) = intent {
        input["i"] = json!(intent);
    }
    let mut tool = json!({"kind": "tool", "callId": call, "name": name, "input": input});
    if let Some(intent) = intent {
        tool["intent"] = json!(intent);
    }
    tool["output"] = output;
    tool
}

/// `tool` with a structured `detail`.
fn tool_with(
    call: &str,
    name: &str,
    intent: Option<&str>,
    input: Value,
    output: Value,
    detail: Value,
) -> Value {
    let mut tool = tool(call, name, intent, input, output);
    tool["detail"] = detail;
    tool
}

/// The open thread's feed items, in order.
fn feed_items() -> Vec<Item> {
    const M: i64 = 60;
    let screenshot = json!({"id": "att-feed-jump", "name": "feed-jump.png",
                            "mimeType": "image/png", "size": 184_320});
    let items: Vec<FeedItem> = vec![
        // Turn 1: the investigation, completed.
        (
            "turn-1",
            "completed",
            52 * M,
            Some(0),
            json!({
                "kind": "user", "delivery": "prompt", "attachments": [screenshot],
                "text": "Use ^anthropic/claude-opus-4-5 and ultrathink this: why does the feed jump while a long reply streams in? Screenshot attached."
            }),
        ),
        (
            "turn-1",
            "completed",
            52 * M - 2,
            Some(18),
            json!({
                "kind": "thinking",
                "text": "The jump lines up with each streamed chunk. The virtual list keeps its scroll offset fixed, but the rows **above** the viewport are remeasured when the reply grows, so the content under the viewport shifts.\n\nCheck where `remeasure_items` runs and whether the anchor is the first or last visible row."
            }),
        ),
        (
            "turn-1",
            "completed",
            51 * M,
            Some(1),
            tool_with(
                "call-grep",
                "grep",
                Some("Finding where rows are remeasured"),
                json!({"pattern": "remeasure", "path": "crates/j3-desktop/src"}),
                json!({"text": "# crates/j3-desktop/src/\n## thread/mod.rs#6163\n*142:        self.remeasure(&changed);\n*188:    fn remeasure(&mut self, rows: &[usize]) {\n## feed.rs#1F0A\n*61:/// Rows whose item changed are remeasured; the rest keep their height."}),
                json!({"type": "search", "pattern": "remeasure", "matches": [
                    {"path": "crates/j3-desktop/src/thread/mod.rs", "line": 142,
                     "text": "        self.remeasure(&changed);"},
                    {"path": "crates/j3-desktop/src/thread/mod.rs", "line": 188,
                     "text": "    fn remeasure(&mut self, rows: &[usize]) {"},
                    {"path": "crates/j3-desktop/src/feed.rs", "line": 61,
                     "text": "/// Rows whose item changed are remeasured; the rest keep their height."}
                ]}),
            ),
        ),
        (
            "turn-1",
            "completed",
            50 * M,
            Some(1),
            tool(
                "call-read",
                "read",
                Some("Reading the thread view"),
                json!({"path": "crates/j3-desktop/src/thread/mod.rs:125-131"}),
                json!({"text": "[crates/j3-desktop/src/thread/mod.rs#6163]\n125:pub fn view(s: &J3, cx: &mut Cx<J3>, ui: &mut Ui, key: &ThreadKey) -> El {\n126:    let k = ui.k;\n127:    let note = |message: String| {\n128:        el().key(\"feed-note\")\n129:            .w_full()\n130:            .grow(1.0)\n131:            .center()"}),
            ),
        ),
        (
            "turn-1",
            "completed",
            49 * M,
            Some(3),
            tool_with(
                "call-web",
                "web_search",
                Some("Searching for scroll anchoring"),
                json!({"query": "virtual list scroll anchoring streaming content"}),
                json!({"text": "Scroll anchoring keeps the visible content stable when content above it changes size: anchor to the last fully visible message and correct the offset after layout."}),
                json!({"type": "web_search", "query": "virtual list scroll anchoring streaming content",
                       "results": [
                    {"title": "CSS Scroll Anchoring", "url": "https://drafts.csswg.org/css-scroll-anchoring/",
                     "snippet": "Scroll anchoring aims to keep the visible content stable when content above it changes size."},
                    {"title": "Building a chat list that does not jump", "url": "https://blog.example.dev/chat-scroll",
                     "snippet": "Anchor to the last fully visible message and correct the offset after layout."},
                    {"title": "Virtualized lists and dynamic heights", "url": "https://github.com/anomalyco/rune/discussions/412",
                     "snippet": "Measure once, then correct by the delta of rows above the viewport."}
                ]}),
            ),
        ),
        (
            "turn-1",
            "completed",
            48 * M,
            Some(4),
            json!({
                "kind": "notice", "level": "warning",
                "text": "overloaded_error: Overloaded (529). Retrying in 4 s, attempt 2 of 5."
            }),
        ),
        (
            "turn-1",
            "completed",
            46 * M,
            Some(38),
            tool_with(
                "call-test",
                "bash",
                Some("Running the feed tests"),
                json!({"command": "cargo test -p j3-desktop feed"}),
                json!({"exitCode": 0, "text": "   Compiling j3-desktop v0.9.0 (/home/jacob/repos/j3code/crates/j3-desktop)\n    Finished `test` profile [unoptimized + debuginfo] target(s) in 31.84s\n     Running unittests src/lib.rs (target/debug/deps/j3_desktop-5c1f0e2a9b7d4e31)\n\nrunning 14 tests\ntest feed::tests::ages_are_compact ... ok\ntest feed::tests::diff_lines_split_omp_and_unified_forms ... ok\ntest feed::tests::dynamic_tools_get_their_omp_titles ... ok\ntest feed::tests::edits_writes_and_searches_read_like_omp ... ok\n\ntest result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 212 filtered out; finished in 0.04s\n"}),
                json!({"type": "command", "command": "cargo test -p j3-desktop feed"}),
            ),
        ),
        (
            "turn-1",
            "completed",
            45 * M,
            Some(24),
            tool_with(
                "call-clippy",
                "bash",
                Some("Linting with clippy"),
                json!({"command": "cargo clippy -p j3-desktop --all-targets -- -D warnings"}),
                json!({"exitCode": 101, "text": "    Checking j3-desktop v0.9.0 (/home/jacob/repos/j3code/crates/j3-desktop)\nerror: unused variable: `delta`\n  --> crates/j3-desktop/src/thread/anchor.rs:18:9\n   |\n18 |     let delta = after - before;\n   |         ^^^^^ help: if this is intentional, prefix it with an underscore: `_delta`\n   |\n   = note: `-D unused-variables` implied by `-D warnings`\n\nerror: could not compile `j3-desktop` (lib) due to 1 previous error\n"}),
                json!({"type": "command", "command": "cargo clippy -p j3-desktop --all-targets -- -D warnings"}),
            ),
        ),
        (
            "turn-1",
            "completed",
            43 * M,
            Some(1),
            tool_with(
                "call-edit-scroll",
                "edit",
                None,
                json!({"input": "[crates/j3-desktop/src/thread/mod.rs#6163]\nPUT 140.=141:\n+        let anchor = self.scroll.last_visible();\n+        let before = self.scroll.offset_of(anchor);\n+        self.remeasure(&changed);\n+        let after = self.scroll.offset_of(anchor);\n+        self.scroll.shift(anchor::hold(&anchor, before, after));\n+        self.scroll.settle();\n"}),
                json!({"text": "Applied 1 edit to crates/j3-desktop/src/thread/mod.rs"}),
                json!({"type": "file_change", "path": "crates/j3-desktop/src/thread/mod.rs",
                       "additions": 6, "deletions": 2,
                       "diff": " 139|        let changed = self.changed_rows(&state);\n-140|        self.remeasure(&changed);\n-141|        self.scroll.keep_offset();\n+140|        let anchor = self.scroll.last_visible();\n+141|        let before = self.scroll.offset_of(anchor);\n+142|        self.remeasure(&changed);\n+143|        let after = self.scroll.offset_of(anchor);\n+144|        self.scroll.shift(anchor::hold(&anchor, before, after));\n+145|        self.scroll.settle();\n 146|    }"}),
            ),
        ),
        (
            "turn-1",
            "completed",
            42 * M,
            Some(1),
            tool_with(
                "call-write",
                "write",
                None,
                json!({"path": "crates/j3-desktop/src/thread/anchor.rs", "content": ANCHOR_RS}),
                json!({"text": "Wrote crates/j3-desktop/src/thread/anchor.rs (13 lines)"}),
                json!({"type": "file_change", "path": "crates/j3-desktop/src/thread/anchor.rs",
                       "content": ANCHOR_RS}),
            ),
        ),
        (
            "turn-1",
            "completed",
            41 * M,
            Some(1),
            tool_with(
                "call-edit-mod",
                "edit",
                None,
                json!({"input": "[crates/j3-desktop/src/thread/mod.rs#6163]\nPUT <8:\n+mod anchor;\n"}),
                json!({"text": "Applied 1 edit to crates/j3-desktop/src/thread/mod.rs"}),
                json!({"type": "file_change", "path": "crates/j3-desktop/src/thread/mod.rs",
                       "additions": 1, "deletions": 0, "diff": "+8|mod anchor;"}),
            ),
        ),
        (
            "turn-1",
            "completed",
            40 * M,
            Some(2),
            tool(
                "call-eval",
                "eval",
                None,
                json!({"language": "py", "title": "Comparing frame times",
                       "code": "before = [6.9, 6.7, 6.8]\nafter = [2.2, 2.0, 2.1]\nprint(f\"{sum(before)/3:.1f} ms -> {sum(after)/3:.1f} ms\")"}),
                json!({"text": "6.8 ms -> 2.1 ms"}),
            ),
        ),
        (
            "turn-1",
            "completed",
            40 * M - 30,
            Some(1),
            tool(
                "call-plot",
                "read",
                Some("Viewing the frame-time plot"),
                json!({"path": "benches/out/frame-times.png"}),
                json!({"text": "Read image file [image/png]",
                       "images": [{"attachmentId": "att-frame-times", "mimeType": "image/png"}]}),
            ),
        ),
        (
            "turn-1",
            "completed",
            39 * M,
            Some(0),
            json!({"kind": "todo", "phases": [
                {"name": "Fix the jump", "tasks": [
                    {"text": "Find where rows are remeasured", "status": "completed"},
                    {"text": "Anchor the scroll to the last visible row", "status": "completed"}
                ]},
                {"name": "Guard it with a bench", "tasks": [
                    {"text": "Bench frame time while streaming", "status": "in_progress"},
                    {"text": "Write the changelog entry", "status": "pending"}
                ]}
            ]}),
        ),
        (
            "turn-1",
            "completed",
            38 * M,
            Some(2),
            tool(
                "call-task",
                "task",
                None,
                json!({"agent": "task", "tasks": [
                    {"name": "AuditScroll", "task": "Find every place the feed changes its scroll offset"},
                    {"name": "ProfileGpu", "task": "Profile GPU time per frame while a reply streams"}
                ]}),
                json!({"text": "Started 2 agents: AuditScroll, ProfileGpu"}),
            ),
        ),
        (
            "turn-1",
            "completed",
            37 * M,
            Some(140),
            json!({
                "kind": "subagent", "subagentId": "sa-audit", "agent": "task", "description": "Audit scroll writers",
                "model": "anthropic/claude-sonnet-4-5",
                "prompt": "Find every place the feed changes its scroll offset and list them with file:line.",
                "progress": "Checked 9 call sites",
                "result": "Three writers: `thread/mod.rs:144` (anchor shift), `thread/mod.rs:201` (jump to bottom on send) and `rune_kit::MessageScroller::follow`. Only the first runs while streaming."
            }),
        ),
        (
            "turn-1",
            "failed",
            37 * M,
            Some(45),
            json!({
                "kind": "subagent", "subagentId": "sa-profile", "agent": "task", "description": "ProfileGpu",
                "model": "anthropic/claude-sonnet-4-5",
                "prompt": "Profile GPU time per frame while a 4,000-line reply streams.",
                "progress": "Opening the GPU counters",
                "result": "GPU counters are not readable over SSH on this machine."
            }),
        ),
        (
            "turn-1",
            "completed",
            34 * M,
            Some(2),
            tool(
                "call-issues",
                "mcp__github__search_issues",
                None,
                json!({"query": "scroll jump is:open repo:anomalyco/rune"}),
                json!({"text": "#412 Virtualized lists and dynamic heights (open)\n#398 MessageScroller jumps when a row above grows (open)"}),
            ),
        ),
        (
            "turn-1",
            "completed",
            31 * M,
            Some(25),
            json!({"kind": "text", "text": ANSWER}),
        ),
        (
            "turn-1",
            "completed",
            29 * M,
            Some(0),
            json!({"kind": "notice", "level": "info",
                   "text": "Model changed to GPT-5.1 Codex · thinking high"}),
        ),
        (
            "turn-1",
            "completed",
            28 * M,
            Some(12),
            json!({
                "kind": "compaction",
                "summary": "Investigated the feed jump, anchored the scroll to the last visible row and added a bench.",
                "tokensBefore": 182_340
            }),
        ),
        // Turn 2: stopped by the user.
        (
            "turn-2",
            "completed",
            20 * M,
            Some(0),
            json!({"kind": "user", "delivery": "prompt",
                   "text": "Also try it on the 240 Hz display."}),
        ),
        (
            "turn-2",
            "cancelled",
            19 * M,
            Some(40),
            json!({"kind": "thinking",
                   "text": "The 240 Hz display is on the other machine; I need the bench binary there first."}),
        ),
        (
            "turn-2",
            "completed",
            18 * M,
            Some(0),
            json!({"kind": "stop", "requestedAt": ago(18), "stoppedAt": ago(18)}),
        ),
        // Turn 3: active, waiting on the user's answer.
        (
            "turn-3",
            "completed",
            6 * M,
            Some(0),
            json!({"kind": "user", "delivery": "prompt",
                   "text": "Keep the anchor, and orchestrate a bench run on both displays."}),
        ),
        (
            "turn-3",
            "completed",
            6 * M - 5,
            Some(0),
            merged(json!({"kind": "goal"}), goal()),
        ),
        (
            "turn-3",
            "running",
            5 * M,
            None,
            json!({
                "kind": "subagent", "subagentId": "sa-bench", "agent": "task", "description": "BenchFeed",
                "model": "anthropic/claude-sonnet-4-5",
                "prompt": "Measure frame time while a 4,000-line reply streams, at 60, 120 and 240 Hz.",
                "progress": "120 Hz: 2.1 ms median, running 240 Hz"
            }),
        ),
        (
            "turn-3",
            "running",
            4 * M,
            None,
            tool_with(
                "call-bench",
                "bash",
                Some("Building the bench for shieldy"),
                json!({"command": "cargo build --release -p j3-desktop --benches"}),
                json!({"text": "   Compiling rune v0.9.0\n   Compiling rune-kit v0.9.0\n   Compiling j3-client v0.9.0 (/home/jacob/repos/j3code/crates/j3-client)\n   Compiling j3-desktop v0.9.0 (/home/jacob/repos/j3code/crates/j3-desktop)\n"}),
                json!({"type": "command", "command": "cargo build --release -p j3-desktop --benches"}),
            ),
        ),
        (
            "turn-3",
            "completed",
            3 * M,
            Some(0),
            json!({"kind": "user", "delivery": "steer", "text": STEER}),
        ),
    ];
    items
        .into_iter()
        .enumerate()
        .map(|(ix, (turn, status, started, seconds, kind))| {
            let mut item = json!({
                "id": format!("item-{}", ix + 1), "threadId": OPEN_THREAD, "turnId": turn,
                "ordinal": ix + 1, "status": status, "startedAt": seconds_ago(started)
            });
            if let Some(seconds) = seconds {
                item["endedAt"] = json!(seconds_ago(started - seconds));
            }
            decode("feed item", merged(item, kind))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use j3_client::{
        EnvThread,
        entities::{Delivery, ItemKind, ItemStatus, ToolDetail},
        herdr::{AgentStatus, agent_status},
        inbox::{self, Section},
    };

    use super::*;

    #[test]
    fn scene_names_match_the_scene_list() {
        let names: Vec<&str> = SCENES.iter().map(|(name, _)| *name).collect();
        let expected: Vec<&str> = Scene::ALL.iter().map(|scene| scene.name()).collect();
        assert_eq!(names, expected);
    }

    #[test]
    fn the_open_thread_covers_every_rendered_item_kind() {
        let state = thread_state();
        let kinds: Vec<&str> = state
            .items
            .iter()
            .map(|item| match &item.kind {
                ItemKind::User(user) if user.delivery == Delivery::Steer => "steer",
                ItemKind::User(user) if !user.attachments.is_empty() => "user with image",
                ItemKind::User(_) => "user",
                ItemKind::Text(_) => "text",
                ItemKind::Thinking(_) => "thinking",
                ItemKind::Tool(tool) => match &tool.detail {
                    Some(ToolDetail::Command { .. }) => "command",
                    Some(ToolDetail::FileChange { diff: Some(_), .. }) => "edit",
                    Some(ToolDetail::FileChange {
                        content: Some(_), ..
                    }) => "write",
                    Some(ToolDetail::FileChange { .. }) => "file change",
                    Some(ToolDetail::Search { .. }) => "search",
                    Some(ToolDetail::WebSearch { .. }) => "web search",
                    Some(ToolDetail::Unknown) => "unknown tool",
                    None if tool.output.as_ref().is_some_and(|o| !o.images.is_empty()) => {
                        "image output"
                    }
                    None => "plain tool",
                },
                ItemKind::Todo(_) => "todo",
                ItemKind::Subagent(_) => "subagent",
                ItemKind::Goal(_) => "goal",
                ItemKind::Compaction(_) => "compaction",
                ItemKind::Notice(_) => "notice",
                ItemKind::Stop(_) => "stop",
                ItemKind::Unknown => "unknown",
            })
            .collect();
        for kind in [
            "user with image",
            "user",
            "steer",
            "text",
            "thinking",
            "command",
            "edit",
            "write",
            "search",
            "web search",
            "image output",
            "plain tool",
            "todo",
            "subagent",
            "goal",
            "compaction",
            "notice",
            "stop",
        ] {
            assert!(kinds.contains(&kind), "the feed has a {kind}");
        }
        assert!(!kinds.contains(&"unknown"));
        assert_eq!(state.queue.len(), 1);
        assert_eq!(state.queue[0].text, "Then write the changelog entry.");
        let requests: Vec<_> = state.open_requests().collect();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].questions.len(), 2);
        assert!(requests[0].questions[1].multiple);
        let thread = state.thread.as_ref().unwrap();
        assert_eq!(thread.title, OPEN_TITLE);
        assert!(thread.goal.is_some());
        assert_eq!(
            thread.pending_request.as_ref().map(|r| r.id.as_str()),
            Some(requests[0].id.as_str())
        );
        let (_, todo) = state.latest_todo().unwrap();
        let tasks: Vec<_> = todo.phases.iter().flat_map(|phase| &phase.tasks).collect();
        assert_eq!(tasks.len(), 4);
        assert_eq!(
            state.active_turn().map(|turn| turn.id.as_str()),
            Some("turn-3")
        );
        let subagents: Vec<ItemStatus> = state.subagents().map(|(item, _)| item.status).collect();
        assert_eq!(
            subagents,
            [
                ItemStatus::Completed,
                ItemStatus::Failed,
                ItemStatus::Running
            ]
        );
        let mut ordinals: Vec<u64> = state.items.iter().map(|item| item.ordinal).collect();
        ordinals.dedup();
        assert_eq!(ordinals.len(), state.items.len());
    }

    #[test]
    fn the_sidebar_fixtures_span_every_status_and_section() {
        let local = local_state();
        let shieldy = shieldy_state();
        let threads: Vec<&Thread> = local
            .shell
            .threads
            .values()
            .chain(shieldy.shell.threads.values())
            .collect();
        let archived: Vec<&str> = threads
            .iter()
            .filter(|thread| !inbox::is_listed(thread))
            .map(|thread| thread.id.as_str())
            .collect();
        assert_eq!(archived.len(), 3);
        let listed: Vec<&Thread> = threads
            .into_iter()
            .filter(|thread| inbox::is_listed(thread))
            .collect();
        assert_eq!(listed.len(), 15);

        let statuses: Vec<inbox::Status> = listed.iter().map(|t| inbox::status(t)).collect();
        for status in [
            inbox::Status::Input,
            inbox::Status::Working,
            inbox::Status::Limited,
            inbox::Status::Failed,
            inbox::Status::Ready,
        ] {
            assert!(statuses.contains(&status), "a thread is {status:?}");
        }
        assert!(
            listed.iter().any(|t| inbox::is_unread(t)),
            "a thread is unread"
        );
        let sections: Vec<Section> = listed
            .iter()
            .map(|t| inbox::section(t, NOW, true, true))
            .collect();
        for section in [
            Section::Pinned,
            Section::Active,
            Section::Working,
            Section::Snoozed,
            Section::Settled,
        ] {
            assert!(sections.contains(&section), "a thread is in {section:?}");
        }
        let agents: Vec<AgentStatus> = listed.iter().map(|t| agent_status(t)).collect();
        for status in [
            AgentStatus::Working,
            AgentStatus::Blocked,
            AgentStatus::Done,
            AgentStatus::Idle,
        ] {
            assert!(agents.contains(&status), "an agent is {status:?}");
        }
        let layout = inbox::layout(
            listed
                .iter()
                .map(|thread| EnvThread {
                    environment: LOCAL_KEY,
                    thread,
                })
                .collect(),
            NOW,
            true,
            |_| true,
        );
        assert_eq!(layout.pinned.len(), 1);
        assert!(layout.next_wake_at.is_some());

        assert_eq!(local.shell.projects.len() + shieldy.shell.projects.len(), 3);
        let catalog = local.catalog.as_ref().unwrap();
        let choices = crate::models::model_choices(catalog);
        let providers: Vec<&str> = crate::models::by_provider(&choices)
            .into_iter()
            .map(|(provider, _)| provider)
            .collect();
        assert_eq!(providers, ["anthropic", "openai"]);
    }
}
