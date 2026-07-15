//! Managed `rimzd` view specification, classification, and repair.
//!
//! The view is `sidebar | content | runtime`: content supervisors occupy the
//! middle column, while the per-session Codex broker, Claude remote-control
//! host, loop panel, and transient loop runs share the right column. Managed
//! panes carry their launch command as mux identity so repair remains stable
//! when a foreground child replaces the original command.
//!
//! Repair plans one placement from pane-listing truth, spawns it, waits for its
//! marker to settle, and re-lists before planning the next placement. This
//! makes every newly restored pane available as the next spec-order anchor and
//! preserves one runtime column when several panes disappear together.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::DaemonConfig;
use crate::ids::{PaneId, WorkspaceId};
use crate::mux::{
    DaemonView, HostPane, MuxBackend, PaneListOptions, PaneListing, SplitDirection,
    SplitPaneOptions,
};
use crate::pane::PaneRef;
use crate::store::parse_cache::StampedPath;
use crate::store::{paths::StatePaths, workspace_record};

/// View name for the managed daemon tab. Shared by the launcher and pane
/// classifiers so every backend speaks the same name.
pub const VIEW_NAME: &str = "rimzd";

const REPAIR_LIST_TIMEOUT: Duration = Duration::from_secs(3);
const SETTLE_ATTEMPTS: usize = 5;
const SETTLE_POLL: Duration = Duration::from_millis(100);

/// Inputs that determine the managed panes in one workspace's daemon view.
pub struct DaemonViewSpecParams<'a> {
    pub claude_host_argv: Option<&'a [String]>,
    pub daemon: &'a DaemonConfig,
    pub rimz_bin: &'a Path,
    pub workspace_id: &'a WorkspaceId,
    pub session_name: &'a str,
    pub project_root: &'a Path,
    pub worktree_root: &'a Path,
    pub codex_present: bool,
}

/// Build the authoritative managed-pane specification for the `rimzd` view.
pub fn daemon_view_spec(params: DaemonViewSpecParams<'_>) -> DaemonView {
    DaemonView {
        name: VIEW_NAME.to_owned(),
        content: content_panes(params.daemon, params.rimz_bin, params.worktree_root),
        hosts: daemon_hosts(&params),
        loop_panel: loop_panel(params.rimz_bin, params.worktree_root),
    }
}

fn daemon_hosts(params: &DaemonViewSpecParams<'_>) -> Vec<HostPane> {
    let mut hosts = Vec::new();
    if params.codex_present {
        hosts.push(HostPane {
            argv: vec![
                params.rimz_bin.to_string_lossy().into_owned(),
                "codex".to_owned(),
                "app-server".to_owned(),
                "serve".to_owned(),
                "--workspace-id".to_owned(),
                params.workspace_id.as_str().to_owned(),
                "--session-name".to_owned(),
                params.session_name.to_owned(),
            ],
            cwd: params.worktree_root.to_path_buf(),
        });
    }
    if let Some(argv) = params.claude_host_argv {
        hosts.push(HostPane {
            argv: argv.to_vec(),
            cwd: params.project_root.to_path_buf(),
        });
    }
    hosts
}

fn loop_panel(rimz_bin: &Path, worktree_root: &Path) -> HostPane {
    HostPane {
        argv: vec![
            rimz_bin.to_string_lossy().into_owned(),
            "loop".to_owned(),
            "watch".to_owned(),
            "--hold".to_owned(),
        ],
        cwd: worktree_root.to_path_buf(),
    }
}

fn content_panes(daemon: &DaemonConfig, rimz_bin: &Path, worktree_root: &Path) -> Vec<HostPane> {
    (0..crate::daemon_content::resolve_content(daemon, rimz_bin, worktree_root).len())
        .map(|slot| content_supervisor_pane(slot, rimz_bin, worktree_root))
        .collect()
}

fn content_supervisor_pane(slot: usize, rimz_bin: &Path, worktree_root: &Path) -> HostPane {
    HostPane {
        argv: vec![
            rimz_bin.to_string_lossy().into_owned(),
            "daemon".to_owned(),
            "content".to_owned(),
            "--slot".to_owned(),
            slot.to_string(),
            "--worktree-root".to_owned(),
            worktree_root.to_string_lossy().into_owned(),
        ],
        cwd: worktree_root.to_path_buf(),
    }
}

