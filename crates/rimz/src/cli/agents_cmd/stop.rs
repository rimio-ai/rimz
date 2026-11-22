use super::*;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use super::runs_lookup::{newest_run_by_ref, newest_run_for_agent};
use crate::cli::{render, select};
use rimz::agents::state::session_row;
use rimz::harness::stop_booking::{StopBooking, book_stop};
use rimz::ids::PaneId;
use rimz::sidebar::produce::hosting::{Hosting, HostingRead, read_hosting};
use rimz::store::runtime::{AgentLiveness, OWNER_EXIT_BOUND, agent_liveness};

type AgentKey = (AgentKind, AgentSessionId);

/// What a tree stop did. The default is the skip: the tracker already took
/// this agent, so nothing was stopped and nothing failed.
#[must_use]
#[derive(Debug, Default)]
pub(in crate::cli) struct TreeStop {
    root_stopped: bool,
    /// The session the stop stopped for the root: its holder when a swap replaced it.
    root: Option<AgentState>,
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

    /// The session stopped for the root, once [`TreeStop::root_stopped`].
    pub(in crate::cli) fn root(&self) -> Option<&AgentState> {
        self.root.as_ref()
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
pub(super) const WHEN_IDLE_DEFAULT: &str = rimz::harness::stop_booking::IDLE_REST_DEFAULT;

pub(super) fn parse_when_idle(raw: &str) -> std::result::Result<WhenIdle, String> {
    if raw.starts_with('@') {
        return Ok(WhenIdle::Reference(raw.to_owned()));
    }
    match rimz::harness::stop_booking::parse_idle_rest(raw) {
        Ok(Some(after)) => Ok(WhenIdle::After(after)),
        Ok(None) => Ok(WhenIdle::Off),
        Err(err) => Err(format!(
            "{err}; use `rimz agents stop <ref> --when-idle <duration>`"
        )),
    }
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
        let label = run.agent_name.as_deref().unwrap_or(run.run_id.as_str());
        let stop = stop_run_agent(&ctx, globals, &run)?;
        let mut out = render::out();
        stop.report_children(&mut out)?;
        if stop.error().is_some() {
            return stop.into_result(label);
        }
        writeln!(out, "stopped {label} (run)")?;
        return Ok(());
    }
    live_agent_result.map(|_| ())
}

/// Stop the run a stdin line names, as a positional reference falls back to
/// one. `Err(None)` when it names no run either; `Err(Some(_))` when the lookup
/// or the run's cancel failed.
fn stop_run_line(
    ctx: &Ctx,
    globals: &GlobalFlags,
    address: &str,
) -> std::result::Result<TreeStop, Option<anyhow::Error>> {
    let run = newest_run_by_ref(&ctx.store, address, None)
        .map_err(Some)?
        .filter(|run| run.peer.is_none())
        .ok_or(None)?;
    stop_run_agent(ctx, globals, &run).map_err(Some)
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
                    Ok(stop) => {
                        closed.extend(stop.reached.iter().cloned());
                        stop.report_children(&mut out)?;
                        match stop.error() {
                            None => writeln!(out, "stopped {} (run)", target.address)?,
                            Some(error) => {
                                writeln!(out, "error {}: {error}", target.address)?;
                                failed += 1;
                            }
                        }
                    }
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
    let after = match when_idle {
        WhenIdle::After(after) => Some(after),
        WhenIdle::Off | WhenIdle::Reference(_) => None,
    };
    let booking = book_stop(ctx.store.paths(), &snapshot, agent, after, || {
        crate::cli::send::resolve_caller(&ctx.store)
    })?;
    let mut out = render::out();
    let (after_secs, replaces_after_secs) = match booking {
        StopBooking::Withdrawn => {
            writeln!(out, "withdrew the pending idle stop for {label}")?;
            return Ok(());
        }
        StopBooking::NonePending => {
            writeln!(out, "{label} has no pending idle stop")?;
            return Ok(());
        }
        StopBooking::Booked {
            after_secs,
            replaces_after_secs,
        } => (after_secs, replaces_after_secs),
    };
    let duration = rimz::harness::schedule::arm::duration_label(Duration::from_secs(after_secs));
    let replaced = replaces_after_secs
        .map(|after_secs| {
            format!(
                " (replaces the pending {} request)",
                rimz::harness::schedule::arm::duration_label(Duration::from_secs(after_secs))
            )
        })
        .unwrap_or_default();
    let reader = ctx.address_context();
    let you = super::report::is_me(&ctx.store, &snapshot, &reader, agent);
    let prefix = super::report::rimz_for(&ctx.workspace.project_root);
    // The snapshot `agents show` reads, so both list the holding rows in one order.
    let booked = ctx.published_snapshot();
    let now = booked
        .as_ref()
        .map_or_else(|_| jiff::Timestamp::now(), |booked| booked.now);
    let started = booked.and_then(|booked| {
        let shown = session_row(&booked.agents, agent)
            .ok_or_else(|| anyhow::anyhow!("{label} left the room snapshot"))?;
        let seat = rimz::harness::idle_stop::started::Seat {
            root: &ctx.workspace.project_root,
            reader: &reader,
            you,
            rimz: &prefix,
        };
        Ok(rimz::harness::idle_stop::started::started(
            &ctx.store, &booked, shown, &seat, now,
        )?)
    });
    let subject = (!you).then_some(label.as_str());
    write_idle_receipt(
        &mut out,
        &mut render::err(),
        subject,
        &duration,
        &replaced,
        started,
        now,
    )
}

/// The `--when-idle` receipt: the booking, then what holds the stop and what the stop
/// reaches, read after the booking was written. `subject` is `None` when the reader is the
/// agent. A failed read warns on `err` and leaves the booking standing.
pub(super) fn write_idle_receipt(
    out: &mut impl Write,
    err: &mut impl Write,
    subject: Option<&str>,
    duration: &str,
    replaced: &str,
    started: Result<rimz::harness::idle_stop::started::Started>,
    now: jiff::Timestamp,
) -> Result<()> {
    let (headings, first_line) = match subject {
        None => (
            [
                "holding your stop",
                "closing with you",
                "still running after you close",
            ],
            format!("you close after {duration} idle{replaced}, once nothing holds you"),
        ),
        Some(subject) => (
            [
                "holding its stop",
                "closing with it",
                "still running after it closes",
            ],
            format!("{subject} closes after {duration} idle{replaced}, once nothing holds it"),
        ),
    };
    writeln!(
        out,
        "stop booked: {first_line}. A prompt from the user withdraws it."
    )?;
    match started {
        Ok(started) => super::report::render_started(
            out,
            &started,
            headings,
            super::report::StartedPlace::Receipt,
            now,
        )?,
        Err(read) => super::report::warn_started_unread(err, "the stop is booked, but ", &read)?,
    }
    Ok(())
}

/// The supervised run a stop of `agent` settles, when it has one.
pub(super) fn stop_run(store: &rimz::Store, agent: &AgentState) -> Result<Option<RunRecord>> {
    Ok(newest_run_for_agent(store, agent)?.filter(|run| run.peer.is_none()))
}

/// Stop every tree in `agents`, children before parents, through the point-of-use lookup:
/// the only pane a stop closes is one a read this stop took says hosts the agent, read again
/// before the close whenever the stop did work since that read (cancelled a swap or a run,
/// waited on a departure, thawed the pane, or closed a pane), and each close is followed by
/// a new read. After the last close it waits once, up to the shared
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
    /// names then gets the backstop grace to leave before any close, unless the run is kept.
    pub(in crate::cli) run: Option<&'a RunRecord>,
    /// Re-checked after the first read: `false` stops nothing and returns `None`.
    pub(in crate::cli) still_due: Option<&'a dyn Fn() -> bool>,
    /// The first read, a hosting read the caller took with no work since, in place of the
    /// stop's own.
    pub(in crate::cli) read: Option<HostingRead>,
    /// Which tab, if any, the stop closes with the panes it closes. A sidebar card key sets it.
    pub(in crate::cli) tab: TabClose,
}

