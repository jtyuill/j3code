use serde_json::Value;

/// Why a call failed. `Display` is a human-readable sentence.
#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    /// The server answered with a typed failure (`Exit` `Fail`), e.g.
    /// `OrchestrationV2DispatchCommandError`. `message` is the error's own
    /// message when it has one.
    #[error("{message}")]
    Rejected {
        /// The error's `_tag`, when present.
        tag: Option<String>,
        message: String,
        /// The encoded error as received.
        error: Value,
    },
    /// The server failed unexpectedly (`Exit` `Die` or a connection-level
    /// `Defect`).
    #[error("server error: {0}")]
    Defect(String),
    /// The server interrupted the request.
    #[error("the request was interrupted")]
    Interrupted,
    /// No live connection to the server (not yet connected, or lost while the
    /// request was in flight).
    #[error("not connected: {0}")]
    Disconnected(String),
    /// An HTTP request (descriptor, upload) failed.
    #[error("{0}")]
    Http(String),
    /// A response did not have the expected shape.
    #[error("unexpected response from the server: {0}")]
    Decode(String),
    /// [`crate::Connection::close`] was called.
    #[error("the connection is closed")]
    Closed,
}

impl Error {
    /// Classifies an encoded typed failure.
    pub(crate) fn rejected(error: Value) -> Self {
        let tag = error.get("_tag").and_then(Value::as_str).map(str::to_owned);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .filter(|message| !message.trim().is_empty())
            .map(str::to_owned)
            .or_else(|| tag.clone())
            .unwrap_or_else(|| error.to_string());
        Self::Rejected {
            tag,
            message,
            error,
        }
    }

    /// Classifies an encoded defect (`Schema.Defect`: `{name, message}` or any value).
    pub(crate) fn defect(defect: &Value) -> Self {
        let message = match defect {
            Value::String(text) => text.clone(),
            other => other
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| other.to_string()),
        };
        Self::Defect(message)
    }
}
