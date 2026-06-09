//! Supervised interactive-agent runs.

use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::{AgentLifecycleObservation, LifecycleSignal};
use crate::ids::{AgentKind, AgentSessionId, PaneId, RunId, WorkspaceId};
use crate::ledger::StatePaths;
use crate::ledger::lock::WorkspaceLock;
use crate::ledger::run_store::{self, RunStoreErr};

pub const ENV_RUN_ID: &str = "RIMZ_RUN_ID";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    Auto,
    Ask,
    Yolo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Pending,
    Running,
    Completed,
    Failed,
    TimedOut,
}

impl RunStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::TimedOut)
    }

    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Completed => 0,
            Self::Failed => 1,
            Self::TimedOut | Self::Pending | Self::Running => 124,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: RunId,
    pub workspace_id: WorkspaceId,
    pub kind: AgentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentSessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<PaneId>,
    pub status: RunStatus,
    pub permission_mode: PermissionMode,
    pub prompt: String,
    pub worktree_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
    pub started_at: Timestamp,
    pub updated_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<Timestamp>,
}

impl RunRecord {
    pub fn new(
        workspace_id: WorkspaceId,
        kind: AgentKind,
        permission_mode: PermissionMode,
        prompt: String,
        worktree_path: PathBuf,
    ) -> Self {
        let now = Timestamp::now();
        Self {
            run_id: RunId::new(),
            workspace_id,
            kind,
            agent_id: None,
            pane_id: None,
            status: RunStatus::Pending,
            permission_mode,
            prompt,
            worktree_path,
            last_message: None,
            started_at: now,
            updated_at: now,
            completed_at: None,
        }
    }
}

pub fn create(paths: &StatePaths, record: &RunRecord) -> Result<()> {
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    run_store::write(&paths.runs_dir, record)
}

pub fn load(paths: &StatePaths, run_id: &RunId) -> Result<RunRecord> {
    run_store::load(&paths.runs_dir, run_id)
}

pub fn list(paths: &StatePaths) -> Result<Vec<RunRecord>> {
    run_store::list(&paths.runs_dir)
}

pub fn record_pane(paths: &StatePaths, run_id: &RunId, pane_id: PaneId) -> Result<RunRecord> {
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    let mut record = load(paths, run_id)?;
    if record.pane_id.as_ref() == Some(&pane_id) {
        return Ok(record);
    }
    record.pane_id = Some(pane_id);
    record.updated_at = Timestamp::now();
    run_store::write(&paths.runs_dir, &record)?;
    Ok(record)
}

pub fn timeout(paths: &StatePaths, run_id: &RunId) -> Result<RunRecord> {
    mark_terminal(paths, run_id, RunStatus::TimedOut).map(|(record, _wrote)| record)
}

pub fn fail(paths: &StatePaths, run_id: &RunId) -> Result<RunRecord> {
    mark_terminal(paths, run_id, RunStatus::Failed).map(|(record, _wrote)| record)
}

pub fn fail_if_nonterminal(paths: &StatePaths, run_id: &RunId) -> Result<Option<RunRecord>> {
    let (record, wrote) = mark_terminal(paths, run_id, RunStatus::Failed)?;
    Ok(wrote.then_some(record))
}

fn mark_terminal(
    paths: &StatePaths,
    run_id: &RunId,
    status: RunStatus,
) -> Result<(RunRecord, bool)> {
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    let mut record = load(paths, run_id)?;
    if record.status.is_terminal() {
        return Ok((record, false));
    }
    let now = Timestamp::now();
    record.status = status;
    record.updated_at = now;
    record.completed_at = Some(now);
    run_store::write(&paths.runs_dir, &record)?;
    Ok((record, true))
}

/// Fold one lifecycle observation into an optional run record update.
///
/// Returns `Some(record)` only when this observation newly makes the run
/// terminal, so callers can send exactly one wakeup datagram.
pub fn record_lifecycle(
    paths: &StatePaths,
    run_id: &RunId,
    kind: &str,
    observation: &AgentLifecycleObservation,
    last_message: Option<String>,
) -> Result<Option<RunRecord>> {
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    if observation.parent_agent_id.is_some()
        || matches!(
            observation.signal,
            LifecycleSignal::SubagentStarted | LifecycleSignal::SubagentStopped { .. }
        )
    {
        return Ok(None);
    }

    let mut record = load(paths, run_id)?;
    if record.kind.as_str() != kind || record.status.is_terminal() {
        return Ok(None);
    }

    match (&record.agent_id, &observation.agent_id) {
        (Some(bound), Some(observed)) if observed != bound => return Ok(None),
        (Some(_), None) => return Ok(None),
        (None, Some(observed)) => record.agent_id = Some(observed.clone()),
        (None, None) | (Some(_), Some(_)) => {}
    }

    let completion = terminal_status_for_signal(&observation.signal);
    let now = Timestamp::now();
    if let Some(status) = completion {
        record.status = status;
        record.last_message = last_message.or(record.last_message);
        record.updated_at = now;
        record.completed_at = Some(now);
        run_store::write(&paths.runs_dir, &record)?;
        Ok(Some(record))
    } else {
        if record.status == RunStatus::Pending {
            record.status = RunStatus::Running;
            record.updated_at = now;
            run_store::write(&paths.runs_dir, &record)?;
        }
        Ok(None)
    }
}

