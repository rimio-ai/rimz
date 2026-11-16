use super::super::{AttemptHost, LaunchFailure, NotLaunched, Observation, Observed, StepLaunch};
use super::*;
use crate::agents::{WorkflowAttemptRef, WorkflowAttemptRole};
use crate::store::run::RunStatus;
use crate::store::workflow::{Children, EndCause, Ending};

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
    if let Some(prompt_file) = step.prompt_file() {
        let file = spec::confined_file(&record.definition_dir(paths), prompt_file)?;
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

/// A judge's launch prompt: its check's prompt file, which validation requires, and the submit
/// contract.
pub(super) fn compose_judge(
    record: &RunRecord,
    paths: &WorkflowPaths,
    spec: &Definition,
    digests: &mut BTreeMap<String, String>,
) -> Result<String> {
    let Some(prompt_file) = spec.check(&record.node).and_then(Node::prompt_file) else {
        return Err(WorkflowErr::Refused(format!(
            "judge.{}.prompt-file: a judge check requires prompt-file",
            record.node
        )));
    };
    let file = spec::confined_file(&record.definition_dir(paths), prompt_file)?;
    let prompt = std::fs::read_to_string(file)?;
    digests.insert(prompt_file.into(), script::digest(prompt.as_bytes()));
    Ok(super::super::brief::judge_prompt(
        &prompt, paths, spec, record,
    ))
}

/// Stop the judges an interrupt abandoned, then clear their debt from the record in a later
/// locked write. The debt stays stored until its stop was attempted, so a pass that dies
/// between the two finds it again and repeats the stop. A workflow launch sets no run timeout,
/// so nothing else would stop the judge. `finish` makes the same stops ahead of its settlement
/// instead, so no write ends a run that still owes one.
pub(super) fn stop_abandoned(
    state: &StatePaths,
    paths: &WorkflowPaths,
    spec: &Definition,
    host: &dyn AttemptHost,
    runs: &[workflow::AttemptRun],
) -> Result<()> {
    if runs.is_empty() {
        return Ok(());
    }
    stop_owed(host, runs);
    let guard = WorkspaceLock::acquire(&state.workspace_lock)?;
    let Some(record) = super::reload(paths)? else {
        return Ok(());
    };
    let mut t = Transition {
        record,
        spec,
        paths,
        now: Timestamp::now(),
        entries: Vec::new(),
    };
    if clear_owed(&mut t.record, runs) {
        persist(&mut t, &guard)?;
    }
    Ok(())
}

/// Stop each owed judge. A stop that fails is logged and still counts as attempted, so a judge
/// that cannot be stopped never holds the run.
fn stop_owed(host: &dyn AttemptHost, runs: &[workflow::AttemptRun]) {
    for run in runs {
        if let Err(error) = host.stop(run) {
            tracing::warn!(run = %run.run_id, %error, "abandoned workflow judge not stopped");
        }
    }
}

/// Drop the debts whose stop was attempted, answering whether any was owed.
fn clear_owed(record: &mut RunRecord, runs: &[workflow::AttemptRun]) -> bool {
    let owed = record.abandoned.len();
    record
        .abandoned
        .retain(|debt| runs.iter().all(|run| run.run_id != debt.run_id));
    record.abandoned.len() != owed
}

pub(super) fn empty_observation() -> Observed {
    Observed {
        status: RunStatus::Pending,
        ask: None,
        limited: None,
        limited_by: None,
        stopped: None,
        errored: None,
        children: Children::default(),
        cost_usd: None,
        turn: None,
    }
}

/// The ending an observation decides. An ask is tested before an errored turn, and each ends
/// the attempt only when its node routes it.
pub(super) fn observed_outcome(
    observed: &Observed,
    on_asked: bool,
    on_errored: bool,
) -> Option<AttemptOutcome> {
    match observed.status {
        RunStatus::Completed => Some(AttemptOutcome::Ended),
        RunStatus::Failed | RunStatus::VerifyFailed | RunStatus::TimedOut => {
            Some(AttemptOutcome::Died)
        }
        RunStatus::Canceled => Some(AttemptOutcome::Canceled),
        RunStatus::Pending | RunStatus::Running if on_asked && observed.ask.is_some() => {
            Some(AttemptOutcome::Asked)
        }
        RunStatus::Pending | RunStatus::Running if on_errored && observed.errored.is_some() => {
            Some(AttemptOutcome::Errored)
        }
        _ => None,
    }
}

/// Fold blocking evidence into one hold, crediting overlapping pauses only once. The stop it
/// stores is the leader's own errored turn when there is one, else the merged stop.
pub(super) fn hold(
    attempt: &mut workflow::LiveAttempt,
    observed: &Observed,
    now: Timestamp,
) -> bool {
    let previous = attempt.held_since;
    let asking = observed.ask.as_ref().map(|seen| workflow::Asking {
        since: match &attempt.asking {
            Some(open) => open.since,
            None => seen.since.max(attempt.started_at).min(now),
        },
        ask: seen.ask.clone(),
    });
    let held_on = observed
        .errored
        .as_ref()
        .or(observed.stopped.as_ref())
        .and_then(super::super::Stopped::hold);
    let since = asking
        .as_ref()
        .map(|asking| asking.since)
        .into_iter()
        .chain(observed.stopped.as_ref().and_then(|stopped| {
            (stopped.kind != super::super::StopKind::Interrupt).then_some(stopped.since)
        }))
        .min();
    let held_since = since.map(|since| {
        previous
            .map_or(since, |previous| previous.min(since))
            .max(attempt.started_at)
            .min(now)
    });
    let held_on = held_since.and(held_on);
    let changed =
        attempt.asking != asking || attempt.held_since != held_since || attempt.held_on != held_on;
    if held_since.is_none()
        && let Some(closed) = previous
    {
        let paused = u64::try_from(now.duration_since(closed).as_millis()).unwrap_or(0);
        attempt.asked_ms = attempt.asked_ms.saturating_add(paused);
    }
    attempt.asking = asking;
    attempt.held_since = held_since;
    attempt.held_on = held_on;
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
    let judge = attempt.kind == AttemptKind::Judge;
    if let Some(ending) = &attempt.ending {
        if host_live {
            return Ok(Decision::Stop);
        }
        // A judge stages its evidence on its own attempt, so only a step's can go missing.
        if !judge
            && t.record
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
    let observed = match &attempt.run {
        Some(run) => host.observe(
            run,
            if rest_checked || judge {
                Observation::LeaderTurn
            } else {
                Observation::Attempt
            },
        )?,
        None => empty_observation(),
    };
    t.now = Timestamp::now();
    let missing_run = attempt.run.is_none();
    let edges = t.spec.edges(&attempt.node);
    let routes = |outcome: &str| edges.iter().any(|edge| edge.outcome == outcome);
    // A judge's ask leaves it in its pane, and its errored turn fails it at once.
    let (on_asked, on_errored) = if judge {
        (false, true)
    } else {
        (routes("asked"), routes("errored"))
    };
    let hold_changed = hold(
        t.record.attempt.as_mut().expect("observed agent attempt"),
        &observed,
        t.now,
    );
    let ending = if !t.record.deadline_fired && t.record.deadline.is_some_and(|at| at <= t.now) {
        Some(Ending {
            outcome: AttemptOutcome::Canceled,
            cause: EndCause::Deadline,
            route: None,
            reason: None,
        })
    } else if judge && attempt_answered(&t.record) {
        Some(Ending {
            outcome: AttemptOutcome::Ended,
            cause: EndCause::Lifecycle,
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
        observed_outcome(&observed, on_asked, on_errored)
            // A judge's session that ended without an answer died, however it ended.
            .map(|outcome| match outcome {
                AttemptOutcome::Ended if judge => AttemptOutcome::Died,
                outcome => outcome,
            })
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
        if judge {
            return remind(t, guard, &observed, hold_changed);
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

fn attempt_answered(record: &RunRecord) -> bool {
    record
        .attempt
        .as_ref()
        .is_some_and(|attempt| attempt.answer.is_some())
}

/// Remind a judge once, at its first clean rest without an answer; otherwise it waits.
fn remind(
    t: &mut Transition<'_>,
    guard: &WorkspaceLock,
    observed: &Observed,
    hold_changed: bool,
) -> Result<Decision> {
    // prepare_end hands on only its published judge attempt.
    let attempt = t.record.attempt.as_mut().expect("observed judge attempt");
    if attempt.reminded || rest::eligible(observed).is_none() {
        if hold_changed {
            persist(t, guard)?;
        }
        return Ok(Decision::Stop);
    }
    attempt.reminded = true;
    attempt.push = Some(workflow::PushBack {
        message: crate::ids::MessageId::new(),
        queued: false,
    });
    persist(t, guard)?;
    Ok(Decision::Push(Box::new(t.record.clone())))
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
    let launched = match prompt.and_then(|prompt| step_launch(paths, spec, record, prompt)) {
        Ok(launch) => host
            .launch(StepLaunch {
                throttle_turn,
                evict,
                ..launch
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
    let reloaded = super::reload(paths)?;
    let moved = reloaded.is_none();
    let mut t = Transition {
        record: reloaded.unwrap_or_else(|| record.clone()),
        spec,
        paths,
        now: Timestamp::now(),
        entries: Vec::new(),
    };
    if moved || !t.current(attempt.seq) {
        drop(guard);
        if let Ok(run) = launched {
            host.stop(&run)?;
        }
        return Ok(false);
    }
    // An ending staged during the launch (a cancel, a move, a misfit) settles through `finish`.
    let unstaged = t
        .record
        .attempt
        .as_ref()
        .is_some_and(|a| a.ending.is_none());
    let launched = match launched {
        Ok(run) => Ok(run),
        Err(NotLaunched::Unstarted(failure))
            if !t.record.launch_retry
                && failure.outcome == AttemptOutcome::Died
                && record.restart.as_ref().is_none_or(|r| r.resume.is_none())
                && unstaged =>
        {
            // Nothing started, so the next advance runs the step again; no attempt is counted.
            t.withdraw_attempt();
            t.record.launch_retry = true;
            t.step_wait("retry", Some(failure.reason));
            persist(&mut t, &guard)?;
            return Ok(false);
        }
        Err(NotLaunched::Failed(failure) | NotLaunched::Unstarted(failure)) => Err(failure),
        Err(NotLaunched::Taken(occupancy)) if unstaged => {
            // The step waits as the first read would have, with no attempt counted.
            t.withdraw_attempt();
            // Only a step's launch carries an eviction, so only a step's is taken.
            let step = spec
                .step(&record.node)
                .expect("an evicting launch is a step's");
            let wait = super::occupancy_wait(&occupancy, step);
            if let Some(wait) = wait {
                t.step_wait(wait, None);
            }
            persist(&mut t, &guard)?;
            return Ok(wait.is_none());
        }
        Err(NotLaunched::Budget(reason)) if unstaged => {
            // The budget was spent after the first read admitted the launch: the node waits as
            // that read would have, with no attempt counted, and a restart stays to be launched.
            t.withdraw_attempt();
            t.step_wait("budget", Some(reason));
            persist(&mut t, &guard)?;
            return Ok(false);
        }
        Err(NotLaunched::Taken(_)) => Err(LaunchFailure {
            outcome: AttemptOutcome::Died,
            reason: "the checkout was taken during the launch".into(),
        }),
        Err(NotLaunched::Budget(reason)) => Err(LaunchFailure {
            outcome: AttemptOutcome::Died,
            reason,
        }),
    };
    if record.restart.is_some() && t.record.restart == record.restart {
        t.record.restart = None;
    }
    let reason = match launched {
        Ok(run) => {
            t.record.attempt.as_mut().expect("current launch").run = Some(run);
            persist(&mut t, &guard)?;
            // A judge's account is its Evaluation entry and its decline assist.
            if attempt.kind != AttemptKind::Judge {
                accounting::step(&t.record, None);
            }
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
            let observed = empty_observation();
            if t.record
                .attempt
                .as_ref()
                .is_some_and(|attempt| attempt.ending.is_none())
            {
                t.stage_agent_end(
                    Ending {
                        outcome: failure.outcome,
                        cause: EndCause::Lifecycle,
                        route: None,
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

/// The launch an agent step or a judge takes, before its machine turn and eviction.
fn step_launch(
    paths: &WorkflowPaths,
    spec: &Definition,
    record: &RunRecord,
    prompt: String,
) -> std::result::Result<StepLaunch, String> {
    // Preparation publishes the attempt before entering this off-lock launch.
    let attempt = record.attempt.as_ref().expect("prepared agent launch");
    let workflow = WorkflowAttemptRef {
        run: record.id.clone(),
        seq: attempt.seq,
        role: role(attempt),
    };
    let automation = crate::agents::Automation::Workflow {
        workflow: record.workflow.clone(),
        step: record.node.clone(),
    };
    let booked_by = record
        .fire
        .as_ref()
        .map(|fire| fire.booked_by().to_vec())
        .unwrap_or_default();
    if attempt.kind == AttemptKind::Judge {
        // Preparation dispatches only validated judge checks here.
        let Some(Kind::Judge(agent)) = spec.check(&record.node).and_then(Node::kind) else {
            unreachable!("judge check layout")
        };
        return Ok(StepLaunch {
            automation,
            workflow,
            agent: agent.clone(),
            lifecycle: attempt.lifecycle,
            keep: false,
            resume: None,
            prompt,
            workflow_note: super::super::brief::judge(spec, record),
            cwd: record.scope.path.clone(),
            throttle_turn: None,
            overrides: super::super::LaunchOverrides::default(),
            takeover: false,
            task: record.task.clone(),
            booked_by,
            evict: None,
        });
    }
    let step = spec.step(&record.node).expect("prepared agent step");
    let overrides = step
        .launch_overrides(&record.definition_dir(paths))
        .map_err(|error| error.to_string())?;
    Ok(StepLaunch {
        automation,
        workflow,
        // Preparation dispatches only validated agent steps here.
        agent: match step.kind() {
            Some(Kind::Agent(agent)) => agent.clone(),
            _ => unreachable!("agent step layout"),
        },
        lifecycle: attempt.lifecycle,
        keep: step.keep().unwrap_or(false),
        resume: record
            .restart
            .as_ref()
            .and_then(|restart| restart.resume.clone()),
        prompt,
        workflow_note: super::super::brief::step(paths, spec, record),
        cwd: record.scope.path.clone(),
        throttle_turn: None,
        overrides,
        takeover: step.occupancy() == Some(super::super::OccupancyPolicy::Takeover),
        task: record.task.clone(),
        booked_by,
        evict: None,
    })
}

/// The pin role of an attempt that owns an agent run.
fn role(attempt: &LiveAttempt) -> WorkflowAttemptRole {
    if attempt.kind == AttemptKind::Judge {
        WorkflowAttemptRole::Judge
    } else {
        WorkflowAttemptRole::Step
    }
}

fn detaches(spec: &Definition, attempt: &LiveAttempt) -> bool {
    let ending = attempt.ending.as_ref().expect("published agent ending");
    let keep = spec.step(&attempt.node).and_then(Node::keep);
    keep.unwrap_or(false)
        && ending.cause == EndCause::Lifecycle
        && matches!(
            ending.outcome,
            AttemptOutcome::Ended | AttemptOutcome::Asked | AttemptOutcome::Errored
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
            role: role(attempt),
        };
        run = host.orphan(state, &identity)?;
    }
    let mut kind = None;
    let (children, cost) = match &run {
        Some(run) => (
            if detached {
                host.detach(run)?
            } else {
                let children = host.stop(run)?;
                // An unreadable record leaves the launch's turn to its report or ceiling.
                if let Ok(stopped) = crate::harness::run::load(state, &run.run_id) {
                    crate::harness::throttle::release_stopped(&stopped);
                    kind = Some(stopped.kind);
                }
                children
            },
            host.observe(run, Observation::Attempt)?.cost_usd,
        ),
        None => (Children::default(), None),
    };
    stop_owed(host, &record.abandoned);
    let head_end = super::super::head(&record.scope.path);
    // A judge settles against the definition as it reads after the stop, as a script check
    // does after its child; a step keeps the one its ending was staged under.
    let fresh = (attempt.kind == AttemptKind::Judge)
        .then(|| super::Reader::new().reread(paths, record))
        .flatten();
    let spec = fresh.as_ref().unwrap_or(spec);
    let guard = WorkspaceLock::acquire(&state.workspace_lock)?;
    let Some(current) = super::reload(paths)? else {
        return Ok(false);
    };
    let mut t = Transition {
        record: current,
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
    // A close stages its own ending and has checked the fit; any other judge ending checks it
    // against the reread definition before it settles.
    let closed = t
        .record
        .attempt
        .as_ref()
        .and_then(|current| current.ending.as_ref())
        .is_some_and(|ending| ending.cause == EndCause::Cancel);
    if attempt.kind == AttemptKind::Judge
        && !closed
        && let Some(reason) = super::fit::misfit(spec, &t.record)
    {
        super::fit::close(
            state,
            paths,
            t,
            guard,
            host,
            super::fit::Closing::Fail(reason),
        )?;
        // The close staged its ending on this live process, so this pass finishes it.
        let Some(record) = super::reload(paths)?.filter(|record| {
            record
                .attempt
                .as_ref()
                .is_some_and(|attempt| attempt.ending.is_some())
        }) else {
            return Ok(false);
        };
        return finish(state, paths, spec, &record, host, None);
    }
    let current = t.record.attempt.as_mut().expect("current agent ending");
    let woken = std::mem::take(&mut current.woken);
    current.run = run;
    // A concurrent cancel may have replaced the ending while the stop ran.
    t.settle_agent(children, cost, head_end, reason)?;
    // A judge a subscribed signal reached while it ran evaluates again rather than sleeping
    // on its answer's recheck time, as a woken script check does; a rest check's held session
    // takes its own wake in `rest::prepare`.
    let rechecks = woken && t.record.held.is_none() && t.record.wait.is_some();
    if rechecks && let Some(wait) = &mut t.record.wait {
        wait.recheck_at = None;
        t.record.recheck_at = None;
    }
    let answer = (attempt.kind == AttemptKind::Judge)
        .then(|| accepted(&t.entries, attempt.seq))
        .flatten();
    clear_owed(&mut t.record, &record.abandoned);
    persist(&mut t, &guard)?;
    drop(guard);
    if let (Some(answer), Some(kind)) = (answer, kind) {
        record_decline(spec, record, kind, &answer, cost);
    }
    Ok(t.record.state == RunState::Active && (t.record.wait.is_none() || rechecks))
}

/// The answer a judge's settlement accepted, read from its check Evaluation entry.
fn accepted(entries: &[workflow::JournalEntry], seq: u64) -> Option<serde_json::Value> {
    entries.iter().find_map(|entry| match &entry.event {
        JournalEvent::Evaluation {
            origin: workflow::EvaluationOrigin::Check,
            object,
            ..
        } if entry.seq == Some(seq) => Some(object.clone()),
        _ => None,
    })
}

/// An accepted answer from a judge in a loop-started run that holds the run's act back is one
/// assist. A loop fire's command row compiles no judge.
fn record_decline(
    spec: &Definition,
    record: &RunRecord,
    kind: crate::ids::AgentKind,
    object: &serde_json::Value,
    cost_usd: Option<f64>,
) {
    let (Some(task), Some(outcome)) = (record.task.as_ref(), object["outcome"].as_str()) else {
        return;
    };
    let Some(Kind::Judge(profile)) = spec.check_kind(&record.node) else {
        return;
    };
    if !declines(spec, &record.node, outcome) {
        return;
    }
    crate::harness::assist_log::append(&crate::harness::assist_log::AssistRecord {
        at: Timestamp::now(),
        assist: crate::harness::assist_log::Assist::JudgeDecline {
            task: task.clone(),
            checkout: record.scope.path.clone(),
            profile: profile.clone(),
            kind,
            reason: object["reason"].as_str().unwrap_or_default().to_owned(),
            cost_usd,
        },
    });
}

/// Whether a judge's verdict routes away from every step and judge check while its opposite
/// verdict leads into one: a judge that guards the run's act.
fn declines(spec: &Definition, node: &str, outcome: &str) -> bool {
    let opposite = match outcome {
        "pass" => "fail",
        "fail" => "pass",
        _ => return false,
    };
    let edges = spec.edges(node);
    let acts = |outcome: &str| {
        edges
            .iter()
            .find(|edge| edge.outcome == outcome)
            .map(|edge| {
                edge.target.node().is_some_and(|node| {
                    spec.step(node).is_some()
                        || matches!(spec.check_kind(node), Some(Kind::Judge(_)))
                })
            })
    };
    acts(outcome) == Some(false) && acts(opposite) == Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::workflow::{Limited, Stopped};
    use crate::store::workflow::{Asked, Asking};

    #[test]
    fn observed_outcome_covers_status_ask_error_limit_and_routed_edges_on_either_lifecycle() {
        let statuses = [
            (RunStatus::Pending, None),
            (RunStatus::Running, None),
            (RunStatus::Completed, Some(AttemptOutcome::Ended)),
            (RunStatus::Failed, Some(AttemptOutcome::Died)),
            (RunStatus::VerifyFailed, Some(AttemptOutcome::Died)),
            (RunStatus::TimedOut, Some(AttemptOutcome::Died)),
            (RunStatus::Canceled, Some(AttemptOutcome::Canceled)),
        ];
        let errored = Stopped {
            since: Timestamp::now(),
            kind: crate::harness::workflow::StopKind::Error,
            handle: Some("@coder".into()),
            class: Some(crate::agents::TurnErrorClass::Failed),
            text: Some("API Error: 500".into()),
        };
        let mut actual = Vec::new();
        let mut expected = Vec::new();
        for (status, terminal) in statuses {
            for ask in [false, true] {
                for error in [false, true] {
                    for limit in [false, true] {
                        for (on_asked, on_errored) in
                            [(false, false), (true, false), (false, true), (true, true)]
                        {
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
                                limited_by: None,
                                stopped: error.then(|| errored.clone()),
                                errored: error.then(|| errored.clone()),
                                children: Children::default(),
                                cost_usd: None,
                                turn: None,
                            };
                            let case = (status, ask, error, limit, on_asked, on_errored);
                            actual.push((case, observed_outcome(&observed, on_asked, on_errored)));
                            // A routed ask wins, then a routed error; a limit never ends a
                            // live attempt.
                            let live = if ask && on_asked {
                                Some(AttemptOutcome::Asked)
                            } else if error && on_errored {
                                Some(AttemptOutcome::Errored)
                            } else {
                                None
                            };
                            expected.push((case, terminal.or(live)));
                        }
                    }
                }
            }
        }
        assert_eq!(actual, expected);
    }
}
