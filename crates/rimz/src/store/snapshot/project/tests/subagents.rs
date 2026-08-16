use super::*;

#[test]
fn answered_parent_ask_keeps_children_from_both_sides_of_the_tool_completion() {
    let event = |at: i64, agent_id: &str, params: serde_json::Value| {
        let mut params = params;
        params["agent_id"] = serde_json::Value::String(agent_id.to_owned());
        raw_lifecycle_at("qwen", at, params)
    };
    let child = |at: i64, id: &str, signal: serde_json::Value| {
        event(
            at,
            id,
            serde_json::json!({
                "event_name": "Subagent",
                "parent_agent_id": "parent",
                "task": "general-purpose",
                "signal": signal,
            }),
        )
    };
    let events = [
        event(
            1,
            "parent",
            serde_json::json!({
                "event_name": "UserPromptSubmit",
                "signal": { "signal": "turn_started" },
            }),
        ),
        event(
            2,
            "parent",
            serde_json::json!({
                "event_name": "PermissionRequest",
                "signal": { "signal": "awaiting_input", "kind": "permission" },
            }),
        ),
        child(
            3,
            "child-a",
            serde_json::json!({ "signal": "subagent_started" }),
        ),
        child(
            4,
            "child-a",
            serde_json::json!({ "signal": "subagent_stopped", "errored": false }),
        ),
        event(
            5,
            "parent",
            serde_json::json!({
                "event_name": "PostToolUse",
                "signal": { "signal": "tool_used", "mutates": false, "edits": false },
            }),
        ),
        child(
            6,
            "child-b",
            serde_json::json!({ "signal": "subagent_started" }),
        ),
        child(
            7,
            "child-b",
            serde_json::json!({ "signal": "subagent_stopped", "errored": false }),
        ),
        event(
            8,
            "parent",
            serde_json::json!({
                "event_name": "Stop",
                "signal": { "signal": "turn_ended", "errored": false, "parked_on_background": false },
            }),
        ),
    ];

    let agents = reduce_agent_states(&events);
    let parent = agents
        .iter()
        .find(|agent| agent.agent_id == "parent")
        .expect("parent");
    assert_eq!(
        parent.turn_started_at,
        Some(Timestamp::from_second(epoch().as_second() + 1).unwrap())
    );
    let snapshot = room_with_agent_panes(agents);
    let child_ids = row(&snapshot, "parent")
        .sub_agents()
        .iter()
        .map(|child| child.id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(child_ids, BTreeSet::from(["child-a", "child-b"]));
}

#[test]
fn same_type_children_keep_repeated_labels_and_exact_ids_through_reverse_stops() {
    let root = raw_lifecycle(
        "cursor",
        serde_json::json!({
            "event_name": "sessionStart",
            "agent_id": "root",
            "agent_name": "generalPurpose",
            "signal": { "signal": "registered" },
        }),
    );
    let child = |id: &str| {
        raw_lifecycle(
            "cursor",
            serde_json::json!({
                "event_name": "subagentStart",
                "agent_id": id,
                "agent_name": "generalPurpose",
                "parent_agent_id": "root",
                "task": format!("task-{id}"),
                "signal": { "signal": "subagent_started" },
            }),
        )
    };
    let stop = |id: &str, errored: bool| {
        raw_lifecycle(
            "cursor",
            serde_json::json!({
                "event_name": "subagentStop",
                "agent_id": id,
                "signal": { "signal": "subagent_stopped", "errored": errored },
            }),
        )
    };

    let events = [
        root,
        child("child-a"),
        child("child-b"),
        stop("child-b", true),
        stop("child-a", false),
    ];
    let events = decode_events(&events);
    let (agents, identity) = reduce_agent_states_seeded_with_identity(
        BTreeMap::new(),
        AgentIdentityState::default(),
        &events,
    );
    assert_eq!(agents.len(), 3);
    let child_a = agents
        .values()
        .find(|agent| agent.agent_id == "child-a")
        .expect("child-a");
    let child_b = agents
        .values()
        .find(|agent| agent.agent_id == "child-b")
        .expect("child-b");
    assert_eq!(child_a.name.as_deref(), Some("generalPurpose"));
    assert_eq!(child_b.name.as_deref(), Some("generalPurpose"));
    assert_eq!(child_a.task.as_deref(), Some("task-child-a"));
    assert_eq!(child_b.task.as_deref(), Some("task-child-b"));
    assert_eq!(child_a.status, AgentStatus::Success);
    assert_eq!(child_b.status, AgentStatus::Failed);
    assert_eq!(
        identity.names,
        BTreeMap::from([(
            "generalPurpose".to_owned(),
            (
                AgentKind::new_unchecked("cursor"),
                AgentSessionId::from("root"),
            ),
        )]),
        "child display labels stay out of the root handle registry",
    );

    let (_, replayed_identity) = reduce_agent_states_seeded_with_identity(agents, identity, &[]);
    assert_eq!(replayed_identity.names.len(), 1);
    assert_eq!(replayed_identity.names["generalPurpose"].1.as_str(), "root");
}

#[test]
fn exact_child_cannot_adopt_a_same_pane_provisional_root() {
    let pane = "tmux:%8";
    let launch = raw_launch(
        AgentLaunchState::Starting,
        "launch_root",
        "root-handle",
        Some(pane),
    );
    let child = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "SubagentStart",
            "agent_id": "child",
            "agent_name": "root-handle",
            "parent_agent_id": "parent",
            "task": "Explore",
            "pane_id": pane,
            "signal": { "signal": "subagent_started" },
        }),
    );

    let agents = reduce_agent_states(&[launch, child]);
    assert!(agents.iter().any(|agent| agent.agent_id == "launch_root"));
    let child = agents
        .iter()
        .find(|agent| agent.agent_id == "child")
        .expect("child");
    assert_eq!(child.name.as_deref(), Some("root-handle"));
    assert_eq!(child.parent_agent_id.as_deref(), Some("parent"));
}

