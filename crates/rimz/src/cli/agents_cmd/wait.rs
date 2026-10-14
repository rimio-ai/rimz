use super::*;

use super::runs_lookup::{agent_name, newest_run_by_ref};
use crate::cli::render;
use rimz::agents::TurnCompletion;
use rimz::message::reply::{TurnWaitReason, TurnWaitView};

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
            Some(TurnWaitReason::WakeInFlight) => "it has a ",
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
        TurnWaitReason::Paused => "paused".to_owned(),
        TurnWaitReason::NoTurnStarted => "no turn started".to_owned(),
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
    let mut seen = std::collections::HashSet::new();
    references.retain(|(typed, _)| seen.insert(typed.clone()));
    if stream_output && references.len() > 1 {
        bail!("--stream tails one target; wait on a single reference");
    }
    if stream_output {
        let ctx = Ctx::open(globals)?;
        let store = &ctx.store;
        let snapshot = ctx.cached_snapshot()?;
        let current_channel = &ctx.address_context();
        let options = WaitStreamOptions {
            timeout,
            from_start,
            json,
        };
        let (typed_reference, reference) =
            references.first().context("wait requires a reference")?;
        return wait_stream_request(
            store,
            snapshot,
            typed_reference,
            reference,
            current_channel,
            options,
        );
    }

    let style = WaitStyle::new(references.len(), any, json);
    wait_non_stream_request(references, any, timeout, style, globals)
}

fn wait_non_stream_request(
    references: Vec<(String, String)>,
    any: bool,
    timeout: Option<Duration>,
    style: WaitStyle,
    globals: &GlobalFlags,
) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.cached_snapshot()?;
    let current_channel = &ctx.address_context();
    wait_non_stream(
        &ctx,
        &snapshot,
        references,
        any,
        timeout,
        style,
        current_channel,
    )
}

#[derive(Clone, Copy)]
struct WaitStreamOptions {
    timeout: Option<Duration>,
    from_start: bool,
    json: bool,
}

fn wait_stream_request(
    store: &rimz::Store,
    snapshot: rimz::store::snapshot::SidebarSnapshot,
    typed_reference: &str,
    reference: &str,
    current_channel: &rimz::address::AddressContext,
    options: WaitStreamOptions,
) -> Result<()> {
    match resolve_wait_target(store, &snapshot, reference, reference, current_channel)? {
        WaitTarget::Run { run_id, .. } => {
            let run = rimz::harness::run::load(store.paths(), &run_id)?;
            wait_run_stream(store, &run, typed_reference, current_channel, options)
        }
        WaitTarget::Agent {
            reference, kind, ..
        } => wait_interactive_agent_stream(
            store,
            typed_reference,
            &reference,
            &kind,
            current_channel,
            options,
        ),
    }
}

