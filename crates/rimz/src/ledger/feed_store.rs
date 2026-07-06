//! Per-request feed files: `feed/<request_id>.json` while pending,
//! `feed/terminal/<request_id>.json` once resolved, timed out, or abandoned.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::feed::{FeedItem, FeedStatus, Surface};
use crate::ids::RequestId;
use crate::ledger::atomic;
use crate::ledger::pending_terminal::{self, PendingTerminalRecord};

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

impl From<pending_terminal::StoreErr> for FeedStoreErr {
    fn from(err: pending_terminal::StoreErr) -> Self {
        match err {
            pending_terminal::StoreErr::Atomic(err) => Self::Atomic(err),
            pending_terminal::StoreErr::Io { path, source } => Self::Io { path, source },
            pending_terminal::StoreErr::Json { path, source } => Self::Json { path, source },
        }
    }
}

impl PendingTerminalRecord for FeedItem {
    fn file_stem(&self) -> String {
        self.request_id.to_string()
    }

    fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }
}

#[must_use = "durability barrier; check the result"]
pub fn write(feed_dir: &Path, item: &FeedItem) -> Result<()> {
    pending_terminal::write(feed_dir, item)?;
    Ok(())
}

pub fn load(feed_dir: &Path, request_id: &RequestId) -> Result<FeedItem> {
    let stem = request_id.to_string();
    pending_terminal::load::<FeedItem>(feed_dir, &stem)?
        .ok_or_else(|| FeedStoreErr::NotFound(request_id.clone()))
}

/// Every feed item, pending and terminal — the audit read. Deduped by
/// request id with the pending side winning, so a terminal rewrite caught
/// between its write and its relocation never lists twice.
pub fn list(feed_dir: &Path) -> Result<Vec<FeedItem>> {
    let mut items = pending_terminal::list_all_lossy::<FeedItem>(feed_dir)?;
    items.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
    Ok(items)
}

/// The decision-path read: O(pending), never O(history). Lists only the
/// pending side; a terminal-status straggler parked there by a crash or a
/// pre-partition layout is returned too and skipped by the caller's
/// pending-status check.
pub fn list_pending(feed_dir: &Path) -> Result<Vec<FeedItem>> {
    let mut items = pending_terminal::list_pending_raw_lossy::<FeedItem>(feed_dir)?;
    items.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
    Ok(items)
}

pub fn prune_terminal(feed_dir: &Path, older_than: Duration) -> Result<atomic::PruneOutcome> {
    Ok(pending_terminal::prune_terminal(feed_dir, older_than)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::FeedKind;
    use crate::ids::WorkspaceId;
    use tempfile::tempdir;

    #[test]
    fn list_skips_malformed_feed_file() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("req_bad.json"), b"{not json").unwrap();

        assert!(list(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn pending_side_terminal_straggler_is_listed_for_the_status_check() {
        // Pre-partition layouts and crash stragglers park terminal-status files
        // on the pending side; `list_pending` returns them so the caller's
        // pending-status check skips them, and `load` still finds them.
        let dir = tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut item = FeedItem::new(
            workspace,
            Surface::Script,
            FeedKind::Question,
            "legacy terminal record",
            "rimz",
            "cli",
        );
        item.status = FeedStatus::Abandoned;
        let bytes = serde_json::to_vec(&item).unwrap();
        std::fs::write(
            pending_terminal::pending_path(dir.path(), &item.request_id.to_string()),
            bytes,
        )
        .unwrap();

        let items = list_pending(dir.path()).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].status, FeedStatus::Abandoned);
        assert_eq!(
            load(dir.path(), &item.request_id).unwrap().title,
            "legacy terminal record"
        );
    }
}
