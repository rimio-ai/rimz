use super::*;

use super::runs_lookup::{agent_name, newest_run_by_ref};
use crate::cli::render;

pub(super) fn wait_agent(
    mut references: Vec<String>,
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
    if references.len() == 1 && !any {
        let reference = references
            .pop()
            .ok_or_else(|| anyhow::anyhow!("wait requires a reference"))?;
        return wait_one(reference, timeout, stream_output, from_start, json, globals);
    }

    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let store = crate::cli::open_store(&workspace)?;
    wait_multi(&store, &workspace, references, any, timeout, json)
}

fn wait_one(
    reference: String,
    timeout: Option<Duration>,
    stream_output: bool,
    from_start: bool,
    json: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let store = crate::cli::open_store(&workspace)?;
    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    let current_channel = crate::cli::current_channel(&workspace);
    let target = resolve_wait_target(&store, &snapshot, &reference, current_channel.as_deref())?;
    if let WaitTarget::Run { run_id, .. } = &target {
        let run = rimz::harness::run::load(store.paths(), run_id)?;
        return wait_run_record(&store, &run, timeout, stream_output, from_start, json);
    }
    if stream_output {
        return wait_interactive_agent_stream(
            &store,
            &reference,
            current_channel.as_deref(),
            timeout,
            from_start,
            json,
        );
    }
    let deadline = timeout.map(|duration| Instant::now() + duration);
    loop {
        let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
        let Some(outcome) =
            poll_target(&store, Some(&snapshot), &target, current_channel.as_deref())?
        else {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                std::process::exit(RunStatus::TimedOut.exit_code());
            }
            std::thread::sleep(Duration::from_millis(500));
            continue;
        };
        if outcome.disappeared {
            crate::cli::resolve_agent_one(&snapshot, &reference, None, current_channel.as_deref())?;
            return Ok(());
        }
        if json {
            supervised::output::print_json(
                outcome
                    .agent
                    .as_ref()
                    .context("agent wait outcome without agent state")?,
            )?;
        }
        std::process::exit(outcome.entry.exit);
    }
}

enum WaitTarget {
    Run { run_id: rimz::RunId, name: String },
    Agent { reference: String },
}

impl WaitTarget {
    fn name(&self) -> &str {
        match self {
            Self::Run { name, .. } => name,
            Self::Agent { reference } => reference,
        }
    }
}

struct TargetOutcome {
    name: String,
    entry: WaitEntryJson,
    agent: Option<AgentState>,
    disappeared: bool,
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
            let status = record.status;
            Ok(Some(TargetOutcome {
                name: name.clone(),
                entry: WaitEntryJson::new(
                    status,
                    record.cost_usd,
                    record.transcript_path,
                    if status == RunStatus::Failed {
                        record.failure_tail
                    } else {
                        None
                    },
                ),
                agent: None,
                disappeared: false,
            }))
        }
        WaitTarget::Agent { reference } => {
            let snapshot = agent_snapshot.context("pending agent target without snapshot")?;
            match crate::cli::resolve_agent_one(snapshot, reference, None, current_channel) {
                Ok(agent) => {
                    let status = if gate_open(DeliveryGate::Done, agent.status) {
                        Some(RunStatus::Completed)
                    } else if agent.status == rimz::agents::AgentStatus::Failed {
                        Some(RunStatus::Failed)
                    } else {
                        None
                    };
                    Ok(status.map(|status| TargetOutcome {
                        name: agent_name(agent).to_owned(),
                        entry: WaitEntryJson::new(
                            status,
                            agent
                                .context
                                .as_ref()
                                .and_then(|context| context.cost.as_ref())
                                .and_then(|cost| cost.total_cost_usd),
                            agent.transcript_path.clone(),
                            None,
                        ),
                        agent: Some(agent.clone()),
                        disappeared: false,
                    }))
                }
                Err(_) => Ok(Some(TargetOutcome {
                    name: reference.clone(),
                    entry: WaitEntryJson::new(
                        RunStatus::Failed,
                        None,
                        None,
                        Some("agent disappeared while waiting".to_owned()),
                    ),
                    agent: None,
                    disappeared: true,
                })),
            }
        }
    }
}

