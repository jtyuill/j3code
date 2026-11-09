//! Protocol v2 on the server side (`docs/protocol/wire.md`): the WebSocket
//! session, the method router, and the omp → v2 mapping shared by the live
//! engine and the session replay.

pub mod driver;
pub mod replay;
pub mod router;
pub mod tools;
