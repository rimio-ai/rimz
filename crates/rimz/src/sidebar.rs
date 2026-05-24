//! Sidebar process liveness helpers.
//!
//! The sidebar heartbeat remains a latency hint. A stale, unreadable, or
//! protocol-mismatched heartbeat never blocks a fresh launch.

use std::fs;
use std::path::Path;
use std::time::SystemTime;

use tracing::debug;

use crate::ledger::RuntimePaths;
use crate::ledger::wakeup::SIDEBAR_HEARTBEAT_TTL;
use crate::mux::{MuxBackend, SidebarPaneOptions};
use crate::schema::SIDEBAR_PROTOCOL_VERSION;
use crate::schema::heartbeat::SidebarHeartbeat;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarLaunchOutcome {
    SkippedFresh,
    Opened,
    Failed,
}

pub fn fresh_sidebar_present(rt: &RuntimePaths) -> bool {
    let entries = match fs::read_dir(&rt.heartbeat_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return false,
        Err(err) => {
            debug!(path = %rt.heartbeat_dir.display(), error = %err, "sidebar heartbeat dir unreadable");
            return false;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !is_sidebar_heartbeat(&path) {
            continue;
        }
        if !heartbeat_mtime_fresh(&path) {
            continue;
        }
        let heartbeat = match read_sidebar_heartbeat(&path) {
            Ok(heartbeat) => heartbeat,
            Err(err) => {
                debug!(path = %path.display(), error = %err, "sidebar heartbeat unreadable");
                continue;
            }
        };
        if heartbeat.protocol_version == SIDEBAR_PROTOCOL_VERSION {
            return true;
        }
    }

    false
}

pub fn launch_sidebar_if_needed(
    backend: &dyn MuxBackend,
    runtime: &RuntimePaths,
    opts: &SidebarPaneOptions,
) -> SidebarLaunchOutcome {
    if fresh_sidebar_present(runtime) {
        return SidebarLaunchOutcome::SkippedFresh;
    }
    match backend.open_sidebar(opts) {
        Ok(()) => SidebarLaunchOutcome::Opened,
        Err(err) => {
            tracing::warn!(
                session = %opts.session_name,
                mux = %backend.name(),
                error = %err,
                "sidebar pane launch failed; continuing without sidebar",
            );
            SidebarLaunchOutcome::Failed
        }
    }
}

fn is_sidebar_heartbeat(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("sidebar.") && name.ends_with(".json"))
}

fn read_sidebar_heartbeat(path: &Path) -> std::io::Result<SidebarHeartbeat> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(std::io::Error::other)
}

fn heartbeat_mtime_fresh(path: &Path) -> bool {
    let modified = match fs::metadata(path).and_then(|meta| meta.modified()) {
        Ok(modified) => modified,
        Err(err) => {
            debug!(path = %path.display(), error = %err, "sidebar heartbeat metadata unreadable");
            return false;
        }
    };
    match SystemTime::now().duration_since(modified) {
        Ok(age) => age <= SIDEBAR_HEARTBEAT_TTL,
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tempfile::TempDir;

    use super::*;
    use crate::ids::{MuxName, SidebarInstanceId, WorkspaceId};

    struct Harness {
        _dir: TempDir,
        runtime: RuntimePaths,
        workspace_id: WorkspaceId,
    }

    impl Harness {
        fn new() -> Self {
            let dir = TempDir::new().expect("tempdir");
            let workspace_id = WorkspaceId::from_project_root(dir.path());
            let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime");
            Self {
                _dir: dir,
                runtime,
                workspace_id,
            }
        }

        fn ensure_runtime(&self) {
            self.runtime.ensure_dirs().expect("runtime dirs");
        }

        fn write_sidebar(&self, filename: &str, protocol_version: &str) -> std::path::PathBuf {
            self.ensure_runtime();
            let mut heartbeat = SidebarHeartbeat::new(
                self.workspace_id.clone(),
                SidebarInstanceId::new(),
                MuxName::Tmux,
                "session",
                self.runtime.sock_dir.join("sidebar.sock"),
            );
            heartbeat.protocol_version = protocol_version.to_owned();
            let path = self.runtime.heartbeat_dir.join(filename);
            std::fs::write(&path, serde_json::to_vec(&heartbeat).expect("json"))
                .expect("write heartbeat");
            path
        }
    }

    #[test]
    fn absent_heartbeat_dir_is_not_fresh() {
        let h = Harness::new();
        assert!(!fresh_sidebar_present(&h.runtime));
    }

    #[test]
    fn fresh_current_protocol_heartbeat_is_present() {
        let h = Harness::new();
        h.write_sidebar("sidebar.fresh.json", SIDEBAR_PROTOCOL_VERSION);
        assert!(fresh_sidebar_present(&h.runtime));
    }

    #[test]
    fn stale_heartbeat_is_ignored() {
        let h = Harness::new();
        let path = h.write_sidebar("sidebar.stale.json", SIDEBAR_PROTOCOL_VERSION);
        let old = SystemTime::now() - SIDEBAR_HEARTBEAT_TTL - Duration::from_secs(1);
        std::fs::File::open(&path)
            .expect("open heartbeat")
            .set_modified(old)
            .expect("set mtime");
        assert!(!fresh_sidebar_present(&h.runtime));
    }

    #[test]
    fn wrong_protocol_heartbeat_is_ignored() {
        let h = Harness::new();
        h.write_sidebar("sidebar.old.json", "rimz.plugin.v0");
        assert!(!fresh_sidebar_present(&h.runtime));
    }

    #[test]
    fn unreadable_json_heartbeat_is_ignored() {
        let h = Harness::new();
        h.ensure_runtime();
        std::fs::write(
            h.runtime.heartbeat_dir.join("sidebar.invalid.json"),
            b"{ not json",
        )
        .expect("write invalid heartbeat");
        assert!(!fresh_sidebar_present(&h.runtime));
    }
}
