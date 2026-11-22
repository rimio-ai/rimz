use super::*;

use super::runs_lookup::{agent_name, newest_run_by_ref};
use crate::cli::render::announce::{MESSAGE_STOP_EXIT, write_message_stop};
use crate::cli::{render, select};
use rimz::agents::TurnCompletion;
use rimz::launcher::agent_join::{Joined, Observed, WaitSet, WaitTarget, join, mark_joined};
pub(super) use rimz::launcher::agent_join::{TargetOutcome, TerminalPayload};
use rimz::message::announce::WaitCaller;
use rimz::message::turn_wait::{TurnWaitReason, TurnWaitView};

pub(super) fn report_wait_reason(
    err: &mut impl Write,
    previous: &mut Option<TurnWaitReason>,
    reason: Option<TurnWaitReason>,
    address: &str,
) -> Result<()> {
    let now = jiff::Timestamp::now();
    let text = reason
        .as_ref()
        .and_then(|reason| wait_reason_text(reason, now));
    let changed = text
        != previous
            .as_ref()
            .and_then(|reason| wait_reason_text(reason, now));
    *previous = reason;
    if changed && let Some(text) = text {
        let prefix = match previous {
            Some(TurnWaitReason::NoTurnStarted) => "",
            Some(TurnWaitReason::WakeInFlight | TurnWaitReason::MessageQueued) => "it has a ",
            _ => "it is ",
        };
        write!(err, "waiting on {address}: {prefix}{text}")?;
        if matches!(previous, Some(TurnWaitReason::WaitingOnPerson(_))) {
            write!(
                err,
                "; answer: {}",
                shlex::try_join(["rimz", "answer", address])?
            )?;
        }
        writeln!(err)?;
        err.flush()?;
    }
    Ok(())
}

fn wait_reason_text(reason: &TurnWaitReason, now: jiff::Timestamp) -> Option<String> {
    Some(match reason {
        TurnWaitReason::Running => return None,
        TurnWaitReason::WaitingOnPerson(ask) => {
            let mut text = "waiting on a person".to_owned();
            if let Some(ask) = ask {
                text.push_str(&format!(" ({}", ask.kind.short_label()));
                if let Some(detail) = &ask.detail {
                    text.push_str(&format!(": {detail:?}"));
                }
                text.push(')');
            }
            text
        }
        TurnWaitReason::Sleeping(waits) if !waits.is_empty() => format!(
            "sleeping ({})",
            waits
                .iter()
                .map(|wait| wait.label(now))
                .collect::<Vec<_>>()
                .join("; "),
        ),
        TurnWaitReason::Sleeping(_) => "sleeping".to_owned(),
        TurnWaitReason::WakeInFlight => "wake in flight".to_owned(),
        TurnWaitReason::MessageQueued => "message queued; this rest is not its finish".to_owned(),
        TurnWaitReason::Paused => "paused".to_owned(),
        TurnWaitReason::NoTurnStarted => "no turn started".to_owned(),
    })
}

/// What `agents wait` waits on: its positional references, or each agent
/// `--filter` or stdin selects by the address `agents list -q` prints, which
/// the wait resolves again on every poll. `None` when nothing was selected.
pub(super) fn wait_references(
    references: Vec<String>,
    filters: &[String],
    all: bool,
    globals: &GlobalFlags,
) -> Result<Option<Vec<String>>> {
    if !select::selects(&references, filters) {
        return Ok(Some(references));
    }
    if select::filter_help(filters)? {
        return Ok(None);
    }
    let ctx = Ctx::open(globals)?;
    let flags = select::TargetFlags {
        references: &references,
        filters,
        all,
        room: false,
    };
    Ok(match select::select_targets(&ctx, "wait", flags)? {
        select::Targets::References(references) => Some(references),
        select::Targets::Selected(selected) => {
            Some(selected.into_iter().map(|target| target.address).collect())
        }
        select::Targets::Done => None,
    })
}

