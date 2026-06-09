//! Garbage collection — runtime liveness hints and provably-dead workspaces.
//!
//! [`collect_runtime`] removes runtime liveness hints older than an
//! operator-supplied threshold: resolver/sidebar heartbeat JSON and sidebar
//! wakeup sockets named by stale heartbeats. Per-request `feed.*.sock` files
//! are deliberately left alone because a long-running `feed ask` may still own
//! one.
//!
//! [`prune_dead_workspaces`] reaps durable workspace ledgers that can hold no
//! recoverable value: a recorded project root that no longer exists, or an
//! abandoned `rimz start` scaffold with no history. A dir whose record is
//! unreadable but still holds history is kept and reported, never deleted —
//! durable history stays the correctness source.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use crate::ledger::paths;

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
    pub dirs_removed: usize,
    pub bytes_removed: u64,
}

#[must_use = "maintenance report; surface it to the caller"]
pub fn collect_runtime(older_than: Duration) -> Result<GcReport> {
    collect::collect_runtime_under(&paths::runtime_home().join("rimz"), older_than)
}

#[must_use = "maintenance report; surface it to the caller"]
pub fn prune_dead_workspaces() -> Result<WorkspacePruneReport> {
    prune::prune_dead_workspaces_under(
        &paths::workspaces_dir(),
        &paths::runtime_home().join("rimz"),
    )
}
