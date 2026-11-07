//! The composer's word prediction (ghost text), from omp's native predictor
//! on an omp of this machine's own, whichever environment the thread is on:
//! a prediction per keystroke should not cross the network.
//!
//! [`AssistOmp`] is that omp process (`omp --mode rpc --no-session`), started
//! on first use and again after it exits. The `J3` methods below are its UI
//! face: they run it on tokio and post answers back through the inbox.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use j3_core::omp::{
    OmpLauncher, OmpSession,
    omp_rpc::{PredictWordCommand, PredictWordFeedbackCommand},
};
use rune::Tx;

use crate::app::J3;

/// The launcher for this machine's assist omp: no tools (and so no MCP
/// servers), skills, rules, context files or extensions, since it never
/// runs a turn. Its stderr goes in `data_dir/logs`.
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
    match std::fs::create_dir_all(data_dir.join("logs")) {
        Ok(()) => launcher.stderr_log(data_dir.join("logs/omp-assist.log")),
        Err(e) => {
            tracing::warn!("creating {}: {e}", data_dir.join("logs").display());
            launcher
        }
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
}

impl AssistOmp {
    /// Runs `launcher` in `cwd` when first needed.
    pub fn new(launcher: OmpLauncher, cwd: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            launcher,
            cwd,
            session: tokio::sync::Mutex::new(None),
            predicting: tokio::sync::Mutex::new(()),
            prediction: AtomicU64::new(0),
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
            while events.recv().await.is_some() {}
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

impl J3 {
    /// Starts the assist omp now, so the first keystroke already gets a
    /// prediction.
    pub fn warm_assist(&self) {
        let omp = Arc::clone(&self.services.assist);
        self.services.runtime.spawn(omp.warm());
    }

    /// Asks omp for the ghost-text suffix of the draft `text` with the caret
    /// at `cursor` (UTF-16) and applies `then` with the answer: `None` when
    /// omp has none or cannot answer (it backs off for a while after
    /// failing), so failures never reach the user per keystroke.
    pub fn predict_word(
        &self,
        text: String,
        cursor: usize,
        then: impl FnOnce(&mut J3, Option<String>, &mut Tx) + Send + 'static,
    ) {
        let omp = Arc::clone(&self.services.assist);
        self.spawn(async move {
            let suffix = match omp.predict(text, cursor).await {
                Ok(suffix) => suffix.filter(|suffix| !suffix.is_empty()),
                Err(e) => {
                    tracing::debug!("predict_word: {e}");
                    None
                }
            };
            move |s: &mut J3, tx: &mut Tx| then(s, suffix, tx)
        });
    }

    /// Tells omp's predictor whether the suggestion shown after `cursor`
    /// (UTF-16) in `text` was taken or typed past; it learns from both.
    pub fn predict_word_feedback(
        &self,
        text: String,
        cursor: usize,
        suggestion: String,
        accepted: bool,
    ) {
        let omp = Arc::clone(&self.services.assist);
        self.services.runtime.spawn(async move {
            if let Err(e) = omp.feedback(text, cursor, suggestion, accepted).await {
                tracing::debug!("predict_word_feedback: {e}");
            }
        });
    }
}
