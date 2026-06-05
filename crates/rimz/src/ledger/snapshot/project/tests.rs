use std::path::Path;

use super::*;

use super::super::view::{attach_sub_agents, row_from_agent, sub_agent_from_state};
use crate::agent_activity::AgentActivity;
use crate::agents::lifecycle::TurnPhase;
use crate::feed::AgentStatus;
use crate::ids::WorkspaceId;
use crate::ledger::snapshot::SidebarSnapshot;
use crate::ledger::snapshot::testkit::*;
use jiff::Timestamp;

#[test]
fn thinking_phase_follows_the_turn_through_the_reducer() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let lifecycle = |params: serde_json::Value| {
        EventEnvelope::new(
            workspace.clone(),
            "session",
            "claude",
            "agent-hook",
            "agent.lifecycle",
            params,
        )
    };
    // A legacy `permission_posture` param rides along unread — replay of an
    // old log never errors on it.
    let start = lifecycle(serde_json::json!({
        "event_name": "SessionStart",
        "agent_id": "sess-1",
        "signal": { "signal": "registered" },
        "permission_posture": "plan",
    }));
    let prompt = lifecycle(serde_json::json!({
        "event_name": "UserPromptSubmit",
        "agent_id": "sess-1",
        "signal": { "signal": "turn_started" },
    }));
    let running = reduce_agent_states(&[start.clone(), prompt.clone()]);
    assert_eq!(running[0].status, AgentStatus::Running);
    assert_eq!(
        running[0].phase,
        TurnPhase::Reasoning,
        "a fresh turn opens reasoning"
    );

    // A mutating-but-not-editing tool (a shell command) keeps the head.
    let shell = lifecycle(serde_json::json!({
        "event_name": "PostToolUse",
        "agent_id": "sess-1",
        "signal": { "signal": "tool_used", "mutates": true, "edits": false },
    }));
    let still = reduce_agent_states(&[start.clone(), prompt.clone(), shell.clone()]);
    assert_eq!(
        still[0].phase,
        TurnPhase::Reasoning,
        "a shell command is not a file edit"
    );

    // The turn's first file edit flips it to working.
    let edit = lifecycle(serde_json::json!({
        "event_name": "PostToolUse",
        "agent_id": "sess-1",
        "signal": { "signal": "tool_used", "mutates": true, "edits": true },
    }));
    let working = reduce_agent_states(&[start.clone(), prompt.clone(), shell, edit]);
    assert_eq!(working[0].status, AgentStatus::Running);
    assert_eq!(working[0].phase, TurnPhase::Acting);

    // The turn end clears the head regardless.
    let stop = lifecycle(serde_json::json!({
        "event_name": "Stop",
        "agent_id": "sess-1",
        "signal": { "signal": "turn_ended", "errored": false, "parked_on_background": false },
    }));
    let stopped = reduce_agent_states(&[start, prompt, stop]);
    assert_eq!(stopped[0].status, AgentStatus::Success);
    assert_eq!(stopped[0].phase, TurnPhase::Idle);
}

#[test]
fn subagent_activity_does_not_change_parent_phase() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let last_seen = Timestamp::now() - std::time::Duration::from_secs(50);
    let mut parent = agent("claude", "sess-1", AgentStatus::Running, 50_000);
    parent.phase = TurnPhase::Reasoning;
    let subagent = agent("claude", "sess-1.sub", AgentStatus::Running, 50_000);

    let subagent_touch = AgentActivity {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: "sess-1.sub".into(),
        at: last_seen + std::time::Duration::from_secs(15),
    };
    let snap = SidebarSnapshot::build_with_agents(
        workspace,
        Vec::new(),
        vec![parent, subagent],
        Timestamp::now(),
    )
    .with_agent_activity(&[subagent_touch]);
    let parent_phase = snap
        .agents
        .iter()
        .find(|a| a.agent_id == "sess-1")
        .unwrap()
        .phase;
    assert_eq!(
        parent_phase,
        TurnPhase::Reasoning,
        "a subagent heartbeat must not clobber the parent's turn phase"
    );
}

