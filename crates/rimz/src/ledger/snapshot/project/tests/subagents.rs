use super::*;

#[test]
fn subagent_start_reduces_identity_that_survives_stop() {
    let start = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "SubagentStart",
            "agent_id": "child-1",
            "signal": { "signal": "subagent_started" },
            "parent_agent_id": "sess-root",
            "task": "Explore",
        }),
    );
    // SubagentStop omits the parent link and carries a blank task — the
    // reducer carries identity forward instead of degrading the child label.
    let stop = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "SubagentStop",
            "agent_id": "child-1",
            "signal": { "signal": "subagent_stopped" },
            "task": "",
        }),
    );
    let agents = reduce_agent_states(&[start, stop]);
    let child = agents
        .iter()
        .find(|a| a.agent_id == "child-1")
        .expect("child row");
    assert_eq!(child.parent_agent_id.as_deref(), Some("sess-root"));
    // The bare `subagent_stopped` wire shape (no `errored` bit) replays as a
    // clean finish.
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
    let pane = "tmux:%1";
    let root_start = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-root",
            "signal": { "signal": "registered" },
            "model": "claude-opus-4-8",
            "pane_id": pane,
            "worktree_path": "/repo/wt",
            "worktree_branch": "feature",
        }),
    );
    let root_prompt = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-root",
            "signal": { "signal": "turn_started" },
            "pane_id": pane,
            "worktree_path": "/repo/wt",
            "worktree_branch": "feature",
        }),
    );
    let child_stop = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "SubagentStop",
            "agent_id": "child-1",
            "signal": { "signal": "subagent_stopped" },
            "parent_agent_id": "sess-root",
            "task": "Explore",
            "pane_id": pane,
            "worktree_path": "/repo/wt",
            "worktree_branch": "feature",
        }),
    );

    let agents = reduce_agent_states(&[root_start, root_prompt, child_stop]);
    let child = agents
        .iter()
        .find(|a| a.agent_id == "child-1")
        .expect("child row");
    assert_eq!(child.parent_agent_id.as_deref(), Some("sess-root"));

    let mut snapshot = SidebarSnapshot::build_with_carryover(
        workspace(),
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
    let root_start = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-root",
            "signal": { "signal": "registered" },
        }),
    );
    let root_prompt = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-root",
            "signal": { "signal": "turn_started" },
        }),
    );
    let child_start = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "SubagentStart",
            "agent_id": "child-real",
            "signal": { "signal": "subagent_started" },
            "parent_agent_id": "sess-root",
            "task": "Explore",
        }),
    );
    let stray_stop = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "SubagentStop",
            "agent_id": "a833a787ad884cee2",
            "signal": { "signal": "subagent_stopped" },
            "parent_agent_id": "sess-root",
            "task": "",
            "total_tokens": 36_410,
        }),
    );

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
    assert_eq!(rows[0].sub_agents().len(), 1);
    assert_eq!(rows[0].sub_agents()[0].id, "child-real");
    assert_eq!(rows[0].sub_agents()[0].name, "Explore");
}

#[test]
fn finished_subagent_verdict_survives_the_parked_wake() {
    let root_start = raw_lifecycle_at(
        "claude",
        0,
        serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-root",
            "signal": { "signal": "registered" },
        }),
    );
    let root_prompt = raw_lifecycle_at(
        "claude",
        10,
        serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-root",
            "signal": { "signal": "turn_started" },
        }),
    );
    let parent_turn_started_at = root_prompt.timestamp;
    let child_start = raw_lifecycle_at(
        "claude",
        20,
        serde_json::json!({
            "event_name": "SubagentStart",
            "agent_id": "child-1",
            "signal": { "signal": "subagent_started" },
            "parent_agent_id": "sess-root",
            "task": "Explore",
        }),
    );
    let root_park = raw_lifecycle_at(
        "claude",
        30,
        serde_json::json!({
            "event_name": "Stop",
            "agent_id": "sess-root",
            "signal": { "signal": "turn_ended", "errored": false, "parked_on_background": true },
        }),
    );
    let child_stop = raw_lifecycle_at(
        "claude",
        100,
        serde_json::json!({
            "event_name": "SubagentStop",
            "agent_id": "child-1",
            "signal": { "signal": "subagent_stopped", "errored": false },
            "task": "Explore",
        }),
    );
    let root_wake = raw_lifecycle_at(
        "claude",
        101,
        serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-root",
            "signal": { "signal": "turn_started" },
        }),
    );

    let agents = reduce_agent_states(&[
        root_start,
        root_prompt,
        child_start,
        root_park,
        child_stop,
        root_wake,
    ]);
    let parent = agents
        .iter()
        .find(|a| a.agent_id == "sess-root")
        .expect("root row");
    assert_eq!(parent.turn_started_at, Some(parent_turn_started_at));

    let now = Timestamp::from_second(epoch().as_second() + 120).unwrap();
    let mut rows = vec![row_from_agent(parent, now)];
    attach_sub_agents(&mut rows, &agents, now);

    assert_eq!(rows[0].sub_agents().len(), 1);
    assert_eq!(rows[0].sub_agents()[0].id, "child-1");
    assert_eq!(rows[0].sub_agents()[0].name, "Explore");
    assert_eq!(rows[0].sub_agents()[0].status, AgentStatus::Success);
}
