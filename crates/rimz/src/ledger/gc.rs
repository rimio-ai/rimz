//! Runtime garbage collection.
//!
//! Durable ledger state is never removed here. This module only cleans
//! runtime liveness hints that are older than an operator-supplied threshold:
//! resolver/sidebar heartbeat JSON and sidebar wakeup sockets named by stale
//! sidebar heartbeats. Per-request `feed.*.sock` files are deliberately left
//! alone because a long-running `feed ask` may still own one.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::ids::WorkspaceId;
use crate::ledger::paths;
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
    collect_heartbeats(&heartbeat_dir, &sock_dir, older_than, report)?;
    remove_dir_if_empty(&heartbeat_dir, report)?;
    remove_dir_if_empty(&sock_dir, report)?;
    remove_dir_if_empty(workspace_root, report)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{MuxName, ResolverId, SidebarInstanceId};
    use crate::ledger::RuntimePaths;
    use crate::schema::heartbeat::{ResolverHeartbeat, SidebarHeartbeat};
    use std::time::SystemTime;
    use tempfile::tempdir;

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
}
