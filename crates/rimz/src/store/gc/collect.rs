use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::{
    GcErr, GcReport, Result, SESSION_PROBE_MARKER_PREFIX, SESSION_PROBE_MARKER_TTL,
    read_dir_if_exists,
};
use crate::disk::paths::Class;
use crate::ids::SidebarInstanceId;
#[cfg(test)]
use crate::ids::WorkspaceId;
use crate::wakeup::heartbeat::SidebarHeartbeat;

#[must_use = "maintenance report; surface it to the caller"]
pub(crate) fn collect_runtime_under(
    state_root: &Path,
    runtime_root: &Path,
    shared_root: &Path,
    older_than: Duration,
    dry_run: bool,
) -> Result<GcReport> {
    let entries = read_dir_if_exists(runtime_root)?;

    let mut report = GcReport::default();
    let mut sweep = Sweep::new(dry_run);
    for entry in entries.into_iter().flatten() {
        let entry = entry.map_err(|source| GcErr::ReadDir {
            path: runtime_root.to_path_buf(),
            source,
        })?;
        let root = entry.path();
        if !root.is_dir() {
            continue;
        }
        let known =
            match crate::disk::paths::check_workspace_layout(&state_root.join(entry.file_name())) {
                Ok(known) => known,
                Err(err) => {
                    tracing::warn!(error = %err, "skipping workspace runtime sweep");
                    continue;
                }
            };
        report.runtime_roots_scanned += 1;
        collect_runtime_classes(&root, older_than, &mut sweep, &mut report)?;
        if !known {
            for class in Class::RUNTIME {
                sweep.remove_dir_if_empty(&class.path_under(&root), &mut report)?;
            }
            sweep.remove_dir_if_empty(&root, &mut report)?;
        }
    }
    collect_stale_probe_markers(shared_root, older_than, &mut sweep, &mut report)?;

    Ok(report)
}

fn collect_runtime_classes(
    workspace_root: &Path,
    older_than: Duration,
    sweep: &mut Sweep,
    report: &mut GcReport,
) -> Result<()> {
    sweep.remember_dir_size(workspace_root);
    let mut classes = Vec::new();
    for class in Class::RUNTIME {
        let mut removed = GcReport::default();
        let dir = class.path_under(workspace_root);
        match class {
            Class::Live => collect_live(workspace_root, Some(older_than), sweep, &mut removed)?,
            Class::Lanes => {}
            Class::Sock => collect_sock(&dir, sweep, &mut removed)?,
            _ => unreachable!("only runtime classes are selected above"),
        }
        classes.push(super::ClassReport {
            class: class.dir_name().to_owned(),
            files_removed: removed.heartbeat_files_removed
                + removed.sidecar_files_removed
                + removed.sidebar_sockets_removed,
            bytes_removed: removed.bytes_removed,
            locks_would_check: removed.locks_would_check,
        });
        report.heartbeat_files_removed += removed.heartbeat_files_removed;
        report.sidecar_files_removed += removed.sidecar_files_removed;
        report.sidebar_sockets_removed += removed.sidebar_sockets_removed;
        report.dirs_removed += removed.dirs_removed;
        report.bytes_removed += removed.bytes_removed;
    }
    report.rooms.push(super::RoomReport {
        retained_reason: None,
        name: workspace_root
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        classes,
    });
    Ok(())
}

