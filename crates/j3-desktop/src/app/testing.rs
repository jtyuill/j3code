//! Headless test support: the app state over in-memory preferences and test services, in a
//! `rune::Runtime` wired to the app's `view`, `poll` and `on_key`, so a test drives frames,
//! keys and pointer input exactly as a window would and inbox updates land in `poll`.
//!
//! ```ignore
//! let mut app = TestApp::new();
//! app.frame();                                  // runs poll (inbox, timers) and the view
//! app.rt.state.inbox.post(|s, _| s.toasts.info("hi"));
//! app.frame();
//! assert!(app.rt.find("toast-1").is_some());
//! ```

use std::{path::PathBuf, sync::Arc};

use rune::{Env, KeyEvent, Runtime};

use super::J3;
use crate::{
    assist::{self, AssistOmp},
    environments::DesktopConfig,
    preferences::Preferences,
    services::Services,
    speech::SpeechEngine,
    ui_settings::UiSettings,
};

/// The window size tests render at.
pub const SIZE: (f32, f32) = (1280.0, 820.0);

pub struct TestApp {
    pub rt: Runtime<J3>,
    /// The tokio runtime `J3::spawn` and connections run on; kept alive with the app.
    pub tokio: tokio::runtime::Runtime,
}

impl TestApp {
    /// No environments, default UI settings, nothing saved to disk.
    pub fn new() -> Self {
        Self::with_ui(UiSettings::default())
    }

    pub fn with_ui(ui: UiSettings) -> Self {
        let tokio = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a test tokio runtime");
        let services = test_services(tokio.handle().clone());
        let prefs = Preferences::in_memory(DesktopConfig {
            environments: Vec::new(),
            ui,
        });
        let state = J3::new(services, prefs);
        let mut rt = Runtime::new(state, super::view, SIZE, Env::default());
        rt.poll = Some(super::poll);
        rt.on_key = Some(super::on_key);
        Self { rt, tokio }
    }

    /// Default UI settings with fixture scene `scene` installed (offline environments, no
    /// network): see `app::fixtures`.
    pub fn with_scene(scene: super::fixtures::Scene) -> Self {
        let mut app = Self::new();
        super::fixtures::install(&mut app.rt.state, scene);
        app
    }

    /// One 60 Hz frame: `poll` (clock, inbox, timers), handlers, the view if state changed.
    pub fn frame(&mut self) -> bool {
        self.rt.frame(1.0 / 60.0)
    }

    /// Frames until the inbox is empty and nothing changes (at most `limit`), for async
    /// updates that arrive over a few frames.
    pub fn settle(&mut self, limit: usize) {
        for _ in 0..limit {
            if !self.frame() {
                return;
            }
        }
    }

    /// A key to the window (as a focused window delivers it), then a frame.
    pub fn key(&mut self, ev: KeyEvent) {
        self.rt.key(ev);
        self.frame();
    }
}

impl Default for TestApp {
    fn default() -> Self {
        Self::new()
    }
}

/// Services for tests: no local server, no config file, an assist omp and dictation engine
/// that start nothing until used.
pub fn test_services(runtime: tokio::runtime::Handle) -> Arc<Services> {
    let dir = std::env::temp_dir().join("j3-test");
    let assist = AssistOmp::new(assist::launcher(None), PathBuf::from("/"));
    let speech = SpeechEngine::local(dir.join("models"), runtime.clone());
    Arc::new(Services::new(runtime, None, assist, speech, None, None))
}
