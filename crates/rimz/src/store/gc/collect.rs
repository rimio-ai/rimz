use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::ids::{SidebarInstanceId, WorkspaceId};
use crate::sidebar::heartbeat::SidebarHeartbeat;
use crate::sidebar::timing::{SESSION_PROBE_MARKER_PREFIX, SESSION_PROBE_MARKER_TTL};

use super::{GcErr, GcReport, Result};

#[must_use = "maintenance report; surface it to the caller"]
pub(crate) fn collect_runtime_under(
    runtime_root: &Path,
    older_than: Duration,
    dry_run: bool,
) -> Result<GcReport> {
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
    let mut sweep = Sweep::new(dry_run);
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
            // `shared/` holds election locks and probe markers; probe markers
            // are collected below, and legacy pre-migration data caches are
            // swept by RuntimePaths::ensure_dirs at startup.
            continue;
        }
        report.runtime_roots_scanned += 1;
        collect_workspace_runtime(&root, older_than, &mut sweep, &mut report)?;
    }
    collect_stale_probe_markers(
        &runtime_root.join("shared"),
        older_than,
        &mut sweep,
        &mut report,
    )?;

    Ok(report)
}

fn collect_workspace_runtime(
    workspace_root: &Path,
    older_than: Duration,
    sweep: &mut Sweep,
    report: &mut GcReport,
) -> Result<()> {
    let heartbeat_dir = workspace_root.join("heartbeat");
    let sock_dir = workspace_root.join("sock");
    let read_marks_dir = workspace_root.join("read-marks");
    let activity_dir = workspace_root.join("agent-activity");
    let context_dir = workspace_root.join("agent_context");
    let subagent_context_dir = workspace_root.join("subagent_context");
    let telemetry_dir = workspace_root.join("agent-telemetry");
    for dir in [
        &heartbeat_dir,
        &sock_dir,
        &read_marks_dir,
        &activity_dir,
        &context_dir,
        &subagent_context_dir,
        &telemetry_dir,
        workspace_root,
    ] {
        sweep.remember_dir_size(dir);
    }
    // An open exporter keeps writing the inode it opened. Preserve both its
    // file and parent while the room heartbeat is fresh; unlinking either
    // would silently strand a live Copilot process because reopen behavior is
    // not part of the verified exporter contract.
    let room_is_live = !fresh_sidebar_instance_ids(&heartbeat_dir, older_than)?.is_empty();
    collect_heartbeats(&heartbeat_dir, &sock_dir, older_than, sweep, report)?;
    collect_stale_read_marks(&read_marks_dir, &heartbeat_dir, older_than, sweep, report)?;
    collect_stale_sidecars(&activity_dir, older_than, sweep, report)?;
    collect_stale_sidecars(&context_dir, older_than, sweep, report)?;
    collect_stale_sidecars(&subagent_context_dir, older_than, sweep, report)?;
    if !room_is_live {
        collect_stale_sidecars(&telemetry_dir, older_than, sweep, report)?;
    }
    sweep.remove_dir_if_empty(&heartbeat_dir, report)?;
    sweep.remove_dir_if_empty(&sock_dir, report)?;
    sweep.remove_dir_if_empty(&read_marks_dir, report)?;
    sweep.remove_dir_if_empty(&activity_dir, report)?;
    sweep.remove_dir_if_empty(&context_dir, report)?;
    sweep.remove_dir_if_empty(&subagent_context_dir, report)?;
    if !room_is_live {
        sweep.remove_dir_if_empty(&telemetry_dir, report)?;
    }
    sweep.remove_dir_if_empty(workspace_root, report)?;
    Ok(())
}

/// Reap stale per-session sidecar files — activity heartbeats, statusline
/// context sidecars, and per-subagent context sidecars. A paired advisory lock
/// keeps its stable inode while the JSON record exists; orphaned locks become
/// ordinary stale sidecars on a later sweep.
fn collect_stale_sidecars(
    dir: &Path,
    older_than: Duration,
    sweep: &mut Sweep,
    report: &mut GcReport,
) -> Result<()> {
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
        if path.extension().and_then(|extension| extension.to_str()) == Some("lock")
            && path.with_extension("json").exists()
        {
            continue;
        }
        if !is_older_than(&path, older_than)? {
            continue;
        }
        sweep.remove_file_if_exists(
            &path,
            |report| {
                report.sidecar_files_removed += 1;
            },
            report,
        )?;
    }
    Ok(())
}

