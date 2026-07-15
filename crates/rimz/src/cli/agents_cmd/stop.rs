use super::*;

use super::runs_lookup::{agent_name, newest_run_by_ref, newest_run_for_agent};
use crate::cli::render;

pub(super) fn stop_agent(reference: String, all: bool, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let store = crate::cli::open_store(&workspace)?;
    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    let current_channel = crate::cli::current_channel(&workspace);
    if all {
        let agents = rimz::harness::target::resolve_many(
            &snapshot,
            &reference,
            None,
            current_channel.as_deref(),
        )?;
        let peers: Vec<&AgentState> = snapshot.root_agents().collect();
        let mut failed = false;
        let mut out = render::out();
        for agent in agents {
            let label = rimz::harness::target::agent_handle(agent, &peers, true);
            match stop_live_agent(&workspace, &store, globals, agent) {
                Ok(()) => writeln!(out, "stopped {label}")?,
                Err(err) => {
                    failed = true;
                    writeln!(out, "error {label}: {err:#}")?;
                }
            }
        }
        if failed {
            std::process::exit(1);
        }
        return Ok(());
    }
    let live_agent_result =
        crate::cli::resolve_agent_one(&snapshot, &reference, None, current_channel.as_deref());
    let live_agent = live_agent_result.as_ref().ok().copied();
    if let Some(run) = newest_run_by_ref(&store, &reference, live_agent)? {
        supervised::stop_supervised_run(&workspace, &store, globals, &run)?;
        return Ok(());
    }
    let live_agent = live_agent_result.map_err(|err| stop_resolve_error(err, &reference))?;
    close_agent_pane(&workspace, live_agent)
}

fn stop_resolve_error(err: anyhow::Error, reference: &str) -> anyhow::Error {
    let Some(target_err) = err.downcast_ref::<rimz::TargetErr>() else {
        return err;
    };
    if matches!(target_err, rimz::TargetErr::Ambiguous { .. }) {
        anyhow::anyhow!(
            "{target_err}; re-run `rimz agents stop {reference} --all` to stop every match"
        )
    } else {
        err
    }
}

fn stop_live_agent(
    workspace: &rimz::ResolvedWorkspace,
    store: &rimz::Store,
    globals: &GlobalFlags,
    agent: &AgentState,
) -> Result<()> {
    if let Some(run) = newest_run_for_agent(store, agent)? {
        supervised::stop_supervised_run(workspace, store, globals, &run)
    } else {
        close_agent_pane(workspace, agent)
    }
}

fn close_agent_pane(workspace: &rimz::ResolvedWorkspace, agent: &AgentState) -> Result<()> {
    let pane = agent
        .pane
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("agent {} has no bound pane", agent_name(agent)))?;
    let backend = rimz::mux::backend_for(pane.pane_id.mux());
    backend
        .close_pane(&workspace.session_name, &pane.pane_id)
        .map_err(Into::into)
}
