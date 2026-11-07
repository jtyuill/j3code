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
