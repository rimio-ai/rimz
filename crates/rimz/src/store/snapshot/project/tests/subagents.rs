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
fn terminal_subagent_stop_absorbs_a_reordered_start() {
    let stop = raw_lifecycle_at(
        "pi",
        1,
        serde_json::json!({
            "event_name": "subagent_stopped",
            "agent_id": "child-1",
            "signal": { "signal": "subagent_stopped", "errored": true },
            "parent_agent_id": "sess-root",
            "task": "reviewer",
        }),
    );
    let late_start = raw_lifecycle_at(
        "pi",
        2,
        serde_json::json!({
            "event_name": "subagent_started",
            "agent_id": "child-1",
            "signal": { "signal": "subagent_started" },
            "parent_agent_id": "sess-root",
            "task": "reviewer",
        }),
    );

    let agents = reduce_agent_states(&[stop, late_start]);
    let child = agents
        .iter()
        .find(|agent| agent.agent_id == "child-1")
        .expect("child row");
    assert_eq!(child.status, AgentStatus::Failed);
    assert_eq!(child.task.as_deref(), Some("reviewer"));
    assert_eq!(child.parent_agent_id.as_deref(), Some("sess-root"));
    assert_eq!(child.turn_started_at, None);
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

    let mut snapshot =
        SidebarSnapshot::build_with_carryover(workspace(), Vec::new(), agents, Timestamp::now());
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

#[test]
fn context_reset_retires_prior_turn_subagents() {
    // A child finishes inside the parent's turn, then the parent runs a context
    // reset. Each reset advances `turn_started_at` past the child's activity, so
    // the finished verdict drops from the expanded card — the user-typed
    // `/compact` and `/clear` behave like the automatic compaction, which
    // already opens a turn.
    let history = |resets: Vec<EventEnvelope>| {
        let mut events = vec![
            raw_lifecycle_at(
                "claude",
                0,
                serde_json::json!({
                    "event_name": "SessionStart",
                    "agent_id": "sess-root",
                    "signal": { "signal": "registered" },
                }),
            ),
            raw_lifecycle_at(
                "claude",
                10,
                serde_json::json!({
                    "event_name": "UserPromptSubmit",
                    "agent_id": "sess-root",
                    "signal": { "signal": "turn_started" },
                }),
            ),
            raw_lifecycle_at(
                "claude",
                20,
                serde_json::json!({
                    "event_name": "SubagentStart",
                    "agent_id": "child-1",
                    "signal": { "signal": "subagent_started" },
                    "parent_agent_id": "sess-root",
                    "task": "Explore",
                }),
            ),
            raw_lifecycle_at(
                "claude",
                30,
                serde_json::json!({
                    "event_name": "SubagentStop",
                    "agent_id": "child-1",
                    "signal": { "signal": "subagent_stopped", "errored": false },
                    "task": "Explore",
                }),
            ),
        ];
        events.extend(resets);
        reduce_agent_states(&events)
    };

    // `/compact`: PreCompact opens the bracket, PostCompact (manual) closes it.
    let manual_compact = vec![
        raw_lifecycle_at(
            "claude",
            35,
            serde_json::json!({
                "event_name": "PreCompact",
                "agent_id": "sess-root",
                "signal": { "signal": "compacting" },
            }),
        ),
        raw_lifecycle_at(
            "claude",
            40,
            serde_json::json!({
                "event_name": "PostCompact",
                "agent_id": "sess-root",
                "signal": { "signal": "compaction_ended", "auto": false },
            }),
        ),
    ];
    // `/clear`: a fresh `SessionStart` carrying the `registered` signal.
    let clear = vec![raw_lifecycle_at(
        "claude",
        40,
        serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-root",
            "signal": { "signal": "registered" },
        }),
    )];
    // Automatic compaction *mid-turn* resumes the same turn (the parent never
    // ended it), so the boundary holds and the finished child stays listed.
    let auto_compact = vec![
        raw_lifecycle_at(
            "claude",
            35,
            serde_json::json!({
                "event_name": "PreCompact",
                "agent_id": "sess-root",
                "signal": { "signal": "compacting" },
            }),
        ),
        raw_lifecycle_at(
            "claude",
            40,
            serde_json::json!({
                "event_name": "PostCompact",
                "agent_id": "sess-root",
                "signal": { "signal": "compaction_ended", "auto": true },
            }),
        ),
    ];

    let reset_at = Timestamp::from_second(epoch().as_second() + 40).unwrap();
    let turn_at = Timestamp::from_second(epoch().as_second() + 10).unwrap();
    for (label, resets, flushes) in [
        ("/compact", manual_compact, true),
        ("/clear", clear, true),
        ("auto compaction mid-turn", auto_compact, false),
    ] {
        let agents = history(resets);
        let parent = agents
            .iter()
            .find(|a| a.agent_id == "sess-root")
            .expect("root row");
        let now = Timestamp::from_second(epoch().as_second() + 50).unwrap();
        let mut rows = vec![row_from_agent(parent, now)];
        attach_sub_agents(&mut rows, &agents, now);

        if flushes {
            assert_eq!(
                parent.turn_started_at,
                Some(reset_at),
                "{label}: the reset advances the subagent boundary",
            );
            assert!(
                rows[0].sub_agents().is_empty(),
                "{label}: the prior turn's finished child is flushed",
            );
        } else {
            assert_eq!(
                parent.turn_started_at,
                Some(turn_at),
                "{label}: the resumed turn keeps its boundary",
            );
            assert_eq!(
                rows[0].sub_agents().len(),
                1,
                "{label}: the in-flight turn's child stays listed",
            );
        }
    }
}
