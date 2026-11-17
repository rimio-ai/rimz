//! What an edited definition no longer fits in a run, and the ending that reads only the run's
//! record: `workflow cancel`'s, and the failure of a run its definition no longer fits.

use super::*;
use crate::store::workflow::{EvaluationOrigin, Verdict};

/// Why the run no longer fits `spec`, naming the node, or `None` when it fits. The run fits
/// when its node is in the definition; each live or held attempt sits at a node of the class
/// it was published as, an agent step keeping its lifecycle; a held session, or a session with
/// a push-back pending, sits at a step with rest checks; a held session's judging check is
/// still in its step's `rest-check` list; a staged asked or errored ending sits at a step that
/// routes it, and a staged asked ending at a judge check that routes it, which only an ending
/// an older RimZ staged reaches, since validation refuses a judge's `on.asked`; and a held wait
/// verdict names a case its check still has.
pub(super) fn misfit(spec: &Definition, record: &RunRecord) -> Option<String> {
    let changed = |node: &str, what: &str| {
        Some(format!(
            "{node}: the workflow definition changed under the run: {what}"
        ))
    };
    if spec.node(&record.node).is_none() {
        return changed(&record.node, "it is no longer in the definition");
    }
    let held = record.held.as_ref();
    let attempts = record
        .attempt
        .iter()
        .map(|attempt| (attempt, false))
        .chain(held.map(|held| (&held.attempt, true)));
    for (attempt, is_held) in attempts {
        let node = attempt.node.as_str();
        let step_kind = spec.step(node).and_then(Node::kind);
        let check_kind = spec.check(node).and_then(Node::kind);
        let (fits, was) = match attempt.kind {
            AttemptKind::Agent | AttemptKind::Team => {
                (matches!(step_kind, Some(Kind::Agent(_))), "an agent step")
            }
            AttemptKind::Judge => (matches!(check_kind, Some(Kind::Judge(_))), "a judge check"),
            AttemptKind::Script if attempt.message.is_some() => (
                matches!(step_kind, Some(Kind::Message(_))),
                "a message step",
            ),
            AttemptKind::Script if attempt.visit.is_some() => (
                matches!(step_kind, Some(Kind::Command(_))),
                "a command step",
            ),
            AttemptKind::Script => (
                check_kind.is_some() && !matches!(check_kind, Some(Kind::Judge(_))),
                "a check",
            ),
            AttemptKind::Workflow => (true, ""),
        };
        if !fits {
            return if spec.node(node).is_none() {
                changed(node, "it is no longer in the definition")
            } else {
                changed(node, &format!("it is no longer {was}"))
            };
        }
        let routed = attempt
            .ending
            .as_ref()
            .filter(|_| {
                matches!(
                    attempt.kind,
                    AttemptKind::Agent | AttemptKind::Team | AttemptKind::Judge
                )
            })
            .filter(|ending| {
                ending.route.is_none()
                    && ending.cause == EndCause::Lifecycle
                    && match ending.outcome {
                        AttemptOutcome::Asked => true,
                        // A judge's errored turn is a failure, which always has a route.
                        AttemptOutcome::Errored => attempt.kind != AttemptKind::Judge,
                        _ => false,
                    }
            })
            .map(|ending| ending.outcome.as_str());
        if let Some(outcome) = routed
            && !spec.edges(node).iter().any(|edge| edge.outcome == outcome)
        {
            return changed(node, &format!("it no longer routes {outcome}"));
        }
        let Some(step) = spec
            .step(node)
            .filter(|_| attempt.kind != AttemptKind::Workflow)
        else {
            continue;
        };
        if !matches!(attempt.kind, AttemptKind::Agent | AttemptKind::Team) {
            continue;
        }
        if step.lifecycle() != attempt.lifecycle {
            return changed(
                node,
                &format!(
                    "its lifecycle is now {}, not {}",
                    lifecycle_word(step.lifecycle()),
                    lifecycle_word(attempt.lifecycle)
                ),
            );
        }
        if step.rest_check().is_empty() && (is_held || attempt.push.is_some()) {
            return changed(node, "it no longer has rest checks");
        }
        if is_held
            && !step
                .rest_check()
                .iter()
                .any(|check| check.as_str() == record.node)
        {
            return changed(
                &record.node,
                &format!("it is no longer a rest check of {node}"),
            );
        }
    }
    let verdict = held.and_then(|held| held.verdict.as_ref())?;
    let case = (verdict.object["outcome"] == "hold")
        .then(|| verdict.object["case"].as_u64())
        .flatten()?;
    let cases = spec
        .check(&verdict.node)
        .map_or(0, |check| check.cases().len());
    if usize::try_from(case).is_ok_and(|case| case < cases) {
        return None;
    }
    changed(&verdict.node, &format!("it no longer has case {case}"))
}

fn lifecycle_word(lifecycle: workflow::StepLifecycle) -> &'static str {
    if lifecycle.is_turn() {
        "turn"
    } else {
        "session"
    }
}