/// Substring marking the Claude remote-control host in a pane command.
pub(crate) const COMMAND_MARKER: &str = "remote-control";

/// Substring marking the per-session Codex app-server broker.
pub(crate) const APP_SERVER_MARKER: &str = "app-server";

/// Substring marking the always-present loop panel command.
pub(crate) const LOOP_PANEL_MARKER: &str = "loop watch";

/// Whether a command line is one of RimZ's managed daemon hosts.
pub fn command_is_host(command: &str) -> bool {
    command.contains(COMMAND_MARKER) || command.contains(APP_SERVER_MARKER)
}

/// Return the oldest loop panel in the managed view.
pub fn find_loop_panel(panes: &[PaneRef]) -> Option<&PaneRef> {
    oldest_matching_managed_pane(panes, &ManagedPaneMarker::LoopPanel)
}

/// A structural column in the managed daemon view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DaemonColumn {
    Content,
    Runtime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ManagedPaneMarker {
    ContentSlot(usize),
    CodexAppServer,
    ClaudeRemoteControl,
    LoopPanel,
}

impl ManagedPaneMarker {
    fn column(&self) -> DaemonColumn {
        match self {
            Self::ContentSlot(_) => DaemonColumn::Content,
            Self::CodexAppServer | Self::ClaudeRemoteControl | Self::LoopPanel => {
                DaemonColumn::Runtime
            }
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ManagedPaneReconciliation {
    spawn: Vec<HostPane>,
    close: Vec<PaneId>,
}

#[derive(Debug, PartialEq, Eq)]
enum RepairStep {
    Spawn {
        pane: HostPane,
        anchor_pane_id: PaneId,
        direction: SplitDirection,
    },
    Close(Vec<PaneId>),
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepairOutcome {
    Converged,
    Retry,
}

/// Plan the next daemon-view mutation from one authoritative pane listing.
fn next_repair_step(panes: &[PaneRef], view: &DaemonView) -> RepairStep {
    if !panes
        .iter()
        .any(|pane| pane.view_name.as_deref() == Some(VIEW_NAME))
    {
        return RepairStep::Done;
    }
    let reconciliation = managed_pane_reconciliation(view, panes);
    if !reconciliation.close.is_empty() {
        return RepairStep::Close(reconciliation.close);
    }
    next_spawn_step(panes, view, reconciliation.spawn.first())
}

fn next_spawn_step(panes: &[PaneRef], view: &DaemonView, missing: Option<&HostPane>) -> RepairStep {
    let Some(pane) = missing else {
        return RepairStep::Done;
    };
    let Some(marker) = host_marker(pane) else {
        return RepairStep::Done;
    };
    let Some((anchor_pane_id, direction)) = spawn_anchor(panes, view, &marker) else {
        return RepairStep::Done;
    };
    RepairStep::Spawn {
        pane: pane.clone(),
        anchor_pane_id,
        direction,
    }
}

/// Recreate every missing managed pane while any pane in `rimzd` survives.
/// Closing the whole view leaves no anchor and is treated as deliberate.
pub fn repair_daemon_view(
    backend: &dyn MuxBackend,
    session_name: &str,
    workspace_id: &WorkspaceId,
    view: &DaemonView,
) -> RepairOutcome {
    let Some(mut listing) = list_daemon_panes(backend, session_name, workspace_id) else {
        return RepairOutcome::Retry;
    };
    if next_repair_step(&listing.panes, view) == RepairStep::Done {
        return RepairOutcome::Converged;
    }
    let reconciliation = managed_pane_reconciliation(view, &listing.panes);
    let missing_count = reconciliation.spawn.len();

    if let RepairStep::Close(pane_ids) = next_repair_step(&listing.panes, view) {
        if !close_surplus_panes(backend, session_name, &pane_ids) {
            return RepairOutcome::Retry;
        }
        listing
            .panes
            .retain(|pane| !pane_ids.contains(&pane.pane_id));
    }

    for _ in 0..missing_count {
        let reconciliation = managed_pane_reconciliation(view, &listing.panes);
        let RepairStep::Spawn {
            pane,
            anchor_pane_id,
            direction,
        } = next_spawn_step(&listing.panes, view, reconciliation.spawn.first())
        else {
            return RepairOutcome::Retry;
        };
        let Some(marker) = host_marker(&pane) else {
            return RepairOutcome::Retry;
        };
        if !split_managed_pane(
            backend,
            session_name,
            &listing.panes,
            &pane,
            &anchor_pane_id,
            direction,
        ) {
            return RepairOutcome::Retry;
        }
        let Some(settled) = settle_managed_pane(backend, session_name, workspace_id, &marker)
        else {
            tracing::debug!(
                session = %session_name,
                argv = ?pane.argv,
                "daemon view repair stopped; spawned pane did not settle",
            );
            return RepairOutcome::Retry;
        };
        listing = settled;
    }
    if next_repair_step(&listing.panes, view) == RepairStep::Done {
        RepairOutcome::Converged
    } else {
        RepairOutcome::Retry
    }
}

/// Ensure the loop panel through the same placement and settle path used by a
/// full daemon-view repair, then return the oldest settled match.
pub fn ensure_loop_panel(
    backend: &dyn MuxBackend,
    session_name: &str,
    workspace_id: &WorkspaceId,
    view: &DaemonView,
) -> Option<PaneRef> {
    let listing = list_daemon_panes(backend, session_name, workspace_id)?;
    if let Some(panel) = find_loop_panel(&listing.panes) {
        return Some(panel.clone());
    }
    let marker = ManagedPaneMarker::LoopPanel;
    let (anchor_pane_id, direction) = spawn_anchor(&listing.panes, view, &marker)?;
    if !split_managed_pane(
        backend,
        session_name,
        &listing.panes,
        &view.loop_panel,
        &anchor_pane_id,
        direction,
    ) {
        return None;
    }
    let settled = settle_managed_pane(backend, session_name, workspace_id, &marker)?;
    find_loop_panel(&settled.panes).cloned()
}

fn list_daemon_panes(
    backend: &dyn MuxBackend,
    session_name: &str,
    workspace_id: &WorkspaceId,
) -> Option<PaneListing> {
    match backend.list_panes(PaneListOptions {
        session_name: Some(session_name.to_owned()),
        workspace_id: Some(workspace_id.clone()),
        command_timeout: Some(REPAIR_LIST_TIMEOUT),
        authoritative: true,
        require_authoritative: true,
        ..Default::default()
    }) {
        Ok(listing) => Some(listing),
        Err(err) => {
            tracing::debug!(
                session = %session_name,
                error = &err as &dyn std::error::Error,
                "daemon view repair skipped; pane listing failed",
            );
            None
        }
    }
}

fn close_surplus_panes(backend: &dyn MuxBackend, session_name: &str, pane_ids: &[PaneId]) -> bool {
    let mut converged = true;
    for pane_id in pane_ids {
        if let Err(err) = backend.close_pane(session_name, pane_id) {
            converged = false;
            tracing::warn!(
                session = %session_name,
                view = VIEW_NAME,
                pane = %pane_id,
                error = &err as &dyn std::error::Error,
                "daemon view repair could not close a surplus managed pane",
            );
        }
    }
    converged
}

fn split_managed_pane(
    backend: &dyn MuxBackend,
    session_name: &str,
    panes: &[PaneRef],
    pane: &HostPane,
    anchor_pane_id: &PaneId,
    direction: SplitDirection,
) -> bool {
    let Some(anchor) = panes
        .iter()
        .find(|candidate| &candidate.pane_id == anchor_pane_id)
    else {
        return false;
    };
    let title = pane.argv.join(" ");
    match backend.split_pane(SplitPaneOptions {
        session_name: Some(session_name.to_owned()),
        target_view_id: anchor.view_id.clone(),
        target_pane_id: Some(anchor.pane_id.clone()),
        cwd: Some(pane.cwd.to_string_lossy().into_owned()),
        command: Some(pane.argv.clone()),
        title: Some(title),
        env: Default::default(),
        stacked: false,
        direction,
        focus: false,
    }) {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!(
                session = %session_name,
                view = VIEW_NAME,
                argv = ?pane.argv,
                error = &err as &dyn std::error::Error,
                "daemon view repair could not recreate managed pane",
            );
            false
        }
    }
}

fn settle_managed_pane(
    backend: &dyn MuxBackend,
    session_name: &str,
    workspace_id: &WorkspaceId,
    marker: &ManagedPaneMarker,
) -> Option<PaneListing> {
    for attempt in 0..SETTLE_ATTEMPTS {
        let listing = list_daemon_panes(backend, session_name, workspace_id)?;
        if !matching_managed_panes(&listing.panes, marker).is_empty() {
            return Some(listing);
        }
        if attempt + 1 < SETTLE_ATTEMPTS {
            std::thread::sleep(SETTLE_POLL);
        }
    }
    None
}

fn spawn_anchor(
    panes: &[PaneRef],
    view: &DaemonView,
    marker: &ManagedPaneMarker,
) -> Option<(PaneId, SplitDirection)> {
    let daemon_panes = panes
        .iter()
        .filter(|pane| pane.view_name.as_deref() == Some(VIEW_NAME))
        .collect::<Vec<_>>();
    if daemon_panes.is_empty() {
        return None;
    }

    let column_markers = spec_markers(view)
        .into_iter()
        .filter(|candidate| candidate.column() == marker.column())
        .collect::<Vec<_>>();
    if let Some(index) = column_markers
        .iter()
        .position(|candidate| candidate == marker)
    {
        for candidate in column_markers[..index].iter().rev() {
            if let Some(anchor) = oldest_matching_managed_pane(panes, candidate) {
                return Some((anchor.pane_id.clone(), SplitDirection::Down));
            }
        }
    }
    for candidate in &column_markers {
        if let Some(anchor) = oldest_matching_managed_pane(panes, candidate) {
            return Some((anchor.pane_id.clone(), SplitDirection::Down));
        }
    }

    let anchor = match marker.column() {
        DaemonColumn::Content => daemon_panes
            .iter()
            .copied()
            .find(|pane| pane.is_rimz_sidebar()),
        DaemonColumn::Runtime => spec_markers(view)
            .into_iter()
            .filter(|candidate| candidate.column() == DaemonColumn::Content)
            .find_map(|candidate| oldest_matching_managed_pane(panes, &candidate))
            .or_else(|| {
                daemon_panes
                    .iter()
                    .copied()
                    .find(|pane| pane.is_rimz_sidebar())
            }),
    }
    .or_else(|| daemon_panes.first().copied())?;
    Some((anchor.pane_id.clone(), SplitDirection::Right))
}

fn spec_markers(view: &DaemonView) -> Vec<ManagedPaneMarker> {
    view.content
        .iter()
        .chain(view.hosts.iter())
        .chain(std::iter::once(&view.loop_panel))
        .filter_map(host_marker)
        .collect()
}

fn managed_pane_reconciliation(view: &DaemonView, panes: &[PaneRef]) -> ManagedPaneReconciliation {
    let managed = view
        .content
        .iter()
        .chain(view.hosts.iter())
        .chain(std::iter::once(&view.loop_panel))
        .filter_map(|host| host_marker(host).map(|marker| (marker, host)));
    let mut reconciliation = ManagedPaneReconciliation::default();
    let mut claude_enabled = false;
    for (marker, host) in managed {
        claude_enabled |= marker == ManagedPaneMarker::ClaudeRemoteControl;
        let mut matches = matching_managed_panes(panes, &marker);
        matches.sort_by(|left, right| {
            left.pane_id
                .creation_ordinal()
                .cmp(&right.pane_id.creation_ordinal())
                .then_with(|| left.pane_id.raw().cmp(right.pane_id.raw()))
        });
        if matches.is_empty() {
            reconciliation.spawn.push(host.clone());
        } else {
            reconciliation
                .close
                .extend(matches.into_iter().skip(1).map(|pane| pane.pane_id.clone()));
        }
    }
    if !claude_enabled {
        reconciliation.close.extend(
            matching_managed_panes(panes, &ManagedPaneMarker::ClaudeRemoteControl)
                .into_iter()
                .map(|pane| pane.pane_id.clone()),
        );
    }
    reconciliation
}

fn matching_managed_panes<'a>(
    panes: &'a [PaneRef],
    marker: &ManagedPaneMarker,
) -> Vec<&'a PaneRef> {
    panes
        .iter()
        .filter(|pane| pane.view_name.as_deref() == Some(VIEW_NAME))
        .filter(|pane| {
            [
                pane.spawn_command.as_deref(),
                pane.command.as_deref(),
                pane.title.as_deref(),
            ]
            .into_iter()
            .flatten()
            .any(|command| command_matches_marker(command, marker))
        })
        .collect()
}

fn oldest_matching_managed_pane<'a>(
    panes: &'a [PaneRef],
    marker: &ManagedPaneMarker,
) -> Option<&'a PaneRef> {
    matching_managed_panes(panes, marker)
        .into_iter()
        .min_by(|left, right| {
            left.pane_id
                .creation_ordinal()
                .cmp(&right.pane_id.creation_ordinal())
                .then_with(|| left.pane_id.raw().cmp(right.pane_id.raw()))
        })
}

