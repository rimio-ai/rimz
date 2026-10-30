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

pub(super) fn cancel(target: &str, globals: &super::super::GlobalFlags) -> Result<()> {
    let run = select(target, "cancel", globals, None, &[])?;
    let changed = workflow::cancel(
        &run.state,
        &run.record.id,
        &Host::new(&run.state, globals, None),
    )?;
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
    let spec = workflow::load(&run.paths.home)?;
    let resumed = run.record.state == RunState::Parked;
    let record = workflow::resume(&run.state, &run.record.id, at, extend)?;
    let address = record.address().to_string();
    let now = jiff::Timestamp::now();
    let grant = record.visit_grant - run.record.visit_grant;
    let mut text = if resumed {
        format!("resumed {address} at {}", record.node)
    } else {
        format!("extended {address}")
    };
    let step = spec.steps.get(&record.node);
    if resumed
        && let Some((_, used, max)) = step.and_then(|step| {
            step.tripped_visit_bound(&record).or_else(|| {
                step.visit_bounds(
                    record.visits.get(&record.node),
                    record.key.as_ref(),
                    record.visit_grant,
                )
                .next()
            })
        })
    {
        text.push_str(&format!(" · {} {used} of {max} visits", record.node));
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
    if resumed
        && let Some((bound, used, max)) = step.and_then(|step| step.tripped_visit_bound(&record))
    {
        let target = spec
            .target(&record.node, "cap")
            .unwrap_or_else(|| "@user".into());
        writeln!(
            render::err(),
            "warning: {} has used {used} of {max} visits ({bound}); {} — {}",
            record.node,
            immediate(&target),
            resolve::run_command(&record, "resume", globals, None, &["visits=+1".into()], &[])
        )?;
    }
    if !record.deadline_fired
        && let Some(deadline) = record.deadline.filter(|deadline| *deadline <= now)
    {
        let target = spec
            .target(&record.node, "deadline")
            .unwrap_or_else(|| "@user".into());
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

fn immediate(target: &str) -> String {
    if target == "@user" {
        "the run will park again at once".into()
    } else {
        format!("the run goes to {target} at once")
    }
}
