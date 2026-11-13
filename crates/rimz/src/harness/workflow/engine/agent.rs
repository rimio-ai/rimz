use super::super::{AttemptHost, LaunchFailure, NotLaunched, Observation, Observed, StepLaunch};
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
    step: &Node,
    digests: &mut BTreeMap<String, String>,
) -> Result<String> {
    let mut parts = Vec::new();
    if record.node == "@break" && step.prompt_file().is_none() {
        parts.push(break_prompt(record, paths)?);
    } else if let Some(prompt_file) = step.prompt_file() {
        let file = spec::confined_file(&record.spec_dir, prompt_file)?;
        let prompt = std::fs::read_to_string(file)?;
        digests.insert(prompt_file.into(), script::digest(prompt.as_bytes()));
        parts.push(prompt);
    } else if !matches!(step.kind(), Some(Kind::Message(_))) {
        return Err(WorkflowErr::Refused(format!(
            "agent.{}.prompt-file: an agent step requires prompt-file",
            record.node
        )));
    }
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

fn break_prompt(record: &RunRecord, paths: &WorkflowPaths) -> Result<String> {
    let spec = spec::load(&paths.home)?;
    let input = record.input.as_ref();
    let from = input
        .and_then(|input| input["from"].as_str())
        .unwrap_or_default();
    let journal = workflow::journal(paths)?;
    let history = journal
        .iter()
        .filter(|entry| entry.node == from)
        .collect::<Vec<_>>();
    let checks = history
        .iter()
        .rev()
        .filter_map(|entry| match &entry.event {
            JournalEvent::Evaluation { object, .. } => Some(json!({
                "answer_or_error": object,
                "log": entry.seq.and_then(|seq| match spec.check_kind(from)? {
                    Kind::Judge(_) => Some(paths.check_output(seq)),
                    Kind::Test(_) | Kind::Script(_) => Some(paths.check_log(seq)),
                    _ => None,
                }),
            })),
            _ => None,
        })
        .take(1)
        .collect::<Vec<_>>();
    let invalid = input.is_some_and(|input| input["edge"] == "invalid");
    let failure = if invalid
        && spec.step(from).is_some()
        && input.is_some_and(|input| input["outcome"] == "died")
    {
        "launch failure"
    } else if invalid && matches!(spec.check_kind(from), Some(Kind::Judge(_))) {
        "judge failure"
    } else {
        "workflow failure"
    };
    let trace = json!({
        "run": record.address().to_string(), "id": record.id,
        "workflow": record.workflow, "definition": record.spec_dir,
        "failure": failure, "input": input, "visits": record.visits.get(from),
        "history_entries": history.len(),
        "evaluations": history
            .iter()
            .filter(|entry| matches!(entry.event, JournalEvent::Evaluation { .. }))
            .count(),
        "attempt_and_visit_history": &history[history.len().saturating_sub(6)..], "checks": checks,
        "journal_tail": &journal[journal.len().saturating_sub(8)..],
        "journal": paths.journal,
        "context": paths.context,
    });
    let mut trace = format!("{trace:#}");
    let limit = crate::harness::prompt_compose::TEXT_PROMPT_LIMIT / 2;
    if trace.len() > limit {
        let mut end = limit;
        while !trace.is_char_boundary(end) {
            end -= 1;
        }
        trace.truncate(end);
        trace.push_str("\n[Trace excerpt truncated; read the full evidence below.]");
    }
    let root = shlex::try_quote(&record.project_root.to_string_lossy())
        .map_err(|error| WorkflowErr::Refused(error.to_string()))?
        .into_owned();
    let command = format!("rimz --root {root} workflow");
    Ok(format!(
        "Investigate this run's {failure}. The failure trace follows:\n\n{trace}\n\n\
         Full journal: {}\nContext: {}\n\n\
         You may fix the cause and run `{command} resume {} [--at <node>]`, \
         cancel with `{command} cancel {}`, or leave the decision to the user. \
         When the user must act immediately, ask now through your own native question. \
         Otherwise end your turn and wait for the user's instructions with your pane open. \
         RimZ never closes this debugger's pane. Closing it ends the run @failed.",
        paths.journal.display(),
        paths.context.display(),
        record.id,
        record.id,
    ))
}

pub(super) fn empty_observation() -> Observed {
    Observed {
        status: RunStatus::Pending,
        ask: None,
        limited: None,
        stopped: None,
        children: Children::default(),
        cost_usd: None,
        turn: None,
    }
}

pub(super) fn observed_outcome(observed: &Observed, on_asked: bool) -> Option<AttemptOutcome> {
    match observed.status {
        RunStatus::Completed => Some(AttemptOutcome::Ended),
        RunStatus::Failed | RunStatus::VerifyFailed | RunStatus::TimedOut => {
            Some(AttemptOutcome::Died)
        }
        RunStatus::Canceled => Some(AttemptOutcome::Canceled),
        RunStatus::BudgetExceeded => Some(AttemptOutcome::Limited),
        RunStatus::Pending | RunStatus::Running if on_asked && observed.ask.is_some() => {
            Some(AttemptOutcome::Asked)
        }
        _ => None,
    }
}

pub(super) fn fold_limited(observed: &mut Observed, outcome: AttemptOutcome) {
    if outcome == AttemptOutcome::Limited
        && (observed.limited.is_none() || observed.status == RunStatus::BudgetExceeded)
    {
        observed.limited = Some(Limited { reset_at: None });
    }
}

/// Fold blocking evidence into one hold, crediting overlapping pauses only once.
pub(super) fn hold(
    attempt: &mut workflow::LiveAttempt,
    ask: Option<&workflow::Asking>,
    stopped: Option<&super::super::Stopped>,
    now: Timestamp,
) -> bool {
    let previous = attempt.held_since;
    let asking = ask.map(|seen| workflow::Asking {
        since: match &attempt.asking {
            Some(open) => open.since,
            None => seen.since.max(attempt.started_at).min(now),
        },
        ask: seen.ask.clone(),
    });
    let since = asking
        .as_ref()
        .map(|asking| asking.since)
        .into_iter()
        .chain(
            stopped
                .filter(|stopped| stopped.kind != super::super::StopKind::Interrupt)
                .map(|stopped| stopped.since),
        )
        .min();
    let held_since = since.map(|since| {
        previous
            .map_or(since, |previous| previous.min(since))
            .max(attempt.started_at)
            .min(now)
    });
    let changed = attempt.asking != asking || attempt.held_since != held_since;
    if held_since.is_none()
        && let Some(closed) = previous
    {
        let paused = u64::try_from(now.duration_since(closed).as_millis()).unwrap_or(0);
        attempt.asked_ms = attempt.asked_ms.saturating_add(paused);
    }
    attempt.asking = asking;
    attempt.held_since = held_since;
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
                Some(run) => host.observe(run, Observation::Attempt)?,
                None => empty_observation(),
            };
            t.stage_agent_end(ending.clone(), &observed);
        }
        t.record.attempt.as_mut().expect("agent attempt").host = identity();
        persist(t, guard)?;
        return Ok(Decision::End(Box::new(t.record.clone())));
    }
    let rest_checked = t
        .spec
        .step(&attempt.node)
        .is_some_and(|step| !step.rest_check().is_empty());
    let mut observed = match &attempt.run {
        Some(run) => host.observe(
            run,
            if rest_checked {
                Observation::LeaderTurn
            } else {
                Observation::Attempt
            },
        )?,
        None => empty_observation(),
    };
    t.now = Timestamp::now();
    let missing_run = attempt.run.is_none();
    let on_asked = t
        .spec
        .edges(&attempt.node)
        .iter()
        .any(|edge| edge.outcome == "asked");
    let hold_changed = hold(
        t.record.attempt.as_mut().expect("observed agent attempt"),
        observed.ask.as_ref(),
        observed.stopped.as_ref(),
        t.now,
    );
    let ending = if !t.spec.is_break(&t.record.node)
        && !t.record.deadline_fired
        && t.record.deadline.is_some_and(|at| at <= t.now)
    {
        Some(Ending {
            outcome: AttemptOutcome::Canceled,
            cause: EndCause::Deadline,
            route: None,
            reason: None,
        })
    } else if t.timeout_at()?.is_some_and(|at| at <= t.now) {
        Some(Ending {
            outcome: AttemptOutcome::Timeout,
            cause: EndCause::Timeout,
            route: None,
            reason: None,
        })
    } else {
        observed_outcome(&observed, on_asked)
            .or_else(|| (missing_run && !host_live).then_some(AttemptOutcome::Canceled))
            .map(|outcome| Ending {
                outcome,
                cause: EndCause::Lifecycle,
                route: None,
                reason: None,
            })
    };
    let Some(ending) = ending else {
        if t.record
            .attempt
            .as_ref()
            .is_some_and(|attempt| attempt.push.as_ref().is_some_and(|push| !push.queued))
        {
            if hold_changed {
                persist(t, guard)?;
            }
            return Ok(Decision::Push(Box::new(t.record.clone())));
        }
        if rest::begin(t, &observed)? {
            persist(t, guard)?;
            return Ok(Decision::Again);
        }
        if hold_changed {
            persist(t, guard)?;
        }
        return Ok(Decision::Stop);
    };
    fold_limited(&mut observed, ending.outcome);
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

