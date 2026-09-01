use super::*;

use super::runs_lookup::{agent_name, newest_run_by_ref};
use crate::cli::render;

pub(in crate::cli) fn wait_agent(
    references: Vec<String>,
    any: bool,
    timeout: Option<Duration>,
    stream_output: bool,
    from_start: bool,
    json: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    if stream_output && references.len() > 1 {
        bail!("--stream tails one target; wait on a single reference");
    }
    if stream_output {
        let ctx = Ctx::open(globals)?;
        let store = &ctx.store;
        let snapshot = ctx.cached_snapshot()?;
        let current_channel = ctx.channel();
        let options = WaitStreamOptions {
            timeout,
            from_start,
            json,
        };
        return wait_stream_request(
            store,
            snapshot,
            references.first().context("wait requires a reference")?,
            current_channel,
            options,
        );
    }

    let style = WaitStyle::new(references.len(), any, json);
    wait_non_stream_request(references, any, timeout, style, globals)
}

fn wait_non_stream_request(
    references: Vec<String>,
    any: bool,
    timeout: Option<Duration>,
    style: WaitStyle,
    globals: &GlobalFlags,
) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.cached_snapshot()?;
    let current_channel = ctx.channel();
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
    reference: &str,
    current_channel: Option<&str>,
    options: WaitStreamOptions,
) -> Result<()> {
    match resolve_wait_target(store, &snapshot, reference, current_channel)? {
        WaitTarget::Run { run_id, .. } => {
            let run = rimz::harness::run::load(store.paths(), &run_id)?;
            wait_run_stream(store, &run, options)
        }
        WaitTarget::Agent { reference, kind } => wait_interactive_agent_stream(
            store,
            &reference,
            &kind,
            snapshot,
            current_channel,
            options,
        ),
    }
}

fn wait_non_stream(
    ctx: &Ctx,
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    references: Vec<String>,
    any: bool,
    timeout: Option<Duration>,
    style: WaitStyle,
    current_channel: Option<&str>,
) -> Result<()> {
    let store = &ctx.store;
    let session_name = &ctx.workspace.session_name;
    let targets = references
        .iter()
        .map(|reference| resolve_wait_target(store, snapshot, reference, current_channel))
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
                std::process::exit(style.exit_code(&waits, selected)?);
            }
            WaitPoll::TimedOut { settled } => {
                style.report_timeout(store, session_name, &waits, &settled)?;
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
        current_channel: Option<&str>,
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
                        &snapshot,
                        waits.targets[selected].name(),
                        None,
                        current_channel,
                    )?;
                    return Ok(());
                }
                mark_joined(store, session_name, std::iter::once(outcome));
                print_single_outcome(outcome, json)
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
            Self::Single { .. } => Ok(()),
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
    for outcome in outcomes {
        let TerminalPayload::Run(record) = &outcome.payload else {
            continue;
        };
        let result = (|| -> Result<()> {
            let joined = rimz::harness::run::report::mark_joined(store.paths(), &record.run_id)?;
            if let Some(message_id) = joined.report_message_id
                && rimz::harness::run::report::digest_fully_joined(store.paths(), &message_id)?
            {
                store.cancel_message(&message_id, session_name, "joined inline")?;
            }
            Ok(())
        })();
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
    },
    Agent {
        reference: String,
        kind: rimz::ids::AgentKind,
    },
}

impl WaitTarget {
    fn name(&self) -> &str {
        match self {
            Self::Run { name, .. } => name,
            Self::Agent { reference, .. } => reference,
        }
    }
}

pub(super) struct TargetOutcome {
    pub(super) name: String,
    pub(super) payload: TerminalPayload,
}

pub(super) enum TerminalPayload {
    Run(Box<RunRecord>),
    Agent(Box<AgentState>),
    Disappeared,
}