/// The tab close of [`StopTerms::tab`].
#[derive(Default)]
pub(in crate::cli) enum TabClose {
    /// Close panes alone.
    #[default]
    Never,
    /// Close the tab with a pane when the read gating the close finds the pane the last
    /// working pane of its tab and this process runs in no pane of it.
    WhenLast,
    /// Every working pane of one tab is a target: its `panes`, as the caller listed them,
    /// of which `loose` host no agent the stop is given. A pane of them hosting an agent
    /// stays open while the stop ends that agent, cancels and departure wait included; once
    /// every agent has been ended, the stop closes the tab in one command, gated as a pane
    /// close is, and polls each such agent as after a pane close. When the gating read gives
    /// the tab no anchor (`HostingRead::tab_anchor`), or an agent's stop failed before its
    /// close, each such agent's pane and each of `loose` closes on its own instead, and a
    /// failed agent's pane stays.
    Whole {
        panes: Vec<PaneId>,
        loose: Vec<PaneId>,
    },
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
    let read = terms
        .read
        .map_or_else(|| read_hosting(&ctx.workspace, &ctx.store, globals.mux), Ok);
    let read = match read {
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
    let stopper = Stopper {
        ctx,
        peers: &peers,
        read,
        fresh: !run_cancelled,
        tab: terms.tab,
        run: terms.run,
        run_cancelled,
        tracker: HashSet::new(),
    };
    stop_with_read(stopper, agents).map(Some)
}

/// Stop the agent the supervised `run` names, settling the run. The run is cancelled first,
/// before any multiplexer read; `Err` means that cancel failed. Its agent is the holder of
/// the run's launch, else the row of its exact session; never one found by name, and never a
/// row that holds another launch now. A run that names no such row is done once cancelled.
/// Otherwise the agent's tree is stopped as a stop of that agent is, with the run's agent awaited as one whose run this stop cancelled: the
/// backstop grace (unless the run is kept) and the owner wait apply even when the run had
/// already ended.
pub(in crate::cli) fn stop_run_agent(
    ctx: &Ctx,
    globals: &GlobalFlags,
    run: &RunRecord,
) -> Result<TreeStop> {
    stop_run_tree(ctx, globals, run, None)
}

/// [`stop_run_agent`], stopping `agent` in place of the agent the run names when given.
pub(super) fn stop_run_tree(
    ctx: &Ctx,
    globals: &GlobalFlags,
    run: &RunRecord,
    agent: Option<&AgentState>,
) -> Result<TreeStop> {
    supervised::cancel_supervised_run(&ctx.store, run)?;
    if agent.is_none() && run.agent_launch_id().is_none() && run.agent_id.is_none() {
        return Ok(TreeStop {
            root_stopped: true,
            ..TreeStop::default()
        });
    }
    let read = match read_hosting(&ctx.workspace, &ctx.store, globals.mux) {
        Ok(mut read) => {
            render::room::print_notices(read.take_notices())?;
            read
        }
        Err(mut err) => {
            render::room::print_notices(std::mem::take(&mut err.notices))?;
            let label = run.agent_name.as_deref().unwrap_or(run.run_id.as_str());
            let settled = if run.status.is_terminal() {
                "had already ended"
            } else {
                "is canceled"
            };
            return Ok(TreeStop {
                failures: vec![format!(
                    "{label}: the run {settled}, but {err}; no pane was closed"
                )],
                ..TreeStop::default()
            });
        }
    };
    // A launch's holder may be a row that holds another launch now (a fresh launch resumed the
    // session); it is not the run's agent, so that stop is the cancel alone.
    let agent = match (agent, run.agent_launch_id(), &run.agent_id) {
        (Some(agent), ..) => Some(agent.clone()),
        (None, Some(launch), _) => read
            .agent_hosting(&run.kind, launch)
            .holder
            .filter(|row| run.matches_agent(row))
            .cloned(),
        (None, None, Some(session)) => read.agent(&run.kind, session).cloned(),
        (None, None, None) => None,
    };
    let Some(agent) = agent else {
        return Ok(TreeStop {
            root_stopped: true,
            ..TreeStop::default()
        });
    };
    let snapshot = ctx.cached_snapshot()?;
    let peers = rimz::address::addressable_agents(&snapshot);
    let stopper = Stopper {
        ctx,
        peers: &peers,
        read,
        fresh: true,
        tab: TabClose::Never,
        run: Some(run),
        run_cancelled: true,
        tracker: HashSet::new(),
    };
    Ok(stop_with_read(stopper, &[&agent])?.remove(0))
}

/// Stop every tree in `agents` through `stopper`, which holds the first read.
fn stop_with_read(mut stopper: Stopper<'_>, agents: &[&AgentState]) -> Result<Vec<TreeStop>> {
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
    let tab = stopper.close_whole_tab(trees.iter_mut().flatten().collect());
    stopper.judge(trees.iter_mut().flatten().collect())?;
    tab.map_err(anyhow::Error::msg)?;
    Ok(trees
        .into_iter()
        .map(|endings| stopper.receipt(endings))
        .collect())
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
    /// The session this stop acts on, as last read: after the swap cancellation the agent's
    /// holder unless the asked session still holds it, and after a re-key the row that
    /// adopted its provisional key.
    session: AgentState,
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
    /// Its pane awaits the tab close of [`TabClose::Whole`].
    awaits_tab: bool,
    /// A reservation no wrapper had bound and no pane hosted at this stop's first read.
    unbound_unhosted: bool,
    close_error: Option<String>,
    run: Option<RunRecord>,
    /// The run the caller passed is this agent's: the stop settles it for this session.
    holds_run: bool,
    /// A fresh launch resumed the session since, so the run's agent is hosted nowhere and
    /// nothing of that launch is this stop's to close, signal, or stamp.
    left_run: bool,
    signaled: bool,
    outcome: Option<std::result::Result<(), String>>,
}

impl Ending {
    /// The key of the session this stop acts on.
    fn key(&self) -> AgentKey {
        (self.session.kind.clone(), self.session.agent_id.clone())
    }

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
    /// No work of this stop (a cancel, a wait, a thaw, a close) sat between `read` and now,
    /// so `read` may gate a close.
    fresh: bool,
    tab: TabClose,
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
        self.tracker.insert(ending.key());
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
            asked: key,
            session: agent.clone(),
            label,
            parent,
            closed: false,
            cancelled: false,
            still_hosted: false,
            awaits_tab: false,
            unbound_unhosted: agent.agent_id.is_provisional()
                && agent.runtime_owner.is_none()
                && self.read.session_hosting(&agent.kind, &agent.agent_id) == Hosting::Nowhere,
            close_error: None,
            run: None,
            holds_run: false,
            left_run: false,
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
        // A restart or molt still starting would otherwise bind its replacement
        // after the stop; cancel it first.
        let cwd = agent
            .worktree_path
            .as_deref()
            .map_or(workspace.worktree_root.as_path(), Path::new);
        #[cfg(feature = "testkit")]
        rimz::testkit::rendezvous("RIMZ_TEST_STOP_BEFORE_SWAP_CANCEL");
        let rows = store.audit_rows().map_err(|err| format!("{err:#}"))?;
        // A replacement the store shows holding the agent may have bound after the read.
        if !closes_asked(rows.iter(), agent) {
            self.fresh = false;
        }
        let mut cancelled_self = false;
        for member in
            rimz::agents::state::launch_members(rows.iter(), &agent.kind, agent.launch_key())
                .into_iter()
                .filter(|member| member.agent_id.is_provisional() && member.epoch > 0)
        {
            // Any swap leaves the read stale, its cancel refused or not: a refusal means the
            // replacement bound, perhaps after the read, and holds the agent now.
            self.fresh = false;
            if store
                .cancel_unbound_swap(member, &workspace.session_name, cwd)
                .map_err(|err| format!("{err:#}"))?
                && member.agent_id == agent.agent_id
            {
                cancelled_self = true;
            }
        }
        if cancelled_self {
            ending.decide(Ok(()));
            return Ok(());
        }
        // Read after any swap: a replacement that bound first holds the agent now, and it
        // is what the stop closes.
        self.refresh_stale()?;
        let target = if closes_asked(self.read.agents(), agent) {
            agent.clone()
        } else {
            self.read
                .agent_hosting(&agent.kind, agent.launch_key())
                .holder
                .ok_or("the restarted agent disappeared while stopping it")?
                .clone()
        };
        ending.session = target.clone();
        #[cfg(feature = "testkit")]
        rimz::testkit::rendezvous("RIMZ_TEST_STOP_AFTER_TARGET");
        let agent = &target;
        // A run the caller passed is the one this stop settles for the agent it names;
        // `stop_run` matches by name as well, so it could pick another.
        let passed = self.run.filter(|run| run.matches_agent(agent));
        ending.holds_run = passed.is_some();
        ending.cancelled = match passed {
            Some(_) => self.run_cancelled,
            None => {
                ending.run =
                    stop_run(store, agent).map_err(|err| format!("reading its runs: {err:#}"))?;
                match ending.run.as_ref().filter(|run| !run.status.is_terminal()) {
                    Some(run) => {
                        supervised::cancel_supervised_run(store, run)
                            .map_err(|err| format!("{err:#}"))?;
                        self.fresh = false;
                        true
                    }
                    None => false,
                }
            }
        };
        if let Some(run) = passed {
            ending.run = Some(run.clone());
        }
        if ending.cancelled && !ending.run.as_ref().is_some_and(|run| run.keep) {
            self.await_departure(ending)?;
        }
        self.close(ending)
    }

