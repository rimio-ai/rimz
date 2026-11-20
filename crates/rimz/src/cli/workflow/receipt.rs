use super::*;
use rimz::store::workflow::RunState;

fn select(
    target: &str,
    verb: &str,
    globals: &super::super::GlobalFlags,
    at: Option<&str>,
    step: Option<&str>,
    flags: &[&str],
) -> Result<std::result::Result<resolve::Located, resolve::Other>> {
    match resolve::select(target, verb, globals, at, step, flags)? {
        resolve::Selection::Run(run) => Ok(Ok(*run)),
        resolve::Selection::Other(other) => Ok(Err(*other)),
        resolve::Selection::Workflow(_) => unreachable!("only show selects workflow lists"),
    }
}

/// A run named by id that a resume, extend, or restart cannot reach: refused with why, before any
/// definition is read.
fn refuse(other: &resolve::Other, verb: &str) -> anyhow::Error {
    other.refusal().map_or_else(
        || {
            anyhow::anyhow!(
                "workflow run {} has ended ({}); cannot {verb}",
                other.id,
                other.state_word()
            )
        },
        |refusal| anyhow::anyhow!("{refusal}"),
    )
}

pub(super) fn cancel(
    target: &str,
    reason: Option<&str>,
    globals: &super::super::GlobalFlags,
) -> Result<()> {
    let run = match select(target, "cancel", globals, None, None, &[])? {
        Ok(run) => run,
        Err(other) => {
            if other.is_stranded() {
                return cancel_stranded(&other, globals);
            }
            if let Some(refusal) = other.refusal() {
                bail!("{refusal}");
            }
            let mut out = render::out();
            if let records::RunClass::Current(record) = &other.found.class {
                writeln!(
                    out,
                    "{} is already {}; unchanged",
                    record.address(),
                    workflow_view::state_word(
                        record.state,
                        record.outcome.as_ref().map(|outcome| &outcome.ending)
                    )
                )?;
                writeln!(
                    out,
                    "→ {}",
                    resolve::run_command(record, "show", globals, None, None, &[])
                )?;
                return Ok(());
            }
            writeln!(
                out,
                "{} is already {}; unchanged",
                other.id,
                other.state_word()
            )?;
            writeln!(out, "→ {}", other.show_command(globals))?;
            return Ok(());
        }
    };
    let changed = workflow::cancel(
        &run.state,
        &run.record.id,
        &Host::new(&run.state, globals, None),
        reason,
    );
    run_log::collect_run(&run.state, &run.record.id);
    let changed = changed?;
    let record = finish_run(&run.state, &run.record.id)?.record;
    let address = run.record.address().to_string();
    let mut out = render::out();
    if changed {
        writeln!(out, "canceled {address}")?;
        // A cancel stops only the step's waiting: the runs it started go on. A run the step
        // reserved but never started has no home and is not named, nor is one whose record cannot
        // be read: the cancel is already committed, so the receipt does not fail on it.
        let mut running = Vec::new();
        for started in run
            .record
            .attempt
            .iter()
            .flat_map(|attempt| &attempt.runs)
            .flat_map(|started| &started.runs)
            .filter(|started| started.ended.is_none())
        {
            if let Some(records::RunClass::Current(child)) = records::find(&run.state, &started.id)
                .ok()
                .flatten()
                .map(|found| found.class)
                && !child.state.is_terminal()
            {
                running.push(started.id.to_string());
            }
        }
        if !running.is_empty() {
            writeln!(
                out,
                "its started runs were left running: {}",
                running.join(", ")
            )?;
        }
    } else {
        let ended = record.as_ref().unwrap_or(&run.record);
        let state = workflow_view::state_word(
            ended.state,
            ended.outcome.as_ref().map(|outcome| &outcome.ending),
        );
        writeln!(out, "{address} is already {state}; unchanged")?;
    }
    writeln!(
        out,
        "→ {}",
        resolve::run_command(&run.record, "show", globals, None, None, &[])
    )?;
    Ok(())
}