/// A published agent attempt ready to launch: its record, prompt, machine turn, and takeover.
pub(super) type Launch = (
    RunRecord,
    std::result::Result<String, String>,
    Option<crate::harness::throttle::Turn>,
    Option<super::super::Eviction>,
);

pub(super) fn launch(
    state: &StatePaths,
    paths: &WorkflowPaths,
    spec: &Definition,
    (record, prompt, throttle_turn, evict): Launch,
    host: &dyn AttemptHost,
) -> Result<bool> {
    let record = &record;
    // Preparation publishes the attempt before entering this off-lock launch.
    let attempt = record.attempt.as_ref().expect("prepared agent launch");
    let step = spec.step(&record.node).expect("prepared agent step");
    let prepared = prompt.and_then(|prompt| {
        let overrides = step
            .launch_overrides(&record.spec_dir)
            .map_err(|error| error.to_string())?;
        Ok((prompt, overrides))
    });
    let launched = match prepared {
        Ok((prompt, overrides)) => host
            .launch(StepLaunch {
                automation: crate::agents::Automation::Workflow {
                    workflow: record.workflow.clone(),
                    step: record.node.clone(),
                },
                workflow: WorkflowAttemptRef {
                    run: record.id.clone(),
                    seq: attempt.seq,
                    role: WorkflowAttemptRole::Step,
                },
                // Preparation dispatches only validated agent steps here.
                agent: match step.kind() {
                    Some(Kind::Agent(agent)) => agent.clone(),
                    _ => unreachable!("agent step layout"),
                },
                lifecycle: attempt.lifecycle,
                keep: spec.is_break(&record.node) || step.keep().unwrap_or(false),
                resume: record
                    .restart
                    .as_ref()
                    .and_then(|restart| restart.resume.clone()),
                prompt,
                workflow_note: super::super::brief::step(paths, spec, record),
                cwd: record.scope.path.clone(),
                throttle_turn,
                overrides,
                takeover: step.occupancy() == Some(super::super::OccupancyPolicy::Takeover),
                task: record.task.clone(),
                booked_by: record
                    .fire
                    .as_ref()
                    .map(|fire| fire.booked_by().to_vec())
                    .unwrap_or_default(),
                evict,
            })
            .unwrap_or_else(|error| {
                Err(NotLaunched::Failed(LaunchFailure {
                    outcome: AttemptOutcome::Died,
                    reason: error.to_string(),
                }))
            }),
        Err(reason) => Err(NotLaunched::Failed(LaunchFailure {
            outcome: AttemptOutcome::Died,
            reason,
        })),
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
            if spec.is_break(&record.node) {
                host.detach(&run)?;
            } else {
                host.stop(&run)?;
            }
        }
        return Ok(false);
    }
    let launched = match launched {
        Ok(run) => Ok(run),
        Err(NotLaunched::Unstarted(failure))
            if !t.record.launch_retry
                && failure.outcome == AttemptOutcome::Died
                && record.restart.as_ref().is_none_or(|r| r.resume.is_none())
                && t.record
                    .attempt
                    .as_ref()
                    .is_some_and(|a| a.ending.is_none()) =>
        {
            // Nothing started, so the next advance runs the step again; no attempt is counted.
            t.withdraw_attempt();
            t.record.launch_retry = true;
            t.step_wait("retry", Some(failure.reason));
            persist(&mut t, &guard)?;
            return Ok(false);
        }
        Err(NotLaunched::Failed(failure) | NotLaunched::Unstarted(failure)) => Err(failure),
        Err(NotLaunched::Taken(occupancy)) => {
            // The step waits as the first read would have, with no attempt counted.
            t.withdraw_attempt();
            let wait = super::occupancy_wait(&occupancy, step);
            if let Some(wait) = wait {
                t.step_wait(wait, None);
            }
            persist(&mut t, &guard)?;
            return Ok(wait.is_none());
        }
    };
    if record.restart.is_some() && t.record.restart == record.restart {
        t.record.restart = None;
    }
    let reason = match launched {
        Ok(run) => {
            t.record.attempt.as_mut().expect("current launch").run = Some(run);
            persist(&mut t, &guard)?;
            accounting::step(&t.record, None);
            if t.record
                .attempt
                .as_ref()
                .is_some_and(|attempt| attempt.ending.is_none())
            {
                // A terminal wake may have preceded publication of the run identity.
                match prepare_end(&mut t, host, &guard)? {
                    Decision::Again => return Ok(true),
                    Decision::End(_) => {}
                    _ => return Ok(false),
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
                        route: (failure.outcome == AttemptOutcome::Died).then(|| {
                            workflow::Verdict {
                                node: record.node.clone(),
                                entered_at: record.entered_at,
                                at: t.now,
                                target: spec.engine_target(&record.node, "invalid").to_string(),
                                object: t.engine_object(
                                    json!({"outcome":"died", "reason":failure.reason}),
                                ),
                                reason: workflow::ParkReason::Invalid,
                            }
                        }),
                        reason: None,
                    },
                    &observed,
                );
            }
            Some(failure.reason)
        }
    };
    t.record.attempt.as_mut().expect("launch ending").host = identity();
    persist(&mut t, &guard)?;
    let record = t.record;
    drop(guard);
    finish(state, paths, spec, &record, host, reason)
}

