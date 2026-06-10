//! Per-request feed files: `feed/<request_id>.json` while pending,
//! `feed/terminal/<request_id>.json` once resolved, timed out, or abandoned.
//!
//! Writes go through [`crate::ledger::atomic::write_temp_then_rename_cache`]
//! — rename-atomic, no fsync; correctness rides the CAS under the workspace
//! lock and the event log's audit trail, not crash-durable item files. The
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
use crate::ledger::atomic::{self, write_temp_then_rename_cache};

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
    // Cache-class: the CAS and the pending→terminal split rely on rename
    // atomicity, not fsync — same-host writers always read their own
    // renames, and after a power cut the dead-owner expel abandons any ask
    // whose waiter died with the machine. Trading "survives a power cut"
    // for a fsync-free decision path is the write-class contract
    // (docs/internals/sidebar/ledger.md); the event log keeps the audit trail.
    write_temp_then_rename_cache(&path, item)?;
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

    #[test]
    fn terminal_status_relocates_out_of_the_pending_scan() {
        let dir = tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut item = FeedItem::new(
            workspace,
            Surface::Bridge,
            FeedKind::Permission,
            "decide me",
            "claude",
            "agent-hook",
        );
        write(dir.path(), &item).unwrap();
        assert!(pending_path(dir.path(), &item.request_id).exists());

        item.status = FeedStatus::Resolved;
        write(dir.path(), &item).unwrap();

        assert!(
            !pending_path(dir.path(), &item.request_id).exists(),
            "the decided item leaves the pending side"
        );
        assert!(terminal_path(dir.path(), &item.request_id).exists());
        assert!(
            list_pending(dir.path()).unwrap().is_empty(),
            "the decision-path scan stays O(pending)"
        );
        let audit = list(dir.path()).unwrap();
        assert_eq!(audit.len(), 1, "the audit read spans both sides");
        assert_eq!(
            load(dir.path(), &item.request_id).unwrap().status,
            FeedStatus::Resolved,
            "load finds the relocated item"
        );
    }

    #[test]
    fn list_dedupes_a_straggler_pair_with_the_pending_side_winning() {
        // A crash between a terminal rewrite and its relocation leaves the same
        // request id on both sides; the pending-side copy is the newer write.
        let dir = tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut item = FeedItem::new(
            workspace,
            Surface::Bridge,
            FeedKind::Permission,
            "older terminal copy",
            "claude",
            "agent-hook",
        );
        item.status = FeedStatus::Resolved;
        write(dir.path(), &item).unwrap();
        assert!(terminal_path(dir.path(), &item.request_id).exists());

        item.title = "newer pending-side copy".to_owned();
        let bytes = serde_json::to_vec(&item).unwrap();
        std::fs::write(pending_path(dir.path(), &item.request_id), bytes).unwrap();

        let items = list(dir.path()).unwrap();
        assert_eq!(items.len(), 1, "one row per request id");
        assert_eq!(items[0].title, "newer pending-side copy");
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
        std::fs::write(pending_path(dir.path(), &item.request_id), bytes).unwrap();

        let items = list_pending(dir.path()).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].status, FeedStatus::Abandoned);
        assert_eq!(
            load(dir.path(), &item.request_id).unwrap().title,
            "legacy terminal record"
        );
    }
}
