//! Workflow specifications, validation, hosted checks, and durable attempt transitions.

use std::path::{Path, PathBuf};

use crate::agents::WorkflowAttemptRef;
use crate::harness::schedule::takeover::OpenTurn;
use crate::store::run::RunStatus;
use crate::store::workflow::{Asking, AttemptOutcome, AttemptRun, Children, Limited};

mod engine;
mod script;
mod spec;
mod state;
mod validate;

pub(crate) use engine::reassign_task;
pub use engine::{WorkflowErr, advance, cancel, create, resume, wake, wake_run};
pub use spec::{
    Case, Check, Header, HeaderEdges, ScopeKind, SpecErr, Step, StepEdges, WorkflowSpec, load,
    schema,
};
pub use validate::{Edge, Issue, NodeRow, Validation, validate};

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
    args
}

pub trait AttemptHost {
    fn occupancy(&self, scope: &Path, project_root: &Path) -> Result<Occupancy, WorkflowErr>;
    fn evict(&self, scope: &Path, project_root: &Path) -> Result<Vec<String>, WorkflowErr>;
    fn launch(&self, step: StepLaunch) -> Result<Result<AttemptRun, LaunchFailure>, WorkflowErr>;
    fn observe(&self, run: &AttemptRun) -> Result<Observed, WorkflowErr>;
    fn stop(&self, run: &AttemptRun) -> Result<Children, WorkflowErr>;
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
}

#[derive(Clone, Debug)]
pub struct Observed {
    pub status: RunStatus,
    pub ask: Option<Asking>,
    pub limited: Option<Limited>,
    pub children: Children,
    pub cost_usd: Option<f64>,
}

pub struct LaunchFailure {
    pub outcome: AttemptOutcome,
    pub reason: String,
}

#[cfg(test)]
mod tests;