/// End a run this RimZ cannot continue, keeping its evidence in the finished directory, and say
/// why, that its agents were left running, and how to read it.
fn cancel_stranded(other: &resolve::Other, globals: &super::super::GlobalFlags) -> Result<()> {
    let settled = super::reconcile::end_named(&other.state, &other.id, globals, false)?;
    let mut out = render::out();
    let Some(ended) = settled.ended else {
        writeln!(out, "{} is already ended; unchanged", other.id)?;
        writeln!(out, "→ {}", other.show_command(globals))?;
        return Ok(());
    };
    match &ended.cause {
        super::reconcile::Cause::Stranded(cause) if settled.ended_now => writeln!(
            out,
            "ended {} as failed: this RimZ cannot continue it: {cause}",
            ended.id
        )?,
        super::reconcile::Cause::Stranded(cause) => writeln!(
            out,
            "{} was already ended as failed: this RimZ cannot continue it: {cause}",
            ended.id
        )?,
        super::reconcile::Cause::NotObject => writeln!(
            out,
            "set aside {}: its record holds no JSON object",
            ended.id
        )?,
    }
    // A release that failed or never ran leaves the agents unnamed: end_named printed why.
    if ended.released.is_empty() && settled.warning.is_none() {
        writeln!(out, "no agent was pinned to it")?;
    } else if !ended.released.is_empty() {
        writeln!(
            out,
            "its agents were left running: {}",
            ended.released.join(", ")
        )?;
    }
    if settled.unreported {
        writeln!(
            out,
            "its loop report did not land yet; the workflow pass retries it"
        )?;
    }
    writeln!(out, "→ {}", other.show_command(globals))?;
    Ok(())
}

pub(super) fn resume(
    target: &str,
    at: Option<&str>,
    globals: &super::super::GlobalFlags,
) -> Result<()> {
    let run = select(target, "resume", globals, at, None, &[])?
        .map_err(|other| refuse(&other, "resume"))?;
    refuse_unwritable_home("resumed")?;
    let spec = workflow::load(&run.record.definition_dir(&run.paths))?;
    let record = match workflow::resume(
        &run.state,
        &run.record.id,
        &Host::new(&run.state, globals, None),
        at,
    ) {
        Err(workflow::WorkflowErr::SpentVisits { step, used, max }) => {
            // A cap lowered under a run that spent more needs more than one visit back.
            let admits_one = format!("+{}", used - max + 1);
            bail!(
                "cannot resume at {step}: it has used {used} of {max} visits; grant it another \
                 first:\n{}",
                resolve::run_command(
                    &run.record,
                    "extend",
                    globals,
                    None,
                    Some(&step),
                    &["--visits", &admits_one]
                )
            )
        }
        record => record?,
    };
    let node = record.moving_to().unwrap_or(&record.node);
    let now = jiff::Timestamp::now();
    let mut text = format!("resumed {} at {node}", record.address());
    if let Some(max) = spec
        .step(node)
        .and_then(|step| step.visit_cap(record.visit_grant(node)))
    {
        text.push_str(&format!(
            " · {node} {} of {max} visits",
            record.visits_to(node)
        ));
    }
    if !record.deadline_fired
        && let Some(deadline) = record.deadline
    {
        text.push_str(&format!(
            " · deadline {}",
            render::workflow_clock(deadline, now)
        ));
    }
    let mut out = render::out();
    writeln!(out, "{text}")?;
    writeln!(
        out,
        "→ {}",
        resolve::run_command(&record, "show", globals, None, None, &[])
    )?;
    warn_passed_deadline(&record, &spec, node, now, globals)
}