/// Reap stale provider probe-throttle markers in the runtime `shared/` dir.
///
/// Live sessions re-touch these stamps within their throttle interval. Session
/// context stamps have a shorter dead-session TTL; other bounded probes use
/// `older_than`.
fn collect_stale_probe_markers(
    shared_dir: &Path,
    older_than: Duration,
    sweep: &mut Sweep,
    report: &mut GcReport,
) -> Result<()> {
    let entries = match fs::read_dir(shared_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(GcErr::ReadDir {
                path: shared_dir.to_path_buf(),
                source,
            });
        }
    };

    for entry in entries {
        let entry = entry.map_err(|source| GcErr::ReadDir {
            path: shared_dir.to_path_buf(),
            source,
        })?;
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        let path = entry.path();
        let threshold = if name.starts_with(SESSION_PROBE_MARKER_PREFIX) {
            older_than.min(SESSION_PROBE_MARKER_TTL)
        } else {
            older_than
        };
        if !is_probe_marker(name) || !is_older_than(&path, threshold)? {
            continue;
        }
        sweep.remove_file_if_exists(
            &path,
            |report| {
                report.probe_markers_removed += 1;
            },
            report,
        )?;
    }
    Ok(())
}

fn is_probe_marker(name: &str) -> bool {
    name.contains("-probe.")
        && !name.ends_with(".json")
        && !name.ends_with(".jsonl")
        && !name.ends_with(".lock")
}

