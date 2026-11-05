//! The composer's helpers that belong to the person at this keyboard rather
//! than to an environment: omp's native word prediction (ghost text) and
//! dictation from this machine's microphone. Both run on one omp of this
//! machine's own, whichever environment the thread is on: a remote host
//! has no microphone near the user, and a prediction per keystroke should
//! not cross the network.
//!
//! [`AssistOmp`] is that omp process (`omp --mode rpc --no-session`), started
//! on first use and again after it exits. [`Assist`] is its GPUI face: it
//! owns the one capture that may run at a time and reports dictation
//! progress as [`AssistEvent`]s to the composer that started it.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use gpui_kit::{App, AppContext as _, Context, Entity, EntityId, EventEmitter, Global, Task};
use j3_core::omp::{
    OmpLauncher, OmpSession,
    omp_rpc::{
        DictationStartCommand, DictationStateEventState, DictationStopCommand, Event,
        PredictWordCommand, PredictWordFeedbackCommand, RpcNotification,
    },
};
use tokio::sync::mpsc;

use crate::services::Services;

/// omp's settings for the assist process, layered over the user's
/// `config.yml`. `stt.enabled` gates omp's own push-to-talk on the space
/// bar; here the microphone button is the opt-in, so dictation is on.
const OVERLAY: &str = "# Written by j3: settings for the omp that serves the composer's\n# word prediction and dictation.\nstt:\n  enabled: true\n";

/// The launcher for this machine's assist omp: no tools (and so no MCP
/// servers), skills, rules, context files or extensions, since it never
/// runs a turn. Its settings overlay goes in `data_dir`, its stderr in
/// `data_dir/logs`.
pub fn launcher(data_dir: Option<&Path>) -> OmpLauncher {
    let mut launcher = OmpLauncher::new(PathBuf::from("omp"));
    for flag in [
        "--no-tools",
        "--no-lsp",
        "--no-skills",
        "--no-rules",
        "--no-context-files",
        "--no-extensions",
        "--no-title",
    ] {
        launcher = launcher.arg(flag);
    }
    let Some(data_dir) = data_dir else {
        return launcher;
    };
    let overlay = data_dir.join("omp-assist.yml");
    match std::fs::create_dir_all(data_dir.join("logs"))
        .and_then(|()| std::fs::write(&overlay, OVERLAY))
    {
        Ok(()) => {
            let mut config = std::ffi::OsString::from("--config=");
            config.push(&overlay);
            launcher
                .arg(config)
                .stderr_log(data_dir.join("logs/omp-assist.log"))
        }
        Err(e) => {
            tracing::warn!(
                "writing {}: {e}; dictation follows omp's stt.enabled",
                overlay.display()
            );
            launcher
        }
    }
}

/// Where a capture is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Loading the speech model and opening the microphone.
    Starting,
    Recording,
    /// The microphone is closed; the rest is being transcribed.
    Transcribing,
}

impl From<DictationStateEventState> for Phase {
    fn from(state: DictationStateEventState) -> Self {
        match state {
            DictationStateEventState::Starting => Self::Starting,
            DictationStateEventState::Recording => Self::Recording,
            DictationStateEventState::Transcribing => Self::Transcribing,
        }
    }
}

/// omp's dictation frames, and the process going away.
#[derive(Debug)]
enum Frame {
    State {
        capture: String,
        phase: Phase,
    },
    Transcript {
        capture: String,
        text: String,
    },
    End {
        capture: String,
        text: String,
        error: Option<String>,
    },
    Exited,
}

impl Frame {
    fn from_event(event: Event) -> Option<Self> {
        let Event::Notification(notification) = event else {
            return None;
        };
        Some(match notification {
            RpcNotification::DictationState(frame) => Self::State {
                capture: frame.capture_id,
                phase: frame.state.into(),
            },
            RpcNotification::DictationTranscript(frame) => Self::Transcript {
                capture: frame.capture_id,
                text: frame.text,
            },
            RpcNotification::DictationEnd(frame) => Self::End {
                capture: frame.capture_id,
                text: frame.text,
                error: frame.error,
            },
            _ => return None,
        })
    }
}

