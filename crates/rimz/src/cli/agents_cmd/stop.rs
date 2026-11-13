use super::*;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use super::runs_lookup::{newest_run_by_ref, newest_run_for_agent};
use crate::cli::{render, select};
use rimz::ids::PaneId;
use rimz::sidebar::produce::hosting::{Hosting, HostingRead, read_hosting};
use rimz::store::runtime::{AgentLiveness, OWNER_EXIT_BOUND, agent_liveness};
use rimz::store::snapshot::find_agent;

type AgentKey = (AgentKind, AgentSessionId);

/// What a tree stop did. The default is the skip: the tracker already took
/// this agent, so nothing was stopped and nothing failed.
#[must_use]
#[derive(Debug, Default)]
pub(in crate::cli) struct TreeStop {
    root_stopped: bool,
    child_receipts: Vec<String>,
    failures: Vec<String>,
    /// Each row this tree closed or stopped, under the key it was asked for and the key
    /// it is now; a batch skips them when a later line names one.
    reached: Vec<AgentKey>,
}

impl TreeStop {
    /// Whether the agent itself was stopped: no pane hosts it and its process is gone,
    /// its unknown process has a recorded end, or its never-bound launch is cancelled.
    pub(in crate::cli) fn root_stopped(&self) -> bool {
        self.root_stopped
    }

