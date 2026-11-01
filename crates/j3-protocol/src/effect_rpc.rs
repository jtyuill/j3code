//! Effect RPC (effect@4.0.5, `effect/rpc`) messages as encoded by
//! `RpcSerialization.layerJson` over a WebSocket.
//!
//! Every WebSocket text frame carries one JSON message, or a JSON array of
//! messages (the JSON parser accepts both; Effect itself always sends a single
//! object per frame). See `docs/protocol/effect-rpc.md` for semantics.
//!
//! Payloads, success values, errors and defects stay as [`Value`]; typing them
//! per method is the caller's job.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// Request id. The Effect client allocates incrementing numbers; the server
/// accepts strings too and echoes the id back in the same JSON type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(u64),
    String(String),
}

impl From<u64> for RequestId {
    fn from(id: u64) -> Self {
        Self::Number(id)
    }
}

impl From<String> for RequestId {
    fn from(id: String) -> Self {
        Self::String(id)
    }
}

impl From<&str> for RequestId {
    fn from(id: &str) -> Self {
        Self::String(id.to_owned())
    }
}

/// `Request` frame (`RpcMessage.RequestEncoded`). Sent client→server; the
/// server→client union also allows it for bidirectional protocols.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub id: RequestId,
    /// RPC method name (`Rpc.make(tag, ...)`).
    pub tag: String,
    /// Payload encoded by the method's payload schema.
    pub payload: Value,
    /// Ordered header pairs, encoded as `[[name, value], ...]`.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// `true` for fire-and-forget requests; the server sends no response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_notification: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampled: Option<bool>,
}

/// Client→server message (`RpcMessage.FromClientEncoded`).
#[derive(Debug, Clone, PartialEq)]
pub enum ClientMessage {
    Request(Request),
    /// Client consumed one `Chunk` of a stream; releases the next one.
    Ack {
        request_id: RequestId,
    },
    /// Cancel an in-flight request. The server answers with an interrupt `Exit`.
    Interrupt {
        request_id: RequestId,
    },
    /// Keepalive; the server answers `Pong`.
    Ping,
    /// Client will send no more requests (not sent by the WebSocket client).
    Eof,
    /// Any message with an unrecognised `_tag`, kept verbatim.
    Unknown(Value),
}

/// Server→client message (`RpcMessage.FromServerEncoded`).
#[derive(Debug, Clone, PartialEq)]
pub enum ServerMessage {
    /// One batch of stream elements for a `stream: true` request.
    Chunk {
        request_id: RequestId,
        values: Vec<Value>,
    },
    /// Terminal response for a request.
    Exit { request_id: RequestId, exit: Exit },
    /// Connection-level defect not attributable to a request; fails every
    /// pending request on the client.
    Defect { defect: Value },
    /// Reply to `Ping`.
    Pong,
    /// Produced inside the client protocol only (socket errors); never sent by
    /// an Effect server, decoded here for completeness.
    ClientProtocolError { error: Value },
    /// Server-initiated request (bidirectional protocols only).
    Request(Request),
    /// Any message with an unrecognised `_tag`, kept verbatim.
    Unknown(Value),
}

/// Encoded `Exit` (`RpcMessage.ExitEncoded`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum Exit {
    /// `value` is the success schema's encoding; `null` for void and streams.
    Success {
        #[serde(default)]
        value: Value,
    },
    Failure {
        cause: Cause,
    },
}

/// Encoded `Cause`: a flat list of reasons (effect 4 has no
/// Sequential/Parallel nodes).
pub type Cause = Vec<CauseReason>;

/// One reason inside a [`Cause`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum CauseReason {
    /// Typed failure, encoded by the method's error schema.
    Fail { error: Value },
    /// Defect, encoded by `Schema.Defect` (`Error` → `{name, message}`, other
    /// values as-is).
    Die { defect: Value },
    /// Interruption; `fiberId` is omitted when unknown.
    Interrupt {
        #[serde(rename = "fiberId", default, skip_serializing_if = "Option::is_none")]
        fiber_id: Option<i64>,
    },
}

impl Exit {
    pub fn success(value: Value) -> Self {
        Self::Success { value }
    }

    pub fn fail(error: Value) -> Self {
        Self::Failure {
            cause: vec![CauseReason::Fail { error }],
        }
    }

    pub fn die(defect: Value) -> Self {
        Self::Failure {
            cause: vec![CauseReason::Die { defect }],
        }
    }

    pub fn interrupt(fiber_id: Option<i64>) -> Self {
        Self::Failure {
            cause: vec![CauseReason::Interrupt { fiber_id }],
        }
    }
}

impl ClientMessage {
    pub fn ack(request_id: impl Into<RequestId>) -> Self {
        Self::Ack {
            request_id: request_id.into(),
        }
    }

    pub fn interrupt(request_id: impl Into<RequestId>) -> Self {
        Self::Interrupt {
            request_id: request_id.into(),
        }
    }
}

impl ServerMessage {
    pub fn chunk(request_id: impl Into<RequestId>, values: Vec<Value>) -> Self {
        Self::Chunk {
            request_id: request_id.into(),
            values,
        }
    }

    pub fn exit(request_id: impl Into<RequestId>, exit: Exit) -> Self {
        Self::Exit {
            request_id: request_id.into(),
            exit,
        }
    }

    pub fn exit_success(request_id: impl Into<RequestId>, value: Value) -> Self {
        Self::exit(request_id, Exit::success(value))
    }

    pub fn exit_fail(request_id: impl Into<RequestId>, error: Value) -> Self {
        Self::exit(request_id, Exit::fail(error))
    }

