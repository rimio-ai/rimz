//! Producer-side supervised-run deadline detection.
//!
//! This module only reads durable run records and spawns a hidden CLI helper.
//! The helper owns the locked mutation, wakeup, and pane reclamation so the
//! sidebar producer remains read-only on the store.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::harness::run::{RunRecord, RunStatus};
use crate::ids::{RunId, WorkspaceId};
use crate::{RuntimePaths, StatePaths};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunTimeoutRequest {
    pub workspace_id: WorkspaceId,
    pub run_id: RunId,
}

/// Detect overdue non-terminal runs and ask short-lived helpers to settle them.
pub fn enforce(
    paths: &StatePaths,
    runtime: &RuntimePaths,
    now: Timestamp,
) -> Option<Vec<RunRecord>> {
    let records = match crate::store::run_store::list(&paths.runs_dir) {
        Ok(records) => records,
        Err(err) => {
            tracing::debug!(
                workspace = %runtime.workspace_id,
                error = &err as &dyn std::error::Error,
                "sidebar: failed to read supervised runs for timeout enforcement",
            );
            return None;
        }
    };
    for record in records.iter().filter(|record| is_overdue(record, now)) {
        spawn_timeout_helper(runtime, record);
    }
    Some(records)
}

fn is_overdue(record: &RunRecord, now: Timestamp) -> bool {
    matches!(record.status, RunStatus::Pending | RunStatus::Running)
        && record.deadline_at.is_some_and(|deadline| deadline <= now)
}

fn spawn_timeout_helper(runtime: &RuntimePaths, record: &RunRecord) {
    let request = RunTimeoutRequest {
        workspace_id: runtime.workspace_id.clone(),
        run_id: record.run_id.clone(),
    };
    let args = crate::child_process::agent_helper_argv("run-timeout", &request);
    if let Err(err) =
        crate::child_process::spawn_detached_rimz(runtime, args, "supervised-run-timeout")
    {
        tracing::debug!(
            workspace = %runtime.workspace_id,
            run_id = %record.run_id,
            error = &err as &dyn std::error::Error,
            "sidebar: failed to spawn supervised run timeout helper",
        );
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;
    use crate::harness::run::PermissionMode;
    use crate::ids::{AgentKind, WorkspaceId};

    fn record() -> RunRecord {
        RunRecord::new(
            WorkspaceId::from_project_root(std::path::Path::new("/tmp/run-timeout")),
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "task".to_owned(),
            PathBuf::from("/tmp/run-timeout"),
        )
    }

    #[test]
    fn only_nonterminal_records_past_their_deadline_are_overdue() {
        let now = Timestamp::now();
        let mut run = record();
        assert!(!is_overdue(&run, now));

        run.deadline_at = Some(now - Duration::from_secs(1));
        assert!(is_overdue(&run, now));

        run.status = RunStatus::Completed;
        assert!(!is_overdue(&run, now));

        run.status = RunStatus::Running;
        run.deadline_at = Some(now + Duration::from_secs(1));
        assert!(!is_overdue(&run, now));
    }
}