    fn report_children(&self, out: &mut impl Write) -> Result<()> {
        for receipt in &self.child_receipts {
            writeln!(out, "{receipt}")?;
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
        if self.root_stopped() {
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
            Some(error) if self.root_stopped() => bail!("{label} stopped, but: {error}"),
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
pub(in crate::cli) fn resume_worktree_name<'a>(
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

pub(in crate::cli) fn offers_resume(
    workspace: &rimz::ResolvedWorkspace,
    agent: &AgentState,
) -> bool {
    !agent.is_provider_subagent()
        && agent.parent_agent_id.is_none()
        && !agent.launch_depth.is_some_and(|depth| depth > 0)
        && agent_session_resumable(
            &agent.kind,
            &agent.agent_id,
            agent
                .worktree_path
                .as_deref()
                .map_or(workspace.project_root.as_path(), Path::new),
        )
}

pub(in crate::cli) fn resume_command(
    reference: &str,
    team_role: Option<(&str, &str)>,
    worktree: Option<&str>,
) -> Vec<String> {
    let Some((team, role)) = team_role else {
        return vec![
            "agents".to_owned(),
            "resume".to_owned(),
            reference.to_owned(),
        ];
    };
    let mut args = vec![
        "agents".to_owned(),
        format!("{team}.{role}"),
        "--continue".to_owned(),
    ];
    if let Some(worktree) = worktree {
        args.extend(["-w".to_owned(), worktree.to_owned()]);
    }
    args
}

pub(super) fn stop_agent(
    reference: Option<String>,
    filters: Vec<String>,
    all: bool,
    when_idle: Option<WhenIdle>,
    globals: &GlobalFlags,
) -> Result<()> {
    if select::filter_help(&filters)? {
        return Ok(());
    }
    let references = Vec::from_iter(reference.clone());
    if select::selects(&references, &filters) {
        if when_idle.is_some() {
            return Err(crate::cli::usage::UsageError::new(
                "--when-idle takes one reference: rimz agents stop <ref> --when-idle <duration>"
                    .to_owned(),
            )
            .into());
        }
        let ctx = Ctx::open(globals)?;
        let flags = select::TargetFlags {
            references: &references,
            filters: &filters,
            all,
            room: false,
        };
        return match select::select_targets(&ctx, "stop", flags)? {
            select::Targets::Selected(selected) => stop_selected(&ctx, globals, selected),
            select::Targets::References(_) | select::Targets::Done => Ok(()),
        };
    }
    let (reference, when_idle) = normalize_stop(reference, when_idle)?;
    if let Some(when_idle) = when_idle {
        return request_idle_stop(&reference, when_idle, globals);
    }
    let ctx = Ctx::open(globals)?;
    let (workspace, store) = (&ctx.workspace, &ctx.store);
    let snapshot = ctx.cached_snapshot()?;
    let current_channel = &ctx.address_context();
    if all && reference != "@me" {
        let selected = rimz::address::resolve_many(&snapshot, &reference, None, current_channel)?
            .into_iter()
            .map(|agent| select::Selected {
                address: rimz::address::paste_address(agent, &snapshot, current_channel),
                agent: Some((agent.kind.clone(), agent.agent_id.clone())),
            })
            .collect();
        return stop_selected(&ctx, globals, selected);
    }
    let live_agent_result =
        crate::cli::resolve_agent_one(store, &snapshot, &reference, None, current_channel).map_err(
            |mut err| {
                if let Some(ambiguity) = err.downcast_mut::<render::AmbiguousAgents>() {
                    ambiguity.offer_all("stop");
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
            return stop.into_result(&rimz::address::agent_handle(live_agent, &peers, true));
        }
        let mut receipt = format!("stopped {label} ({})", live_agent.kind);
        if offers_resume(workspace, live_agent) {
            let name = live_agent
                .name
                .as_deref()
                .unwrap_or(live_agent.agent_id.as_str());
            let args = resume_command(
                &format!("@{name}"),
                live_agent.team.as_deref().zip(live_agent.role.as_deref()),
                resume_worktree_name(workspace, live_agent),
            );
            let command =
                shlex::try_join(std::iter::once("rimz").chain(args.iter().map(String::as_str)))?;
            receipt.push_str(&format!("; resume: {command}"));
        }
        writeln!(out, "{receipt}")?;
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

/// Stop the run a stdin line names, as a positional reference falls back to
/// one. `Err(None)` when it names no run either; `Err(Some(_))` when the lookup
/// or the stop failed.
fn stop_run_line(
    ctx: &Ctx,
    globals: &GlobalFlags,
    address: &str,
) -> std::result::Result<(), Option<anyhow::Error>> {
    let run = newest_run_by_ref(&ctx.store, address, None)
        .map_err(Some)?
        .filter(|run| run.peer.is_none())
        .ok_or(None)?;
    supervised::stop_supervised_run(&ctx.workspace, &ctx.store, globals, &run)
        .map_err(|err| Some(err.into()))
}

/// Stop each selected agent in turn, reading it again just before its stop,
/// with one receipt line per agent labelled by its selected address. An agent
/// an earlier tree in the batch already closed is skipped; a stdin line that
/// names no agent falls back to a run, as a positional reference does.
fn stop_selected(
    ctx: &Ctx,
    globals: &GlobalFlags,
    mut selected: Vec<select::Selected>,
) -> Result<()> {
    select::identify(ctx, &ctx.cached_snapshot()?, &mut selected);
    let mut closed = HashSet::new();
    let mut failed = 0;
    let mut out = render::out();
    for target in &selected {
        if target
            .agent
            .as_ref()
            .is_some_and(|key| closed.contains(key))
        {
            continue;
        }
        let snapshot = ctx.cached_snapshot()?;
        let agent = match select::reread(ctx, &snapshot, target) {
            Ok(agent) => agent,
            Err(err) => {
                let stopped = match target.agent {
                    Some(_) => Err(err),
                    None => stop_run_line(ctx, globals, &target.address)
                        .map_err(|run_err| run_err.unwrap_or(err)),
                };
                match stopped {
                    Ok(()) => writeln!(out, "stopped {} (run)", target.address)?,
                    Err(err) => {
                        writeln!(out, "error {}: {err:#}", target.address)?;
                        failed += 1;
                    }
                }
                continue;
            }
        };
        let stop = stop_resolved(ctx, globals, &snapshot, &[agent])?.remove(0);
        closed.extend(stop.reached.iter().cloned());
        if stop.report(&mut out, &target.address)? {
            failed += 1;
        }
    }
    select::batch_result(failed)
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

/// The supervised run a stop of `agent` settles, when it has one.
pub(super) fn stop_run(store: &rimz::Store, agent: &AgentState) -> Result<Option<RunRecord>> {
    Ok(newest_run_for_agent(store, agent)?.filter(|run| run.peer.is_none()))
}

/// Stop every tree in `agents`, children before parents, through the point-of-use lookup:
/// the only pane a stop closes is one a read this stop took says hosts the agent, and each
/// close is followed by a new read. After the last close it waits once, up to the shared
/// owner-exit bound, then sends one start-token-checked SIGTERM to each agent whose pane it
/// closed, or whose run it cancelled, and whose process outlived the wait with no pane
/// hosting it, and waits once more; it never sends SIGKILL.
/// Returns one `TreeStop` per root, in order, judged by a read taken after the waits: an
/// agent is stopped when no pane hosts it and its process is gone, its unknown process
/// has a recorded end, or its never-bound launch is cancelled. A first read that fails
/// stops nothing.
pub(in crate::cli) fn stop_resolved(
    ctx: &Ctx,
    globals: &GlobalFlags,
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    agents: &[&AgentState],
) -> Result<Vec<TreeStop>> {
    let stops = stop_resolved_with(ctx, globals, snapshot, agents, StopTerms::default())?;
    // Without `still_due` every stop goes ahead.
    Ok(stops.unwrap_or_default())
}

/// What a caller adds to [`stop_resolved`].
#[derive(Default)]
pub(in crate::cli) struct StopTerms<'a> {
    /// The run this stop settles: cancelled once the first read succeeds, and the agent it
    /// names then gets the backstop grace to leave before any close.
    pub(in crate::cli) run: Option<&'a RunRecord>,
    /// Re-checked after the first read: `false` stops nothing and returns `None`.
    pub(in crate::cli) still_due: Option<&'a dyn Fn() -> bool>,
}

/// [`stop_resolved`] under `terms`; `None` when `still_due` withdrew the stop.
pub(in crate::cli) fn stop_resolved_with(
    ctx: &Ctx,
    globals: &GlobalFlags,
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    agents: &[&AgentState],
    terms: StopTerms<'_>,
) -> Result<Option<Vec<TreeStop>>> {
    let peers = rimz::address::addressable_agents(snapshot);
    let read = match read_hosting(&ctx.workspace, &ctx.store, globals.mux) {
        Ok(mut read) => {
            render::room::print_notices(read.take_notices())?;
            read
        }
        Err(mut err) => {
            render::room::print_notices(std::mem::take(&mut err.notices))?;
            return Ok(Some(
                agents
                    .iter()
                    .map(|agent| TreeStop {
                        failures: vec![format!(
                            "{}: {err}; nothing was stopped",
                            rimz::address::agent_handle(agent, &peers, true)
                        )],
                        ..TreeStop::default()
                    })
                    .collect(),
            ));
        }
    };
    if terms.still_due.is_some_and(|still_due| !still_due()) {
        return Ok(None);
    }
    let run_cancelled = match terms.run.filter(|run| !run.status.is_terminal()) {
        Some(run) => {
            supervised::cancel_supervised_run(&ctx.store, run)?;
            true
        }
        None => false,
    };
    let mut stopper = Stopper {
        ctx,
        peers: &peers,
        read,
        run: terms.run,
        run_cancelled,
        tracker: HashSet::new(),
    };
    let mut trees = agents
        .iter()
        .map(|agent| {
            let current = stopper
                .read
                .agent(&agent.kind, &agent.agent_id)
                .unwrap_or(agent)
                .clone();
            let mut endings = Vec::new();
            stopper.end_tree(current, None, &mut endings);
            endings
        })
        .collect::<Vec<_>>();
    stopper.judge(trees.iter_mut().flatten().collect())?;
    Ok(Some(
        trees
            .into_iter()
            .map(|endings| stopper.receipt(endings))
            .collect(),
    ))
}

/// How long a stop waits for an agent's process to leave, and for a closed pane to leave
/// the listing: an exec wrapper holds both through two signal graces after the hangup.
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

/// The pause between reads while a stop waits for a closed pane to leave the listing.
const CLOSE_POLL: Duration = Duration::from_millis(100);
/// How long a stop leaves a pane it closed before closing it again while it stays listed.
const RECLOSE_AFTER: Duration = Duration::from_millis(500);
/// The pause between store reads while a stop waits for processes and recorded ends.
const ENDING_POLL: Duration = Duration::from_millis(50);

/// One agent of a stop: what this stop did to it, and its outcome once judged.
struct Ending {
    /// The key the stop was asked for; schedules retire under it.
    asked: AgentKey,
    /// The row the agent is now, after a re-key this stop followed.
    key: AgentKey,
    label: String,
    /// The handle of the agent whose stop reached this one as a launched child.
    parent: Option<String>,
    /// This stop issued a close of its pane, whatever the close returned.
    closed: bool,
    /// This stop cancelled the agent's run; a pane that then left counts as closed for the
    /// owner wait and the SIGTERM.
    cancelled: bool,
    /// The final read still found a pane hosting it.
    still_hosted: bool,
    /// A reservation no wrapper had bound and no pane hosted at this stop's first read.
    unbound_unhosted: bool,
    close_error: Option<String>,
    run: Option<RunRecord>,
    signaled: bool,
    outcome: Option<std::result::Result<(), String>>,
}

impl Ending {
    fn decide(&mut self, outcome: std::result::Result<(), String>) {
        self.outcome = Some(outcome);
    }

    /// Whether this stop is waiting on the agent's live process to leave.
    fn awaits_owner(&self) -> bool {
        self.closed || self.cancelled
    }
}

struct Stopper<'a> {
    ctx: &'a Ctx,
    peers: &'a [&'a AgentState],
    /// Every read after the first lists this read's backend.
    read: HostingRead,
    run: Option<&'a RunRecord>,
    run_cancelled: bool,
    tracker: HashSet<AgentKey>,
}

impl Stopper<'_> {
    /// Stop `agent` after its live launched children, pushing each ending in that order.
    fn end_tree(&mut self, agent: AgentState, parent: Option<String>, endings: &mut Vec<Ending>) {
        let key = (agent.kind.clone(), agent.agent_id.clone());
        if self.tracker.contains(&key) {
            return;
        }
        let label = rimz::address::agent_handle(&agent, self.peers, true);
        let children = rimz::address::launched_children(self.read.agents(), &agent)
            .into_iter()
            .filter(|child| child.ended_at.is_none())
            .cloned()
            .collect::<Vec<_>>();
        for child in children {
            self.end_tree(child, Some(label.clone()), endings);
        }
        let ending = self.end(&agent, key.clone(), label, parent);
        self.tracker.insert(key);
        self.tracker.insert(ending.key.clone());
        endings.push(ending);
    }

    fn end(
        &mut self,
        agent: &AgentState,
        key: AgentKey,
        label: String,
        parent: Option<String>,
    ) -> Ending {
        let mut ending = Ending {
            asked: key.clone(),
            key,
            label,
            parent,
            closed: false,
            cancelled: false,
            still_hosted: false,
            unbound_unhosted: agent.agent_id.is_provisional()
                && agent.runtime_owner.is_none()
                && self.read.hosting(&agent.kind, &agent.agent_id) == Hosting::Nowhere,
            close_error: None,
            run: None,
            signaled: false,
            outcome: None,
        };
        if let Err(reason) = self.act(agent, &mut ending) {
            ending.decide(Err(reason));
        }
        ending
    }

    fn act(&mut self, agent: &AgentState, ending: &mut Ending) -> std::result::Result<(), String> {
        let store = &self.ctx.store;
        let workspace = &self.ctx.workspace;
        if agent.molted_from.is_some() && agent.agent_id.is_provisional() {
            let cwd = agent
                .worktree_path
                .as_deref()
                .map_or(workspace.worktree_root.as_path(), Path::new);
            if store
                .cancel_unbound_molt_launch(
                    &agent.kind,
                    &agent.agent_id,
                    &workspace.session_name,
                    cwd,
                )
                .map_err(|err| format!("{err:#}"))?
            {
                ending.decide(Ok(()));
                return Ok(());
            }
        }
        // A run the caller passed is the one this stop settles for the agent it names;
        // `stop_run` matches by name as well, so it could pick another.
        let passed = self.run.filter(|run| run.matches_agent(agent));
        ending.cancelled = match passed {
            Some(_) => self.run_cancelled,
            None => {
                ending.run =
                    stop_run(store, agent).map_err(|err| format!("reading its runs: {err:#}"))?;
                match ending.run.as_ref().filter(|run| !run.status.is_terminal()) {
                    Some(run) => {
                        supervised::cancel_supervised_run(store, run)
                            .map_err(|err| format!("{err:#}"))?;
                        true
                    }
                    None => false,
                }
            }
        };
        if let Some(run) = passed {
            ending.run = Some(run.clone());
        }
        if ending.cancelled {
            self.await_departure(&mut ending.key)?;
        }
        self.close(ending)
    }

    /// Record a stopped subagent run's end, once its ending is judged stopped.
    fn stamp_end(&self, ending: &Ending) -> std::result::Result<(), String> {
        let Some(run) = &ending.run else {
            return Ok(());
        };
        supervised::stamp_stopped_subagent_end(
            &self.ctx.store,
            &self.ctx.workspace.session_name,
            run,
        )
        .map_err(|err| format!("recording its end failed: {err:#}"))
    }

    /// Where the agent is hosted in the current read, following a re-key first.
    fn hosting(&self, key: &mut AgentKey) -> Hosting {
        if let Some(followed) = follow(self.read.agents(), key) {
            *key = followed;
        }
        self.read.hosting(&key.0, &key.1)
    }

    fn refresh(&mut self) -> std::result::Result<(), String> {
        match read_hosting(&self.ctx.workspace, &self.ctx.store, Some(self.read.mux())) {
            Ok(mut read) => {
                let _ = render::room::print_notices(read.take_notices());
                self.read = read;
                Ok(())
            }
            Err(mut err) => {
                let _ = render::room::print_notices(std::mem::take(&mut err.notices));
                Err(err.to_string())
            }
        }
    }

    /// A pane whose run this stop cancelled gets the backstop grace to leave on its own.
    fn await_departure(&mut self, key: &mut AgentKey) -> std::result::Result<(), String> {
        let deadline = Instant::now() + supervised::pane::STOP_BACKSTOP_GRACE;
        while self.hosting(key) != Hosting::Nowhere && Instant::now() < deadline {
            std::thread::sleep(CLOSE_POLL);
            self.refresh()?;
        }
        Ok(())
    }

    /// Close whatever pane each new read says hosts the agent, until none does or the
    /// owner-exit bound passes after the first close. A pane still listed is closed again
    /// once `RECLOSE_AFTER` has passed since its last close.
    fn close(&mut self, ending: &mut Ending) -> std::result::Result<(), String> {
        let mut deadline = Instant::now() + stop_owner_exit_bound();
        let mut last: Option<(PaneId, Instant)> = None;
        loop {
            let Hosting::Pane {
                pane_id,
                session_name,
            } = self.hosting(&mut ending.key)
            else {
                return Ok(());
            };
            let now = Instant::now();
            if now >= deadline {
                return Ok(());
            }
            if last
                .as_ref()
                .is_none_or(|(closed, at)| closed != &pane_id || now >= *at + RECLOSE_AFTER)
            {
                rimz::harness::hibernate::wake_pane(&self.ctx.store, &pane_id)
                    .map_err(|err| format!("{err:#}"))?;
                self.refresh()?;
                if self.hosting(&mut ending.key)
                    != (Hosting::Pane {
                        pane_id: pane_id.clone(),
                        session_name: session_name.clone(),
                    })
                {
                    continue;
                }
                let closed_at = Instant::now();
                if last.is_none() {
                    deadline = closed_at + stop_owner_exit_bound();
                }
                let result =
                    rimz::mux::backend_for(self.read.mux()).close_pane(&session_name, &pane_id);
                ending.closed = true;
                ending.close_error = result.err().map(|err| format!("{err:#}"));
                last = Some((pane_id, closed_at));
            }
            std::thread::sleep(CLOSE_POLL);
            self.refresh()?;
        }
    }

    /// Wait once for the batch, signal each closed agent whose process outlived it, wait
    /// once more for those, and judge every ending not yet decided from the final reads.
    fn judge(&mut self, mut endings: Vec<&mut Ending>) -> Result<()> {
        endings.retain(|ending| ending.outcome.is_none());
        if endings.is_empty() {
            return Ok(());
        }
        let bound = stop_owner_exit_bound();
        let mut projection = self.await_endings(&mut endings, Instant::now() + bound)?;
        let mut read = self.refresh();
        let mut signaled = false;
        if read.is_ok() {
            for ending in endings.iter_mut().filter(|ending| ending.awaits_owner()) {
                if self.hosting(&mut ending.key) != Hosting::Nowhere {
                    continue;
                }
                let Some(owner) = row(self.read.agents(), &mut ending.key)
                    .filter(|row| matches!(agent_liveness(row), AgentLiveness::Live { .. }))
                    .and_then(|row| row.runtime_owner.as_ref())
                    .filter(|owner| owner.process_start.is_some())
                else {
                    continue;
                };
                ending.signaled = rimz::child_process::signal_process_term(
                    owner.pid,
                    owner.process_start.as_deref(),
                );
                signaled |= ending.signaled;
            }
        }
        if signaled {
            let mut termed = endings
                .iter_mut()
                .filter(|ending| ending.signaled)
                .map(|ending| &mut **ending)
                .collect::<Vec<_>>();
            projection = self.await_endings(&mut termed, Instant::now() + bound)?;
            read = self.refresh();
        }
        if read.is_ok() {
            for ending in endings.iter_mut().filter(|ending| ending.unbound_unhosted) {
                if self.hosting(&mut ending.key) != Hosting::Nowhere {
                    continue;
                }
                let Some(agent) = row(&projection.agents, &mut ending.key) else {
                    continue;
                };
                let cwd = agent
                    .worktree_path
                    .as_deref()
                    .map_or(self.ctx.workspace.worktree_root.as_path(), Path::new);
                if let Err(err) = self.ctx.store.cancel_unbound_agent_launch(
                    &ending.key.0,
                    &ending.key.1,
                    &self.ctx.workspace.session_name,
                    cwd,
                ) {
                    ending.decide(Err(format!("cancelling its launch failed: {err:#}")));
                }
            }
            read = self.refresh();
            projection = self
                .ctx
                .store
                .runtime_projection(rimz::RuntimeScope::Audit)?;
        }
        for ending in endings {
            if ending.outcome.is_some() {
                continue;
            }
            let outcome = match &read {
                Ok(()) => self
                    .verdict(ending, &projection.agents)
                    .and_then(|()| self.stamp_end(ending)),
                Err(err) => Err(err.clone()),
            };
            ending.decide(outcome);
        }
        Ok(())
    }

    /// Wait until no ending has a process this stop is waiting on (a live one whose pane it
    /// closed, or an unknown one whose end is not recorded), or `deadline` passes.
    fn await_endings(
        &self,
        endings: &mut [&mut Ending],
        deadline: Instant,
    ) -> Result<rimz::store::runtime::RuntimeProjection> {
        loop {
            let projection = self
                .ctx
                .store
                .runtime_projection(rimz::RuntimeScope::Audit)?;
            let waiting = endings.iter_mut().any(|ending| {
                let closed = ending.awaits_owner();
                row(&projection.agents, &mut ending.key).is_some_and(|row| {
                    match agent_liveness(row) {
                        AgentLiveness::Live { .. } => closed,
                        AgentLiveness::Unknown => {
                            !failed_unbound_launch(row)
                                && (row.ended_at.is_none()
                                    || (ending.unbound_unhosted
                                        && row.agent_id.is_provisional()
                                        && row.runtime_owner.is_none()))
                        }
                        AgentLiveness::Dead => false,
                    }
                })
            });
            if !waiting || Instant::now() >= deadline {
                return Ok(projection);
            }
            std::thread::sleep(ENDING_POLL);
        }
    }

    fn verdict(
        &self,
        ending: &mut Ending,
        agents: &[AgentState],
    ) -> std::result::Result<(), String> {
        if let Hosting::Pane { pane_id, .. } = self.hosting(&mut ending.key) {
            ending.still_hosted = true;
            let close = ending
                .close_error
                .as_ref()
                .map(|err| format!("; closing it failed: {err}"))
                .unwrap_or_default();
            return Err(format!("a pane still hosts it: {pane_id}{close}"));
        }
        unhosted_verdict(
            row(agents, &mut ending.key),
            ending.awaits_owner(),
            ending.signaled,
        )
    }

    /// One root's receipt; schedules retire for each agent this stop stopped, or whose
    /// pane it closed and no longer finds.
    fn receipt(&self, endings: Vec<Ending>) -> TreeStop {
        let mut stop = TreeStop::default();
        for ending in endings {
            // `judge` decides every ending before a receipt is built, so an undecided one
            // is impossible; it still never reads as stopped.
            let outcome = ending
                .outcome
                .unwrap_or_else(|| Err("its stop was never judged".to_owned()));
            match (&ending.parent, &outcome) {
                (Some(parent), Ok(())) => stop
                    .child_receipts
                    .push(format!("stopped {} (subagent of {parent})", ending.label)),
                (None, Ok(())) => stop.root_stopped = true,
                (_, Err(reason)) => stop.failures.push(format!("{}: {reason}", ending.label)),
            }
            if ending.closed || outcome.is_ok() {
                stop.reached.push(ending.asked.clone());
                stop.reached.push(ending.key.clone());
            }
            if outcome.is_err() && !(ending.closed && !ending.still_hosted) {
                continue;
            }
            let (kind, session) = &ending.asked;
            if let Err(err) = rimz::harness::schedule::arm::retire_session(
                &self.ctx.workspace.project_root,
                kind,
                session,
                rimz::harness::schedule::arm::RetireScope::Session,
            ) {
                stop.failures.push(format!("{}: {err}", ending.label));
            }
        }
        stop
    }
}

/// With no hosting pane, only a known-dead owner, a recorded end, or a cancelled
/// reservation proves the stop. A missing row (including an ambiguous re-key) proves none.
fn unhosted_verdict(
    agent: Option<&AgentState>,
    closed: bool,
    signaled: bool,
) -> std::result::Result<(), String> {
    let unconfirmed = || {
        Err(
            "RimZ could not confirm its process is gone; the stop succeeds once the agent's \
             end is recorded"
                .to_owned(),
        )
    };
    let Some(agent) = agent else {
        return unconfirmed();
    };
    match agent_liveness(agent) {
        AgentLiveness::Dead => Ok(()),
        AgentLiveness::Live { pid } if signaled => Err(format!(
            "its process {pid} is still running after RimZ sent it SIGTERM"
        )),
        AgentLiveness::Live { pid } if closed => Err(format!(
            "its process {pid} is still running after its pane closed"
        )),
        AgentLiveness::Live { pid } => Err(format!(
            "RimZ found no pane for it in this room and did not signal its process {pid}, \
             which is still running"
        )),
        AgentLiveness::Unknown if agent.ended_at.is_some() => Ok(()),
        AgentLiveness::Unknown if failed_unbound_launch(agent) => Ok(()),
        AgentLiveness::Unknown => unconfirmed(),
    }
}

fn failed_unbound_launch(agent: &AgentState) -> bool {
    agent.agent_id.is_provisional()
        && agent.runtime_owner.is_none()
        && agent.status == rimz::agents::AgentStatus::Failed
}

/// The row `key` names, after following a re-key.
fn row<'a>(agents: &'a [AgentState], key: &mut AgentKey) -> Option<&'a AgentState> {
    if let Some(followed) = follow(agents, key) {
        *key = followed;
    }
    find_agent(agents, &key.0, &key.1)
}

/// The row a provisional key moved to when a hook re-keyed it: the one unended row of its
/// kind holding that launch id. `None` while the key still names a row, and whenever no
/// row or more than one could be it.
fn follow(agents: &[AgentState], (kind, id): &AgentKey) -> Option<AgentKey> {
    if !id.is_provisional() || find_agent(agents, kind, id).is_some() {
        return None;
    }
    let mut adopters = agents.iter().filter(|agent| {
        &agent.kind == kind && agent.ended_at.is_none() && agent.launch_id.as_ref() == Some(id)
    });
    let adopter = adopters.next()?;
    adopters
        .next()
        .is_none()
        .then(|| (adopter.kind.clone(), adopter.agent_id.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provisional_key() -> AgentKey {
        (AgentKind::new_unchecked("claude"), "launch_closed".into())
    }

    #[test]
    fn a_missing_final_row_is_not_evidence_that_the_agent_stopped() {
        assert_eq!(
            unhosted_verdict(None, true, false),
            Err(
                "RimZ could not confirm its process is gone; the stop succeeds once the agent's \
                 end is recorded"
                    .to_owned()
            )
        );
    }

    #[test]
    fn follow_keeps_a_key_that_still_names_a_row() {
        let mut exact = AgentState::stub(
            "claude",
            "launch_closed",
            rimz::agents::AgentStatus::Success,
        );
        exact.ended_at = Some(jiff::Timestamp::now());
        let mut adopter = AgentState::stub("claude", "adopter", rimz::agents::AgentStatus::Idle);
        adopter.launch_id = Some("launch_closed".into());
        assert_eq!(follow(&[exact, adopter], &provisional_key()), None);
    }

    #[test]
    fn follow_moves_a_provisional_key_only_to_its_one_unended_adopter() {
        let mut adopter = AgentState::stub("claude", "adopter", rimz::agents::AgentStatus::Idle);
        adopter.launch_id = Some("launch_closed".into());
        let followed = follow(std::slice::from_ref(&adopter), &provisional_key());
        assert_eq!(followed.map(|(_, id)| id), Some("adopter".into()));

        let mut peer = adopter.clone();
        peer.agent_id = "peer".into();
        assert_eq!(
            follow(&[adopter.clone(), peer.clone()], &provisional_key()),
            None
        );

        peer.ended_at = Some(jiff::Timestamp::now());
        let followed = follow(&[adopter.clone(), peer], &provisional_key());
        assert_eq!(followed.map(|(_, id)| id), Some("adopter".into()));

        let real = (AgentKind::new_unchecked("claude"), "real-session".into());
        let mut namesake = adopter;
        namesake.launch_id = Some("real-session".into());
        assert_eq!(
            follow(&[namesake], &real),
            None,
            "only a provisional key moves"
        );
    }

    #[test]
    fn a_stop_never_settles_a_peer_run() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = rimz::WorkspaceId::from_project_root(dir.path());
        let store = rimz::Store::open(
            rimz::StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap(),
            rimz::RuntimePaths::under(workspace_id.clone(), &dir.path().join("runtime")).unwrap(),
        )
        .unwrap();
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
            assert!(stop_run(&store, &peer).unwrap().is_none());
            assert_eq!(
                rimz::harness::run::load(store.paths(), &record.run_id).unwrap(),
                record
            );
        }
    }
}
