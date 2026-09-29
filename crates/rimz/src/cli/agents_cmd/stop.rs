use super::*;

use std::collections::HashSet;

use super::runs_lookup::{agent_name, newest_run_by_ref, newest_run_for_agent};
use crate::cli::render;
use rimz::store::snapshot::find_agent;

#[derive(Default)]
pub(in crate::cli) struct StopTracker {
    stopped: HashSet<(AgentKind, AgentSessionId)>,
}

pub(super) fn stop_agent(reference: String, all: bool, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let (workspace, store) = (&ctx.workspace, &ctx.store);
    let snapshot = ctx.cached_snapshot()?;
    let current_channel = ctx.channel();
    if all && reference != "@me" {
        let agents = rimz::address::resolve_many(&snapshot, &reference, None, current_channel)?;
        let peers = rimz::address::addressable_agents(&snapshot);
        let mut tracker = StopTracker::default();
        let mut failed = false;
        let mut out = render::out();
        for agent in agents {
            let label = rimz::address::agent_handle(agent, &peers, true);
            match stop_live_agent_tree(
                workspace,
                store,
                globals,
                &snapshot,
                &peers,
                agent,
                &mut tracker,
            ) {
                Ok(true) => writeln!(out, "stopped {label}")?,
                Ok(false) => {}
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
        crate::cli::resolve_agent_one(store, &snapshot, &reference, None, current_channel);
    let live_agent = live_agent_result.as_ref().ok().copied();
    if let Some(live_agent) = live_agent {
        let peers = rimz::address::addressable_agents(&snapshot);
        stop_live_agent_tree(
            workspace,
            store,
            globals,
            &snapshot,
            &peers,
            live_agent,
            &mut StopTracker::default(),
        )?;
        return Ok(());
    }
    if let Some(run) =
        newest_run_by_ref(store, &reference, live_agent)?.filter(|run| run.peer.is_none())
    {
        supervised::stop_supervised_run(workspace, store, globals, &run)?;
        return Ok(());
    }
    let live_agent = live_agent_result.map_err(|err| stop_resolve_error(err, &reference))?;
    close_agent_pane(workspace, live_agent)
}

fn stop_resolve_error(err: anyhow::Error, reference: &str) -> anyhow::Error {
    let Some(target_err) = err.downcast_ref::<rimz::address::TargetErr>() else {
        return err;
    };
    if matches!(target_err, rimz::address::TargetErr::Ambiguous { .. }) {
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
    if let Some(run) = newest_run_for_agent(store, agent)?.filter(|run| run.peer.is_none()) {
        supervised::stop_supervised_run(workspace, store, globals, &run)
    } else {
        close_agent_pane(workspace, agent)
    }
}

pub(in crate::cli) fn stop_resolved(
    ctx: &Ctx,
    globals: &GlobalFlags,
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    agent: &AgentState,
    tracker: &mut StopTracker,
) -> Result<bool> {
    let peers = rimz::address::addressable_agents(snapshot);
    let current = find_agent(&snapshot.agents, &agent.kind, &agent.agent_id).unwrap_or(agent);
    stop_live_agent_tree(
        &ctx.workspace,
        &ctx.store,
        globals,
        snapshot,
        &peers,
        current,
        tracker,
    )
}

fn stop_live_agent_tree(
    workspace: &rimz::ResolvedWorkspace,
    store: &rimz::Store,
    globals: &GlobalFlags,
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    peers: &[&AgentState],
    agent: &AgentState,
    tracker: &mut StopTracker,
) -> Result<bool> {
    let key = (agent.kind.clone(), agent.agent_id.clone());
    if tracker.stopped.contains(&key) {
        return Ok(false);
    }

    let parent_label = rimz::address::agent_handle(agent, peers, true);
    let mut failures = Vec::new();
    for child in rimz::address::launched_children(&snapshot.agents, agent)
        .into_iter()
        .filter(|child| child.ended_at.is_none())
    {
        let child_label = rimz::address::agent_handle(child, peers, true);
        match stop_live_agent_tree(workspace, store, globals, snapshot, peers, child, tracker) {
            Ok(true) => writeln!(
                render::out(),
                "stopped {child_label} (subagent of {parent_label})"
            )?,
            Ok(false) => {}
            Err(err) => failures.push(format!("{child_label}: {err:#}")),
        }
    }

    match stop_live_agent(workspace, store, globals, agent) {
        Ok(()) => {
            tracker.stopped.insert(key);
            if let Err(err) = rimz::harness::schedule::arm::retire_session(
                &workspace.project_root,
                &agent.kind,
                &agent.agent_id,
                rimz::harness::schedule::arm::RetireScope::Session,
            ) {
                failures.push(format!("{parent_label}: {err}"));
            }
        }
        Err(err) => failures.push(format!("{parent_label}: {err:#}")),
    }
    if !failures.is_empty() {
        bail!(
            "one or more agents could not be stopped: {}",
            failures.join("; ")
        );
    }
    Ok(true)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopping_peer_uses_interactive_pane_path_without_canceling_turn() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = rimz::WorkspaceId::from_project_root(dir.path());
        let workspace = rimz::ResolvedWorkspace {
            workspace_id: workspace_id.clone(),
            project_root: dir.path().into(),
            cwd_project_root: None,
            root_class: rimz::workspace::RootClass::Directory,
            worktree_root: dir.path().into(),
            worktree_branch: None,
            session_name: "stop-test".into(),
            mux_hint: None,
        };
        let store = rimz::Store::open(
            rimz::StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap(),
            rimz::RuntimePaths::under(workspace_id.clone(), &dir.path().join("runtime")).unwrap(),
        )
        .unwrap();
        let globals = GlobalFlags {
            mux: None,
            zellij: false,
            tmux: false,
            root: None,
            color: crate::cli::ColorWhen::Never,
        };
        let mut peer = AgentState::stub("codex", "peer", rimz::agents::AgentStatus::Idle);
        peer.name = Some("peer".into());
        peer.launch_id = Some("peer-launch".into());
        for status in [
            rimz::store::run::RunStatus::Running,
            rimz::store::run::RunStatus::Completed,
        ] {
            let mut record = rimz::store::run::RunRecord::new(
                workspace_id.clone(),
                peer.kind.clone(),
                rimz::agents::PermissionMode::Auto,
                "task".into(),
                dir.path().into(),
            );
            record.peer = Some(rimz::store::run::PeerRun {
                launch_id: "peer-launch".into(),
                opened_by: Vec::new(),
            });
            record.status = status;
            rimz::harness::run::create(store.paths(), &record).unwrap();
            let error = stop_live_agent(&workspace, &store, &globals, &peer).unwrap_err();
            assert!(error.to_string().contains("has no bound pane"), "{error:#}");
            assert_eq!(
                rimz::harness::run::load(store.paths(), &record.run_id).unwrap(),
                record
            );
        }
    }
}
