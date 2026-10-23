use super::*;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use super::runs_lookup::{agent_name, newest_run_by_ref, newest_run_for_agent};
use crate::cli::render;
use rimz::ids::PaneId;
use rimz::store::runtime::{AgentLiveness, OWNER_EXIT_BOUND, agent_liveness, await_owner_exits};
use rimz::store::snapshot::find_agent;

type AgentKey = (AgentKind, AgentSessionId);

#[derive(Default)]
struct StopTracker {
    stopped: HashSet<AgentKey>,
}

/// What a tree stop did. The default is the skip: the tracker already took
/// this agent, so nothing closed and nothing failed.
#[must_use]
#[derive(Debug, Default)]
pub(in crate::cli) struct TreeStop {
    root: Option<AgentKey>,
    /// Each row this tree closed, with the handle its failure lines use.
    closed: Vec<(AgentKey, Option<PaneId>, String)>,
    child_receipts: Vec<String>,
    still_running: Vec<(AgentKey, String, u32)>,
    failures: Vec<String>,
}

impl TreeStop {
    /// Whether the agent's own pane closed.
    pub(in crate::cli) fn root_closed(&self) -> bool {
        self.root.is_some()
    }

    pub(in crate::cli) fn root_still_running(&self) -> bool {
        self.still_running
            .iter()
            .any(|(key, _, _)| Some(key) == self.root.as_ref())
    }

    fn report_children(&self, out: &mut impl Write) -> Result<()> {
        for receipt in &self.child_receipts {
            writeln!(out, "{receipt}")?;
        }
        Ok(())
    }

    fn report_running(&self, out: &mut impl Write) -> Result<()> {
        for (_, label, pid) in &self.still_running {
            writeln!(
                out,
                "{label}: pane closed, but its process {pid} is still running; \
                 a resume refuses until it exits"
            )?;
        }
        Ok(())
    }

    /// Everything that failed, joined; each part starts with the handle it
    /// concerns. `None` when nothing failed.
    pub(in crate::cli) fn error(&self) -> Option<String> {
        (!self.failures.is_empty()).then(|| self.failures.join("; "))
    }

