//! herdr-style attention alerts: watches every thread's agent status and
//! reports when one becomes blocked (always) or finishes while the user is
//! not watching it. The workspace turns alerts into toasts and, while the
//! window is in the background, sounds, as the notification preferences
//! allow.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use j3_client::{
    ShellState,
    workflow::{AgentNotification, AgentStatus, agent_notification, agent_status, is_listed},
};
use tokio::runtime::Handle;

/// One alert to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub env: String,
    pub thread_id: String,
    pub title: String,
    pub kind: AgentNotification,
}

impl Alert {
    /// herdr's toast title: `«agent» needs attention` / `«agent» finished`.
    pub fn message(&self) -> String {
        let title = if self.title.trim().is_empty() {
            "Untitled thread"
        } else {
            self.title.as_str()
        };
        match self.kind {
            AgentNotification::NeedsAttention => format!("{title} needs attention"),
            AgentNotification::Finished => format!("{title} finished"),
        }
    }

    pub fn sound(&self) -> Sound {
        match self.kind {
            AgentNotification::NeedsAttention => Sound::Request,
            AgentNotification::Finished => Sound::Done,
        }
    }
}

/// The last status seen of every thread, per environment.
#[derive(Default)]
pub struct AttentionMonitor {
    /// Thread statuses by environment, then thread id. An environment is
    /// present once its shell has been seen loaded: its first statuses are a
    /// baseline, not changes.
    statuses: HashMap<String, HashMap<String, AgentStatus>>,
}

impl AttentionMonitor {
    /// Records `env`'s statuses and returns the alerts their changes raise.
    /// `open` is the open thread of this environment, if any.
    pub fn observe(&mut self, env: &str, shell: &ShellState, open: Option<&str>) -> Vec<Alert> {
        if !shell.loaded {
            return Vec::new();
        }
        let baseline = !self.statuses.contains_key(env);
        if baseline {
            self.statuses.insert(env.to_owned(), HashMap::new());
        }
        let Some(statuses) = self.statuses.get_mut(env) else {
            return Vec::new();
        };
        let mut alerts = Vec::new();
        let mut seen = HashSet::new();
        for thread in shell.threads.iter().filter(|thread| is_listed(thread)) {
            let next = agent_status(thread);
            seen.insert(thread.id.as_str());
            let previous = statuses.insert(thread.id.clone(), next);
            if baseline {
                continue;
            }
            let watching = open == Some(thread.id.as_str());
            // A thread first seen now was unknown until now.
            let previous = previous.unwrap_or(AgentStatus::Unknown);
            if let Some(kind) = agent_notification(previous, next, watching) {
                alerts.push(Alert {
                    env: env.to_owned(),
                    thread_id: thread.id.clone(),
                    title: thread.title.clone(),
                    kind,
                });
            }
        }
        statuses.retain(|id, _| seen.contains(id.as_str()));
        alerts
    }

    /// Forgets a removed environment.
    pub fn forget(&mut self, env: &str) {
        self.statuses.remove(env);
    }
}

/// The two alert sounds, synthesized for j3 (`assets/sounds`): a soft
/// rising chime when a run finishes, a questioning three-note ping when a
/// thread needs an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    /// A thread needs you.
    Request,
    /// A thread finished.
    Done,
}

