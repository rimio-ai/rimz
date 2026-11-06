use super::super::{LeaderTurn, Observed};
use super::*;
use crate::store::workflow::{HeldSession, PushBack, StepLifecycle, TurnEndAction, Verdict};

pub(super) fn has_pending_verdict(record: &RunRecord) -> bool {
    record
        .held
        .as_ref()
        .is_some_and(|held| held.verdict.is_some())
}

fn eligible(observed: &Observed) -> Option<&LeaderTurn> {
    observed
        .turn
        .as_ref()
        .filter(|turn| turn.at_rest && observed.ask.is_none() && observed.stopped.is_none())
}

pub(super) fn begin(t: &mut Transition<'_>, observed: &Observed) -> Result<bool> {
    let Some(turn) = eligible(observed) else {
        return Ok(false);
    };
    // prepare_end calls only for a published agent attempt with no ending.
    let attempt = t.record.attempt.as_mut().expect("observed agent attempt");
    if attempt.lifecycle != StepLifecycle::Until || attempt.judged.as_ref() == Some(&turn.end) {
        return Ok(false);
    }
    let check = t.spec.steps[&attempt.node]
        .then
        .first()
        .cloned()
        .ok_or_else(|| {
            WorkflowErr::Refused(format!(
                "steps.{}.then: an until step requires a then check",
                attempt.node
            ))
        })?;
    attempt.judged = Some(turn.end.clone());
    t.record.held = Some(HeldSession {
        attempt: t.record.attempt.take().expect("observed until attempt"),
        entered_at: t.record.entered_at,
        verdict: None,
    });
    t.record.node = check;
    t.record.entered_at = t.now;
    t.record.check_retry = 0;
    t.record.check_retry_context = None;
    Ok(true)
}

pub(super) fn interrupt(t: &mut Transition<'_>, reason: &str) {
    // Callers interrupt only a published held session.
    let verdict = t
        .record
        .held
        .as_ref()
        .expect("held session")
        .verdict
        .clone();
    t.abandon_check("canceled");
    let attempt = t
        .record
        .held
        .as_ref()
        .expect("held session")
        .attempt
        .clone();
    t.turn_end(
        &attempt,
        TurnEndAction::Interrupted,
        verdict.as_ref(),
        Some(reason.into()),
    );
    t.restore_session();
}

pub(super) fn prepare(
    t: &mut Transition<'_>,
    host: &dyn AttemptHost,
    guard: &WorkspaceLock,
    seen: &mut BTreeSet<String>,
) -> Result<Option<Decision>> {
    // prepare dispatches here only while the session is held, with its run pinned.
    let held = t.record.held.as_ref().expect("held preflight");
    let mut observed = host.observe(
        held.attempt.run.as_ref().expect("held session has a run"),
        Observation::LeaderTurn,
    )?;
    t.now = Timestamp::now();
    let hold_changed = agent::hold(
        &mut t
            .record
            .held
            .as_mut()
            .expect("observed held session")
            .attempt,
        observed.ask.as_ref(),
        observed.stopped.as_ref(),
        t.now,
    );
    let ending = if !t.record.deadline_fired && t.record.deadline.is_some_and(|at| at <= t.now) {
        Some((AttemptOutcome::Canceled, EndCause::Deadline, "deadline"))
    } else {
        agent::observed_outcome(&observed, StepLifecycle::Until, false)
            .map(|outcome| (outcome, EndCause::Lifecycle, outcome_reason(outcome)))
    };
    if let Some((outcome, cause, reason)) = ending {
        agent::fold_limited(&mut observed, outcome);
        interrupt(t, reason);
        seen.clear();
        t.stage_agent_end(
            Ending {
                outcome,
                cause,
                route: None,
                reason: None,
            },
            &observed,
        );
        t.record.attempt.as_mut().expect("interrupted session").host = agent::identity();
        persist(t, guard)?;
        return Ok(Some(Decision::End(Box::new(t.record.clone()))));
    }
    let held = t.record.held.as_ref().expect("observed held session");
    let verdict = held.verdict.clone();
    let waiting = verdict
        .as_ref()
        .is_none_or(|verdict| verdict.target == "@wait");
    if observed.turn.as_ref().map(|turn| &turn.end) != held.attempt.judged.as_ref()
        || (waiting && eligible(&observed).is_none())
    {
        t.abandon_check("canceled");
        t.close_wait();
        return restore(
            t,
            guard,
            seen,
            TurnEndAction::Discarded,
            verdict.as_ref(),
            Some("leader turn moved on".into()),
        )
        .map(Some);
    }
    let Some(verdict) = verdict else {
        if hold_changed {
            persist(t, guard)?;
        }
        return Ok(None);
    };
    if verdict.target == "@wait" {
        t.merge_wait(&verdict.object, verdict.at.unwrap_or(t.now), true)?;
        let held = t.record.held.as_mut().expect("waiting session");
        held.verdict = None;
        let woken = std::mem::take(&mut held.attempt.woken);
        let attempt = held.attempt.clone();
        t.turn_end(&attempt, TurnEndAction::Kept, Some(&verdict), None);
        if woken {
            t.record.recheck_at = None;
            // merge_wait has just published this held wait.
            t.record.wait.as_mut().expect("merged held wait").recheck_at = None;
            seen.remove(&t.record.node);
        }
        persist(t, guard)?;
        return Ok(Some(if woken {
            Decision::Again
        } else {
            Decision::Stop
        }));
    }
    if verdict.target == t.record.held.as_ref().expect("held verdict").attempt.node {
        if eligible(&observed).is_none() {
            if hold_changed {
                persist(t, guard)?;
            }
            return Ok(Some(Decision::Stop));
        }
        t.close_wait();
        t.restore_session();
        seen.clear();
        t.record.key = verdict
            .object
            .get("key")
            .and_then(|key| serde_json::from_value(key.clone()).ok());
        t.record.input = Some(verdict.object.clone());
        let step = &t.spec.steps[&t.record.node];
        if let Some((bound, visits, max)) = step.tripped_visit_bound(&t.record) {
            let route = t.cap_route(bound, visits, max);
            t.stage_agent_end(
                Ending {
                    outcome: AttemptOutcome::Ended,
                    cause: EndCause::Lifecycle,
                    route: Some(route),
                    reason: None,
                },
                &observed,
            );
            t.record.attempt.as_mut().expect("capped session").host = agent::identity();
            persist(t, guard)?;
            return Ok(Some(Decision::End(Box::new(t.record.clone()))));
        }
        let bucket = t.record.visit_bucket();
        let bucket = (bucket.0.map(str::to_owned), bucket.1.cloned());
        t.record
            .visits
            .entry(t.record.node.clone())
            .or_default()
            .bump(bucket);
        let visit = t.record.visits[&t.record.node].for_bucket(t.record.visit_bucket());
        let attempt = t
            .record
            .attempt
            .as_mut()
            .expect("restored push-back session");
        let closed_visit = attempt.visit;
        attempt.visit = Some(visit);
        attempt.push = Some(PushBack {
            message: crate::ids::MessageId::new(),
            queued: false,
        });
        let mut attempt = attempt.clone();
        attempt.visit = closed_visit;
        t.turn_end(&attempt, TurnEndAction::Pushed, Some(&verdict), None);
        persist(t, guard)?;
        write_context(&t.record, t.paths, t.spec)?;
        return Ok(Some(Decision::Push(Box::new(t.record.clone()))));
    }
    let held = t.record.held.as_ref().expect("held verdict");
    if t.spec.steps[&held.attempt.node]
        .then
        .contains(&verdict.target)
    {
        if eligible(&observed).is_none() {
            if hold_changed {
                persist(t, guard)?;
            }
            return Ok(Some(Decision::Stop));
        }
        let attempt = held.attempt.clone();
        t.close_wait();
        t.turn_end(&attempt, TurnEndAction::Kept, Some(&verdict), None);
        t.record.held.as_mut().expect("moving held session").verdict = None;
        t.record.node = verdict.target;
        t.record.entered_at = t.now;
        t.record.check_retry = 0;
        t.record.check_retry_context = None;
        persist(t, guard)?;
        return Ok(Some(Decision::Again));
    }
    if super::super::close_after(t.spec, &t.record).is_some_and(|(at, _)| t.now < at)
        || eligible(&observed).is_none()
    {
        if hold_changed {
            persist(t, guard)?;
        }
        return Ok(Some(Decision::Stop));
    }
    t.restore_session();
    seen.clear();
    t.stage_agent_end(
        Ending {
            outcome: AttemptOutcome::Ended,
            cause: EndCause::Lifecycle,
            route: Some(verdict),
            reason: None,
        },
        &observed,
    );
    t.record.attempt.as_mut().expect("closing session").host = agent::identity();
    persist(t, guard)?;
    Ok(Some(Decision::End(Box::new(t.record.clone()))))
}