/// How `close` ends the run.
pub(super) enum Closing<'r> {
    /// `workflow cancel`, with its reason.
    Cancel(Option<&'r str>),
    /// Failed, with the reason its outcome carries.
    Fail(String),
}

/// End the run reading only its record, as `workflow cancel` does: a held session is
/// interrupted, a live agent or judge attempt is staged canceled and stopped off-lock (or left
/// to the host still launching it), a step's child is killed and its attempt settled, and a
/// check's evaluation is abandoned. A `Fail` first journals an engine fault naming the node and
/// its reason, which the run's outcome reads, and ends the run failed through no route, a try
/// as well (`Transition::misfit`). An agent attempt a close already staged is closed: a later
/// close only finishes the stop, as the first one meant it.
pub(super) fn close(
    state: &StatePaths,
    paths: &WorkflowPaths,
    mut t: Transition<'_>,
    guard: WorkspaceLock,
    host: &dyn AttemptHost,
    closing: Closing<'_>,
) -> Result<()> {
    if let Some(attempt) = &t.record.attempt
        && attempt.kind.owns_agent()
        && attempt
            .ending
            .as_ref()
            .is_some_and(|ending| ending.cause == EndCause::Cancel)
    {
        if crate::proc::process_is_live(attempt.host.pid, attempt.host.start.as_deref()) {
            return Ok(());
        }
        t.record.attempt.as_mut().expect("staged agent ending").host = agent::identity();
        persist(&mut t, &guard)?;
        let (spec, record) = (t.spec, t.record);
        drop(guard);
        agent::finish(state, paths, spec, &record, host, None)?;
        return Ok(());
    }
    let word = match closing {
        Closing::Cancel(_) => "cancel",
        Closing::Fail(..) => "definition changed",
    };
    if t.record.held.is_some() {
        rest::interrupt(&mut t, word);
    }
    t.close_wait();
    let (reason, route) = match &closing {
        Closing::Cancel(reason) => (reason.map(str::to_owned), None),
        Closing::Fail(reason) => {
            let node = t.record.node.clone();
            let object = t.engine_object(json!({"outcome":"fault", "node":node, "reason":reason}));
            t.push(
                None,
                JournalEvent::Evaluation {
                    origin: EvaluationOrigin::Engine,
                    outcome: "fault".into(),
                    cost_usd: None,
                    object: object.clone(),
                    digests: BTreeMap::new(),
                    started_at: t.now,
                    ended_at: t.now,
                    stderr_tail: None,
                },
            );
            let verdict = Verdict {
                node,
                entered_at: t.record.entered_at,
                at: t.now,
                target: Target::Failed.to_string(),
                object,
                reason: ParkReason::Invalid,
            };
            (Some(reason.clone()), Some(verdict))
        }
    };
    let ending = Ending {
        outcome: AttemptOutcome::Canceled,
        cause: EndCause::Cancel,
        route,
        reason: reason.clone(),
    };
    let spec = t.spec;
    if let Some(attempt) = &t.record.attempt
        && attempt.kind.owns_agent()
    {
        let busy = (attempt.ending.is_some() || attempt.run.is_none())
            && crate::proc::process_is_live(attempt.host.pid, attempt.host.start.as_deref());
        let observed = match &attempt.run {
            Some(run) => host.observe(run, Observation::Attempt)?,
            None => agent::empty_observation(),
        };
        // A route staged on a judged turn closes that turn; only a judged turn has one.
        if let Some(verdict) = attempt
            .ending
            .as_ref()
            .and_then(|ending| ending.route.as_ref())
            .filter(|_| attempt.judged.is_some())
        {
            let attempt = attempt.clone();
            let verdict = verdict.clone();
            t.turn_end(
                &attempt,
                workflow::TurnEndAction::Interrupted,
                Some(&verdict),
                Some(word.into()),
            );
        }
        t.stage_agent_end(ending, &observed);
        if !busy {
            t.record.attempt.as_mut().expect("staged agent ending").host = agent::identity();
        }
        persist(&mut t, &guard)?;
        let record = t.record;
        drop(guard);
        if !busy {
            agent::finish(state, paths, spec, &record, host, None)?;
        }
        return Ok(());
    }
    t.record.restart = None;
    let orphan = orphaned_notice(&t.record);
    let settled = match &closing {
        Closing::Cancel(_) => None,
        Closing::Fail(reason) => Some(reason.clone()),
    };
    if !t.finish_script(ending, None, None, settled)? {
        match closing {
            Closing::Cancel(_) => {
                t.record.state = RunState::Canceled;
                t.record.terminal_at = Some(t.now);
                t.push(None, JournalEvent::Cancel { reason });
                t.push(
                    None,
                    JournalEvent::Terminal {
                        state: RunState::Canceled,
                        tried: None,
                    },
                );
            }
            Closing::Fail(reason) => t.misfit(reason)?,
        }
    }
    persist(&mut t, &guard)?;
    drop(guard);
    if let Some(message) = orphan {
        host.retire(&message)?;
    }
    Ok(())
}
