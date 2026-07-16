use super::*;

use super::runs_lookup::{agent_name, newest_run_by_ref};
use crate::cli::render;

pub(super) fn wait_agent(
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
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let store = crate::cli::open_store(&workspace)?;
    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    let current_channel = crate::cli::current_channel(&workspace);
    if stream_output {
        let reference = references.first().context("wait requires a reference")?;
        let target = resolve_wait_target(&store, &snapshot, reference, current_channel.as_deref())?;
        return match target {
            WaitTarget::Run { run_id, .. } => {
                let run = rimz::harness::run::load(store.paths(), &run_id)?;
                wait_run_stream(&store, &run, timeout, from_start, json)
            }
            WaitTarget::Agent { reference, kind } => wait_interactive_agent_stream(
                &store,
                &reference,
                &kind,
                snapshot,
                current_channel.as_deref(),
                timeout,
                from_start,
                json,
            ),
        };
    }

    let targets = references
        .iter()
        .map(|reference| {
            resolve_wait_target(&store, &snapshot, reference, current_channel.as_deref())
        })
        .collect::<Result<Vec<_>>>()?;
    let single = targets.len() == 1 && !any;
    let mut waits = WaitSet::new(
        targets,
        if any { JoinMode::Any } else { JoinMode::All },
        timeout,
    );
    loop {
        match waits.poll(&store, current_channel.as_deref())? {
            WaitPoll::Pending { settled } => {
                if !single {
                    report_settled_disappearances(&waits, &settled)?;
                }
                if !single && !json {
                    print_settled_statuses(&waits, &settled)?;
                }
            }
            WaitPoll::Settled { selected, settled } => {
                if single {
                    let outcome = waits
                        .outcome(selected)
                        .context("settled wait without outcome")?;
                    if matches!(&outcome.payload, TerminalPayload::Disappeared) {
                        let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
                        crate::cli::resolve_agent_one(
                            &snapshot,
                            waits.targets[selected].name(),
                            None,
                            current_channel.as_deref(),
                        )?;
                    }
                    print_single_outcome(outcome, json)?;
                    std::process::exit(outcome.exit_code());
                }
                if waits.mode == JoinMode::Any {
                    let outcome = waits
                        .outcome(selected)
                        .context("settled wait without outcome")?;
                    report_disappearance(&waits.targets[selected], outcome)?;
                    if json {
                        print_wait_json(std::iter::once(outcome))?;
                    } else {
                        writeln!(render::out(), "{}", outcome.name)?;
                        print_wait_status(&mut render::err(), outcome)?;
                    }
                    std::process::exit(outcome.exit_code());
                }
                report_settled_disappearances(&waits, &settled)?;
                if !json {
                    print_settled_statuses(&waits, &settled)?;
                } else {
                    print_wait_json(waits.outcomes.iter().flatten())?;
                }
                std::process::exit(waits.all_exit_code());
            }
            WaitPoll::TimedOut { settled } => {
                if !single {
                    report_settled_disappearances(&waits, &settled)?;
                }
                if !single && !json {
                    print_settled_statuses(&waits, &settled)?;
                } else if !single {
                    print_timeout_json(&waits)?;
                }
                std::process::exit(RunStatus::TimedOut.exit_code());
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
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

struct TargetOutcome {
    name: String,
    payload: TerminalPayload,
}

enum TerminalPayload {
    Run(RunRecord),
    Agent(AgentState),
    Disappeared,
}

impl TargetOutcome {
    fn entry(&self) -> WaitEntryJson {
        match &self.payload {
            TerminalPayload::Run(record) => WaitEntryJson::new(
                record.status,
                record.cost_usd,
                record.transcript_path.clone(),
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
            ),
            TerminalPayload::Disappeared => WaitEntryJson::new(
                RunStatus::Failed,
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
struct WaitEntryJson {
    status: RunStatus,
    exit: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transcript_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl WaitEntryJson {
    fn new(
        status: RunStatus,
        cost: Option<f64>,
        transcript_path: Option<String>,
        error: Option<String>,
    ) -> Self {
        Self {
            status,
            exit: status.exit_code(),
            cost,
            transcript_path,
            error,
        }
    }
}

fn resolve_wait_target(
    store: &rimz::Store,
    snapshot: &rimz::SidebarSnapshot,
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
    agent_snapshot: Option<&rimz::SidebarSnapshot>,
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
                payload: TerminalPayload::Run(record),
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
                        payload: TerminalPayload::Agent(agent.clone()),
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
    let mut out = render::out();
    serde_json::to_writer(&mut out, &entries)?;
    writeln!(out)?;
    out.flush()?;
    Ok(())
}

fn print_wait_status(out: &mut impl Write, outcome: &TargetOutcome) -> std::io::Result<()> {
    let entry = outcome.entry();
    writeln!(
        out,
        "{} {}",
        render::paint(render::palette::ACCENT, &outcome.name),
        render::paint(
            render::status::run(entry.status),
            supervised::output::status_label(entry.status)
        ),
    )
}

fn print_single_outcome(outcome: &TargetOutcome, json: bool) -> Result<()> {
    match &outcome.payload {
        TerminalPayload::Run(record) if json => supervised::output::print_json(record),
        TerminalPayload::Run(record) => {
            let mut stdout = render::out();
            let mut stderr = render::err();
            supervised::output::print_run_output(record, &mut stdout, &mut stderr)
        }
        TerminalPayload::Agent(agent) if json => supervised::output::print_json(agent),
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

fn print_settled_statuses(waits: &WaitSet, settled: &[usize]) -> Result<()> {
    let mut out = render::out();
    for &index in settled {
        let outcome = waits
            .outcome(index)
            .context("settled wait without outcome")?;
        print_wait_status(&mut out, outcome)?;
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
                WaitEntryJson::new(RunStatus::TimedOut, None, None, None),
            ),
        })
        .collect::<BTreeMap<_, _>>();
    let mut out = render::out();
    serde_json::to_writer(&mut out, &entries)?;
    writeln!(out)?;
    out.flush()?;
    Ok(())
}

fn wait_interactive_agent_stream(
    store: &rimz::Store,
    reference: &str,
    kind: &rimz::ids::AgentKind,
    mut snapshot: rimz::SidebarSnapshot,
    current_channel: Option<&str>,
    timeout: Option<Duration>,
    from_start: bool,
    json: bool,
) -> Result<()> {
    let adapter = rimz::agents::find_adapter(kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{kind}`"))?;
    let mut cursor = rimz::agents::transcript::TranscriptCursor::new(from_start);
    let mut stdout = render::out();
    let mut stderr = render::err();
    let mut json_stdout = std::io::stdout().lock();
    let mut sink = if json {
        supervised::output::StreamSink::ndjson(&mut json_stdout)
    } else {
        supervised::output::StreamSink::text(&mut stdout, &mut stderr)
    };
    let deadline = timeout.map(|duration| Instant::now() + duration);
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

fn wait_run_stream(
    store: &rimz::Store,
    run: &RunRecord,
    timeout: Option<Duration>,
    from_start: bool,
    json: bool,
) -> Result<()> {
    let adapter = rimz::agents::find_adapter(run.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", run.kind))?;
    let mut stdout = render::out();
    let mut stderr = render::err();
    let mut json_stdout = std::io::stdout().lock();
    let mut sink = if json {
        supervised::output::StreamSink::ndjson(&mut json_stdout)
    } else {
        supervised::output::StreamSink::text(&mut stdout, &mut stderr)
    };
    match supervised::stream::stream_attached_run(
        store,
        &run.run_id,
        adapter,
        from_start,
        timeout,
        &mut sink,
    )? {
        Some(record) => std::process::exit(record.status.exit_code()),
        None => std::process::exit(RunStatus::TimedOut.exit_code()),
    }
}
