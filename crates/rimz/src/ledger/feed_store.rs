//! Per-request feed files: `feed/<request_id>.json` while pending,
//! `feed/terminal/<request_id>.json` once resolved, timed out, or abandoned.
//!
//! Writes go through [`crate::ledger::atomic::write_temp_then_rename`]. The
//! ledger module owns CAS sequencing under the workspace lock.
//!
//! The pending/terminal split keeps every decision-path scan O(pending): a
//! terminal write lands beside the pending file, then an atomic rename moves
//! it into `terminal/`. A crash between the two leaves the terminal-status
//! file on the pending side — exactly one copy, skipped by every
//! pending-status fold, found by [`load`], and listed once by [`list`] — so
//! no window can resurrect a decided ask. Pre-partition layouts self-describe
//! the same way, so there is no migration step.

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

const TERMINAL_SUBDIR: &str = "terminal";

fn pending_path(feed_dir: &Path, request_id: &RequestId) -> PathBuf {
    feed_dir.join(format!("{request_id}.json"))
}

fn terminal_path(feed_dir: &Path, request_id: &RequestId) -> PathBuf {
    feed_dir
        .join(TERMINAL_SUBDIR)
        .join(format!("{request_id}.json"))
}

#[must_use = "durability barrier; check the result"]
pub fn write(feed_dir: &Path, item: &FeedItem) -> Result<()> {
    let path = pending_path(feed_dir, &item.request_id);
    write_temp_then_rename(&path, item)?;
    if item.status.is_terminal() {
        // Relocate the decided item out of the pending scan. The rename is
        // atomic: a crash leaves the file on exactly one side, and a
        // terminal-status straggler on the pending side is inert (every
        // pending-status fold skips it).
        let dest = terminal_path(feed_dir, &item.request_id);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|source| FeedStoreErr::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        fs::rename(&path, &dest).map_err(|source| FeedStoreErr::Io { path, source })?;
    }
    Ok(())
}

pub fn load(feed_dir: &Path, request_id: &RequestId) -> Result<FeedItem> {
    let pending = pending_path(feed_dir, request_id);
    let path = if pending.exists() {
        pending
    } else {
        let terminal = terminal_path(feed_dir, request_id);
        if !terminal.exists() {
            return Err(FeedStoreErr::NotFound(request_id.clone()));
        }
        terminal
    };
    let bytes = fs::read(&path).map_err(|e| FeedStoreErr::Io {
        path: path.clone(),
        source: e,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| FeedStoreErr::Json { path, source })
}

/// Every feed item, pending and terminal — the audit read. Deduped by
/// request id with the pending side winning, so a terminal rewrite caught
/// between its write and its relocation never lists twice.
pub fn list(feed_dir: &Path) -> Result<Vec<FeedItem>> {
    let mut by_id = std::collections::HashMap::new();
    for item in read_dir_items(&feed_dir.join(TERMINAL_SUBDIR))?
        .into_iter()
        .chain(read_dir_items(feed_dir)?)
    {
        by_id.insert(item.request_id.clone(), item);
    }
    let mut items: Vec<FeedItem> = by_id.into_values().collect();
    items.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
    Ok(items)
}

/// The decision-path read: O(pending), never O(history). Lists only the
/// pending side; a terminal-status straggler parked there by a crash or a
/// pre-partition layout is returned too and skipped by the caller's
/// pending-status check.
pub fn list_pending(feed_dir: &Path) -> Result<Vec<FeedItem>> {
    let mut items = read_dir_items(feed_dir)?;
    items.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
    Ok(items)
}

fn read_dir_items(dir: &Path) -> Result<Vec<FeedItem>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut items = Vec::new();
    let entries = fs::read_dir(dir).map_err(|e| FeedStoreErr::Io {
        path: dir.to_path_buf(),
        source: e,
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| FeedStoreErr::Io {
            path: dir.to_path_buf(),
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
