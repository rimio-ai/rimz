//! Workflow run phrases shared by workflow and loop inspection.

use jiff::Timestamp;
use rimz::harness::workflow::WorkflowSpec;
use rimz::store::workflow::{Asking, AttemptKind, LiveAttempt, ParkReason, RunRecord, RunState};

use super::render;

pub(super) fn sealed_specs(
    state: &rimz::StatePaths,
    records: &[RunRecord],
) -> std::collections::BTreeMap<rimz::ids::WorkflowRunId, WorkflowSpec> {
    records
        .iter()
        .filter(|record| !record.state.is_terminal())
        .filter_map(|record| {
            rimz::harness::workflow::load(&state.workflow(&record.id).home)
                .ok()
                .map(|spec| (record.id.clone(), spec))
        })
        .collect()
}

pub(super) fn session_attempt(record: &RunRecord) -> Option<&LiveAttempt> {
    record
        .held
        .as_ref()
        .map(|held| &held.attempt)
        .or(record.attempt.as_ref())
        .filter(|attempt| attempt.kind == AttemptKind::Agent)
}

pub(super) fn with_you(record: &RunRecord) -> bool {
    record.state == RunState::Active
        && session_attempt(record)
            .is_some_and(|attempt| !attempt.lifecycle.is_turn() && attempt.run.is_some())
}

pub(super) fn asking(record: &RunRecord) -> Option<&Asking> {
    if record.state != RunState::Active {
        return None;
    }
    session_attempt(record)?.asking.as_ref()
}

pub(super) fn since(record: &RunRecord) -> Timestamp {
    if record.state == RunState::Parked
        && let Some(park) = &record.park
    {
        return park.at;
    }
    asking(record).map_or(record.entered_at, |asking| asking.since)
}

pub(super) fn wait(
    record: &RunRecord,
    spec: Option<&WorkflowSpec>,
    now: Timestamp,
) -> Option<String> {
    wait_counted(record, spec, now, true)
}

pub(super) fn wait_counted(
    record: &RunRecord,
    spec: Option<&WorkflowSpec>,
    now: Timestamp,
    show_count: bool,
) -> Option<String> {
    let wait = record.wait.as_ref()?;
    if wait.outcome == "capacity" {
        return Some(format!(
            "{} · {}, ×{}",
            rimz::theme::fmt::capacity_wait(&wait.kinds, wait.first, wait.gives_up_at, now),
            render::one_line(wait.detail.as_deref().unwrap_or("—")),
            wait.count,
        ));
    }
    let detail = match wait.outcome.as_str() {
        "hold" => {
            let hold = spec
                .and_then(|spec| spec.checks.get(&wait.node))
                .and_then(|check| wait.case.and_then(|index| check.cases.get(index as usize)))
                .and_then(|case| case.hold.as_deref())
                .unwrap_or("?");
            format!(
                "holding {} {}/{hold}",
                render::one_line(wait.detail.as_deref().unwrap_or("—")),
                render::age_short(wait.first, now),
            )
        }
        "unknown" | "window" => match wait.detail.as_deref() {
            Some(detail) => format!(
                "{}{}{detail}",
                wait.outcome,
                if wait.outcome == "unknown" { ": " } else { " " },
                detail = render::one_line(detail),
            ),
            None => wait.outcome.clone(),
        },
        "takeover" | "occupied" => wait.outcome.clone(),
        _ => format!("said {}", wait.outcome),
    };
    let recheck = wait
        .recheck_at
        .map(|at| format!(", recheck {}", render::workflow_clock(at, now)))
        .unwrap_or_default();
    let count = if show_count {
        format!(", ×{}", wait.count)
    } else {
        String::new()
    };
    Some(format!("{detail}{count}{recheck}"))
}

pub(super) fn park(
    record: &RunRecord,
    spec: Option<&WorkflowSpec>,
    now: Timestamp,
) -> Option<String> {
    if record.state != RunState::Parked {
        return None;
    }
    let park = record.park.as_ref()?;
    let derived = match park.reason {
        ParkReason::Cap => spec
            .and_then(|spec| spec.steps.get(&record.node))
            .and_then(|step| step.tripped_visit_bound(record))
            .map(|(_, used, max)| format!("{used} of {max} visits")),
        ParkReason::Timeout => spec
            .and_then(|spec| {
                spec.checks
                    .get(&record.node)
                    .and_then(|check| check.timeout.as_ref())
                    .or_else(|| spec.steps.get(&record.node)?.timeout.as_ref())
            })
            .map(|timeout| format!("waited {timeout}")),
        ParkReason::Deadline => record
            .deadline
            .map(|at| format!("passed {}", render::workflow_clock(at, now))),
        _ => None,
    };
    Some(match derived.as_deref().or(park.text.as_deref()) {
        Some(text) => format!("{}: {}", park.reason.as_str(), render::one_line(text)),
        None => park.reason.as_str().into(),
    })
}

pub(super) fn detail(record: &RunRecord, spec: Option<&WorkflowSpec>, now: Timestamp) -> String {
    if record.state.is_terminal() {
        return String::new();
    }
    if let Some(asking) = asking(record) {
        let ask = &asking.ask;
        return format!(
            "asking you: {} ({}): {}",
            ask.handle,
            ask.kind,
            render::one_line(&ask.text),
        );
    }
    if with_you(record) {
        let mut detail = "with you".to_owned();
        if record.held.is_some() {
            detail.push_str(&format!("; checking {}", record.node));
        }
        if let Some(wait) = wait(record, spec, now) {
            detail.push_str(&format!("; {wait}"));
        }
        return detail;
    }
    if let Some(park) = park(record, spec, now) {
        return park;
    }
    if let Some(wait) = wait(record, spec, now) {
        return wait;
    }
    if let Some(step) = spec.and_then(|spec| spec.steps.get(&record.node)) {
        return step
            .visit_bounds(
                record.visits.get(&record.node),
                record.key.as_ref(),
                record.visit_grant,
            )
            .next()
            .map_or_else(
                || "running".into(),
                |(_, used, max)| format!("running · visit {used} of {max}"),
            );
    }
    "checking".into()
}