fn host_marker(host: &HostPane) -> Option<ManagedPaneMarker> {
    command_marker(&host.argv.join(" "))
}

fn command_marker(command: &str) -> Option<ManagedPaneMarker> {
    content_slot_from_command(command)
        .map(ManagedPaneMarker::ContentSlot)
        .or_else(|| {
            if command.contains(APP_SERVER_MARKER) {
                Some(ManagedPaneMarker::CodexAppServer)
            } else if command_is_claude_host(command) {
                Some(ManagedPaneMarker::ClaudeRemoteControl)
            } else if command.contains(LOOP_PANEL_MARKER) {
                Some(ManagedPaneMarker::LoopPanel)
            } else {
                None
            }
        })
}

fn command_matches_marker(command: &str, marker: &ManagedPaneMarker) -> bool {
    match marker {
        ManagedPaneMarker::ContentSlot(slot) => content_slot_from_command(command) == Some(*slot),
        ManagedPaneMarker::CodexAppServer => command.contains(APP_SERVER_MARKER),
        ManagedPaneMarker::ClaudeRemoteControl => command_is_claude_host(command),
        ManagedPaneMarker::LoopPanel => command.contains(LOOP_PANEL_MARKER),
    }
}

pub(crate) fn command_is_claude_host(command: &str) -> bool {
    let mut tokens = command.split_whitespace();
    while let Some(token) = tokens.next() {
        let is_claude = Path::new(token)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "claude");
        if is_claude {
            return tokens.next() == Some(COMMAND_MARKER);
        }
    }
    false
}