impl Sound {
    fn name(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Done => "done",
        }
    }

    fn wav(self) -> &'static [u8] {
        match self {
            Self::Request => include_bytes!("../assets/sounds/request.wav"),
            Self::Done => include_bytes!("../assets/sounds/done.wav"),
        }
    }

    /// The players to try, in order; the first one that starts wins.
    fn players() -> &'static [&'static str] {
        if cfg!(target_os = "macos") {
            &["afplay"]
        } else {
            // PulseAudio (or PipeWire's pulse server), PipeWire, ALSA.
            &["paplay", "pw-play", "aplay"]
        }
    }

    /// The sound as a file the players can open: written once per app
    /// version to the cache directory (rewritten when its size differs).
    fn file(self) -> std::io::Result<PathBuf> {
        let dir = dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("j3")
            .join("sounds");
        let path = dir.join(format!("{}.wav", self.name()));
        let wav = self.wav();
        let current = std::fs::metadata(&path).is_ok_and(|meta| meta.len() == wav.len() as u64);
        if !current {
            std::fs::create_dir_all(&dir)?;
            std::fs::write(&path, wav)?;
        }
        Ok(path)
    }

    /// Plays the sound on `runtime` without waiting for it. No player (or no
    /// audio device) stays silent.
    pub fn play(&self, runtime: &Handle) {
        let sound = *self;
        runtime.spawn(async move {
            let path = match tokio::task::spawn_blocking(move || sound.file()).await {
                Ok(Ok(path)) => path,
                Ok(Err(e)) => return tracing::debug!("writing the {} sound: {e}", sound.name()),
                Err(e) => return tracing::debug!("{e}"),
            };
            for program in Self::players() {
                let status = tokio::process::Command::new(program)
                    .arg(&path)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .await;
                match status {
                    Ok(_) => return,
                    Err(e) => tracing::debug!("{program}: {e}"),
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use j3_client::ConnectionState;
    use serde_json::{Value, json};

    use crate::{
        app::{
            Main,
            fixtures::{self, Scene, ago, local_state, open_thread_key},
            testing::TestApp,
        },
        environment::LOCAL_KEY,
        sidebar::ThreadKey,
        toasts::ToastKind,
        ui_settings::UiSettings,
    };

    /// `state` with thread `id`'s fields patched.
    fn patched(mut state: ConnectionState, id: &str, patch: Value) -> ConnectionState {
        let thread = state
            .shell
            .threads
            .iter_mut()
            .find(|thread| thread.id == id)
            .expect("a fixture thread");
        let mut value = serde_json::to_value(&*thread).expect("a thread serializes");
        if let (Some(value), Value::Object(patch)) = (value.as_object_mut(), patch) {
            value.extend(patch);
        }
        *thread = serde_json::from_value(value).expect("the patched thread decodes");
        state
    }

    fn finished() -> Value {
        json!({"status": "completed", "activeRunId": null, "activityRunStatus": null,
               "activityRunStartedAt": null, "pendingRuntimeRequest": null,
               "latestRunCompletedAt": ago(0), "lastVisitedAt": ago(30)})
    }

    fn app() -> TestApp {
        // No sounds: tests never play audio.
        let mut app = TestApp::with_ui(UiSettings {
            sounds: false,
            ..UiSettings::default()
        });
        fixtures::install(&mut app.rt.state, Scene::Thread);
        app.frame();
        app
    }

    fn observe(app: &mut TestApp, state: ConnectionState) {
        app.rt.update(move |s, tx| s.environment_changed(LOCAL_KEY, Arc::new(state), tx));
        app.frame();
    }

    fn toasts(app: &TestApp) -> Vec<(ToastKind, String)> {
        app.rt
            .state
            .toasts
            .iter()
            .map(|(_, toast)| (toast.kind, toast.title.clone()))
            .collect()
    }

    #[test]
    fn changes_after_the_baseline_raise_toasts_that_open_their_thread() {
        let mut app = app();
        observe(&mut app, local_state());
        assert!(toasts(&app).is_empty(), "the first observation is a baseline");

        let blocked = patched(local_state(), "thr-sidebar-dnd", json!({
            "status": "failed", "activeRunId": null, "activityRunStatus": null,
            "latestRunCompletedAt": ago(0), "lastError": "boom"
        }));
        let state = patched(blocked, "thr-scroll", finished());
        observe(&mut app, state.clone());
        assert_eq!(
            toasts(&app),
            [
                (ToastKind::Warning, "Sidebar drag and drop between shelves needs attention".to_owned()),
                (ToastKind::Success, "Momentum scrolling stutters at 120 Hz finished".to_owned()),
            ]
        );

        let key = ThreadKey::new(LOCAL_KEY, "thr-scroll");
        let (x, y) = app.rt.find(&key.element_id("attention")).expect("the toast is drawn");
        app.rt.pointer_move(x, y);
        app.rt.pointer_down(x, y);
        app.rt.pointer_up(x, y);
        app.frame();
        assert_eq!(app.rt.state.route.main, Main::Thread(key));

        // Toasts off: the next change raises none.
        app.rt.update(|s, tx| s.update_ui(|ui| ui.toasts = false, tx));
        observe(&mut app, patched(state, "thr-math", json!({"status": "failed"})));
        assert_eq!(toasts(&app).len(), 1, "only the earlier toast is left");
    }

    #[test]
    fn the_open_thread_finishing_is_on_screen() {
        let mut app = app();
        observe(&mut app, local_state());
        let open = open_thread_key();
        // Watched while the window is active: no alert at all.
        observe(&mut app, patched(local_state(), &open.thread_id, finished()));
        assert!(toasts(&app).is_empty());

        // In the background the finish alerts (a sound, when on) but needs no toast.
        let mut app = self::app();
        observe(&mut app, local_state());
        app.rt.state.window_active = false;
        observe(&mut app, patched(local_state(), &open.thread_id, finished()));
        assert!(toasts(&app).is_empty());
        // A question on it still raises one.
        let asking = patched(local_state(), &open.thread_id, json!({"status": "waiting"}));
        observe(&mut app, asking);
        assert_eq!(
            toasts(&app),
            [(ToastKind::Warning, "Port the feed renderer to Rune needs attention".to_owned())]
        );
    }
}
