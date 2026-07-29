//! Producer-side supervised-run deadline detection.
//!
//! This module only reads durable run records and spawns a hidden CLI helper.
//! The helper owns the locked mutation, wakeup, and pane reclamation so the
//! sidebar producer remains read-only on the store.

use std::ffi::OsString;

use jiff::Timestamp;

use crate::harness::run::{RunRecord, RunStatus};
use crate::{RuntimePaths, StatePaths};

/// Detect overdue non-terminal runs and ask short-lived helpers to settle them.
pub fn enforce(paths: &StatePaths, runtime: &RuntimePaths, now: Timestamp) {
    let records = match crate::store::run_store::list(&paths.runs_dir) {
        Ok(records) => records,
        Err(err) => {
            tracing::debug!(
                workspace = %runtime.workspace_id,
                error = &err as &dyn std::error::Error,
                "sidebar: failed to read supervised runs for timeout enforcement",
            );
            return;
        }
    };
    for record in records.iter().filter(|record| is_overdue(record, now)) {
        spawn_timeout_helper(runtime, record);
    }
}

fn is_overdue(record: &RunRecord, now: Timestamp) -> bool {
    matches!(record.status, RunStatus::Pending | RunStatus::Running)
        && record.deadline_at.is_some_and(|deadline| deadline <= now)
}

fn spawn_timeout_helper(runtime: &RuntimePaths, record: &RunRecord) {
    let args: Vec<OsString> = vec![
        "agents".into(),
        "run-timeout".into(),
        "--workspace-id".into(),
        runtime.workspace_id.as_str().into(),
        "--run-id".into(),
        record.run_id.as_str().into(),
    ];
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