    pub fn exit_die(request_id: impl Into<RequestId>, defect: Value) -> Self {
        Self::exit(request_id, Exit::die(defect))
    }

    pub fn exit_interrupt(request_id: impl Into<RequestId>) -> Self {
        Self::exit(request_id, Exit::interrupt(None))
    }

    pub fn defect(defect: Value) -> Self {
        Self::Defect { defect }
    }

    pub fn pong() -> Self {
        Self::Pong
    }
}

/// Decode one WebSocket text frame into client messages (object or array).
pub fn decode_client_frame(frame: &str) -> serde_json::Result<Vec<ClientMessage>> {
    decode_frame(frame)
}

/// Decode one WebSocket text frame into server messages (object or array).
pub fn decode_server_frame(frame: &str) -> serde_json::Result<Vec<ServerMessage>> {
    decode_frame(frame)
}

fn decode_frame<T: for<'de> Deserialize<'de>>(frame: &str) -> serde_json::Result<Vec<T>> {
    match serde_json::from_str::<Value>(frame)? {
        Value::Array(items) => items.into_iter().map(serde_json::from_value).collect(),
        single => Ok(vec![serde_json::from_value(single)?]),
    }
}

// Wire representations. Known tags go through these; anything else is kept
// as `Unknown(Value)`.

#[derive(Deserialize)]
#[serde(tag = "_tag")]
enum ClientWire {
    Request(Request),
    Ack {
        #[serde(rename = "requestId")]
        request_id: RequestId,
    },
    Interrupt {
        #[serde(rename = "requestId")]
        request_id: RequestId,
    },
    Ping,
    Eof,
}

#[derive(Deserialize)]
#[serde(tag = "_tag")]
enum ServerWire {
    Chunk {
        #[serde(rename = "requestId")]
        request_id: RequestId,
        values: Vec<Value>,
    },
    Exit {
        #[serde(rename = "requestId")]
        request_id: RequestId,
        exit: Exit,
    },
    Defect {
        defect: Value,
    },
    Pong,
    ClientProtocolError {
        error: Value,
    },
    Request(Request),
}

const CLIENT_TAGS: &[&str] = &["Request", "Ack", "Interrupt", "Ping", "Eof"];
const SERVER_TAGS: &[&str] = &[
    "Chunk",
    "Exit",
    "Defect",
    "Pong",
    "ClientProtocolError",
    "Request",
];

fn is_known(value: &Value, tags: &[&str]) -> bool {
    value
        .get("_tag")
        .and_then(Value::as_str)
        .is_some_and(|tag| tags.contains(&tag))
}

#[derive(Serialize)]
#[serde(tag = "_tag")]
enum ClientWireRef<'a> {
    Request(&'a Request),
    Ack {
        #[serde(rename = "requestId")]
        request_id: &'a RequestId,
    },
    Interrupt {
        #[serde(rename = "requestId")]
        request_id: &'a RequestId,
    },
    Ping,
    Eof,
}

#[derive(Serialize)]
#[serde(tag = "_tag")]
enum ServerWireRef<'a> {
    Chunk {
        #[serde(rename = "requestId")]
        request_id: &'a RequestId,
        values: &'a [Value],
    },
    Exit {
        #[serde(rename = "requestId")]
        request_id: &'a RequestId,
        exit: &'a Exit,
    },
    Defect {
        defect: &'a Value,
    },
    Pong,
    ClientProtocolError {
        error: &'a Value,
    },
    Request(&'a Request),
}

impl Serialize for ClientMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let wire = match self {
            Self::Request(request) => ClientWireRef::Request(request),
            Self::Ack { request_id } => ClientWireRef::Ack { request_id },
            Self::Interrupt { request_id } => ClientWireRef::Interrupt { request_id },
            Self::Ping => ClientWireRef::Ping,
            Self::Eof => ClientWireRef::Eof,
            Self::Unknown(value) => return value.serialize(serializer),
        };
        wire.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ClientMessage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        if !is_known(&value, CLIENT_TAGS) {
            return Ok(Self::Unknown(value));
        }
        Ok(
            match serde_json::from_value(value).map_err(D::Error::custom)? {
                ClientWire::Request(request) => Self::Request(request),
                ClientWire::Ack { request_id } => Self::Ack { request_id },
                ClientWire::Interrupt { request_id } => Self::Interrupt { request_id },
                ClientWire::Ping => Self::Ping,
                ClientWire::Eof => Self::Eof,
            },
        )
    }
}

impl Serialize for ServerMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let wire = match self {
            Self::Chunk { request_id, values } => ServerWireRef::Chunk { request_id, values },
            Self::Exit { request_id, exit } => ServerWireRef::Exit { request_id, exit },
            Self::Defect { defect } => ServerWireRef::Defect { defect },
            Self::Pong => ServerWireRef::Pong,
            Self::ClientProtocolError { error } => ServerWireRef::ClientProtocolError { error },
            Self::Request(request) => ServerWireRef::Request(request),
            Self::Unknown(value) => return value.serialize(serializer),
        };
        wire.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ServerMessage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        if !is_known(&value, SERVER_TAGS) {
            return Ok(Self::Unknown(value));
        }
        Ok(
            match serde_json::from_value(value).map_err(D::Error::custom)? {
                ServerWire::Chunk { request_id, values } => Self::Chunk { request_id, values },
                ServerWire::Exit { request_id, exit } => Self::Exit { request_id, exit },
                ServerWire::Defect { defect } => Self::Defect { defect },
                ServerWire::Pong => Self::Pong,
                ServerWire::ClientProtocolError { error } => Self::ClientProtocolError { error },
                ServerWire::Request(request) => Self::Request(request),
            },
        )
    }
}