#[test]
fn lifecycle_carries_capability_forward_when_event_omits_it() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let lifecycle = |params: serde_json::Value| {
        EventEnvelope::new(
            workspace.clone(),
            "session",
            "codex",
            "agent-hook",
            "agent.lifecycle",
            params,
        )
    };
    // SessionStart establishes the capability line.
    let start = lifecycle(serde_json::json!({
        "event_name": "SessionStart",
        "agent_id": "sess-1",
        "signal": { "signal": "registered" },
        "model": "GPT-5.5",
        "effort": "high",
        "context_window": 258_400,
        "worktree_branch": "main",
    }));
    // A prompt-submit moves the agent to running but reports no model.
    let prompt = lifecycle(serde_json::json!({
        "event_name": "UserPromptSubmit",
        "agent_id": "sess-1",
        "signal": { "signal": "turn_started" },
        "task": "fix auth flow",
        "worktree_path": "/tmp/hook-subprocess-cwd",
        "worktree_branch": "wrong-branch",
    }));

    let agents = reduce_agent_states(&[start, prompt]);
    assert_eq!(agents.len(), 1);
    let agent = &agents[0];
    assert_eq!(agent.status, AgentStatus::Running);
    assert_eq!(agent.task.as_deref(), Some("fix auth flow"));
    // Capability survives the prompt.
    assert_eq!(agent.model.as_deref(), Some("GPT-5.5"));
    assert_eq!(agent.effort.as_deref(), Some("high"));
    assert_eq!(agent.context_window, Some(258_400));
    assert_eq!(agent.worktree_branch.as_deref(), Some("main"));
}

#[test]
fn canonical_model_strips_capability_tag() {
    assert_eq!(canonical_model("claude-opus-4-8[1m]"), "claude-opus-4-8");
    // Idempotent on a bare id.
    assert_eq!(canonical_model("claude-opus-4-8"), "claude-opus-4-8");
    assert_eq!(canonical_model("gpt-5.5"), "gpt-5.5");
}

#[test]
fn model_label_holds_canonical_across_suffix_drop() {
    // The live flip: SessionStart reports the suffixed id, the prompt omits
    // model entirely, and the first Stop falls back to the transcript's
    // bare id. Canonicalizing at reduce time keeps the label stable so the
    // `[1m]` tag never appears and then vanishes.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let lifecycle = |params: serde_json::Value| {
        EventEnvelope::new(
            workspace.clone(),
            "session",
            "claude",
            "agent-hook",
            "agent.lifecycle",
            params,
        )
    };
    let start = lifecycle(serde_json::json!({
        "event_name": "SessionStart",
        "agent_id": "sess-1",
        "signal": { "signal": "registered" },
        "model": "claude-opus-4-8[1m]",
    }));
    let prompt = lifecycle(serde_json::json!({
        "event_name": "UserPromptSubmit",
        "agent_id": "sess-1",
        "signal": { "signal": "turn_started" },
    }));
    let stop = lifecycle(serde_json::json!({
        "event_name": "Stop",
        "agent_id": "sess-1",
        "signal": { "signal": "turn_ended", "errored": false, "parked_on_background": false },
        "model": "claude-opus-4-8",
    }));

    let agents = reduce_agent_states(&[start, prompt, stop]);
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].model.as_deref(), Some("claude-opus-4-8"));
}

// ---- Subagent observability (M6): parent link, nesting, retention, reaping ----

