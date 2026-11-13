//! Workflow/step start counts from live run records, and FIFO among their limit waiters.
//! The home-wide launch stamps track machine, workflow, and step stagger.

use std::collections::BTreeMap;
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::{Hold, HoldKind, Host, LimitScope, NAMED_AGENTS, ThrottleError, cap_hold, duration_ms};
use crate::ids::WorkflowRunId;
use crate::store::workflow::RunState;
use crate::utils::time::{TimeInputErr, format_duration_compact};

#[derive(Clone, Debug)]
pub(in crate::harness) struct WorkflowStart {
    pub(in crate::harness) workflow: String,
    pub(in crate::harness) step: String,
    pub(in crate::harness) run: WorkflowRunId,
    pub(in crate::harness) arrival: Timestamp,
    pub(in crate::harness) workflow_limit: Option<StartLimit>,
    pub(in crate::harness) step_limit: Option<StartLimit>,
}

/// A workflow or step `limit` table as its definition parsed it.
#[derive(Clone, Debug)]
pub(in crate::harness) struct StartLimit {
    pub(in crate::harness) max_parallel: Option<u32>,
    pub(in crate::harness) stagger: Option<Result<Duration, TimeInputErr>>,
}

#[derive(Default)]
struct Census {
    workflow: usize,
    step: usize,
    workflow_names: Vec<String>,
    step_names: Vec<String>,
    earlier_workflow: usize,
    earlier_step: usize,
}

/// One start per non-terminal run of the workflow whose record holds an agent session,
/// attributed to the session's own step; the starting run is the incoming one.
fn census(host: &dyn Host, start: &WorkflowStart) -> Result<Census, String> {
    let mut census = Census::default();
    for workspace in host.workspaces()? {
        for record in host
            .workflows(&workspace)?
            .iter()
            .filter(|record| !record.state.is_terminal() && record.workflow == start.workflow)
        {
            if let Some(wait) = record.wait.as_ref().filter(|wait| {
                record.state == RunState::Active
                    && wait.outcome == "capacity"
                    && (&wait.first, &record.id) < (&start.arrival, &start.run)
            }) {
                match wait.hold {
                    Some(HoldKind::WorkflowLimit) => census.earlier_workflow += 1,
                    Some(HoldKind::StepLimit) if record.node == start.step => {
                        census.earlier_step += 1;
                    }
                    _ => {}
                }
            }
            let Some(session) = record.session_attempt().filter(|_| record.id != start.run) else {
                continue;
            };
            let handle = session.run.as_ref().map(|run| run.handle.as_str());
            census.workflow += 1;
            name_counted(&mut census.workflow_names, handle);
            if session.node == start.step {
                census.step += 1;
                name_counted(&mut census.step_names, handle);
            }
        }
    }
    Ok(census)
}

/// Name a counted start by its recorded leader, up to the few a reason has room for.
fn name_counted(names: &mut Vec<String>, handle: Option<&str>) {
    if let Some(handle) = handle
        && names.len() < NAMED_AGENTS
    {
        names.push(handle.to_owned());
    }
}

fn hold(kind: HoldKind, text: String) -> Hold {
    Hold {
        kind,
        text,
        kinds: Vec::new(),
    }
}

fn scope(kind: HoldKind) -> &'static str {
    match kind {
        HoldKind::WorkflowLimit => "workflow",
        HoldKind::StepLimit => "step",
        _ => "",
    }
}

/// A limited start keeps FIFO standing while a same-workflow launch is in flight.
pub(super) fn launching(start: &WorkflowStart, count: usize) -> Hold {
    let (kind, name) = match start.workflow_limit {
        Some(_) => (HoldKind::WorkflowLimit, &start.workflow),
        None => (HoldKind::StepLimit, &start.step),
    };
    let plural = if count == 1 { "" } else { "s" };
    hold(
        kind,
        format!("{} {name}: {count} start{plural} launching", scope(kind)),
    )
}

