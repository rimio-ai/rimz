//! Agent hook-payload fixtures.

use rimz::EventEnvelope;
use rimz::agents::lifecycle::LifecycleSignal;
use rimz::agents::{AgentLifecycleObservation, LaunchParams};
use rimz::ids::AgentSessionId;
use serde_json::json;

use super::harness::Harness;

/// One `agent.lifecycle` event envelope, registered-signal shaped — the
/// fixture every store suite seeds agents with. One builder so the wire
/// shape lives in one place; a shape change lands here, not per suite.
pub fn lifecycle_event(
    h: &Harness,
    session: &str,
    event_name: &str,
    agent_id: &str,
) -> EventEnvelope {
    EventEnvelope::agent_lifecycle(
        h.workspace_id.clone(),
        session,
        "claude",
        event_name,
        &registered_observation(agent_id),
    )
}

fn registered_observation(agent_id: &str) -> AgentLifecycleObservation {
    AgentLifecycleObservation {
        agent_id: Some(AgentSessionId::from(agent_id)),
        agent_name: None,
        launch: LaunchParams::default(),
        signal: LifecycleSignal::Registered,
        agent_pid: None,
        agent_process_start: None,
        runtime_owner: None,
        worktree_path: None,
        worktree_branch: None,
        task: None,
        prompt: None,
        transcript_path: None,
        origin: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        turn_error: None,
        cache_read_input_tokens: None,
        cache_write_input_tokens: None,
        fresh_input_tokens: None,
        output_tokens: None,
        pane_id: None,
        pane_stamp: None,
        parent_agent_id: None,
    }
}

/// Claude-shaped `PermissionRequest` hook payload for `tool_name`.
pub fn permission_payload(tool_name: &str) -> String {
    serde_json::to_string(&json!({
        "hook_event_name": "PermissionRequest",
        "session_id": "sess-claude-permission",
        "tool_name": tool_name,
        "tool_input": { "command": "echo hi" },
    }))
    .expect("payload")
}

/// Codex-shaped `PermissionRequest` payload (shell command vector, no
/// Claude-only fields).
pub fn codex_permission_payload() -> String {
    serde_json::to_string(&json!({
        "hook_event_name": "PermissionRequest",
        "session_id": "sess-codex-permission",
        "tool_name": "shell",
        "command": ["echo", "hi"],
    }))
    .expect("payload")
}

/// Claude `PreToolUse` blocking-hook payload (`ExitPlanMode`,
/// `AskUserQuestion`).
pub fn claude_pre_tool_use_payload(tool_name: &str) -> String {
    serde_json::to_string(&json!({
        "hook_event_name": "PreToolUse",
        "tool_name": tool_name,
        "tool_input": { "plan": "ship it" },
        "session_id": "sess-claude-pretool",
    }))
    .expect("payload")
}

/// Codex `PreToolUse` blocking question payload (`request_user_input`).
pub fn codex_pre_tool_use_payload() -> String {
    serde_json::to_string(&json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "request_user_input",
        "tool_input": {
            "questions": [{ "question": "which fix shape?" }]
        },
        "session_id": "sess-codex-pretool",
    }))
    .expect("payload")
}

/// Pi-shaped blocking `tool_call` payload. Rimz authors pi's wire (the
/// extension is Rimz code), so this mirrors `extension.ts`'s envelope —
/// lowercase pi tool names (`bash`, `read`, `edit`, …).
pub fn pi_tool_call_payload(tool_name: &str) -> String {
    serde_json::to_string(&json!({
        "hook_event_name": "tool_call",
        "session_id": "sess-pi-tool",
        "tool_name": tool_name,
        "tool_input": { "command": "echo hi" },
    }))
    .expect("payload")
}
