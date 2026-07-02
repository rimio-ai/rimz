//! Liveness heartbeat written by resolver clients.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::ids::{ResolverId, WorkspaceId};

pub const RESOLVER_PROTOCOL_VERSION: &str = "rimz.resolver.v1";

/// What a resolver writes to advertise that it's alive and listening.
///
/// `capabilities` is informational in v0 — the bridge engages whenever a
/// fresh, allowlisted heartbeat exists. A resolver that declines just doesn't
/// call `feed resolve`.
///
/// `pid` is consumed by the binary-pin verifier when the allowlist entry
/// carries `--binary <path>`. Resolvers running with a pin should publish
/// their process id so Rimz can readlink `/proc/<pid>/exe` (Linux) and
/// confirm the executable matches.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolverHeartbeat {
    pub protocol_version: String,
    pub workspace_id: WorkspaceId,
    pub resolver_id: ResolverId,
    pub display_name: Option<String>,
    pub capabilities: Vec<String>,
    pub last_seen: Timestamp,
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

impl ResolverHeartbeat {
    pub fn new(workspace_id: WorkspaceId, resolver_id: ResolverId) -> Self {
        Self {
            protocol_version: RESOLVER_PROTOCOL_VERSION.to_owned(),
            workspace_id,
            resolver_id,
            display_name: None,
            capabilities: Vec::new(),
            last_seen: Timestamp::now(),
            version: None,
            pid: None,
        }
    }
}