    /// Record a stopped subagent run's end, once its ending is judged stopped.
    fn stamp_end(&self, ending: &Ending) -> std::result::Result<(), String> {
        let Some(run) = ending.run.as_ref().filter(|_| !ending.left_run) else {
            return Ok(());
        };
        supervised::stamp_stopped_subagent_end(
            &self.ctx.store,
            &self.ctx.workspace.session_name,
            run,
            (&ending.session.kind, &ending.session.agent_id),
        )
        .map_err(|err| format!("recording its end failed: {err:#}"))
    }

    /// Where the ending's session is hosted in the current read, following a re-key first;
    /// nowhere once it left the run this stop settles.
    fn hosting(&self, ending: &mut Ending) -> Hosting {
        let row = track(self.read.agents(), &mut ending.session);
        if row.is_some_and(|row| self.leaves_run(ending, row)) {
            ending.left_run = true;
        }
        if ending.left_run {
            return Hosting::Nowhere;
        }
        self.read
            .session_hosting(&ending.session.kind, &ending.session.agent_id)
    }

    /// Whether `row`, the ending's session as read again, no longer holds the run the caller
    /// passed: a fresh launch resumed the session and holds it now.
    fn leaves_run(&self, ending: &Ending, row: &AgentState) -> bool {
        ending.holds_run && self.run.is_some_and(|run| !run.matches_agent(row))
    }