#[test]
fn parent_stamped_observation_cannot_reparent_or_release_existing_roots() {
    let pane = "tmux:%9";
    let exact_root = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "exact",
            "agent_name": "exact-root",
            "signal": { "signal": "registered" },
        }),
    );
    let provisional_root = raw_launch(
        AgentLaunchState::Starting,
        "launch_root",
        "provisional-root",
        Some(pane),
    );
    let child = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "SubagentStart",
            "agent_id": "exact",
            "agent_name": "Explore",
            "parent_agent_id": "parent",
            "task": "Inspect",
            "pane_id": pane,
            "signal": { "signal": "subagent_started" },
        }),
    );

    let events = [exact_root, provisional_root, child];
    let events = decode_events(&events);
    let (agents, identity) = reduce_agent_states_seeded_with_identity(
        BTreeMap::new(),
        AgentIdentityState::default(),
        &events,
    );
    assert!(agents.values().any(|agent| agent.agent_id == "launch_root"));
    let exact = agents
        .values()
        .find(|agent| agent.agent_id == "exact")
        .expect("exact root");
    assert_eq!(exact.name.as_deref(), Some("Explore"));
    assert_eq!(exact.parent_agent_id, None);
    assert!(!identity.names.values().any(|owner| owner.1 == "exact"));
    assert_eq!(identity.names["provisional-root"].1, "launch_root");
}

#[test]
fn explicit_root_registration_clears_stale_child_lineage() {
    let child = raw_lifecycle(
        "pi",
        serde_json::json!({
            "event_name": "subagent_started",
            "agent_id": "resumed-session",
            "parent_agent_id": "temporary-session",
            "task": "resume lane",
            "signal": { "signal": "subagent_started" },
        }),
    );
    let promoted = raw_lifecycle(
        "pi",
        serde_json::json!({
            "event_name": "session_start",
            "agent_id": "resumed-session",
            "explicit_root": true,
            "signal": { "signal": "registered" },
        }),
    );

    let agents = reduce_agent_states(&[child, promoted]);
    let resumed = agents
        .iter()
        .find(|agent| agent.agent_id == "resumed-session")
        .expect("resumed session");
    assert_eq!(resumed.parent_agent_id, None);
}

#[test]
fn legacy_registration_preserves_established_child_lineage() {
    let child = raw_lifecycle(
        "pi",
        serde_json::json!({
            "event_name": "subagent_started",
            "agent_id": "child-session",
            "parent_agent_id": "root-session",
            "task": "review lane",
            "signal": { "signal": "subagent_started" },
        }),
    );
    let legacy_registration = raw_lifecycle(
        "pi",
        serde_json::json!({
            "event_name": "session_start",
            "agent_id": "child-session",
            "signal": { "signal": "registered" },
        }),
    );

    let agents = reduce_agent_states(&[child, legacy_registration]);
    let child = agents
        .iter()
        .find(|agent| agent.agent_id == "child-session")
        .expect("child session");
    assert_eq!(child.parent_agent_id.as_deref(), Some("root-session"));
}

#[test]
fn subagent_adoption_attaches_an_existing_root_once() {
    let root = raw_lifecycle(
        "antigravity",
        serde_json::json!({
            "event_name": "PreInvocation",
            "agent_id": "child",
            "signal": { "signal": "turn_started" },
        }),
    );
    let adoption = raw_lifecycle(
        "antigravity",
        serde_json::json!({
            "event_name": "SubagentAdopted",
            "agent_id": "child",
            "parent_agent_id": "parent",
            "task": "Inspect",
            "signal": { "signal": "subagent_stopped", "errored": false },
        }),
    );

    let agents = reduce_agent_states(&[root, adoption]);
    let child = agents
        .iter()
        .find(|agent| agent.agent_id == "child")
        .expect("adopted child");
    assert_eq!(child.status, AgentStatus::Success);
    assert_eq!(child.parent_agent_id.as_deref(), Some("parent"));
    assert_eq!(child.task.as_deref(), Some("Inspect"));
}

