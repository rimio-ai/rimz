//! Shared fixtures for the snapshot unit tests: canned agents, panes, and
//! lifecycle events the per-module test mods build scenarios from.

use jiff::Timestamp;

use crate::agents::lifecycle::{self, TurnPhase};
use crate::feed::{AgentState, AgentStatus, PaneRef};
use crate::ids::{MuxName, PaneId, WorkspaceId};
use crate::schema::event::EventEnvelope;

pub(super) fn agent(kind: &str, id: &str, status: AgentStatus, last_seen: i64) -> AgentState {
    // The `last_seen` arg is a recency rank, not an absolute epoch: anchor it
    // to recent wall-clock (larger rank = more recent, all within ~100s of
    // now) so a `running` test agent is never falsely flagged stalled by the
    // real-time stall window. Tests that exercise the stall/ghost windows
    // override `last_activity` explicitly after construction.
    let offset_ms = (100_000 - last_seen).max(0) as u64;
    let timestamp = Timestamp::now() - std::time::Duration::from_millis(offset_ms);
    AgentState {
        agent_id: id.into(),
        kind: kind.into(),
        status,
        phase: TurnPhase::Idle,
        pane: None,
        agent_pid: None,
        agent_process_start: None,
        runtime_owner: None,
        parent_agent_id: None,
        worktree_path: None,
        worktree_branch: None,
        task: None,
        prompt: None,
        model: None,
        effort: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        todo_done: None,
        todo_total: None,
        context: None,
        subagent_description: None,
        subagent_started_at: None,
        turn_started_at: None,
        compacting_since: None,
        last_seen: timestamp,
        last_activity: timestamp,
    }
}

pub(super) fn pane(raw: &str, command: &str, cwd: &str) -> PaneRef {
    PaneRef {
        pane_id: PaneId::from_parts(MuxName::Tmux, raw),
        session_name: "rimz-test".to_owned(),
        view_id: Some("@0".to_owned()),
        view_kind: Some(crate::ids::ViewKind::Window),
        view_name: None,
        is_focused: false,
        command: Some(command.to_owned()),
        cwd: Some(cwd.to_owned()),
        pane_pid: None,
        pane_process_start: None,
        rss_kb: None,
        cpu_pct: None,
        io_bps: None,
    }
}

pub(super) fn pane_started(raw: &str, cwd: &str, start: Timestamp) -> PaneRef {
    PaneRef {
        pane_process_start: Some(start),
        ..pane(raw, "claude", cwd)
    }
}

pub(super) fn agent_in(id: &str, path: &str, status: AgentStatus, rank: i64) -> AgentState {
    let mut agent = agent("claude", id, status, rank);
    agent.worktree_path = Some(path.to_owned());
    agent
}

pub(super) fn lifecycle_at(
    workspace: &WorkspaceId,
    source: &str,
    event_name: &str,
    agent_id: &str,
    signal: lifecycle::LifecycleSignal,
) -> EventEnvelope {
    EventEnvelope::new(
        workspace.clone(),
        "session",
        source,
        "agent-hook",
        "agent.lifecycle",
        serde_json::json!({
            "event_name": event_name,
            "agent_id": agent_id,
            "signal": signal,
        }),
    )
}

pub(super) fn sorted_value(mut agents: Vec<AgentState>) -> serde_json::Value {
    agents.sort_by_key(|a| (a.kind.clone(), a.agent_id.clone()));
    serde_json::to_value(agents).unwrap()
}

/// A paneless child `AgentState` of `parent`, stamped `secs_ago` before now.
pub(super) fn child_state(
    parent: &str,
    id: &str,
    status: AgentStatus,
    secs_ago: i64,
) -> AgentState {
    let now = Timestamp::now();
    let mut child = agent("claude", id, status, 0);
    child.parent_agent_id = Some(parent.to_owned());
    child.task = Some("Explore".to_owned());
    let at = Timestamp::from_second(now.as_second() - secs_ago).unwrap();
    child.last_activity = at;
    child.last_seen = at;
    child
}
