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

use std::collections::BTreeSet;
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
    pub locks_would_check: usize,
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
    pub locks_would_check: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TempSweepReport {
    pub files_removed: usize,
    pub bytes_removed: u64,
}

/// Sweep one room's state and runtime classes without visiting other rooms or shared runtime files.
#[must_use = "maintenance report; surface it to the caller"]
pub fn collect_room(
    paths: &paths::StatePaths,
    older_than: Duration,
    dry_run: bool,
    watcher_is_live: impl Fn(&paths::RuntimePaths, &str) -> io::Result<bool>,
) -> Result<GcReport> {
    if !paths.root.is_dir() {
        return Ok(GcReport::default());
    }
    collect_room_under(
        paths,
        &paths::RuntimePaths::for_state(paths)?,
        older_than,
        dry_run,
        watcher_is_live,
    )
}

fn collect_room_under(
    paths: &paths::StatePaths,
    runtime: &paths::RuntimePaths,
    older_than: Duration,
    dry_run: bool,
    watcher_is_live: impl Fn(&paths::RuntimePaths, &str) -> io::Result<bool>,
) -> Result<GcReport> {
    paths::check_workspace_layout(&paths.root)?;
    let mut report = GcReport {
        runtime_roots_scanned: 1,
        ..GcReport::default()
    };
    collect::collect_runtime_classes(
        &runtime.root,
        older_than,
        &mut collect::Sweep::new(dry_run),
        &mut report,
    )?;
    collect_room_state(paths, runtime, dry_run, &watcher_is_live, &mut report)?;
    complete_room_reports(&mut report);
    Ok(report)
}

/// Sweep atomic-write temps only under this room's two roots.
#[must_use = "maintenance report; surface it to the caller"]
pub fn collect_room_orphan_temps(
    paths: &paths::StatePaths,
    older_than: Duration,
    dry_run: bool,
) -> Result<TempSweepReport> {
    collect_room_temps_under(
        paths,
        &paths::RuntimePaths::for_state(paths)?,
        older_than,
        dry_run,
    )
}

fn collect_room_temps_under(
    paths: &paths::StatePaths,
    runtime: &paths::RuntimePaths,
    older_than: Duration,
    dry_run: bool,
) -> Result<TempSweepReport> {
    paths::check_workspace_layout(&paths.root)?;
    let mut report = TempSweepReport::default();
    for root in [&paths.root, &runtime.root] {
        let (files, bytes) = temp_sweep::sweep_orphan_temps_under(root, older_than, dry_run);
        report.files_removed += files;
        report.bytes_removed = report.bytes_removed.saturating_add(bytes);
    }
    Ok(report)
}

#[must_use = "maintenance report; surface it to the caller"]
pub fn collect_classes(
    older_than: Duration,
    dry_run: bool,
    pruned: &BTreeSet<String>,
    watcher_is_live: impl Fn(&paths::RuntimePaths, &str) -> io::Result<bool>,
) -> Result<GcReport> {
    let mut report = collect::collect_runtime_under(
        &paths::workspaces_dir(),
        &paths::runtime_workspaces_dir(),
        &paths::RuntimePaths::shared().shared_root,
        older_than,
        dry_run,
        pruned,
    )?;
    let root = paths::workspaces_dir();
    let rooms = crate::workspace::known_workspaces_under(&root)
        .map_err(|source| GcErr::ReadDir { path: root, source })?;
    for room in rooms
        .into_iter()
        .filter(|room| !pruned.contains(room.dir_name.as_str()))
    {
        let paths = paths::StatePaths::under_named(
            room.workspace_id,
            room.dir_name.clone(),
            &paths::rimz_home(),
        );
        let runtime = match paths::RuntimePaths::for_state(&paths) {
            Ok(runtime) => runtime,
            Err(err) => {
                tracing::warn!(workspace = %paths.dir_name, error = %err, "state gc skipped inaccessible runtime paths");
                continue;
            }
        };
        if let Err(err) =
            collect_room_state(&paths, &runtime, dry_run, &watcher_is_live, &mut report)
        {
            tracing::warn!(workspace = %paths.dir_name, error = %err, "state gc skipped inaccessible workspace");
        }
    }
    complete_room_reports(&mut report);
    Ok(report)
}

fn collect_room_state(
    paths: &paths::StatePaths,
    runtime: &paths::RuntimePaths,
    dry_run: bool,
    watcher_is_live: &impl Fn(&paths::RuntimePaths, &str) -> io::Result<bool>,
    report: &mut GcReport,
) -> Result<()> {
    let (classes, waits_removed) =
        classes::collect_state(paths, dry_run, &|name| watcher_is_live(runtime, name))?;
    report.wait_outputs_removed += waits_removed;
    report.state_bytes_removed += classes.iter().map(|class| class.bytes_removed).sum::<u64>();
    report.state_files_removed += classes
        .iter()
        .map(|class| class.files_removed)
        .sum::<usize>();
    if let Some(existing) = report
        .rooms
        .iter_mut()
        .find(|entry| entry.name == paths.dir_name.as_str())
    {
        existing.classes.extend(classes);
    } else {
        report.rooms.push(RoomReport {
            name: paths.dir_name.to_string(),
            classes,
        });
    }
    Ok(())
}

