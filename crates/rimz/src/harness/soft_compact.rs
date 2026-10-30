//! Shared soft-compaction decisions and detached helper requests.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::agents::AgentState;
use crate::agents::state::SoftCompactProgress;
use crate::ids::{AgentKind, AgentSessionId, PaneId, WorkspaceId};
use crate::store::message::{AutoCompact, SoftCompactStep};
use crate::store::snapshot::SidebarSnapshot;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoftCompactRequest {
    pub workspace_id: WorkspaceId,
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub pane_id: PaneId,
    pub label: String,
    pub step: SoftCompactStep,
}

pub fn due_step(
    snapshot: &SidebarSnapshot,
    agent: &AgentState,
    threshold: Option<AutoCompact>,
    continue_text: &str,
    now: Timestamp,
) -> Option<SoftCompactStep> {
    if continue_text.trim().is_empty()
        || agent.ended_at.is_some()
        || agent.is_provider_subagent()
        || agent.is_awaiting_input()
        || snapshot
            .live_agent_pane(&agent.kind, &agent.agent_id)
            .is_none()
    {
        return None;
    }
    if agent
        .soft_compact
        .is_some_and(|mark| mark.progress == SoftCompactProgress::TurnEnded)
    {
        return Some(SoftCompactStep::Continue);
    }
    let definition = crate::agents::definition_by_kind(agent.kind.as_str()).ok()?;
    if crate::agents::soft_compact_skip(definition).is_some()
        || !threshold.is_some_and(|threshold| threshold.triggered(agent))
        || !agent.holds_open_turn()
        || agent.is_compacting(now)
        || agent.compaction_unprompted(now)
        || agent
            .soft_compact
            .as_ref()
            .is_some_and(|mark| mark.compactions == Some(agent.compaction_count))
    {
        return None;
    }
    Some(SoftCompactStep::WrapUp)
}

pub fn spawn_soft_compact(runtime: &RuntimePaths, request: &SoftCompactRequest) -> bool {
    let args = crate::child_process::agent_helper_argv("soft-compact", request);
    if let Err(error) =
        crate::child_process::spawn_detached_rimz(runtime, args, "agent-soft-compact")
    {
        tracing::debug!(
            workspace = %runtime.workspace_id,
            tags.operation = "soft_compact.spawn",
            %error,
            "sidebar: failed to spawn soft-compaction helper",
        );
        return false;
    }
    true
}

#[cfg(test)]
mod tests;