/// This machine's assist omp. Lives on the tokio runtime.
pub struct AssistOmp {
    launcher: OmpLauncher,
    cwd: PathBuf,
    session: tokio::sync::Mutex<Option<Arc<OmpSession>>>,
    /// One `predict_word` on the wire at a time. Each call runs on its own
    /// blocking thread, so concurrent ones could reach omp out of order, and
    /// omp answers whichever arrived first with `null` once a later one
    /// does: a stale draft could then cost the newest its answer.
    predicting: tokio::sync::Mutex<()>,
    /// Bumped by every prediction request; one that is no longer the
    /// newest when its turn comes is answered `None` without asking.
    prediction: AtomicU64,
    frames: mpsc::UnboundedSender<Frame>,
    /// Taken once, by [`Assist`].
    receiver: Mutex<Option<mpsc::UnboundedReceiver<Frame>>>,
}

impl AssistOmp {
    /// Runs `launcher` in `cwd` when first needed.
    pub fn new(launcher: OmpLauncher, cwd: PathBuf) -> Arc<Self> {
        let (frames, receiver) = mpsc::unbounded_channel();
        Arc::new(Self {
            launcher,
            cwd,
            session: tokio::sync::Mutex::new(None),
            predicting: tokio::sync::Mutex::new(()),
            prediction: AtomicU64::new(0),
            frames,
            receiver: Mutex::new(Some(receiver)),
        })
    }

    /// The running process, started if there is none.
    async fn session(self: &Arc<Self>) -> Result<Arc<OmpSession>, String> {
        let mut slot = self.session.lock().await;
        if let Some(session) = slot.as_ref() {
            return Ok(Arc::clone(session));
        }
        let (session, mut events) = self
            .launcher
            .start_utility(&self.cwd)
            .await
            .map_err(|e| format!("starting omp: {e}"))?;
        let session = Arc::new(session);
        *slot = Some(Arc::clone(&session));
        let this = Arc::downgrade(self);
        let started = Arc::downgrade(&session);
        tokio::spawn(async move {
            // Ends when the process exits or is closed.
            while let Some(event) = events.recv().await {
                if let (Some(frame), Some(this)) = (Frame::from_event(event), this.upgrade()) {
                    let _ = this.frames.send(frame);
                }
            }
            let Some(this) = this.upgrade() else {
                return;
            };
            let mut slot = this.session.lock().await;
            if slot
                .as_ref()
                .is_some_and(|current| Weak::as_ptr(&started) == Arc::as_ptr(current))
            {
                *slot = None;
            }
            let _ = this.frames.send(Frame::Exited);
        });
        Ok(session)
    }

    /// Starts the process ahead of the first request.
    pub async fn warm(self: Arc<Self>) {
        if let Err(e) = self.session().await {
            tracing::warn!("assist omp: {e}");
        }
    }