/// Wait on pairs of the typed reference and its lookup reference.
pub(in crate::cli) fn wait_agent(
    mut references: Vec<(String, String)>,
    any: bool,
    timeout: Option<Duration>,
    stream_output: bool,
    from_start: bool,
    json: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let caller = WaitCaller::starting_now();
    let mut seen = std::collections::HashSet::new();
    references.retain(|(typed, _)| seen.insert(typed.clone()));
    if stream_output && references.len() > 1 {
        bail!("--stream tails one target; wait on a single reference");
    }
    let ctx = &Ctx::open(globals)?;
    let store = &ctx.store;
    let snapshot = ctx.cached_snapshot()?;
    let current_channel = &ctx.address_context();
    let targets = references
        .iter()
        .map(|(typed, reference)| {
            resolve_wait_target(store, &snapshot, typed, reference, current_channel)
        })
        .collect::<Result<Vec<_>>>()?;
    let deferring = defers_to_report(store, &snapshot, &targets)?;
    if stream_output {
        if deferring {
            bail!(
                "--stream does not follow a run you launched that reports to you: its answer \
                 reaches you in your report; drop --stream to see where it stands"
            );
        }
        let options = WaitStreamOptions {
            timeout,
            from_start,
            json,
        };
        let target = targets.first().context("wait requires a reference")?;
        return wait_stream_request(
            (store, &ctx.workspace.session_name),
            target,
            current_channel,
            options,
            caller,
        );
    }
    if deferring {
        return wait_deferred(ctx, targets, any, json, caller);
    }

    let style = WaitStyle::new(targets.len(), any, json);
    wait_non_stream(ctx, targets, any, timeout, style, caller)
}

