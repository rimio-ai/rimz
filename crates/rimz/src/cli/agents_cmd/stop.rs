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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum WhenIdle {
    After(Duration),
    Off,
    Reference(String),
}

/// The rest a bare `--when-idle` waits for.
pub(super) const WHEN_IDLE_DEFAULT: &str = "3m";

pub(super) fn parse_when_idle(raw: &str) -> std::result::Result<WhenIdle, String> {
    if raw.starts_with('@') {
        return Ok(WhenIdle::Reference(raw.to_owned()));
    }
    if raw == "off" {
        return Ok(WhenIdle::Off);
    }
    supervised::parse_timeout(raw)
        .map(WhenIdle::After)
        .map_err(|err| format!("{err}; use `rimz agents stop <ref> --when-idle <duration>`"))
}

pub(super) fn normalize_stop(
    reference: Option<String>,
    when_idle: Option<WhenIdle>,
) -> Result<(String, Option<WhenIdle>)> {
    let usage = || {
        crate::cli::usage::UsageError::new(
            "name one reference: rimz agents stop <ref> --when-idle <duration>".to_owned(),
        )
    };
    match (reference, when_idle) {
        (None, None) => Err(crate::cli::usage::UsageError::new(
            "name one reference: rimz agents stop <ref>".to_owned(),
        )
        .into()),
        (None, Some(WhenIdle::Reference(reference))) => {
            let default = parse_when_idle(WHEN_IDLE_DEFAULT).map_err(anyhow::Error::msg)?;
            Ok((reference, Some(default)))
        }
        (None, _) | (Some(_), Some(WhenIdle::Reference(_))) => Err(usage().into()),
        (Some(reference), when_idle) => Ok((reference, when_idle)),
    }
}

/// The `-w` name that `agents <team>.<role> --continue` maps back to this seat's
/// recorded worktree; `None` for a project-root seat or a worktree whose path
/// that name does not reproduce.
fn resume_worktree_name<'a>(
    workspace: &rimz::ResolvedWorkspace,
    agent: &'a AgentState,
) -> Option<&'a str> {
    use rimz::utils::path::normalize_path_lexical;
    let recorded = agent.lane_path()?;
    let name = Path::new(recorded).file_name()?.to_str()?;
    let path = normalize_path_lexical(Path::new(recorded));
    let mapped = rimz::worktree::worktree_path(
        workspace.launch_repo_root(),
        &crate::cli::machine_config().agents.worktree,
        name,
    )
    .ok()?;
    (path != normalize_path_lexical(&workspace.project_root)
        && normalize_path_lexical(&mapped) == path)
        .then_some(name)
}

pub(super) fn stop_agent(
    reference: Option<String>,
    all: bool,
    when_idle: Option<WhenIdle>,
    globals: &GlobalFlags,
) -> Result<()> {
    let (reference, when_idle) = normalize_stop(reference, when_idle)?;
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
            let label = rimz::address::paste_address(agent, &snapshot, current_channel);
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
        crate::cli::resolve_agent_one(store, &snapshot, &reference, None, current_channel).map_err(
            |mut err| {
                if let Some(ambiguity) = err.downcast_mut::<render::AmbiguousAgents>() {
                    ambiguity.offer_stop_all();
                }
                err
            },
        );
    let live_agent = live_agent_result.as_ref().ok().copied();
    if let Some(live_agent) = live_agent {
        let peers = rimz::address::addressable_agents(&snapshot);
        let label = rimz::address::paste_address(live_agent, &snapshot, current_channel);
        stop_live_agent_tree(
            workspace,
            store,
            globals,
            &snapshot,
            &peers,
            live_agent,
            &mut StopTracker::default(),
        )?
        .into_result(&rimz::address::agent_handle(live_agent, &peers, true))?;
        let mut receipt = format!("stopped {label} ({})", live_agent.kind);
        if !live_agent.is_provider_subagent()
            && live_agent.parent_agent_id.is_none()
            && !live_agent.launch_depth.is_some_and(|depth| depth > 0)
            && agent_session_resumable(
                &live_agent.kind,
                &live_agent.agent_id,
                live_agent
                    .worktree_path
                    .as_deref()
                    .map_or(workspace.project_root.as_path(), Path::new),
            )
        {
            let command = if let (Some(team), Some(role)) = (&live_agent.team, &live_agent.role) {
                let spec = format!("{team}.{role}");
                let mut args = vec!["rimz", "agents", &spec, "--continue"];
                if let Some(name) = resume_worktree_name(workspace, live_agent) {
                    args.extend(["-w", name]);
                }
                shlex::try_join(args)?
            } else {
                let name = live_agent
                    .name
                    .as_deref()
                    .unwrap_or(live_agent.agent_id.as_str());
                shlex::try_join(["rimz", "agents", "resume", &format!("@{name}")])?
            };
            receipt.push_str(&format!("; resume: {command}"));
        }
        writeln!(render::out(), "{receipt}")?;
        return Ok(());
    }
    if let Some(run) =
        newest_run_by_ref(store, &reference, live_agent)?.filter(|run| run.peer.is_none())
    {
        supervised::stop_supervised_run(workspace, store, globals, &run)?;
        writeln!(
            render::out(),
            "stopped {} (run)",
            run.agent_name.as_deref().unwrap_or(run.run_id.as_str())
        )?;
        return Ok(());
    }
    let live_agent = live_agent_result?;
    close_agent_pane(workspace, store, live_agent)
}

/// Arm, replace, or withdraw the agent's soft stop; the room host and
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
        "{label} stops once idle for {duration} with nothing owed{replaced}; a prompt from the user withdraws it"
    )?;
    Ok(())
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
        close_agent_pane(workspace, store, agent)
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

fn close_agent_pane(
    workspace: &rimz::ResolvedWorkspace,
    store: &rimz::Store,
    agent: &AgentState,
) -> Result<()> {
    let refreshed;
    let agent = if agent.molted_from.is_some() && agent.agent_id.is_provisional() {
        let cwd = agent
            .worktree_path
            .as_deref()
            .map_or(workspace.worktree_root.as_path(), Path::new);
        if store.cancel_unbound_molt_launch(
            &agent.kind,
            &agent.agent_id,
            &workspace.session_name,
            cwd,
        )? {
            return Ok(());
        }
        refreshed = store.runtime_projection(rimz::RuntimeScope::Audit)?;
        rimz::address::launch_row(&refreshed.agents, &agent.kind, &agent.agent_id)
            .context("molt successor disappeared while stopping it")?
    } else {
        agent
    };
    let pane = agent
        .pane
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("agent {} has no bound pane", agent_name(agent)))?;
    rimz::harness::hibernate::wake_pane(store, &pane.pane_id)?;
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
