use super::*;

#[test]
fn merge_carryover_prefers_newer_observation_and_preserves_orphans() {
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

    let only_in_carryover = agent("claude", "agent-1", AgentStatus::Idle, 1_000);
    let only_live = agent("codex", "agent-2", AgentStatus::Running, 2_000);
    let merged = merge_agent_rollups(
        std::slice::from_ref(&only_in_carryover),
        std::slice::from_ref(&only_live),
    );
    assert_eq!(merged.len(), 2);
}

#[test]
fn live_winner_backfills_trimmed_carryover_enrichment() {
    let mut carried = agent("claude", "agent-1", AgentStatus::Idle, 1_000);
    carried.transcript_path = Some("/tmp/transcript.jsonl".into());
    carried.worktree_path = Some("/repo".into());
    carried.worktree_branch = Some("main".into());
    carried.role = Some("coder".into());
    carried.team = Some("forge".into());
    carried.channel = Some("event-log".into());
    carried.profile = Some("claude-coder".into());
    carried.model = Some("opus".into());
    carried.effort = Some("high".into());
    carried.usage.context_window = Some(200_000);
    carried.last_compact_command_tokens = Some(180_000);
    let live = agent("claude", "agent-1", AgentStatus::Running, 2_000);

    let merged = merge_agent_rollups(std::slice::from_ref(&carried), std::slice::from_ref(&live));

    assert_eq!(merged.len(), 1);
    let agent = &merged[0];
    assert_eq!(agent.status, AgentStatus::Running);
    assert_eq!(agent.transcript_path, carried.transcript_path);
    assert_eq!(agent.worktree_path, carried.worktree_path);
    assert_eq!(agent.worktree_branch, carried.worktree_branch);
    assert_eq!(agent.role, carried.role);
    assert_eq!(agent.team, carried.team);
    assert_eq!(agent.channel, carried.channel);
    assert_eq!(agent.profile, carried.profile);
    assert_eq!(agent.model, carried.model);
    assert_eq!(agent.effort, carried.effort);
    assert_eq!(agent.usage.context_window, carried.usage.context_window);
    assert_eq!(
        agent.last_compact_command_tokens,
        carried.last_compact_command_tokens
    );
}

#[test]
fn carryover_session_end_stamps_and_preserves_resumable_identity() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut carried = agent("claude", "agent-1", AgentStatus::Idle, 1_000);
    carried.name = Some("lucid-atlas".to_owned());
    carried.name_explicit = true;
    carried.kind_ordinal = Some(7);
    carried.worktree_path = Some("/repo/forge".to_owned());
    carried.worktree_branch = Some("feature/forge".to_owned());
    carried.role = Some("planner".to_owned());
    carried.team = Some("forge".to_owned());
    carried.launch_group = Some("launch_forge".to_owned());
    carried.launch_ordinal = Some(0);
    carried.channel = Some("forge".to_owned());
    carried.profile = Some("claude-planner".to_owned());
    carried.transcript_path = Some("/provider/agent-1.jsonl".to_owned());
    let ended = lifecycle_at(
        &workspace,
        "claude",
        "SessionEnd",
        "agent-1",
        lifecycle::LifecycleSignal::Ended,
    );

    let ended_at = ended.timestamp;
    let merged = agent_rollup_with_carryover(&[ended], vec![carried.clone()]);

    assert_eq!(merged.len(), 1);
    let retained = &merged[0];
    assert_eq!(retained.ended_at, Some(ended_at));
    assert_eq!(retained.last_seen, ended_at);
    assert_eq!(retained.name, carried.name);
    assert_eq!(retained.name_explicit, carried.name_explicit);
    assert_eq!(retained.kind_ordinal, carried.kind_ordinal);
    assert_eq!(retained.worktree_path, carried.worktree_path);
    assert_eq!(retained.worktree_branch, carried.worktree_branch);
    assert_eq!(retained.role, carried.role);
    assert_eq!(retained.team, carried.team);
    assert_eq!(retained.launch_group, carried.launch_group);
    assert_eq!(retained.launch_ordinal, carried.launch_ordinal);
    assert_eq!(retained.channel, carried.channel);
    assert_eq!(retained.profile, carried.profile);
    assert_eq!(retained.transcript_path, carried.transcript_path);
}

#[test]
fn legacy_lost_markers_replay_as_state_noops() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let registered = lifecycle_at(
        &workspace,
        "claude",
        "SessionStart",
        "agent-1",
        lifecycle::LifecycleSignal::Registered,
    );
    let lost = lifecycle_at(
        &workspace,
        "claude",
        "rimz.agent-lost",
        "agent-1",
        lifecycle::LifecycleSignal::Lost,
    );

    let merged = agent_rollup_with_carryover(&[registered, lost], Vec::new());

    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].agent_id.as_str(), "agent-1");
    assert!(
        agent_rollup_with_carryover(
            &[lifecycle_at(
                &workspace,
                "claude",
                "rimz.agent-lost",
                "unknown",
                lifecycle::LifecycleSignal::Lost,
            )],
            Vec::new(),
        )
        .is_empty(),
        "lost-only legacy markers stay parseable without creating agent rows"
    );
}

#[test]
fn legacy_carryover_agents_get_card_identity() {
    let mut carried = agent("claude", "agent-1", AgentStatus::Idle, 1_000);
    carried.name = None;
    carried.kind_ordinal = None;

    let merged = agent_rollup_with_carryover(&[], vec![carried]);

    assert_eq!(merged.len(), 1);
    assert!(merged[0].name.is_some());
    assert_eq!(merged[0].kind_ordinal, Some(1));
}

#[test]
fn rebirth_after_rotation_reassigns_carryover_ordinals_without_colliding_with_live_agents() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let mut carried = agent("claude", "old-session", AgentStatus::Idle, 1_000);
    carried.name = Some("lucid-atlas".to_owned());
    carried.kind_ordinal = Some(1);
    let events = vec![
        EventEnvelope::session_rebirth(workspace.clone(), "session"),
        lifecycle_at(
            &workspace,
            "claude",
            "SessionStart",
            "new-session",
            lifecycle::LifecycleSignal::Registered,
        ),
    ];

    let merged = agent_rollup_with_carryover(&events, vec![carried]);
    let old = merged
        .iter()
        .find(|agent| agent.agent_id.as_str() == "old-session")
        .expect("carryover agent survives audit rollup");
    let new = merged
        .iter()
        .find(|agent| agent.agent_id.as_str() == "new-session")
        .expect("new live agent");

    assert_eq!(new.kind_ordinal, Some(1));
    assert_eq!(old.kind_ordinal, Some(2));
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
        agent_identity: AgentIdentityState::default(),
        resume_outcomes: Vec::new(),
    };
    write_carryover(&path, &carryover).unwrap();
    let loaded = read_carryover(&path).unwrap();
    assert_eq!(loaded, carryover);
}