fn content_slot_from_args(args: &[String]) -> Option<usize> {
    if !args
        .windows(2)
        .any(|pair| pair[0] == "daemon" && pair[1] == "content")
    {
        return None;
    }
    args.windows(2).find_map(|pair| {
        (pair[0] == "--slot")
            .then(|| pair[1].parse().ok())
            .flatten()
    })
}

fn content_slot_from_command(command: &str) -> Option<usize> {
    let args = command
        .split_whitespace()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    content_slot_from_args(&args)
}

/// Whether `pane` belongs to the daemon dashboard.
pub fn pane_is_host(pane: &PaneRef) -> bool {
    pane.spawn_command.as_deref().is_some_and(command_is_host)
        || pane.command.as_deref().is_some_and(command_is_host)
        || pane.view_name.as_deref() == Some(VIEW_NAME)
}

/// Whether the managed Claude remote-control host pane is present in `panes`.
pub fn claude_host_present(panes: &[PaneRef]) -> bool {
    !matching_managed_panes(panes, &ManagedPaneMarker::ClaudeRemoteControl).is_empty()
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DaemonViewInputsStamp {
    config_generation: u64,
    workspace: DaemonWorkspaceInputs,
    rimz_bin: StampedPath,
    claude_bin: Option<StampedPath>,
    codex_bin: Option<StampedPath>,
    claude_settings: StampedPath,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DaemonWorkspaceInputs {
    project_root: PathBuf,
    worktree_root: PathBuf,
}

impl DaemonWorkspaceInputs {
    /// Keep workspace freshness metadata out of daemon invalidation: ordinary
    /// CLI and hook entry points refresh `updated_at`, while only these roots
    /// shape the effective view.
    fn from_record(record: &workspace_record::WorkspaceRecord) -> Self {
        Self {
            project_root: record.project_root.clone(),
            worktree_root: record
                .worktree_root
                .clone()
                .unwrap_or_else(|| record.project_root.clone()),
        }
    }
}

struct ResolvedDaemonInputs {
    stamp: DaemonViewInputsStamp,
    rimz_bin: PathBuf,
    codex_present: bool,
}

impl ResolvedDaemonInputs {
    fn read(record: &workspace_record::WorkspaceRecord) -> Self {
        let rimz_bin = crate::proc::rimz_exe();
        let claude_bin = which::which("claude").ok();
        let codex_bin = which::which("codex").ok();
        let claude_settings = crate::remote_control::claude_settings_path();
        Self {
            stamp: DaemonViewInputsStamp {
                config_generation: crate::config::MachineConfig::load_stamp_generation(),
                workspace: DaemonWorkspaceInputs::from_record(record),
                rimz_bin: StampedPath::of(&rimz_bin),
                claude_bin: claude_bin.as_deref().map(StampedPath::of),
                codex_bin: codex_bin.as_deref().map(StampedPath::of),
                claude_settings: StampedPath::of(&claude_settings),
            },
            rimz_bin,
            codex_present: codex_bin.is_some(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DaemonFrameAction {
    Skip,
    Repair,
}

fn classify_daemon_frame(
    frame: Option<&crate::sidebar::frame::PaneFrame>,
    session_name: &str,
    view: &DaemonView,
    now_ms: u64,
) -> DaemonFrameAction {
    let Some(frame) = frame else {
        return DaemonFrameAction::Repair;
    };
    if frame.session_name != session_name
        || !crate::sidebar::cache::snapshot_cache_is_fresh(
            frame,
            now_ms,
            None,
            crate::sidebar::timing::EVENT_PANE_TTL,
        )
    {
        return DaemonFrameAction::Repair;
    }
    if next_repair_step(&frame.to_pane_refs(), view) == RepairStep::Done {
        DaemonFrameAction::Skip
    } else {
        DaemonFrameAction::Repair
    }
}

/// Long-lived elder maintenance state. Stable inputs and a healthy published
/// frame take the pure zero-child path; changed inputs rebuild the effective
/// specification, while stale or unhealthy frame truth falls back to the
/// authoritative backend repair path.
pub struct DaemonRepairTracker {
    workspace_id: WorkspaceId,
    session_name: String,
    inputs: Option<DaemonViewInputsStamp>,
    view: Option<DaemonView>,
    repair_pending: bool,
}

impl DaemonRepairTracker {
    pub fn new(workspace_id: WorkspaceId, session_name: String) -> Self {
        Self {
            workspace_id,
            session_name,
            inputs: None,
            view: None,
            repair_pending: false,
        }
    }

    pub fn maintain(&mut self, backend: &dyn MuxBackend, runtime: &crate::RuntimePaths) {
        let state = match StatePaths::for_workspace(self.workspace_id.clone()) {
            Ok(state) => state,
            Err(err) => {
                tracing::debug!(
                    workspace = %self.workspace_id,
                    error = &err as &dyn std::error::Error,
                    "daemon view maintenance skipped; state paths unavailable",
                );
                return;
            }
        };
        let record = match workspace_record::read(&state.workspace_record) {
            Ok(record) => record,
            Err(err) => {
                tracing::debug!(
                    workspace = %self.workspace_id,
                    error = &err as &dyn std::error::Error,
                    "daemon view maintenance skipped; workspace record unavailable",
                );
                return;
            }
        };
        let resolved = ResolvedDaemonInputs::read(&record);
        let frame = crate::sidebar::cache::read_snapshot_cache(
            &runtime.pane_frame_path(),
            &self.session_name,
        );
        let workspace_id = self.workspace_id.clone();
        let session_name = self.session_name.clone();
        self.maintain_with(
            resolved.stamp,
            frame.as_deref(),
            crate::sidebar::timing::unix_now_ms(),
            || {
                let machine = crate::config::MachineConfig::load_lenient();
                let readiness =
                    crate::remote_control::ReadinessSnapshot::probe(&machine.remote_control);
                Some(effective_daemon_view(
                    &workspace_id,
                    &session_name,
                    &record,
                    machine.as_ref(),
                    &resolved.rimz_bin,
                    &readiness,
                    resolved.codex_present,
                ))
            },
            |view| repair_daemon_view(backend, &session_name, &workspace_id, view),
        );
    }

    fn maintain_with(
        &mut self,
        inputs: DaemonViewInputsStamp,
        frame: Option<&crate::sidebar::frame::PaneFrame>,
        now_ms: u64,
        build: impl FnOnce() -> Option<DaemonView>,
        repair: impl FnOnce(&DaemonView) -> RepairOutcome,
    ) {
        if self.inputs.as_ref() != Some(&inputs) || self.view.is_none() {
            let Some(view) = build() else {
                return;
            };
            self.view = Some(view);
            self.inputs = Some(inputs);
            self.repair_pending = true;
        }

        let view = self.view.as_ref().expect("view rebuilt above");
        if !self.repair_pending
            && classify_daemon_frame(frame, &self.session_name, view, now_ms)
                == DaemonFrameAction::Skip
        {
            return;
        }
        self.repair_pending = repair(view) == RepairOutcome::Retry;
    }
}

/// Best-effort elder duty that reconstructs the daemon-view spec from durable
/// workspace metadata and current machine configuration, then repairs it.
pub fn ensure_daemon_view(
    backend: &dyn MuxBackend,
    workspace_id: &WorkspaceId,
    session_name: &str,
) {
    let paths = match StatePaths::for_workspace(workspace_id.clone()) {
        Ok(paths) => paths,
        Err(err) => {
            tracing::debug!(
                workspace = %workspace_id,
                error = &err as &dyn std::error::Error,
                "daemon view repair skipped; state paths unavailable",
            );
            return;
        }
    };
    let record = match workspace_record::read(&paths.workspace_record) {
        Ok(record) => record,
        Err(err) => {
            tracing::debug!(
                workspace = %workspace_id,
                error = &err as &dyn std::error::Error,
                "daemon view repair skipped; workspace record unavailable",
            );
            return;
        }
    };
    let machine = crate::config::MachineConfig::load_lenient();
    ensure_daemon_view_with_config(
        backend,
        workspace_id,
        session_name,
        &record,
        machine.as_ref(),
    );
}

pub(crate) fn ensure_daemon_view_with_config(
    backend: &dyn MuxBackend,
    workspace_id: &WorkspaceId,
    session_name: &str,
    record: &workspace_record::WorkspaceRecord,
    machine: &crate::config::MachineConfig,
) {
    let readiness = crate::remote_control::ReadinessSnapshot::probe(&machine.remote_control);
    ensure_daemon_view_with_readiness(
        backend,
        workspace_id,
        session_name,
        record,
        machine,
        &readiness,
    );
}

pub(crate) fn ensure_daemon_view_with_readiness(
    backend: &dyn MuxBackend,
    workspace_id: &WorkspaceId,
    session_name: &str,
    record: &workspace_record::WorkspaceRecord,
    machine: &crate::config::MachineConfig,
    readiness: &crate::remote_control::ReadinessSnapshot,
) {
    let rimz_bin = crate::proc::rimz_exe();
    let view = effective_daemon_view(
        workspace_id,
        session_name,
        record,
        machine,
        &rimz_bin,
        readiness,
        which::which("codex").is_ok(),
    );
    let _ = repair_daemon_view(backend, session_name, workspace_id, &view);
}

fn effective_daemon_view(
    workspace_id: &WorkspaceId,
    session_name: &str,
    record: &workspace_record::WorkspaceRecord,
    machine: &crate::config::MachineConfig,
    rimz_bin: &Path,
    readiness: &crate::remote_control::ReadinessSnapshot,
    codex_present: bool,
) -> DaemonView {
    let worktree_root = record
        .worktree_root
        .as_deref()
        .unwrap_or(&record.project_root);
    daemon_view_spec(DaemonViewSpecParams {
        claude_host_argv: readiness.claude_host_argv(),
        daemon: &machine.daemon,
        rimz_bin,
        workspace_id,
        session_name,
        project_root: &record.project_root,
        worktree_root,
        codex_present,
    })
}

#[cfg(test)]
mod tests;
