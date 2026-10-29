//! Managed `rimzd` view specification, classification, and repair.
//!
//! The view is `sidebar | content | runtime`: content supervisors occupy the
//! middle column, while the per-session Codex broker, Claude remote-control
//! host, and loop panel share the right column. Managed
//! panes carry their launch command as mux identity so repair remains stable
//! when a foreground child replaces the original command.
//!
//! Repair plans one placement from pane-listing truth, spawns it, waits for its
//! marker to settle, and re-lists before planning the next placement. This
//! makes every newly restored pane available as the next spec-order anchor and
//! preserves one runtime column when several panes disappear together.
//!
//! Internals: `docs/internals/rimzd.md`.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::DaemonConfig;
use crate::disk::parse_cache::StampedPath;
use crate::disk::paths::StatePaths;
use crate::ids::{PaneId, WorkspaceId};
use crate::mux::{
    DaemonView, HostPane, MuxBackend, PaneListOptions, PaneListing, PaneReadConsistency, PaneRect,
    SplitDirection, SplitPaneOptions, SplitPlacement, SplitTarget, ViewGeometry,
};
use crate::pane::{APP_SERVER_MARKER, PaneRef, VIEW_NAME, command_is_claude_host};
use crate::workspace::record;

const REPAIR_LIST_TIMEOUT: Duration = Duration::from_secs(3);
const SETTLE_ATTEMPTS: usize = 5;
const SETTLE_POLL: Duration = Duration::from_millis(100);

/// Shape of the managed content and runtime columns in a daemon view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DaemonShape {
    Healthy,
    Repairable { faults: Vec<DaemonShapeFault> },
    Misshapen { faults: Vec<DaemonShapeFault> },
    Unjudged,
}

/// Column faults, in content, runtime, then left-to-right order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonShapeFault {
    ContentSplit,
    ContentNotFullHeight,
    RuntimeSplit,
    RuntimeNotFullHeight,
    RuntimeLeftOfContent,
}

/// Inspect a daemon view without waking or changing any pane. `None` means closed.
pub fn inspect_daemon_shape(
    backend: &dyn MuxBackend,
    session_name: &str,
    workspace_id: &WorkspaceId,
) -> crate::mux::Result<Option<DaemonShape>> {
    let listing = list_daemon_panes(backend, session_name, workspace_id)?;
    listed_daemon_shape(backend, session_name, &listing.panes)
}

fn pane_marker(pane: &PaneRef) -> Option<ManagedPaneMarker> {
    if pane.view_name.as_deref() != Some(VIEW_NAME) || pane.is_floating {
        return None;
    }
    [
        pane.spawn_command.as_deref(),
        pane.command.as_deref(),
        pane.title.as_deref(),
    ]
    .into_iter()
    .flatten()
    .find_map(command_marker)
}

fn rect_marker(panes: &[PaneRef], rect: &PaneRect) -> Option<ManagedPaneMarker> {
    panes
        .iter()
        .find(|pane| pane.pane_id == rect.pane_id)
        .and_then(pane_marker)
}

fn listed_daemon_shape(
    backend: &dyn MuxBackend,
    session_name: &str,
    panes: &[PaneRef],
) -> crate::mux::Result<Option<DaemonShape>> {
    let anchor = panes
        .iter()
        .find(|pane| pane_marker(pane).is_some())
        .or_else(|| {
            panes
                .iter()
                .find(|pane| pane.view_name.as_deref() == Some(VIEW_NAME))
        });
    let Some(anchor) = anchor else {
        return Ok(None);
    };
    let geometry = backend.view_geometry(session_name, &anchor.pane_id, REPAIR_LIST_TIMEOUT)?;
    Ok(Some(judge_daemon_shape(panes, geometry.as_ref())))
}

fn judge_daemon_shape(panes: &[PaneRef], geometry: Option<&ViewGeometry>) -> DaemonShape {
    let Some(geometry) = geometry else {
        return DaemonShape::Unjudged;
    };
    if geometry.panes.is_empty()
        || panes.iter().any(|pane| {
            pane_marker(pane).is_some()
                && !geometry
                    .panes
                    .iter()
                    .any(|rect| rect.pane_id == pane.pane_id)
        })
    {
        return DaemonShape::Unjudged;
    }
    let faults = shape_faults(panes, geometry);
    if faults.is_empty() {
        return DaemonShape::Healthy;
    }
    if repairable_loop(panes, geometry).is_some() {
        DaemonShape::Repairable { faults }
    } else {
        DaemonShape::Misshapen { faults }
    }
}

