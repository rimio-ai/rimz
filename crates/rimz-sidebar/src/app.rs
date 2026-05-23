//! Runtime loop for the native sidebar process.

use std::io;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use rimz::ledger::paths::PathErr;
use rimz::{MuxName, RuntimePaths, SidebarInstanceId, SidebarSnapshot, WorkspaceId};
use tracing::warn;

use crate::render;

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
    let tick = Duration::from_secs(config.tick_seconds.max(1)).min(Duration::from_secs(2));
    socket.set_read_timeout(Some(tick))?;

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    loop {
        if let Err(err) = write_heartbeat(&config, &socket_path) {
            warn!(error = %err, "sidebar heartbeat failed");
        }
        match fetch_snapshot(&config.workspace_id) {
            Ok(snapshot) => render::draw_to_terminal(&mut terminal, &snapshot)?,
            Err(err) => warn!(error = %err, "sidebar snapshot refresh failed"),
        }
        wait_for_wakeup(&socket)?;
    }
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
