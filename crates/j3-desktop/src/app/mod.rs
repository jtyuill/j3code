//! The app's root state ([`J3`]) and the Rune hooks around it: `setup` (store the waker,
//! open the environments), `poll` (drain the inbox, run due timers), `on_key` (route keys
//! to actions) and the window spec (spec §2.1, §3).
//!
//! Each feature module owns its state struct (a field here) and an `impl J3 { … }` block
//! with its commands. Async work reaches the state only through [`Inbox`] updates, via
//! [`J3::spawn`] / [`J3::spawn_blocking`].

pub mod fixtures;
#[cfg(test)]
pub mod testing;
#[cfg(test)]
mod tests;
mod view;
mod workspace;

use std::{
    cell::RefCell,
    collections::HashMap,
    future::Future,
    rc::Rc,
    sync::{Arc, OnceLock},
};

use rune::{AppSpec, Chrome, KeyEvent, Tx, Waker};
use time::OffsetDateTime;

use crate::{
    attention::AttentionMonitor,
    composer::{ComposerKey, ComposerState},
    dialogs::DialogState,
    environment::Environments,
    files::FilePane,
    git::GitControl,
    hub::AgentHub,
    images::Images,
    inbox::Inbox,
    math::Typesetter,
    new_thread::NewThreadState,
    preferences::Preferences,
    services::Services,
    settings::SettingsState,
    sidebar::{SidebarState, ThreadKey},
    speech::SpeechUi,
    terminal::Drawer,
    thread::ThreadUi,
    toasts::Toasts,
    usage::UsageStore,
};

pub use view::{Ui, VERSION_LABEL, view};

/// Wayland app id / X11 WM_CLASS; the Linux desktop entry is `<APP_ID>.desktop`.
pub const APP_ID: &str = "dev.jacobyuill.j3code";

/// What the main pane shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Main {
    /// "No thread open" and the shortcut cheat sheet.
    #[default]
    Empty,
    /// One open thread.
    Thread(ThreadKey),
    /// The new-thread surface (`s.new_thread` holds its environment and project).
    NewThread,
}

/// Which screen the window shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Route {
    pub main: Main,
    /// Settings covers the whole window while true.
    pub settings: bool,
}

/// Time as the app sees it, advanced at the start of every frame's `poll`.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    /// Seconds since start from Rune (virtual and deterministic in headless runs). Timer
    /// deadlines are in these seconds; ask for a poll with `tx.wake_after`.
    pub t: f64,
    /// Wall-clock now (start + `t`), for ages, snoozes and working timers.
    pub now: OffsetDateTime,
    start: OffsetDateTime,
}

impl Clock {
    pub fn new(start: OffsetDateTime) -> Self {
        Self {
            t: 0.0,
            now: start,
            start,
        }
    }

    fn advance(&mut self, t: f64) {
        self.t = t;
        self.now = self.start + time::Duration::seconds_f64(t);
    }
}

pub struct J3 {
    pub services: Arc<Services>,
    pub inbox: Inbox,
    /// `desktop.json`: remote environments and UI settings, saved on every change.
    pub prefs: Preferences,
    /// Local first, then remotes; `envs.distinct()` hides servers reached twice.
    pub envs: Environments,
    /// Decoded chat images (bounded LRU); read through `J3::image`.
    pub images: Images,
    /// Typeset TeX shared by every markdown view (see `math`).
    pub math: Rc<RefCell<Typesetter>>,
    pub route: Route,
    pub sidebar: SidebarState,
    /// Per open-or-visited thread UI state, fed by its watch-forwarding task.
    pub threads: HashMap<ThreadKey, ThreadUi>,
    pub composers: HashMap<ComposerKey, ComposerState>,
    pub new_thread: NewThreadState,
    pub settings: SettingsState,
    /// At most one modal dialog.
    pub dialogs: DialogState,
    pub toasts: Toasts,
    pub terminals: HashMap<ThreadKey, Drawer>,
    pub files: HashMap<ThreadKey, FilePane>,
    pub git: HashMap<ThreadKey, GitControl>,
    pub hubs: HashMap<ThreadKey, AgentHub>,
    pub usage: UsageStore,
    pub attention: AttentionMonitor,
    /// App-wide dictation (one capture at a time).
    pub speech: SpeechUi,
    /// Whether the window has keyboard focus (sounds play only while it doesn't).
    pub window_active: bool,
    pub clock: Clock,
    /// The open thread's completion last visited because it finished while open, so a
    /// completion is acknowledged once.
    pub(crate) visited_completion: Option<(ThreadKey, String)>,
    /// Font families from `desktop.json` applied (first poll).
    fonts_applied: bool,
}

impl J3 {
    /// The app state over `services` and loaded `prefs`, with no environments yet
    /// (`setup` opens them; tests push their own).
    pub fn new(services: Arc<Services>, prefs: Preferences) -> Self {
        let inbox = Inbox::default();
        let usage = UsageStore::load(services.config_path.clone());
        Self {
            images: Images::new(inbox.waker()),
            math: Rc::default(),
            inbox,
            prefs,
            envs: Environments::default(),
            route: Route::default(),
            sidebar: SidebarState::default(),
            threads: HashMap::new(),
            composers: HashMap::new(),
            new_thread: NewThreadState::default(),
            settings: SettingsState::default(),
            dialogs: DialogState::default(),
            toasts: Toasts::default(),
            terminals: HashMap::new(),
            files: HashMap::new(),
            git: HashMap::new(),
            hubs: HashMap::new(),
            usage,
            attention: AttentionMonitor::default(),
            speech: SpeechUi::default(),
            window_active: true,
            clock: Clock::new(OffsetDateTime::now_utc()),
            visited_completion: None,
            fonts_applied: false,
            services,
        }
    }