fn shape_faults(panes: &[PaneRef], geometry: &ViewGeometry) -> Vec<DaemonShapeFault> {
    let top = geometry.panes.iter().map(|pane| pane.y).min().unwrap_or(0);
    let bottom = geometry
        .panes
        .iter()
        .map(|pane| pane.y + pane.rows)
        .max()
        .unwrap_or(0);
    let mut faults = Vec::new();
    let mut band = |column, split, short| {
        let members = geometry
            .panes
            .iter()
            .filter(|rect| rect_marker(panes, rect).is_some_and(|marker| marker.column() == column))
            .collect::<Vec<_>>();
        let first = *members.first()?;
        let band = (first.x, first.cols);
        if members.iter().any(|pane| (pane.x, pane.cols) != band) {
            faults.push(split);
            return None;
        }
        let mut tiles = geometry
            .panes
            .iter()
            .filter(|pane| {
                (pane.x, pane.cols) == band
                    && rect_marker(panes, pane).is_none_or(|marker| marker.column() == column)
            })
            .collect::<Vec<_>>();
        tiles.sort_by_key(|pane| pane.y);
        let mut edge = top;
        for tile in tiles {
            if tile.y != edge {
                faults.push(short);
                return Some(band);
            }
            edge = tile.y + tile.rows + geometry.gap;
        }
        if edge != bottom + geometry.gap {
            faults.push(short);
        }
        Some(band)
    };
    let content = band(
        DaemonColumn::Content,
        DaemonShapeFault::ContentSplit,
        DaemonShapeFault::ContentNotFullHeight,
    );
    let runtime = band(
        DaemonColumn::Runtime,
        DaemonShapeFault::RuntimeSplit,
        DaemonShapeFault::RuntimeNotFullHeight,
    );
    if let (Some((content_x, content_cols)), Some((runtime_x, _))) = (content, runtime)
        && runtime_x < content_x + content_cols + geometry.gap
    {
        faults.push(DaemonShapeFault::RuntimeLeftOfContent);
    }
    faults
}

fn repairable_loop(panes: &[PaneRef], geometry: &ViewGeometry) -> Option<PaneId> {
    let panel = geometry
        .panes
        .iter()
        .filter(|rect| rect_marker(panes, rect) == Some(ManagedPaneMarker::LoopPanel))
        .min_by_key(|rect| (rect.pane_id.creation_ordinal(), rect.pane_id.as_str()))?;
    let bottom = geometry.panes.iter().map(|pane| pane.y + pane.rows).max()?;
    if panel.y + panel.rows != bottom {
        return None;
    }
    let mut above = geometry
        .panes
        .iter()
        .filter(|pane| {
            pane.y + pane.rows + geometry.gap == panel.y
                && pane.x >= panel.x
                && pane.x + pane.cols <= panel.x + panel.cols
        })
        .collect::<Vec<_>>();
    above.sort_by_key(|pane| pane.x);
    let mut edge = panel.x;
    for pane in &above {
        if pane.x != edge {
            return None;
        }
        edge = pane.x + pane.cols + geometry.gap;
    }
    if edge != panel.x + panel.cols + geometry.gap
        || !above.iter().any(|rect| {
            rect_marker(panes, rect).is_some_and(|marker| marker.column() == DaemonColumn::Content)
        })
        || !above.iter().any(|rect| {
            rect_marker(panes, rect).is_some_and(|marker| marker.column() == DaemonColumn::Runtime)
        })
    {
        return None;
    }
    for left in [true, false] {
        let edges = geometry
            .panes
            .iter()
            .filter(|pane| {
                if left {
                    pane.x + pane.cols + geometry.gap == panel.x
                } else {
                    pane.x == panel.x + panel.cols + geometry.gap
                }
            })
            .flat_map(|pane| [pane.y, pane.y + pane.rows])
            .collect::<BTreeSet<_>>();
        if edges.contains(&panel.y) && edges.contains(&(panel.y + panel.rows)) {
            return None;
        }
    }
    let mut filled = geometry.clone();
    filled.panes.retain(|pane| pane.pane_id != panel.pane_id);
    for pane in &mut filled.panes {
        if above.iter().any(|old| old.pane_id == pane.pane_id) {
            pane.rows += panel.rows + geometry.gap;
        }
    }
    shape_faults(panes, &filled)
        .is_empty()
        .then(|| panel.pane_id.clone())
}

