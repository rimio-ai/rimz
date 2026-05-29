//! Liveness heartbeats: sidebar instances and resolver clients.

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::ids::{MuxName, ResolverId, SidebarInstanceId, WorkspaceId};
use crate::schema::{RESOLVER_PROTOCOL_VERSION, SIDEBAR_PROTOCOL_VERSION};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SidebarHeartbeat {
    pub protocol_version: String,
    pub workspace_id: WorkspaceId,
    pub instance_id: SidebarInstanceId,
    pub mux: MuxName,
    /// Multiplexer session this sidebar is pinned to. The ledger wakeup walk
    /// uses it to address backend-specific fast paths (e.g. the broadcast
    /// `zellij pipe` on the Zellij backend).
    pub session_name: String,
    pub wakeup_socket: PathBuf,
    pub last_seen: Timestamp,
}

impl SidebarHeartbeat {
    pub fn new(
        workspace_id: WorkspaceId,
        instance_id: SidebarInstanceId,
        mux: MuxName,
        session_name: impl Into<String>,
        wakeup_socket: PathBuf,
    ) -> Self {
        Self {
            protocol_version: SIDEBAR_PROTOCOL_VERSION.to_owned(),
            workspace_id,
            instance_id,
            mux,
            session_name: session_name.into(),
            wakeup_socket,
            last_seen: Timestamp::now(),
        }
    }

    /// Whether `path` is a sidebar heartbeat file (`sidebar.<id>.json`). The
    /// naming convention is owned here so the wakeup walk and the launch
    /// freshness gate agree on which files are heartbeats.
    pub fn is_heartbeat_file(path: &Path) -> bool {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("sidebar.") && name.ends_with(".json"))
    }

    /// Decode a heartbeat from its on-disk JSON. A parse failure maps to an IO
    /// error so callers handle read and decode uniformly.
    pub fn read_from(path: &Path) -> std::io::Result<Self> {
        let bytes = std::fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(std::io::Error::other)
    }
}

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