#[test]
fn subagent_start_reduces_parent_link_that_survives_stop() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let lifecycle = |params: serde_json::Value| {
        EventEnvelope::new(
            workspace.clone(),
            "session",
            "claude",
            "agent-hook",
            "agent.lifecycle",
            params,
        )
    };
    let start = lifecycle(serde_json::json!({
        "event_name": "SubagentStart",
        "agent_id": "child-1",
        "signal": { "signal": "subagent_started" },
        "parent_agent_id": "sess-root",
        "task": "Explore",
    }));
    // SubagentStop omits the parent link — the reducer carries identity forward.
    let stop = lifecycle(serde_json::json!({
        "event_name": "SubagentStop",
        "agent_id": "child-1",
        "signal": { "signal": "subagent_stopped" },
        "task": "Explore",
    }));
    let agents = reduce_agent_states(&[start, stop]);
    let child = agents
        .iter()
        .find(|a| a.agent_id == "child-1")
        .expect("child row");
    assert_eq!(child.parent_agent_id.as_deref(), Some("sess-root"));
    // The bare `subagent_stopped` wire shape (no `errored` bit) replays as a
    // clean finish.
    assert_eq!(child.status, AgentStatus::Success);
}

#[test]
fn subagent_keeps_its_type_when_stop_omits_it() {
    // The regression: a subagent's type is identity, not activity, so a
    // task-less `SubagentStop` must not wipe the label the `SubagentStart`
    // established. Before the carry-forward, the finished child degraded to
    // a `subagent <id>` placeholder.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let lifecycle = |params: serde_json::Value| {
        EventEnvelope::new(
            workspace.clone(),
            "session",
            "claude",
            "agent-hook",
            "agent.lifecycle",
            params,
        )
    };
    let start = lifecycle(serde_json::json!({
        "event_name": "SubagentStart",
        "agent_id": "child-1",
        "signal": { "signal": "subagent_started" },
        "parent_agent_id": "sess-root",
        "task": "Explore",
    }));
    // SubagentStop carries a blank `task` — the exact shape that wiped the
    // label in live Claude events.
    let stop = lifecycle(serde_json::json!({
        "event_name": "SubagentStop",
        "agent_id": "child-1",
        "signal": { "signal": "subagent_stopped" },
        "task": "",
    }));
    let agents = reduce_agent_states(&[start, stop]);
    let child = agents
        .iter()
        .find(|a| a.agent_id == "child-1")
        .expect("child row");
    assert_eq!(child.status, AgentStatus::Success);
    assert_eq!(
        child.task.as_deref(),
        Some("Explore"),
        "a task-less SubagentStop must not wipe the carried-forward type",
    );
    // The projected sidebar row reads the type, never the hash placeholder.
    let now = Timestamp::from_second(1_700_000_100).unwrap();
    assert_eq!(sub_agent_from_state(child, now).name, "Explore");
}

#[test]
fn subagent_stop_without_start_keeps_parent_link_and_spares_the_parent() {
    // Claude can report a typed child only at `SubagentStop`. That Stop
    // still carries `parent_agent_id`; adopting it keeps the finished child
    // nested instead of letting it supersede the parent as a newer root on
    // the same pane.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let lifecycle = |params: serde_json::Value| {
        EventEnvelope::new(
            workspace.clone(),
            "session",
            "claude",
            "agent-hook",
            "agent.lifecycle",
            params,
        )
    };
    let pane = "tmux:%1";
    let root_start = lifecycle(serde_json::json!({
        "event_name": "SessionStart",
        "agent_id": "sess-root",
        "signal": { "signal": "registered" },
        "model": "claude-opus-4-8",
        "pane_id": pane,
        "worktree_path": "/repo/wt",
        "worktree_branch": "feature",
    }));
    let root_prompt = lifecycle(serde_json::json!({
        "event_name": "UserPromptSubmit",
        "agent_id": "sess-root",
        "signal": { "signal": "turn_started" },
        "pane_id": pane,
        "worktree_path": "/repo/wt",
        "worktree_branch": "feature",
    }));
    let child_stop = lifecycle(serde_json::json!({
        "event_name": "SubagentStop",
        "agent_id": "child-1",
        "signal": { "signal": "subagent_stopped" },
        "parent_agent_id": "sess-root",
        "task": "Explore",
        "pane_id": pane,
        "worktree_path": "/repo/wt",
        "worktree_branch": "feature",
    }));

    let agents = reduce_agent_states(&[root_start, root_prompt, child_stop]);
    let child = agents
        .iter()
        .find(|a| a.agent_id == "child-1")
        .expect("child row");
    assert_eq!(child.parent_agent_id.as_deref(), Some("sess-root"));

    let mut snapshot = SidebarSnapshot::build_with_carryover(
        workspace,
        Vec::new(),
        Vec::new(),
        agents,
        Timestamp::now(),
    );
    snapshot.reap_stale_sessions();
    assert!(
        snapshot.agents.iter().any(|a| a.agent_id == "sess-root"),
        "a Stop-only child must not reap its live parent",
    );
}

