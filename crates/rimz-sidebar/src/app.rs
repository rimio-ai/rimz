//! Runtime loop for the native sidebar process.

use std::io;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use jiff::Timestamp;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use rimz::ledger::paths::PathErr;
use rimz::{MuxName, RuntimePaths, SidebarInstanceId, SidebarSnapshot, WorkspaceId};
use tracing::warn;

use crate::render::{self, FetchStatus};

#[derive(Clone, Debug)]
pub struct ServeConfig {
    pub workspace_id: WorkspaceId,
    pub mux: MuxName,
    pub session_name: String,
    pub instance_id: SidebarInstanceId,
    pub tick_seconds: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum SidebarAppErr {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Paths(#[from] PathErr),
    #[error("snapshot command failed: {stderr}")]
    SnapshotCommand { stderr: String },
    #[error("heartbeat command failed: {stderr}")]
    HeartbeatCommand { stderr: String },
}

pub type Result<T> = std::result::Result<T, SidebarAppErr>;

pub fn serve(config: ServeConfig) -> Result<()> {
    let runtime = RuntimePaths::for_workspace(config.workspace_id.clone())?;
    runtime.ensure_dirs()?;
    let socket_path = sidebar_socket_path(&runtime, &config.instance_id);
    let socket = bind_socket(&socket_path)?;
    let _cleanup = SocketGuard {
        path: socket_path.clone(),
    };
    let tick = tick_for(config.tick_seconds);
    socket.set_read_timeout(Some(tick))?;

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut last_snapshot: Option<SidebarSnapshot> = None;
    let mut status = FetchStatus::Ok;

    loop {
        let heartbeat_outcome = write_heartbeat(&config, &socket_path);
        let snapshot_outcome = fetch_snapshot(&config.workspace_id);

        let state = compute_next_state(
            &config.workspace_id,
            heartbeat_outcome.as_ref().err().map(|e| e.to_string()),
            snapshot_outcome.map_err(|e| e.to_string()),
            last_snapshot.take(),
            &status,
        );
        if let Err(err) = &heartbeat_outcome {
            warn!(error = %err, "sidebar heartbeat failed");
        }
        if let FetchStatus::Degraded { reason, .. } = &state.status {
            warn!(reason = %reason, "sidebar refresh degraded");
        }
        last_snapshot = state.last_snapshot;
        status = state.status;
        render::draw_to_terminal(&mut terminal, &state.snapshot, &status)?;
        wait_for_wakeup(&socket)?;
    }
}

/// Decide what to render next given the latest heartbeat + snapshot outcomes.
/// Pure data, no I/O — extracted so the loop's recovery rules are testable.
pub fn compute_next_state(
    workspace_id: &WorkspaceId,
    heartbeat_failure: Option<String>,
    snapshot: std::result::Result<SidebarSnapshot, String>,
    previous_snapshot: Option<SidebarSnapshot>,
    previous_status: &FetchStatus,
) -> RenderState {
    let (last_snapshot, snapshot_failure) = match snapshot {
        Ok(snapshot) => (Some(snapshot), None),
        Err(reason) => (previous_snapshot, Some(reason)),
    };

    let new_status = match (snapshot_failure, heartbeat_failure) {
        (None, None) => FetchStatus::Ok,
        (Some(reason), _) => promote(previous_status, format!("snapshot failed: {reason}")),
        (None, Some(reason)) => promote(previous_status, format!("heartbeat failed: {reason}")),
    };

    let snapshot_to_render = last_snapshot
        .clone()
        .unwrap_or_else(|| placeholder_snapshot(workspace_id.clone()));

    RenderState {
        snapshot: snapshot_to_render,
        status: new_status,
        last_snapshot,
    }
}

/// Reuse the existing `since` when the previous status was already degraded
/// so the banner can show monotonically increasing "for Ns" elapsed time.
fn promote(previous: &FetchStatus, reason: String) -> FetchStatus {
    match previous {
        FetchStatus::Degraded { since, .. } => FetchStatus::Degraded {
            reason,
            since: *since,
        },
        FetchStatus::Ok => FetchStatus::degraded(reason),
    }
}

fn tick_for(seconds: u64) -> Duration {
    Duration::from_secs(seconds.max(1))
}

fn placeholder_snapshot(workspace_id: WorkspaceId) -> SidebarSnapshot {
    SidebarSnapshot {
        workspace_id,
        generated_at: Timestamp::now(),
        needs_attention: Vec::new(),
        resolver_working: Vec::new(),
        recently_answered: Vec::new(),
        recent_activity: Vec::new(),
        agents: Vec::new(),
    }
}

/// Bundle returned by [`compute_next_state`]; the loop applies it verbatim.
#[derive(Clone, Debug)]
pub struct RenderState {
    pub snapshot: SidebarSnapshot,
    pub status: FetchStatus,
    pub last_snapshot: Option<SidebarSnapshot>,
}

fn fetch_snapshot(workspace_id: &WorkspaceId) -> Result<SidebarSnapshot> {
    let output = Command::new("rimz")
        .args(["sidebar", "snapshot", "--workspace-id"])
        .arg(workspace_id.as_str())
        .arg("--json")
        .output()?;
    if !output.status.success() {
        return Err(SidebarAppErr::SnapshotCommand {
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn write_heartbeat(config: &ServeConfig, socket_path: &Path) -> Result<()> {
    let output = Command::new("rimz")
        .args(["sidebar", "heartbeat", "--workspace-id"])
        .arg(config.workspace_id.as_str())
        .arg("--instance-id")
        .arg(config.instance_id.as_str())
        .arg("--mux")
        .arg(config.mux.as_str())
        .arg("--session-name")
        .arg(&config.session_name)
        .arg("--wakeup-socket")
        .arg(socket_path)
        .output()?;
    if !output.status.success() {
        return Err(SidebarAppErr::HeartbeatCommand {
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(())
}

fn sidebar_socket_path(runtime: &RuntimePaths, instance_id: &SidebarInstanceId) -> PathBuf {
    runtime
        .sock_dir
        .join(format!("sidebar.{}.sock", instance_id.as_str()))
}

fn bind_socket(path: &Path) -> io::Result<UnixDatagram> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    UnixDatagram::bind(path)
}

fn wait_for_wakeup(socket: &UnixDatagram) -> io::Result<()> {
    let mut buf = [0_u8; 4096];
    match socket.recv(&mut buf) {
        Ok(_) => Ok(()),
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            Ok(())
        }
        Err(err) => Err(err),
    }
}

struct SocketGuard {
    path: PathBuf,
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                warn!(path = %self.path.display(), error = %err, "sidebar socket cleanup failed")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> WorkspaceId {
        WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap()
    }

    fn snapshot(ws: &WorkspaceId) -> SidebarSnapshot {
        placeholder_snapshot(ws.clone())
    }

    #[test]
    fn first_ok_fetch_clears_status_and_records_snapshot() {
        let ws = workspace();
        let snap = snapshot(&ws);
        let state = compute_next_state(&ws, None, Ok(snap.clone()), None, &FetchStatus::Ok);
        assert!(matches!(state.status, FetchStatus::Ok));
        assert!(state.last_snapshot.is_some());
        assert_eq!(state.snapshot.workspace_id, ws);
    }

    #[test]
    fn fetch_failure_uses_previous_snapshot_and_marks_degraded() {
        let ws = workspace();
        let previous = snapshot(&ws);
        let state = compute_next_state(
            &ws,
            None,
            Err("ledger not found".to_owned()),
            Some(previous.clone()),
            &FetchStatus::Ok,
        );
        match &state.status {
            FetchStatus::Degraded { reason, .. } => {
                assert!(reason.contains("snapshot failed"));
                assert!(reason.contains("ledger not found"));
            }
            FetchStatus::Ok => panic!("expected Degraded"),
        }
        assert!(state.last_snapshot.is_some());
        assert_eq!(state.snapshot.workspace_id, previous.workspace_id);
    }

    #[test]
    fn fetch_failure_without_previous_snapshot_uses_placeholder() {
        let ws = workspace();
        let state = compute_next_state(
            &ws,
            None,
            Err("ledger not found".to_owned()),
            None,
            &FetchStatus::Ok,
        );
        assert!(matches!(state.status, FetchStatus::Degraded { .. }));
        assert!(state.last_snapshot.is_none());
        assert_eq!(state.snapshot.workspace_id, ws);
        assert!(state.snapshot.needs_attention.is_empty());
    }

    #[test]
    fn heartbeat_failure_alone_marks_degraded() {
        let ws = workspace();
        let snap = snapshot(&ws);
        let state = compute_next_state(
            &ws,
            Some("hb failed".to_owned()),
            Ok(snap.clone()),
            None,
            &FetchStatus::Ok,
        );
        match &state.status {
            FetchStatus::Degraded { reason, .. } => {
                assert!(reason.contains("heartbeat failed"));
            }
            FetchStatus::Ok => panic!("expected Degraded"),
        }
        // Heartbeat failing does not invalidate a fresh snapshot.
        assert!(state.last_snapshot.is_some());
    }

    #[test]
    fn promote_preserves_since_across_iterations() {
        let ws = workspace();
        let first = compute_next_state(&ws, None, Err("first".to_owned()), None, &FetchStatus::Ok);
        let FetchStatus::Degraded {
            since: first_since, ..
        } = first.status.clone()
        else {
            panic!("expected first iteration to be Degraded");
        };
        let second = compute_next_state(
            &ws,
            None,
            Err("second".to_owned()),
            first.last_snapshot,
            &first.status,
        );
        match &second.status {
            FetchStatus::Degraded { since, reason } => {
                assert_eq!(*since, first_since, "since must remain pinned");
                assert!(reason.contains("second"));
            }
            FetchStatus::Ok => panic!("expected second iteration still Degraded"),
        }
    }

    #[test]
    fn recovery_clears_degraded_status() {
        let ws = workspace();
        let degraded = FetchStatus::degraded("snapshot failed: x");
        let recovered = compute_next_state(&ws, None, Ok(snapshot(&ws)), None, &degraded);
        assert!(matches!(recovered.status, FetchStatus::Ok));
    }

    #[test]
    fn tick_for_honours_above_two_seconds() {
        assert_eq!(tick_for(5), Duration::from_secs(5));
    }

    #[test]
    fn tick_for_clamps_zero_to_one() {
        assert_eq!(tick_for(0), Duration::from_secs(1));
    }
}