    /// The per-agent lines of a stop over several agents; `true` when this
    /// agent had a failure.
    pub(in crate::cli) fn report(&self, out: &mut impl Write, label: &str) -> Result<bool> {
        self.report_children(out)?;
        if self.root_closed() {
            writeln!(out, "stopped {label}")?;
        }
        self.report_running(out)?;
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
            Some(error) if self.root_closed() => bail!("{label} stopped, but: {error}"),
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
        let stops = stop_resolved(&ctx, globals, &snapshot, &agents)?;
        let mut failed = false;
        let mut out = render::out();
        for (agent, stop) in agents.into_iter().zip(stops) {
            let label = rimz::address::paste_address(agent, &snapshot, current_channel);
            failed |= stop.report(&mut out, &label)?;
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
        let stop = stop_resolved(&ctx, globals, &snapshot, &[live_agent])?.remove(0);
        let mut out = render::out();
        stop.report_children(&mut out)?;
        if stop.error().is_some() {
            stop.report_running(&mut out)?;
            return stop.into_result(&rimz::address::agent_handle(live_agent, &peers, true));
        }
        let mut receipt = format!("stopped {label} ({})", live_agent.kind);
        if !stop.root_still_running()
            && !live_agent.is_provider_subagent()
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
        writeln!(out, "{receipt}")?;
        stop.report_running(&mut out)?;
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
    live_agent_result.map(|_| ())
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
) -> Result<(AgentKey, Option<PaneId>)> {
    if let Some(run) = stop_run(store, agent)? {
        supervised::stop_supervised_run(workspace, store, globals, &run)?;
        Ok((
            (agent.kind.clone(), agent.agent_id.clone()),
            agent.pane.as_ref().map(|pane| pane.pane_id.clone()),
        ))
    } else {
        close_agent_pane(workspace, store, agent)
    }
}

/// The supervised run a stop of `agent` settles, when it has one.
pub(super) fn stop_run(store: &rimz::Store, agent: &AgentState) -> Result<Option<RunRecord>> {
    Ok(newest_run_for_agent(store, agent)?.filter(|run| run.peer.is_none()))
}

/// Stop every tree in `agents`, then wait once, up to the shared owner-exit
/// bound, for the processes of the rows it closed; it never signals them.
/// Returns one `TreeStop` per root, in order, each naming its rows still
/// running after a read taken past the wait.
pub(in crate::cli) fn stop_resolved(
    ctx: &Ctx,
    globals: &GlobalFlags,
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    agents: &[&AgentState],
) -> Result<Vec<TreeStop>> {
    let peers = rimz::address::addressable_agents(snapshot);
    let mut tracker = StopTracker::default();
    let mut stops = agents
        .iter()
        .map(|agent| {
            let current =
                find_agent(&snapshot.agents, &agent.kind, &agent.agent_id).unwrap_or(agent);
            stop_live_agent_tree(
                &ctx.workspace,
                &ctx.store,
                globals,
                snapshot,
                &peers,
                current,
                &mut tracker,
            )
        })
        .collect::<Vec<_>>();
    if tracker.stopped.is_empty() {
        return Ok(stops);
    }

    let deadline = Instant::now() + stop_owner_exit_bound();
    let projection = ctx.store.runtime_projection(rimz::RuntimeScope::Audit)?;
    let closed = stops
        .iter()
        .flat_map(|stop| &stop.closed)
        .filter_map(|(key, pane, _)| closed_row(&projection.agents, key, pane.as_ref()))
        .collect::<Vec<_>>();
    // An owner may change or record its end during the wait, so a wait that
    // slept decides from a fresh read.
    let refreshed = if await_owner_exits(&closed, deadline) {
        Some(ctx.store.runtime_projection(rimz::RuntimeScope::Audit)?)
    } else {
        None
    };
    let settled = refreshed.as_ref().unwrap_or(&projection);
    for stop in &mut stops {
        for (key, pane, label) in &stop.closed {
            let Some(agent) = closed_row(&settled.agents, key, pane.as_ref()) else {
                continue;
            };
            if let AgentLiveness::Live { pid } = agent_liveness(agent) {
                stop.still_running.push((key.clone(), label.clone(), pid));
            }
        }
    }
    Ok(stops)
}

fn stop_owner_exit_bound() -> Duration {
    #[cfg(any(test, feature = "testkit"))]
    if let Some(ms) = std::env::var("RIMZ_TEST_STOP_OWNER_EXIT_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
    {
        return Duration::from_millis(ms);
    }
    OWNER_EXIT_BOUND
}

/// Stop `agent` after its live launched children, collecting close and retirement failures.
fn stop_live_agent_tree(
    workspace: &rimz::ResolvedWorkspace,
    store: &rimz::Store,
    globals: &GlobalFlags,
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    peers: &[&AgentState],
    agent: &AgentState,
    tracker: &mut StopTracker,
) -> TreeStop {
    let key = (agent.kind.clone(), agent.agent_id.clone());
    if tracker.stopped.contains(&key) {
        return TreeStop::default();
    }

    let parent_label = rimz::address::agent_handle(agent, peers, true);
    let mut stop = TreeStop::default();
    for child in rimz::address::launched_children(&snapshot.agents, agent)
        .into_iter()
        .filter(|child| child.ended_at.is_none())
    {
        let child_stop =
            stop_live_agent_tree(workspace, store, globals, snapshot, peers, child, tracker);
        stop.child_receipts.extend(child_stop.child_receipts);
        if child_stop.root.is_some() {
            let child_label = rimz::address::agent_handle(child, peers, true);
            stop.child_receipts.push(format!(
                "stopped {child_label} (subagent of {parent_label})"
            ));
        }
        stop.closed.extend(child_stop.closed);
        stop.failures.extend(child_stop.failures);
    }

    let (closed, pane) = match stop_live_agent(workspace, store, globals, agent) {
        Ok(closed) => closed,
        Err(err) => {
            stop.failures.push(format!("{parent_label}: {err:#}"));
            return stop;
        }
    };
    stop.root = Some(closed.clone());
    stop.closed
        .push((closed.clone(), pane, parent_label.clone()));
    tracker.stopped.insert(closed);
    tracker.stopped.insert(key);
    if let Err(err) = rimz::harness::schedule::arm::retire_session(
        &workspace.project_root,
        &agent.kind,
        &agent.agent_id,
        rimz::harness::schedule::arm::RetireScope::Session,
    ) {
        stop.failures.push(format!("{parent_label}: {err}"));
    }
    stop
}

fn close_agent_pane(
    workspace: &rimz::ResolvedWorkspace,
    store: &rimz::Store,
    agent: &AgentState,
) -> Result<(AgentKey, Option<PaneId>)> {
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
            return Ok(((agent.kind.clone(), agent.agent_id.clone()), None));
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
    backend.close_pane(&workspace.session_name, &pane.pane_id)?;
    Ok((
        (agent.kind.clone(), agent.agent_id.clone()),
        Some(pane.pane_id.clone()),
    ))
}

/// The row a stop closed: the exact key, else (once a hook re-keyed it) the
/// launch's row on the closed pane. Never `launch_row` across panes, which can
/// pick a live peer of the same launch in another pane.
fn closed_row<'a>(
    agents: &'a [AgentState],
    (kind, id): &AgentKey,
    pane: Option<&PaneId>,
) -> Option<&'a AgentState> {
    find_agent(agents, kind, id).or_else(|| {
        let pane = pane?;
        rimz::address::launch_row(
            agents.iter().filter(|agent| {
                agent
                    .pane
                    .as_ref()
                    .is_some_and(|bound| &bound.pane_id == pane)
            }),
            kind,
            id,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_row_prefers_the_exact_ended_row_to_a_live_launch_peer() {
        let mut exact = AgentState::stub("claude", "closed", rimz::agents::AgentStatus::Success);
        exact.launch_id = Some("launch_shared".into());
        exact.ended_at = Some(jiff::Timestamp::now());
        let mut peer = exact.clone();
        peer.agent_id = "other".into();
        peer.ended_at = None;
        let key = (exact.kind.clone(), exact.agent_id.clone());
        let agents = [exact, peer];
        assert_eq!(
            closed_row(&agents, &key, None).map(|agent| agent.agent_id.as_str()),
            Some("closed")
        );
    }

    #[test]
    fn closed_row_follows_a_rekey_only_on_the_closed_pane() {
        let pane = rimz::ids::PaneId::from_parts(rimz::ids::MuxName::Zellij, "terminal_3");
        let mut successor =
            AgentState::stub("claude", "successor", rimz::agents::AgentStatus::Idle);
        successor.launch_id = Some("launch_closed".into());
        successor.pane = Some(rimz::pane::PaneRef::from_id(pane.clone()));
        let mut peer = successor.clone();
        peer.agent_id = "other".into();
        peer.pane = Some(rimz::pane::PaneRef::from_id(rimz::ids::PaneId::from_parts(
            rimz::ids::MuxName::Zellij,
            "terminal_4",
        )));
        let key = (successor.kind.clone(), "launch_closed".into());
        let agents = [successor, peer];
        assert_eq!(
            closed_row(&agents, &key, Some(&pane)).map(|agent| agent.agent_id.as_str()),
            Some("successor")
        );
        assert!(closed_row(&agents, &key, None).is_none());
        assert!(closed_row(&agents[1..], &key, Some(&pane)).is_none());
        let unrelated = (key.0.clone(), "unrelated".into());
        assert!(closed_row(&agents, &unrelated, Some(&pane)).is_none());
    }

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
