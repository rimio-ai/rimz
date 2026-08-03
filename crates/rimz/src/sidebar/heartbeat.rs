//! Liveness heartbeat written by each sidebar renderer.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::ids::{MuxName, PaneId, SidebarInstanceId, WorkspaceId};

// v5: the snapshot view-model carries explicit named-channel identity on agent
// rows and panes. v4 carried `root_class`, and the worktree-group kind
// vocabulary was `worktree`/`root`/`external` (the catch-all renamed from
// `workspace`). The version gate keeps a mixed-version fleet from honouring
// each other's elders mid-upgrade; a consumer that cannot parse a published
// snapshot already falls back to its own produce.
pub const SIDEBAR_PROTOCOL_VERSION: &str = "rimz.plugin.v5";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SidebarHeartbeat {
    pub protocol_version: String,
    pub workspace_id: WorkspaceId,
    pub instance_id: SidebarInstanceId,
    pub mux: MuxName,
    /// Multiplexer session this sidebar is pinned to. Reload and reconcile
    /// consumers use it to match the renderer to its live mux session; wakeup
    /// fanout is workspace-wide and does not inspect it.
    pub session_name: String,
    pub wakeup_socket: PathBuf,
    /// Normalized pane this renderer paints into (`<mux>:<raw>`). `rimz reload`
    /// links a live sidebar pane to its instance through this so it can tell a
    /// healthy sidebar from a duplicate or an unclaimed (wedged) pane. `None`
    /// when the renderer has no per-pane mux env var — treated as a wildcard
    /// that reconcile never closes a pane for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<PaneId>,
    /// Short digest of the renderer binary that wrote this heartbeat. Missing
    /// means an older renderer or an unreadable running image; reload treats it
    /// as live but not build-verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    /// Semantic RimZ version of the writer, for human-facing build-drift
    /// notices. Missing means the renderer predates this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub last_seen: Timestamp,
}

impl SidebarHeartbeat {
    pub fn new(
        workspace_id: WorkspaceId,
        instance_id: SidebarInstanceId,
        mux: MuxName,
        session_name: impl Into<String>,
        wakeup_socket: PathBuf,
        pane_id: Option<PaneId>,
    ) -> Self {
        Self {
            protocol_version: SIDEBAR_PROTOCOL_VERSION.to_owned(),
            workspace_id,
            instance_id,
            mux,
            session_name: session_name.into(),
            wakeup_socket,
            pane_id,
            build: None,
            version: None,
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

/// Walk a heartbeat directory, decode every sidebar heartbeat file, and keep
/// only current-protocol records. Freshness deliberately stays with callers:
/// launch, election, and reload check mtime TTL; wakeup fanout checks
/// `last_seen` plus a TOCTOU re-stat; session records compare mtimes.
pub fn read_current_heartbeats(dir: &Path) -> io::Result<Vec<(PathBuf, SidebarHeartbeat)>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };

    let mut heartbeats = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                debug!(path = %dir.display(), error = %err, "sidebar heartbeat dir entry unreadable");
                continue;
            }
        };
        let path = entry.path();
        if !SidebarHeartbeat::is_heartbeat_file(&path) {
            continue;
        }
        let heartbeat = match SidebarHeartbeat::read_from(&path) {
            Ok(heartbeat) => heartbeat,
            Err(err) => {
                debug!(path = %path.display(), error = %err, "sidebar heartbeat unreadable");
                continue;
            }
        };
        if heartbeat.protocol_version != SIDEBAR_PROTOCOL_VERSION {
            debug!(
                path = %path.display(),
                protocol = heartbeat.protocol_version,
                expected = SIDEBAR_PROTOCOL_VERSION,
                "sidebar heartbeat unsupported protocol version"
            );
            continue;
        }
        heartbeats.push((path, heartbeat));
    }
    Ok(heartbeats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_heartbeat_build_identity_is_backward_compatible() {
        let json = serde_json::json!({
            "protocol_version": SIDEBAR_PROTOCOL_VERSION,
            "workspace_id": WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
            "instance_id": SidebarInstanceId::new(),
            "mux": "tmux",
            "session_name": "rimz-test",
            "wakeup_socket": "/tmp/sidebar.sock",
            "last_seen": Timestamp::now(),
        });

        let heartbeat: SidebarHeartbeat =
            serde_json::from_value(json).expect("missing build identity defaults to None");
        assert_eq!(heartbeat.build, None);
        assert_eq!(heartbeat.version, None);

        let encoded = serde_json::to_string(&heartbeat).expect("serialize heartbeat");
        assert!(
            !encoded.contains("\"build\""),
            "None build stays absent from heartbeat JSON",
        );
        assert!(
            !encoded.contains("\"version\""),
            "None version stays absent from heartbeat JSON",
        );
    }
}
