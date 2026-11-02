//! Workflow and step limits in the locked throttle pass: the census of working
//! attempts across rooms, FIFO among limit waiters by `Wait.first`, and the
//! home-wide pace record `Turn::report_launch` stamps.

use std::collections::BTreeMap;
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::{
    AgentState, Hold, HoldKind, Host, LimitScope, ThrottleError, duration_ms, host_time, working,
};
use crate::agents::WorkflowAttemptRole;
use crate::harness::workflow::Limit;
use crate::ids::WorkflowRunId;
use crate::store::workflow::{LiveAttempt, RunRecord, RunState};
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

impl WorkflowStart {
    pub(super) fn task_cap_exempt(&self) -> bool {
        self.workflow_limit
            .as_ref()
            .is_some_and(|limit| limit.max_active.is_some())
    }
}

#[derive(Default)]
struct Census {
    workflow: usize,
    step: usize,
    workflow_with_you: usize,
    step_with_you: usize,
    earlier_workflow: usize,
    earlier_step: usize,
}

fn live_attempt(record: &RunRecord) -> Option<&LiveAttempt> {
    record
        .held
        .as_ref()
        .map(|held| &held.attempt)
        .or(record.attempt.as_ref())
}

fn census(host: &dyn Host, start: &WorkflowStart) -> Result<Census, String> {
    let now = host_time(host);
    let mut census = Census::default();
    let mut counted = BTreeMap::new();
    for workspace in host.workspaces()? {
        let workflows: Vec<_> = host
            .workflows(&workspace)?
            .into_iter()
            .filter(|record| !record.state.is_terminal() && record.workflow == start.workflow)
            .collect();
        for record in &workflows {
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
        if workflows.is_empty() {
            continue;
        }
        let agents = host.agents(&workspace)?;
        let runs = host.runs(&workspace)?;
        for agent in agents
            .iter()
            .filter(|agent| working(agent, now) && !agent.is_provider_subagent())
        {
            let Some(pin) = agent
                .workflow
                .as_ref()
                .filter(|pin| pin.role == WorkflowAttemptRole::Step)
            else {
                continue;
            };
            let Some(record) = workflows.iter().find(|record| record.id == pin.run) else {
                continue;
            };
            let live = live_attempt(record).filter(|attempt| attempt.seq == pin.seq);
            let leader = runs
                .iter()
                .any(|run| run.workflow.as_ref() == Some(pin) && run.matches_agent(agent));
            if !leader && !live.is_some_and(|attempt| attempt.run.is_none()) {
                continue;
            }
            counted.entry((pin.run.clone(), pin.seq)).or_insert((
                live.is_some_and(|attempt| attempt.node == start.step),
                live.is_some_and(|attempt| !attempt.lifecycle.is_turn()),
            ));
        }
    }
    for (step, with_you) in counted.values() {
        census.workflow += 1;
        census.step += usize::from(*step);
        census.workflow_with_you += usize::from(*with_you);
        census.step_with_you += usize::from(*step && *with_you);
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

/// The hold of a limited start while `count` same-workflow launches are in
/// flight, reported under its limit so the start keeps its FIFO standing.
pub(super) fn launching(start: &WorkflowStart, count: usize) -> Hold {
    let (kind, name) = match start.workflow_limit {
        Some(_) => (HoldKind::WorkflowLimit, &start.workflow),
        None => (HoldKind::StepLimit, &start.step),
    };
    let plural = if count == 1 { "" } else { "s" };
    hold(
        kind,
        format!("limit {name}: {count} start{plural} launching"),
    )
}

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
                format!("limit {name}: {count} earlier start{plural} waiting"),
            )));
        }
    }
    for (kind, name, limit, count, with_you) in [
        (
            HoldKind::WorkflowLimit,
            &start.workflow,
            &start.workflow_limit,
            census.workflow,
            census.workflow_with_you,
        ),
        (
            HoldKind::StepLimit,
            &start.step,
            &start.step_limit,
            census.step,
            census.step_with_you,
        ),
    ] {
        if let Some(cap) = limit.as_ref().and_then(|limit| limit.max_active)
            && count >= cap as usize
        {
            let suffix = if with_you == 0 {
                String::new()
            } else {
                format!(" ({with_you} with you)")
            };
            return Ok(Some(hold(
                kind,
                format!("limit {name} {count}/{cap} working{suffix}"),
            )));
        }
    }
    let paced = |limit: &Option<Limit>| limit.as_ref().is_some_and(|limit| limit.pace.is_some());
    if !paced(&start.workflow_limit) && !paced(&start.step_limit) {
        return Ok(None);
    }
    let paces = read_paces(host)?;
    let stamps = paces.get(&start.workflow);
    let now = host.now_ms();
    for (kind, name, limit, stamp) in [
        (
            HoldKind::WorkflowLimit,
            &start.workflow,
            &start.workflow_limit,
            stamps.map(|stamp| stamp.last_launch_ms),
        ),
        (
            HoldKind::StepLimit,
            &start.step,
            &start.step_limit,
            stamps.and_then(|stamp| stamp.steps.get(&start.step).copied()),
        ),
    ] {
        let Some(limit) = limit else { continue };
        let Some(pace) = limit.pace().map_err(|error| error.to_string())? else {
            continue;
        };
        let Some(stamp) = stamp else { continue };
        let remaining = duration_ms(pace).saturating_sub(now.saturating_sub(stamp));
        if remaining > 0 {
            let remaining = format_duration_compact(Duration::from_secs(remaining.div_ceil(1_000)));
            return Ok(Some(hold(
                kind,
                format!("limit {name} next start in {remaining}"),
            )));
        }
    }
    Ok(None)
}

#[derive(Default, Deserialize, Serialize)]
struct Pace {
    last_launch_ms: u64,
    steps: BTreeMap<String, u64>,
}

fn read_paces(host: &dyn Host) -> Result<BTreeMap<String, Pace>, String> {
    let path = host.workflow_pace();
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display()))
}

pub(super) fn stamp(host: &dyn Host, scope: &LimitScope, at_ms: u64) -> super::Result<()> {
    let mut paces = read_paces(host).map_err(|detail| ThrottleError::Reading {
        reading: "workflow pace",
        detail,
    })?;
    let pace = paces.entry(scope.workflow.clone()).or_default();
    pace.last_launch_ms = at_ms;
    pace.steps.insert(scope.step.clone(), at_ms);
    crate::disk::atomic::write_temp_then_rename_cache(&host.workflow_pace(), &paces)
        .map_err(ThrottleError::WritePace)
}

pub(super) fn reported(
    agents: &[AgentState],
    reference: &crate::ids::AgentSessionId,
    now: Timestamp,
) -> bool {
    agents.iter().any(|agent| {
        (agent.agent_id == *reference || agent.launch_id.as_ref() == Some(reference))
            && (working(agent, now)
                || agent.ended_at.is_some()
                || agent.status == super::AgentStatus::Failed)
    })
}
