//! Wire schemas for everything Rimz puts on disk or on a socket.
//!
//! Protocol versions are pinned here as constants; bump them in lockstep with
//! the corresponding schema change and update `docs/internals/ledger.md`.

pub mod event;
pub mod heartbeat;

// v2: `agent.lifecycle` params carry a `signal` (the agent-agnostic lifecycle
// intent folded through `agents::lifecycle::step`) in place of the legacy bare
// `status` + `compacting`. The reducer tolerantly decodes either form.
pub const EVENT_SCHEMA_VERSION: &str = "rimz.event.v2";
pub const SIDEBAR_PROTOCOL_VERSION: &str = "rimz.plugin.v3";
pub const RESOLVER_PROTOCOL_VERSION: &str = "rimz.resolver.v1";
