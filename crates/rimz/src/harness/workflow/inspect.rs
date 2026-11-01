use jiff::Timestamp;

use super::WorkflowSpec;
use crate::store::workflow::{
    AttemptOutcome, EvaluationOrigin, JournalEntry, JournalEvent, ParkReason, RunRecord, RunState,
    TurnEndAction,
};
use crate::utils::time::TimeInputErr;

/// A routing decision or lifecycle marker projected from the durable journal.
/// Markers have no outcome or target and count once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hop {
    pub at: Timestamp,
    pub node: String,
    pub outcome: Option<String>,
    pub target: Option<String>,
    pub count: u64,
    pub kind: HopKind,
}

/// Distinguish routing decisions from resume, park, and terminal markers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HopKind {
    Route,
    Resume,
    Park { reason: ParkReason },
    Terminal { state: RunState },
}

impl Hop {
    fn fold(&mut self, next: &Self) -> bool {
        if self.kind != HopKind::Route
            || next.kind != HopKind::Route
            || self.node != next.node
            || self.outcome != next.outcome
            || self.target != next.target
        {
            return false;
        }
        self.count += 1;
        true
    }
}

/// Fold consecutive matching evaluations, retaining the first decision's timestamp.
/// Wait summaries and the engine's duplicate route evidence add no routing decisions.
pub fn hops(spec: &WorkflowSpec, journal: &[JournalEntry]) -> Vec<Hop> {
    let mut hops: Vec<Hop> = Vec::new();
    let mut previous_evaluation = false;
    for (index, entry) in journal.iter().enumerate() {
        if repeats_evaluation(entry, index.checked_sub(1).map(|i| &journal[i])) {
            continue;
        }
        let Some((kind, outcome, target)) = hop_kind(spec, entry, &journal[index + 1..]) else {
            continue;
        };
        let hop = Hop {
            at: entry.at,
            node: match &entry.event {
                JournalEvent::TurnEnd { check, .. } => check.clone(),
                _ => entry.node.clone(),
            },
            outcome,
            target,
            count: 1,
            kind,
        };
        let evaluation = matches!(
            entry.event,
            JournalEvent::Evaluation { .. } | JournalEvent::TurnEnd { .. }
        );
        if evaluation && previous_evaluation && hops.last_mut().is_some_and(|last| last.fold(&hop))
        {
            continue;
        }
        hops.push(hop);
        previous_evaluation = evaluation;
    }
    hops
}

/// Whether a route repeats the immediately preceding evaluation's non-null answer.
pub fn repeats_evaluation(entry: &JournalEntry, previous: Option<&JournalEntry>) -> bool {
    let Some(previous) = previous else {
        return false;
    };
    match (&entry.event, &previous.event) {
        (
            JournalEvent::Route { outcome, .. },
            JournalEvent::Evaluation {
                outcome: evaluated,
                object,
                ..
            },
        ) => {
            entry.node == previous.node
                && entry.seq == previous.seq
                && outcome == evaluated
                && !object.is_null()
        }
        _ => false,
    }
}

fn hop_kind(
    spec: &WorkflowSpec,
    entry: &JournalEntry,
    following: &[JournalEntry],
) -> Option<(HopKind, Option<String>, Option<String>)> {
    if matches!(entry.event, JournalEvent::Route { .. }) && held_verdict(entry, following) {
        return None;
    }
    let outcome: &str = match &entry.event {
        JournalEvent::Evaluation { outcome, .. } => {
            return evaluation_target(spec, entry, following)
                .map(|target| (HopKind::Route, Some(outcome.clone()), Some(target)));
        }
        JournalEvent::Route { outcome, .. } => outcome,
        JournalEvent::Attempt { outcome, .. } => {
            if attempt_superseded(entry, *outcome, following) {
                return None;
            }
            outcome.as_str()
        }
        JournalEvent::TurnEnd {
            action: TurnEndAction::Kept | TurnEndAction::Closed,
            outcome: Some(outcome),
            target: Some(target),
            ..
        } => {
            return Some((HopKind::Route, Some(outcome.clone()), Some(target.clone())));
        }
        JournalEvent::Resume { .. } => return Some((HopKind::Resume, None, None)),
        JournalEvent::Park { reason, .. } => {
            return Some((HopKind::Park { reason: *reason }, None, None));
        }
        JournalEvent::Terminal { state } => {
            return Some((HopKind::Terminal { state: *state }, None, None));
        }
        _ => return None,
    };
    Some((
        HopKind::Route,
        Some(outcome.into()),
        Some(spec.target(&entry.node, outcome)?),
    ))
}

/// The routed target of an evaluation, shared by trail and full-history projection.
pub fn evaluation_target(
    spec: &WorkflowSpec,
    entry: &JournalEntry,
    following: &[JournalEntry],
) -> Option<String> {
    let JournalEvent::Evaluation {
        origin,
        outcome,
        object,
        ..
    } = &entry.event
    else {
        return None;
    };
    if object.is_null() || held_verdict(entry, following) {
        return None;
    }
    if (*origin == EvaluationOrigin::Check && outcome != "invalid")
        || following
            .first()
            .is_some_and(|next| repeats_evaluation(next, Some(entry)))
    {
        return spec.target(&entry.node, outcome);
    }
    None
}