    /// Runs `fut` on the tokio runtime and applies the update it returns on the UI thread.
    pub fn spawn<F, U>(&self, fut: F)
    where
        F: Future<Output = U> + Send + 'static,
        U: FnOnce(&mut J3, &mut Tx) + Send + 'static,
    {
        let inbox = self.inbox.clone();
        self.services.runtime.spawn(async move {
            let update = fut.await;
            inbox.post(update);
        });
    }

    /// Runs blocking `f` on tokio's blocking pool and applies the update it returns.
    pub fn spawn_blocking<F, U>(&self, f: F)
    where
        F: FnOnce() -> U + Send + 'static,
        U: FnOnce(&mut J3, &mut Tx) + Send + 'static,
    {
        let inbox = self.inbox.clone();
        self.services.runtime.spawn_blocking(move || inbox.post(f()));
    }

    /// Runs a server command and applies `then` with its value; a failure becomes an error
    /// toast "Couldn’t {what}: {error}".
    pub fn run_command<T, F>(
        &self,
        what: &'static str,
        command: F,
        then: impl FnOnce(&mut J3, T, &mut Tx) + Send + 'static,
    ) where
        T: Send + 'static,
        F: Future<Output = Result<T, j3_client::Error>> + Send + 'static,
    {
        self.spawn(async move {
            let result = command.await;
            move |s: &mut J3, tx: &mut Tx| match result {
                Ok(value) => then(s, value, tx),
                Err(e) => {
                    s.toasts.error(format!("Couldn’t {what}: {e}"));
                }
            }
        });
    }

    /// Quits: the window's loop ends and `main` shuts the services down (bounded).
    pub fn quit(&mut self, tx: &mut Tx) {
        tx.quit();
    }
}

/// The services `init` builds the state over; set by `main` before `run`.
static SERVICES: OnceLock<Arc<Services>> = OnceLock::new();

fn init() -> J3 {
    let services = SERVICES
        .get()
        .expect("main installs the services before run")
        .clone();
    let hostname = gethostname::gethostname().to_string_lossy().into_owned();
    let prefs = Preferences::load(services.config_path.clone(), &hostname);
    J3::new(services, prefs)
}

/// Stores the waker and opens the environments (connections start connecting on tokio),
/// unless a fixture scene already installed its offline ones.
pub fn setup(s: &mut J3, waker: Waker) {
    s.inbox.set_waker(waker);
    if s.envs.is_empty() {
        s.open_environments();
    }
}

/// Before every frame: advances the clock, applies inbox updates and runs due timers.
pub fn poll(s: &mut J3, t: f64, tx: &mut Tx) -> bool {
    s.clock.advance(t);
    let mut changed = false;
    if !s.fonts_applied {
        s.fonts_applied = true;
        if crate::theme::families(s.prefs.ui())
            != (
                crate::theme::FAMILIES.0.to_owned(),
                crate::theme::FAMILIES.1.to_owned(),
            )
        {
            crate::theme::apply_families(s.prefs.ui(), tx);
            changed = true;
        }
    }
    let inbox = s.inbox.clone();
    changed |= inbox.drain(s, tx);
    changed |= s.fetch_wanted_images();
    changed |= s.poll_speech(tx);
    changed |= s.poll_usage(tx);
    changed |= s.toasts.poll(s.clock.t, tx);
    changed |= s.poll_sidebar(tx);
    changed |= s.poll_git(tx);
    changed
}

/// Keys the focused control passed on (or every key while nothing has focus): the sidebar
/// toggle (⇧⌘B), then the binding table in the key's context.
pub fn on_key(s: &mut J3, ev: &KeyEvent, tx: &mut Tx) -> bool {
    if rune_kit::shell::is_sidebar_toggle(ev) && !s.route.settings {
        s.sidebar.pane.toggle();
        return true;
    }
    let context = s.key_context(tx);
    match crate::actions::action_for(ev, context, tx) {
        Some(action) => {
            s.dispatch(action, tx);
            true
        }
        None => false,
    }
}

fn on_focus(s: &mut J3, focused: bool, _: &mut Tx) {
    s.window_active = focused;
}

fn on_close(s: &mut J3, tx: &mut Tx) {
    s.quit(tx);
}

/// The window (title `j3`, 1280×820, at least 720×480) over `services`.
pub fn app(services: Arc<Services>) -> rune::App<J3> {
    if SERVICES.set(services).is_err() {
        panic!("app() is called once");
    }
    rune::set_app_id(APP_ID);
    let spec = AppSpec {
        min_size: Some((720.0, 480.0)),
        fonts: crate::theme::FONTS,
        families: crate::theme::FAMILIES,
        setup: Some(setup),
        poll: Some(poll),
        on_key: Some(on_key),
        scenes: fixtures::SCENES,
        ..AppSpec::new("j3", (1280.0, 820.0), init, view)
    };
    rune::App::new(spec)
        // macOS: the panes' title bar band is the titlebar; the traffic lights sit in its
        // middle (rune-kit's shell band is 48 points tall).
        .chrome(Chrome {
            full_size_content: true,
            traffic_lights: Some((16.0, 17.0)),
            drag_regions: true,
        })
        .on_focus(on_focus)
        .on_close(on_close)
}
