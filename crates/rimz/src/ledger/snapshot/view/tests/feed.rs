use super::*;

// ── Feed classification: which pending items become attention ───────────────

#[test]
fn build_groups_by_surface_and_status() {
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

    let snap = room(vec![native, bridge, answered, timed], Vec::new());
    // Pending native + bridge asks surface as attention/working metadata; the
    // resolved and timed-out items are history, so they are dropped. Without a
    // live frame, none of them become rows.
    assert_eq!(snap.needs_attention.len(), 1);
    assert_eq!(snap.resolver_working.len(), 1);
    assert!(snap.worktree_groups.is_empty());
}

#[test]
fn pending_cli_native_items_do_not_become_sidebar_attention() {
    let item = FeedItem::new(
        workspace(),
        Surface::NativeUi,
        FeedKind::Generic,
        "Should I proceed?",
        "rimz",
        "cli",
    );

    let snap = room(vec![item], Vec::new());

    assert!(snap.needs_attention.is_empty());
    assert!(snap.worktree_groups.is_empty());
}

#[test]
fn pending_script_items_wait_for_a_live_frame() {
    let mut item = FeedItem::new(
        workspace(),
        Surface::Script,
        FeedKind::Question,
        "Should I proceed?",
        "rimz",
        "cli",
    );
    item.worktree_path = Some("/repo/rimz".to_owned());
    item.worktree_branch = Some("main".to_owned());

    let snap = room(vec![item], Vec::new());

    assert_eq!(snap.needs_attention.len(), 1);
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
fn pending_attention_survives_as_metadata_without_pane_fold_in() {
    let item = FeedItem::new(
        workspace(),
        Surface::Script,
        FeedKind::Question,
        "approve deploy?",
        "deploy",
        "script",
    );

    let snapshot = room(vec![item], Vec::new());

    assert_eq!(snapshot.needs_attention.len(), 1);
    assert!(snapshot.worktree_groups.is_empty());
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
