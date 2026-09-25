//! Lifetime-class garbage collection for known rooms, plus orphan write temps and dead workspaces.
//!
//! [`collect_classes`] previews or applies state and runtime retention, reporting files and bytes per room and class. Runtime exporter protection and the renderer claim clock live beside the shared class sweeps. Watcher liveness is supplied by the harness caller; store owns the file removals.
//!
//! [`collect_orphan_temps`] removes or previews atomic-write temp siblings left
//! behind by a process killed between create and rename.
//!
//! [`prune_dead_workspaces`] reaps or previews durable workspace stores that
//! can hold no recoverable value: a recorded project root that no longer
//! exists, or an abandoned `rimz start` scaffold with no history. A dir whose
//! record is unreadable but still holds history is kept and reported, never
//! deleted — durable history stays the correctness source.

use std::fs::ReadDir;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use crate::disk::paths;

/// Reap grace for the per-session context-refresh throttle stamp. A live
/// session re-touches its stamp within `crate::sidebar::timing::SESSION_REFRESH_INTERVAL`
/// plus the producer fold cadence, so a stamp older than this is dead.
pub const SESSION_PROBE_MARKER_TTL: Duration = Duration::from_secs(5 * 60);

/// Runtime `shared/` filename prefix for per-session context-refresh throttle
/// stamps.
pub const SESSION_PROBE_MARKER_PREFIX: &str = "session-context-probe.";

mod classes;
mod collect;
mod prune;
mod temp_sweep;

pub(crate) fn collect_runtime_claims(runtime: &paths::RuntimePaths) -> Result<()> {
    collect::collect_claims(runtime)
}

pub use prune::{PruneReason, RemovedWorkspace, WorkspacePruneReport};

#[derive(Debug, thiserror::Error)]
pub enum GcErr {
    #[error(transparent)]
    Path(#[from] paths::PathErr),
    #[error(transparent)]
    Lock(#[from] crate::disk::lock::LockErr),
    #[error(transparent)]
    Run(#[from] crate::store::run::RunStoreErr),
    #[error("reading dir {path}: {source}")]
    ReadDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub type Result<T> = std::result::Result<T, GcErr>;

fn read_dir_if_exists(path: &Path) -> Result<Option<ReadDir>> {
    match std::fs::read_dir(path) {
        Ok(entries) => Ok(Some(entries)),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(GcErr::ReadDir {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GcReport {
    pub runtime_roots_scanned: usize,
    pub heartbeat_files_removed: usize,
    pub sidecar_files_removed: usize,
    pub sidebar_sockets_removed: usize,
    pub probe_markers_removed: usize,
    pub dirs_removed: usize,
    pub bytes_removed: u64,
    pub state_bytes_removed: u64,
    pub state_files_removed: usize,
    pub wait_outputs_removed: usize,
    pub rooms: Vec<RoomReport>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct RoomReport {
    pub name: String,
    pub classes: Vec<ClassReport>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct ClassReport {
    pub class: String,
    pub files_removed: usize,
    pub bytes_removed: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TempSweepReport {
    pub files_removed: usize,
    pub bytes_removed: u64,
}

#[must_use = "maintenance report; surface it to the caller"]
pub fn collect_classes(
    older_than: Duration,
    dry_run: bool,
    watcher_is_live: impl Fn(&paths::RuntimePaths, &str) -> io::Result<bool>,
) -> Result<GcReport> {
    let mut report = collect::collect_runtime_under(
        &paths::workspaces_dir(),
        &paths::runtime_workspaces_dir(),
        &paths::RuntimePaths::shared().shared_root,
        older_than,
        dry_run,
    )?;
    let root = paths::workspaces_dir();
    let rooms = crate::workspace::known_workspaces_under(&root)
        .map_err(|source| GcErr::ReadDir { path: root, source })?;
    for room in rooms {
        let runtime = paths::RuntimePaths::under_named(
            room.workspace_id.clone(),
            room.dir_name.clone(),
            &paths::runtime_home(),
        );
        let paths = paths::StatePaths::under_named(
            room.workspace_id,
            room.dir_name.clone(),
            &paths::rimz_home(),
            &runtime,
        );
        let runtime = paths::RuntimePaths::for_state(&paths)?;
        let (classes, waits_removed) = match classes::collect_state(&paths, dry_run, &|name| {
            watcher_is_live(&runtime, name)
        }) {
            Ok(report) => report,
            Err(err) => {
                tracing::warn!(workspace = %paths.dir_name, error = %err, "state gc skipped inaccessible workspace");
                continue;
            }
        };
        report.wait_outputs_removed += waits_removed;
        report.state_bytes_removed += classes.iter().map(|class| class.bytes_removed).sum::<u64>();
        report.state_files_removed += classes
            .iter()
            .map(|class| class.files_removed)
            .sum::<usize>();
        if let Some(existing) = report
            .rooms
            .iter_mut()
            .find(|entry| entry.name == room.dir_name.as_str())
        {
            existing.classes.extend(classes);
        } else {
            report.rooms.push(RoomReport {
                name: room.dir_name.to_string(),
                classes,
            });
        }
    }
    for room in &mut report.rooms {
        use paths::Class;
        for class in Class::STATE.into_iter().chain(Class::RUNTIME) {
            if !room
                .classes
                .iter()
                .any(|entry| entry.class == class.dir_name())
            {
                room.classes.push(ClassReport {
                    class: class.dir_name().to_owned(),
                    ..ClassReport::default()
                });
            }
        }
        room.classes.sort_by(|a, b| a.class.cmp(&b.class));
    }
    report.rooms.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(report)
}

#[must_use = "maintenance report; surface it to the caller"]
pub fn collect_orphan_temps(older_than: Duration, dry_run: bool) -> TempSweepReport {
    let mut report = TempSweepReport::default();
    for root in [paths::rimz_home(), paths::runtime_rimz_root()] {
        let (files, bytes) = temp_sweep::sweep_orphan_temps_under(
            &root,
            &paths::workspaces_dir(),
            older_than,
            dry_run,
        );
        report.files_removed += files;
        report.bytes_removed = report.bytes_removed.saturating_add(bytes);
    }
    report
}

#[must_use = "maintenance report; surface it to the caller"]
pub fn prune_dead_workspaces(dry_run: bool) -> Result<WorkspacePruneReport> {
    prune::prune_dead_workspaces_under(
        &paths::workspaces_dir(),
        &paths::runtime_workspaces_dir(),
        dry_run,
    )
}
