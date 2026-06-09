use super::*;

#[test]
fn merge_carryover_prefers_newer_observation() {
    for (label, carried_seen, live_seen) in [
        ("strictly newer live observation wins", 1_000, 2_000),
        ("live wins an equal last_seen tie", 2_000, 2_000),
    ] {
        let mut carried = agent("claude", "agent-1", AgentStatus::Idle, carried_seen);
        carried.worktree_branch = Some("main".into());
        let mut live = agent("claude", "agent-1", AgentStatus::Running, live_seen);
        live.worktree_branch = Some("feature".into());

        let merged =
            merge_agent_rollups(std::slice::from_ref(&carried), std::slice::from_ref(&live));
        assert_eq!(merged.len(), 1, "{label}");
        assert_eq!(merged[0].status, AgentStatus::Running, "{label}");
        assert_eq!(
            merged[0].worktree_branch.as_deref(),
            Some("feature"),
            "{label}"
        );
    }
}

#[test]
fn merge_carryover_preserves_orphaned_entries() {
    let only_in_carryover = agent("claude", "agent-1", AgentStatus::Idle, 1_000);
    let only_live = agent("codex", "agent-2", AgentStatus::Running, 2_000);
    let merged = merge_agent_rollups(
        std::slice::from_ref(&only_in_carryover),
        std::slice::from_ref(&only_live),
    );
    assert_eq!(merged.len(), 2);
}

#[test]
fn carryover_session_end_tombstones_older_agent_state() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let carried = agent("claude", "agent-1", AgentStatus::Idle, 1_000);
    let ended = lifecycle_at(
        &workspace,
        "claude",
        "SessionEnd",
        "agent-1",
        lifecycle::LifecycleSignal::Ended,
    );

    let merged = agent_rollup_with_carryover(&[ended], vec![carried]);

    assert!(
        merged.is_empty(),
        "active-log SessionEnd must tombstone older carryover state"
    );
}

#[test]
fn carryover_round_trips_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agents.carryover.json");
    assert_eq!(
        read_carryover(&path).unwrap(),
        EventCarryover::default(),
        "missing file yields empty carryover"
    );

    let carryover = EventCarryover {
        agents: vec![{
            let mut agent = agent("claude", "agent-1", AgentStatus::Success, 3_000);
            agent.worktree_branch = Some("main".into());
            agent
        }],
    };
    write_carryover(&path, &carryover).unwrap();
    let loaded = read_carryover(&path).unwrap();
    assert_eq!(loaded, carryover);
}
