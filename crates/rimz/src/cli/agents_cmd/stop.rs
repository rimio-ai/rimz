use super::*;

use std::collections::HashSet;
use std::time::Duration;

use super::runs_lookup::{agent_name, newest_run_by_ref, newest_run_for_agent};
use crate::cli::render;
use rimz::store::snapshot::find_agent;

#[derive(Default)]
pub(in crate::cli) struct StopTracker {
    stopped: HashSet<(AgentKind, AgentSessionId)>,
}

/// What a tree stop did. The default is the skip: the tracker already took
/// this agent, so nothing closed and nothing failed.
#[must_use]
#[derive(Debug, Default)]
pub(in crate::cli) struct TreeStop {
    root_closed: bool,
    failures: Vec<String>,
}

impl TreeStop {
    /// Whether the agent's own pane closed.
    pub(in crate::cli) fn root_closed(&self) -> bool {
        self.root_closed
    }

    /// Everything that failed, joined; each part starts with the handle it
    /// concerns. `None` when nothing failed.
    pub(in crate::cli) fn error(&self) -> Option<String> {
        (!self.failures.is_empty()).then(|| self.failures.join("; "))
    }

    /// The per-agent lines of a stop over several agents; `true` when this
    /// agent had a failure.
    pub(in crate::cli) fn report(&self, out: &mut impl Write, label: &str) -> Result<bool> {
        if self.root_closed {
            writeln!(out, "stopped {label}")?;
        }
        let Some(error) = self.error() else {
            return Ok(false);
        };
        writeln!(out, "error {label}: {error}")?;
        Ok(true)
    }