    fn refresh(&mut self) -> std::result::Result<(), String> {
        match read_hosting(&self.ctx.workspace, &self.ctx.store, Some(self.read.mux())) {
            Ok(mut read) => {
                let _ = render::room::print_notices(read.take_notices());
                self.read = read;
                self.fresh = true;
                Ok(())
            }
            Err(mut err) => {
                let _ = render::room::print_notices(std::mem::take(&mut err.notices));
                Err(err.to_string())
            }
        }
    }

    /// Read again unless the current read is fresh.
    fn refresh_stale(&mut self) -> std::result::Result<(), String> {
        if self.fresh {
            return Ok(());
        }
        self.refresh()
    }

    /// A pane whose run this stop cancelled gets the backstop grace to leave on its own; a
    /// kept run's never leaves on its own, so `act` closes it at once.
    fn await_departure(&mut self, ending: &mut Ending) -> std::result::Result<(), String> {
        let deadline = Instant::now() + supervised::pane::STOP_BACKSTOP_GRACE;
        while self.hosting(ending) != Hosting::Nowhere && Instant::now() < deadline {
            std::thread::sleep(CLOSE_POLL);
            self.refresh()?;
        }
        self.fresh = false;
        Ok(())
    }

    /// Close whatever pane the current read says hosts the agent, read again first unless
    /// it is fresh, until none does or the owner-exit bound passes after the first close. A
    /// pane still listed is closed again once `RECLOSE_AFTER` has passed since its last close.
    /// Under [`TabClose::WhenLast`], a pane the gating read finds the last working pane of its
    /// tab closes with its tab; under [`TabClose::Whole`], a pane of the tab stays open for
    /// [`Stopper::close_whole_tab`].
    fn close(&mut self, ending: &mut Ending) -> std::result::Result<(), String> {
        let mut deadline = Instant::now() + stop_owner_exit_bound();
        let mut last: Option<(PaneId, Instant)> = None;
        let mut awaited_hold = false;
        loop {
            let Hosting::Pane {
                pane_id,
                session_name,
            } = self.hosting(ending)
            else {
                if last.is_none() && !awaited_hold {
                    awaited_hold = true;
                    self.await_kept_hold(ending)?;
                    deadline = Instant::now() + stop_owner_exit_bound();
                    continue;
                }
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
                let woken = rimz::harness::hibernate::wake_pane(&self.ctx.store, &pane_id)
                    .map_err(|err| format!("{err:#}"))?;
                if matches!(woken, rimz::harness::hibernate::Woken::Thawed { .. }) {
                    self.fresh = false;
                }
                if !self.fresh {
                    self.refresh()?;
                }
                if self.hosting(ending)
                    != (Hosting::Pane {
                        pane_id: pane_id.clone(),
                        session_name: session_name.clone(),
                    })
                {
                    continue;
                }
                if let TabClose::Whole { panes, .. } = &self.tab
                    && panes.contains(&pane_id)
                {
                    ending.awaits_tab = true;
                    return Ok(());
                }
                let closed_at = Instant::now();
                if last.is_none() {
                    deadline = closed_at + stop_owner_exit_bound();
                }
                let backend = rimz::mux::backend_for(self.read.mux());
                let result = if matches!(self.tab, TabClose::WhenLast)
                    && self.read.closing_empties_tab(
                        &pane_id,
                        rimz::mux::own_pane_id(self.read.mux()).as_ref(),
                    ) {
                    backend.close_tab_of_pane(&session_name, &pane_id)
                } else {
                    backend.close_pane(&session_name, &pane_id)
                };
                self.fresh = false;
                ending.closed = true;
                ending.close_error = result.err().map(|err| format!("{err:#}"));
                last = Some((pane_id, closed_at));
            }
            std::thread::sleep(CLOSE_POLL);
            self.refresh()?;
        }
    }