fn restore(
    t: &mut Transition<'_>,
    guard: &WorkspaceLock,
    seen: &mut BTreeSet<String>,
    action: TurnEndAction,
    verdict: Option<&Verdict>,
    reason: Option<String>,
) -> Result<Decision> {
    // Restoring a held session always publishes its original agent attempt.
    let attempt = t
        .record
        .held
        .as_ref()
        .expect("held session")
        .attempt
        .clone();
    t.turn_end(&attempt, action, verdict, reason);
    t.restore_session();
    seen.clear();
    persist(t, guard)?;
    write_context(&t.record, t.paths, t.spec)?;
    Ok(Decision::Again)
}

pub(super) fn push(
    state: &StatePaths,
    paths: &WorkflowPaths,
    spec: &WorkflowSpec,
    record: &RunRecord,
    host: &dyn AttemptHost,
) -> Result<bool> {
    // Preparation published this pending push and its owning agent attempt together.
    let attempt = record.attempt.as_ref().expect("prepared push-back");
    let push = attempt.push.as_ref().expect("prepared push-back id");
    let text = super::super::brief::push(paths, spec, record)?;
    let pushed = host.push(
        attempt.run.as_ref().expect("push-back session run"),
        push,
        &text,
    )?;
    if !pushed.queued {
        return Ok(false);
    }
    let guard = WorkspaceLock::acquire(&state.workspace_lock)?;
    let mut t = Transition {
        record: workflow::load(paths)?,
        spec,
        paths,
        now: Timestamp::now(),
        entries: Vec::new(),
    };
    if !t.current(attempt.seq) {
        return Ok(false);
    }
    let current = t
        .record
        .attempt
        .as_mut()
        .expect("current push-back attempt");
    let Some(current) = current
        .push
        .as_mut()
        .filter(|current| current.message == push.message)
    else {
        return Ok(false);
    };
    if !current.queued {
        current.queued = true;
        persist(&mut t, &guard)?;
    }
    Ok(true)
}

fn outcome_reason(outcome: AttemptOutcome) -> &'static str {
    match outcome {
        AttemptOutcome::Ended => "ended",
        AttemptOutcome::Died => "died",
        AttemptOutcome::Canceled => "canceled",
        AttemptOutcome::Limited => "limited",
        AttemptOutcome::Timeout => "timeout",
        AttemptOutcome::Asked => "asked",
    }
}