#[test]
fn typeless_subagent_stop_without_start_is_ignored() {
    // Claude can also emit extra SubagentStop hooks for task ids that never
    // had a SubagentStart and carry an empty task label. Those are not useful
    // child identity; reducing them used to mint `subagent <hash>` entries in
    // the parent's expanded card.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let lifecycle = |params: serde_json::Value| {
        EventEnvelope::new(
            workspace.clone(),
            "session",
            "claude",
            "agent-hook",
            "agent.lifecycle",
            params,
        )
    };
    let root_start = lifecycle(serde_json::json!({
        "event_name": "SessionStart",
        "agent_id": "sess-root",
        "signal": { "signal": "registered" },
    }));
    let root_prompt = lifecycle(serde_json::json!({
        "event_name": "UserPromptSubmit",
        "agent_id": "sess-root",
        "signal": { "signal": "turn_started" },
    }));
    let child_start = lifecycle(serde_json::json!({
        "event_name": "SubagentStart",
        "agent_id": "child-real",
        "signal": { "signal": "subagent_started" },
        "parent_agent_id": "sess-root",
        "task": "Explore",
    }));
    let stray_stop = lifecycle(serde_json::json!({
        "event_name": "SubagentStop",
        "agent_id": "a833a787ad884cee2",
        "signal": { "signal": "subagent_stopped" },
        "parent_agent_id": "sess-root",
        "task": "",
        "total_tokens": 36_410,
    }));

    let agents = reduce_agent_states(&[root_start, root_prompt, child_start, stray_stop]);
    assert!(
        agents.iter().all(|a| a.agent_id != "a833a787ad884cee2"),
        "an unknown blank-label stop must not become a child row",
    );
    let mut rows = vec![row_from_agent(
        agents
            .iter()
            .find(|a| a.agent_id == "sess-root")
            .expect("root row"),
        Timestamp::now(),
    )];
    attach_sub_agents(&mut rows, &agents, Timestamp::now());
    assert_eq!(rows[0].sub_agents.len(), 1);
    assert_eq!(rows[0].sub_agents[0].id, "child-real");
    assert_eq!(rows[0].sub_agents[0].name, "Explore");
}

#[test]
fn turn_started_tracks_prompt_never_stop() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let lifecycle = |params: serde_json::Value| {
        EventEnvelope::new(
            workspace.clone(),
            "session",
            "claude",
            "agent-hook",
            "agent.lifecycle",
            params,
        )
    };
    let start = lifecycle(
        serde_json::json!({ "event_name": "SessionStart", "agent_id": "s1", "signal": { "signal": "registered" } }),
    );
    let prompt = lifecycle(
        serde_json::json!({ "event_name": "UserPromptSubmit", "agent_id": "s1", "signal": { "signal": "turn_started" } }),
    );
    let prompt_ts = prompt.timestamp;
    let stop = lifecycle(
        serde_json::json!({ "event_name": "Stop", "agent_id": "s1", "signal": { "signal": "turn_ended", "errored": false, "parked_on_background": false } }),
    );
    let agents = reduce_agent_states(&[start, prompt, stop]);
    // The boundary is the prompt; the later Stop must not advance it (that is
    // what keeps a finished child visible until the *next* prompt).
    assert_eq!(agents[0].turn_started_at, Some(prompt_ts));
}