/// The live class expires by mtime. Its one retention exception is agent-telemetry: preserve the exporter file and directory while the room is live, since the external exporter holds the inode open and has no verified reopen contract. With no class TTL, only dead renderer claims expire at the heartbeat protocol TTL.
fn collect_live(
    workspace_root: &Path,
    class_ttl: Option<Duration>,
    sweep: &mut Sweep,
    report: &mut GcReport,
) -> Result<()> {
    let dir = Class::Live.path_under(workspace_root);
    let heartbeat = dir.join("heartbeat");
    let read_marks = dir.join("read-marks");
    let telemetry = dir.join("agent-telemetry");
    let protocol_ttl = crate::wakeup::heartbeat::SIDEBAR_HEARTBEAT_TTL;
    let live = fresh_sidebar_instance_ids(&heartbeat, class_ttl.unwrap_or(protocol_ttl))?;
    let keep_telemetry = !live.is_empty();
    for (path, _, _) in sweep.files_under(&dir)? {
        if keep_telemetry && path.starts_with(&telemetry) {
            continue;
        }
        let threshold = match class_ttl {
            Some(ttl) => ttl,
            None if path.parent() == Some(heartbeat.as_path())
                && SidebarHeartbeat::is_heartbeat_file(&path) =>
            {
                protocol_ttl
            }
            None if path.parent() == Some(read_marks.as_path())
                && sidebar_instance_id_from_json_name(&path)
                    .is_some_and(|id| !live.contains(id.as_str())) =>
            {
                protocol_ttl
            }
            None => continue,
        };
        if is_older_than(&path, threshold)? {
            sweep.remove_file_if_exists(
                &path,
                |report| {
                    if path.parent() == Some(heartbeat.as_path())
                        && SidebarHeartbeat::is_heartbeat_file(&path)
                    {
                        report.heartbeat_files_removed += 1;
                    } else {
                        report.sidecar_files_removed += 1;
                    }
                },
                report,
            )?;
        }
    }
    sweep.remove_empty_dirs(&dir, keep_telemetry.then_some(telemetry.as_path()), report)
}

pub(super) fn collect_ttl(
    dir: &Path,
    ttl: Duration,
    sweep: &mut Sweep,
    report: &mut GcReport,
) -> Result<()> {
    for (path, _, _) in sweep.files_under(dir)? {
        if is_older_than(&path, ttl)? {
            sweep.remove_file_if_exists(
                &path,
                |report| report.sidecar_files_removed += 1,
                report,
            )?;
        }
    }
    sweep.remove_empty_dirs(dir, None, report)
}

fn collect_sock(dir: &Path, sweep: &mut Sweep, report: &mut GcReport) -> Result<()> {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    use nix::sys::socket::{AddressFamily, SockFlag, SockType, UnixAddr, connect, socket};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::FileTypeExt;

    for (path, _, _) in sweep.files_under(dir)? {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => return Err(GcErr::Io { path, source }),
        };
        if !metadata.file_type().is_socket() {
            continue;
        }
        if !is_older_than(&path, crate::wakeup::heartbeat::SIDEBAR_HEARTBEAT_TTL)? {
            continue;
        }
        let address = UnixAddr::new(&path).map_err(|source| GcErr::Io {
            path: path.clone(),
            source: source.into(),
        })?;
        for kind in [SockType::Datagram, SockType::Stream] {
            // nix has no non-blocking `SockFlag` on Darwin, so `fcntl` sets it portably.
            let probe = socket(AddressFamily::Unix, kind, SockFlag::empty(), None)
                .and_then(|probe| {
                    fcntl(&probe, FcntlArg::F_SETFL(OFlag::O_NONBLOCK))?;
                    Ok(probe)
                })
                .map_err(|source| GcErr::Io {
                    path: path.clone(),
                    source: source.into(),
                })?;
            match connect(probe.as_raw_fd(), &address) {
                Err(nix::errno::Errno::EPROTOTYPE) => continue,
                Err(nix::errno::Errno::ECONNREFUSED) => {
                    sweep.remove_file_if_exists(
                        &path,
                        |report| report.sidebar_sockets_removed += 1,
                        report,
                    )?;
                }
                _ => {}
            }
            break;
        }
    }
    sweep.remove_empty_dirs(dir, None, report)
}

pub(super) fn collect_claims(runtime: &crate::RuntimePaths) -> Result<()> {
    let mut sweep = Sweep::new(false);
    let mut report = GcReport::default();
    collect_live(&runtime.root, None, &mut sweep, &mut report)?;
    collect_sock(&runtime.sock_dir, &mut sweep, &mut report)
}

