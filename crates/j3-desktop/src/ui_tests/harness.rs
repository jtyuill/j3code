//! The app under test: the production [`J3`] in a headless [`TestApp`] whose local
//! environment is a real `j3_core` server on a free port, sessions scripted by [`Script`].
//!
//! The server and the connections run on tokio threads, so waits are real-time bounded
//! ([`WAIT`]) loops of frames; each frame advances the app's clock by the real time since the
//! previous one, so timers (snoozes, double taps, toasts) run on the wall clock.

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use j3_client::{
    Connection, ConnectionStatus, ThreadState,
    t3::{model::ModelSelection, orchestration::ThreadShell},
};
use j3_core::{
    app::{App as CoreApp, AppConfig},
    omp::OmpLauncher,
    serve::Server,
    store::random_hex,
};
use rune::{ElementInfo, Key, KeyEvent, Mods, Runtime};
use serde_json::{Value, json};

use super::omp::{FakeFactory, FakeMicrophone, FakeRecognizer, MODEL, Script, install_fake_omp};
use crate::{
    app::{J3, testing::TestApp},
    assist::AssistOmp,
    composer::ComposerKey,
    local_server::LocalServer,
    preferences::Preferences,
    services::Services,
    speech::{SpeechEngine, models::Hardware},
};

/// Bound on every wait for the server, the fake or the UI.
pub const WAIT: Duration = Duration::from_secs(15);

pub fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("j3-desktop-{name}-{}", random_hex(10)));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn model() -> ModelSelection {
    serde_json::from_value(
        json!({"instanceId": "omp", "model": MODEL, "options": [{"id": "thinking", "value": "high"}]}),
    )
    .unwrap()
}

/// The UI preferences tests start with: quiet, every T3 shelf open.
pub fn test_ui() -> Value {
    json!({"sounds": false, "expandedShelves": ["working", "snoozed", "settled"]})
}

/// Serves a j3 server over `data_dir` on a free port, sessions faked by `script` (their
/// transcript is `data_dir/fake-session.jsonl`); returns it with its port.
pub fn serve(
    runtime: &tokio::runtime::Runtime,
    data_dir: &Path,
    omp_path: &Path,
    script: Arc<Script>,
) -> (Server, u16) {
    let factory = FakeFactory {
        script,
        session_file: data_dir.join("fake-session.jsonl"),
    };
    runtime.block_on(async {
        let app = CoreApp::open_with(
            AppConfig {
                data_dir: data_dir.to_owned(),
                label: "test-host".into(),
                omp: OmpLauncher::new(omp_path),
            },
            Arc::new(factory),
            Duration::from_secs(600),
        )
        .await
        .unwrap();
        let any_port: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let server = Server::bind(app, &[any_port]).await.unwrap();
        let port = server.addrs()[0].port();
        (server, port)
    })
}

