//! Runtime pane and agent facts for worktree removal safety.
//!
//! Domain removal policy stays in `rimz::worktree::ProtectionSet`; this module
//! preserves each CLI caller's fact-gathering and failure semantics.

use std::path::Path;

use anyhow::Result;

use super::{GlobalFlags, open_store};
use rimz::agents::AgentState;
use rimz::ids::{MuxName, PaneId};
use rimz::pane::PaneRef;
use rimz::workspace::{ResolvedWorkspace, WorkspaceResolver};

pub(super) struct RuntimeProtection {
    pub(super) protections: rimz::worktree::ProtectionSet,
    pub(super) agents: Vec<AgentState>,
}

/// Best-effort facts for a user-requested removal.
///
/// Stale records do not block an explicit decision, and the command's own pane
/// is excluded so invoking removal from inside a checkout can proceed.
pub(super) fn for_explicit_removal(repo_root: &Path, globals: &GlobalFlags) -> RuntimeProtection {
    best_effort(repo_root, globals, rimz::worktree::Occupancy::ProvenLive)
}

/// Best-effort facts for unattended wrapper cleanup.
///
/// Unlike explicit removal, unknown agent liveness remains protective.
pub(super) fn for_wrapper_cleanup(repo_root: &Path, globals: &GlobalFlags) -> RuntimeProtection {
    best_effort(repo_root, globals, rimz::worktree::Occupancy::Unproven)
}

/// Required roster facts for automatic gc.
///
/// The caller already resolved and opened its workspace. Roster failure is
/// returned so gc can skip the sweep instead of reclaiming without durable
/// agent truth; the invoking pane remains part of automatic occupancy.
pub(super) fn for_automatic_gc(
    workspace: &ResolvedWorkspace,
    store: &rimz::Store,
    globals: &GlobalFlags,
) -> Result<RuntimeProtection> {
    let agents = alive_agents(workspace, store)?;
    let (_, panes) = list_panes(Some(workspace), globals);
    Ok(assemble(
        &panes,
        agents,
        None,
        rimz::worktree::Occupancy::Unproven,
    ))
}

fn best_effort(
    repo_root: &Path,
    globals: &GlobalFlags,
    occupancy: rimz::worktree::Occupancy,
) -> RuntimeProtection {
    let workspace = match WorkspaceResolver::resolve(repo_root, globals.root.clone()) {
        Ok(workspace) => Some(workspace),
        Err(err) => {
            tracing::debug!(
                path = %repo_root.display(),
                error = %err,
                "could not resolve workspace while gathering worktree protection facts",
            );
            None
        }
    };
    let (mux, panes) = list_panes(workspace.as_ref(), globals);
    let own_pane = mux.and_then(rimz::mux::own_pane_id);
    let agents = workspace
        .as_ref()
        .and_then(|workspace| {
            open_store(workspace)
                .and_then(|store| alive_agents(workspace, &store))
                .ok()
        })
        .unwrap_or_default();
    assemble(&panes, agents, own_pane.as_ref(), occupancy)
}

fn list_panes(
    workspace: Option<&ResolvedWorkspace>,
    globals: &GlobalFlags,
) -> (Option<MuxName>, Vec<PaneRef>) {
    rimz::mux::auto_detect_backend(globals.mux)
        .ok()
        .map(|mux| {
            let panes = rimz::mux::backend_for(mux)
                .list_panes(rimz::mux::PaneListOptions {
                    session_name: workspace.map(|workspace| workspace.session_name.clone()),
                    workspace_id: workspace.map(|workspace| workspace.workspace_id.clone()),
                    ..Default::default()
                })
                .map(|listing| listing.panes)
                .unwrap_or_default();
            (Some(mux), panes)
        })
        .unwrap_or_default()
}

fn alive_agents(workspace: &ResolvedWorkspace, store: &rimz::Store) -> Result<Vec<AgentState>> {
    Ok(super::alive_snapshot(store, &workspace.session_name)?.agents)
}

fn assemble(
    panes: &[PaneRef],
    agents: Vec<AgentState>,
    own_pane: Option<&PaneId>,
    occupancy: rimz::worktree::Occupancy,
) -> RuntimeProtection {
    let protections =
        rimz::worktree::protection_set_from_runtime(panes, &agents, own_pane, occupancy);
    RuntimeProtection {
        protections,
        agents,
    }
}
