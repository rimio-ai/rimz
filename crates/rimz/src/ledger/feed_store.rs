//! Per-request feed files: `feed/<request_id>.json`.
//!
//! Writes go through [`crate::ledger::atomic::write_temp_then_rename`]. The
//! ledger module owns CAS sequencing under the workspace lock.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::feed::{FeedItem, FeedStatus, Surface};
use crate::ids::{RequestId, ResolverId};
use crate::ledger::atomic::{self, write_temp_then_rename};

#[derive(Debug, thiserror::Error)]
pub enum FeedStoreErr {
    #[error("feed item {0} not found")]
    NotFound(RequestId),
    #[error("feed item {request_id} is not pending (status = {status})")]
    NotPending {
        request_id: RequestId,
        status: FeedStatus,
    },
    #[error("surface mismatch for {request_id}: surface {surface} does not support {verb}")]
    SurfaceMismatch {
        request_id: RequestId,
        surface: Surface,
        verb: &'static str,
    },
    #[error("resolver {resolver} is not active for request {request_id}")]
    ResolverNotActive {
        request_id: RequestId,
        resolver: ResolverId,
    },
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("json parse error on {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

pub type Result<T> = std::result::Result<T, FeedStoreErr>;

#[must_use = "durability barrier; check the result"]
pub fn write(feed_dir: &Path, item: &FeedItem) -> Result<()> {
    let path = feed_dir.join(format!("{}.json", item.request_id));
    write_temp_then_rename(&path, item)?;
    Ok(())
}

pub fn load(feed_dir: &Path, request_id: &RequestId) -> Result<FeedItem> {
    let path = feed_dir.join(format!("{request_id}.json"));
    if !path.exists() {
        return Err(FeedStoreErr::NotFound(request_id.clone()));
    }
    let bytes = fs::read(&path).map_err(|e| FeedStoreErr::Io {
        path: path.clone(),
        source: e,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| FeedStoreErr::Json { path, source })
}

pub fn list(feed_dir: &Path) -> Result<Vec<FeedItem>> {
    if !feed_dir.exists() {
        return Ok(Vec::new());
    }
    let mut items = Vec::new();
    let entries = fs::read_dir(feed_dir).map_err(|e| FeedStoreErr::Io {
        path: feed_dir.to_path_buf(),
        source: e,
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| FeedStoreErr::Io {
            path: feed_dir.to_path_buf(),
            source: e,
        })?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let bytes = fs::read(&path).map_err(|source| FeedStoreErr::Io {
            path: path.clone(),
            source,
        })?;
        let item =
            serde_json::from_slice::<FeedItem>(&bytes).map_err(|source| FeedStoreErr::Json {
                path: path.clone(),
                source,
            })?;
        items.push(item);
    }
    items.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::FeedKind;
    use crate::ids::WorkspaceId;
    use tempfile::tempdir;

    #[test]
    fn write_then_load_round_trip() {
        let dir = tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let item = FeedItem::new(
            workspace,
            Surface::NativeUi,
            FeedKind::Permission,
            "approve?",
            "rimz",
            "cli",
        );
        write(dir.path(), &item).unwrap();
        let loaded = load(dir.path(), &item.request_id).unwrap();
        assert_eq!(loaded.title, "approve?");
        assert_eq!(loaded.surface, Surface::NativeUi);
    }

    #[test]
    fn list_returns_most_recently_updated_first() {
        let dir = tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut old = FeedItem::new(
            workspace.clone(),
            Surface::Script,
            FeedKind::Question,
            "old",
            "rimz",
            "cli",
        );
        let new = FeedItem::new(
            workspace,
            Surface::Script,
            FeedKind::Question,
            "new",
            "rimz",
            "cli",
        );
        old.updated_at -= std::time::Duration::from_secs(10);
        write(dir.path(), &old).unwrap();
        write(dir.path(), &new).unwrap();
        let items = list(dir.path()).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "new");
        assert_eq!(items[1].title, "old");
    }

    #[test]
    fn list_reports_malformed_feed_file() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("req_bad.json"), b"{not json").unwrap();

        let err = list(dir.path()).unwrap_err();
        assert!(matches!(err, FeedStoreErr::Json { .. }));
    }
}
