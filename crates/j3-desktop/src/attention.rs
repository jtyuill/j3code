//! herdr-style attention alerts: watches every thread's agent status and
//! reports when one becomes blocked (always) or finishes while it is not the
//! open thread. The workspace turns alerts into toasts and sounds, as the
//! notification preferences allow.

use std::collections::{HashMap, HashSet};

use gpui_kit::SharedString;
use j3_client::{
    ShellState,
    workflow::{AgentNotification, AgentStatus, agent_notification, agent_status, is_listed},
};
use tokio::runtime::Handle;

/// One alert to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub env: SharedString,
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
    statuses: HashMap<(SharedString, String), AgentStatus>,
    /// Environments whose shell has been seen loaded: their first statuses
    /// are a baseline, not changes.
    primed: HashSet<SharedString>,
}

impl AttentionMonitor {
    /// Records `env`'s statuses and returns the alerts their changes raise.
    /// `open` is the open thread of this environment, if any.
    pub fn observe(
        &mut self,
        env: &SharedString,
        shell: &ShellState,
        open: Option<&str>,
    ) -> Vec<Alert> {
        if !shell.loaded {
            return Vec::new();
        }
        let baseline = self.primed.insert(env.clone());
        let mut alerts = Vec::new();
        let mut seen = HashSet::new();
        for thread in shell.threads.iter().filter(|thread| is_listed(thread)) {
            let next = agent_status(thread);
            let key = (env.clone(), thread.id.clone());
            seen.insert(thread.id.as_str());
            let previous = self.statuses.insert(key, next);
            if baseline {
                continue;
            }
            let watching = open == Some(thread.id.as_str());
            // A thread first seen now was unknown until now.
            let previous = previous.unwrap_or(AgentStatus::Unknown);
            if let Some(kind) = agent_notification(previous, next, watching) {
                alerts.push(Alert {
                    env: env.clone(),
                    thread_id: thread.id.clone(),
                    title: thread.title.clone(),
                    kind,
                });
            }
        }
        self.statuses
            .retain(|(e, id), _| e != env || seen.contains(id.as_str()));
        alerts
    }

    /// Forgets a removed environment.
    pub fn forget(&mut self, env: &str) {
        self.statuses.retain(|(e, _), _| e != env);
        self.primed.remove(env);
    }
}

/// herdr's two sounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    /// A thread needs you.
    Request,
    /// A background thread finished.
    Done,
}

impl Sound {
    /// The command that plays it on this platform, if any.
    fn command(self) -> Option<(&'static str, Vec<String>)> {
        if cfg!(target_os = "macos") {
            let name = match self {
                Self::Request => "Ping",
                Self::Done => "Glass",
            };
            Some((
                "afplay",
                vec![format!("/System/Library/Sounds/{name}.aiff")],
            ))
        } else if cfg!(target_os = "linux") {
            let id = match self {
                Self::Request => "dialog-warning",
                Self::Done => "complete",
            };
            Some(("canberra-gtk-play", vec!["-i".into(), id.into()]))
        } else {
            None
        }
    }

    /// Plays the sound on `runtime` without waiting for it. A missing
    /// player (e.g. no `canberra-gtk-play`) stays silent.
    pub fn play(self, runtime: &Handle) {
        let Some((program, args)) = self.command() else {
            return;
        };
        runtime.spawn(async move {
            let status = tokio::process::Command::new(program)
                .args(&args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await;
            if let Err(e) = status {
                tracing::debug!("{program}: {e}");
            }
        });
    }
}