/// Whether this join leaves its answers to the caller's fleet report: every target is a run
/// the calling agent launched that reports to it
/// (`rimz::harness::run::report::defers_to_report`). One target outside that rule, an
/// interactive turn included, makes the whole join block as it would for anyone.
fn defers_to_report(
    store: &rimz::Store,
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    targets: &[WaitTarget],
) -> Result<bool> {
    let caller = rimz::harness::ancestry::resolve_caller(&snapshot.agents);
    if caller.is_none() || targets.is_empty() {
        return Ok(false);
    }
    let audit = store.runtime_projection(rimz::RuntimeScope::Audit)?.agents;
    for target in targets {
        let WaitTarget::Run { run_id, .. } = target else {
            return Ok(false);
        };
        let record = rimz::harness::run::load(store.paths(), run_id)?;
        if !rimz::harness::run::report::defers_to_report(
            &snapshot.agents,
            &audit,
            caller.as_ref(),
            &record,
        ) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// A join that leaves its answers to the caller's fleet report: it polls once and prints, in
/// the order the targets were named, the report's row for each settled run (claimed as any
/// printed answer is) and a still-running line for each open one. Its exit is a settled join's
/// when the join is met, else `MESSAGE_STOP_EXIT`; `--timeout` has no effect.
fn wait_deferred(
    ctx: &Ctx,
    targets: Vec<WaitTarget>,
    any: bool,
    json: bool,
    caller: WaitCaller,
) -> Result<()> {
    let store = &ctx.store;
    let session_name = &ctx.workspace.session_name;
    let mut waits = WaitSet::once(targets, any);
    let mut caller = caller;
    let joined = join(
        store,
        &ctx.address_context(),
        &mut waits,
        &mut || caller.queued(store, session_name),
        &mut |observed| match observed {
            Observed::Reason {
                address,
                mut previous,
                reason,
            } => report_wait_reason(&mut render::err(), &mut previous, reason, address),
            Observed::Pending { .. } => Ok(()),
        },
    )?;
    let (exit, announcement) = match joined {
        Joined::Settled { selected, .. } => (
            if any {
                exit_code(settled_outcome(&waits, selected)?)
            } else {
                all_exit_code(&waits)
            },
            None,
        ),
        Joined::StillRunning { .. } => (MESSAGE_STOP_EXIT, None),
        Joined::Interrupted { answer, .. } => (MESSAGE_STOP_EXIT, Some(answer)),
    };
    let audit = store.runtime_projection(rimz::RuntimeScope::Audit)?.agents;
    let mut rows = Vec::new();
    let mut entries = BTreeMap::new();
    let mut printed = Vec::new();
    for (target, outcome) in waits.targets.iter().zip(&waits.outcomes) {
        let WaitTarget::Run { run_id, name, .. } = target else {
            bail!("a deferring join waits only on runs");
        };
        let Some(outcome) = outcome else {
            let record = rimz::harness::run::load(store.paths(), run_id)?;
            let secs = jiff::Timestamp::now()
                .duration_since(record.answer_started_at())
                .as_secs()
                .max(0) as u64;
            rows.push(format!(
                "- @{name}: still running ({} so far); it will be in your report",
                render::format_compact_duration(secs)
            ));
            entries.insert(
                target.typed_reference(),
                unfinished_entry(target, Unfinished::Running),
            );
            continue;
        };
        let TerminalPayload::Run(record) = &outcome.payload else {
            bail!("a deferring join settles only runs");
        };
        let child = audit
            .iter()
            .find(|agent| record.matches_agent(agent))
            .context("the run's agent row is gone")?;
        let response = match rimz::harness::run::publish_response(store.paths(), record)? {
            Some(path) => Some(super::subagent_report::ResponseFile {
                summary: rimz::disk::summary::FileSummary::measure(&path)
                    .with_context(|| format!("measuring {}", path.display()))?,
                path,
            }),
            None => None,
        };
        rows.push(super::subagent_report::compose_digest_row(
            child,
            record,
            response.as_ref(),
        ));
        let mut entry = entry(outcome);
        entry.last_message = None;
        if let Some(response) = response {
            entry.response_path = Some(response.path);
            entry.response_tokens = Some(response.summary.tokens);
            entry.response_lines = Some(response.summary.lines);
        }
        entries.insert(target.typed_reference(), entry);
        printed.push(outcome);
    }
    if json {
        render::json(&entries)?;
    } else {
        let mut out = render::out();
        for row in rows {
            writeln!(out, "{row}")?;
        }
        out.flush()?;
    }
    // Claimed only once printed: a row that failed to compose leaves every answer to the report.
    mark_joined(store, session_name, printed);
    if let Some(announcement) = &announcement {
        write_message_stop(&mut render::err(), announcement)?;
    }
    render::err().flush()?;
    std::process::exit(exit);
}

#[derive(Clone, Copy)]
struct WaitStreamOptions {
    timeout: Option<Duration>,
    from_start: bool,
    json: bool,
}

fn wait_stream_request(
    (store, session_name): (&rimz::Store, &str),
    target: &WaitTarget,
    current_channel: &rimz::address::AddressContext,
    options: WaitStreamOptions,
    caller: WaitCaller,
) -> Result<()> {
    let typed_reference = target.typed_reference();
    match target {
        WaitTarget::Run { run_id, .. } => {
            let run = rimz::harness::run::load(store.paths(), run_id)?;
            wait_run_stream(
                (store, session_name),
                &run,
                typed_reference,
                current_channel,
                options,
                caller,
            )
        }
        WaitTarget::Agent {
            reference, kind, ..
        } => wait_interactive_agent_stream(
            (store, session_name),
            typed_reference,
            reference,
            kind,
            current_channel,
            options,
            caller,
        ),
    }
}

fn wait_non_stream(
    ctx: &Ctx,
    targets: Vec<WaitTarget>,
    any: bool,
    timeout: Option<Duration>,
    style: WaitStyle,
    caller: WaitCaller,
) -> Result<()> {
    let current_channel = &ctx.address_context();
    let store = &ctx.store;
    let session_name = &ctx.workspace.session_name;
    let mut waits = WaitSet::new(targets, any, timeout);
    let mut caller = caller;
    let joined = join(
        store,
        current_channel,
        &mut waits,
        &mut || caller.queued(store, session_name),
        &mut |observed| match observed {
            Observed::Reason {
                address,
                mut previous,
                reason,
            } => report_wait_reason(&mut render::err(), &mut previous, reason, address),
            Observed::Pending { waits, settled } => {
                style.report_progress(store, session_name, waits, settled)
            }
        },
    )?;
    {
        match joined {
            Joined::Settled { selected, settled } => {
                style.report_settled(
                    store,
                    session_name,
                    current_channel,
                    &waits,
                    selected,
                    &settled,
                )?;
                render::err().flush()?;
                std::process::exit(style.exit_code(&waits, selected)?);
            }
            Joined::StillRunning { settled } => {
                style.report_unfinished(
                    store,
                    session_name,
                    &waits,
                    &settled,
                    Unfinished::TimedOut,
                )?;
                render::err().flush()?;
                std::process::exit(Unfinished::TimedOut.exit_code());
            }
            Joined::Interrupted {
                settled,
                answer: announcement,
            } => {
                style.report_unfinished(
                    store,
                    session_name,
                    &waits,
                    &settled,
                    Unfinished::Running,
                )?;
                write_message_stop(&mut render::err(), &announcement)?;
                std::process::exit(Unfinished::Running.exit_code());
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WaitStyle {
    Single { json: bool },
    Any { json: bool },
    All { json: bool },
}

impl WaitStyle {
    pub(super) fn new(target_count: usize, any: bool, json: bool) -> Self {
        if target_count == 1 && !any {
            Self::Single { json }
        } else if any {
            Self::Any { json }
        } else {
            Self::All { json }
        }
    }

    fn report_progress(
        self,
        store: &rimz::Store,
        session_name: &str,
        waits: &WaitSet,
        settled: &[usize],
    ) -> Result<()> {
        match self {
            Self::Single { .. } => Ok(()),
            Self::Any { json } | Self::All { json } => {
                report_settled_disappearances(waits, settled)?;
                if !json {
                    mark_settled_joined(store, session_name, waits, settled)?;
                    print_settled_blocks(waits, settled)?;
                }
                Ok(())
            }
        }
    }

    fn report_settled(
        self,
        store: &rimz::Store,
        session_name: &str,
        current_channel: &rimz::address::AddressContext,
        waits: &WaitSet,
        selected: usize,
        settled: &[usize],
    ) -> Result<()> {
        match self {
            Self::Single { json } => {
                let outcome = settled_outcome(waits, selected)?;
                if matches!(&outcome.payload, TerminalPayload::Disappeared) {
                    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
                    crate::cli::resolve_agent_one(
                        store,
                        &snapshot,
                        waits.targets[selected].typed_reference(),
                        None,
                        current_channel,
                    )?;
                    return Ok(());
                }
                mark_joined(store, session_name, std::iter::once(outcome));
                if json {
                    print_wait_json(std::iter::once(outcome))
                } else {
                    print_single_outcome(outcome)
                }
            }
            Self::Any { json } => {
                let outcome = settled_outcome(waits, selected)?;
                report_disappearance(&waits.targets[selected], outcome)?;
                mark_joined(store, session_name, std::iter::once(outcome));
                if json {
                    print_wait_json(std::iter::once(outcome))
                } else {
                    print_wait_block(
                        &mut render::out(),
                        &mut render::err(),
                        outcome,
                        render::prose::Prose::for_stdout(),
                    )
                }
            }
            Self::All { json } => {
                report_settled_disappearances(waits, settled)?;
                if json {
                    mark_joined(store, session_name, waits.outcomes.iter().flatten());
                    print_wait_json(waits.outcomes.iter().flatten())
                } else {
                    mark_settled_joined(store, session_name, waits, settled)?;
                    print_settled_blocks(waits, settled)
                }
            }
        }
    }

    /// The ending of a wait that stops with targets unfinished: at its
    /// deadline, or on a message queued for its caller.
    fn report_unfinished(
        self,
        store: &rimz::Store,
        session_name: &str,
        waits: &WaitSet,
        settled: &[usize],
        ending: Unfinished,
    ) -> Result<()> {
        match self {
            Self::Single { json: true } => print_unfinished_json(waits, ending),
            Self::Single { json: false } => print_unfinished_headers(waits, ending),
            Self::Any { json } | Self::All { json } => {
                report_settled_disappearances(waits, settled)?;
                if json {
                    mark_joined(store, session_name, waits.outcomes.iter().flatten());
                    print_unfinished_json(waits, ending)
                } else {
                    mark_settled_joined(store, session_name, waits, settled)?;
                    print_settled_blocks(waits, settled)?;
                    print_unfinished_headers(waits, ending)
                }
            }
        }
    }

    fn exit_code(self, waits: &WaitSet, selected: usize) -> Result<i32> {
        match self {
            Self::Single { .. } | Self::Any { .. } => {
                Ok(exit_code(settled_outcome(waits, selected)?))
            }
            Self::All { .. } => Ok(all_exit_code(waits)),
        }
    }
}

fn mark_settled_joined(
    store: &rimz::Store,
    session_name: &str,
    waits: &WaitSet,
    settled: &[usize],
) -> Result<()> {
    for &index in settled {
        mark_joined(
            store,
            session_name,
            std::iter::once(settled_outcome(waits, index)?),
        );
    }
    Ok(())
}

fn settled_outcome(waits: &WaitSet, index: usize) -> Result<&TargetOutcome> {
    waits.outcome(index).context("settled wait without outcome")
}

fn unfinished_entry(target: &WaitTarget, ending: Unfinished) -> WaitEntryJson {
    let (name, reason) = match target {
        WaitTarget::Run { name, reason, .. } | WaitTarget::Agent { name, reason, .. } => {
            (name, reason)
        }
    };
    let mut entry = WaitEntryJson::new(name.clone(), ending.status(), None, None, None, None);
    entry.exit = ending.exit_code();
    entry.reason = reason
        .as_ref()
        .and_then(|reason| wait_reason_text(reason, jiff::Timestamp::now()));
    entry
}

/// The JSON entry of a settled target.
pub(super) fn entry(outcome: &TargetOutcome) -> WaitEntryJson {
    match &outcome.payload {
        TerminalPayload::Run(record) => {
            let mut entry = WaitEntryJson::new(
                outcome.name.clone(),
                record.status,
                record.cost_usd,
                record.transcript_path.clone(),
                record.last_message.clone(),
                (record.status == RunStatus::Failed)
                    .then(|| record.failure_tail.clone())
                    .flatten(),
            );
            entry.cancel_reason = record.cancel_reason;
            entry
        }
        TerminalPayload::Agent {
            agent,
            completion,
            last_message,
        } => WaitEntryJson::new(
            outcome.name.clone(),
            if *completion == TurnCompletion::Failed {
                RunStatus::Failed
            } else {
                RunStatus::Completed
            },
            agent
                .context
                .as_ref()
                .and_then(|context| context.cost.as_ref())
                .and_then(|cost| cost.total_cost_usd),
            agent.transcript_path.clone(),
            last_message.clone(),
            None,
        ),
        TerminalPayload::Disappeared => WaitEntryJson::new(
            outcome.name.clone(),
            RunStatus::Failed,
            None,
            None,
            None,
            Some("agent disappeared while waiting".to_owned()),
        ),
    }
}

fn exit_code(outcome: &TargetOutcome) -> i32 {
    entry(outcome).exit
}

#[derive(serde::Serialize)]
pub(super) struct WaitEntryJson {
    name: String,
    status: RunStatus,
    exit: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transcript_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_message: Option<String>,
    /// A deferring join's answer file, carried in place of `last_message`.
    #[serde(skip_serializing_if = "Option::is_none")]
    response_path: Option<std::path::PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_lines: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cancel_reason: Option<rimz::store::run::CancelReason>,
}

impl WaitEntryJson {
    fn new(
        name: String,
        status: RunStatus,
        cost: Option<f64>,
        transcript_path: Option<String>,
        last_message: Option<String>,
        error: Option<String>,
    ) -> Self {
        Self {
            name,
            status,
            exit: status.exit_code(),
            cost,
            transcript_path,
            last_message,
            response_path: None,
            response_tokens: None,
            response_lines: None,
            error,
            reason: None,
            cancel_reason: None,
        }
    }
}

fn resolve_wait_target(
    store: &rimz::Store,
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    typed_reference: &str,
    reference: &str,
    current_channel: &rimz::address::AddressContext,
) -> Result<WaitTarget> {
    let live_agent_result =
        crate::cli::resolve_agent_one(store, snapshot, reference, None, current_channel);
    let live_agent = live_agent_result.as_ref().ok().copied();
    if let Some(run) = newest_run_by_ref(store, reference, live_agent)?
        && (!run.status.is_terminal()
            || live_agent.is_none()
            || run.run_id.as_str() == reference
            || (run.subagent && live_agent.is_some_and(AgentState::is_launched_child))
            || run_describes_latest_turn(store, &run, reference, current_channel)?)
    {
        let name = run
            .agent_name
            .clone()
            .unwrap_or_else(|| run.run_id.as_str().to_owned());
        return Ok(WaitTarget::Run {
            run_id: run.run_id,
            name,
            typed_reference: typed_reference.to_owned(),
            reason: None,
        });
    }
    if live_agent.is_none() {
        live_agent_result?;
    }
    Ok(WaitTarget::Agent {
        reference: reference.to_owned(),
        typed_reference: typed_reference.to_owned(),
        name: agent_name(live_agent.context("resolved wait agent without state")?).to_owned(),
        reason: None,
        kind: live_agent
            .context("resolved wait agent without state")?
            .kind
            .clone(),
        completed_seen_at: None,
    })
}

/// Whether a live peer's terminal run is its latest turn, read from the same
/// wake-aware view the agent target would settle on.
fn run_describes_latest_turn(
    store: &rimz::Store,
    run: &RunRecord,
    reference: &str,
    current_channel: &rimz::address::AddressContext,
) -> Result<bool> {
    if run.peer.is_none() {
        return Ok(false);
    }
    let view = TurnWaitView::load(store).context("reading agent snapshot")?;
    let Ok(peer) =
        crate::cli::resolve_agent_one(store, &view.snapshot, reference, None, current_channel)
    else {
        return Ok(false);
    };
    Ok(rimz::harness::run::describes_latest_turn(
        run,
        peer,
        view.completion(peer),
    ))
}

/// How a wait that stops with targets unfinished reports them.
#[derive(Clone, Copy)]
enum Unfinished {
    TimedOut,
    /// Stopped on a message queued for the caller; the targets run on.
    Running,
}

impl Unfinished {
    const fn status(self) -> RunStatus {
        match self {
            Self::TimedOut => RunStatus::TimedOut,
            Self::Running => RunStatus::Running,
        }
    }

    const fn exit_code(self) -> i32 {
        match self {
            Self::TimedOut => RunStatus::TimedOut.exit_code(),
            Self::Running => MESSAGE_STOP_EXIT,
        }
    }
}

fn report_run_wait_reason(
    view: &TurnWaitView,
    record: &RunRecord,
    current_channel: &rimz::address::AddressContext,
    reason: &mut Option<TurnWaitReason>,
) -> Result<()> {
    let Some(agent) = view
        .snapshot
        .agents
        .iter()
        .find(|agent| record.matches_agent(agent))
    else {
        *reason = None;
        return Ok(());
    };
    let address = rimz::address::paste_address(agent, &view.snapshot, current_channel);
    report_wait_reason(&mut render::err(), reason, view.reason(agent), &address)
}

fn all_exit_code(waits: &WaitSet) -> i32 {
    waits
        .outcomes
        .iter()
        .flatten()
        .find(|outcome| entry(outcome).status != RunStatus::Completed)
        .map_or(0, exit_code)
}

fn print_wait_json<'a>(outcomes: impl IntoIterator<Item = &'a TargetOutcome>) -> Result<()> {
    let entries = outcomes
        .into_iter()
        .map(|outcome| (outcome.reference.as_str(), entry(outcome)))
        .collect::<BTreeMap<_, _>>();
    render::json(&entries)
}

pub(super) fn print_wait_block(
    out: &mut impl Write,
    err: &mut impl Write,
    outcome: &TargetOutcome,
    prose: render::prose::Prose,
) -> Result<()> {
    write_wait_header(out, outcome)?;
    if let TerminalPayload::Agent { last_message, .. } = &outcome.payload {
        let completed = entry(outcome).status == RunStatus::Completed;
        if completed && last_message.is_none() {
            write_wait_header(err, outcome)?;
        }
        supervised::output::print_final_message(
            supervised::output::FinalMessage {
                scope: "turn",
                message: last_message.as_deref(),
                completed,
            },
            out,
            err,
            prose,
            render::prose::prose_width(0),
        )?;
    }
    if let TerminalPayload::Run(record) = &outcome.payload {
        let emits_diagnostics = record.status != RunStatus::Completed
            || record
                .last_message
                .as_deref()
                .is_none_or(|message| message.trim().is_empty());
        if emits_diagnostics {
            write_wait_header(err, outcome)?;
        }
        supervised::output::print_run_output(
            record,
            out,
            err,
            prose,
            render::prose::prose_width(0),
        )?;
    }
    writeln!(out)?;
    Ok(())
}

fn write_wait_header(out: &mut impl Write, outcome: &TargetOutcome) -> Result<()> {
    let entry = entry(outcome);
    writeln!(
        out,
        "--- {} ({}) ---",
        render::paint(render::palette::body(), &outcome.reference),
        render::paint(render::status::run(entry.status), entry.status.label()),
    )?;
    Ok(())
}

fn print_single_outcome(outcome: &TargetOutcome) -> Result<()> {
    match &outcome.payload {
        TerminalPayload::Run(record) => {
            let mut stdout = render::out();
            let mut stderr = render::err();
            supervised::output::print_run_output(
                record,
                &mut stdout,
                &mut stderr,
                render::prose::Prose::for_stdout(),
                render::prose::prose_width(0),
            )
        }
        TerminalPayload::Agent { last_message, .. } => supervised::output::print_final_message(
            supervised::output::FinalMessage {
                scope: "turn",
                message: last_message.as_deref(),
                completed: entry(outcome).status == RunStatus::Completed,
            },
            &mut render::out(),
            &mut render::err(),
            render::prose::Prose::for_stdout(),
            render::prose::prose_width(0),
        ),
        TerminalPayload::Disappeared => Ok(()),
    }
}

fn report_disappearance(target: &WaitTarget, outcome: &TargetOutcome) -> Result<()> {
    if matches!(&outcome.payload, TerminalPayload::Disappeared) {
        writeln!(
            render::err(),
            "{}: agent disappeared while waiting",
            target.typed_reference()
        )?;
    }
    Ok(())
}

fn report_settled_disappearances(waits: &WaitSet, settled: &[usize]) -> Result<()> {
    for &index in settled {
        let outcome = waits
            .outcome(index)
            .context("settled wait without outcome")?;
        report_disappearance(&waits.targets[index], outcome)?;
    }
    Ok(())
}

fn print_settled_blocks(waits: &WaitSet, settled: &[usize]) -> Result<()> {
    let mut out = render::out();
    let mut err = render::err();
    let prose = render::prose::Prose::for_stdout();
    for &index in settled {
        let outcome = waits
            .outcome(index)
            .context("settled wait without outcome")?;
        print_wait_block(&mut out, &mut err, outcome, prose)?;
    }
    Ok(())
}

fn print_unfinished_headers(waits: &WaitSet, ending: Unfinished) -> Result<()> {
    let mut err = render::err();
    for (target, outcome) in waits.targets.iter().zip(&waits.outcomes) {
        if outcome.is_none() {
            write_unfinished_header(
                &mut err,
                target.typed_reference(),
                unfinished_entry(target, ending).reason,
                ending,
            )?;
        }
    }
    Ok(())
}

fn write_unfinished_header(
    err: &mut impl Write,
    reference: &str,
    reason: Option<String>,
    ending: Unfinished,
) -> Result<()> {
    let status = ending.status();
    let label = reason.map_or_else(
        || status.label().to_owned(),
        |reason| format!("{}: {reason}", status.label()),
    );
    writeln!(
        err,
        "--- {} ({}) ---",
        render::paint(render::palette::body(), reference),
        render::paint(render::status::run(status), &label),
    )?;
    err.flush()?;
    Ok(())
}

fn print_unfinished_json(waits: &WaitSet, ending: Unfinished) -> Result<()> {
    let entries = waits
        .targets
        .iter()
        .zip(&waits.outcomes)
        .map(|(target, outcome)| match outcome {
            Some(outcome) => (outcome.reference.as_str(), entry(outcome)),
            None => (target.typed_reference(), unfinished_entry(target, ending)),
        })
        .collect::<BTreeMap<_, _>>();
    render::json(&entries)
}

fn wait_interactive_agent_stream(
    (store, session_name): (&rimz::Store, &str),
    typed_reference: &str,
    reference: &str,
    kind: &rimz::ids::AgentKind,
    current_channel: &rimz::address::AddressContext,
    options: WaitStreamOptions,
    mut caller: WaitCaller,
) -> Result<()> {
    let adapter = rimz::agents::find_definition(kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{kind}`"))?;
    let mut cursor = rimz::agents::transcript::TranscriptCursor::new(options.from_start);
    let mut stdout = render::out();
    let mut stderr = render::err();
    let mut json_stdout = std::io::stdout().lock();
    let mut sink = if options.json {
        supervised::output::StreamSink::ndjson(&mut json_stdout)
    } else {
        supervised::output::StreamSink::text(
            &mut stdout,
            &mut stderr,
            render::prose::Prose::for_stdout(),
            render::prose::prose_width(0),
        )
    };
    let deadline = options.timeout.map(|duration| Instant::now() + duration);
    let mut reason = None;
    loop {
        let view = TurnWaitView::load(store).context("reading agent snapshot")?;
        let agent =
            crate::cli::resolve_agent_one(store, &view.snapshot, reference, None, current_channel)?;
        let address = rimz::address::paste_address(agent, &view.snapshot, current_channel);
        report_wait_reason(
            &mut render::err(),
            &mut reason,
            view.reason(agent),
            &address,
        )?;
        for text in cursor.messages(
            agent.transcript_path.as_deref(),
            Some(&agent.agent_id),
            adapter,
        ) {
            sink.message(text)?;
        }
        sink.status(interactive_live_status(agent))?;
        match view.completion(agent) {
            TurnCompletion::Completed => {
                sink.end_status(RunStatus::Completed, None)?;
                std::process::exit(0);
            }
            TurnCompletion::Failed => {
                sink.end_status(RunStatus::Failed, None)?;
                std::process::exit(RunStatus::Failed.exit_code());
            }
            TurnCompletion::Open => {}
        }
        if let Some(announcement) = caller.queued(store, session_name) {
            stop_stream(typed_reference, reason.as_ref(), Unfinished::Running)?;
            write_message_stop(&mut render::err(), &announcement)?;
            std::process::exit(Unfinished::Running.exit_code());
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            if sink.is_text() {
                sink.timeout()?;
            }
            stop_stream(typed_reference, reason.as_ref(), Unfinished::TimedOut)?;
            std::process::exit(Unfinished::TimedOut.exit_code());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn interactive_live_status(agent: &AgentState) -> rimz::harness::run::RunLiveStatus {
    rimz::harness::run::RunLiveStatus {
        agent_status: agent.status,
        phase: agent.phase,
        pane_id: agent.pane.as_ref().map(|pane| pane.pane_id.clone()),
        context_pct: agent
            .context_fill_pct()
            .map(|pct| pct.round().clamp(0.0, 100.0) as u8),
        // An interactive agent has no supervised run to be parked.
        parked_at: None,
        permission_review_until: agent.permission_review_until(),
    }
}

/// The header a stream that stops unfinished prints, when the target has a
/// reason it has not finished.
fn stop_stream(
    typed_reference: &str,
    reason: Option<&TurnWaitReason>,
    ending: Unfinished,
) -> Result<()> {
    let Some(text) = reason.and_then(|reason| wait_reason_text(reason, jiff::Timestamp::now()))
    else {
        return Ok(());
    };
    write_unfinished_header(&mut render::err(), typed_reference, Some(text), ending)
}

fn wait_run_stream(
    (store, session_name): (&rimz::Store, &str),
    run: &RunRecord,
    typed_reference: &str,
    current_channel: &rimz::address::AddressContext,
    options: WaitStreamOptions,
    mut caller: WaitCaller,
) -> Result<()> {
    let adapter = rimz::agents::find_definition(run.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", run.kind))?;
    let mut stdout = render::out();
    let mut stderr = render::err();
    let mut json_stdout = std::io::stdout().lock();
    let mut sink = if options.json {
        supervised::output::StreamSink::ndjson(&mut json_stdout)
    } else {
        supervised::output::StreamSink::text(
            &mut stdout,
            &mut stderr,
            render::prose::Prose::for_stdout(),
            render::prose::prose_width(0),
        )
    };
    let mut reason = None;
    let mut announcement = None;
    let end = supervised::stream::stream_attached_run(
        store,
        &run.run_id,
        adapter,
        options.from_start,
        options.timeout,
        &mut sink,
        |record| {
            if let Ok(view) = TurnWaitView::load(store) {
                report_run_wait_reason(&view, record, current_channel, &mut reason)?;
            } else {
                reason = None;
            }
            announcement = caller.queued(store, session_name);
            Ok(if announcement.is_some() {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            })
        },
    )?;
    match end {
        supervised::stream::AttachedEnd::Terminal(record) => {
            std::process::exit(record.status.exit_code())
        }
        supervised::stream::AttachedEnd::TimedOut => {
            stop_stream(typed_reference, reason.as_ref(), Unfinished::TimedOut)?;
            std::process::exit(Unfinished::TimedOut.exit_code());
        }
        supervised::stream::AttachedEnd::Stopped => {
            stop_stream(typed_reference, reason.as_ref(), Unfinished::Running)?;
            if let Some(announcement) = &announcement {
                write_message_stop(&mut render::err(), announcement)?;
            }
            std::process::exit(Unfinished::Running.exit_code());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::agents::AgentStatus;

    #[test]
    fn interactive_status_keeps_raw_waiting_with_permission_review_evidence() {
        let since = jiff::Timestamp::from_second(1_000).unwrap();
        let mut agent = rimz::testkit::agent_state("codex", "review", since);
        agent.status = AgentStatus::Waiting;
        agent.waiting_since = Some(since);
        agent.open_ask = Some(rimz::agents::OpenAsk {
            extra_questions: 0,
            id: rimz::ids::AskId::new(),
            kind: rimz::agents::AskKind::Permission,
            detail: None,
            native_key: None,
            since,
        });
        for mode in [Some(PermissionMode::Auto), None] {
            agent.mode = mode;
            let live = interactive_live_status(&agent);
            assert_eq!(live.agent_status, AgentStatus::Waiting);
            let json = serde_json::to_value(live).unwrap();
            if mode.is_some() {
                assert_eq!(
                    json["permission_review_until"],
                    serde_json::json!(since + Duration::from_secs(60))
                );
            } else {
                assert!(json.get("permission_review_until").is_none());
            }
        }
    }

    #[test]
    fn a_run_canceled_because_its_parent_ended_carries_the_reason_in_json() {
        let mut record = RunRecord::new(
            rimz::WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-run")),
            rimz::ids::AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "go".to_owned(),
            std::path::Path::new("/tmp/rimz-run").to_path_buf(),
        );
        record.status = RunStatus::Canceled;
        let outcome = |record: &RunRecord| TargetOutcome {
            reference: "@otter".to_owned(),
            name: "otter".to_owned(),
            payload: TerminalPayload::Run(Box::new(record.clone())),
        };
        let json = serde_json::to_value(entry(&outcome(&record))).unwrap();
        assert!(json.get("cancel_reason").is_none(), "{json}");

        record.cancel_reason = Some(rimz::store::run::CancelReason::ParentEnded);
        let json = serde_json::to_value(entry(&outcome(&record))).unwrap();
        assert_eq!(json["cancel_reason"], "parent_ended", "{json}");
        assert_eq!(json["status"], "canceled", "{json}");
    }
}