pub(super) fn extend(
    target: &str,
    step: Option<&str>,
    visits: Option<u64>,
    deadline: Option<&str>,
    globals: &super::super::GlobalFlags,
) -> Result<()> {
    let added = visits.map(|count| format!("+{count}"));
    let flags = added
        .iter()
        .flat_map(|added| ["--visits", added.as_str()])
        .chain(deadline.into_iter().flat_map(|when| ["--deadline", when]))
        .collect::<Vec<_>>();
    let run = select(target, "extend", globals, None, step, &flags)?
        .map_err(|other| refuse(&other, "extend"))?;
    refuse_unwritable_home("extended")?;
    let spec = workflow::load(&run.record.definition_dir(&run.paths))?;
    let when = deadline
        .map(workflow::parse_when)
        .transpose()
        .map_err(anyhow::Error::msg)?;
    let workflow::Extension { record, step } = workflow::extend(
        &run.state,
        &run.record.id,
        &Host::new(&run.state, globals, None),
        visits.map(|count| (step, count)),
        when,
    )?;
    let now = jiff::Timestamp::now();
    let mut text = format!("extended {}", record.address());
    if let (Some(step), Some(count)) = (&step, visits) {
        text.push_str(&format!(" · {step} +{count} visits"));
        if let Some(max) = spec
            .step(step)
            .and_then(|node| node.visit_cap(record.visit_grant(step)))
        {
            text.push_str(&format!(", {} of {max} used", record.visits_to(step)));
        }
    }
    if when.is_some()
        && let Some(deadline) = record.deadline
    {
        text.push_str(&format!(
            " · deadline {}",
            render::workflow_clock(deadline, now)
        ));
    }
    let mut out = render::out();
    writeln!(out, "{text}")?;
    writeln!(
        out,
        "→ {}",
        resolve::run_command(&record, "show", globals, None, None, &[])
    )?;
    let node = record.moving_to().unwrap_or(&record.node);
    warn_passed_deadline(&record, &spec, node, now, globals)
}

/// Warn when the run's armed deadline has already passed at `node`, with what it does there
/// and the command that sets another.
fn warn_passed_deadline(
    record: &RunRecord,
    spec: &workflow::Definition,
    node: &str,
    now: jiff::Timestamp,
    globals: &super::super::GlobalFlags,
) -> Result<()> {
    if !record.deadline_fired
        && let Some(deadline) = record.deadline.filter(|deadline| *deadline <= now)
        && let Some(target) = spec.target(node, "deadline")
    {
        writeln!(
            render::err(),
            "warning: deadline {} has already passed; {} — {}",
            render::workflow_clock(deadline, now),
            immediate(&target),
            resolve::run_command(
                record,
                "extend",
                globals,
                None,
                None,
                &["--deadline", "<when>"]
            )
        )?;
    }
    Ok(())
}

pub(in crate::cli) fn restart(
    target: &str,
    continued: bool,
    globals: &super::super::GlobalFlags,
) -> Result<()> {
    let run = select(target, "restart", globals, None, None, &[])?
        .map_err(|other| refuse(&other, "restart"))?;
    refuse_unwritable_home("restarted")?;
    let resume = if continued {
        Some(continue_session(&run, globals)?)
    } else {
        None
    };
    let session = resume.as_ref().map(|(_, session)| session.clone());
    let host = Host::new(&run.state, globals, None);
    let restarted = workflow::restart(&run.state, &run.record.id, resume, &host);
    run_log::collect_run(&run.state, &run.record.id);
    let (restarted, advance) = restarted?;
    // A run that ended meanwhile may already have left the live directory.
    let record = first_pass(&run.state, &run.record.id, advance.as_ref())?
        .unwrap_or_else(|| restarted.clone());
    let record = finish_run(&run.state, &run.record.id)?
        .record
        .unwrap_or(record);
    let step = restarted
        .session_attempt()
        .map_or(&restarted.node, |attempt| &attempt.node);
    let mode = session.map_or_else(|| "fresh".into(), |session| format!("resumed ({session})"));
    let mut out = render::out();
    if let Some(agent) = record
        .session_attempt()
        .and_then(|attempt| attempt.run.as_ref())
        .filter(|_| record.restart.is_none())
    {
        writeln!(
            out,
            "restarted {} step {step} as {} · {mode} · visits restarted at 1",
            record.address(),
            agent.handle
        )?;
    } else if record.state == RunState::Parked {
        writeln!(
            out,
            "restart {} step {step} · {mode} · no live replacement: {}",
            record.address(),
            record
                .park
                .as_ref()
                .map_or_else(String::new, |park| park.label())
        )?;
    } else if record.state.is_terminal() {
        writeln!(
            out,
            "restart {} step {step} · {mode} · run is {}; no live replacement",
            record.address(),
            workflow_view::state_word(
                record.state,
                record.outcome.as_ref().map(|outcome| &outcome.ending)
            )
        )?;
    } else {
        writeln!(
            out,
            "restarting {} step {step} · {mode} · awaiting launch; visits reset",
            record.address()
        )?;
    }
    writeln!(
        out,
        "→ {}",
        resolve::run_command(&record, "show", globals, None, None, &[])
    )?;
    drop(out);
    write_held(&run.paths, &record, "")
}

