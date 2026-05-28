//! Runtime loop for the native sidebar process.

use std::io;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use jiff::Timestamp;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal;
use rimz::feed::PaneRef;
use rimz::ids::PaneId;
use rimz::ledger::paths::PathErr;
use rimz::{MuxName, RuntimePaths, SidebarInstanceId, SidebarSnapshot, WorkspaceId};
use tracing::{debug, warn};

use crate::render::{self, FetchStatus, UiState};

mod input;
use input::{KeyAction, Wakeup, encode_key, encode_mouse, wait_for_wakeup};

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
    let _input_mode = InputModeGuard::enable()?;
    spawn_event_waker(socket_path.clone());
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut last_snapshot: Option<SidebarSnapshot> = None;
    let mut status = FetchStatus::Ok;
    let mut self_close = SelfCloseState::default();
    let mut ui = UiState::default();

    loop {
        let heartbeat_outcome = write_heartbeat(&config, &socket_path);
        let snapshot_outcome = fetch_snapshot_for(
            &config.rimz_bin,
            &config.workspace_id,
            Some(config.mux),
            Some(&config.session_name),
            own_pane_id(config.mux),
        );

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
        let own_view = own_view_state(&config);
        clamp_selection(&mut ui, &state.snapshot);
        sync_selection_to_focused_pane(
            &mut ui,
            &state.snapshot,
            own_view
                .as_ref()
                .filter(|state| !state.own_is_focused)
                .and_then(|state| state.focused_pane_id.as_ref()),
        );
        last_snapshot = state.last_snapshot;
        status = state.status;
        render::draw_to_terminal_with_ui(&mut terminal, &state.snapshot, &status, &ui)?;

        if self_close_decision(
            &mut self_close,
            own_view.as_ref().map(|state| state.sibling_count),
        ) {
            debug!(
                session = %config.session_name,
                "sidebar tab emptied; exiting so the pane closes itself",
            );
            break;
        }
        let wakeup = wait_for_wakeup(&socket)?;
        let outcome = handle_wakeup(wakeup, &mut ui, &state.snapshot, &status);
        if outcome.redraw {
            render::draw_to_terminal_with_ui(&mut terminal, &state.snapshot, &status, &ui)?;
        }
        if let Some(index) = outcome.focus_index {
            focus_selected_row(&state.snapshot, index, &config);
        }
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct OwnViewState {
    sibling_count: usize,
    own_is_focused: bool,
    focused_pane_id: Option<PaneId>,
}

/// Summarize the panes that share this sidebar's view (tab/window). Best-effort
/// and backend-agnostic: it shells out to the normalized `rimz pane list`, the
/// same read-only discovery primitive a resolver uses. Returns `None` on any
/// failure so the caller never self-closes or moves selection on bad data.
fn own_view_state(config: &ServeConfig) -> Option<OwnViewState> {
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
    own_view_state_from_panes(&own, &panes, own_view.as_deref())
}

fn own_view_state_from_panes(
    own: &PaneId,
    panes: &[PaneRef],
    own_view: Option<&str>,
) -> Option<OwnViewState> {
    if !panes.iter().any(|pane| pane.pane_id == *own) {
        return None;
    }
    let siblings = panes
        .iter()
        .filter(|pane| pane.pane_id != *own && pane.view_id.as_deref() == own_view)
        .collect::<Vec<_>>();
    let own_is_focused = panes
        .iter()
        .find(|pane| pane.pane_id == *own)
        .is_some_and(|pane| pane.is_focused);
    let focused_pane_id = siblings
        .iter()
        .find(|pane| pane.is_focused)
        .map(|pane| pane.pane_id.clone());
    Some(OwnViewState {
        sibling_count: siblings.len(),
        own_is_focused,
        focused_pane_id,
    })
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
    let display_name = workspace_id.as_str().to_owned();
    SidebarSnapshot {
        workspace_id,
        display_name,
        generated_at: Timestamp::now(),
        worktree_groups: Vec::new(),
        needs_attention: Vec::new(),
        resolver_working: Vec::new(),
        recently_answered: Vec::new(),
        recent_activity: Vec::new(),
        agents: Vec::new(),
        agent_hooks_ready: false,
    }
}

/// Bundle returned by [`compute_next_state`]; the loop applies it verbatim.
#[derive(Clone, Debug)]
pub struct RenderState {
    pub snapshot: SidebarSnapshot,
    pub status: FetchStatus,
    pub last_snapshot: Option<SidebarSnapshot>,
}

fn fetch_snapshot_for(
    rimz_bin: &Path,
    workspace_id: &WorkspaceId,
    mux: Option<MuxName>,
    session_name: Option<&str>,
    exclude_pane_id: Option<PaneId>,
) -> Result<SidebarSnapshot> {
    let mut command = Command::new(rimz_bin);
    command
        .args(["sidebar", "snapshot", "--workspace-id"])
        .arg(workspace_id.as_str());
    if let Some(mux) = mux {
        command.args(["--mux", mux.as_str()]);
    }
    if let Some(session_name) = session_name {
        command.args(["--session-name", session_name]);
    }
    if let Some(pane_id) = exclude_pane_id {
        command.args(["--exclude-pane-id", pane_id.as_str()]);
    }
    command.arg("--json");
    let output = command
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

/// Watch the terminal for resize and key events and wake the serve loop. Runs
/// on its own thread for the life of the process; it self-wakes by sending to
/// `wake_path` (the loop's bound wakeup socket), which keeps redraw and input
/// on one path. Stops quietly if the event source or socket goes away.
fn spawn_event_waker(wake_path: PathBuf) {
    std::thread::spawn(move || {
        let waker = match UnixDatagram::unbound() {
            Ok(socket) => socket,
            Err(err) => {
                warn!(error = %err, "event waker disabled; input waits for the tick");
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
                    Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                        if let Some(encoded) = encode_key(key.code)
                            && waker.send_to(encoded.as_bytes(), &wake_path).is_err()
                        {
                            return;
                        }
                    }
                    Ok(Event::Mouse(mouse)) => {
                        if let Some(encoded) = encode_mouse(mouse.kind, mouse.column, mouse.row)
                            && waker.send_to(encoded.as_bytes(), &wake_path).is_err()
                        {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(err) => {
                        warn!(error = %err, "event waker stopping: event read failed");
                        return;
                    }
                },
                Ok(false) => {}
                Err(err) => {
                    warn!(error = %err, "event waker stopping: event poll failed");
                    return;
                }
            }
        }
    });
}

fn handle_wakeup(
    wakeup: Wakeup,
    ui: &mut UiState,
    snapshot: &SidebarSnapshot,
    status: &FetchStatus,
) -> InputOutcome {
    match wakeup {
        Wakeup::Key(action) => handle_key(action, ui, snapshot),
        Wakeup::MouseClick { column, row } => handle_mouse_click(column, row, ui, snapshot, status),
        Wakeup::Resize => InputOutcome::redraw(),
        Wakeup::Tick => InputOutcome::default(),
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct InputOutcome {
    redraw: bool,
    focus_index: Option<usize>,
}

impl InputOutcome {
    fn redraw() -> Self {
        Self {
            redraw: true,
            focus_index: None,
        }
    }

    fn focus(index: usize) -> Self {
        Self {
            redraw: true,
            focus_index: Some(index),
        }
    }
}

fn handle_key(action: KeyAction, ui: &mut UiState, snapshot: &SidebarSnapshot) -> InputOutcome {
    match action {
        KeyAction::Up => {
            if ui.selected_index > 0 {
                ui.selected_index -= 1;
                return InputOutcome::redraw();
            }
            InputOutcome::default()
        }
        KeyAction::Down => {
            let len = visible_row_count(snapshot);
            if ui.selected_index + 1 < len {
                ui.selected_index += 1;
                return InputOutcome::redraw();
            }
            InputOutcome::default()
        }
        KeyAction::Enter => InputOutcome::focus(ui.selected_index),
        KeyAction::Space => {
            if let Some(index) = next_attention_index(snapshot, ui.selected_index) {
                ui.selected_index = index;
                return InputOutcome::focus(ui.selected_index);
            }
            InputOutcome::default()
        }
        KeyAction::Help => {
            ui.help_visible = !ui.help_visible;
            InputOutcome::redraw()
        }
        KeyAction::Digit(digit) => {
            let index = usize::from(digit.saturating_sub(1));
            if index < visible_row_count(snapshot) {
                ui.selected_index = index;
                return InputOutcome::focus(ui.selected_index);
            }
            InputOutcome::default()
        }
    }
}

fn handle_mouse_click(
    column: u16,
    row: u16,
    ui: &mut UiState,
    snapshot: &SidebarSnapshot,
    status: &FetchStatus,
) -> InputOutcome {
    if let Some(index) = row_index_at_screen_position(snapshot, status, column, row) {
        ui.selected_index = index;
        return InputOutcome::focus(ui.selected_index);
    }
    InputOutcome::default()
}

fn clamp_selection(ui: &mut UiState, snapshot: &SidebarSnapshot) {
    let len = visible_row_count(snapshot);
    if len == 0 {
        ui.selected_index = 0;
    } else if ui.selected_index >= len {
        ui.selected_index = len - 1;
    }
}

fn sync_selection_to_focused_pane(
    ui: &mut UiState,
    snapshot: &SidebarSnapshot,
    focused_pane_id: Option<&PaneId>,
) {
    let Some(focused_pane_id) = focused_pane_id else {
        return;
    };
    if let Some(index) = visible_rows(snapshot).position(|row| {
        row.pane
            .as_ref()
            .is_some_and(|pane| pane.pane_id == *focused_pane_id)
    }) {
        ui.selected_index = index;
    }
}

fn row_index_at_screen_position(
    snapshot: &SidebarSnapshot,
    status: &FetchStatus,
    column: u16,
    row: u16,
) -> Option<usize> {
    // The block border occupies row 0 and column 0. Ratatui renders the
    // snapshot body one cell in from the top-left border.
    if row == 0 || column == 0 {
        return None;
    }
    let target = usize::from(row - 1);
    row_index_at_content_line(snapshot, status, target)
}

fn row_index_at_content_line(
    snapshot: &SidebarSnapshot,
    status: &FetchStatus,
    target: usize,
) -> Option<usize> {
    let mut line = 0_usize;
    let mut last_nonempty = false;

    if matches!(status, FetchStatus::Degraded { .. }) {
        if target == line {
            return None;
        }
        line += 1;
        if target == line {
            return None;
        }
        line += 1;
        last_nonempty = false;
    }

    if has_attention_line(snapshot) {
        if target == line {
            return None;
        }
        line += 1;
        last_nonempty = true;
    }

    if snapshot.worktree_groups.is_empty() {
        return None;
    }

    if last_nonempty {
        if target == line {
            return None;
        }
        line += 1;
    }

    let mut row_index = 0_usize;
    for (group_index, group) in snapshot.worktree_groups.iter().enumerate() {
        if group_index > 0 {
            if target == line {
                return None;
            }
            line += 1;
        }

        if target == line {
            return None;
        }
        line += 1;

        for row in &group.rows {
            if target == line {
                return Some(row_index);
            }
            line += 1;

            if row.row_kind == rimz::SidebarRowKind::Agent && row_has_capability_line(row) {
                if target == line {
                    return Some(row_index);
                }
                line += 1;
            }
            row_index += 1;
        }

        if group.hidden_count > 0 {
            if target == line {
                return None;
            }
            line += 1;
        }
    }
    None
}

fn has_attention_line(snapshot: &SidebarSnapshot) -> bool {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.status_counts)
        .any(|count| {
            count.count > 0
                && matches!(
                    count.status,
                    rimz::feed::AgentStatus::Waiting | rimz::feed::AgentStatus::Failed
                )
        })
}

fn row_has_capability_line(row: &rimz::SidebarRow) -> bool {
    row.model.as_deref().is_some_and(|value| !value.is_empty())
        || row.effort.as_deref().is_some_and(|value| !value.is_empty())
        || matches!(
            row.mode,
            Some(
                rimz::feed::AgentMode::Plan
                    | rimz::feed::AgentMode::Auto
                    | rimz::feed::AgentMode::Bypass
            )
        )
}

fn visible_row_count(snapshot: &SidebarSnapshot) -> usize {
    snapshot
        .worktree_groups
        .iter()
        .map(|group| group.rows.len())
        .sum()
}

fn visible_rows(snapshot: &SidebarSnapshot) -> impl Iterator<Item = &rimz::SidebarRow> {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
}

fn next_attention_index(snapshot: &SidebarSnapshot, selected: usize) -> Option<usize> {
    let rows = visible_rows(snapshot).collect::<Vec<_>>();
    if rows.is_empty() {
        return None;
    }
    let start = selected.saturating_add(1);
    (0..rows.len()).find_map(|offset| {
        let index = (start + offset) % rows.len();
        matches!(
            rows[index].status,
            Some(rimz::feed::AgentStatus::Waiting | rimz::feed::AgentStatus::Failed)
        )
        .then_some(index)
    })
}

fn focus_selected_row(snapshot: &SidebarSnapshot, selected: usize, config: &ServeConfig) {
    let Some(row) = visible_rows(snapshot).nth(selected) else {
        return;
    };
    let Some(pane) = &row.pane else {
        return;
    };
    let mut command = Command::new(&config.rimz_bin);
    command.args(["pane", "focus", pane.pane_id.as_str(), "--session-name"]);
    command.arg(&pane.session_name);
    if let Some(start) = pane.pane_process_start {
        command.arg("--pane-process-start").arg(start.to_string());
    }
    match command.output() {
        Ok(output) if output.status.success() => {}
        Ok(output) => warn!(
            pane = %pane.pane_id,
            stderr = %String::from_utf8_lossy(&output.stderr),
            "sidebar pane focus failed",
        ),
        Err(err) => warn!(
            pane = %pane.pane_id,
            error = %err,
            "sidebar pane focus command failed",
        ),
    }
}

struct InputModeGuard;

impl InputModeGuard {
    fn enable() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        if let Err(err) = execute!(io::stdout(), EnableMouseCapture) {
            let _ = terminal::disable_raw_mode();
            return Err(err);
        }
        Ok(Self)
    }
}

impl Drop for InputModeGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableMouseCapture);
        let _ = terminal::disable_raw_mode();
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

    fn pane(raw: &str, view: &str, focused: bool) -> PaneRef {
        PaneRef {
            pane_id: PaneId::from_parts(MuxName::Zellij, raw),
            session_name: "rimz-test".to_owned(),
            view_id: Some(view.to_owned()),
            view_kind: Some(rimz::ids::ViewKind::Tab),
            is_focused: focused,
            command: Some("zsh".to_owned()),
            cwd: Some("/repo/main".to_owned()),
            pane_pid: None,
            pane_process_start: None,
        }
    }

    fn snapshot_with_panes(ws: &WorkspaceId, panes: Vec<PaneRef>) -> SidebarSnapshot {
        let mut snapshot = snapshot(ws);
        snapshot.worktree_groups = vec![rimz::SidebarWorktreeGroup {
            key: "/repo/main".to_owned(),
            label: "main".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: panes
                .into_iter()
                .map(|pane| rimz::SidebarRow {
                    row_kind: rimz::SidebarRowKind::Process,
                    id: pane.pane_id.to_string(),
                    name: pane.command.clone().unwrap_or_else(|| "process".to_owned()),
                    status: None,
                    mode: None,
                    pane: Some(pane),
                    request_id: None,
                    surface: None,
                    task: None,
                    model: None,
                    effort: None,
                    worktree_path: Some("/repo/main".to_owned()),
                    worktree_branch: Some("main".to_owned()),
                    last_activity: Timestamp::now(),
                    resolver: None,
                    options: Vec::new(),
                })
                .collect(),
            hidden_count: 0,
        }];
        snapshot
    }

    fn agent_snapshot(ws: &WorkspaceId) -> SidebarSnapshot {
        let mut snapshot = snapshot(ws);
        let row = rimz::SidebarRow {
            row_kind: rimz::SidebarRowKind::Agent,
            id: "agent-1".to_owned(),
            name: "claude".to_owned(),
            status: Some(rimz::feed::AgentStatus::Idle),
            mode: Some(rimz::feed::AgentMode::Plan),
            pane: Some(pane("terminal_9", "tab_0", false)),
            request_id: None,
            surface: None,
            task: Some("inspect auth".to_owned()),
            model: Some("Opus".to_owned()),
            effort: None,
            worktree_path: Some("/repo/main".to_owned()),
            worktree_branch: Some("main".to_owned()),
            last_activity: Timestamp::now(),
            resolver: None,
            options: Vec::new(),
        };
        snapshot.worktree_groups = vec![rimz::SidebarWorktreeGroup {
            key: "/repo/main".to_owned(),
            label: "main".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: vec![rimz::SidebarStatusCount {
                status: rimz::feed::AgentStatus::Idle,
                count: 1,
            }],
            rows: vec![row],
            hidden_count: 0,
        }];
        snapshot
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

    #[test]
    fn own_view_state_tracks_focused_sibling_in_own_view() {
        let own = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let focused_here = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let focused_elsewhere = PaneId::from_parts(MuxName::Zellij, "terminal_3");
        let panes = vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", true),
            pane("terminal_3", "tab_1", true),
        ];

        let state =
            own_view_state_from_panes(&own, &panes, Some("tab_0")).expect("own pane is present");

        assert_eq!(state.sibling_count, 1);
        assert_eq!(state.focused_pane_id, Some(focused_here));
        assert_ne!(state.focused_pane_id, Some(focused_elsewhere));
    }

    #[test]
    fn own_view_state_marks_when_sidebar_itself_is_focused() {
        let own = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let panes = vec![
            pane("terminal_1", "tab_0", true),
            pane("terminal_2", "tab_0", false),
        ];

        let state =
            own_view_state_from_panes(&own, &panes, Some("tab_0")).expect("own pane is present");

        assert!(state.own_is_focused);
        assert_eq!(state.focused_pane_id, None);
    }

    #[test]
    fn selection_syncs_to_focused_pane_row() {
        let ws = workspace();
        let focused = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", true),
            ],
        );
        let mut ui = UiState {
            selected_index: 0,
            help_visible: false,
        };

        sync_selection_to_focused_pane(&mut ui, &snapshot, Some(&focused));

        assert_eq!(ui.selected_index, 1);
    }

    #[test]
    fn selection_stays_put_when_focus_is_unknown() {
        let ws = workspace();
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", false),
            ],
        );
        let mut ui = UiState {
            selected_index: 1,
            help_visible: false,
        };

        sync_selection_to_focused_pane(&mut ui, &snapshot, None);

        assert_eq!(ui.selected_index, 1);
    }

    #[test]
    fn row_index_maps_process_row_screen_positions() {
        let ws = workspace();
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", false),
            ],
        );

        assert_eq!(
            row_index_at_screen_position(&snapshot, &FetchStatus::Ok, 1, 1),
            None,
            "the group header is not a row"
        );
        assert_eq!(
            row_index_at_screen_position(&snapshot, &FetchStatus::Ok, 0, 2),
            None,
            "the border is not clickable content"
        );
        assert_eq!(
            row_index_at_screen_position(&snapshot, &FetchStatus::Ok, 1, 2),
            Some(0)
        );
        assert_eq!(
            row_index_at_screen_position(&snapshot, &FetchStatus::Ok, 1, 3),
            Some(1)
        );
    }

    #[test]
    fn row_index_maps_agent_capability_line_to_same_row() {
        let ws = workspace();
        let snapshot = agent_snapshot(&ws);

        assert_eq!(
            row_index_at_screen_position(&snapshot, &FetchStatus::Ok, 1, 2),
            Some(0)
        );
        assert_eq!(
            row_index_at_screen_position(&snapshot, &FetchStatus::Ok, 1, 3),
            Some(0),
            "clicking an agent capability line routes to that agent row"
        );
    }

    #[test]
    fn mouse_click_selects_clicked_row() {
        let ws = workspace();
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", false),
            ],
        );
        let mut ui = UiState {
            selected_index: 0,
            help_visible: false,
        };

        let outcome = handle_mouse_click(1, 3, &mut ui, &snapshot, &FetchStatus::Ok);

        assert_eq!(outcome, InputOutcome::focus(1));
        assert_eq!(ui.selected_index, 1);
    }

    #[test]
    fn arrow_key_reports_immediate_ui_change() {
        let ws = workspace();
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", false),
            ],
        );
        let mut ui = UiState {
            selected_index: 0,
            help_visible: false,
        };

        let outcome = handle_key(KeyAction::Down, &mut ui, &snapshot);

        assert_eq!(outcome, InputOutcome::redraw());
        assert_eq!(ui.selected_index, 1);
    }

    #[test]
    fn enter_reports_focus_after_highlight_redraw() {
        let ws = workspace();
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", false),
            ],
        );
        let mut ui = UiState {
            selected_index: 1,
            help_visible: false,
        };

        let outcome = handle_key(KeyAction::Enter, &mut ui, &snapshot);

        assert_eq!(outcome, InputOutcome::focus(1));
        assert_eq!(ui.selected_index, 1);
    }
}
