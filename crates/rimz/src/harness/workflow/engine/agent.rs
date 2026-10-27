use super::super::{AttemptHost, LaunchFailure, Observed, StepLaunch};
use super::*;
use crate::agents::{WorkflowAttemptRef, WorkflowAttemptRole};
use crate::store::run::RunStatus;
use crate::store::workflow::{Children, EndCause, Ending, Limited};

pub(super) fn identity() -> Host {
    Host {
        pid: std::process::id(),
        start: crate::proc::process_start_token(std::process::id()),
    }
}

pub(super) fn compose(
    record: &RunRecord,
    paths: &WorkflowPaths,
    step: &super::super::Step,
    digests: &mut BTreeMap<String, String>,
) -> Result<String> {
    // Only validated agent steps compose prompts.
    let prompt_file = step.prompt_file.as_deref().expect("agent step prompt file");
    let file = spec::confined_file(&record.spec_dir, prompt_file)?;
    let prompt = std::fs::read_to_string(file)?;
    digests.insert(prompt_file.into(), script::digest(prompt.as_bytes()));
    let mut parts = vec![prompt];
    match paths.signs(Some(&record.node)).canonicalize() {
        Ok(path) => {
            if !path.starts_with(paths.work.canonicalize()?) {
                return Err(WorkflowErr::Refused(
                    "step sign must remain inside workflow work".into(),
                ));
            }
            let sign = std::fs::read_to_string(path)?;
            digests.insert(
                format!("signs/{}.md", record.node),
                script::digest(sign.as_bytes()),
            );
            parts.push(sign);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    if let Some(prompt) = &record.prompt {
        digests.insert("prompt".into(), script::digest(prompt.as_bytes()));
        parts.push(prompt.clone());
    }
    Ok(parts.join("\n\n"))
}

pub(super) fn empty_observation() -> Observed {
    Observed {
        status: RunStatus::Pending,
        ask: None,
        limited: None,
        children: Children::default(),
        cost_usd: None,
    }
}

fn observed_outcome(
    observed: &Observed,
    lifecycle: crate::store::workflow::StepLifecycle,
    on_asked: bool,
) -> Option<AttemptOutcome> {
    match observed.status {
        RunStatus::Completed => Some(AttemptOutcome::Ended),
        RunStatus::Failed | RunStatus::VerifyFailed | RunStatus::TimedOut => {
            Some(AttemptOutcome::Died)
        }
        RunStatus::Canceled => Some(AttemptOutcome::Canceled),
        RunStatus::BudgetExceeded => Some(AttemptOutcome::Limited),
        RunStatus::Pending | RunStatus::Running
            if lifecycle.is_turn() && observed.limited.is_some() =>
        {
            Some(AttemptOutcome::Limited)
        }
        RunStatus::Pending | RunStatus::Running
            if lifecycle.is_turn() && on_asked && observed.ask.is_some() =>
        {
            Some(AttemptOutcome::Asked)
        }
        _ => None,
    }
}

/// Folds one observation's blocking ask into the attempt's hold and reports whether it changed.
/// A new hold opens at the ask's own open time, clamped to the attempt; a kept hold keeps its
/// start; the first observation without an ask closes it into `asked_ms`.
fn hold(
    attempt: &mut workflow::LiveAttempt,
    ask: Option<&workflow::Asking>,
    now: Timestamp,
) -> bool {
    let asking = ask.map(|seen| workflow::Asking {
        since: match &attempt.asking {
            Some(open) => open.since,
            None => seen.since.max(attempt.started_at).min(now),
        },
        ask: seen.ask.clone(),
    });
    let changed = attempt.asking != asking;
    if asking.is_none()
        && let Some(closed) = attempt.asking.take()
    {
        let paused = u64::try_from(now.duration_since(closed.since).as_millis()).unwrap_or(0);
        attempt.asked_ms = attempt.asked_ms.saturating_add(paused);
    }
    attempt.asking = asking;
    changed
}

pub(super) fn prepare_end(
    t: &mut Transition<'_>,
    host: &dyn AttemptHost,
    guard: &WorkspaceLock,
) -> Result<Decision> {
    // Called only for an agent attempt already published on this record.
    let attempt = t
        .record
        .attempt
        .as_ref()
        .expect("agent attempt was checked");
    let host_live = crate::proc::process_is_live(attempt.host.pid, attempt.host.start.as_deref());
    if let Some(ending) = &attempt.ending {
        if host_live {
            return Ok(Decision::Stop);
        }
        if t.record
            .prev_attempt
            .as_ref()
            .is_none_or(|previous| previous.seq != attempt.seq)
        {
            let observed = match &attempt.run {
                Some(run) => host.observe(run)?,
                None => empty_observation(),
            };
            t.stage_agent_end(ending.clone(), &observed);
        }
        t.record.attempt.as_mut().expect("agent attempt").host = identity();
        persist(t, guard)?;
        return Ok(Decision::End(Box::new(t.record.clone())));
    }
    let mut observed = match &attempt.run {
        Some(run) => host.observe(run)?,
        None => empty_observation(),
    };
    t.now = Timestamp::now();
    let lifecycle = attempt.lifecycle;
    let missing_run = attempt.run.is_none();
    let on_asked = t.spec.steps[&attempt.node].on.asked.is_some();
    let hold_changed = hold(
        t.record.attempt.as_mut().expect("observed agent attempt"),
        observed.ask.as_ref(),
        t.now,
    );
    let ending = if !t.record.deadline_fired && t.record.deadline.is_some_and(|at| at <= t.now) {
        Some(Ending {
            outcome: AttemptOutcome::Canceled,
            cause: EndCause::Deadline,
        })
    } else if t.timeout_at()?.is_some_and(|at| at <= t.now) {
        Some(Ending {
            outcome: AttemptOutcome::Timeout,
            cause: EndCause::Timeout,
        })
    } else {
        observed_outcome(&observed, lifecycle, on_asked)
            .or_else(|| (missing_run && !host_live).then_some(AttemptOutcome::Canceled))
            .map(|outcome| Ending {
                outcome,
                cause: EndCause::Lifecycle,
            })
    };
    let Some(ending) = ending else {
        if hold_changed {
            persist(t, guard)?;
        }
        return Ok(Decision::Stop);
    };
    if ending.outcome == AttemptOutcome::Limited
        && (observed.limited.is_none() || observed.status == RunStatus::BudgetExceeded)
    {
        observed.limited = Some(Limited { reset_at: None });
    }
    let launching = missing_run && host_live;
    t.stage_agent_end(ending, &observed);
    if launching {
        // The launch owner publishes its returned identity before stopping it.
        persist(t, guard)?;
        return Ok(Decision::Stop);
    }
    t.record.attempt.as_mut().expect("staged agent end").host = identity();
    persist(t, guard)?;
    Ok(Decision::End(Box::new(t.record.clone())))
}

pub(super) fn launch(
    state: &StatePaths,
    paths: &WorkflowPaths,
    spec: &WorkflowSpec,
    record: &RunRecord,
    prompt: std::result::Result<String, String>,
    host: &dyn AttemptHost,
) -> Result<bool> {
    // Preparation publishes the attempt before entering this off-lock launch.
    let attempt = record.attempt.as_ref().expect("prepared agent launch");
    let launched = match prompt {
        Ok(prompt) => host
            .launch(StepLaunch {
                workflow: WorkflowAttemptRef {
                    run: record.id.clone(),
                    seq: attempt.seq,
                    role: WorkflowAttemptRole::Step,
                },
                // Preparation dispatches only validated agent steps here.
                agent: spec.steps[&record.node]
                    .agent
                    .clone()
                    .expect("agent step layout"),
                lifecycle: attempt.lifecycle,
                prompt,
                cwd: record.scope.path.clone(),
            })
            .unwrap_or_else(|error| {
                Err(LaunchFailure {
                    outcome: AttemptOutcome::Died,
                    reason: error.to_string(),
                })
            }),
        Err(reason) => Err(LaunchFailure {
            outcome: AttemptOutcome::Died,
            reason,
        }),
    };
    let guard = WorkspaceLock::acquire(&state.workspace_lock)?;
    let mut t = Transition {
        record: workflow::load(paths)?,
        spec,
        paths,
        now: Timestamp::now(),
        entries: Vec::new(),
    };
    if !t.current(attempt.seq) {
        drop(guard);
        if let Ok(run) = launched {
            host.stop(&run)?;
        }
        return Ok(false);
    }
    let reason = match launched {
        Ok(run) => {
            t.record.attempt.as_mut().expect("current launch").run = Some(run);
            persist(&t, &guard)?;
            accounting::step(&t.record);
            if t.record
                .attempt
                .as_ref()
                .is_some_and(|attempt| attempt.ending.is_none())
            {
                // A terminal wake may have preceded publication of the run identity.
                if !matches!(prepare_end(&mut t, host, &guard)?, Decision::End(_)) {
                    return Ok(false);
                }
            }
            None
        }
        Err(failure) => {
            let mut observed = empty_observation();
            if failure.outcome == AttemptOutcome::Limited {
                observed.limited = Some(Limited { reset_at: None });
            }
            if t.record
                .attempt
                .as_ref()
                .is_some_and(|attempt| attempt.ending.is_none())
            {
                t.stage_agent_end(
                    Ending {
                        outcome: failure.outcome,
                        cause: EndCause::Lifecycle,
                    },
                    &observed,
                );
            }
            Some(failure.reason)
        }
    };
    t.record.attempt.as_mut().expect("launch ending").host = identity();
    persist(&t, &guard)?;
    let record = t.record;
    drop(guard);
    finish(state, paths, spec, &record, host, reason)
}

pub(super) fn finish(
    state: &StatePaths,
    paths: &WorkflowPaths,
    spec: &WorkflowSpec,
    record: &RunRecord,
    host: &dyn AttemptHost,
    reason: Option<String>,
) -> Result<bool> {
    // Only a published ending reaches the off-lock stop cascade.
    let attempt = record.attempt.as_ref().expect("published agent ending");
    let mut run = attempt.run.clone();
    if run.is_none() {
        let identity = WorkflowAttemptRef {
            run: record.id.clone(),
            seq: attempt.seq,
            role: WorkflowAttemptRole::Step,
        };
        if let Some(orphan) = crate::harness::run::list(state)
            .map_err(|error| WorkflowErr::Refused(error.to_string()))?
            .into_iter()
            .find(|run| run.workflow.as_ref() == Some(&identity))
        {
            run = Some(workflow::AttemptRun {
                run_id: orphan.run_id,
                handle: orphan
                    .agent_name
                    .map(|name| format!("@{name}"))
                    .ok_or_else(|| {
                        WorkflowErr::Refused("workflow attempt run has no handle".into())
                    })?,
            });
        }
    }
    let (children, cost) = match &run {
        Some(run) => (host.stop(run)?, host.observe(run)?.cost_usd),
        None => (Children::default(), None),
    };
    let head_end = super::super::head(&record.scope.path);
    let guard = WorkspaceLock::acquire(&state.workspace_lock)?;
    let mut t = Transition {
        record: workflow::load(paths)?,
        spec,
        paths,
        now: Timestamp::now(),
        entries: Vec::new(),
    };
    if !t.current(attempt.seq)
        || t.record
            .attempt
            .as_ref()
            .is_none_or(|a| a.host != attempt.host)
    {
        return Ok(false);
    }
    t.record.attempt.as_mut().expect("current agent ending").run = run;
    // A concurrent cancel may have replaced the ending while the stop ran.
    t.settle_agent(children, cost, head_end, reason)?;
    persist(&t, &guard)?;
    Ok(t.record.state == RunState::Active && t.record.wait.is_none())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::workflow::{Asked, Asking, StepLifecycle};

    #[test]
    fn observed_outcome_covers_status_ask_limit_lifecycle_and_asked_edge() {
        let statuses = [
            (RunStatus::Pending, None),
            (RunStatus::Running, None),
            (RunStatus::Completed, Some(AttemptOutcome::Ended)),
            (RunStatus::Failed, Some(AttemptOutcome::Died)),
            (RunStatus::VerifyFailed, Some(AttemptOutcome::Died)),
            (RunStatus::TimedOut, Some(AttemptOutcome::Died)),
            (RunStatus::Canceled, Some(AttemptOutcome::Canceled)),
            (RunStatus::BudgetExceeded, Some(AttemptOutcome::Limited)),
        ];
        let signals = [
            (false, false, None, None),
            (true, false, None, Some(AttemptOutcome::Asked)),
            (
                false,
                true,
                Some(AttemptOutcome::Limited),
                Some(AttemptOutcome::Limited),
            ),
            (
                true,
                true,
                Some(AttemptOutcome::Limited),
                Some(AttemptOutcome::Limited),
            ),
        ];
        let mut actual = Vec::new();
        let mut expected = Vec::new();
        for (status, terminal) in statuses {
            for (ask, limit, held, routed) in signals {
                for lifecycle in [StepLifecycle::Turn, StepLifecycle::Stay] {
                    for on_asked in [false, true] {
                        let observed = Observed {
                            status,
                            ask: ask.then(|| Asking {
                                since: Timestamp::now(),
                                ask: Asked {
                                    text: "Approve?".into(),
                                    kind: "permission".into(),
                                    handle: "@child".into(),
                                },
                            }),
                            limited: limit.then_some(Limited { reset_at: None }),
                            children: Children::default(),
                            cost_usd: None,
                        };
                        let case = (status, ask, limit, lifecycle, on_asked);
                        actual.push((case, observed_outcome(&observed, lifecycle, on_asked)));
                        let live = if lifecycle.is_turn() {
                            if on_asked { routed } else { held }
                        } else {
                            None
                        };
                        expected.push((case, terminal.or(live)));
                    }
                }
            }
        }
        assert_eq!(actual, expected);
    }
}
