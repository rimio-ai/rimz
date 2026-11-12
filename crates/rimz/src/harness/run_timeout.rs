//! Producer-side supervised-run deadline detection.
//!
//! This module only reads durable run records and spawns a hidden CLI helper.
//! The helper owns the locked mutation, wakeup, and pane reclamation so the
//! sidebar producer remains read-only on the store.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::deadline::{interrupt_evidence, interrupt_expired, kill_due, stop_due_by_pane};
use crate::agents::AgentState;
use crate::ids::{RunId, WorkspaceId};
use crate::store::run::{RunListing, RunRecord};
use crate::{RuntimePaths, StatePaths};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunTimeoutRequest {
    pub workspace_id: WorkspaceId,
    pub run_id: RunId,
    /// The settle instant of the Interrupted marker the host read on the
    /// run's child card, for the helper to open the run's interrupt hold at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupted_at: Option<Timestamp>,
}

/// Detect overdue non-terminal runs, expired interrupt holds and unrecorded
/// interrupt markers, and ask short-lived helpers to settle them.
pub(crate) fn enforce(
    paths: &StatePaths,
    runtime: &RuntimePaths,
    agents: &[AgentState],
    now: Timestamp,
) -> Option<RunListing> {
    let records = match crate::harness::run::listing(paths) {
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
    for request in records
        .iter()
        .filter_map(|record| due_request(&runtime.workspace_id, record, agents, now))
    {
        spawn_timeout_helper(runtime, &request);
    }
    Some(records)
}

/// The helper request `record` is owed this pass, if any.
fn due_request(
    workspace_id: &WorkspaceId,
    record: &RunRecord,
    agents: &[AgentState],
    now: Timestamp,
) -> Option<RunTimeoutRequest> {
    let interrupted_at = interrupt_evidence(record, agents);
    (kill_due(record, now)
        || interrupt_expired(record, now)
        || interrupted_at.is_some()
        || stop_due_by_pane(record, now))
    .then(|| RunTimeoutRequest {
        workspace_id: workspace_id.clone(),
        run_id: record.run_id.clone(),
        interrupted_at,
    })
}

fn spawn_timeout_helper(runtime: &RuntimePaths, request: &RunTimeoutRequest) {
    let args = crate::child_process::agent_helper_argv("run-timeout", request);
    if let Err(err) =
        crate::child_process::spawn_detached_rimz(runtime, args, "supervised-run-timeout")
    {
        tracing::debug!(
            workspace = %runtime.workspace_id,
            run_id = %request.run_id,
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
    use crate::agents::PermissionMode;
    use crate::ids::{AgentKind, WorkspaceId};
    use crate::store::run::RunStatus;

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
        assert!(!kill_due(&run, now));

        run.deadline_at = Some(now - Duration::from_secs(1));
        assert!(kill_due(&run, now));

        run.status = RunStatus::Completed;
        assert!(!kill_due(&run, now));

        run.status = RunStatus::Running;
        run.deadline_at = Some(now + Duration::from_secs(1));
        assert!(!kill_due(&run, now));
    }

    #[test]
    fn a_pass_asks_a_helper_for_an_expired_hold_and_carries_a_fresh_marker() {
        let workspace = record().workspace_id;
        let now = Timestamp::now();
        let mut run = record();
        run.subagent = true;
        run.status = RunStatus::Running;
        run.agent_id = Some("child".into());
        assert_eq!(due_request(&workspace, &run, &[], now), None);

        let mut child = AgentState::stub("codex", "child", crate::agents::AgentStatus::Running);
        child.last_activity = now - Duration::from_secs(10);
        let mut context = crate::agents::AgentContext::new("codex", now);
        context.settle = Some(crate::agents::context::TurnSettle {
            at: now - Duration::from_secs(5),
            outcome: crate::agents::context::TurnSettleOutcome::Interrupted,
        });
        child.context = Some(context);
        let request = due_request(&workspace, &run, std::slice::from_ref(&child), now)
            .expect("a marker asks for a stamp");
        assert_eq!(request.interrupted_at, Some(now - Duration::from_secs(5)));

        run.interrupted_at = Some(now - Duration::from_secs(5));
        assert_eq!(
            due_request(&workspace, &run, std::slice::from_ref(&child), now),
            None,
            "a stamped run is not selected again inside its window"
        );
        let request = due_request(&workspace, &run, &[child], now + Duration::from_secs(55))
            .expect("an expired hold asks for its settle");
        assert_eq!(request.interrupted_at, None);
    }
}
