//! Workflow specifications, validation, hosted checks, and durable attempt transitions.

use std::path::{Path, PathBuf};

use crate::agents::WorkflowAttemptRef;
use crate::harness::schedule::takeover::OpenTurn;
use crate::store::run::RunStatus;
use crate::store::workflow::{Asked, AttemptOutcome, AttemptRun, Children, Limited};

mod engine;
mod script;
mod spec;
mod state;
mod validate;

pub(crate) use engine::rename_task;
pub use engine::{WorkflowErr, advance, cancel, create, resume, wake, wake_run};
pub use spec::{
    Check, Header, HeaderEdges, ScopeKind, SpecErr, Step, StepEdges, WorkflowSpec, load, schema,
};
pub use validate::{Edge, Issue, NodeRow, Validation, validate};

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
    wake_signal: Option<&str>,
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
        args.extend(["--wake-signal".into(), signal.into()]);
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
    pub profile: String,
    pub prompt: String,
    pub cwd: PathBuf,
    pub project_root: PathBuf,
}

#[derive(Clone, Debug)]
pub struct Observed {
    pub status: RunStatus,
    pub ask: Option<Asked>,
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