#[test]
fn prompt_persists_past_stop_while_task_clears() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let lifecycle = |params: serde_json::Value| {
        EventEnvelope::new(
            workspace.clone(),
            "session",
            "claude",
            "agent-hook",
            "agent.lifecycle",
            params,
        )
    };
    let prompt = lifecycle(serde_json::json!({
        "event_name": "UserPromptSubmit",
        "agent_id": "s1",
        "signal": { "signal": "turn_started" },
        "task": "fix auth flow",
        "prompt": "fix auth flow",
    }));
    // Stop carries neither task nor prompt: task is activity-bound and clears,
    // but the prompt persists to label the unnamed session past its turn.
    let stop = lifecycle(
        serde_json::json!({ "event_name": "Stop", "agent_id": "s1", "signal": { "signal": "turn_ended", "errored": false, "parked_on_background": false } }),
    );
    let agents = reduce_agent_states(&[prompt, stop]);
    let agent = agents.iter().find(|a| a.agent_id == "s1").expect("agent");
    assert_eq!(agent.task, None, "the task clears on idle");
    assert_eq!(
        agent.prompt.as_deref(),
        Some("fix auth flow"),
        "the latest prompt persists past the Stop"
    );
}

#[test]
fn lifecycle_carries_enrichment_forward() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let lifecycle = |params: serde_json::Value| {
        EventEnvelope::new(
            workspace.clone(),
            "session",
            "claude",
            "agent-hook",
            "agent.lifecycle",
            params,
        )
    };
    let start = lifecycle(serde_json::json!({
        "event_name": "SessionStart",
        "agent_id": "sess-1",
        "signal": { "signal": "registered" },
        "context_pct": 38,
        "total_tokens": 12_400,
        "todo_done": 3,
        "todo_total": 5,
    }));
    let prompt = lifecycle(serde_json::json!({
        "event_name": "UserPromptSubmit",
        "agent_id": "sess-1",
        "signal": { "signal": "turn_started" },
        "task": "fix auth flow",
    }));

    let agents = reduce_agent_states(&[start, prompt]);
    assert_eq!(agents.len(), 1);
    let agent = &agents[0];
    assert_eq!(agent.context_pct, Some(38));
    assert_eq!(agent.total_tokens, Some(12_400));
    assert_eq!(agent.todo_done, Some(3));
    assert_eq!(agent.todo_total, Some(5));
    assert_eq!(agent.task.as_deref(), Some("fix auth flow"));
}

#[test]
fn session_less_lifecycle_events_are_quarantined_not_merged() {
    // Identity is required: an event without an agent_id folds to nothing
    // (with a log) rather than collapsing into a shared per-kind bucket
    // where two distinct session-less instances would merge into one row.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let event = EventEnvelope::new(
        workspace,
        "session",
        "claude",
        "agent-hook",
        "agent.lifecycle",
        serde_json::json!({
            "event_name": "SessionStart",
            "signal": { "signal": "registered" },
        }),
    );
    assert!(
        reduce_agent_states(&[event]).is_empty(),
        "a session-less event produces no rollup entry"
    );
}

#[test]
fn lifecycle_reduces_pane_id_and_carries_it_forward() {
    // The hook stamps the mux pane id on every lifecycle event so the
    // reducer can bind each agent to its own pane. A later event that omits
    // pane_id must not unbind the agent.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let lifecycle = |params: serde_json::Value| {
        EventEnvelope::new(
            workspace.clone(),
            "session",
            "claude",
            "agent-hook",
            "agent.lifecycle",
            params,
        )
    };
    let start = lifecycle(serde_json::json!({
        "event_name": "SessionStart",
        "agent_id": "sess-1",
        "signal": { "signal": "registered" },
        "pane_id": "tmux:%7",
    }));
    let prompt = lifecycle(serde_json::json!({
        "event_name": "UserPromptSubmit",
        "agent_id": "sess-1",
        "signal": { "signal": "turn_started" },
    }));

    let agents = reduce_agent_states(&[start, prompt]);
    assert_eq!(agents.len(), 1);
    let bound = agents[0].pane.as_ref().expect("pane carries forward");
    assert_eq!(bound.pane_id.raw(), "%7");
}
