//! Wire protocol between j3 clients (desktop, iOS) and j3 servers.
//!
//! Frames follow Effect RPC's JSON serialization so the forked iOS client
//! (`@t3tools/client-runtime`) can talk to the Rust server unchanged.

pub mod effect_rpc;

pub mod t3;