#[derive(Debug, PartialEq, Eq)]
enum ShapeCloseOutcome {
    Left,
    Closed,
    Retry,
}

fn close_shape_loop_with(
    pane: &PaneId,
    wake: impl FnOnce() -> bool,
    recheck: impl FnOnce() -> crate::mux::Result<Option<PaneId>>,
    close: impl FnOnce() -> bool,
) -> ShapeCloseOutcome {
    if !wake() {
        return ShapeCloseOutcome::Retry;
    }
    match recheck() {
        Ok(Some(fresh)) if fresh == *pane => {
            if close() {
                ShapeCloseOutcome::Closed
            } else {
                ShapeCloseOutcome::Retry
            }
        }
        Ok(_) => ShapeCloseOutcome::Left,
        Err(error) => {
            tracing::debug!(%error, "daemon shape could not be re-read after wake");
            ShapeCloseOutcome::Retry
        }
    }
}

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
        content: content_panes(
            params.daemon,
            params.rimz_bin,
            params.worktree_root,
            params.workspace_id,
        ),
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
        let mut wrapped = vec![
            params.rimz_bin.to_string_lossy().into_owned(),
            "daemon".to_owned(),
            "host".to_owned(),
            "--workspace-id".to_owned(),
            params.workspace_id.as_str().to_owned(),
            "--".to_owned(),
        ];
        wrapped.extend_from_slice(argv);
        hosts.push(HostPane {
            argv: wrapped,
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

fn content_panes(
    daemon: &DaemonConfig,
    rimz_bin: &Path,
    worktree_root: &Path,
    workspace_id: &WorkspaceId,
) -> Vec<HostPane> {
    (0..crate::daemon_content::resolve_content(daemon, rimz_bin, worktree_root).len())
        .map(|slot| content_supervisor_pane(slot, rimz_bin, worktree_root, workspace_id))
        .collect()
}

fn content_supervisor_pane(
    slot: usize,
    rimz_bin: &Path,
    worktree_root: &Path,
    workspace_id: &WorkspaceId,
) -> HostPane {
    HostPane {
        argv: vec![
            rimz_bin.to_string_lossy().into_owned(),
            "daemon".to_owned(),
            "content".to_owned(),
            "--slot".to_owned(),
            slot.to_string(),
            "--worktree-root".to_owned(),
            worktree_root.to_string_lossy().into_owned(),
            "--workspace-id".to_owned(),
            workspace_id.as_str().to_owned(),
        ],
        cwd: worktree_root.to_path_buf(),
    }
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
/// Once the markers are satisfied, restart a loop watch spanning beneath both
/// columns; every other wrong shape is left as it is.
pub fn repair_daemon_view(
    backend: &dyn MuxBackend,
    session_name: &str,
    workspace_id: &WorkspaceId,
    view: &DaemonView,
) -> RepairOutcome {
    let Ok(mut listing) = list_daemon_panes(backend, session_name, workspace_id) else {
        return RepairOutcome::Retry;
    };
    if !listing
        .panes
        .iter()
        .any(|pane| pane.view_name.as_deref() == Some(VIEW_NAME))
    {
        return RepairOutcome::Converged;
    }
    let reconciliation = managed_pane_reconciliation(view, &listing.panes);
    let missing_count = reconciliation.spawn.len();

    if !close_surplus_panes(backend, session_name, workspace_id, &reconciliation.close) {
        return RepairOutcome::Retry;
    }
    listing
        .panes
        .retain(|pane| !reconciliation.close.contains(&pane.pane_id));

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
        repair_daemon_shape(backend, session_name, workspace_id, view, &listing.panes)
    } else {
        RepairOutcome::Retry
    }
}

fn list_daemon_panes(
    backend: &dyn MuxBackend,
    session_name: &str,
    workspace_id: &WorkspaceId,
) -> crate::mux::Result<PaneListing> {
    backend
        .list_panes(PaneListOptions {
            session_name: Some(session_name.to_owned()),
            workspace_id: Some(workspace_id.clone()),
            command_timeout: Some(REPAIR_LIST_TIMEOUT),
            consistency: PaneReadConsistency::RequireAuthoritative,
            ..Default::default()
        })
        .inspect_err(|err| {
            tracing::debug!(
                session = %session_name,
                error = err as &dyn std::error::Error,
                "daemon view repair skipped; pane listing failed",
            );
        })
}

fn shape_repair_target(
    backend: &dyn MuxBackend,
    session_name: &str,
    panes: &[PaneRef],
) -> crate::mux::Result<Option<PaneId>> {
    if !matches!(
        listed_daemon_shape(backend, session_name, panes)?,
        Some(DaemonShape::Repairable { .. })
    ) {
        return Ok(None);
    }
    Ok(panes
        .iter()
        .filter(|pane| pane_marker(pane) == Some(ManagedPaneMarker::LoopPanel))
        .min_by_key(|pane| (pane.pane_id.creation_ordinal(), pane.pane_id.as_str()))
        .map(|pane| pane.pane_id.clone()))
}

fn repair_daemon_shape(
    backend: &dyn MuxBackend,
    session_name: &str,
    workspace_id: &WorkspaceId,
    view: &DaemonView,
    panes: &[PaneRef],
) -> RepairOutcome {
    let panel = match shape_repair_target(backend, session_name, panes) {
        Ok(Some(panel)) => panel,
        Ok(None) => return RepairOutcome::Converged,
        Err(error) => {
            tracing::debug!(%error, "daemon shape read failed");
            return RepairOutcome::Retry;
        }
    };
    let Ok(paths) = StatePaths::for_workspace(workspace_id.clone()) else {
        return RepairOutcome::Retry;
    };
    match close_shape_loop_with(
        &panel,
        || {
            !crate::store::hibernate::read(&paths)
                .iter()
                .any(|entry| entry.pane_id == panel)
                || match crate::harness::hibernate::wake_pane_at(&paths, &panel) {
                    Ok(_) => true,
                    Err(error) => {
                        tracing::warn!(%error, %panel, "could not wake misshapen loop panel");
                        false
                    }
                }
        },
        || {
            let fresh = list_daemon_panes(backend, session_name, workspace_id)?;
            shape_repair_target(backend, session_name, &fresh.panes)
        },
        || match backend.close_pane(session_name, &panel) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, %panel, "could not close misshapen loop panel");
                false
            }
        },
    ) {
        ShapeCloseOutcome::Left => return RepairOutcome::Converged,
        ShapeCloseOutcome::Retry => return RepairOutcome::Retry,
        ShapeCloseOutcome::Closed => {}
    }
    let Ok(listing) = list_daemon_panes(backend, session_name, workspace_id) else {
        return RepairOutcome::Retry;
    };
    let marker = ManagedPaneMarker::LoopPanel;
    if !matching_managed_panes(&listing.panes, &marker).is_empty() {
        return RepairOutcome::Retry;
    }
    let Some((anchor, direction)) = spawn_anchor(&listing.panes, view, &marker) else {
        return RepairOutcome::Retry;
    };
    if !split_managed_pane(
        backend,
        session_name,
        &listing.panes,
        &view.loop_panel,
        &anchor,
        direction,
    ) {
        return RepairOutcome::Retry;
    }
    match settle_managed_pane(backend, session_name, workspace_id, &marker) {
        Some(listing) if next_repair_step(&listing.panes, view) == RepairStep::Done => {
            RepairOutcome::Converged
        }
        _ => RepairOutcome::Retry,
    }
}

