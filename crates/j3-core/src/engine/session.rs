//! The slice of an omp session a thread actor uses, behind a trait so actor
//! logic (queue, steer, stop, answers) is testable without a real omp.

use std::path::{Path, PathBuf};

use futures_util::future::BoxFuture;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::omp::{
    self, OmpLauncher, OmpSession,
    omp_rpc::{
        AbortCommand, AvailableSlashCommand, Command, Event, ExtensionUiResponse,
        GetAvailableCommandsCommand, GetStateCommand, ImageContent, PromptAck, PromptCommand,
        SessionState, SetModelCommand, SetThinkingLevelCommand, SteerCommand, ThinkingLevel,
    },
};

pub type Events = mpsc::UnboundedReceiver<Event>;
pub type Result<T, E = omp::Error> = std::result::Result<T, E>;

/// One running omp process bound to one thread.
pub trait AgentSession: Send + Sync + 'static {
    /// Transcript path, stored in the thread index for resume.
    fn session_file(&self) -> &Path;
    fn prompt(
        &self,
        message: String,
        images: Vec<ImageContent>,
    ) -> BoxFuture<'_, Result<PromptAck>>;
    fn steer(&self, message: String, images: Vec<ImageContent>) -> BoxFuture<'_, Result<()>>;
    fn abort(&self) -> BoxFuture<'_, Result<()>>;
    fn set_model(&self, provider: String, model_id: String) -> BoxFuture<'_, Result<()>>;
    fn set_thinking_level(&self, level: ThinkingLevel) -> BoxFuture<'_, Result<()>>;
    fn get_state(&self) -> BoxFuture<'_, Result<SessionState>>;
    fn available_commands(&self) -> BoxFuture<'_, Result<Vec<AvailableSlashCommand>>>;
    fn respond_ui(&self, response: ExtensionUiResponse) -> Result<()>;
    fn close(self: Box<Self>) -> BoxFuture<'static, ()>;
}

/// Starts sessions for threads.
pub trait SessionFactory: Send + Sync + 'static {
    fn start(
        &self,
        cwd: PathBuf,
        resume: Option<PathBuf>,
    ) -> BoxFuture<'static, Result<(Box<dyn AgentSession>, Events)>>;
}

/// `set_event_filter` with `messageUpdates: "delta"`: every event, with
/// `message_update` frames reduced to their deltas. The generated
/// `SetEventFilterCommand` predates the `messageUpdates` field.
#[derive(Debug, Clone, Serialize)]
struct SetEventFilterDelta {
    events: Option<Vec<String>>,
    #[serde(rename = "messageUpdates")]
    message_updates: &'static str,
}

impl Command for SetEventFilterDelta {
    const NAME: &'static str = "set_event_filter";
    const TIMEOUT_MS: Option<u64> = None;
    type Output = Value;

    fn decode(data: Option<Value>) -> std::result::Result<Self::Output, serde_json::Error> {
        Ok(data.unwrap_or(Value::Null))
    }
}

struct Omp(OmpSession);

impl AgentSession for Omp {
    fn session_file(&self) -> &Path {
        self.0.session_file()
    }

    fn prompt(
        &self,
        message: String,
        images: Vec<ImageContent>,
    ) -> BoxFuture<'_, Result<PromptAck>> {
        Box::pin(self.0.call(PromptCommand {
            message,
            images: (!images.is_empty()).then_some(images),
            streaming_behavior: None,
        }))
    }

    fn steer(&self, message: String, images: Vec<ImageContent>) -> BoxFuture<'_, Result<()>> {
        Box::pin(self.0.call(SteerCommand {
            message,
            images: (!images.is_empty()).then_some(images),
        }))
    }

    fn abort(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(self.0.call(AbortCommand {}))
    }

    fn set_model(&self, provider: String, model_id: String) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            self.0.call(SetModelCommand { provider, model_id }).await?;
            Ok(())
        })
    }

    fn set_thinking_level(&self, level: ThinkingLevel) -> BoxFuture<'_, Result<()>> {
        Box::pin(self.0.call(SetThinkingLevelCommand { level }))
    }

    fn get_state(&self) -> BoxFuture<'_, Result<SessionState>> {
        Box::pin(self.0.call(GetStateCommand {}))
    }

    fn available_commands(&self) -> BoxFuture<'_, Result<Vec<AvailableSlashCommand>>> {
        Box::pin(self.0.call(GetAvailableCommandsCommand {}))
    }

    fn respond_ui(&self, response: ExtensionUiResponse) -> Result<()> {
        self.0.respond_ui(response)
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            if let Err(error) = self.0.close().await {
                tracing::warn!(%error, "closing omp session");
            }
        })
    }
}

/// Real omp processes from an [`OmpLauncher`].
pub struct OmpFactory(pub OmpLauncher);

impl SessionFactory for OmpFactory {
    fn start(
        &self,
        cwd: PathBuf,
        resume: Option<PathBuf>,
    ) -> BoxFuture<'static, Result<(Box<dyn AgentSession>, Events)>> {
        let launcher = self.0.clone();
        Box::pin(async move {
            let (session, events) = launcher.start(&cwd, resume.as_deref()).await?;
            // Older omp builds reject or ignore `messageUpdates`; the mapper
            // reads deltas in both modes, so a failure here is not fatal.
            if let Err(error) = session
                .call(SetEventFilterDelta {
                    events: None,
                    message_updates: "delta",
                })
                .await
            {
                tracing::debug!(%error, "omp set_event_filter delta mode unavailable");
            }
            Ok((Box::new(Omp(session)) as Box<dyn AgentSession>, events))
        })
    }
}