    /// Close the tab of [`TabClose::Whole`] once every agent has been ended, deciding each
    /// ending that awaits it as [`Stopper::close`] would. An agent whose stop failed before
    /// its close keeps its pane, and so the tab: the awaiting agents' panes and the `loose`
    /// panes close on their own instead, as they do when the gating read gives the tab no
    /// anchor. An error is a close that failed with no ending to carry it.
    fn close_whole_tab(&mut self, endings: Vec<&mut Ending>) -> std::result::Result<(), String> {
        let TabClose::Whole { panes, loose } = std::mem::take(&mut self.tab) else {
            return Ok(());
        };
        let failed = endings.iter().any(|ending| {
            !ending.closed
                && !ending.awaits_tab
                && ending.outcome.as_ref().is_some_and(Result::is_err)
        });
        let mut endings = endings
            .into_iter()
            .filter(|ending| ending.awaits_tab)
            .collect::<Vec<_>>();
        let anchor = match self.refresh_stale() {
            Ok(()) => {
                let own = rimz::mux::own_pane_id(self.read.mux());
                self.read.tab_anchor(&panes, own.as_ref()).cloned()
            }
            Err(reason) => return decide_all(&mut endings, reason),
        };
        let Some(anchor) = anchor.filter(|_| !failed) else {
            return self.close_each(&loose, &mut endings);
        };
        match self.close_tab_of(&panes, anchor, &mut endings) {
            Err(reason) => decide_all(&mut endings, reason),
            Ok(()) => Ok(()),
        }
    }