pub(super) fn collect_locks(dir: &Path, sweep: &mut Sweep, report: &mut GcReport) -> Result<()> {
    for entry in read_dir_if_exists(dir)?.into_iter().flatten() {
        let entry = entry.map_err(|source| GcErr::ReadDir {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let io_err = |source| GcErr::Io {
            path: path.clone(),
            source,
        };
        let file_type = entry.file_type().map_err(io_err)?;
        // Lock subdirectories stay like class roots: a waiter reopening an
        // unlinked lock recreates only the file, never its parent.
        if file_type.is_dir() {
            collect_locks(&path, sweep, report)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        if sweep.dry_run {
            report.locks_would_check += 1;
            continue;
        }
        let mut file = match fs::OpenOptions::new().read(true).write(true).open(&path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(io_err(err)),
        };
        match crate::disk::lock::try_lock_file(&mut file, &path) {
            Ok(()) => sweep.remove_file_if_exists(
                &path,
                |report| report.sidecar_files_removed += 1,
                report,
            )?,
            Err(fs::TryLockError::WouldBlock) => continue,
            Err(fs::TryLockError::Error(err)) => return Err(io_err(err)),
        }
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
    let Some(entries) = read_dir_if_exists(shared_dir)? else {
        return Ok(());
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

fn fresh_sidebar_instance_ids(
    heartbeat_dir: &Path,
    older_than: Duration,
) -> Result<HashSet<String>> {
    let Some(entries) = read_dir_if_exists(heartbeat_dir)? else {
        return Ok(HashSet::new());
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

fn sidebar_instance_id_from_json_name(path: &Path) -> Option<SidebarInstanceId> {
    let name = path.file_name()?.to_str()?;
    let id = name.strip_prefix("sidebar.")?.strip_suffix(".json")?;
    SidebarInstanceId::parse(id).ok()
}

pub(super) struct Sweep {
    dry_run: bool,
    planned: HashSet<PathBuf>,
    dir_bytes: HashMap<PathBuf, u64>,
}

impl Sweep {
    pub(super) fn files_under(&mut self, dir: &Path) -> Result<Vec<(PathBuf, SystemTime, u64)>> {
        self.remember_dir_size(dir);
        let mut files = Vec::new();
        for entry in read_dir_if_exists(dir)?.into_iter().flatten() {
            let entry = entry.map_err(|source| GcErr::ReadDir {
                path: dir.to_owned(),
                source,
            })?;
            let path = entry.path();
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => return Err(GcErr::Io { path, source }),
            };
            if metadata.is_dir() {
                files.extend(self.files_under(&path)?);
            } else {
                let modified = metadata.modified().map_err(|source| GcErr::Io {
                    path: path.clone(),
                    source,
                })?;
                files.push((path, modified, metadata.len()));
            }
        }
        Ok(files)
    }

    pub(super) fn remove_empty_dirs(
        &mut self,
        root: &Path,
        keep: Option<&Path>,
        report: &mut GcReport,
    ) -> Result<()> {
        let mut dirs: Vec<_> = self
            .dir_bytes
            .keys()
            .filter(|dir| {
                dir.as_path() != root
                    && dir.starts_with(root)
                    && keep.is_none_or(|keep| !dir.starts_with(keep))
            })
            .cloned()
            .collect();
        dirs.sort();
        for dir in dirs.into_iter().rev() {
            self.remove_dir_if_empty(&dir, report)?;
        }
        Ok(())
    }

    pub(super) fn remove_tree(
        &mut self,
        path: &Path,
        files: usize,
        report: &mut GcReport,
    ) -> Result<()> {
        let bytes = crate::disk::usage::dir_size(path);
        if !self.dry_run {
            match fs::remove_dir_all(path) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(source) => {
                    return Err(GcErr::Io {
                        path: path.to_owned(),
                        source,
                    });
                }
            }
        }
        self.record_removed(path, bytes, report, |report| {
            report.sidecar_files_removed += files
        });
        Ok(())
    }

    pub(super) fn new(dry_run: bool) -> Self {
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

    pub(super) fn remove_file_if_exists(
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
        let Some(entries) = read_dir_if_exists(path)? else {
            return Ok(());
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
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
                ) =>
            {
                Ok(())
            }
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

/// A file that vanished between the listing and the stat (a concurrent sweep
/// or an atomic publish renaming its temp) is not a candidate.
pub(super) fn is_older_than(path: &Path, older_than: Duration) -> Result<bool> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(GcErr::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
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
mod tests;