fn detaches(spec: &Definition, attempt: &LiveAttempt) -> bool {
    if spec.is_break(&attempt.node) {
        return true;
    }
    let ending = attempt.ending.as_ref().expect("published agent ending");
    let keep = spec.step(&attempt.node).and_then(Node::keep);
    keep.unwrap_or(false)
        && ending.cause == EndCause::Lifecycle
        && matches!(
            ending.outcome,
            AttemptOutcome::Ended | AttemptOutcome::Asked
        )
}

pub(super) fn finish(
    state: &StatePaths,
    paths: &WorkflowPaths,
    spec: &Definition,
    record: &RunRecord,
    host: &dyn AttemptHost,
    reason: Option<String>,
) -> Result<bool> {
    // Only a published ending reaches the off-lock stop cascade.
    let attempt = record.attempt.as_ref().expect("published agent ending");
    #[cfg(feature = "testkit")]
    crate::testkit::rendezvous("RIMZ_TEST_WORKFLOW_STOP_GATE");
    let detached = detaches(spec, attempt);
    let mut run = attempt.run.clone();
    if run.is_none() {
        let identity = WorkflowAttemptRef {
            run: record.id.clone(),
            seq: attempt.seq,
            role: WorkflowAttemptRole::Step,
        };
        run = host.orphan(state, &identity)?;
    }
    let (children, cost) = match &run {
        Some(run) => (
            if detached {
                host.detach(run)?
            } else {
                host.stop(run)?
            },
            host.observe(run, Observation::Attempt)?.cost_usd,
        ),
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
    if detached
        && !detaches(
            spec,
            t.record.attempt.as_ref().expect("current agent ending"),
        )
    {
        let record = t.record;
        drop(guard);
        return finish(state, paths, spec, &record, host, reason);
    }
    t.record.attempt.as_mut().expect("current agent ending").run = run;
    // A concurrent cancel may have replaced the ending while the stop ran.
    t.settle_agent(children, cost, head_end, reason)?;
    persist(&mut t, &guard)?;
    Ok(t.record.state == RunState::Active && t.record.wait.is_none())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::workflow::{Asked, Asking};

    #[test]
    fn observed_outcome_covers_status_ask_limit_and_asked_edge_on_either_lifecycle() {
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
            (false, true, None, None),
            (true, true, None, Some(AttemptOutcome::Asked)),
        ];
        let mut actual = Vec::new();
        let mut expected = Vec::new();
        for (status, terminal) in statuses {
            for (ask, limit, held, routed) in signals {
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
                        stopped: None,
                        children: Children::default(),
                        cost_usd: None,
                        turn: None,
                    };
                    let case = (status, ask, limit, on_asked);
                    actual.push((case, observed_outcome(&observed, on_asked)));
                    let live = if on_asked { routed } else { held };
                    expected.push((case, terminal.or(live)));
                }
            }
        }
        assert_eq!(actual, expected);
    }
}
