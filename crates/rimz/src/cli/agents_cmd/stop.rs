use super::*;

use std::collections::HashSet;
use std::time::Duration;

use super::runs_lookup::newest_run_by_ref;
use crate::cli::{render, select};
use rimz::agents::state::session_row;
use rimz::harness::stop_booking::{StopBooking, book_stop};
use rimz::launcher::stop::{self as tree, StopRoom};
pub(in crate::cli) use rimz::launcher::stop::{StopTerms, TabClose, TreeStop};

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

pub(in crate::cli) fn offers_resume(agent: &AgentState) -> bool {
    !agent.is_provider_subagent()
        && agent.parent_agent_id.is_none()
        && !agent.launch_depth.is_some_and(|depth| depth > 0)
        && rimz::agents::registry::session_resumable(agent.kind.as_str(), &agent.agent_id)
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
        let name = live_agent.name.as_deref();
        let label = rimz::address::handle_with_name(
            &rimz::address::paste_address(live_agent, &snapshot, current_channel),
            name,
        );
        let stop = stop_resolved(&ctx, globals, &snapshot, &[live_agent])?.remove(0);
        let mut out = render::out();
        report_children(&stop, &mut out)?;
        if stop.error().is_some() {
            return stop.into_result(&rimz::address::named_handle(live_agent, &peers, true));
        }
        let mut receipt = format!("stopped {label} ({})", live_agent.kind);
        if offers_resume(live_agent) {
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
        report_children(&stop, &mut out)?;
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
                        closed.extend(stop.reached().iter().cloned());
                        report_children(&stop, &mut out)?;
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
        closed.extend(stop.reached().iter().cloned());
        let label = rimz::address::handle_with_name(&target.address, agent.name.as_deref());
        if report_stop(&stop, &mut out, &label)? {
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
    let label = rimz::address::named_handle(agent, &peers, true);
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

pub(in crate::cli) fn stop_room<'a>(ctx: &'a Ctx, globals: &GlobalFlags) -> StopRoom<'a> {
    StopRoom {
        workspace: &ctx.workspace,
        store: &ctx.store,
        mux: globals.mux,
    }
}

/// The receipt line of each child a tree stop stopped.
fn report_children(stop: &TreeStop, out: &mut impl Write) -> Result<()> {
    for receipt in stop.child_receipts() {
        writeln!(out, "{receipt}")?;
    }
    Ok(())
}

/// The per-agent lines of a stop over several agents; `true` when this agent had a failure.
pub(in crate::cli) fn report_stop(
    stop: &TreeStop,
    out: &mut impl Write,
    label: &str,
) -> Result<bool> {
    report_children(stop, out)?;
    if stop.root_stopped() {
        writeln!(out, "stopped {label}")?;
    }
    let Some(error) = stop.error() else {
        return Ok(false);
    };
    writeln!(out, "error {label}: {error}")?;
    Ok(true)
}

/// [`tree::stop_run`].
pub(super) fn stop_run(store: &rimz::Store, agent: &AgentState) -> Result<Option<RunRecord>> {
    tree::stop_run(store, agent)
}

/// [`tree::stop_resolved`], printing the backend notices its reads gave.
pub(in crate::cli) fn stop_resolved(
    ctx: &Ctx,
    globals: &GlobalFlags,
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    agents: &[&AgentState],
) -> Result<Vec<TreeStop>> {
    let mut notices = Vec::new();
    let stops = tree::stop_resolved(stop_room(ctx, globals), snapshot, agents, &mut notices);
    // The stop has acted, so a notice stderr cannot take does not fail it.
    let _ = render::room::print_notices(notices);
    stops
}

/// [`tree::stop_resolved_with`], printing the backend notices its reads gave.
pub(in crate::cli) fn stop_resolved_with(
    ctx: &Ctx,
    globals: &GlobalFlags,
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    agents: &[&AgentState],
    terms: StopTerms<'_>,
) -> Result<Option<Vec<TreeStop>>> {
    let mut notices = Vec::new();
    let stops = tree::stop_resolved_with(
        stop_room(ctx, globals),
        snapshot,
        agents,
        terms,
        &mut notices,
    );
    // The stop has acted, so a notice stderr cannot take does not fail it.
    let _ = render::room::print_notices(notices);
    stops
}

/// [`tree::stop_run_agent`], printing the backend notices its reads gave.
pub(in crate::cli) fn stop_run_agent(
    ctx: &Ctx,
    globals: &GlobalFlags,
    run: &RunRecord,
) -> Result<TreeStop> {
    stop_run_tree(ctx, globals, run, None)
}

/// [`tree::stop_run_tree`], printing the backend notices its reads gave.
pub(super) fn stop_run_tree(
    ctx: &Ctx,
    globals: &GlobalFlags,
    run: &RunRecord,
    agent: Option<&AgentState>,
) -> Result<TreeStop> {
    let mut notices = Vec::new();
    let stop = tree::stop_run_tree(stop_room(ctx, globals), run, agent, &mut notices);
    // The stop has acted, so a notice stderr cannot take does not fail it.
    let _ = render::room::print_notices(notices);
    stop
}
