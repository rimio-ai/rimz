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

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::ids::WorkspaceId;
use crate::ledger::paths;
use crate::ledger::workspace_record;
use crate::schema::heartbeat::SidebarHeartbeat;

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
    collect_runtime_under(&paths::runtime_home().join("rimz"), older_than)
}

#[must_use = "maintenance report; surface it to the caller"]
pub fn collect_runtime_under(runtime_root: &Path, older_than: Duration) -> Result<GcReport> {
    let entries = match fs::read_dir(runtime_root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(GcReport::default()),
        Err(source) => {
            return Err(GcErr::ReadDir {
                path: runtime_root.to_path_buf(),
                source,
            });
        }
    };

    let mut report = GcReport::default();
    for entry in entries {
        let entry = entry.map_err(|source| GcErr::ReadDir {
            path: runtime_root.to_path_buf(),
            source,
        })?;
        let root = entry.path();
        if !root.is_dir() {
            continue;
        }
        let Some(name) = root.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if WorkspaceId::parse(name).is_err() {
            continue;
        }
        report.runtime_roots_scanned += 1;
        collect_workspace_runtime(&root, older_than, &mut report)?;
    }

    Ok(report)
}

fn collect_workspace_runtime(
    workspace_root: &Path,
    older_than: Duration,
    report: &mut GcReport,
) -> Result<()> {
    let heartbeat_dir = workspace_root.join("heartbeat");
    let sock_dir = workspace_root.join("sock");
    let activity_dir = workspace_root.join("agent-activity");
    let context_dir = workspace_root.join("agent_context");
    collect_heartbeats(&heartbeat_dir, &sock_dir, older_than, report)?;
    collect_stale_sidecars(&activity_dir, older_than, report)?;
    collect_stale_sidecars(&context_dir, older_than, report)?;
    remove_dir_if_empty(&heartbeat_dir, report)?;
    remove_dir_if_empty(&sock_dir, report)?;
    remove_dir_if_empty(&activity_dir, report)?;
    remove_dir_if_empty(&context_dir, report)?;
    remove_dir_if_empty(workspace_root, report)?;
    Ok(())
}

