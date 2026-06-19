use super::*;

// ── Feed classification: which pending items become attention ───────────────

#[test]
fn pending_items_classify_to_metadata_until_a_live_frame_admits_rows() {
    let mut native = FeedItem::new(
        workspace(),
        Surface::NativeUi,
        FeedKind::Permission,
        "n",
        "claude",
        "agent-hook",
    );
    let bridge = FeedItem::new(
        workspace(),
        Surface::Bridge,
        FeedKind::Permission,
        "b",
        "rimz",
        "cli",
    );
    let mut answered = FeedItem::new(
        workspace(),
        Surface::Bridge,
        FeedKind::Permission,
        "a",
        "rimz",
        "cli",
    );
    let mut cli_native = FeedItem::new(
        workspace(),
        Surface::NativeUi,
        FeedKind::Generic,
        "Should I proceed?",
        "rimz",
        "cli",
    );
    let mut script = FeedItem::new(
        workspace(),
        Surface::Script,
        FeedKind::Question,
        "approve deploy?",
        "deploy",
        "script",
    );
    answered.status = FeedStatus::Resolved;
    let mut timed = FeedItem::new(
        workspace(),
        Surface::Bridge,
        FeedKind::Permission,
        "t",
        "rimz",
        "cli",
    );
    timed.status = FeedStatus::TimedOut;
    native.updated_at += std::time::Duration::from_secs(1);
    cli_native.updated_at += std::time::Duration::from_secs(2);
    script.worktree_path = Some("/repo/rimz".to_owned());

    let snap = room(
        vec![native, bridge, answered, timed, cli_native, script],
        Vec::new(),
    );
    // Agent-native and script asks survive as attention metadata; bridge asks
    // remain resolver-working metadata. CLI native asks, resolved items, and
    // timed-out items are history for the sidebar. Without a live frame, none
    // of the metadata becomes a row.
    assert_eq!(snap.needs_attention.len(), 2);
    assert_eq!(snap.resolver_working.len(), 1);
    assert!(snap.worktree_groups.is_empty());
}

#[test]
fn multiple_pending_asks_for_one_session_render_one_row() {
    // The live pile-up: a session held several pending native_ui asks, and
    // the no-panes rollup emitted one row each. Read-time dedup collapses
    // them to a single row keyed by `(source, agent_id)`.
    let session = agent("claude", "sess-1", AgentStatus::Idle, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    let items = vec![
        agent_ask(FeedKind::Permission, "claude", "sess-1"),
        agent_ask(FeedKind::Question, "claude", "sess-1"),
    ];

    let snapshot =
        room(items, vec![session]).with_live_panes(vec![pane("%1", "claude", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    let agent_rows: Vec<_> = rows.iter().filter(|row| row.is_agent()).collect();
    assert_eq!(
        agent_rows.len(),
        1,
        "two pending asks for one session collapse to one row: {rows:?}"
    );
    assert_eq!(agent_rows[0].status(), Some(AgentStatus::Waiting));
}

#[test]
fn row_from_agent_projects_role_then_profile_as_display_handle() {
    let mut role = agent("claude", "sess-role", AgentStatus::Idle, 1_000);
    role.role = Some("planner".to_owned());
    role.profile = Some("claude-planner".to_owned());
    let row = row_from_agent(&role, epoch());
    assert_eq!(row.name, "claude");
    assert_eq!(row.as_agent().unwrap().handle.as_deref(), Some("planner"));
    assert_eq!(row.display_name(), "planner");

    let mut profile = agent("claude", "sess-profile", AgentStatus::Idle, 1_000);
    profile.profile = Some("claude-planner".to_owned());
    let row = row_from_agent(&profile, epoch());
    assert_eq!(row.name, "claude");
    assert_eq!(
        row.as_agent().unwrap().handle.as_deref(),
        Some("claude-planner")
    );
    assert_eq!(row.display_name(), "claude-planner");

    let bare = agent("claude", "sess-bare", AgentStatus::Idle, 1_000);
    let row = row_from_agent(&bare, epoch());
    assert_eq!(row.name, "claude");
    assert_eq!(row.as_agent().unwrap().handle, None);
    assert_eq!(row.display_name(), "claude");
}

// ── Activity heartbeat fold ─────────────────────────────────────────────────

#[test]
fn activity_heartbeat_updates_last_activity_not_phase() {
    let mut agent = agent("claude", "sess-1", AgentStatus::Running, 50_000);
    agent.phase = TurnPhase::Reasoning;
    let original_seen = agent.last_seen;
    let at = original_seen + std::time::Duration::from_secs(10);
    let touch = AgentActivity {
        kind: agent.kind.clone(),
        agent_id: agent.agent_id.clone(),
        at,
    };
    let snap = room(Vec::new(), vec![agent]).with_agent_activity(&[touch]);

    // The heartbeat is latency, not a lifecycle signal — it advances
    // `last_activity` only, never the turn-phase head.
    assert_eq!(snap.agents[0].phase, TurnPhase::Reasoning);
    assert_eq!(snap.agents[0].last_activity, at);
    assert_eq!(snap.agents[0].last_seen, original_seen);
}