/// The run's record once the advance a start or restart spawned has had its first pass: a run
/// not yet ended shows a wait, a launched attempt, or a state other than active, or no process
/// that could decide (the spawned advance, a live attempt's host) still runs. An ended run waits
/// for its advance, which moves its home once the engine returns. Bounded at 30 s, after which
/// the record is returned as it stands. Once the run has left the live directory, the record last
/// read there when it says how the run ended, else the record in its finished home; None only
/// when neither home had one to read.
pub(in crate::cli) fn first_pass(
    state: &paths::StatePaths,
    id: &rimz::ids::WorkflowRunId,
    advance: Option<&workflow::Advance>,
) -> Result<Option<RunRecord>> {
    let paths = &state.workflow(id);
    let deadline = std::time::Instant::now().checked_add(std::time::Duration::from_secs(30));
    let mut last = None;
    loop {
        let record = match records::load(paths) {
            Ok(record) => record,
            Err(error) if not_found(&error) => {
                if last
                    .as_ref()
                    .is_some_and(|last: &RunRecord| last.state.is_terminal())
                {
                    return Ok(last);
                }
                return match records::load(&state.finished_workflow(id)) {
                    Ok(finished) => Ok(Some(finished)),
                    Err(error) if not_found(&error) => Ok(last),
                    Err(error) => Err(error.into()),
                };
            }
            Err(error) => return Err(error.into()),
        };
        let deciding = advance.is_some_and(workflow::Advance::is_running)
            || record.attempt.as_ref().is_some_and(|attempt| {
                rimz::proc::process_is_live(attempt.host.pid, attempt.host.start.as_deref())
            });
        let ended = record.state.is_terminal();
        if (!ended
            && (record.wait.is_some()
                || record.state != RunState::Active
                || (record.restart.is_none() && record.attempt.as_ref().is_some_and(launched))))
            || !deciding
            || deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            return Ok(Some(record));
        }
        last = Some(record);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn not_found(error: &records::WorkflowStoreErr) -> bool {
    matches!(error, records::WorkflowStoreErr::Io { source, .. }
        if source.kind() == std::io::ErrorKind::NotFound)
}

/// An agent or judge attempt counts once its run is written or it has ended: until then its
/// launch can still be withdrawn into a wait. Other kinds publish no run.
fn launched(attempt: &records::LiveAttempt) -> bool {
    !attempt.kind.owns_agent() || attempt.run.is_some() || attempt.ending.is_some()
}

/// The receipt's held line: the hold in front of the run's node, in the phrase
/// `rimz workflow show` prints for it; nothing for a run that is not held.
pub(in crate::cli) fn write_held(
    paths: &paths::WorkflowPaths,
    record: &RunRecord,
    indent: &str,
) -> Result<()> {
    let Some(wait) = record.wait.as_ref().filter(|wait| {
        record.state == RunState::Active
            && wait.node == record.node
            && matches!(
                wait.outcome.as_str(),
                "capacity" | "window" | "budget" | "occupied" | "takeover" | "trust"
            )
    }) else {
        return Ok(());
    };
    write_wait(paths, record, &wait.node, indent)
}

/// A try's wait line after its receipt: what the first pass left it waiting on, in the phrase
/// `rimz workflow show` prints for it; nothing for a try not waiting.
pub(super) fn write_waiting(paths: &paths::WorkflowPaths, record: &RunRecord) -> Result<()> {
    let Some(wait) = record
        .wait
        .as_ref()
        .filter(|_| record.state == RunState::Active)
    else {
        return Ok(());
    };
    write_wait(paths, record, &wait.node, "")
}

/// One receipt line: `node` and the run's wait in the phrase `rimz workflow show` prints for it.
fn write_wait(
    paths: &paths::WorkflowPaths,
    record: &RunRecord,
    node: &str,
    indent: &str,
) -> Result<()> {
    let spec = workflow::load(&record.definition_dir(paths)).ok();
    if let Some(text) = workflow_view::wait(record, spec.as_ref(), jiff::Timestamp::now()) {
        writeln!(render::out(), "{indent}{node} {text}")?;
    }
    Ok(())
}

/// Block until a try has ended and its home has left the live directory, then print its
/// finished view. A home still live 30 s after its record ended is shown as it stands. A try
/// that ended other than `tried` exits 1 naming how it ended. Interrupting the wait leaves the
/// try running.
pub(super) fn finish_try(
    state: &paths::StatePaths,
    id: &rimz::ids::WorkflowRunId,
    globals: &super::super::GlobalFlags,
) -> Result<()> {
    let mut ended = None;
    while let Some(record) = live_record(state, id)? {
        if record.state.is_terminal()
            && ended.get_or_insert_with(std::time::Instant::now).elapsed()
                >= std::time::Duration::from_secs(30)
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    writeln!(render::out())?;
    super::show(Some(id.as_str()), None, false, false, globals)?;
    let ended = match records::find(state, id)? {
        Some(records::FoundRun {
            class: records::RunClass::Current(record),
            ..
        }) => Some(*record),
        _ => None,
    };
    let tried = ended.as_ref().and_then(|record| record.outcome.as_ref());
    if tried.is_some_and(|outcome| outcome.ending == records::RunEnding::Tried) {
        return Ok(());
    }
    Err(super::super::ExitCodeError {
        code: 1,
        message: format!(
            "workflow run {id} ended {}, not tried",
            ended.map_or("unread", |record| record.state.as_str())
        ),
    }
    .into())
}

fn continue_session(
    run: &resolve::Located,
    globals: &super::super::GlobalFlags,
) -> Result<(u64, rimz::ids::AgentSessionId)> {
    if run.record.state.is_terminal() {
        bail!(
            "workflow run {} is {}; cannot restart",
            run.record.id,
            workflow_view::state_word(
                run.record.state,
                run.record.outcome.as_ref().map(|outcome| &outcome.ending)
            )
        );
    }
    let fix = format!("rimz workflow restart {}", run.record.id);
    let candidate = run
        .record
        .session_attempt()
        .and_then(|attempt| {
            attempt
                .run
                .as_ref()
                .map(|supervised| (attempt.seq, attempt.node.as_str(), &supervised.run_id))
        })
        .or_else(|| {
            run.record
                .prev_attempt
                .as_ref()
                .filter(|previous| {
                    run.record.state == RunState::Parked && previous.step == run.record.node
                })
                .and_then(|previous| {
                    previous
                        .run_id
                        .as_ref()
                        .map(|id| (previous.seq, previous.step.as_str(), id))
                })
        });
    let (seq, step, run_id) = candidate.with_context(|| {
        let spec = workflow::load(&run.record.definition_dir(&run.paths)).ok();
        let kind = if spec
            .as_ref()
            .is_some_and(|spec| spec.check(&run.record.node).is_some())
        {
            "check"
        } else {
            "step"
        };
        format!(
            "{kind} {} has no resumable session; use {fix} for a fresh agent step",
            run.record.node
        )
    })?;
    let supervised = rimz::harness::run::load(&run.state, run_id)?;
    let ctx = super::super::ctx::Ctx::for_workspace(run.state.workspace_id.clone(), globals.mux)?;
    let agents = ctx.store.audit_rows()?;
    let agent = supervised
        .agent_id
        .as_ref()
        .and_then(|id| rimz::address::launch_row(agents.iter(), &supervised.kind, id))
        .with_context(|| format!("no resumable session for step {step}; use {fix}"))?;
    let logins = rimz::agents::room_accounts(
        &run.state.workspace_record,
        Some(&run.record.project_root),
        &rimz::config::MachineConfig::load_lenient(),
    )?;
    rimz::harness::resume::relaunch_login(agent, &logins, &rimz::agents::machine_login_catalog())?;
    let adapter = rimz::agents::find_definition(agent.kind.as_str())
        .with_context(|| format!("unknown agent kind {}; use {fix}", agent.kind))?;
    if agent.agent_id.is_provisional() || !rimz::harness::resume::resume_session_present(agent) {
        bail!("no resumable session for step {step}; use {fix}");
    }
    rimz::harness::launch::compile_provider_argv(
        adapter,
        agent.kind.as_str(),
        &rimz::harness::launch::ExecAction::Resume {
            session_id: agent.agent_id.to_string(),
            extra_args: Vec::new(),
        },
        &run.record.scope.path,
    )
    .with_context(|| format!("{} has no resume support; use {fix}", agent.kind))?;
    Ok((seq, agent.agent_id.clone()))
}

fn immediate(target: &workflow::Target) -> String {
    if *target == workflow::Target::User {
        "the run will park again at once".into()
    } else {
        format!("the run goes to {target} at once")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_pass_waits_past_a_published_attempt_until_its_run_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let state = paths::StatePaths::for_project_root_under(dir.path(), dir.path()).unwrap();
        let now = jiff::Timestamp::now();
        let mut record: RunRecord = serde_json::from_value(serde_json::json!({
            "version":rimz::store::workflow::VERSION,"id":rimz::ids::WorkflowRunId::new(),
            "workflow":"review","spec_dir":dir.path(),
            "project_root":dir.path(),"scope":{"kind":"worktree","path":dir.path()},
            "state":"active","node":"build","created_at":now,"entered_at":now,"visits":{},
            "deadline_fired":false,"next_seq":2,
            "attempt":{"kind":"agent","seq":1,"node":"build","started_at":now,
                "host":{"pid":std::process::id(),"start":null}},
        }))
        .unwrap();
        let guard = rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
        records::initialize(&state, &record, &guard).unwrap();
        drop(guard);
        let paths = state.workflow(&record.id);
        let record_id = record.id.clone();
        let writer = {
            let (state, paths) = (state.clone(), paths.clone());
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(200));
                record.attempt.as_mut().unwrap().run = Some(records::AttemptRun {
                    run_id: rimz::ids::RunId::new(),
                    handle: "fox".into(),
                });
                let guard =
                    rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
                records::write(&paths, &record, &guard).unwrap();
            })
        };
        let seen = first_pass(&state, &record_id, None).unwrap().unwrap();
        writer.join().unwrap();
        assert!(
            seen.attempt
                .as_ref()
                .is_some_and(|attempt| attempt.run.is_some()),
            "an attempt published without a run is still preparing its launch: {seen:#?}"
        );
    }

    #[test]
    fn first_pass_reads_the_finished_home_when_the_run_left_before_it_read_its_end() {
        let dir = tempfile::tempdir().unwrap();
        let state = paths::StatePaths::for_project_root_under(dir.path(), dir.path()).unwrap();
        let now = jiff::Timestamp::now();
        let mut record: RunRecord = serde_json::from_value(serde_json::json!({
            "version":rimz::store::workflow::VERSION,"id":rimz::ids::WorkflowRunId::new(),
            "workflow":"review","spec_dir":dir.path(),
            "project_root":dir.path(),"scope":{"kind":"worktree","path":dir.path()},
            "state":"active","node":"build","created_at":now,"entered_at":now,"visits":{},
            "deadline_fired":false,"next_seq":2,
            "attempt":{"kind":"agent","seq":1,"node":"build","started_at":now,
                "host":{"pid":std::process::id(),"start":null}},
        }))
        .unwrap();
        let guard = rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
        records::initialize(&state, &record, &guard).unwrap();
        drop(guard);
        let id = record.id.clone();
        // The run ends and its home moves between two polls, so no poll reads the ended record
        // in the live directory.
        let mover = {
            let (state, aside) = (state.clone(), dir.path().join("moved-aside"));
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(200));
                record.state = RunState::Failed;
                let finished = state.finished_workflow(&record.id);
                std::fs::create_dir_all(&finished.home).unwrap();
                let guard =
                    rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
                records::write(&finished, &record, &guard).unwrap();
                std::fs::rename(&state.workflow(&record.id).home, aside).unwrap();
            })
        };
        let seen = first_pass(&state, &id, None).unwrap().unwrap();
        mover.join().unwrap();
        assert_eq!(
            seen.state,
            RunState::Failed,
            "the ended record from the finished home"
        );
    }
}
