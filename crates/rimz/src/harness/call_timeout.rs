//! Producer-side detection of a call's deadline: an `agent` or `team` call's `timeout`.
//!
//! This module only reads call records and agent rows and spawns a hidden CLI helper. The
//! helper owns the claim, the report, and the stop, so the sidebar producer stays read-only on
//! the store.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::AgentState;
use crate::ids::{AgentSessionId, WorkspaceId};
use crate::store::call::CallRecord;
use crate::{RuntimePaths, StatePaths};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallTimeoutRequest {
    pub workspace_id: WorkspaceId,
    /// The launch whose deadline passed, by its leader's launch id.
    pub launch_id: AgentSessionId,
}

/// Ask a helper to stop each launch past its call's deadline that still has a live seat.
pub(crate) fn enforce(
    paths: &StatePaths,
    runtime: &RuntimePaths,
    agents: &[AgentState],
    now: Timestamp,
) {
    let calls = match crate::store::call::list(paths) {
        Ok(calls) => calls,
        Err(error) => {
            tracing::debug!(
                workspace = %runtime.workspace_id,
                %error,
                "sidebar: failed to read call records for timeout enforcement",
            );
            return;
        }
    };
    for call in calls.iter().filter(|call| due(call, agents, now)) {
        let request = CallTimeoutRequest {
            workspace_id: runtime.workspace_id.clone(),
            launch_id: call.launch_id.clone(),
        };
        let args = crate::child_process::agent_helper_argv("call-timeout", &request);
        if let Err(error) = crate::child_process::spawn_detached_rimz(runtime, args, "call-timeout")
        {
            tracing::debug!(
                workspace = %runtime.workspace_id,
                launch_id = %call.launch_id,
                error = &error as &dyn std::error::Error,
                "sidebar: failed to spawn call timeout helper",
            );
        }
    }
}

/// Whether `call` is owed its stop this pass: past its deadline, not yet claimed, and with a
/// seat still live ([`super::call::live_seats`]). A launch that ended before its deadline is owed
/// nothing, and neither is the agent revived from it under a new launch id.
fn due(call: &CallRecord, agents: &[AgentState], now: Timestamp) -> bool {
    call.timed_out_at.is_none()
        && call.deadline.is_some_and(|deadline| deadline <= now)
        && !super::call::live_seats(call, agents).is_empty()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::agents::AgentStatus;
    use crate::store::call::Seat;

    #[test]
    fn only_a_passed_unclaimed_deadline_with_a_live_seat_is_due() {
        let now = Timestamp::now();
        let mut row = AgentState::stub("claude", "launch-timed", AgentStatus::Idle);
        row.launch_id = Some("launch-timed".into());
        let mut call = CallRecord {
            launch_id: "launch-timed".into(),
            seats: vec![Seat {
                kind: row.kind.clone(),
                launch_id: "launch-timed".into(),
            }],
            stay: true,
            schema: None,
            created_at: now,
            deadline: Some(now + Duration::from_secs(60)),
            answer: None,
            asked: None,
            timed_out_at: None,
        };
        let later = now + Duration::from_secs(60);
        assert!(
            !due(&call, std::slice::from_ref(&row), now),
            "before its deadline"
        );
        assert!(due(&call, std::slice::from_ref(&row), later));
        assert!(
            !due(&call, &[], later),
            "a launch that ended first is owed nothing"
        );
        let mut ended = row.clone();
        ended.ended_at = Some(now);
        assert!(
            !due(&call, &[ended], later),
            "an ended seat is owed nothing"
        );
        let mut revived = row.clone();
        revived.launch_id = Some("launch-revived".into());
        revived.prior_launch_ids = vec!["launch-timed".into()];
        assert!(
            !due(&call, &[revived], later),
            "an agent revived under a new launch id is another launch"
        );
        call.timed_out_at = Some(later);
        assert!(!due(&call, &[row], later), "an earlier claim took it");
    }
}