fn wait_non_stream(
    ctx: &Ctx,
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    references: Vec<(String, String)>,
    any: bool,
    timeout: Option<Duration>,
    style: WaitStyle,
    current_channel: &rimz::address::AddressContext,
) -> Result<()> {
    let store = &ctx.store;
    let session_name = &ctx.workspace.session_name;
    let targets = references
        .iter()
        .map(|(typed, reference)| {
            resolve_wait_target(store, snapshot, typed, reference, current_channel)
        })
        .collect::<Result<Vec<_>>>()?;
    let mut waits = WaitSet::new(
        targets,
        if any { JoinMode::Any } else { JoinMode::All },
        timeout,
    );
    loop {
        match waits.poll(store, current_channel)? {
            WaitPoll::Pending { settled } => {
                style.report_progress(store, session_name, &waits, &settled)?;
            }
            WaitPoll::Settled { selected, settled } => {
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
            WaitPoll::TimedOut { settled } => {
                style.report_timeout(store, session_name, &waits, &settled)?;
                render::err().flush()?;
                std::process::exit(RunStatus::TimedOut.exit_code());
            }
        }
        std::thread::sleep(Duration::from_millis(500));
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

    fn report_timeout(
        self,
        store: &rimz::Store,
        session_name: &str,
        waits: &WaitSet,
        settled: &[usize],
    ) -> Result<()> {
        match self {
            Self::Single { json: true } => print_timeout_json(waits),
            Self::Single { json: false } => print_pending_timeouts(waits),
            Self::Any { json } | Self::All { json } => {
                report_settled_disappearances(waits, settled)?;
                if json {
                    mark_joined(store, session_name, waits.outcomes.iter().flatten());
                    print_timeout_json(waits)
                } else {
                    mark_settled_joined(store, session_name, waits, settled)?;
                    print_settled_blocks(waits, settled)?;
                    print_pending_timeouts(waits)
                }
            }
        }
    }

    fn exit_code(self, waits: &WaitSet, selected: usize) -> Result<i32> {
        match self {
            Self::Single { .. } | Self::Any { .. } => {
                Ok(settled_outcome(waits, selected)?.exit_code())
            }
            Self::All { .. } => Ok(waits.all_exit_code()),
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

fn mark_joined<'a>(
    store: &rimz::Store,
    session_name: &str,
    outcomes: impl IntoIterator<Item = &'a TargetOutcome>,
) {
    let attended = std::cell::LazyCell::new(|| match store.snapshot_cached() {
        Ok(snapshot) => rimz::harness::ancestry::resolve_calling_agent(&snapshot.agents)
            .map_or(true, AgentState::holds_open_turn),
        Err(err) => {
            tracing::warn!(error = %err, "could not read the caller's turn for the joined run");
            true
        }
    });
    for outcome in outcomes {
        let TerminalPayload::Run(record) = &outcome.payload else {
            continue;
        };
        if !*attended {
            tracing::debug!(
                run_id = %record.run_id,
                "result printed outside the caller's turn; left unjoined for the digest",
            );
            continue;
        }
        let result = rimz::harness::run::report::join_and_settle_digest(
            store,
            session_name,
            &record.run_id,
            Some(record.follow_ups + 1),
            "joined inline",
        );
        if let Err(err) = result {
            tracing::warn!(
                run_id = %record.run_id,
                error = %err,
                "could not mark the joined run",
            );
        }
    }
}

fn settled_outcome(waits: &WaitSet, index: usize) -> Result<&TargetOutcome> {
    waits.outcome(index).context("settled wait without outcome")
}

enum WaitTarget {
    Run {
        run_id: rimz::RunId,
        name: String,
        typed_reference: String,
        reason: Option<TurnWaitReason>,
    },
    Agent {
        reference: String,
        typed_reference: String,
        name: String,
        reason: Option<TurnWaitReason>,
        kind: rimz::ids::AgentKind,
        completed_seen_at: Option<Instant>,
    },
}

impl WaitTarget {
    fn typed_reference(&self) -> &str {
        match self {
            Self::Run {
                typed_reference, ..
            }
            | Self::Agent {
                typed_reference, ..
            } => typed_reference,
        }
    }

    fn timeout_entry(&self) -> WaitEntryJson {
        let (name, reason) = match self {
            Self::Run { name, reason, .. } | Self::Agent { name, reason, .. } => (name, reason),
        };
        let mut entry =
            WaitEntryJson::new(name.clone(), RunStatus::TimedOut, None, None, None, None);
        entry.reason = reason
            .as_ref()
            .and_then(|reason| wait_reason_text(reason, jiff::Timestamp::now()));
        entry
    }
}

pub(super) struct TargetOutcome {
    pub(super) reference: String,
    pub(super) name: String,
    pub(super) payload: TerminalPayload,
}

pub(super) enum TerminalPayload {
    Run(Box<RunRecord>),
    Agent {
        agent: Box<AgentState>,
        last_message: Option<String>,
    },
    Disappeared,
}

impl TargetOutcome {
    pub(super) fn entry(&self) -> WaitEntryJson {
        match &self.payload {
            TerminalPayload::Run(record) => WaitEntryJson::new(
                self.name.clone(),
                record.status,
                record.cost_usd,
                record.transcript_path.clone(),
                record.last_message.clone(),
                (record.status == RunStatus::Failed)
                    .then(|| record.failure_tail.clone())
                    .flatten(),
            ),
            TerminalPayload::Agent {
                agent,
                last_message,
            } => WaitEntryJson::new(
                self.name.clone(),
                if agent.effective_status() == rimz::agents::AgentStatus::Failed {
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
                self.name.clone(),
                RunStatus::Failed,
                None,
                None,
                None,
                Some("agent disappeared while waiting".to_owned()),
            ),
        }
    }

    fn exit_code(&self) -> i32 {
        self.entry().exit
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
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
            error,
            reason: None,
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
            || (run.subagent && live_agent.is_some_and(AgentState::is_launched_child)))
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

/// How long a completed turn may wait for its final message to reach the
/// transcript. The hook helper records the turn's end before it appends the
/// message, and a provider whose hook carries none settles after this grace.
const FINAL_MESSAGE_GRACE: Duration = Duration::from_secs(3);

/// Decide whether an interactive turn has settled, and with which message.
/// `None` keeps waiting; `Some(message)` settles. Only a message recorded
/// during the current turn counts. A wait deadline cuts the grace short, since
/// the turn itself has already completed.
pub(super) fn settle_agent_turn(
    completion: TurnCompletion,
    turn_started_at: Option<jiff::Timestamp>,
    latest: Option<rimz::transcript::TranscriptEntry>,
    completed_seen_at: &mut Option<Instant>,
    (now, deadline): (Instant, Option<Instant>),
) -> Option<Option<String>> {
    if completion == TurnCompletion::Open {
        *completed_seen_at = None;
        return None;
    }
    let message = latest
        .filter(|entry| turn_started_at.is_some_and(|started| entry.at >= started))
        .map(|entry| entry.text);
    if message.is_some() || completion == TurnCompletion::Failed {
        return Some(message);
    }
    let seen = *completed_seen_at.get_or_insert(now);
    (now.duration_since(seen) >= FINAL_MESSAGE_GRACE
        || deadline.is_some_and(|deadline| now >= deadline))
    .then_some(None)
}

fn poll_target(
    store: &rimz::Store,
    agent_view: Option<&TurnWaitView>,
    target: &mut WaitTarget,
    current_channel: &rimz::address::AddressContext,
    deadline: Option<Instant>,
) -> Result<Option<TargetOutcome>> {
    match target {
        WaitTarget::Run {
            run_id,
            name,
            typed_reference,
            reason,
        } => {
            let record = rimz::harness::run::load(store.paths(), run_id)?;
            *name = record
                .agent_name
                .clone()
                .unwrap_or_else(|| run_id.to_string());
            if !record.status.is_terminal() {
                if let Some(view) = agent_view {
                    report_run_wait_reason(view, &record, current_channel, reason)?;
                } else {
                    *reason = None;
                }
                return Ok(None);
            }
            Ok(Some(TargetOutcome {
                reference: typed_reference.clone(),
                name: name.clone(),
                payload: TerminalPayload::Run(Box::new(record)),
            }))
        }
        WaitTarget::Agent {
            reference,
            typed_reference,
            name,
            reason,
            completed_seen_at,
            ..
        } => {
            let view = agent_view.context("pending agent target without snapshot")?;
            match crate::cli::resolve_agent_one(
                store,
                &view.snapshot,
                reference,
                None,
                current_channel,
            ) {
                Ok(agent) => {
                    let address =
                        rimz::address::paste_address(agent, &view.snapshot, current_channel);
                    report_wait_reason(&mut render::err(), reason, view.reason(agent), &address)?;
                    *name = agent_name(agent).to_owned();
                    let completion = view.completion(agent);
                    let latest = match agent.turn_started_at {
                        Some(since) if completion != TurnCompletion::Open => {
                            rimz::transcript::latest_assistant(
                                store.paths(),
                                &agent.kind,
                                &agent.agent_id,
                                since,
                            )
                            .context("reading the agent's final message")?
                        }
                        _ => None,
                    };
                    let settled = settle_agent_turn(
                        completion,
                        agent.turn_started_at,
                        latest,
                        completed_seen_at,
                        (Instant::now(), deadline),
                    );
                    Ok(settled.map(|last_message| TargetOutcome {
                        reference: typed_reference.clone(),
                        name: agent_name(agent).to_owned(),
                        payload: TerminalPayload::Agent {
                            agent: Box::new(agent.clone()),
                            last_message,
                        },
                    }))
                }
                Err(_) => Ok(Some(TargetOutcome {
                    reference: typed_reference.clone(),
                    name: name.clone(),
                    payload: TerminalPayload::Disappeared,
                })),
            }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JoinMode {
    Any,
    All,
}

struct WaitSet {
    targets: Vec<WaitTarget>,
    outcomes: Vec<Option<TargetOutcome>>,
    deadline: Option<Instant>,
    mode: JoinMode,
}

enum WaitPoll {
    Pending {
        settled: Vec<usize>,
    },
    Settled {
        selected: usize,
        settled: Vec<usize>,
    },
    TimedOut {
        settled: Vec<usize>,
    },
}

impl WaitSet {
    fn new(targets: Vec<WaitTarget>, mode: JoinMode, timeout: Option<Duration>) -> Self {
        let outcomes = std::iter::repeat_with(|| None)
            .take(targets.len())
            .collect();
        Self {
            targets,
            outcomes,
            deadline: timeout.map(|duration| Instant::now() + duration),
            mode,
        }
    }

    fn poll(
        &mut self,
        store: &rimz::Store,
        current_channel: &rimz::address::AddressContext,
    ) -> Result<WaitPoll> {
        let pending_agent = self
            .targets
            .iter()
            .zip(&self.outcomes)
            .any(|(target, outcome)| {
                matches!(target, WaitTarget::Agent { .. }) && outcome.is_none()
            });
        let agent_view = if pending_agent {
            Some(TurnWaitView::load(store).context("reading agent snapshot")?)
        } else if self.outcomes.iter().any(Option::is_none) {
            TurnWaitView::load(store).ok()
        } else {
            None
        };

        let mut settled = Vec::new();
        for (index, target) in self.targets.iter_mut().enumerate() {
            if self.outcomes[index].is_some() {
                continue;
            }
            let Some(outcome) = poll_target(
                store,
                agent_view.as_ref(),
                target,
                current_channel,
                self.deadline,
            )?
            else {
                continue;
            };
            self.outcomes[index] = Some(outcome);
            settled.push(index);
            if self.mode == JoinMode::Any {
                return Ok(WaitPoll::Settled {
                    selected: index,
                    settled,
                });
            }
        }

        if self.outcomes.iter().all(Option::is_some) {
            return Ok(WaitPoll::Settled {
                selected: 0,
                settled,
            });
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Ok(WaitPoll::TimedOut { settled });
        }
        Ok(WaitPoll::Pending { settled })
    }

    fn outcome(&self, index: usize) -> Option<&TargetOutcome> {
        self.outcomes.get(index)?.as_ref()
    }

    fn all_exit_code(&self) -> i32 {
        self.outcomes
            .iter()
            .flatten()
            .find(|outcome| outcome.entry().status != RunStatus::Completed)
            .map_or(0, TargetOutcome::exit_code)
    }
}

fn print_wait_json<'a>(outcomes: impl IntoIterator<Item = &'a TargetOutcome>) -> Result<()> {
    let entries = outcomes
        .into_iter()
        .map(|outcome| (outcome.reference.as_str(), outcome.entry()))
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
        let completed = outcome.entry().status == RunStatus::Completed;
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
    let entry = outcome.entry();
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
                completed: outcome.entry().status == RunStatus::Completed,
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

fn print_pending_timeouts(waits: &WaitSet) -> Result<()> {
    let mut err = render::err();
    for (target, outcome) in waits.targets.iter().zip(&waits.outcomes) {
        if outcome.is_none() {
            write_timeout_header(
                &mut err,
                target.typed_reference(),
                target.timeout_entry().reason,
            )?;
        }
    }
    Ok(())
}

fn write_timeout_header(
    err: &mut impl Write,
    reference: &str,
    reason: Option<String>,
) -> Result<()> {
    let label = reason.map_or_else(
        || "timed out".to_owned(),
        |reason| format!("timed out: {reason}"),
    );
    writeln!(
        err,
        "--- {} ({}) ---",
        render::paint(render::palette::body(), reference),
        render::paint(render::status::run(RunStatus::TimedOut), &label),
    )?;
    err.flush()?;
    Ok(())
}

fn print_timeout_json(waits: &WaitSet) -> Result<()> {
    let entries = waits
        .targets
        .iter()
        .zip(&waits.outcomes)
        .map(|(target, outcome)| match outcome {
            Some(outcome) => (outcome.reference.as_str(), outcome.entry()),
            None => (target.typed_reference(), target.timeout_entry()),
        })
        .collect::<BTreeMap<_, _>>();
    render::json(&entries)
}

fn wait_interactive_agent_stream(
    store: &rimz::Store,
    typed_reference: &str,
    reference: &str,
    kind: &rimz::ids::AgentKind,
    current_channel: &rimz::address::AddressContext,
    options: WaitStreamOptions,
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
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            if sink.is_text() {
                sink.timeout()?;
            }
            if let Some(text) = reason
                .as_ref()
                .and_then(|reason| wait_reason_text(reason, jiff::Timestamp::now()))
            {
                write_timeout_header(&mut render::err(), typed_reference, Some(text))?;
            }
            std::process::exit(RunStatus::TimedOut.exit_code());
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
    }
}

fn wait_run_stream(
    store: &rimz::Store,
    run: &RunRecord,
    typed_reference: &str,
    current_channel: &rimz::address::AddressContext,
    options: WaitStreamOptions,
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
    match supervised::stream::stream_attached_run(
        store,
        &run.run_id,
        adapter,
        options.from_start,
        options.timeout,
        &mut sink,
        |record| {
            if let Ok(view) = TurnWaitView::load(store) {
                return report_run_wait_reason(&view, record, current_channel, &mut reason);
            }
            reason = None;
            Ok(())
        },
    )? {
        Some(record) => std::process::exit(record.status.exit_code()),
        None => {
            if let Some(text) = reason
                .as_ref()
                .and_then(|reason| wait_reason_text(reason, jiff::Timestamp::now()))
            {
                write_timeout_header(&mut render::err(), typed_reference, Some(text))?;
            }
            std::process::exit(RunStatus::TimedOut.exit_code());
        }
    }
}
