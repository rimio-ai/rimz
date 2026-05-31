//! Event envelope appended to `events.log.jsonl`.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agents::AgentLifecycleObservation;
use crate::feed::FeedItem;
use crate::ids::{EventId, MuxName, WorkspaceId};
use crate::schema::EVENT_SCHEMA_VERSION;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub schema_version: String,
    pub event_id: EventId,
    pub workspace_id: WorkspaceId,
    pub session_name: String,
    pub mux: Option<MuxName>,
    pub source: String,
    pub source_kind: String,
    pub method: String,
    pub timestamp: Timestamp,
    pub params: Value,
}

impl EventEnvelope {
    pub fn new(
        workspace_id: WorkspaceId,
        session_name: impl Into<String>,
        source: impl Into<String>,
        source_kind: impl Into<String>,
        method: impl Into<String>,
        params: Value,
    ) -> Self {
        Self {
            schema_version: EVENT_SCHEMA_VERSION.to_owned(),
            event_id: EventId::new(),
            workspace_id,
            session_name: session_name.into(),
            mux: None,
            source: source.into(),
            source_kind: source_kind.into(),
            method: method.into(),
            timestamp: Timestamp::now(),
            params,
        }
    }

    /// Convenience constructor for a `feed.push` event from a `FeedItem`.
    pub fn feed_pushed(item: &FeedItem, session_name: impl Into<String>) -> Self {
        Self::new(
            item.workspace_id.clone(),
            session_name,
            item.source.clone(),
            item.source_kind.clone(),
            "feed.push",
            json!({
                "request_id": item.request_id,
                "surface": item.surface,
                "kind": item.kind,
            }),
        )
    }

    /// Convenience constructor for an `agent.lifecycle` event. The CLI hook
    /// path calls this after a lifecycle hook fires, so the sidebar's agent
    /// rollup sees the status, permission posture, and enrichment update
    /// without each adapter touching the ledger.
    pub fn agent_lifecycle(
        workspace_id: WorkspaceId,
        session_name: impl Into<String>,
        agent_kind: impl Into<String>,
        event_name: impl Into<String>,
        observation: &AgentLifecycleObservation,
    ) -> Self {
        let agent_kind = agent_kind.into();
        let event_name = event_name.into();
        let params = json!({
            "event_name": event_name,
            "agent_id": observation.agent_id,
            "status": observation.status,
            "permission_posture": observation.permission_posture,
            "agent_pid": observation.agent_pid,
            "agent_process_start": observation.agent_process_start,
            "runtime_owner": observation.runtime_owner,
            "worktree_path": observation.worktree_path,
            "worktree_branch": observation.worktree_branch,
            "task": observation.task,
            "prompt": observation.prompt,
            "model": observation.model,
            "effort": observation.effort,
            "context_pct": observation.context_pct,
            "total_tokens": observation.total_tokens,
            "todo_done": observation.todo_done,
            "todo_total": observation.todo_total,
            "pane_id": observation.pane_id.as_ref().map(|id| id.as_str()),
            "parent_agent_id": observation.parent_agent_id,
        });
        Self::new(
            workspace_id,
            session_name,
            agent_kind.clone(),
            "agent-hook",
            "agent.lifecycle",
            params,
        )
    }
}
