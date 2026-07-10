//! Durable runtime record for sidebar-initiated pane jumps.
//!
//! The fusion layer trusts the intent pane until the mux confirms it or the
//! anchor expires. Peer renderers also adopt its scroll offset and frozen row
//! order when the jump lands.

use std::collections::HashSet;
use std::fs;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::ids::PaneId;
use crate::sidebar::timing::FOCUS_ANCHOR_FRESH;
use crate::store::{RuntimePaths, atomic};

pub const FOCUS_ANCHOR_VERSION: &str = "rimz.focus-anchor.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusAnchor {
    pub pane_id: PaneId,
    pub offset: usize,
    pub stamp_ms: u64,
    #[serde(default)]
    pub order: Option<FrozenOrder>,
}

/// A snapshot of painted row/group order and the rows visible in that frame.
///
/// Group keys and row ids preserve presentation order. `visible` names the row
/// ids the renderer painted, so a peer renderer can keep cap exemptions stable
/// while it adopts a shared hold.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenOrder {
    pub(crate) groups: Vec<String>,
    pub(crate) rows: Vec<String>,
    pub(crate) visible: HashSet<String>,
}

pub fn store(runtime: &RuntimePaths, anchor: &FocusAnchor) -> atomic::Result<()> {
    let path = runtime.focus_anchor_path();
    let file = FocusAnchorFile {
        v: FOCUS_ANCHOR_VERSION.to_owned(),
        anchor: anchor.clone(),
    };
    atomic::write_temp_then_rename_cache(&path, &file)
}

pub fn load(runtime: &RuntimePaths) -> Option<FocusAnchor> {
    let path = runtime.focus_anchor_path();
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            debug!(path = %path.display(), error = %err, "sidebar focus anchor unreadable");
            return None;
        }
    };
    let file: FocusAnchorFile = match serde_json::from_slice(&bytes) {
        Ok(file) => file,
        Err(err) => {
            debug!(path = %path.display(), error = %err, "sidebar focus anchor invalid");
            return None;
        }
    };
    if file.v != FOCUS_ANCHOR_VERSION {
        debug!(
            path = %path.display(),
            version = file.v,
            "sidebar focus anchor version ignored",
        );
        return None;
    }
    Some(file.anchor)
}

pub fn is_fresh(stamp_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(stamp_ms) <= FOCUS_ANCHOR_FRESH.as_millis() as u64
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct FocusAnchorFile {
    v: String,
    #[serde(flatten)]
    anchor: FocusAnchor,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{MuxName, WorkspaceId};
    use tempfile::TempDir;

    fn runtime() -> (TempDir, RuntimePaths) {
        let dir = TempDir::new().expect("tempdir");
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).expect("runtime");
        (dir, runtime)
    }

    fn anchor(stamp_ms: u64) -> FocusAnchor {
        FocusAnchor {
            pane_id: PaneId::from_parts(MuxName::Tmux, "%1"),
            offset: 7,
            stamp_ms,
            order: None,
        }
    }

    #[test]
    fn stores_and_loads_anchor() {
        let (_dir, runtime) = runtime();
        let mut anchor = anchor(1_000);
        anchor.order = Some(FrozenOrder {
            groups: vec!["main".to_owned()],
            rows: vec!["row-1".to_owned(), "row-2".to_owned()],
            visible: HashSet::from(["row-2".to_owned()]),
        });

        store(&runtime, &anchor).expect("store anchor");

        assert_eq!(load(&runtime), Some(anchor));
    }

    #[test]
    fn missing_order_loads_scroll_only_anchor() {
        let (_dir, runtime) = runtime();
        let anchor = anchor(1_000);
        let mut file = serde_json::to_value(FocusAnchorFile {
            v: FOCUS_ANCHOR_VERSION.to_owned(),
            anchor: anchor.clone(),
        })
        .expect("anchor json");
        file.as_object_mut().expect("file object").remove("order");
        atomic::write_temp_then_rename_cache(&runtime.focus_anchor_path(), &file)
            .expect("write anchor");

        assert_eq!(load(&runtime), Some(anchor));
    }

    #[test]
    fn missing_anchor_loads_none() {
        let (_dir, runtime) = runtime();

        assert_eq!(load(&runtime), None);
    }

    #[test]
    fn wrong_version_loads_none() {
        let (_dir, runtime) = runtime();
        let file = FocusAnchorFile {
            v: "rimz.focus-anchor.v0".to_owned(),
            anchor: anchor(1_000),
        };
        atomic::write_temp_then_rename_cache(&runtime.focus_anchor_path(), &file)
            .expect("write anchor");

        assert_eq!(load(&runtime), None);
    }

    #[test]
    fn freshness_includes_ttl_boundary() {
        let ttl_ms = FOCUS_ANCHOR_FRESH.as_millis() as u64;

        assert!(is_fresh(1_000, 1_000 + ttl_ms));
        assert!(!is_fresh(1_000, 1_000 + ttl_ms + 1));
    }
}