fn close_surplus_panes(
    backend: &dyn MuxBackend,
    session_name: &str,
    workspace_id: &WorkspaceId,
    pane_ids: &[PaneId],
) -> bool {
    if pane_ids.is_empty() {
        return true;
    }
    let Ok(paths) = StatePaths::for_workspace(workspace_id.clone()) else {
        return false;
    };
    close_surplus_panes_with(
        &paths,
        pane_ids,
        |pane| match crate::harness::hibernate::wake_pane_at(&paths, pane) {
            Ok(_) => true,
            Err(error) => {
                tracing::warn!(%error, %pane, "could not wake surplus pane before closing");
                false
            }
        },
        |pane_id| {
            if let Err(err) = backend.close_pane(session_name, pane_id) {
                tracing::warn!(
                    session = %session_name,
                    view = VIEW_NAME,
                    pane = %pane_id,
                    error = &err as &dyn std::error::Error,
                    "daemon view repair could not close a surplus managed pane",
                );
                return false;
            }
            true
        },
    )
}

fn close_surplus_panes_with(
    paths: &StatePaths,
    pane_ids: &[PaneId],
    mut wake: impl FnMut(&PaneId) -> bool,
    mut close: impl FnMut(&PaneId) -> bool,
) -> bool {
    let mut converged = true;
    let entries = crate::store::hibernate::read(paths);
    for pane in pane_ids {
        if entries.iter().any(|entry| &entry.pane_id == pane) && !wake(pane) {
            converged = false;
            continue;
        }
        converged &= close(pane);
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
        target: SplitTarget::SessionPane {
            session_name: session_name.to_owned(),
            pane_id: anchor.pane_id.clone(),
        },
        cwd: Some(pane.cwd.to_string_lossy().into_owned()),
        command: Some(pane.argv.clone()),
        title: Some(title),
        // Match view-born managed panes on both backends: an exit leaves no pane.
        close_on_exit: true,
        env: Default::default(),
        placement: SplitPlacement::Directional(direction),
        focus: false,
    }) {
        Ok(_) => true,
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
        let listing = list_daemon_panes(backend, session_name, workspace_id).ok()?;
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
            [
                ManagedPaneMarker::CodexAppServer,
                ManagedPaneMarker::ClaudeRemoteControl,
                ManagedPaneMarker::LoopPanel,
            ]
            .into_iter()
            .find(|marker| command_matches_marker(command, marker))
        })
}