    /// Close the tab by `anchor` in one command, then wait as [`Stopper::close`] does until
    /// no pane hosts one of `endings` or the owner-exit bound passes, closing the tab again
    /// once `RECLOSE_AFTER` has passed while one does and a read still gives it an anchor
    /// among `panes`.
    fn close_tab_of(
        &mut self,
        panes: &[PaneId],
        mut anchor: PaneId,
        endings: &mut [&mut Ending],
    ) -> std::result::Result<(), String> {
        let mux = self.read.mux();
        let own = rimz::mux::own_pane_id(mux);
        let deadline = Instant::now() + stop_owner_exit_bound();
        loop {
            let result = rimz::mux::backend_for(mux)
                .close_tab_of_pane(&self.ctx.workspace.session_name, &anchor);
            self.fresh = false;
            let closed_at = Instant::now();
            let close_error = result.err().map(|err| format!("{err:#}"));
            if endings.is_empty() {
                return close_error.map_or(Ok(()), Err);
            }
            for ending in endings.iter_mut() {
                ending.closed = true;
                ending.close_error.clone_from(&close_error);
            }
            loop {
                std::thread::sleep(CLOSE_POLL);
                self.refresh()?;
                if endings
                    .iter_mut()
                    .all(|ending| self.hosting(ending) == Hosting::Nowhere)
                    || Instant::now() >= deadline
                {
                    return Ok(());
                }
                if Instant::now() >= closed_at + RECLOSE_AFTER {
                    break;
                }
            }
            let Some(listed) = self.read.tab_anchor(panes, own.as_ref()) else {
                return Ok(());
            };
            anchor = listed.clone();
        }
    }

    /// Close each of `endings` on its own, then each of `loose`.
    fn close_each(
        &mut self,
        loose: &[PaneId],
        endings: &mut [&mut Ending],
    ) -> std::result::Result<(), String> {
        for ending in endings.iter_mut() {
            if let Err(reason) = self.close(ending) {
                ending.decide(Err(reason));
            }
        }
        for pane in loose {
            rimz::mux::backend_for(pane.mux())
                .close_pane(&self.ctx.workspace.session_name, pane)
                .map_err(|err| format!("{err:#}"))?;
        }
        Ok(())
    }

