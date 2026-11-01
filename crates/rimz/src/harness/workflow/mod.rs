//! Workflow specifications, validation, hosted checks, and durable attempt transitions.

use std::path::{Path, PathBuf};

use crate::Store;
use crate::agents::{AgentState, WorkflowAttemptRef};
use crate::harness::schedule::takeover::OpenTurn;
use crate::ids::{EventId, RunId, WorkflowRunId};
use crate::store::StoreErr;
use crate::store::run::RunStatus;
use crate::store::workflow::{Asking, AttemptOutcome, AttemptRun, Children, Limited, TurnEnd};

mod engine;
mod graph;
mod inspect;
mod script;
mod spec;
mod state;
mod validate;

pub(crate) use engine::reassign_task;
pub use engine::{WorkflowErr, advance, cancel, create, resume, wake, wake_run};
pub use graph::{Edge, EdgeSource, Fireability, NeverReason, Walk, WalkNode};
pub use inspect::{
    Hop, HopKind, evaluation_target, hold_until, hops, next_bound, repeats_evaluation, timeout_at,
};
pub use script::{CaseMark, CaseMarks, case_marks, unread_reasons};
pub use spec::{
    Case, Check, CheckKind, Header, HeaderEdges, ScopeKind, SpecErr, Step, StepEdges, StepKind,
    WorkflowSpec, load, schema,
};
pub use validate::{Issue, NodeRow, Validation, validate};

fn head(path: &Path) -> Option<String> {
    let output = crate::proc::run_bounded_git_output(
        path,
        &["rev-parse", "HEAD"],
        std::time::Duration::from_secs(2),
    )
    .ok()?;
    if output.timed_out || !output.status.success() {
        return None;
    }
    Some(String::from_utf8(output.stdout).ok()?.trim().to_owned())
}

pub struct ScopeRoots {
    pub checkout: PathBuf,
    pub project: PathBuf,
}

impl ScopeRoots {
    pub fn path(&self, kind: ScopeKind) -> &Path {
        match kind {
            ScopeKind::Worktree => &self.checkout,
            ScopeKind::Project => &self.project,
        }
    }
}

pub fn advance_args(
    project_root: &Path,
    id: Option<&crate::ids::WorkflowRunId>,
    wake_signal: Option<&crate::harness::schedule::signal::Signal>,
    woken_by: Option<&EventId>,
) -> Vec<std::ffi::OsString> {
    let mut args = vec![
        "--root".into(),
        project_root.as_os_str().into(),
        "workflow".into(),
        "advance".into(),
    ];
    if let Some(id) = id {
        args.push(id.as_str().into());
    }
    if let Some(signal) = wake_signal {
        let signal = serde_json::json!(crate::store::event::SignalEventPayload::from(signal));
        args.extend(["--wake-signal".into(), signal.to_string().into()]);
    }
    if let Some(event) = woken_by {
        args.extend(["--woken-by".into(), event.to_string().into()]);
    }
    args
}

pub trait AttemptHost {
    fn occupancy(&self, scope: &Path, project_root: &Path) -> Result<Occupancy, WorkflowErr>;
    fn evict(&self, scope: &Path, project_root: &Path) -> Result<Vec<String>, WorkflowErr>;
    fn launch(&self, step: StepLaunch) -> Result<Result<AttemptRun, LaunchFailure>, WorkflowErr>;
    fn observe(&self, run: &AttemptRun, observation: Observation) -> Result<Observed, WorkflowErr>;
    fn stop(&self, run: &AttemptRun) -> Result<Children, WorkflowErr>;
}

pub enum Observation {
    Attempt,
    LeaderTurn,
}

pub enum Occupancy {
    Free,
    Occupied(Vec<String>),
    Blocked(Vec<(String, OpenTurn)>),
}

pub struct StepLaunch {
    pub workflow: WorkflowAttemptRef,
    pub agent: String,
    pub lifecycle: crate::store::workflow::StepLifecycle,
    pub prompt: String,
    pub cwd: PathBuf,
    pub throttle_turn: Option<crate::harness::schedule::throttle::Turn>,
}

#[derive(Clone, Debug)]
pub struct Observed {
    pub status: RunStatus,
    pub ask: Option<Asking>,
    pub limited: Option<Limited>,
    pub children: Children,
    pub cost_usd: Option<f64>,
    pub turn: Option<LeaderTurn>,
}

#[derive(Clone, Debug)]
pub struct LeaderTurn {
    pub end: TurnEnd,
    pub at_rest: bool,
}

/// Read the leader's closed turn and current eligibility while holding the workspace lock.
/// The advance's waking frame has committed, so only other pending frames can defer rest.
pub fn leader_turn(
    store: &Store,
    leader: &AgentState,
    run: &WorkflowRunId,
    session: &RunId,
    woken_by: Option<&EventId>,
) -> Result<Option<LeaderTurn>, StoreErr> {
    let Some((started_at, ended_at)) = leader.turn_started_at.zip(leader.turn_ended_at) else {
        return Ok(None);
    };
    if ended_at < started_at {
        return Ok(None);
    }
    let mut turn = LeaderTurn {
        end: TurnEnd {
            agent_id: leader.agent_id.clone(),
            started_at,
            ended_at,
        },
        at_rest: false,
    };
    if super::idle_stop::resting(leader).is_err() {
        return Ok(Some(turn));
    }
    let messages = store.list_messages()?;
    let runs = super::run::listing(store.paths())?;
    let other_runs: Vec<_> = runs
        .iter()
        .filter(|run| &run.run_id != session)
        .cloned()
        .collect();
    if super::idle_stop::debt(store, &messages, &other_runs, leader)?.is_some() {
        return Ok(Some(turn));
    }
    let cursor = crate::store::ingress::read_cursor(store.paths())?;
    let (pending, _) = crate::store::ingress::read_from_offset(store.paths(), cursor.applied)?;
    turn.at_rest = !pending.iter().any(|(frame, _)| {
        Some(&frame.event_id) != woken_by
            && frame
                .env
                .get("RIMZ_WORKFLOW_RUN")
                .is_some_and(|id| id == run.as_str())
    });
    Ok(Some(turn))
}

pub struct LaunchFailure {
    pub outcome: AttemptOutcome,
    pub reason: String,
}

#[cfg(test)]
mod tests;
