//! Protocol v2: j3's own wire protocol (`docs/protocol/wire.md`).

pub mod entities;
pub mod envelope;
pub mod methods;
pub mod streams;

/// The protocol version: `GET /api/v1/info` `protocol`, and the
/// `protocol` query parameter of the WebSocket upgrade.
pub const PROTOCOL: u32 = 2;