    /// A kept run this stop cancelled can read as hosted nowhere for a moment: its wrapper
    /// ends the provider and then binds the pane again to hold it (`kept_run_stopped` in
    /// `exec/supervisor.rs`). Wait up to the backstop grace for a read that hosts it again.
    fn await_kept_hold(&mut self, ending: &mut Ending) -> std::result::Result<(), String> {
        let kept = ending
            .run
            .as_ref()
            .is_some_and(|run| run.keep && !run.subagent);
        if !(ending.cancelled && kept) {
            return Ok(());
        }
        let deadline = Instant::now() + supervised::pane::STOP_BACKSTOP_GRACE;
        while self.hosting(ending) == Hosting::Nowhere
            && !ending.left_run
            && Instant::now() < deadline
        {
            std::thread::sleep(CLOSE_POLL);
            self.refresh()?;
        }
        Ok(())
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
                if self.hosting(ending) != Hosting::Nowhere || ending.left_run {
                    continue;
                }
                let Some(owner) = track(self.read.agents(), &mut ending.session)
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
                if self.hosting(ending) != Hosting::Nowhere || ending.left_run {
                    continue;
                }
                let Some(agent) = track(&projection.agents, &mut ending.session) else {
                    continue;
                };
                let cwd = agent
                    .worktree_path
                    .as_deref()
                    .map_or(self.ctx.workspace.worktree_root.as_path(), Path::new);
                if let Err(err) = self.ctx.store.cancel_unbound_agent_launch(
                    &ending.session.kind,
                    &ending.session.agent_id,
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
                track(&projection.agents, &mut ending.session).is_some_and(|row| {
                    if self.leaves_run(ending, row) {
                        return false;
                    }
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
        if let Hosting::Pane { pane_id, .. } = self.hosting(ending) {
            ending.still_hosted = true;
            let close = ending
                .close_error
                .as_ref()
                .map(|err| format!("; closing it failed: {err}"))
                .unwrap_or_default();
            return Err(format!("a pane still hosts it: {pane_id}{close}"));
        }
        // The run's stop was its cancel; the launch that resumed the session is not stopped.
        if ending.left_run {
            return Ok(());
        }
        unhosted_verdict(
            track(agents, &mut ending.session),
            ending.awaits_owner(),
            ending.signaled,
        )
    }

    /// One root's receipt; schedules retire for each agent this stop stopped, or whose
    /// pane it closed and no longer finds.
    fn receipt(&self, endings: Vec<Ending>) -> TreeStop {
        let mut stop = TreeStop::default();
        for ending in endings {
            let key = ending.key();
            // `judge` decides every ending before a receipt is built, so an undecided one
            // is impossible; it still never reads as stopped.
            let outcome = ending
                .outcome
                .unwrap_or_else(|| Err("its stop was never judged".to_owned()));
            match (&ending.parent, &outcome) {
                (Some(parent), Ok(())) => stop
                    .child_receipts
                    .push(format!("stopped {} (subagent of {parent})", ending.label)),
                (None, Ok(())) => {
                    stop.root_stopped = true;
                    stop.root = Some(ending.session.clone());
                }
                (_, Err(reason)) => stop.failures.push(format!("{}: {reason}", ending.label)),
            }
            if ending.left_run {
                continue;
            }
            if ending.closed || outcome.is_ok() {
                stop.reached.push(ending.asked.clone());
                stop.reached.push(key.clone());
            }
            if outcome.is_err() && !(ending.closed && !ending.still_hosted) {
                continue;
            }
            // A stop that ends the agent retires the waits armed in any of its
            // sessions, including those a swap's replaced session kept for the
            // holder. One that closed only a replaced session leaves them.
            let rows = match self.ctx.store.audit_rows() {
                Ok(rows) => rows,
                Err(err) => {
                    stop.failures.push(format!("{}: {err:#}", ending.label));
                    continue;
                }
            };
            let agent = rows
                .iter()
                .find(|row| (&row.kind, &row.agent_id) == (&key.0, &key.1));
            if agent.is_some_and(|agent| !stop_ends_agent(rows.iter(), agent, &key)) {
                continue;
            }
            let members = agent.map_or_else(Vec::new, |agent| {
                rimz::agents::state::launch_members(rows.iter(), &agent.kind, agent.launch_key())
            });
            // A wait armed since targets hold launch ids is pinned to the launch, under
            // the kind its arming session ran as.
            let sessions = std::iter::once((&ending.asked.0, &ending.asked.1))
                .chain(members.iter().flat_map(|member| {
                    [
                        (&member.kind, &member.agent_id),
                        (&member.kind, member.launch_key()),
                    ]
                }))
                .collect::<std::collections::BTreeSet<_>>();
            for (kind, session) in sessions {
                if let Err(err) = rimz::harness::schedule::arm::retire_session(
                    &self.ctx.workspace.project_root,
                    kind,
                    session,
                ) {
                    stop.failures.push(format!("{}: {err}", ending.label));
                }
            }
        }
        stop
    }
}

/// Decide each of `endings` failed for `reason`; with none to carry it, `reason` is the error.
fn decide_all(endings: &mut [&mut Ending], reason: String) -> std::result::Result<(), String> {
    if endings.is_empty() {
        return Err(reason);
    }
    for ending in endings.iter_mut() {
        ending.decide(Err(reason.clone()));
    }
    Ok(())
}

/// Whether a stop closes `agent` itself, judged over the rows of a read taken after any swap
/// cancellation: unless a swap replaced it meanwhile (the replacement bound before the
/// cancellation could land, so the cancellation was refused) or it is a starting
/// reservation with no pane. Otherwise it closes the agent's holder.
fn closes_asked<'a>(
    rows: impl IntoIterator<Item = &'a AgentState> + Clone,
    agent: &AgentState,
) -> bool {
    let members = rimz::agents::state::launch_members(rows, &agent.kind, agent.launch_key());
    let holders = rimz::agents::state::LaunchHolders::new(members.iter().copied());
    let replaced = members.iter().any(|member| {
        member.kind == agent.kind && member.agent_id == agent.agent_id && holders.replaced(member)
    });
    !replaced && (agent.pane.is_some() || !agent.agent_id.is_provisional())
}

/// Whether closing `closed` ends `agent`'s agent: every other row holding it
/// is over already. Waits and watchers armed for the agent go only then; a
/// stop that closed a session a swap replaced leaves them to the holder.
fn stop_ends_agent<'a>(
    rows: impl IntoIterator<Item = &'a AgentState> + Clone,
    agent: &AgentState,
    closed: &AgentKey,
) -> bool {
    let members = rimz::agents::state::launch_members(rows, &agent.kind, agent.launch_key());
    rimz::agents::state::launch_holder(&members)
        .iter()
        .all(|holder| {
            holder.session_over() || (&holder.kind, &holder.agent_id) == (&closed.0, &closed.1)
        })
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

/// The row `session` is now in `agents`; `session` follows it when a re-key moved it.
fn track<'a>(agents: &'a [AgentState], session: &mut AgentState) -> Option<&'a AgentState> {
    let row = session_row(agents, session)?;
    if row.agent_id != session.agent_id {
        *session = row.clone();
    }
    Some(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stop that read the old session while its swap's replacement bound
    /// closes the holder instead, and only a stop of the holder ends the agent.
    #[test]
    fn a_stop_racing_a_bound_swap_targets_the_replacement() {
        let pane = |raw: &str| {
            rimz::pane::PaneRef::from_id(rimz::ids::PaneId::from_parts(
                rimz::ids::MuxName::Tmux,
                raw,
            ))
        };
        let mut old = AgentState::stub("claude", "sess-old", rimz::agents::AgentStatus::Idle);
        old.launch_id = Some("launch_agent".into());
        old.pane = Some(pane("%1"));
        let mut replacement = old.clone();
        replacement.agent_id = "launch_swap".into();
        replacement.epoch = 1;
        replacement.pane = Some(pane("%2"));
        let key = |row: &AgentState| (row.kind.clone(), row.agent_id.clone());

        let unbound = [old.clone(), replacement.clone()];
        assert!(
            closes_asked(&unbound, &old),
            "a reservation that never stood leaves the old session the agent"
        );
        assert!(stop_ends_agent(&unbound, &old, &key(&old)));

        replacement.launch_bound = true;
        let bound = [old.clone(), replacement.clone()];
        assert!(!closes_asked(&bound, &old));
        assert!(
            !stop_ends_agent(&bound, &old, &key(&old)),
            "closing the replaced session leaves the holder's waits"
        );
        assert!(stop_ends_agent(&bound, &old, &key(&replacement)));
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