fn command_matches_marker(command: &str, marker: &ManagedPaneMarker) -> bool {
    match marker {
        ManagedPaneMarker::ContentSlot(slot) => content_slot_from_command(command) == Some(*slot),
        ManagedPaneMarker::CodexAppServer => command
            .split_whitespace()
            .any(|word| word == APP_SERVER_MARKER),
        ManagedPaneMarker::ClaudeRemoteControl => command_is_claude_host(command),
        ManagedPaneMarker::LoopPanel => command
            .split_whitespace()
            .zip(command.split_whitespace().skip(1))
            .any(|(command, subcommand)| command == "loop" && subcommand == "watch"),
    }
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
    claude_settings: Option<StampedPath>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DaemonWorkspaceInputs {
    project_root: PathBuf,
    worktree_root: PathBuf,
    remote_control_off: BTreeSet<crate::ids::AgentKind>,
}

impl DaemonWorkspaceInputs {
    /// Keep workspace freshness metadata out of daemon invalidation: ordinary
    /// CLI and hook entry points refresh `updated_at`, while only these roots
    /// shape the effective view.
    fn from_record(record: &record::WorkspaceRecord) -> Self {
        Self {
            project_root: record.project_root.clone(),
            worktree_root: record
                .worktree_root
                .clone()
                .unwrap_or_else(|| record.project_root.clone()),
            remote_control_off: record.remote_control_off.clone(),
        }
    }
}

struct ResolvedDaemonInputs {
    stamp: DaemonViewInputsStamp,
    rimz_bin: PathBuf,
    codex_present: bool,
    host_envs: Result<crate::remote_control::HostLoginEnvs, crate::agents::RoomLoginErr>,
}

impl ResolvedDaemonInputs {
    fn read(
        record: &record::WorkspaceRecord,
        record_path: &Path,
        machine: &crate::config::MachineConfig,
    ) -> Self {
        let rimz_bin = crate::proc::rimz_exe();
        let claude_bin = which::which("claude").ok();
        let codex_bin = crate::agents::runtime_control::installed_broker_bin();
        let host_envs = crate::remote_control::HostLoginEnvs::for_room(record_path, machine);
        let claude_settings = host_envs
            .as_ref()
            .ok()
            .and_then(|envs| {
                envs.for_host(crate::remote_control::RemoteControlHost::Claude)
                    .ok()
            })
            .map(crate::remote_control::claude_settings_path);
        Self {
            stamp: DaemonViewInputsStamp {
                config_generation: crate::config::MachineConfig::load_stamp_generation(),
                workspace: DaemonWorkspaceInputs::from_record(record),
                rimz_bin: StampedPath::of(&rimz_bin),
                claude_bin: claude_bin.as_deref().map(StampedPath::of),
                codex_bin: codex_bin.as_deref().map(StampedPath::of),
                claude_settings: claude_settings.as_deref().map(StampedPath::of),
            },
            rimz_bin,
            codex_present: codex_bin.is_some(),
            host_envs,
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
/// frame with unchanged pane membership take the pure zero-child path; changed
/// inputs rebuild the effective specification, while changed membership or stale
/// or unhealthy frame truth falls back to authoritative backend repair.
pub struct DaemonRepairTracker {
    workspace_id: WorkspaceId,
    session_name: String,
    inputs: Option<DaemonViewInputsStamp>,
    view: Option<DaemonView>,
    repair_pending: bool,
    last_pass_pane_ids: Option<HashSet<PaneId>>,
}

impl DaemonRepairTracker {
    pub fn new(workspace_id: WorkspaceId, session_name: String) -> Self {
        Self {
            workspace_id,
            session_name,
            inputs: None,
            view: None,
            repair_pending: false,
            last_pass_pane_ids: None,
        }
    }

    pub fn maintain(
        &mut self,
        backend: &dyn MuxBackend,
        frame: Option<&crate::sidebar::frame::PaneFrame>,
    ) {
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
        let record = match record::read(&state.workspace_record) {
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
        let machine = crate::config::MachineConfig::load_lenient();
        let remote_control = crate::remote_control::room_config(&machine.remote_control, &record);
        let resolved = ResolvedDaemonInputs::read(&record, &state.workspace_record, &machine);
        let workspace_id = self.workspace_id.clone();
        let session_name = self.session_name.clone();
        self.maintain_with(
            resolved.stamp,
            frame,
            crate::utils::time::unix_now_ms(),
            || {
                // Repair keeps an enabled host serving, so it refills the host's
                // own preconditions before judging it. Probing first would read a
                // precondition this pass is able to restore, and tear down a
                // working host over a gap that outlives nothing but this tick.
                let readiness = match &resolved.host_envs {
                    Ok(envs) => {
                        crate::remote_control::prepare_hosts(&remote_control, envs);
                        crate::remote_control::ReadinessSnapshot::probe(&remote_control, envs, Some(&record.project_root))
                    }
                    Err(err) => {
                        tracing::warn!(workspace = %workspace_id, error = %err, "daemon host accounts unavailable");
                        crate::remote_control::ReadinessSnapshot::disabled()
                    }
                };
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
        let pane_ids = frame.map(|frame| {
            frame
                .tabs
                .iter()
                .filter(|tab| tab.name.as_deref() == Some(VIEW_NAME))
                .flat_map(|tab| tab.panes.iter().map(|pane| pane.pane_id.clone()))
                .collect::<HashSet<_>>()
        });
        if !self.repair_pending
            && pane_ids == self.last_pass_pane_ids
            && classify_daemon_frame(frame, &self.session_name, view, now_ms)
                == DaemonFrameAction::Skip
        {
            return;
        }
        self.repair_pending = repair(view) == RepairOutcome::Retry;
        self.last_pass_pane_ids = pane_ids;
    }
}

pub(crate) fn ensure_daemon_view_with_readiness(
    backend: &dyn MuxBackend,
    workspace_id: &WorkspaceId,
    session_name: &str,
    record: &record::WorkspaceRecord,
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
        crate::agents::runtime_control::installed_broker_bin().is_some(),
    );
    let _ = repair_daemon_view(backend, session_name, workspace_id, &view);
}

fn effective_daemon_view(
    workspace_id: &WorkspaceId,
    session_name: &str,
    record: &record::WorkspaceRecord,
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