fn wait_multi(
    store: &rimz::Store,
    workspace: &rimz::ResolvedWorkspace,
    references: Vec<String>,
    any: bool,
    timeout: Option<Duration>,
    json: bool,
) -> Result<()> {
    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    let current_channel = crate::cli::current_channel(workspace);
    let targets = references
        .iter()
        .map(|reference| {
            resolve_wait_target(store, &snapshot, reference, current_channel.as_deref())
        })
        .collect::<Result<Vec<_>>>()?;
    let deadline = timeout.map(|duration| Instant::now() + duration);
    let mut outcomes: Vec<Option<TargetOutcome>> = std::iter::repeat_with(|| None)
        .take(targets.len())
        .collect();
    loop {
        let agent_snapshot = targets
            .iter()
            .zip(&outcomes)
            .any(|(target, outcome)| {
                outcome.is_none() && matches!(target, WaitTarget::Agent { .. })
            })
            .then(|| store.snapshot_cached().context("reading agent snapshot"))
            .transpose()?;

        for (index, target) in targets.iter().enumerate() {
            if outcomes[index].is_some() {
                continue;
            }
            let Some(outcome) = poll_target(
                store,
                agent_snapshot.as_ref(),
                target,
                current_channel.as_deref(),
            )?
            else {
                continue;
            };
            if outcome.disappeared {
                writeln!(
                    render::err(),
                    "{}: agent disappeared while waiting",
                    target.name()
                )?;
            }
            if any {
                if json {
                    print_wait_json(std::iter::once(&outcome))?;
                } else {
                    writeln!(render::out(), "{}", outcome.name)?;
                    print_wait_status(&mut render::err(), &outcome)?;
                }
                std::process::exit(outcome.entry.exit);
            }
            if !json {
                print_wait_status(&mut render::out(), &outcome)?;
            }
            outcomes[index] = Some(outcome);
        }

        if outcomes.iter().all(Option::is_some) {
            let exit_code = outcomes
                .iter()
                .flatten()
                .find(|outcome| outcome.entry.status != RunStatus::Completed)
                .map_or(0, |outcome| outcome.entry.exit);
            if json {
                print_wait_json(outcomes.iter().flatten())?;
            }
            std::process::exit(exit_code);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            if json {
                for (target, outcome) in targets.iter().zip(&mut outcomes) {
                    if outcome.is_some() {
                        continue;
                    }
                    *outcome = Some(TargetOutcome {
                        name: target.name().to_owned(),
                        entry: WaitEntryJson::new(RunStatus::TimedOut, None, None, None),
                        agent: None,
                        disappeared: false,
                    });
                }
                print_wait_json(outcomes.iter().flatten())?;
            }
            std::process::exit(RunStatus::TimedOut.exit_code());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn print_wait_json<'a>(outcomes: impl IntoIterator<Item = &'a TargetOutcome>) -> Result<()> {
    let entries = outcomes
        .into_iter()
        .map(|outcome| (outcome.name.as_str(), &outcome.entry))
        .collect::<BTreeMap<_, _>>();
    let mut out = render::out();
    serde_json::to_writer(&mut out, &entries)?;
    writeln!(out)?;
    out.flush()?;
    Ok(())
}

fn print_wait_status(out: &mut impl Write, outcome: &TargetOutcome) -> std::io::Result<()> {
    writeln!(
        out,
        "{} {}",
        render::paint(render::palette::ACCENT, &outcome.name),
        render::paint(
            render::status::run(outcome.entry.status),
            supervised::output::status_label(outcome.entry.status)
        ),
    )
}

fn wait_interactive_agent_stream(
    store: &rimz::Store,
    reference: &str,
    current_channel: Option<&str>,
    timeout: Option<Duration>,
    from_start: bool,
    json: bool,
) -> Result<()> {
    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    let agent = crate::cli::resolve_agent_one(&snapshot, reference, None, current_channel)?;
    let adapter = rimz::agents::find_adapter(agent.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", agent.kind))?;
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
        let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
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

fn wait_run_record(
    store: &rimz::Store,
    run: &RunRecord,
    timeout: Option<Duration>,
    stream_output: bool,
    from_start: bool,
    json: bool,
) -> Result<()> {
    let adapter = rimz::agents::find_adapter(run.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", run.kind))?;
    if stream_output {
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
    let deadline = timeout.map(|duration| Instant::now() + duration);
    loop {
        let current = rimz::harness::run::load(store.paths(), &run.run_id)?;
        if current.status.is_terminal() {
            if json {
                supervised::output::print_json(&current)?;
            } else {
                let mut stdout = render::out();
                let mut stderr = render::err();
                supervised::output::print_run_output(&current, &mut stdout, &mut stderr)?;
            }
            std::process::exit(current.status.exit_code());
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            std::process::exit(RunStatus::TimedOut.exit_code());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}