    /// omp's ghost-text suffix for the word ending at `cursor` (UTF-16).
    /// The request is numbered now, in the order requests are made, not
    /// when the runtime first polls it; one a newer request superseded
    /// while it waited answers `None` without asking.
    fn predict(
        self: Arc<Self>,
        text: String,
        cursor: usize,
    ) -> impl Future<Output = Result<Option<String>, String>> + Send + 'static {
        let request = self.prediction.fetch_add(1, Ordering::SeqCst) + 1;
        async move {
            let _turn = self.predicting.lock().await;
            if self.prediction.load(Ordering::SeqCst) != request {
                return Ok(None);
            }
            let session = self.session().await?;
            session
                .call(PredictWordCommand {
                    text,
                    cursor: cursor as i64,
                })
                .await
                .map_err(|e| e.to_string())
        }
    }

    async fn feedback(
        self: Arc<Self>,
        text: String,
        cursor: usize,
        suggestion: String,
        accepted: bool,
    ) -> Result<(), String> {
        let session = self.session().await?;
        session
            .call(PredictWordFeedbackCommand {
                text,
                cursor: cursor as i64,
                suggestion,
                accepted,
            })
            .await
            .map_err(|e| e.to_string())
    }

    async fn start_dictation(self: Arc<Self>, capture: String) -> Result<(), String> {
        let session = self.session().await?;
        session
            .call(DictationStartCommand {
                capture_id: capture,
            })
            .await
            .map_err(|e| e.to_string())
    }

    async fn stop_dictation(self: Arc<Self>) -> Result<String, String> {
        let session = self.session().await?;
        session
            .call(DictationStopCommand {})
            .await
            .map_err(|e| e.to_string())
    }

    /// Stops the process, if one runs and no call is in flight on it (one
    /// that is ends with the app).
    pub async fn close(&self) {
        let session = self.session.lock().await.take();
        if let Some(session) = session.and_then(Arc::into_inner)
            && let Err(e) = session.close().await
        {
            tracing::warn!("closing the assist omp: {e}");
        }
    }
}

/// What the composer that started a capture hears about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssistEvent {
    Phase {
        capture: String,
        phase: Phase,
    },
    /// The whole transcript so far; it replaces the previous one.
    Transcript {
        capture: String,
        text: String,
    },
    /// The capture is over: its final text, and why it failed if it did.
    End {
        capture: String,
        text: String,
        error: Option<String>,
    },
}

struct Capture {
    id: String,
    owner: EntityId,
    phase: Phase,
    /// A stop was sent: a start that fails now was cancelled by it, and the
    /// stop's result (or omp's `dictation_end`) ends the capture.
    stopping: bool,
}

pub struct Assist {
    omp: Arc<AssistOmp>,
    capture: Option<Capture>,
    next_capture: u64,
    _frames: Task<()>,
}

impl EventEmitter<AssistEvent> for Assist {}

struct GlobalAssist(Entity<Assist>);

impl Global for GlobalAssist {}

