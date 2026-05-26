//! Runtime loop for the native sidebar process.

use std::io;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use jiff::Timestamp;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event};
use rimz::feed::PaneRef;
use rimz::ids::PaneId;
use rimz::ledger::paths::PathErr;
use rimz::{MuxName, RuntimePaths, SidebarInstanceId, SidebarSnapshot, WorkspaceId};
use tracing::{debug, warn};

use crate::render::{self, FetchStatus};

#[derive(Clone, Debug)]
pub struct ServeConfig {
    pub workspace_id: WorkspaceId,
    pub mux: MuxName,
    pub session_name: String,
    pub instance_id: SidebarInstanceId,
    pub tick_seconds: u64,
    pub rimz_bin: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum SidebarAppErr {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Paths(#[from] PathErr),
    #[error("running `{program}`: {source}")]
    CommandIo {
        program: String,
        #[source]
        source: io::Error,
    },
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
    let _socket_cleanup = RuntimeFileGuard {
        path: socket_path.clone(),
    };
    // Drop the heartbeat on exit too — including the self-close below. A
    // lingering heartbeat stays mtime-fresh for `SIDEBAR_HEARTBEAT_TTL`, during
    // which `rimz`'s freshness gate would skip relaunch and let a plain
    // `attach` rebirth the session with no sidebar.
    let _heartbeat_cleanup = RuntimeFileGuard {
        path: runtime.sidebar_heartbeat_path(&config.instance_id),
    };
    let tick = tick_for(config.tick_seconds);
    socket.set_read_timeout(Some(tick))?;

    // Redraw the instant the pane is resized — most importantly when a user
    // attaches to a background session and Zellij sizes the pane for the first
    // time. The watcher nudges this loop through the same wakeup socket the
    // ledger uses, so a resize is just another wakeup; without it the first
    // usable frame waits for the next `tick`, reading as a blank sidebar.
    spawn_resize_waker(socket_path.clone());

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut last_snapshot: Option<SidebarSnapshot> = None;
    let mut status = FetchStatus::Ok;
    let mut self_close = SelfCloseState::default();

    loop {
        let heartbeat_outcome = write_heartbeat(&config, &socket_path);
        let snapshot_outcome = fetch_snapshot(&config.rimz_bin, &config.workspace_id);

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

        if self_close_decision(&mut self_close, own_view_sibling_count(&config)) {
            debug!(
                session = %config.session_name,
                "sidebar tab emptied; exiting so the pane closes itself",
            );
            break;
        }
        wait_for_wakeup(&socket)?;
    }
    Ok(())
}

/// Decide whether the sidebar should exit so its own pane closes. The sidebar
/// shares a tab/view with the user's working pane(s); when the last of them
/// exits, the sidebar is alone and has no reason to stay.
///
/// Startup gets one empty observation before close: during session birth the
/// sidebar can run before Zellij materializes the terminal sibling, but a tab
/// born permanently sidebar-only must still clean itself up.
///
/// `sibling_count` is `None` when the count could not be determined (no mux
/// pane env var, a failed `pane list`, or our own pane missing from the list);
/// in that case we never close.
fn self_close_decision(state: &mut SelfCloseState, sibling_count: Option<usize>) -> bool {
    state.should_close(sibling_count)
}

#[derive(Debug, Default)]
struct SelfCloseState {
    seen_sibling: bool,
    empty_startup_observations: u8,
}

impl SelfCloseState {
    fn should_close(&mut self, sibling_count: Option<usize>) -> bool {
        match sibling_count {
            Some(0) if self.seen_sibling => true,
            Some(0) => {
                self.empty_startup_observations = self.empty_startup_observations.saturating_add(1);
                self.empty_startup_observations >= EMPTY_STARTUP_OBSERVATIONS_BEFORE_CLOSE
            }
            Some(_) => {
                self.seen_sibling = true;
                self.empty_startup_observations = 0;
                false
            }
            None => false,
        }
    }
}

const EMPTY_STARTUP_OBSERVATIONS_BEFORE_CLOSE: u8 = 2;

/// Count the panes that share this sidebar's view (tab/window) but are not the
/// sidebar itself. Best-effort and backend-agnostic: it shells out to the
/// normalized `rimz pane list`, the same read-only discovery primitive a
/// resolver uses. Returns `None` on any failure so the caller never
/// self-closes on bad data.
fn own_view_sibling_count(config: &ServeConfig) -> Option<usize> {
    let own = own_pane_id(config.mux)?;
    let output = Command::new(&config.rimz_bin)
        .args(["pane", "list", "--json", "--session-name"])
        .arg(&config.session_name)
        .output()
        .ok()?;
    if !output.status.success() {
        debug!(
            stderr = %String::from_utf8_lossy(&output.stderr),
            "sidebar pane-list probe failed; staying open",
        );
        return None;
    }
    let panes: Vec<PaneRef> = serde_json::from_slice(&output.stdout).ok()?;
    let own_view = panes
        .iter()
        .find(|pane| pane.pane_id == own)?
        .view_id
        .clone();
    Some(
        panes
            .iter()
            .filter(|pane| pane.pane_id != own && pane.view_id == own_view)
            .count(),
    )
}

/// This process's normalized pane id, read from the multiplexer's per-pane env
/// var. Zellij exposes a bare integer in `ZELLIJ_PANE_ID` (normalized as
/// `terminal_<id>`); tmux exposes the full raw id in `TMUX_PANE`.
fn own_pane_id(mux: MuxName) -> Option<PaneId> {
    let raw = match mux {
        MuxName::Zellij => format!("terminal_{}", std::env::var("ZELLIJ_PANE_ID").ok()?),
        MuxName::Tmux => std::env::var("TMUX_PANE").ok()?,
    };
    Some(PaneId::from_parts(mux, raw))
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

fn fetch_snapshot(rimz_bin: &Path, workspace_id: &WorkspaceId) -> Result<SidebarSnapshot> {
    let output = Command::new(rimz_bin)
        .args(["sidebar", "snapshot", "--workspace-id"])
        .arg(workspace_id.as_str())
        .arg("--json")
        .output()
        .map_err(|source| SidebarAppErr::CommandIo {
            program: rimz_bin.display().to_string(),
            source,
        })?;
    if !output.status.success() {
        return Err(SidebarAppErr::SnapshotCommand {
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn write_heartbeat(config: &ServeConfig, socket_path: &Path) -> Result<()> {
    let output = Command::new(&config.rimz_bin)
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
        .output()
        .map_err(|source| SidebarAppErr::CommandIo {
            program: config.rimz_bin.display().to_string(),
            source,
        })?;
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

/// How long the resize watcher blocks per poll. A resize event wakes it
/// immediately regardless; this only bounds how often it loops while idle.
const RESIZE_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Watch the terminal for resize events and wake the serve loop so it repaints
/// at the new size. Runs on its own thread for the life of the process; it
/// self-wakes by sending to `wake_path` (the loop's bound wakeup socket), which
/// keeps the loop's single recv-driven redraw path. Key and mouse events are
/// ignored — the sidebar takes no input. Stops quietly if the event source or
/// the socket goes away (e.g. the loop has exited).
fn spawn_resize_waker(wake_path: PathBuf) {
    std::thread::spawn(move || {
        let waker = match UnixDatagram::unbound() {
            Ok(socket) => socket,
            Err(err) => {
                warn!(error = %err, "resize waker disabled; resizes wait for the tick");
                return;
            }
        };
        loop {
            match event::poll(RESIZE_POLL_INTERVAL) {
                Ok(true) => match event::read() {
                    Ok(Event::Resize(_, _)) => {
                        if waker.send_to(b"resize", &wake_path).is_err() {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(err) => {
                        warn!(error = %err, "resize waker stopping: event read failed");
                        return;
                    }
                },
                Ok(false) => {}
                Err(err) => {
                    warn!(error = %err, "resize waker stopping: event poll failed");
                    return;
                }
            }
        }
    });
}

fn wait_for_wakeup(socket: &UnixDatagram) -> io::Result<()> {
    let mut buf = [0_u8; 4096];
    match socket.recv(&mut buf) {
        Ok(_) => Ok(()),
        // Timeout (the tick), or a signal (the resize watcher's SIGWINCH handler
        // interrupts this blocking recv): all are just "redraw now", never fatal.
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(())
        }
        Err(err) => Err(err),
    }
}

/// Removes a per-instance runtime file (wakeup socket, heartbeat) when the
/// sidebar exits, so a later `rimz` launch sees an honest "no sidebar here" and
/// rebirths one rather than trusting a stale artifact.
struct RuntimeFileGuard {
    path: PathBuf,
}

impl Drop for RuntimeFileGuard {
    fn drop(&mut self) {
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                warn!(path = %self.path.display(), error = %err, "sidebar runtime file cleanup failed")
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

    #[test]
    fn self_close_waits_for_a_sibling_before_ever_closing() {
        let mut state = SelfCloseState::default();
        // Startup: no sibling yet (terminal pane not materialized). Give Zellij
        // one observation to finish materializing the sibling.
        assert!(!self_close_decision(&mut state, Some(0)));
        assert!(!state.seen_sibling);
    }

    #[test]
    fn self_close_fires_when_a_sibling_never_appears() {
        let mut state = SelfCloseState::default();
        assert!(!self_close_decision(&mut state, Some(0)));
        assert!(self_close_decision(&mut state, Some(0)));
    }

    #[test]
    fn self_close_latches_then_fires_when_alone() {
        let mut state = SelfCloseState::default();
        assert!(!self_close_decision(&mut state, Some(1)));
        assert!(state.seen_sibling, "seeing a sibling must latch");
        // Sibling went away: now alone, so close.
        assert!(self_close_decision(&mut state, Some(0)));
    }

    #[test]
    fn self_close_holds_while_siblings_remain() {
        let mut state = SelfCloseState {
            seen_sibling: true,
            empty_startup_observations: 0,
        };
        assert!(!self_close_decision(&mut state, Some(2)));
    }

    #[test]
    fn self_close_never_fires_on_unknown_count() {
        let mut state = SelfCloseState {
            seen_sibling: true,
            empty_startup_observations: 0,
        };
        assert!(!self_close_decision(&mut state, None));
        assert!(
            state.seen_sibling,
            "an unknown count must not clear the latch"
        );
    }
}
