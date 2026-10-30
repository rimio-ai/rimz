use super::super::{LeaderTurn, Observed};
use super::*;
use crate::store::workflow::{HeldSession, StepLifecycle, TurnEndAction, Verdict};

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
        .filter(|turn| turn.at_rest && observed.ask.is_none() && observed.limited.is_none())
}

pub(super) fn begin(t: &mut Transition<'_>, observed: &Observed) -> bool {
    let Some(turn) = eligible(observed) else {
        return false;
    };
    // prepare_end calls only for a published, validated agent attempt with no ending.
    let attempt = t.record.attempt.as_mut().expect("observed agent attempt");
    if attempt.lifecycle != StepLifecycle::Until || attempt.judged.as_ref() == Some(&turn.end) {
        return false;
    }
    attempt.judged = Some(turn.end.clone());
    let check = t.spec.steps[&attempt.node]
        .then
        .clone()
        .expect("validated agent step has a then check");
    t.record.held = Some(HeldSession {
        attempt: t.record.attempt.take().expect("observed until attempt"),
        entered_at: t.record.entered_at,
        verdict: None,
    });
    t.record.node = check;
    t.record.entered_at = t.now;
    t.record.check_retry = 0;
    t.record.check_retry_context = None;
    true
}

pub(super) fn interrupt(t: &mut Transition<'_>, reason: &str) {
    // Held preflight and cancel call only with a published held session.
    let verdict = t
        .record
        .held
        .as_ref()
        .expect("held session")
        .verdict
        .clone();
    t.abandon_check("canceled");
    t.restore_session();
    let attempt = t.record.attempt.as_ref().expect("restored session").clone();
    t.turn_end(
        &attempt,
        TurnEndAction::Interrupted,
        verdict.as_ref(),
        Some(reason.into()),
    );
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
            },
            &observed,
        );
        t.record.attempt.as_mut().expect("interrupted session").host = agent::identity();
        persist(t, guard)?;
        return Ok(Some(Decision::End(Box::new(t.record.clone()))));
    }
    let held = t.record.held.as_ref().expect("observed held session");
    let Some(verdict) = held.verdict.clone() else {
        if hold_changed {
            persist(t, guard)?;
        }
        return Ok(None);
    };
    if verdict.target == held.attempt.node || verdict.target == "@wait" {
        if verdict.target == "@wait" {
            t.merge_wait(&verdict.object, verdict.at.unwrap_or(t.now), false)?;
        } else {
            t.close_wait();
        }
        return restore(t, guard, seen, TurnEndAction::Kept, &verdict, None).map(Some);
    }
    if observed.turn.as_ref().map(|turn| &turn.end) != held.attempt.judged.as_ref() {
        return restore(
            t,
            guard,
            seen,
            TurnEndAction::Discarded,
            &verdict,
            Some("leader turn moved on".into()),
        )
        .map(Some);
    }
    if eligible(&observed).is_none() {
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
    verdict: &Verdict,
    reason: Option<String>,
) -> Result<Decision> {
    // Restoring a held session always publishes its original agent attempt.
    t.restore_session();
    seen.clear();
    let attempt = t.record.attempt.as_ref().expect("restored session").clone();
    t.turn_end(&attempt, action, Some(verdict), reason);
    persist(t, guard)?;
    write_context(&t.record, t.paths, t.spec)?;
    Ok(Decision::Again)
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
