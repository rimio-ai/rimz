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
    let step = spec.steps.get(node);
    if resumed
        && let Some((_, used, max)) = step.and_then(|step| {
            let mut destination = record.clone();
            destination.node = node.into();
            step.tripped_visit_bound(&destination).or_else(|| {
                step.visit_bounds(
                    destination.visits.get(node),
                    destination.visit_bucket(),
                    destination.visit_grant,
                )
                .next()
            })
        })
    {
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
            immediate(target.as_str()),
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
    let restarted = restarted?;
    let receipt_deadline =
        std::time::Instant::now().checked_add(std::time::Duration::from_secs(30));
    let record = loop {
        let record = records::load(&run.paths)?;
        if record.restart.is_none()
            || record.wait.is_some()
            || record.state.is_terminal()
            || receipt_deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            break record;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
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
            .is_some_and(|spec| spec.checks.contains_key(&run.record.node))
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

fn immediate(target: &str) -> String {
    if target == "@user" {
        "the run will park again at once".into()
    } else {
        format!("the run goes to {target} at once")
    }
}