impl TargetOutcome {
    pub(super) fn entry(&self) -> WaitEntryJson {
        match &self.payload {
            TerminalPayload::Run(record) => WaitEntryJson::new(
                record.status,
                record.cost_usd,
                record.transcript_path.clone(),
                record.last_message.clone(),
                (record.status == RunStatus::Failed)
                    .then(|| record.failure_tail.clone())
                    .flatten(),
            ),
            TerminalPayload::Agent(agent) => WaitEntryJson::new(
                if agent.status == rimz::agents::AgentStatus::Failed {
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
                None,
                None,
            ),
            TerminalPayload::Disappeared => WaitEntryJson::new(
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
}

impl WaitEntryJson {
    fn new(
        status: RunStatus,
        cost: Option<f64>,
        transcript_path: Option<String>,
        last_message: Option<String>,
        error: Option<String>,
    ) -> Self {
        Self {
            status,
            exit: status.exit_code(),
            cost,
            transcript_path,
            last_message,
            error,
        }
    }
}

fn resolve_wait_target(
    store: &rimz::Store,
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    reference: &str,
    current_channel: Option<&str>,
) -> Result<WaitTarget> {
    let live_agent_result =
        crate::cli::resolve_agent_one(snapshot, reference, None, current_channel);
    let live_agent = live_agent_result.as_ref().ok().copied();
    if let Some(run) = newest_run_by_ref(store, reference, live_agent)?
        && (!run.status.is_terminal() || live_agent.is_none() || run.run_id.as_str() == reference)
    {
        let name = run
            .agent_name
            .clone()
            .unwrap_or_else(|| run.run_id.as_str().to_owned());
        return Ok(WaitTarget::Run {
            run_id: run.run_id,
            name,
        });
    }
    if live_agent.is_none() {
        live_agent_result?;
    }
    Ok(WaitTarget::Agent {
        reference: reference.to_owned(),
        kind: live_agent
            .context("resolved wait agent without state")?
            .kind
            .clone(),
    })
}

fn poll_target(
    store: &rimz::Store,
    agent_snapshot: Option<&rimz::store::snapshot::SidebarSnapshot>,
    target: &WaitTarget,
    current_channel: Option<&str>,
) -> Result<Option<TargetOutcome>> {
    match target {
        WaitTarget::Run { run_id, name } => {
            let record = rimz::harness::run::load(store.paths(), run_id)?;
            if !record.status.is_terminal() {
                return Ok(None);
            }
            Ok(Some(TargetOutcome {
                name: name.clone(),
                payload: TerminalPayload::Run(Box::new(record)),
            }))
        }
        WaitTarget::Agent { reference, .. } => {
            let snapshot = agent_snapshot.context("pending agent target without snapshot")?;
            match crate::cli::resolve_agent_one(snapshot, reference, None, current_channel) {
                Ok(agent) => {
                    let terminal = gate_open(DeliveryGate::Done, agent.status)
                        || agent.status == rimz::agents::AgentStatus::Failed;
                    Ok(terminal.then(|| TargetOutcome {
                        name: agent_name(agent).to_owned(),
                        payload: TerminalPayload::Agent(Box::new(agent.clone())),
                    }))
                }
                Err(_) => Ok(Some(TargetOutcome {
                    name: reference.clone(),
                    payload: TerminalPayload::Disappeared,
                })),
            }
        }
    }
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

    fn poll(&mut self, store: &rimz::Store, current_channel: Option<&str>) -> Result<WaitPoll> {
        let agent_snapshot = self
            .targets
            .iter()
            .zip(&self.outcomes)
            .any(|(target, outcome)| {
                outcome.is_none() && matches!(target, WaitTarget::Agent { .. })
            })
            .then(|| store.snapshot_cached().context("reading agent snapshot"))
            .transpose()?;

        let mut settled = Vec::new();
        for (index, target) in self.targets.iter().enumerate() {
            if self.outcomes[index].is_some() {
                continue;
            }
            let Some(outcome) =
                poll_target(store, agent_snapshot.as_ref(), target, current_channel)?
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
        .map(|outcome| (outcome.name.as_str(), outcome.entry()))
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
    if entry.status == RunStatus::Completed {
        writeln!(
            out,
            "--- {} ---",
            render::paint(render::palette::body(), &outcome.name)
        )?;
    } else {
        writeln!(
            out,
            "--- {} ({}) ---",
            render::paint(render::palette::body(), &outcome.name),
            render::paint(
                render::status::run(entry.status),
                supervised::output::status_label(entry.status)
            ),
        )?;
    }
    Ok(())
}

fn print_single_outcome(outcome: &TargetOutcome, json: bool) -> Result<()> {
    match &outcome.payload {
        TerminalPayload::Run(record) if json => render::json_pretty(record),
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
        TerminalPayload::Agent(agent) if json => render::json_pretty(agent),
        TerminalPayload::Agent(_) | TerminalPayload::Disappeared => Ok(()),
    }
}

fn report_disappearance(target: &WaitTarget, outcome: &TargetOutcome) -> Result<()> {
    if matches!(&outcome.payload, TerminalPayload::Disappeared) {
        writeln!(
            render::err(),
            "{}: agent disappeared while waiting",
            target.name()
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
            writeln!(
                err,
                "--- {} ({}) ---",
                render::paint(render::palette::body(), target.name()),
                render::paint(render::status::run(RunStatus::TimedOut), "timed out"),
            )?;
        }
    }
    Ok(())
}

fn print_timeout_json(waits: &WaitSet) -> Result<()> {
    let entries = waits
        .targets
        .iter()
        .zip(&waits.outcomes)
        .map(|(target, outcome)| match outcome {
            Some(outcome) => (outcome.name.as_str(), outcome.entry()),
            None => (
                target.name(),
                WaitEntryJson::new(RunStatus::TimedOut, None, None, None, None),
            ),
        })
        .collect::<BTreeMap<_, _>>();
    render::json(&entries)
}

fn wait_interactive_agent_stream(
    store: &rimz::Store,
    reference: &str,
    kind: &rimz::ids::AgentKind,
    mut snapshot: rimz::store::snapshot::SidebarSnapshot,
    current_channel: Option<&str>,
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
    loop {
        let agent = crate::cli::resolve_agent_one(&snapshot, reference, None, current_channel)?;
        for text in cursor.messages(
            agent.transcript_path.as_deref(),
            Some(&agent.agent_id),
            adapter,
        ) {
            sink.message(text)?;
        }
        sink.status(interactive_live_status(agent))?;
        if gate_open(DeliveryGate::Done, agent.status) {
            sink.end_status(RunStatus::Completed, None)?;
            std::process::exit(0);
        }
        if agent.status == rimz::agents::AgentStatus::Failed {
            sink.end_status(RunStatus::Failed, None)?;
            std::process::exit(RunStatus::Failed.exit_code());
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            if sink.is_text() {
                sink.timeout()?;
            }
            std::process::exit(RunStatus::TimedOut.exit_code());
        }
        std::thread::sleep(Duration::from_millis(500));
        snapshot = store.snapshot_cached().context("reading agent snapshot")?;
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
    }
}

fn wait_run_stream(store: &rimz::Store, run: &RunRecord, options: WaitStreamOptions) -> Result<()> {
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
    match supervised::stream::stream_attached_run(
        store,
        &run.run_id,
        adapter,
        options.from_start,
        options.timeout,
        &mut sink,
    )? {
        Some(record) => std::process::exit(record.status.exit_code()),
        None => std::process::exit(RunStatus::TimedOut.exit_code()),
    }
}