/// Terminal run status produced by one agent lifecycle signal.
///
/// Hook ingestion uses this same predicate before extracting the final
/// assistant message, so the deliverable and status transition stay in lockstep.
pub fn terminal_status_for_signal(signal: &LifecycleSignal) -> Option<RunStatus> {
    match signal {
        LifecycleSignal::TurnEnded {
            errored,
            parked_on_background,
        } if !parked_on_background => Some(if *errored {
            RunStatus::Failed
        } else {
            RunStatus::Completed
        }),
        LifecycleSignal::Ended => Some(RunStatus::Failed),
        _ => None,
    }
}

pub type Result<T> = std::result::Result<T, RunStoreErr>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use crate::agents::LifecycleSignal;
    use crate::ids::MuxName;
    use tempfile::tempdir;

    #[test]
    fn terminal_status_maps_to_exit_code() {
        assert_eq!(RunStatus::Completed.exit_code(), 0);
        assert_eq!(RunStatus::Failed.exit_code(), 1);
        assert_eq!(RunStatus::TimedOut.exit_code(), 124);
    }

    #[test]
    fn lifecycle_completion_writes_terminal_record_once() {
        let dir = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        let record = RunRecord::new(
            workspace_id,
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        create(&paths, &record).unwrap();
        let observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("sess-1")),
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
        );
        let completed = record_lifecycle(
            &paths,
            &record.run_id,
            "claude",
            &observation,
            Some("done".to_owned()),
        )
        .unwrap()
        .expect("terminal update");
        assert_eq!(completed.status, RunStatus::Completed);
        assert_eq!(completed.last_message.as_deref(), Some("done"));
        assert_eq!(completed.agent_id.as_deref(), Some("sess-1"));

        let repeated = record_lifecycle(
            &paths,
            &record.run_id,
            "claude",
            &observation,
            Some("done".to_owned()),
        )
        .unwrap();
        assert!(repeated.is_none());
    }

    #[test]
    fn subagent_observation_does_not_complete_parent_run() {
        let dir = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        let record = RunRecord::new(
            workspace_id,
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        create(&paths, &record).unwrap();
        let mut observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("child-1")),
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
        );
        observation.parent_agent_id = Some(AgentSessionId::from("sess-parent"));

        let update = record_lifecycle(
            &paths,
            &record.run_id,
            "claude",
            &observation,
            Some("child done".to_owned()),
        )
        .unwrap();
        assert!(update.is_none());
        let after = load(&paths, &record.run_id).unwrap();
        assert_eq!(after.status, RunStatus::Pending);
        assert_eq!(after.last_message, None);
    }

    #[test]
    fn same_kind_child_process_does_not_complete_bound_parent_run() {
        let dir = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        let record = RunRecord::new(
            workspace_id,
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        create(&paths, &record).unwrap();
        let parent = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("sess-parent")),
            LifecycleSignal::TurnStarted,
        );
        record_lifecycle(&paths, &record.run_id, "claude", &parent, None).unwrap();

        let child = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("sess-child")),
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
        );
        let update = record_lifecycle(
            &paths,
            &record.run_id,
            "claude",
            &child,
            Some("child done".to_owned()),
        )
        .unwrap();

        assert!(update.is_none());
        let after = load(&paths, &record.run_id).unwrap();
        assert_eq!(after.status, RunStatus::Running);
        assert_eq!(after.agent_id.as_deref(), Some("sess-parent"));
        assert_eq!(after.last_message, None);
    }

    #[test]
    fn timeout_marks_nonterminal_run_and_preserves_terminal_run() {
        let dir = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        let record = RunRecord::new(
            workspace_id,
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        create(&paths, &record).unwrap();

        let timed_out = timeout(&paths, &record.run_id).unwrap();
        assert_eq!(timed_out.status, RunStatus::TimedOut);
        assert!(timed_out.completed_at.is_some());

        let still_timed_out = fail(&paths, &record.run_id).unwrap();
        assert_eq!(still_timed_out.status, RunStatus::TimedOut);
    }

    #[test]
    fn record_pane_persists_launch_pane_id() {
        let dir = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        let record = RunRecord::new(
            workspace_id,
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        create(&paths, &record).unwrap();
        let pane_id = PaneId::from_parts(MuxName::Tmux, "%7");

        let updated = record_pane(&paths, &record.run_id, pane_id.clone()).unwrap();
        assert_eq!(updated.pane_id.as_ref(), Some(&pane_id));
        assert_eq!(
            load(&paths, &record.run_id).unwrap().pane_id.as_ref(),
            Some(&pane_id)
        );
    }
}
