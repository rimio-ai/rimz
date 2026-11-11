//! Stamped open-agent counts and FIFO among workflow/step limit waiters.
//! The home-wide launch stamps track machine, workflow, and step stagger.

use std::collections::BTreeMap;
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::{
    AgentState, Hold, HoldKind, Host, LimitScope, ThrottleError, cap_hold, duration_ms,
    name_counted, open,
};
use crate::harness::throttle::Limit;
use crate::ids::WorkflowRunId;
use crate::store::workflow::RunState;
use crate::utils::time::format_duration_compact;

#[derive(Clone, Debug)]
pub(in crate::harness) struct WorkflowStart {
    pub(in crate::harness) workflow: String,
    pub(in crate::harness) step: String,
    pub(in crate::harness) run: WorkflowRunId,
    pub(in crate::harness) arrival: Timestamp,
    pub(in crate::harness) workflow_limit: Option<Limit>,
    pub(in crate::harness) step_limit: Option<Limit>,
    pub(in crate::harness) may_wait: bool,
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

fn census(host: &dyn Host, start: &WorkflowStart) -> Result<Census, String> {
    let mut census = Census::default();
    for workspace in host.workspaces()? {
        for record in host
            .workflows(&workspace)?
            .iter()
            .filter(|record| !record.state.is_terminal() && record.workflow == start.workflow)
        {
            let Some(wait) = record.wait.as_ref().filter(|wait| {
                record.state == RunState::Active
                    && wait.outcome == "capacity"
                    && (&wait.first, &record.id) < (&start.arrival, &start.run)
            }) else {
                continue;
            };
            match wait.hold {
                Some(HoldKind::WorkflowLimit) => census.earlier_workflow += 1,
                Some(HoldKind::StepLimit) if record.node == start.step => census.earlier_step += 1,
                _ => {}
            }
        }
        let agents = host.agents(&workspace)?;
        let workflow: Vec<_> = agents
            .iter()
            .filter(|agent| {
                open(agent)
                    && agent
                        .automation
                        .as_ref()
                        .and_then(|stamp| stamp.workflow_scope())
                        .is_some_and(|(workflow, _)| workflow == start.workflow)
            })
            .collect();
        let step: Vec<_> = workflow
            .iter()
            .copied()
            .filter(|agent| {
                agent
                    .automation
                    .as_ref()
                    .and_then(|stamp| stamp.workflow_scope())
                    .is_some_and(|(_, step)| step == start.step)
            })
            .collect();
        census.workflow += workflow.len();
        census.step += step.len();
        name_counted(&mut census.workflow_names, &agents, &workflow);
        name_counted(&mut census.step_names, &agents, &step);
    }
    Ok(census)
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

pub(super) fn evaluate(
    host: &dyn Host,
    start: &WorkflowStart,
    incoming: usize,
) -> Result<Option<Hold>, String> {
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
                incoming,
                0,
                names,
            )
        {
            return Ok(Some(hold));
        }
    }
    let staggered =
        |limit: &Option<Limit>| limit.as_ref().is_some_and(|limit| limit.stagger.is_some());
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
        let Some(limit) = limit else { continue };
        let Some(gap) = limit.stagger().map_err(|error| error.to_string())? else {
            continue;
        };
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

pub(super) fn reported(agents: &[AgentState], reference: &crate::ids::AgentSessionId) -> bool {
    agents
        .iter()
        .any(|agent| agent.agent_id == *reference || agent.launch_id.as_ref() == Some(reference))
}
