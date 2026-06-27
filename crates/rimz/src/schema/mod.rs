//! Wire schemas for everything Rimz puts on disk or on a socket.
//!
//! Protocol versions are pinned here as constants; bump them in lockstep with
//! the corresponding schema change and update `docs/internals/sidebar/ledger.md`.

pub mod diag;
pub mod event;
pub mod heartbeat;
pub mod notify_trace;
pub mod pane_topology;
pub mod sidebar_event;

// v2: `agent.lifecycle` params carry a `signal` (the agent-agnostic lifecycle
// intent folded through `agents::lifecycle::step`) in place of the legacy bare
// `status` + `compacting`; signal-less lifecycle frames fold to nothing.
pub const EVENT_SCHEMA_VERSION: &str = "rimz.event.v2";
// v5: the snapshot view-model carries explicit named-channel identity on agent
// rows and panes. v4 carried `root_class`, and the worktree-group kind
// vocabulary was `worktree`/`root`/`external` (the catch-all renamed from
// `workspace`). The version gate keeps a mixed-version fleet from honouring
// each other's elders mid-upgrade; a consumer that cannot parse a published
// snapshot already falls back to its own produce.
pub const SIDEBAR_PROTOCOL_VERSION: &str = "rimz.plugin.v5";
pub const RESOLVER_PROTOCOL_VERSION: &str = "rimz.resolver.v1";