impl Assist {
    /// The app's assist, created on first use.
    pub fn global(cx: &mut App) -> Entity<Self> {
        if let Some(assist) = cx.try_global::<GlobalAssist>() {
            return assist.0.clone();
        }
        let omp = Arc::clone(Services::global(cx).assist());
        let receiver = omp
            .receiver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let assist = cx.new(|cx: &mut Context<Self>| {
            let frames = cx.spawn(async move |this, cx| {
                let Some(mut receiver) = receiver else {
                    return;
                };
                while let Some(frame) = receiver.recv().await {
                    if this
                        .update(cx, |this, cx| this.on_frame(frame, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            });
            Self {
                omp,
                capture: None,
                next_capture: 0,
                _frames: frames,
            }
        });
        cx.set_global(GlobalAssist(assist.clone()));
        assist
    }

    /// Starts the assist omp now, so the first keystroke already gets a
    /// prediction.
    pub fn warm(&self, cx: &App) {
        // Runs on tokio whether or not the result is awaited.
        drop(Services::global(cx).run(Arc::clone(&self.omp).warm()));
    }

    /// omp's ghost-text suffix for the draft `text` with the caret at
    /// `cursor` (UTF-16). `None` when omp has none or cannot answer (it
    /// backs off for a while after failing), so failures never reach the
    /// user per keystroke.
    pub fn predict(
        &self,
        text: String,
        cursor: usize,
        cx: &App,
    ) -> impl Future<Output = Option<String>> + use<> {
        let answer = Services::global(cx).run(Arc::clone(&self.omp).predict(text, cursor));
        async move {
            match answer.await? {
                Ok(suffix) => suffix.filter(|suffix| !suffix.is_empty()),
                Err(e) => {
                    tracing::debug!("predict_word: {e}");
                    None
                }
            }
        }
    }

    /// Tells omp's predictor whether the suggestion shown after `cursor`
    /// (UTF-16) in `text` was taken or typed past; it learns from both.
    pub fn feedback(
        &self,
        text: String,
        cursor: usize,
        suggestion: String,
        accepted: bool,
        cx: &App,
    ) {
        let omp = Arc::clone(&self.omp);
        drop(Services::global(cx).run(async move {
            if let Err(e) = omp.feedback(text, cursor, suggestion, accepted).await {
                tracing::debug!("predict_word_feedback: {e}");
            }
        }));
    }

    /// The running capture's composer and phase.
    pub fn capture(&self) -> Option<(EntityId, Phase)> {
        self.capture.as_ref().map(|c| (c.owner, c.phase))
    }

    /// Starts dictating for the composer `owner`. Returns the capture's id,
    /// which its [`AssistEvent`]s carry, or `None` while another capture
    /// runs (one microphone, one capture).
    pub fn start_dictation(&mut self, owner: EntityId, cx: &mut Context<Self>) -> Option<String> {
        if self.capture.is_some() {
            return None;
        }
        self.next_capture += 1;
        let id = format!("j3-{}-{}", std::process::id(), self.next_capture);
        self.capture = Some(Capture {
            id: id.clone(),
            owner,
            phase: Phase::Starting,
            stopping: false,
        });
        let started = Services::global(cx).run(Arc::clone(&self.omp).start_dictation(id.clone()));
        let ended = id.clone();
        cx.spawn(async move |this, cx| {
            let id = ended;
            let error = match started.await {
                Some(Ok(())) => return,
                Some(Err(e)) => e,
                None => "the app is shutting down".to_owned(),
            };
            let _ = this.update(cx, |this, cx| {
                if this
                    .capture
                    .as_ref()
                    .is_some_and(|c| c.id == id && !c.stopping)
                {
                    this.end(&id, String::new(), Some(error), cx);
                }
            });
        })
        .detach();
        cx.notify();
        Some(id)
    }

    /// Stops the running capture, cancelling it if it is still starting;
    /// what was said so far is still transcribed.
    pub fn stop_dictation(&mut self, cx: &mut Context<Self>) {
        let Some(capture) = self.capture.as_mut().filter(|c| !c.stopping) else {
            return;
        };
        capture.stopping = true;
        let id = capture.id.clone();
        let stopped = Services::global(cx).run(Arc::clone(&self.omp).stop_dictation());
        cx.spawn(async move |this, cx| {
            let (text, error) = match stopped.await {
                Some(Ok(text)) => (text, None),
                Some(Err(e)) => (String::new(), Some(e)),
                None => (String::new(), Some("the app is shutting down".to_owned())),
            };
            let _ = this.update(cx, |this, cx| this.end(&id, text, error, cx));
        })
        .detach();
    }

    fn on_frame(&mut self, frame: Frame, cx: &mut Context<Self>) {
        match frame {
            Frame::State { capture, phase } => {
                let Some(current) = self.capture.as_mut().filter(|c| c.id == capture) else {
                    return;
                };
                current.phase = phase;
                cx.emit(AssistEvent::Phase { capture, phase });
                cx.notify();
            }
            Frame::Transcript { capture, text } => {
                if self.capture.as_ref().is_some_and(|c| c.id == capture) {
                    cx.emit(AssistEvent::Transcript { capture, text });
                }
            }
            Frame::End {
                capture,
                text,
                error,
            } => self.end(&capture, text, error, cx),
            Frame::Exited => {
                if let Some(capture) = self.capture.as_ref().map(|c| c.id.clone()) {
                    self.end(&capture, String::new(), Some("omp exited".to_owned()), cx);
                }
            }
        }
    }

    /// Ends the capture `id`, once: omp's `dictation_end` frame and the
    /// stop's response carry the same text, whichever comes first.
    fn end(&mut self, id: &str, text: String, error: Option<String>, cx: &mut Context<Self>) {
        if self.capture.as_ref().is_none_or(|c| c.id != id) {
            return;
        }
        self.capture = None;
        cx.emit(AssistEvent::End {
            capture: id.to_owned(),
            text,
            error,
        });
        cx.notify();
    }
}
