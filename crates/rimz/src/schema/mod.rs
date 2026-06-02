//! Wire schemas for everything Rimz puts on disk or on a socket.
//!
//! Protocol versions are pinned here as constants; bump them in lockstep with
//! the corresponding schema change and update `docs/internals/ledger.md`.

pub mod event;
pub mod heartbeat;

pub const EVENT_SCHEMA_VERSION: &str = "rimz.event.v1";
pub const SIDEBAR_PROTOCOL_VERSION: &str = "rimz.plugin.v3";
pub const RESOLVER_PROTOCOL_VERSION: &str = "rimz.resolver.v1";