/// A key with the platform's command modifier (⌘ on macOS, Ctrl elsewhere).
pub fn primary(c: char) -> KeyEvent {
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

/// [`primary`] with Shift.
pub fn primary_shift(c: char) -> KeyEvent {
    let mut ev = primary(c);
    ev.mods.shift = true;
    ev
}

pub fn plain(key: Key) -> KeyEvent {
    KeyEvent {
        key,
        mods: Mods::default(),
    }
}

/// The centre of an element's drawn box.
pub fn centre(e: &ElementInfo) -> (f32, f32) {
    (e.rect[0] + e.rect[2] / 2.0, e.rect[1] + e.rect[3] / 2.0)
}

pub fn top(e: &ElementInfo) -> f32 {
    e.rect[1]
}

pub fn bottom(e: &ElementInfo) -> f32 {
    e.rect[1] + e.rect[3]
}

pub struct Harness {
    /// `None` only while dropping.
    app: Option<TestApp>,
    pub script: Arc<Script>,
    /// The local environment's connection (the app's own).
    pub connection: Connection,
    pub project_dir: PathBuf,
    pub data_dir: PathBuf,
    pub config_dir: PathBuf,
    /// When the last frame ran: the next one advances the app's clock by the time since.
    last_frame: Instant,
}

impl Harness {
    /// Serves a fresh j3 server and opens the app on it, with the local connection live.
    pub fn start(name: &str) -> Self {
        Self::start_with(name, |_| json!([]), test_ui())
    }

    /// Like [`Harness::start`], with these UI preferences.
    pub fn start_ui(name: &str, ui: Value) -> Self {
        Self::start_with(name, |_| json!([]), ui)
    }

    /// Like [`Harness::start`], with the remote environments `remotes` returns for the local
    /// server's port and the UI preferences `ui` written to the desktop config.
    pub fn start_with(name: &str, remotes: impl FnOnce(u16) -> Value, ui: Value) -> Self {
        let tokio = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let data_dir = temp_dir(name);
        let project_dir = temp_dir(&format!("{name}-project"));
        let config_dir = temp_dir(&format!("{name}-config"));
        let omp_path = install_fake_omp(&data_dir);
        let script = Arc::new(Script::default());
        let (server, port) = serve(&tokio, &data_dir, &omp_path, Arc::clone(&script));
        let config_path = config_dir.join("desktop.json");
        std::fs::write(
            &config_path,
            json!({ "environments": remotes(port), "ui": ui }).to_string(),
        )
        .unwrap();
        let local = Arc::new(LocalServer::from_server(server));
        // The composer's word prediction asks the fake omp too.
        let assist = AssistOmp::new(OmpLauncher::new(&omp_path), project_dir.clone());
        let speech = SpeechEngine::new(
            Box::new(FakeMicrophone),
            Arc::new(FakeRecognizer(Hardware {
                gpu: None,
                threads: 4,
            })),
        );
        let services = Arc::new(Services::new(
            tokio.handle().clone(),
            Some(local),
            assist,
            speech,
            Some(config_path.clone()),
            Some(data_dir.clone()),
        ));
        let prefs = Preferences::load(Some(config_path), "test-host");
        let mut app = TestApp::with_services(services, prefs, tokio);
        app.rt.update(|s, _| s.open_environments());
        let connection = app
            .rt
            .state
            .envs
            .local()
            .expect("the local environment")
            .connection()
            .clone();
        let mut harness = Self {
            app: Some(app),
            script,
            connection,
            project_dir,
            data_dir,
            config_dir,
            last_frame: Instant::now(),
        };
        harness.until("the local connection is live", |h| {
            let state = h.connection.state();
            let state = state.borrow();
            state.status == ConnectionStatus::Live && state.shell.loaded
        });
        harness
    }

    pub fn rt(&self) -> &Runtime<J3> {
        &self.app.as_ref().expect("the app runs").rt
    }

    pub fn rt_mut(&mut self) -> &mut Runtime<J3> {
        &mut self.app.as_mut().expect("the app runs").rt
    }

    pub fn state(&self) -> &J3 {
        &self.rt().state
    }

    pub fn tokio(&self) -> &tokio::runtime::Runtime {
        &self.app.as_ref().expect("the app runs").tokio
    }

    /// Runs `fut` to completion on the app's tokio runtime.
    pub fn block_on<F: Future>(&self, fut: F) -> F::Output {
        self.tokio().block_on(fut)
    }

    /// One frame, advancing the app's clock by the real time since the last.
    pub fn frame(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f64();
        self.last_frame = now;
        self.rt_mut().frame(dt);
    }

    /// Frames until `done` holds; panics naming `what` after [`WAIT`].
    pub fn until(&mut self, what: &str, mut done: impl FnMut(&Self) -> bool) {
        let start = Instant::now();
        loop {
            self.frame();
            if done(self) {
                return;
            }
            assert!(start.elapsed() < WAIT, "timed out waiting until {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Frames for `time` of real time.
    pub fn pause(&mut self, time: Duration) {
        let start = Instant::now();
        while start.elapsed() < time {
            self.frame();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Waits until the fake has recorded `call`.
    pub fn until_called(&mut self, call: &str) {
        let script = Arc::clone(&self.script);
        self.until(&format!("omp is asked {call:?}"), move |_| {
            script.called(call)
        });
    }

    /// Waits until the thread's client state satisfies `check`.
    pub fn until_thread(
        &mut self,
        what: &str,
        thread_id: &str,
        mut check: impl FnMut(&ThreadState) -> bool,
    ) {
        let handle = self.connection.thread(thread_id);
        self.until(what, move |_| check(&handle.state().borrow()));
    }

    /// Waits until the shell's row of the thread satisfies `check`.
    pub fn until_shell(
        &mut self,
        what: &str,
        thread_id: &str,
        mut check: impl FnMut(&ThreadShell) -> bool,
    ) {
        let connection = self.connection.clone();
        self.until(what, move |_| {
            connection
                .state()
                .borrow()
                .shell
                .thread(thread_id)
                .is_some_and(&mut check)
        });
    }

    /// Waits until `desktop.json` satisfies `check`.
    pub fn until_config(&mut self, what: &str, check: impl Fn(&Value) -> bool) {
        let path = self.config_dir.join("desktop.json");
        self.until(what, move |_| {
            std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .is_some_and(|config| check(&config))
        });
    }

    /// Waits until the local server's model catalog (from the fake omp) has arrived.
    pub fn until_models(&mut self) {
        let connection = self.connection.clone();
        self.until("the model catalog loads", move |_| {
            connection
                .state()
                .borrow()
                .config
                .as_ref()
                .is_some_and(|config| config.providers.iter().any(|p| !p.models.is_empty()))
        });
    }

    /// Creates the project "Demo" through the connection and waits until the app has it.
    pub fn project(&mut self) -> String {
        let connection = self.connection.clone();
        let dir = self.project_dir.to_string_lossy().into_owned();
        let project = self
            .block_on(async move { connection.create_project("Demo", &dir).await })
            .unwrap();
        let id = project.id.clone();
        self.until("the app has the project", |h| {
            h.state()
                .envs
                .local()
                .is_some_and(|env| env.state().shell.project(&id).is_some())
        });
        project.id
    }

    /// Launches a thread through the connection and starts its run (in the newest fake
    /// session); returns its id.
    pub fn launch(&mut self, project_id: &str, text: &str) -> String {
        let connection = self.connection.clone();
        let (project, message) = (project_id.to_owned(), text.to_owned());
        let thread_id = self
            .block_on(async move {
                connection
                    .launch(&project, &message, Vec::new(), model())
                    .await
            })
            .unwrap();
        self.until_called(&format!("prompt:{text}:0"));
        self.script.emit(json!({"type": "agent_start"}));
        self.until_shell("the run starts", &thread_id, |thread| {
            thread.active_run_id.is_some()
        });
        thread_id
    }

    /// Launches a thread and lets its run complete; returns its id.
    pub fn completed(&mut self, project_id: &str, text: &str) -> String {
        let thread_id = self.launch(project_id, text);
        self.script.settle("completed");
        self.until_shell("the run completes", &thread_id, |thread| {
            thread.active_run_id.is_none()
                && thread
                    .latest_run_completed_at
                    .as_ref()
                    .is_some_and(Option::is_some)
        });
        thread_id
    }

    /// Launches a thread, starts its run and opens it from the sidebar; returns its id.
    pub fn open_running_thread(&mut self, project_id: &str, text: &str) -> String {
        let thread_id = self.launch(project_id, text);
        let row = format!("{text}, Working");
        self.until("the sidebar lists the running thread", |h| {
            h.in_sidebar(&row)
        });
        self.click_sidebar(&row);
        // The thread view's composer offers Stop while the run is active.
        self.until("the composer offers Stop", |h| h.stoppable());
        self.until_thread("the run can steer", &thread_id, |state| state.can_steer());
        thread_id
    }

    /// Opens a new thread in a fresh project once the model catalog has loaded, with the
    /// composer focused.
    pub fn focused_new_thread_composer(&mut self) {
        self.project();
        self.until_models();
        self.click("new-thread");
        self.until("the composer is focused", |h| h.composer_focused());
    }

    // -- Reading the window ------------------------------------------------------------

    /// Every element not leaving the window.
    pub fn elements(&self) -> Vec<ElementInfo> {
        self.rt()
            .elements()
            .into_iter()
            .filter(|e| !e.exiting)
            .collect()
    }

    /// The element keyed `key`.
    pub fn element(&self, key: &str) -> Option<ElementInfo> {
        self.elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some(key))
    }

    pub fn exists(&self, key: &str) -> bool {
        self.rt().find(key).is_some()
    }

    /// The accessible label of the element keyed `key`, if any.
    pub fn label_of(&self, key: &str) -> Option<String> {
        self.element(key).and_then(|e| e.label)
    }

    /// The topmost element labelled exactly `label`.
    pub fn labelled(&self, label: &str) -> Option<ElementInfo> {
        self.elements()
            .into_iter()
            .rfind(|e| e.label.as_deref() == Some(label))
    }

    pub fn has_label(&self, label: &str) -> bool {
        self.labelled(label).is_some()
    }

    /// The element labelled `label` inside the element keyed `scope`.
    pub fn in_scope(&self, scope: &str, label: &str) -> Option<ElementInfo> {
        self.elements().into_iter().find(|e| {
            e.label.as_deref() == Some(label) && e.path.split('/').any(|part| part == scope)
        })
    }

    /// How many elements inside `scope` are labelled `label`.
    pub fn count_in_scope(&self, scope: &str, label: &str) -> usize {
        self.elements()
            .into_iter()
            .filter(|e| {
                e.label.as_deref() == Some(label) && e.path.split('/').any(|part| part == scope)
            })
            .count()
    }

    pub fn sidebar_row(&self, label: &str) -> Option<ElementInfo> {
        self.in_scope("sidebar", label)
    }

    pub fn in_sidebar(&self, label: &str) -> bool {
        self.sidebar_row(label).is_some()
    }

    /// The top edge of the sidebar row labelled `label`.
    pub fn top_of(&self, label: &str) -> f32 {
        top(&self
            .sidebar_row(label)
            .unwrap_or_else(|| panic!("sidebar row {label:?}")))
    }

    /// Whether the sidebar rows labelled `labels` all show, at rest (rows glide to new
    /// places), top to bottom in this order.
    pub fn rows_in_order(&self, labels: &[&str]) -> bool {
        let tops: Option<Vec<f32>> = labels
            .iter()
            .map(|label| {
                self.sidebar_row(label)
                    .filter(|row| !row.moving)
                    .map(|row| top(&row))
            })
            .collect();
        tops.is_some_and(|tops| tops.windows(2).all(|pair| pair[0] < pair[1]))
    }

    /// The feed row whose accessible label is `text`.
    pub fn feed_row(&self, text: &str) -> Option<ElementInfo> {
        self.in_scope("feed-rows", text)
    }

    /// Whether a text drawn in the feed contains `text`.
    pub fn feed_shows(&self, text: &str) -> bool {
        self.elements().into_iter().any(|e| {
            e.path.split('/').any(|part| part == "feed-rows")
                && e.text.as_deref().is_some_and(|shown| shown.contains(text))
        })
    }

    /// Whether the composer's one button offers Stop: a run is active and the draft is empty.
    pub fn stoppable(&self) -> bool {
        self.label_of("send-message").as_deref() == Some("Stop")
    }

    /// The main pane's composer.
    pub fn composer_key(&self) -> ComposerKey {
        self.state().main_composer_key().expect("a composer shows")
    }

    /// The main composer's draft text (chips as their typed selectors).
    pub fn draft(&self) -> String {
        self.state()
            .composers
            .get(&self.composer_key())
            .map(|composer| crate::composer::plain_text(&composer.edit))
            .unwrap_or_default()
    }

    /// Whether the composer's text field (labelled "Message") has the keyboard.
    pub fn composer_focused(&self) -> bool {
        self.labelled("Message").is_some_and(|e| e.focused)
    }

    /// The focused element's key.
    pub fn focused(&self) -> Option<String> {
        self.elements()
            .into_iter()
            .find(|e| e.focused)
            .and_then(|e| e.key)
    }

    // -- Input -------------------------------------------------------------------------

    /// A press and release at window point `at`, then a frame.
    pub fn tap_at(&mut self, (x, y): (f32, f32)) {
        let rt = self.rt_mut();
        rt.pointer_move(x, y);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        self.frame();
    }

    /// The centre of the element `pick` finds, once it shows and has come to rest (a press
    /// lands where the element is drawn, which an entering or moving one is not yet): not
    /// moving itself and drawn where it was the frame before (ancestors may be moving it).
    pub fn at_rest(
        &mut self,
        what: &str,
        pick: impl Fn(&Self) -> Option<ElementInfo>,
    ) -> (f32, f32) {
        let mut last = None;
        self.until(&format!("{what} shows, at rest"), |h| {
            let rect = pick(h).filter(|e| !e.moving).map(|e| e.rect);
            let rest = rect.is_some() && rect == last;
            last = rect;
            rest
        });
        centre(&pick(self).unwrap())
    }

    /// Clicks the element keyed `key` (waiting for it to show).
    pub fn click(&mut self, key: &str) {
        let at = self.at_rest(key, |h| h.element(key));
        self.tap_at(at);
    }

    /// Focuses the composer's text field by clicking it.
    pub fn click_composer(&mut self) {
        let field = self.composer_key().input_key();
        self.click(&field);
        assert!(self.composer_focused(), "a click focuses the composer");
    }

    /// Clicks the topmost element labelled `label` (an open menu's item over a row).
    pub fn click_label(&mut self, label: &str) {
        let at = self.at_rest(&format!("“{label}”"), |h| h.labelled(label));
        self.tap_at(at);
    }

    /// Clicks a sidebar row.
    pub fn click_sidebar(&mut self, label: &str) {
        let at = self.at_rest(&format!("sidebar row {label:?}"), |h| h.sidebar_row(label));
        self.tap_at(at);
    }

    /// A secondary (right) click on the element keyed `key`.
    pub fn right_click(&mut self, key: &str) {
        let (x, y) = self.at_rest(key, |h| h.element(key));
        let rt = self.rt_mut();
        rt.pointer_move(x, y);
        rt.secondary_down(x, y);
        rt.secondary_up(x, y);
        self.frame();
    }

    /// Drags the element keyed `from` onto the element keyed `to`.
    pub fn drag_to(&mut self, from: &str, to: &str) {
        let (x, y) = self.at_rest(from, |h| h.element(from));
        self.at_rest(to, |h| h.element(to));
        self.rt_mut().pointer_move(x, y);
        self.rt_mut().pointer_down(x, y);
        self.frame();
        let (tx, ty) = self.rt().find(to).unwrap_or_else(|| panic!("{to}"));
        for step in 1..=8 {
            let f = step as f32 / 8.0;
            self.rt_mut()
                .pointer_move(x + (tx - x) * f, y + (ty - y) * f);
            self.frame();
        }
        // The drop target may have moved aside for the dragged row.
        let (tx, ty) = self.rt().find(to).unwrap_or((tx, ty));
        self.rt_mut().pointer_move(tx, ty);
        self.frame();
        self.rt_mut().pointer_up(tx, ty);
        self.frame();
    }

    /// A key to the window, then a frame.
    pub fn key(&mut self, ev: KeyEvent) {
        self.rt_mut().key(ev);
        self.frame();
    }

    pub fn press(&mut self, key: Key) {
        self.key(plain(key));
    }

    /// Types `text` key by key (`\n` as Enter) into whatever has the keyboard.
    pub fn typed(&mut self, text: &str) {
        for c in text.chars() {
            let key = if c == '\n' {
                Key::Enter
            } else {
                Key::Text(c.to_string())
            };
            self.press(key);
        }
    }
}

impl Drop for Harness {
    /// Stops the server, the connections and the assist omp, and removes the temp dirs.
    fn drop(&mut self) {
        if let Some(app) = self.app.take() {
            let TestApp { rt, tokio } = app;
            let services = Arc::clone(&rt.state.services);
            drop(rt);
            services.shutdown();
            tokio.shutdown_background();
        }
        for dir in [&self.data_dir, &self.project_dir, &self.config_dir] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}