/// The workflow and step holds for `start`, which adds one start whatever its width.
pub(super) fn evaluate(host: &dyn Host, start: &WorkflowStart) -> Result<Option<Hold>, String> {
    let census = census(host, start)?;
    for (kind, name, count) in [
        (
            HoldKind::WorkflowLimit,
            &start.workflow,
            census.earlier_workflow,
        ),
        (HoldKind::StepLimit, &start.step, census.earlier_step),
    ] {
        if count > 0 {
            let plural = if count == 1 { "" } else { "s" };
            return Ok(Some(hold(
                kind,
                format!(
                    "{} {name}: {count} earlier start{plural} waiting",
                    scope(kind)
                ),
            )));
        }
    }
    for (kind, name, limit, count, names) in [
        (
            HoldKind::WorkflowLimit,
            &start.workflow,
            &start.workflow_limit,
            census.workflow,
            census.workflow_names,
        ),
        (
            HoldKind::StepLimit,
            &start.step,
            &start.step_limit,
            census.step,
            census.step_names,
        ),
    ] {
        if let Some(cap) = limit.as_ref().and_then(|limit| limit.max_parallel)
            && let Some(hold) = cap_hold(
                kind,
                &format!("{} {name}: ", scope(kind)),
                cap,
                count,
                1,
                0,
                names,
            )
        {
            return Ok(Some(hold));
        }
    }
    let staggered =
        |limit: &Option<StartLimit>| limit.as_ref().is_some_and(|limit| limit.stagger.is_some());
    if !staggered(&start.workflow_limit) && !staggered(&start.step_limit) {
        return Ok(None);
    }
    let stamps = read_stamps(host)?;
    let workflow = stamps.workflows.get(&start.workflow);
    for (kind, name, limit, stamp) in [
        (
            HoldKind::WorkflowLimit,
            &start.workflow,
            &start.workflow_limit,
            workflow.map(|stamp| stamp.last_launch_ms),
        ),
        (
            HoldKind::StepLimit,
            &start.step,
            &start.step_limit,
            workflow.and_then(|stamp| stamp.steps.get(&start.step).copied()),
        ),
    ] {
        let Some(gap) = limit.as_ref().and_then(|limit| limit.stagger.as_ref()) else {
            continue;
        };
        let gap = *gap.as_ref().map_err(ToString::to_string)?;
        if let Some(remaining) = remaining(host, gap, stamp) {
            return Ok(Some(hold(
                kind,
                format!("{} {name}: next start in {remaining}", scope(kind)),
            )));
        }
    }
    Ok(None)
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct LaunchStamps {
    machine: Option<u64>,
    workflows: BTreeMap<String, WorkflowStamps>,
}

#[derive(Default, Deserialize, Serialize)]
struct WorkflowStamps {
    last_launch_ms: u64,
    steps: BTreeMap<String, u64>,
}

fn read_stamps(host: &dyn Host) -> Result<LaunchStamps, String> {
    let path = host.start_stamps();
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LaunchStamps::default());
        }
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display()))
}

fn remaining(host: &dyn Host, gap: Duration, stamp: Option<u64>) -> Option<String> {
    let remaining = duration_ms(gap).saturating_sub(host.now_ms().saturating_sub(stamp?));
    (remaining > 0).then(|| format_duration_compact(Duration::from_secs(remaining.div_ceil(1_000))))
}

pub(super) fn machine_hold(host: &dyn Host, gap: Duration) -> Result<Option<Hold>, String> {
    Ok(remaining(host, gap, read_stamps(host)?.machine)
        .map(|remaining| hold(HoldKind::Stagger, format!("next start in {remaining}"))))
}

pub(super) fn stamp(
    host: &dyn Host,
    machine: bool,
    scope: Option<&LimitScope>,
    at_ms: u64,
) -> super::Result<()> {
    let mut stamps = read_stamps(host).map_err(|detail| ThrottleError::Reading {
        reading: "launch stagger",
        detail,
    })?;
    if machine {
        stamps.machine = Some(at_ms);
    }
    if let Some(scope) = scope {
        let workflow = stamps.workflows.entry(scope.workflow.clone()).or_default();
        workflow.last_launch_ms = at_ms;
        workflow.steps.insert(scope.step.clone(), at_ms);
    }
    crate::disk::atomic::write_temp_then_rename_cache(&host.start_stamps(), &stamps)
        .map_err(ThrottleError::WriteStamps)
}