fn collect_stale_read_marks(
    read_marks_dir: &Path,
    heartbeat_dir: &Path,
    older_than: Duration,
    sweep: &mut Sweep,
    report: &mut GcReport,
) -> Result<()> {
    let live_instances = fresh_sidebar_instance_ids(heartbeat_dir, older_than)?;
    let entries = match fs::read_dir(read_marks_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(GcErr::ReadDir {
                path: read_marks_dir.to_path_buf(),
                source,
            });
        }
    };

    for entry in entries {
        let entry = entry.map_err(|source| GcErr::ReadDir {
            path: read_marks_dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let Some(instance_id) = sidebar_instance_id_from_json_name(&path) else {
            continue;
        };
        if live_instances.contains(instance_id.as_str()) || !is_older_than(&path, older_than)? {
            continue;
        }
        sweep.remove_file_if_exists(
            &path,
            |report| {
                report.sidecar_files_removed += 1;
            },
            report,
        )?;
    }
    Ok(())
}

fn fresh_sidebar_instance_ids(
    heartbeat_dir: &Path,
    older_than: Duration,
) -> Result<HashSet<String>> {
    let entries = match fs::read_dir(heartbeat_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(HashSet::new()),
        Err(source) => {
            return Err(GcErr::ReadDir {
                path: heartbeat_dir.to_path_buf(),
                source,
            });
        }
    };

    let mut live_instances = HashSet::new();
    for entry in entries {
        let entry = entry.map_err(|source| GcErr::ReadDir {
            path: heartbeat_dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if !SidebarHeartbeat::is_heartbeat_file(&path) || is_older_than(&path, older_than)? {
            continue;
        }
        if let Some(instance_id) = sidebar_instance_id_from_json_name(&path) {
            live_instances.insert(instance_id.as_str().to_owned());
        }
    }
    Ok(live_instances)
}

fn collect_heartbeats(
    heartbeat_dir: &Path,
    sock_dir: &Path,
    older_than: Duration,
    sweep: &mut Sweep,
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
            sweep.remove_file_if_exists(
                &socket,
                |report| {
                    report.sidebar_sockets_removed += 1;
                },
                report,
            )?;
        }
        sweep.remove_file_if_exists(
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
    Sidebar,
}

fn heartbeat_kind(name: &str) -> Option<HeartbeatKind> {
    if name.starts_with("sidebar.") && name.ends_with(".json") {
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

fn sidebar_instance_id_from_json_name(path: &Path) -> Option<SidebarInstanceId> {
    let name = path.file_name()?.to_str()?;
    let id = name.strip_prefix("sidebar.")?.strip_suffix(".json")?;
    SidebarInstanceId::parse(id).ok()
}

struct Sweep {
    dry_run: bool,
    planned: HashSet<PathBuf>,
    dir_bytes: HashMap<PathBuf, u64>,
}

impl Sweep {
    fn new(dry_run: bool) -> Self {
        Self {
            dry_run,
            planned: HashSet::new(),
            dir_bytes: HashMap::new(),
        }
    }

    fn remember_dir_size(&mut self, path: &Path) {
        if let Ok(meta) = fs::symlink_metadata(path) {
            self.dir_bytes.insert(path.to_path_buf(), meta.len());
        }
    }

    fn remove_file_if_exists(
        &mut self,
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
        if self.dry_run {
            self.record_removed(path, bytes, report, increment);
            return Ok(());
        }
        match fs::remove_file(path) {
            Ok(()) => {
                self.record_removed(path, bytes, report, increment);
                Ok(())
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(GcErr::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    fn remove_dir_if_empty(&mut self, path: &Path, report: &mut GcReport) -> Result<()> {
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(GcErr::ReadDir {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        for entry in entries {
            let entry = entry.map_err(|source| GcErr::ReadDir {
                path: path.to_path_buf(),
                source,
            })?;
            if !self.planned.contains(&entry.path()) {
                return Ok(());
            }
        }
        let bytes = match self.dir_bytes.get(path).copied() {
            Some(bytes) => bytes,
            None => match fs::symlink_metadata(path) {
                Ok(meta) => meta.len(),
                Err(err) if err.kind() == io::ErrorKind::NotFound => 0,
                Err(source) => {
                    return Err(GcErr::Io {
                        path: path.to_path_buf(),
                        source,
                    });
                }
            },
        };
        if self.dry_run {
            self.record_removed(path, bytes, report, |report| {
                report.dirs_removed += 1;
            });
            return Ok(());
        }
        match fs::remove_dir(path) {
            Ok(()) => {
                self.record_removed(path, bytes, report, |report| {
                    report.dirs_removed += 1;
                });
                Ok(())
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(GcErr::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    fn record_removed(
        &mut self,
        path: &Path,
        bytes: u64,
        report: &mut GcReport,
        increment: impl FnOnce(&mut GcReport),
    ) {
        self.planned.insert(path.to_path_buf());
        report.bytes_removed = report.bytes_removed.saturating_add(bytes);
        increment(report);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{MuxName, SidebarInstanceId};
    use crate::sidebar::heartbeat::SidebarHeartbeat;
    use crate::store::RuntimePaths;
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

        let stale_read_marks = rt.sidebar_read_marks_path(&SidebarInstanceId::new());
        fs::write(&stale_read_marks, br#"{"marks":{"row-a":1000}}"#).unwrap();
        let stale_activity = rt.agent_activity_dir.join("deadbeefdeadbeef.json");
        fs::write(
            &stale_activity,
            br#"{"kind":"claude","agent_id":"sess-1","at":"1970-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        let stale_context = rt.agent_context_dir.join("cafef00dcafef00d.json");
        fs::write(&stale_context, b"{}").unwrap();
        let stale_subagent = rt.subagent_context_dir.join("sub.cafebabecafebabe.json");
        fs::write(&stale_subagent, b"{}").unwrap();
        let stale_telemetry = rt.copilot_otel_path();
        fs::write(&stale_telemetry, b"{}\n").unwrap();
        let old = SystemTime::now() - Duration::from_secs(7200);
        for path in [
            &stale_read_marks,
            &stale_activity,
            &stale_context,
            &stale_subagent,
            &stale_telemetry,
        ] {
            fs::File::open(path).unwrap().set_modified(old).unwrap();
        }

        let report =
            collect_runtime_under(&temp.path().join("rimz"), Duration::from_secs(3600), false)
                .unwrap();

        assert_eq!(report.sidecar_files_removed, 5);
        assert!(
            !rt.read_marks_dir.exists(),
            "the emptied read-marks dir is removed"
        );
        assert!(
            !rt.agent_activity_dir.exists(),
            "the emptied activity dir is removed"
        );
        assert!(
            !rt.agent_context_dir.exists(),
            "the emptied context dir is removed"
        );
        assert!(
            !rt.subagent_context_dir.exists(),
            "the emptied subagent-context dir is removed"
        );
        assert!(
            !rt.agent_telemetry_dir.exists(),
            "the emptied telemetry dir is removed"
        );
        assert!(
            !rt.agent_activity_dir.parent().unwrap().exists(),
            "with no runtime files left, the workspace root is reaped too"
        );
    }

    #[test]
    fn runtime_gc_keeps_an_old_lock_while_its_sidecar_is_live() {
        let temp = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(temp.path());
        let rt = RuntimePaths::under(workspace_id, temp.path()).unwrap();
        rt.ensure_dirs().unwrap();
        let context = rt.agent_context_dir.join("ctx.live.json");
        let lock = rt.agent_context_dir.join("ctx.live.lock");
        fs::write(&context, b"{}").unwrap();
        fs::write(&lock, b"").unwrap();
        fs::File::open(&lock)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(7200))
            .unwrap();

        let report =
            collect_runtime_under(&temp.path().join("rimz"), Duration::from_secs(3600), false)
                .unwrap();

        assert_eq!(report.sidecar_files_removed, 0);
        assert!(context.exists(), "fresh sidecar is kept");
        assert!(lock.exists(), "its advisory lock keeps a stable inode");
    }

    #[test]
    fn runtime_gc_dry_run_reports_apply_without_removing() {
        let temp = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(temp.path());
        let rt = RuntimePaths::under(workspace_id, temp.path()).unwrap();
        rt.ensure_dirs().unwrap();

        let stale_activity = rt.agent_activity_dir.join("deadbeefdeadbeef.json");
        fs::write(&stale_activity, b"{}").unwrap();
        fs::File::open(&stale_activity)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(7200))
            .unwrap();

        let preview =
            collect_runtime_under(&temp.path().join("rimz"), Duration::from_secs(3600), true)
                .unwrap();

        assert!(stale_activity.exists(), "dry-run keeps stale sidecar");
        assert!(rt.agent_activity_dir.exists(), "dry-run keeps emptied dirs");

        let applied =
            collect_runtime_under(&temp.path().join("rimz"), Duration::from_secs(3600), false)
                .unwrap();

        assert_eq!(preview, applied);
        assert!(!stale_activity.exists(), "apply removes stale sidecar");
        assert!(
            !rt.agent_activity_dir.parent().unwrap().exists(),
            "apply removes the now-empty workspace root"
        );
    }

    #[test]
    fn runtime_gc_accounts_for_stale_telemetry_and_keeps_fresh_files() {
        let temp = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(temp.path());
        let rt = RuntimePaths::under(workspace_id, temp.path()).unwrap();
        rt.ensure_dirs().unwrap();

        let stale = rt.copilot_otel_path();
        let fresh = rt.agent_telemetry_dir.join("fresh.jsonl");
        fs::write(&stale, b"stale telemetry\n").unwrap();
        fs::write(&fresh, b"fresh telemetry\n").unwrap();
        fs::File::open(&stale)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(7200))
            .unwrap();

        let preview =
            collect_runtime_under(&temp.path().join("rimz"), Duration::from_secs(3600), true)
                .unwrap();
        assert_eq!(preview.sidecar_files_removed, 1);
        assert!(preview.bytes_removed >= b"stale telemetry\n".len() as u64);
        assert!(stale.exists(), "dry-run keeps stale telemetry");

        let applied =
            collect_runtime_under(&temp.path().join("rimz"), Duration::from_secs(3600), false)
                .unwrap();
        assert_eq!(preview, applied);
        assert!(!stale.exists());
        assert!(fresh.exists());
        assert!(rt.agent_telemetry_dir.exists());
    }

    #[test]
    fn runtime_gc_does_not_unlink_a_live_room_exporter() {
        let temp = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(temp.path());
        let rt = RuntimePaths::under(workspace_id.clone(), temp.path()).unwrap();
        rt.ensure_dirs().unwrap();

        let telemetry = rt.copilot_otel_path();
        fs::write(&telemetry, b"open exporter\n").unwrap();
        fs::File::open(&telemetry)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(7200))
            .unwrap();
        let instance_id = SidebarInstanceId::new();
        let heartbeat = SidebarHeartbeat::new(
            workspace_id,
            instance_id.clone(),
            MuxName::Tmux,
            "rimz-live",
            rt.sock_dir.join("sidebar.live.sock"),
            None,
        );
        write_json(&rt.sidebar_heartbeat_path(&instance_id), &heartbeat);

        let report =
            collect_runtime_under(&temp.path().join("rimz"), Duration::from_secs(3600), false)
                .unwrap();

        assert_eq!(report.sidecar_files_removed, 0);
        assert!(telemetry.exists(), "live exporter inode remains linked");
        assert!(rt.agent_telemetry_dir.exists());
    }

    #[test]
    fn runtime_gc_removes_stale_sidebar_heartbeats_and_sidebar_socket_only() {
        let temp = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(temp.path());
        let rt = RuntimePaths::under(workspace_id.clone(), temp.path()).unwrap();
        rt.ensure_dirs().unwrap();

        let stale_socket = rt.sock_dir.join("sidebar.stale.sock");
        fs::write(&stale_socket, b"socket placeholder").unwrap();
        let run_socket = rt.sock_dir.join("run.123456789abc.sock");
        fs::write(&run_socket, b"run socket placeholder").unwrap();

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

        let legacy_unknown_path = rt.heartbeat_dir.join("unknown.opus-policy.json");
        fs::write(&legacy_unknown_path, b"{}").unwrap();

        let old = SystemTime::now() - Duration::from_secs(7200);
        for path in [&stale_socket, &stale_sidebar_path, &legacy_unknown_path] {
            fs::File::open(path).unwrap().set_modified(old).unwrap();
        }

        let report =
            collect_runtime_under(&temp.path().join("rimz"), Duration::from_secs(3600), false)
                .unwrap();

        assert_eq!(report.runtime_roots_scanned, 1);
        assert_eq!(report.heartbeat_files_removed, 1);
        assert_eq!(report.sidebar_sockets_removed, 1);
        assert!(!stale_sidebar_path.exists());
        assert!(!stale_socket.exists());
        assert!(legacy_unknown_path.exists());
        assert!(run_socket.exists(), "run sockets are not GC-owned");
    }

    #[test]
    fn runtime_gc_keeps_stale_read_marks_while_owner_heartbeat_is_fresh() {
        let temp = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(temp.path());
        let rt = RuntimePaths::under(workspace_id.clone(), temp.path()).unwrap();
        rt.ensure_dirs().unwrap();

        let instance_id = SidebarInstanceId::new();
        let read_marks = rt.sidebar_read_marks_path(&instance_id);
        fs::write(&read_marks, br#"{"marks":{"row-a":1000}}"#).unwrap();
        fs::File::open(&read_marks)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(7200))
            .unwrap();

        let socket = rt
            .sock_dir
            .join(format!("sidebar.{}.sock", instance_id.short()));
        let heartbeat = SidebarHeartbeat::new(
            workspace_id,
            instance_id.clone(),
            MuxName::Tmux,
            "rimz-test",
            socket,
            None,
        );
        write_json(&rt.sidebar_heartbeat_path(&instance_id), &heartbeat);

        let report =
            collect_runtime_under(&temp.path().join("rimz"), Duration::from_secs(3600), false)
                .unwrap();

        assert_eq!(report.sidecar_files_removed, 0);
        assert!(read_marks.exists(), "live owner's read marks are kept");
    }

    #[test]
    fn runtime_gc_reaps_stale_probe_markers() {
        let temp = tempdir().unwrap();
        let shared = temp.path().join("rimz").join("shared");
        fs::create_dir_all(&shared).unwrap();
        let nonce = "00000000000000000000000000000000";
        let stale_session = shared.join(format!("{SESSION_PROBE_MARKER_PREFIX}{nonce}"));
        let recent_session = shared.join(format!(
            "{SESSION_PROBE_MARKER_PREFIX}11111111111111111111111111111111"
        ));
        let accounts = shared.join("accounts.json");
        let lock = shared.join("accounts.lock");
        let trace = shared.join("rate_limits_trace.jsonl");
        for path in [&stale_session, &recent_session, &accounts, &lock, &trace] {
            fs::write(path, b"probe").unwrap();
        }
        let old = SystemTime::now() - Duration::from_secs(7200);
        for path in [&stale_session, &accounts, &lock, &trace] {
            fs::File::open(path).unwrap().set_modified(old).unwrap();
        }
        let recently_dead = SystemTime::now() - (SESSION_PROBE_MARKER_TTL + Duration::from_secs(1));
        fs::File::open(&recent_session)
            .unwrap()
            .set_modified(recently_dead)
            .unwrap();

        let report =
            collect_runtime_under(&temp.path().join("rimz"), Duration::from_secs(3600), false)
                .unwrap();

        assert_eq!(report.probe_markers_removed, 2);
        assert!(!stale_session.exists());
        assert!(!recent_session.exists());
        assert!(accounts.exists());
        assert!(lock.exists());
        assert!(trace.exists());
    }

    fn write_json<T: serde::Serialize>(path: &Path, value: &T) {
        fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    }
}