fn complete_room_reports(report: &mut GcReport) {
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
}

#[must_use = "maintenance report; surface it to the caller"]
pub fn collect_orphan_temps(older_than: Duration, dry_run: bool) -> TempSweepReport {
    let mut report = TempSweepReport::default();
    for root in [paths::rimz_home(), paths::runtime_rimz_root()] {
        let (files, bytes) = temp_sweep::sweep_orphan_temps_under(&root, older_than, dry_run);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::SystemTime;

    #[test]
    fn room_collection_isolated_and_preview_preserves_files() {
        let home = tempfile::tempdir().unwrap();
        let runtime_home = tempfile::tempdir().unwrap();
        let rooms: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|name| {
                let state = paths::StatePaths::under(
                    crate::WorkspaceId::from_project_root(&home.path().join(name)),
                    home.path(),
                )
                .unwrap();
                state.ensure_dirs().unwrap();
                let runtime = paths::RuntimePaths::for_state_under(&state, runtime_home.path());
                runtime.ensure_dirs().unwrap();
                fs::create_dir_all(&state.waits_dir).unwrap();
                let live = runtime.live_dir.join("stale.json");
                let wait = state.waits_dir.join("stale.output");
                for file in [&live, &wait] {
                    fs::write(file, b"stale").unwrap();
                    fs::File::open(file)
                        .unwrap()
                        .set_modified(SystemTime::now() - Duration::from_secs(15 * 86_400))
                        .unwrap();
                }
                (state, runtime, live, wait)
            })
            .collect();
        let (state, runtime, live, wait) = &rooms[0];
        for dry_run in [true, false] {
            let report = collect_room_under(
                state,
                runtime,
                Duration::from_secs(7 * 86_400),
                dry_run,
                |_, _| Ok(false),
            )
            .unwrap();
            assert_eq!(report.rooms.len(), 1);
            assert_eq!(report.rooms[0].name, state.dir_name.as_str());
            assert_eq!(report.runtime_roots_scanned, 1);
            assert_eq!(report.wait_outputs_removed, 1);
            assert_eq!(report.sidecar_files_removed, 1);
            assert_eq!(live.exists(), dry_run);
            assert_eq!(wait.exists(), dry_run);
            assert!(rooms[1].2.exists());
            assert!(rooms[1].3.exists());
        }
    }

    #[test]
    fn absent_room_collection_creates_nothing() {
        let home = tempfile::tempdir().unwrap();
        let state = paths::StatePaths::under(
            crate::WorkspaceId::from_project_root(home.path()),
            home.path(),
        )
        .unwrap();
        let report = collect_room(&state, Duration::ZERO, false, |_, _| Ok(false)).unwrap();
        assert!(report.rooms.is_empty());
        assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
    }

    #[test]
    fn incompatible_room_collection_refuses_and_leaves_both_tiers() {
        let home = tempfile::tempdir().unwrap();
        let state = paths::StatePaths::under(
            crate::WorkspaceId::from_project_root(home.path()),
            home.path(),
        )
        .unwrap();
        state.ensure_dirs().unwrap();
        fs::write(state.root.join("workspace.json"), br#"{"layout":1}"#).unwrap();
        let runtime = paths::RuntimePaths::for_state_under(&state, home.path());
        runtime.ensure_dirs().unwrap();
        let live = runtime.live_dir.join("stale.json");
        fs::write(&live, b"keep").unwrap();
        assert!(
            collect_room_under(&state, &runtime, Duration::ZERO, false, |_, _| Ok(false)).is_err()
        );
        assert!(live.exists());
        assert!(!state.workspace_lock.exists());
    }

    #[test]
    fn room_temp_collection_leaves_other_rooms_and_shared_files() {
        let home = tempfile::tempdir().unwrap();
        let runtime_home = tempfile::tempdir().unwrap();
        let mut files = Vec::new();
        let mut rooms = Vec::new();
        for name in ["a", "b"] {
            let state = paths::StatePaths::under(
                crate::WorkspaceId::from_project_root(&home.path().join(name)),
                home.path(),
            )
            .unwrap();
            state.ensure_dirs().unwrap();
            let runtime = paths::RuntimePaths::for_state_under(&state, runtime_home.path());
            runtime.ensure_dirs().unwrap();
            for root in [&state.root, &runtime.root] {
                let file = root.join("record.json.tmp.1.00000000000000000000000000000000");
                fs::write(&file, b"stale").unwrap();
                files.push(file);
            }
            rooms.push((state, runtime));
        }
        let shared = runtime_home
            .path()
            .join("record.json.tmp.1.00000000000000000000000000000000");
        fs::write(&shared, b"keep").unwrap();
        for dry_run in [true, false] {
            let report =
                collect_room_temps_under(&rooms[0].0, &rooms[0].1, Duration::ZERO, dry_run)
                    .unwrap();
            assert_eq!(report.files_removed, 2);
            assert_eq!(report.bytes_removed, 10);
            for file in &files[..2] {
                assert_eq!(file.exists(), dry_run);
            }
            for file in &files[2..] {
                assert!(file.exists());
            }
            assert!(shared.exists());
        }
    }
}
