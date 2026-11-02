//! Runtime pane, agent, loop-task, team-board, and workflow-run removal protection facts.
//!
//! Domain removal policy stays in `rimz::worktree::ProtectionSet`; this module
//! preserves each CLI caller's fact-gathering and failure semantics.

use std::path::Path;

use anyhow::{Context, Result};

use super::{GlobalFlags, open_existing_store};
use rimz::agents::AgentState;
use rimz::ids::{MuxName, PaneId};
use rimz::pane::PaneRef;
use rimz::workspace::ResolvedWorkspace;

pub(super) struct RuntimeProtection {
    pub(super) protections: rimz::worktree::ProtectionSet,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ProtectionErr {
    #[error("reading the live agent roster: {0:#}")]
    Roster(#[source] anyhow::Error),
    #[error("loop tasks unavailable: {0:#}")]
    LoopTasks(#[source] anyhow::Error),
    #[error("workflow runs unavailable: {0:#}")]
    WorkflowRuns(#[source] anyhow::Error),
    #[error("listing owned worktrees: {0}")]
    WorktreeListing(#[source] rimz::worktree::WorktreeErr),
}

fn add_loop_tasks(protection: &mut RuntimeProtection) -> Result<()> {
    for pin in rimz::harness::schedule::catalog::load_worktree_pins()? {
        protection
            .protections
            .insert_loop_task(&pin.name, &pin.path);
    }
    Ok(())
}

fn add_workflow_runs(
    protection: &mut RuntimeProtection,
    mode: rimz::store::workflow::RunRead,
) -> Result<()> {
    for run in rimz::store::workflow::unfinished_under(&rimz::disk::paths::rimz_home(), mode)? {
        protection.protections.insert_workflow_run(
            &run.id,
            &run.address().to_string(),
            &run.scope.path,
        );
    }
    Ok(())
}

fn add_team_board(protection: &mut RuntimeProtection, path: &Path) {
    if let Some(team) = rimz::harness::scratch::holding_team(path) {
        protection.protections.insert_team(&team, path);
    }
}

fn add_team_boards(
    protection: &mut RuntimeProtection,
    repo_root: &Path,
) -> rimz::worktree::Result<()> {
    for tree in rimz::worktree::discover_owned(repo_root)? {
        add_team_board(protection, &tree.path);
    }
    Ok(())
}

/// Best-effort occupancy, task, team-board, and workflow-run facts, with gc's own-pane policy.
pub(super) fn for_listing(
    workspace: &ResolvedWorkspace,
    globals: &GlobalFlags,
) -> RuntimeProtection {
    let (_, panes) = list_panes(Some(workspace), globals);
    let agents = open_existing_store(workspace)
        .ok()
        .flatten()
        .and_then(|store| alive_agents(workspace, &store).ok())
        .unwrap_or_default();
    let mut protection = assemble(&panes, agents, None, rimz::worktree::Occupancy::Unproven);
    let _ = add_loop_tasks(&mut protection);
    let _ = add_team_boards(&mut protection, &workspace.project_root);
    let _ = add_workflow_runs(&mut protection, rimz::store::workflow::RunRead::BestEffort);
    protection
}

/// Best-effort facts for a user-requested removal, from the room of each repository root.
///
/// Stale records do not block an explicit decision, and the command's own pane
/// is excluded so invoking removal from inside a checkout can proceed.
/// Unfinished workflow runs are read best-effort across every room.
pub(super) fn for_explicit_removal(
    repo_roots: &[&Path],
    globals: &GlobalFlags,
) -> RuntimeProtection {
    RuntimeProtection {
        protections: rimz::launcher::worktree_entry::explicit_removal_protections(
            repo_roots,
            globals.root.as_deref(),
            globals.mux,
        ),
    }
}

/// Best-effort occupancy for landing, which does not remove a workflow's checkout.
pub(super) fn for_merge(repo_roots: &[&Path], globals: &GlobalFlags) -> RuntimeProtection {
    RuntimeProtection {
        protections: rimz::launcher::worktree_entry::occupancy_protections(
            repo_roots,
            globals.root.as_deref(),
            globals.mux,
        ),
    }
}

/// Best-effort occupancy, required task and workflow facts, and the cleanup path's team board.
///
/// Unlike explicit removal, unknown agent liveness remains protective.
pub(super) fn for_wrapper_cleanup(
    workspace: &ResolvedWorkspace,
    path: &Path,
    globals: &GlobalFlags,
) -> Result<RuntimeProtection> {
    let mut protection = best_effort_workspaces(
        std::slice::from_ref(workspace),
        globals,
        rimz::worktree::Occupancy::Unproven,
    );
    add_loop_tasks(&mut protection).context("loop tasks unavailable")?;
    add_workflow_runs(&mut protection, rimz::store::workflow::RunRead::Strict)
        .context("workflow runs unavailable")?;
    add_team_board(&mut protection, path);
    Ok(protection)
}

/// Required roster, task, workflow-run, and owned-tree facts, including team-board holds.
///
/// The caller already resolved and opened its workspace. A roster, task-source, run-source,
/// or owned-tree listing failure is returned so gc can skip the sweep instead
/// of reclaiming without durable agent, task, run, or board truth; the invoking pane
/// remains part of automatic occupancy.
pub(super) fn for_automatic_gc(
    workspace: &ResolvedWorkspace,
    store: &rimz::Store,
    globals: &GlobalFlags,
) -> std::result::Result<RuntimeProtection, ProtectionErr> {
    let agents = alive_agents(workspace, store).map_err(ProtectionErr::Roster)?;
    let (_, panes) = list_panes(Some(workspace), globals);
    let mut protection = assemble(&panes, agents, None, rimz::worktree::Occupancy::Unproven);
    add_loop_tasks(&mut protection).map_err(ProtectionErr::LoopTasks)?;
    add_workflow_runs(&mut protection, rimz::store::workflow::RunRead::Strict)
        .map_err(ProtectionErr::WorkflowRuns)?;
    add_team_boards(&mut protection, &workspace.project_root)
        .map_err(ProtectionErr::WorktreeListing)?;
    Ok(protection)
}

fn best_effort_workspaces(
    workspaces: &[ResolvedWorkspace],
    globals: &GlobalFlags,
    occupancy: rimz::worktree::Occupancy,
) -> RuntimeProtection {
    RuntimeProtection {
        protections: rimz::launcher::worktree_entry::runtime_protections(
            workspaces,
            globals.mux,
            occupancy,
        ),
    }
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
    Ok(std::sync::Arc::unwrap_or_clone(
        super::alive_snapshot(store, &workspace.session_name)?.agents,
    ))
}

fn assemble(
    panes: &[PaneRef],
    agents: Vec<AgentState>,
    own_pane: Option<&PaneId>,
    occupancy: rimz::worktree::Occupancy,
) -> RuntimeProtection {
    let protections =
        rimz::worktree::protection_set_from_runtime(panes, &agents, own_pane, occupancy);
    RuntimeProtection { protections }
}
