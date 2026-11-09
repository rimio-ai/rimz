//! Workflow specifications, validation, hosted checks, and durable attempt transitions.

use std::path::{Path, PathBuf};

use crate::Store;
use crate::agents::{AgentState, WorkflowAttemptRef};
use crate::harness::schedule::takeover::OpenTurn;
use crate::harness::schedule::when::head;
use crate::ids::{EventId, RunId, WorkflowRunId};
use crate::store::StoreErr;
use crate::store::run::RunStatus;
use crate::store::workflow::{Asking, AttemptOutcome, AttemptRun, Children, Limited, TurnEnd};

mod brief;
mod engine;
mod graph;
mod inspect;
mod script;
mod spec;
mod state;
mod validate;

pub use engine::{
    DefinitionSource, InlineDefinition, WorkflowErr, advance, cancel, create, restart, resume,
    wake, wake_run,
};
pub(crate) use engine::{hears_lifecycle, reassign_task};
pub use graph::{Edge, EdgeSource, Fireability, NeverReason, Walk, WalkNode};
pub use inspect::{
    Hop, HopKind, close_after, evaluation_target, hold_until, hops, next_bound, reached,
    repeats_evaluation, routing_hop, timeout_at,
};
pub use script::{CaseMark, CaseMarks, case_marks, condition_now, unread_reasons};
pub use spec::{
    Case, Check, CheckKind, Header, HeaderEdges, Issue, LaunchOverrides, Limit, OutcomeCap, Route,
    ScopeKind, SpecErr, Step, StepEdges, StepKind, WorkflowSpec, load, load_run, schema,
};
pub(crate) use spec::{parse, parse_run};
pub use validate::{NodeRow, Validation, validate};

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

pub fn cancel_args(id: &WorkflowRunId, reason: &str) -> Vec<std::ffi::OsString> {
    ["workflow", "cancel", id.as_str(), "--reason", reason]
        .map(std::ffi::OsString::from)
        .into()
}

pub trait AttemptHost {
    fn occupancy(
        &self,
        scope: &Path,
        project_root: &Path,
        excluded: &[String],
    ) -> Result<Occupancy, WorkflowErr>;
    fn launch(&self, step: StepLaunch) -> Result<Result<AttemptRun, NotLaunched>, WorkflowErr>;
    fn observe(&self, run: &AttemptRun, observation: Observation) -> Result<Observed, WorkflowErr>;
    fn push(
        &self,
        run: &AttemptRun,
        push: &crate::store::workflow::PushBack,
        text: &str,
    ) -> Result<Pushed, WorkflowErr>;
    /// Queue a subscribed signal for the leader's next done boundary under `message`.
    fn forward(
        &self,
        run: &AttemptRun,
        message: &crate::ids::MessageId,
        text: &str,
    ) -> Result<(), WorkflowErr>;
    /// Queue a message step's notice for `target`'s next done boundary under `message`,
    /// resolved as a shell in `scope` would; the inner error says why no one agent took it.
    fn message(
        &self,
        scope: &Path,
        target: &str,
        session: Option<&crate::ids::AgentSessionId>,
        message: &crate::ids::MessageId,
        text: &str,
    ) -> Result<Result<String, String>, WorkflowErr>;
    /// Withdraw a message step's notice whose attempt ended before it settled.
    fn retire(&self, message: &crate::ids::MessageId) -> Result<(), WorkflowErr>;
    fn stop(&self, run: &AttemptRun) -> Result<Children, WorkflowErr>;
    fn detach(&self, run: &AttemptRun) -> Result<Children, WorkflowErr>;
}

pub struct Pushed {
    pub queued: bool,
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
    pub automation: crate::agents::Automation,
    pub agent: String,
    pub lifecycle: crate::store::workflow::StepLifecycle,
    pub keep: bool,
    pub resume: Option<crate::ids::AgentSessionId>,
    pub prompt: String,
    pub workflow_note: String,
    pub cwd: PathBuf,
    pub throttle_turn: Option<crate::harness::schedule::throttle::Turn>,
    pub overrides: LaunchOverrides,
    /// The step stops its checkout's occupants, so a named worktree's stopped cohort
    /// relaunches fresh.
    pub takeover: bool,
    /// The loop task that started the run, whose agents launch as that task's.
    pub task: Option<String>,
    /// The checkout to read again once the launch is prepared: a takeover stops its occupants
    /// then, so a refused launch stops no one, and any other step refuses one that arrived.
    pub evict: Option<Eviction>,
}

/// An agent step's checkout, read as `AttemptHost::occupancy` read it.
pub struct Eviction {
    pub checkout: PathBuf,
    pub project_root: PathBuf,
    pub excluded: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Observed {
    pub status: RunStatus,
    pub ask: Option<Asking>,
    pub limited: Option<Limited>,
    pub stopped: Option<Stopped>,
    pub children: Children,
    pub cost_usd: Option<f64>,
    pub turn: Option<LeaderTurn>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stopped {
    pub since: jiff::Timestamp,
    pub kind: StopKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopKind {
    Limit,
    Error,
    Interrupt,
    Budget,
}

#[derive(Clone, Debug)]
pub struct LeaderTurn {
    pub end: TurnEnd,
    pub at_rest: bool,
}

/// Read the leader's closed turn and current eligibility while holding the workspace lock.
/// Only pending frames from this leader's supervised run can defer rest; companion hooks
/// do not decide the leader's rest. The advance's waking frame has already committed.
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
    if super::idle_stop::resting(leader, jiff::Timestamp::now()).is_err() {
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
            && frame
                .env
                .get("RIMZ_RUN_ID")
                .is_some_and(|id| id == session.as_str())
    });
    Ok(Some(turn))
}

pub struct LaunchFailure {
    pub outcome: AttemptOutcome,
    pub reason: String,
}

/// Why a launch opened no agent.
pub enum NotLaunched {
    /// The launch failed, ending its attempt with this outcome.
    Failed(LaunchFailure),
    /// The checkout's read once the launch was prepared found it taken; the step waits as it
    /// would on the first read, with no attempt counted.
    Taken(Occupancy),
}

impl From<LaunchFailure> for NotLaunched {
    fn from(failure: LaunchFailure) -> Self {
        Self::Failed(failure)
    }
}

#[cfg(test)]
mod tests;