fn held_verdict(entry: &JournalEntry, following: &[JournalEntry]) -> bool {
    let outcome = match &entry.event {
        JournalEvent::Evaluation { outcome, .. } | JournalEvent::Route { outcome, .. } => outcome,
        _ => return false,
    };
    for next in following {
        match &next.event {
            JournalEvent::TurnEnd {
                check,
                outcome: verdict,
                ..
            } => {
                return check == &entry.node && verdict.as_ref() == Some(outcome);
            }
            JournalEvent::Wait { .. }
            | JournalEvent::Wake { .. }
            | JournalEvent::Attempt { .. } => {}
            JournalEvent::Route { .. } if repeats_evaluation(next, Some(entry)) => {}
            _ => return false,
        }
    }
    false
}

fn attempt_superseded(
    entry: &JournalEntry,
    outcome: AttemptOutcome,
    following: &[JournalEntry],
) -> bool {
    let Some(next) = following.iter().find(|entry| {
        !matches!(
            entry.event,
            JournalEvent::Wait { .. } | JournalEvent::Wake { .. }
        )
    }) else {
        return false;
    };
    if next.node != entry.node {
        return false;
    }
    match (&next.event, outcome) {
        (JournalEvent::Route { outcome, .. }, AttemptOutcome::Asked) => outcome == "asked",
        (JournalEvent::Route { outcome, .. }, AttemptOutcome::Canceled) => outcome == "deadline",
        (
            JournalEvent::TurnEnd {
                action: TurnEndAction::Closed,
                ..
            },
            _,
        ) => true,
        (JournalEvent::Cancel, AttemptOutcome::Canceled) => true,
        _ => false,
    }
}

/// The first bound's instant and target, for inspection: `first_bound` routed by the spec.
pub fn next_bound(
    spec: &WorkflowSpec,
    record: &RunRecord,
    hold: Option<u32>,
    now: Timestamp,
) -> Result<Option<(Timestamp, String)>, TimeInputErr> {
    Ok(
        first_bound(spec, record, hold, now)?.and_then(|(at, outcome)| {
            spec.target(&record.node, outcome)
                .map(|target| (at, target))
        }),
    )
}

/// Earliest unfired deadline, timeout or supplied hold case, in that order on ties, with the
/// outcome it settles. Engine rechecks take the instant alone, so timing never waits on routing.
pub(super) fn first_bound<'a>(
    spec: &'a WorkflowSpec,
    record: &RunRecord,
    hold: Option<u32>,
    now: Timestamp,
) -> Result<Option<(Timestamp, &'a str)>, TimeInputErr> {
    let deadline = record
        .deadline
        .filter(|_| !record.deadline_fired)
        .map(|at| (at, "deadline"));
    let timeout = timeout_at(spec, record)?.map(|at| (at, "timeout"));
    let hold = match hold {
        Some(index) => hold_until(spec, record, index, now)?.zip(
            spec.checks
                .get(&record.node)
                .and_then(|check| check.cases.get(index as usize))
                .map(|case| case.outcome.as_str()),
        ),
        None => None,
    };
    Ok([deadline, timeout, hold]
        .into_iter()
        .flatten()
        .min_by_key(|(at, _)| *at))
}

/// The check's entry-based timeout or a turn step's timeout, paused while asking.
pub fn timeout_at(
    spec: &WorkflowSpec,
    record: &RunRecord,
) -> Result<Option<Timestamp>, TimeInputErr> {
    let (timeout, start) = if let Some(check) = spec.checks.get(&record.node) {
        (check.timeout.as_deref(), record.entered_at)
    } else {
        let Some(attempt) = record.attempt.as_ref() else {
            return Ok(None);
        };
        if !attempt.lifecycle.is_turn() || attempt.asking.is_some() {
            return Ok(None);
        }
        let Ok(start) = attempt
            .started_at
            .checked_add(std::time::Duration::from_millis(attempt.asked_ms))
        else {
            return Ok(None);
        };
        (
            spec.steps
                .get(&record.node)
                .and_then(|step| step.timeout.as_deref()),
            start,
        )
    };
    let Some(raw) = timeout else {
        return Ok(None);
    };
    Ok(start.checked_add(super::state::duration(raw)?).ok())
}

/// The case's hold instant, measured from its first matching settlement or `now`.
pub fn hold_until(
    spec: &WorkflowSpec,
    record: &RunRecord,
    case: u32,
    now: Timestamp,
) -> Result<Option<Timestamp>, TimeInputErr> {
    let Some(hold) = spec
        .checks
        .get(&record.node)
        .and_then(|check| check.cases.get(case as usize))
        .and_then(|case| case.hold.as_deref())
    else {
        return Ok(None);
    };
    let first = record
        .wait
        .as_ref()
        .filter(|wait| wait.is_hold() && wait.case == Some(case))
        .map_or(now, |wait| wait.first);
    Ok(first.checked_add(super::state::duration(hold)?).ok())
}