    /// The stop of one agent as a command result.
    pub(in crate::cli) fn into_result(self, label: &str) -> Result<()> {
        match self.error() {
            None => Ok(()),
            Some(error) if self.root_closed => bail!("{label} stopped, but: {error}"),
            Some(error) => bail!("{label} was not stopped: {error}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WhenIdle {
    After(Duration),
    Off,
}

pub(super) fn parse_when_idle(raw: &str) -> std::result::Result<WhenIdle, String> {
    if raw == "off" {
        return Ok(WhenIdle::Off);
    }
    supervised::parse_timeout(raw)
        .map(WhenIdle::After)
        .map_err(|err| {
            format!(
                "{err}; --when-idle takes a duration (`90s`, `5m`) or `off`, so put the reference before the flag (`rimz agents stop @me --when-idle`)"
            )
        })
}

pub(super) fn stop_agent(
    reference: String,
    all: bool,
    when_idle: Option<WhenIdle>,
    globals: &GlobalFlags,
) -> Result<()> {
    if let Some(when_idle) = when_idle {
        return request_idle_stop(&reference, when_idle, globals);
    }
    let ctx = Ctx::open(globals)?;
    let (workspace, store) = (&ctx.workspace, &ctx.store);
    let snapshot = ctx.cached_snapshot()?;
    let current_channel = &ctx.address_context();
    if all && reference != "@me" {
        let agents = rimz::address::resolve_many(&snapshot, &reference, None, current_channel)?;
        let peers = rimz::address::addressable_agents(&snapshot);
        let mut tracker = StopTracker::default();
        let mut failed = false;
        let mut out = render::out();
        for agent in agents {
            let label = rimz::address::agent_handle(agent, &peers, true);
            failed |= stop_live_agent_tree(
                workspace,
                store,
                globals,
                &snapshot,
                &peers,
                agent,
                &mut tracker,
            )?
            .report(&mut out, &label)?;
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
        return stop_live_agent_tree(
            workspace,
            store,
            globals,
            &snapshot,
            &peers,
            live_agent,
            &mut StopTracker::default(),
        )?
        .into_result(&rimz::address::agent_handle(live_agent, &peers, true));
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

/// Arm, replace, or withdraw the agent's soft stop; the elected producer and
/// its helper act on it later.
fn request_idle_stop(reference: &str, when_idle: WhenIdle, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.cached_snapshot()?;
    let agent = match crate::cli::resolve_agent_one(
        &ctx.store,
        &snapshot,
        reference,
        None,
        &ctx.address_context(),
    ) {
        Ok(agent) => agent,
        Err(err) => {
            if newest_run_by_ref(&ctx.store, reference, None)?.is_some() {
                bail!(
                    "`{reference}` names a run, and --when-idle needs one live agent; stop the run with `rimz agents stop {reference}`"
                );
            }
            return Err(err);
        }
    };
    let peers = rimz::address::addressable_agents(&snapshot);
    let label = rimz::address::agent_handle(agent, &peers, true);
    let paths = ctx.store.paths();
    let pending = rimz::store::idle_stop::read(paths)
        .into_iter()
        .find(|request| request.kind == agent.kind && request.agent_id == agent.agent_id);
    let mut out = render::out();
    let WhenIdle::After(after) = when_idle else {
        if rimz::store::idle_stop::withdraw(paths, &agent.kind, &agent.agent_id)? {
            writeln!(out, "withdrew the pending idle stop for {label}")?;
        } else {
            writeln!(out, "{label} has no pending idle stop")?;
        }
        return Ok(());
    };
    if agent.ended_at.is_some() {
        bail!(
            "{label} has ended, and --when-idle needs one live agent; reopen it with `rimz agents resume` first"
        );
    }
    if agent.is_provider_subagent() {
        bail!(
            "{label} is a provider subagent and ends with its parent; request the stop on the parent"
        );
    }
    let requested_by = crate::cli::send::resolve_caller(&ctx.store)?
        .and_then(|caller| {
            rimz::harness::ancestry::resolve_launch_caller(&snapshot.agents, &caller).ok()
        })
        .map(|caller| rimz::address::agent_handle(caller, &peers, true));
    rimz::store::idle_stop::arm(
        paths,
        rimz::store::idle_stop::IdleStopRequest {
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            stop: rimz::agents::IdleStop {
                after_secs: after.as_secs(),
                requested_at: jiff::Timestamp::now(),
                requested_by,
            },
        },
    )?;
    let duration = rimz::harness::schedule::arm::duration_label(after);
    let replaced = pending
        .map(|request| {
            format!(
                " (replaces the pending {} request)",
                rimz::harness::schedule::arm::duration_label(Duration::from_secs(
                    request.stop.after_secs
                ))
            )
        })
        .unwrap_or_default();
    writeln!(
        out,
        "{label} stops once idle for {duration} with nothing owed{replaced}"
    )?;
    Ok(())
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
    if let Some(run) = stop_run(store, agent)? {
        Ok(supervised::stop_supervised_run(
            workspace, store, globals, &run,
        )?)
    } else {
        close_agent_pane(workspace, agent)
    }
}

/// The supervised run a stop of `agent` settles, when it has one.
pub(super) fn stop_run(store: &rimz::Store, agent: &AgentState) -> Result<Option<RunRecord>> {
    Ok(newest_run_for_agent(store, agent)?.filter(|run| run.peer.is_none()))
}

pub(in crate::cli) fn stop_resolved(
    ctx: &Ctx,
    globals: &GlobalFlags,
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    agent: &AgentState,
    tracker: &mut StopTracker,
) -> Result<TreeStop> {
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

/// Stop `agent` after its live launched children. A stop that fails is a
/// line in the returned value; `Err` is only a result line that could not be
/// written.
fn stop_live_agent_tree(
    workspace: &rimz::ResolvedWorkspace,
    store: &rimz::Store,
    globals: &GlobalFlags,
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    peers: &[&AgentState],
    agent: &AgentState,
    tracker: &mut StopTracker,
) -> Result<TreeStop> {
    let key = (agent.kind.clone(), agent.agent_id.clone());
    if tracker.stopped.contains(&key) {
        return Ok(TreeStop::default());
    }

    let parent_label = rimz::address::agent_handle(agent, peers, true);
    let mut stop = TreeStop::default();
    for child in rimz::address::launched_children(&snapshot.agents, agent)
        .into_iter()
        .filter(|child| child.ended_at.is_none())
    {
        let child_stop =
            stop_live_agent_tree(workspace, store, globals, snapshot, peers, child, tracker)?;
        if child_stop.root_closed {
            let child_label = rimz::address::agent_handle(child, peers, true);
            writeln!(
                render::out(),
                "stopped {child_label} (subagent of {parent_label})"
            )?;
        }
        stop.failures.extend(child_stop.failures);
    }

    if let Err(err) = stop_live_agent(workspace, store, globals, agent) {
        stop.failures.push(format!("{parent_label}: {err:#}"));
        return Ok(stop);
    }
    stop.root_closed = true;
    tracker.stopped.insert(key);
    if let Err(err) = rimz::harness::schedule::arm::retire_session(
        &workspace.project_root,
        &agent.kind,
        &agent.agent_id,
        rimz::harness::schedule::arm::RetireScope::Session,
    ) {
        stop.failures.push(format!("{parent_label}: {err}"));
    }
    Ok(stop)
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