/// Reap stale per-session sidecar files — the activity heartbeats and the
/// statusline context sidecars. These are latency hints, not ledger truth, so a
/// file an ended session left behind is aged out like the other runtime liveness
/// files; reaping them also lets the workspace root be removed once the workspace
/// goes quiet. Any orphaned atomic-write `.tmp` sibling ages out the same way.
fn collect_stale_sidecars(dir: &Path, older_than: Duration, report: &mut GcReport) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(GcErr::ReadDir {
                path: dir.to_path_buf(),
                source,
            });
        }
    };

    for entry in entries {
        let entry = entry.map_err(|source| GcErr::ReadDir {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if !is_older_than(&path, older_than)? {
            continue;
        }
        remove_file_if_exists(
            &path,
            |report| {
                report.sidecar_files_removed += 1;
            },
            report,
        )?;
    }
    Ok(())
}

fn collect_heartbeats(
    heartbeat_dir: &Path,
    sock_dir: &Path,
    older_than: Duration,
    report: &mut GcReport,
) -> Result<()> {
    let entries = match fs::read_dir(heartbeat_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(GcErr::ReadDir {
                path: heartbeat_dir.to_path_buf(),
                source,
            });
        }
    };

    for entry in entries {
        let entry = entry.map_err(|source| GcErr::ReadDir {
            path: heartbeat_dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let heartbeat_kind = heartbeat_kind(name);
        if heartbeat_kind.is_none() || !is_older_than(&path, older_than)? {
            continue;
        }

        if heartbeat_kind == Some(HeartbeatKind::Sidebar)
            && let Some(socket) = stale_sidebar_socket(&path, sock_dir)
        {
            remove_file_if_exists(
                &socket,
                |report| {
                    report.sidebar_sockets_removed += 1;
                },
                report,
            )?;
        }
        remove_file_if_exists(
            &path,
            |report| {
                report.heartbeat_files_removed += 1;
            },
            report,
        )?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HeartbeatKind {
    Resolver,
    Sidebar,
}

fn heartbeat_kind(name: &str) -> Option<HeartbeatKind> {
    if name.starts_with("resolver.") && name.ends_with(".json") {
        Some(HeartbeatKind::Resolver)
    } else if name.starts_with("sidebar.") && name.ends_with(".json") {
        Some(HeartbeatKind::Sidebar)
    } else {
        None
    }
}

fn stale_sidebar_socket(heartbeat_path: &Path, sock_dir: &Path) -> Option<PathBuf> {
    let bytes = fs::read(heartbeat_path).ok()?;
    let hb: SidebarHeartbeat = serde_json::from_slice(&bytes).ok()?;
    if sidebar_socket_is_owned_by_workspace(&hb.wakeup_socket, sock_dir) {
        Some(hb.wakeup_socket)
    } else {
        None
    }
}

fn sidebar_socket_is_owned_by_workspace(socket: &Path, sock_dir: &Path) -> bool {
    if socket.parent() != Some(sock_dir) {
        return false;
    }
    socket
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("sidebar.") && name.ends_with(".sock"))
}

fn remove_file_if_exists(
    path: &Path,
    increment: impl FnOnce(&mut GcReport),
    report: &mut GcReport,
) -> Result<()> {
    let bytes = match fs::symlink_metadata(path) {
        Ok(meta) => meta.len(),
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(GcErr::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    match fs::remove_file(path) {
        Ok(()) => {
            report.bytes_removed = report.bytes_removed.saturating_add(bytes);
            increment(report);
            Ok(())
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(GcErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn remove_dir_if_empty(path: &Path, report: &mut GcReport) -> Result<()> {
    let mut entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(GcErr::ReadDir {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if entries.next().is_some() {
        return Ok(());
    }
    match fs::remove_dir(path) {
        Ok(()) => {
            report.dirs_removed += 1;
            Ok(())
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(GcErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn is_older_than(path: &Path, older_than: Duration) -> Result<bool> {
    let meta = fs::symlink_metadata(path).map_err(|source| GcErr::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let modified = meta.modified().map_err(|source| GcErr::Io {
        path: path.to_path_buf(),
        source,
    })?;
    match SystemTime::now().duration_since(modified) {
        Ok(age) => Ok(age >= older_than),
        Err(_) => Ok(false),
    }
}

/// Why a workspace directory was reaped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PruneReason {
    /// `workspace.json` records a project root that no longer exists on disk.
    ProjectRootGone,
    /// No usable `workspace.json` and no durable history — an abandoned
    /// `rimz start` scaffold (empty `feed`/`locks`/`snapshots`).
    AbandonedScaffold,
}

/// A workspace ledger removed by [`prune_dead_workspaces`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemovedWorkspace {
    pub workspace_id: WorkspaceId,
    pub reason: PruneReason,
    /// The recorded project root, when the record was readable.
    pub project_root: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspacePruneReport {
    pub removed: Vec<RemovedWorkspace>,
    pub kept: usize,
    /// Dirs with an unreadable record that still hold history, kept for the
    /// operator to inspect rather than silently deleted: `(id, error)`.
    pub retained_unreadable: Vec<(WorkspaceId, String)>,
}

/// Reap provably-dead workspace ledgers under `$XDG_STATE_HOME/rimz/workspaces`
/// and their runtime dirs. See [`prune_dead_workspaces_under`] for the rules.
#[must_use = "maintenance report; surface it to the caller"]
pub fn prune_dead_workspaces() -> Result<WorkspacePruneReport> {
    prune_dead_workspaces_under(
        &paths::workspaces_dir(),
        &paths::runtime_home().join("rimz"),
    )
}

/// A workspace is removed when it is *provably dead*:
/// 1. its `workspace.json` reads and the recorded `project_root` no longer
///    exists, or
/// 2. it has no usable record (missing or unparseable `workspace.json`) **and**
///    no durable history — an abandoned start scaffold.
///
/// A dir with an unreadable record that still holds history is kept and
/// reported, never deleted.
#[must_use = "maintenance report; surface it to the caller"]
pub fn prune_dead_workspaces_under(
    workspaces_root: &Path,
    runtime_rimz_root: &Path,
) -> Result<WorkspacePruneReport> {
    let entries = match fs::read_dir(workspaces_root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok(WorkspacePruneReport::default());
        }
        Err(source) => {
            return Err(GcErr::ReadDir {
                path: workspaces_root.to_path_buf(),
                source,
            });
        }
    };

    let mut report = WorkspacePruneReport::default();
    for entry in entries {
        let entry = entry.map_err(|source| GcErr::ReadDir {
            path: workspaces_root.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Ok(workspace_id) = WorkspaceId::parse(name) else {
            continue;
        };

        match classify_workspace(&path) {
            Verdict::Keep => report.kept += 1,
            Verdict::Retain(err) => report.retained_unreadable.push((workspace_id, err)),
            Verdict::Remove(reason, project_root) => {
                remove_workspace(&path, &workspace_id, runtime_rimz_root)?;
                report.removed.push(RemovedWorkspace {
                    workspace_id,
                    reason,
                    project_root,
                });
            }
        }
    }
    Ok(report)
}

enum Verdict {
    Keep,
    Retain(String),
    Remove(PruneReason, Option<PathBuf>),
}

fn classify_workspace(path: &Path) -> Verdict {
    match workspace_record::read(&path.join("workspace.json")) {
        Ok(record) if record.project_root.exists() => Verdict::Keep,
        Ok(record) => Verdict::Remove(PruneReason::ProjectRootGone, Some(record.project_root)),
        Err(err) if workspace_has_history(path) => Verdict::Retain(err.to_string()),
        Err(_) => Verdict::Remove(PruneReason::AbandonedScaffold, None),
    }
}

/// Whether a workspace dir holds durable history worth preserving.
fn workspace_has_history(path: &Path) -> bool {
    path.join("events.log.jsonl").exists()
        || path.join("snapshots").join("latest.json").exists()
        || dir_has_entries(&path.join("events.log.archive"))
        || dir_has_entries(&path.join("feed"))
}

fn dir_has_entries(path: &Path) -> bool {
    fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_some())
}

fn remove_workspace(
    state_dir: &Path,
    workspace_id: &WorkspaceId,
    runtime_rimz_root: &Path,
) -> Result<()> {
    remove_dir_all_if_exists(state_dir)?;
    remove_dir_all_if_exists(&runtime_rimz_root.join(workspace_id.as_str()))
}

fn remove_dir_all_if_exists(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(GcErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{MuxName, ResolverId, SidebarInstanceId};
    use crate::ledger::RuntimePaths;
    use crate::schema::heartbeat::{ResolverHeartbeat, SidebarHeartbeat};
    use std::time::SystemTime;
    use tempfile::tempdir;

    #[test]
    fn runtime_gc_reaps_sidecars_and_unblocks_the_workspace_root() {
        // Before the sweep covered them, a leftover per-session sidecar (an
        // activity heartbeat or a statusline context file) kept the workspace
        // root non-empty forever, so the root never reaped.
        let temp = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(temp.path());
        let rt = RuntimePaths::under(workspace_id, temp.path()).unwrap();
        rt.ensure_dirs().unwrap();

        let stale_activity = rt.agent_activity_dir.join("deadbeefdeadbeef.json");
        fs::write(
            &stale_activity,
            br#"{"kind":"claude","agent_id":"sess-1","at":"1970-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        let stale_context = rt.agent_context_dir.join("cafef00dcafef00d.json");
        fs::write(&stale_context, b"{}").unwrap();
        let old = SystemTime::now() - Duration::from_secs(7200);
        for path in [&stale_activity, &stale_context] {
            fs::File::open(path).unwrap().set_modified(old).unwrap();
        }

        let report =
            collect_runtime_under(&temp.path().join("rimz"), Duration::from_secs(3600)).unwrap();

        assert_eq!(report.sidecar_files_removed, 2);
        assert!(
            !rt.agent_activity_dir.exists(),
            "the emptied activity dir is removed"
        );
        assert!(
            !rt.agent_context_dir.exists(),
            "the emptied context dir is removed"
        );
        assert!(
            !rt.agent_activity_dir.parent().unwrap().exists(),
            "with no runtime files left, the workspace root is reaped too"
        );
    }

    #[test]
    fn runtime_gc_removes_stale_heartbeats_and_sidebar_socket_only() {
        let temp = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(temp.path());
        let rt = RuntimePaths::under(workspace_id.clone(), temp.path()).unwrap();
        rt.ensure_dirs().unwrap();

        let stale_socket = rt.sock_dir.join("sidebar.stale.sock");
        fs::write(&stale_socket, b"socket placeholder").unwrap();
        let feed_socket = rt.sock_dir.join("feed.123456789abc.sock");
        fs::write(&feed_socket, b"feed socket placeholder").unwrap();

        let stale_sidebar = SidebarHeartbeat::new(
            workspace_id.clone(),
            SidebarInstanceId::new(),
            MuxName::Tmux,
            "rimz-test",
            stale_socket.clone(),
            None,
        );
        let stale_sidebar_path = rt.heartbeat_dir.join("sidebar.stale.json");
        write_json(&stale_sidebar_path, &stale_sidebar);

        let resolver_id: ResolverId = "opus-policy".parse().unwrap();
        let stale_resolver = ResolverHeartbeat::new(workspace_id.clone(), resolver_id);
        let stale_resolver_path = rt.heartbeat_dir.join("resolver.opus-policy.json");
        write_json(&stale_resolver_path, &stale_resolver);

        let fresh_resolver_id: ResolverId = "slack-on-call".parse().unwrap();
        let fresh_resolver = ResolverHeartbeat::new(workspace_id, fresh_resolver_id);
        let fresh_resolver_path = rt.heartbeat_dir.join("resolver.slack-on-call.json");
        write_json(&fresh_resolver_path, &fresh_resolver);

        let old = SystemTime::now() - Duration::from_secs(7200);
        for path in [&stale_socket, &stale_sidebar_path, &stale_resolver_path] {
            fs::File::open(path).unwrap().set_modified(old).unwrap();
        }

        let report =
            collect_runtime_under(&temp.path().join("rimz"), Duration::from_secs(3600)).unwrap();

        assert_eq!(report.runtime_roots_scanned, 1);
        assert_eq!(report.heartbeat_files_removed, 2);
        assert_eq!(report.sidebar_sockets_removed, 1);
        assert!(!stale_sidebar_path.exists());
        assert!(!stale_resolver_path.exists());
        assert!(!stale_socket.exists());
        assert!(fresh_resolver_path.exists());
        assert!(feed_socket.exists(), "feed sockets are not GC-owned");
    }

    fn write_json<T: serde::Serialize>(path: &Path, value: &T) {
        fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    }

    #[test]
    fn prune_reaps_dead_roots_and_scaffolds_but_keeps_history() {
        let temp = tempdir().unwrap();
        let workspaces = temp.path().join("workspaces");
        let runtime = temp.path().join("runtime-rimz");
        fs::create_dir_all(&workspaces).unwrap();

        // 1. Alive: record points at a project root that still exists.
        let alive_root = temp.path().join("alive");
        fs::create_dir_all(&alive_root).unwrap();
        let alive_id = WorkspaceId::from_project_root(&alive_root);
        write_record(&workspaces.join(alive_id.as_str()), &alive_id, &alive_root);

        // 2. Dead root: recorded project root is gone; runtime dir present too.
        let gone_root = temp.path().join("gone");
        let gone_id = WorkspaceId::from_project_root(&gone_root);
        write_record(&workspaces.join(gone_id.as_str()), &gone_id, &gone_root);
        fs::create_dir_all(runtime.join(gone_id.as_str())).unwrap();

        // 3. Abandoned scaffold: empty feed/locks/snapshots, no record.
        let scaffold_id = WorkspaceId::from_project_root(Path::new("/scaffold"));
        let scaffold_dir = workspaces.join(scaffold_id.as_str());
        for sub in ["feed", "locks", "snapshots"] {
            fs::create_dir_all(scaffold_dir.join(sub)).unwrap();
        }

        // 4. Unreadable record but real history: retained, never deleted.
        let history_id = WorkspaceId::from_project_root(Path::new("/history"));
        let history_dir = workspaces.join(history_id.as_str());
        fs::create_dir_all(&history_dir).unwrap();
        fs::write(history_dir.join("workspace.json"), b"{ not json").unwrap();
        fs::write(history_dir.join("events.log.jsonl"), b"{}\n").unwrap();

        let report = prune_dead_workspaces_under(&workspaces, &runtime).unwrap();

        assert_eq!(report.kept, 1, "alive workspace kept");
        assert_eq!(report.removed.len(), 2, "dead root + scaffold removed");
        assert_eq!(report.retained_unreadable.len(), 1, "history dir retained");
        let reasons: Vec<_> = report.removed.iter().map(|r| r.reason).collect();
        assert!(reasons.contains(&PruneReason::ProjectRootGone));
        assert!(reasons.contains(&PruneReason::AbandonedScaffold));

        assert!(workspaces.join(alive_id.as_str()).exists());
        assert!(!workspaces.join(gone_id.as_str()).exists());
        assert!(
            !runtime.join(gone_id.as_str()).exists(),
            "runtime dir reaped"
        );
        assert!(!scaffold_dir.exists());
        assert!(history_dir.exists(), "history retained");
    }

    fn write_record(dir: &Path, id: &WorkspaceId, project_root: &Path) {
        use crate::ledger::workspace_record::WorkspaceRecord;
        fs::create_dir_all(dir).unwrap();
        let record = WorkspaceRecord {
            workspace_id: id.clone(),
            project_root: project_root.to_path_buf(),
            session_name: "rimz-test".to_owned(),
            updated_at: jiff::Timestamp::now(),
        };
        fs::write(
            dir.join("workspace.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
    }
}