#[test]
fn root_and_child_labels_do_not_contend_across_allocator_rebuilds() {
    let child = raw_lifecycle(
        "cursor",
        serde_json::json!({
            "event_name": "subagentStart",
            "agent_id": "child",
            "agent_name": "shared-label",
            "parent_agent_id": "parent",
            "task": "Explore",
            "signal": { "signal": "subagent_started" },
        }),
    );
    let child_events = [child];
    let child_events = decode_events(&child_events);
    let (children, mut identity) = reduce_agent_states_seeded_with_identity(
        BTreeMap::new(),
        AgentIdentityState::default(),
        &child_events,
    );
    let child_ordinal = children
        .values()
        .find(|agent| agent.agent_id == "child")
        .and_then(|agent| agent.kind_ordinal)
        .expect("child ordinal");
    identity.names.insert(
        "shared-label".to_owned(),
        (
            AgentKind::new_unchecked("cursor"),
            AgentSessionId::from("child"),
        ),
    );
    let root = raw_lifecycle(
        "cursor",
        serde_json::json!({
            "event_name": "sessionStart",
            "agent_id": "root",
            "agent_name": "shared-label",
            "signal": { "signal": "registered" },
        }),
    );
    let root_events = [root];
    let root_events = decode_events(&root_events);

    let (agents, identity) =
        reduce_agent_states_seeded_with_identity(children, identity, &root_events);
    let child = agents
        .values()
        .find(|agent| agent.agent_id == "child")
        .expect("child");
    let root = agents
        .values()
        .find(|agent| agent.agent_id == "root")
        .expect("root");
    assert_eq!(child.name.as_deref(), Some("shared-label"));
    assert_eq!(root.name.as_deref(), Some("shared-label"));
    assert_eq!(child.kind_ordinal, Some(child_ordinal));
    assert_eq!(root.kind_ordinal, Some(child_ordinal + 1));
    assert_eq!(identity.names["shared-label"].1.as_str(), "root");

    let (_, replayed_identity) = reduce_agent_states_seeded_with_identity(agents, identity, &[]);
    assert_eq!(replayed_identity.names.len(), 1);
    assert_eq!(replayed_identity.names["shared-label"].1.as_str(), "root");
}

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
fn established_subagent_parent_survives_mismatched_stop() {
    let start = raw_lifecycle(
        "pi",
        serde_json::json!({
            "event_name": "subagent_started",
            "agent_id": "child-1",
            "signal": { "signal": "subagent_started" },
            "parent_agent_id": "parent-a",
            "task": "reviewer",
        }),
    );
    let stop = raw_lifecycle(
        "pi",
        serde_json::json!({
            "event_name": "subagent_stopped",
            "agent_id": "child-1",
            "signal": { "signal": "subagent_stopped", "errored": false },
            "parent_agent_id": "parent-b",
        }),
    );

    let agents = reduce_agent_states(&[start, stop]);
    let child = agents
        .iter()
        .find(|agent| agent.agent_id == "child-1")
        .expect("child row");
    assert_eq!(child.status, AgentStatus::Success);
    assert_eq!(child.parent_agent_id.as_deref(), Some("parent-a"));
    assert_eq!(child.task.as_deref(), Some("reviewer"));
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
fn repeated_running_subagent_start_enriches_without_resetting_identity() {
    let start = raw_lifecycle_at(
        "opencode",
        1,
        serde_json::json!({
            "event_name": "SubagentStart",
            "agent_id": "child-1",
            "signal": { "signal": "subagent_started" },
            "parent_agent_id": "sess-root",
            "task": "review auth",
        }),
    );
    let model_announcement = raw_lifecycle_at(
        "opencode",
        2,
        serde_json::json!({
            "event_name": "SubagentStart",
            "agent_id": "child-1",
            "signal": { "signal": "subagent_started" },
            "parent_agent_id": "sess-root",
            "model": "claude-sonnet-4-5",
        }),
    );

    let agents = reduce_agent_states(&[start, model_announcement]);
    let child = agents
        .iter()
        .find(|agent| agent.agent_id == "child-1")
        .expect("child row");
    assert_eq!(child.status, AgentStatus::Running);
    assert_eq!(child.task.as_deref(), Some("review auth"));
    assert_eq!(child.model.as_deref(), Some("claude-sonnet-4-5"));
    assert_eq!(
        child.registered_at,
        Some(Timestamp::from_second(epoch().as_second() + 1).unwrap()),
    );
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
