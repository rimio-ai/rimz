use super::*;
use rimz::store::workflow::RunState;

fn select(
    target: &str,
    verb: &str,
    globals: &super::super::GlobalFlags,
    at: Option<&str>,
    extend: &[String],
) -> Result<resolve::Located> {
    match resolve::select(target, verb, globals, at, extend)? {
        resolve::Selection::Run(run) => Ok(*run),
        resolve::Selection::Workflow(_) => unreachable!("only show selects workflow lists"),
    }
}

pub(super) fn cancel(
    target: &str,
    reason: Option<&str>,
    globals: &super::super::GlobalFlags,
) -> Result<()> {
    if let Some((state, paths)) = resolve::refused_run(target) {
        let guard = rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock)?;
        if records::remove_refused(&paths, &guard)? {
            writeln!(
                render::out(),
                "removed {target}: another RimZ version wrote its record, so it was cleared \
                 unread; any agent it left running is untouched"
            )?;
            return Ok(());
        }
    }
    let run = select(target, "cancel", globals, None, &[])?;
    let changed = workflow::cancel(
        &run.state,
        &run.record.id,
        &Host::new(&run.state, globals, None),
        reason,
    );
    run_log::collect_run(&run.state, &run.record.id);
    let changed = changed?;
    write_history_copy(&run.state, &records::load(&run.paths)?)?;
    let address = run.record.address().to_string();
    let mut out = render::out();
    if changed {
        writeln!(out, "canceled {address}")?;
    } else {
        let record = records::load(&run.paths)?;
        writeln!(
            out,
            "{address} is already {}; unchanged",
            record.state.as_str()
        )?;
    }
    writeln!(
        out,
        "→ {}",
        resolve::run_command(&run.record, "show", globals, None, &[], &[])
    )?;
    Ok(())
}

pub(super) fn resume(
    target: &str,
    at: Option<&str>,
    extend: &[String],
    globals: &super::super::GlobalFlags,
) -> Result<()> {
    let run = select(target, "resume", globals, at, extend)?;
    refuse_unwritable_home("resumed")?;
    let spec = workflow::load(&run.paths.home)?;
    let resumed = run.record.state == RunState::Parked || run.record.node == "@break";
    let record = workflow::resume(&run.state, &run.record.id, at, extend)?;
    let node = record
        .attempt
        .as_ref()
        .filter(|attempt| attempt.node == "@break")
        .and_then(|attempt| attempt.ending.as_ref()?.route.as_ref())
        .map_or(record.node.as_str(), |verdict| verdict.target.as_str());
    let address = record.address().to_string();
    let now = jiff::Timestamp::now();
    let grant = record.visit_grant - run.record.visit_grant;
    let mut text = if resumed {
        format!("resumed {address} at {node}")
    } else {
        format!("extended {address}")
    };
    let step = spec.step(node);
    if resumed && let Some(max) = step.and_then(|step| step.visit_cap(record.visit_grant)) {
        let used = record.visits_to(node);
        text.push_str(&format!(" · {node} {used} of {max} visits"));
    }
    if grant > 0 {
        text.push_str(&if resumed {
            format!(", every capped step +{grant}")
        } else {
            format!(" · every capped step +{grant} visits")
        });
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
        resolve::run_command(&record, "show", globals, None, &[], &[])
    )?;
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
                &record,
                "resume",
                globals,
                None,
                &["deadline=<when>".into()],
                &[]
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
    let run = select(target, "restart", globals, None, &[])?;
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
    let record = first_pass(&run.paths, advance.as_ref())?;
    write_history_copy(&run.state, &record)?;
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
            record.state.as_str()
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
        resolve::run_command(&record, "show", globals, None, &[], &[])
    )?;
    drop(out);
    write_held(&run.paths, &record, "")
}

/// The run's record once the advance a start or restart spawned has had its first pass: it
/// shows a wait, a launched attempt, or a run no longer active, or no process that could
/// decide (the spawned advance, a live attempt's host) still runs. Bounded at 30 s, after
/// which the record is returned as it stands.
pub(in crate::cli) fn first_pass(
    paths: &paths::WorkflowPaths,
    advance: Option<&workflow::Advance>,
) -> Result<RunRecord> {
    let deadline = std::time::Instant::now().checked_add(std::time::Duration::from_secs(30));
    loop {
        let record = records::load(paths)?;
        let deciding = advance.is_some_and(workflow::Advance::is_running)
            || record.attempt.as_ref().is_some_and(|attempt| {
                rimz::proc::process_is_live(attempt.host.pid, attempt.host.start.as_deref())
            });
        if record.wait.is_some()
            || record.state != RunState::Active
            || (record.restart.is_none() && record.attempt.as_ref().is_some_and(launched))
            || !deciding
            || deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            return Ok(record);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// An agent attempt counts once its run is written or it has ended: until then its launch can
/// still be withdrawn into an occupancy wait. Other kinds publish no run.
fn launched(attempt: &records::LiveAttempt) -> bool {
    attempt.kind != records::AttemptKind::Agent || attempt.run.is_some() || attempt.ending.is_some()
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
                "capacity" | "window" | "occupied" | "takeover"
            )
    }) else {
        return Ok(());
    };
    let spec = workflow::load(&paths.home).ok();
    if let Some(text) = workflow_view::wait(record, spec.as_ref(), jiff::Timestamp::now()) {
        writeln!(render::out(), "{indent}{} {text}", wait.node)?;
    }
    Ok(())
}

fn continue_session(
    run: &resolve::Located,
    globals: &super::super::GlobalFlags,
) -> Result<(u64, rimz::ids::AgentSessionId)> {
    if run.record.state.is_terminal() {
        bail!(
            "workflow run {} is {}; cannot restart",
            run.record.id,
            run.record.state.as_str()
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
        let spec = workflow::load(&run.paths.home).ok();
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
            "workflow":"review","spec_dir":dir.path(),"spec_digest":"digest",
            "project_root":dir.path(),"scope":{"kind":"worktree","path":dir.path()},
            "state":"active","node":"build","created_at":now,"entered_at":now,"visits":{},
            "visit_grant":0,"deadline_fired":false,"next_seq":2,
            "attempt":{"kind":"agent","seq":1,"node":"build","started_at":now,
                "host":{"pid":std::process::id(),"start":null}},
        }))
        .unwrap();
        let guard = rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
        records::initialize(&state, &record, b"", &guard).unwrap();
        drop(guard);
        let paths = state.workflow(&record.id);
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
        let seen = first_pass(&paths, None).unwrap();
        writer.join().unwrap();
        assert!(
            seen.attempt
                .as_ref()
                .is_some_and(|attempt| attempt.run.is_some()),
            "an attempt published without a run is still preparing its launch: {seen:#?}"
        );
    }
}
