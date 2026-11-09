//! Frames and errors of protocol v2 (`docs/protocol/wire.md` §3).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Stream window a new stream starts with, in `items` frames (wire.md §3.2).
pub const INITIAL_WINDOW: u32 = 8;
/// Interval between client pings, in seconds (wire.md §3.3).
pub const PING_INTERVAL_SECS: u64 = 5;
/// Silence after which a client reconnects, in seconds.
pub const CLIENT_TIMEOUT_SECS: u64 = 15;
/// Silence after which the server closes a connection, in seconds.
pub const SERVER_TIMEOUT_SECS: u64 = 30;

/// Id of one call, unique among a connection's open calls.
pub type CallId = u64;

/// Client → server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientFrame {
    Call {
        id: CallId,
        method: String,
        #[serde(default = "empty_params")]
        params: Value,
    },
    Ack {
        id: CallId,
        n: u32,
    },
    Cancel {
        id: CallId,
    },
    Ping,
}

fn empty_params() -> Value {
    Value::Object(Default::default())
}

/// Server → client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerFrame {
    Ok {
        id: CallId,
        result: Value,
    },
    Items {
        id: CallId,
        items: Vec<Value>,
    },
    End {
        id: CallId,
    },
    /// `id: None` is a connection-level error; the server closes the socket
    /// after sending it.
    Err {
        id: Option<CallId>,
        error: Error,
    },
    Pong,
}

/// wire.md §3.1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::BadRequest, message)
    }

    pub fn rejected(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Rejected, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    /// `not_found` with `data: {kind, id}`.
    pub fn not_found(kind: &str, id: &str) -> Self {
        Self::new(ErrorCode::NotFound, format!("No {kind} {id}."))
            .with_data(serde_json::json!({"kind": kind, "id": id}))
    }
}

impl std::error::Error for Error {}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

/// wire.md §3.1. Unknown codes from a newer server decode to `Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    UnknownMethod,
    NotFound,
    Rejected,
    Conflict,
    TooLarge,
    Unavailable,
    Cancelled,
    Internal,
    #[serde(other)]
    Other,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BadRequest => "bad_request",
            Self::UnknownMethod => "unknown_method",
            Self::NotFound => "not_found",
            Self::Rejected => "rejected",
            Self::Conflict => "conflict",
            Self::TooLarge => "too_large",
            Self::Unavailable => "unavailable",
            Self::Cancelled => "cancelled",
            Self::Internal => "internal",
            Self::Other => "other",
        }
    }
}
