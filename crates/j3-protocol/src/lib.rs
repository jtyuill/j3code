//! j3's wire protocol between j3 clients (desktop, iOS) and j3 servers
//! (`docs/protocol/wire.md`). Its types live in [`v2`] and are re-exported
//! here.

pub mod v2;

pub use v2::*;
