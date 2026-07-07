//! Garbage collection — stale runtime hints, orphan write temps, and dead
//! workspaces.
//!
//! [`collect_runtime`] removes or previews runtime liveness hints older than an
//! operator-supplied threshold: sidebar heartbeat JSON, sidebar wakeup sockets
//! named by stale heartbeats, and sidebar read-mark receipts whose owner
//! heartbeat has expired. It also removes stale runtime provider probe
//! markers. Run sockets are owned by their waiting process and cleaned up by
//! the bridge guard.
//!
//! [`collect_orphan_temps`] removes or previews atomic-write temp siblings left
//! behind by a process killed between create and rename.
//!
//! [`prune_dead_workspaces`] reaps or previews durable workspace ledgers that
//! can hold no recoverable value: a recorded project root that no longer
//! exists, or an abandoned `rimz start` scaffold with no history. A dir whose
//! record is unreadable but still holds history is kept and reported, never
//! deleted — durable history stays the correctness source.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use crate::ledger::{atomic, paths};

mod collect;
mod prune;

pub use prune::{PruneReason, RemovedWorkspace, WorkspacePruneReport};

#[derive(Debug, thiserror::Error)]
pub enum GcErr {
    #[error("reading runtime dir {path}: {source}")]
    ReadDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub type Result<T> = std::result::Result<T, GcErr>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GcReport {
    pub runtime_roots_scanned: usize,
    pub heartbeat_files_removed: usize,
    pub sidecar_files_removed: usize,
    pub sidebar_sockets_removed: usize,
    pub probe_markers_removed: usize,
    pub dirs_removed: usize,
    pub bytes_removed: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TempSweepReport {
    pub files_removed: usize,
    pub bytes_removed: u64,
}

#[must_use = "maintenance report; surface it to the caller"]
pub fn collect_runtime(older_than: Duration, dry_run: bool) -> Result<GcReport> {
    collect::collect_runtime_under(&paths::runtime_home().join("rimz"), older_than, dry_run)
}

#[must_use = "maintenance report; surface it to the caller"]
pub fn collect_orphan_temps(older_than: Duration, dry_run: bool) -> TempSweepReport {
    let mut report = TempSweepReport::default();
    for root in [
        paths::state_home().join("rimz"),
        paths::runtime_home().join("rimz"),
    ] {
        let (files, bytes) = atomic::sweep_orphan_temps_under(&root, older_than, dry_run);
        report.files_removed += files;
        report.bytes_removed = report.bytes_removed.saturating_add(bytes);
    }
    report
}

#[must_use = "maintenance report; surface it to the caller"]
pub fn prune_dead_workspaces(dry_run: bool) -> Result<WorkspacePruneReport> {
    prune::prune_dead_workspaces_under(
        &paths::workspaces_dir(),
        &paths::runtime_home().join("rimz"),
        dry_run,
    )
}
