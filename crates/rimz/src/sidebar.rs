//! Sidebar process liveness helpers.
//!
//! The sidebar heartbeat remains a latency hint. A stale, unreadable, or
//! protocol-mismatched heartbeat never blocks a fresh launch.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use tracing::debug;

use crate::ids::{MuxName, SidebarInstanceId, WorkspaceId};
use crate::ledger::RuntimePaths;
use crate::ledger::atomic;
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

#[derive(Debug, thiserror::Error)]
#[error("writing sidebar heartbeat {path}: {source}")]
pub struct HeartbeatWriteErr {
    pub path: PathBuf,
    #[source]
    pub source: atomic::AtomicErr,
}

/// Write this sidebar instance's liveness heartbeat in-process.
///
/// The heartbeat is a runtime liveness file, not ledger truth, so the renderer
/// owns it directly rather than forking `rimz sidebar heartbeat` once per tick.
/// The JSON shape and the atomic temp-then-rename are identical to the CLI path
/// they replace, so the ledger wakeup fanout and the launch freshness gate that
/// read it are unchanged. The renderer ensures the runtime dirs at startup, so
/// this only does the write.
pub fn write_heartbeat(
    runtime: &RuntimePaths,
    workspace_id: WorkspaceId,
    instance_id: &SidebarInstanceId,
    mux: MuxName,
    session_name: &str,
    wakeup_socket: &Path,
) -> Result<(), HeartbeatWriteErr> {
    let heartbeat = SidebarHeartbeat::new(
        workspace_id,
        instance_id.clone(),
        mux,
        session_name,
        wakeup_socket.to_path_buf(),
    );
    let path = runtime.sidebar_heartbeat_path(instance_id);
    atomic::write_temp_then_rename(&path, &heartbeat)
        .map_err(|source| HeartbeatWriteErr { path, source })
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
        if !SidebarHeartbeat::is_heartbeat_file(&path) {
            continue;
        }
        if !heartbeat_mtime_fresh(&path) {
            continue;
        }
        let heartbeat = match SidebarHeartbeat::read_from(&path) {
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
    let mut opts = opts.clone();
    opts.replace_existing = true;
    match backend.open_sidebar(&opts) {
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

    #[test]
    fn in_process_write_heartbeat_is_fresh_and_round_trips() {
        // The renderer writes its heartbeat in-process now; it must land in the
        // same shape and freshness the ledger wakeup fanout and launch gate read.
        let h = Harness::new();
        h.ensure_runtime();
        let instance = SidebarInstanceId::new();
        let socket = h.runtime.sock_dir.join("sidebar.test.sock");

        write_heartbeat(
            &h.runtime,
            h.workspace_id.clone(),
            &instance,
            MuxName::Zellij,
            "rimz-test",
            &socket,
        )
        .expect("write heartbeat");

        assert!(fresh_sidebar_present(&h.runtime));
        let path = h.runtime.sidebar_heartbeat_path(&instance);
        let hb = SidebarHeartbeat::read_from(&path).expect("read back");
        assert_eq!(hb.instance_id, instance);
        assert_eq!(hb.protocol_version, SIDEBAR_PROTOCOL_VERSION);
        assert_eq!(hb.mux, MuxName::Zellij);
        assert_eq!(hb.wakeup_socket, socket);
    }
}
