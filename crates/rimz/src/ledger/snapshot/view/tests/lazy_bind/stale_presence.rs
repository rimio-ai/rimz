use super::*;

#[test]
fn paneless_claude_agent_recovers_by_exact_cwd_after_rebirth() {
    // A session.rebirth clears pane stamps even while the pane's Claude process
    // keeps running. The read-time cwd bind recovers that live non-lazy session
    // before the next hook re-stamps the pane.
    let claude = agent("claude", "sess-1", AgentStatus::Running, 1_000).worktree("/repo/main");
    let snapshot = room(Vec::new(), vec![claude])
        .with_live_panes(vec![pane("term1", "claude", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_agent());
    assert_eq!(rows[0].id, "sess-1");
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "term1");
}

#[test]
fn two_paneless_codex_in_one_worktree_bind_most_recent() {
    // When two pane-less Codex sessions claim one worktree — a lingering
    // closed session and a live one — the most-recently-active binds the
    // single live pane; the stale session does not render.
    let snapshot = room(
        Vec::new(),
        vec![
            paneless_codex("sess-old", "/repo/main", 1_000),
            paneless_codex("sess-new", "/repo/main", 2_000),
        ],
    )
    .with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_agent());
    assert_eq!(rows[0].id, "sess-new");
}

#[test]
fn paneless_codex_and_new_stamped_codex_share_one_worktree_without_idle_row() {
    // Daemon-routed Codex can first bind one session by cwd, then recover a
    // newer session's focused pane at hook ingestion. The older paneless
    // session must survive long enough to bind the other same-cwd pane.
    let newer = paneless_codex("sess-new", "/repo/main", 2_000).in_pane("%2");
    let snapshot = room(
        Vec::new(),
        vec![paneless_codex("sess-old", "/repo/main", 1_000), newer],
    )
    .with_live_panes(
        vec![
            pane("%1", "codex", "/repo/main"),
            pane("%2", "codex", "/repo/main"),
        ],
        None,
    );

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.is_agent()));
    let old = rows
        .iter()
        .find(|row| row.id == "sess-old")
        .expect("older session renders");
    let new = rows
        .iter()
        .find(|row| row.id == "sess-new")
        .expect("newer session renders");
    assert_eq!(old.pane.as_ref().unwrap().pane_id.raw(), "%1");
    assert_eq!(new.pane.as_ref().unwrap().pane_id.raw(), "%2");
}

#[test]
fn resumed_codex_pane_binds_the_matching_session_exactly() {
    let mut old = paneless_codex("sess-old", "/repo/main", 1_000);
    old.registered_at = Some(ago(1_000));
    old.last_activity = ago(1_000);
    let newer = paneless_codex("sess-new", "/repo/main", 2_000).active_ago(-1);
    let resumed_pane = PaneRef {
        command: Some("codex resume sess-old".to_owned()),
        pane_process_start: Some(epoch()),
        resumed_session_id: Some("sess-old".into()),
        ..pane("term1", "codex", "/repo/main")
    };

    let snapshot = room(Vec::new(), vec![newer, old]).with_live_panes(vec![resumed_pane], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "sess-old");
}

#[test]
fn resumed_codex_pane_heals_a_stale_existing_stamp() {
    let mut stale_stamp = pane("term1", "codex", "/repo/main");
    stale_stamp.pane_process_start = Some(ago(1_000));
    let mut old = paneless_codex("sess-old", "/repo/main", 1_000);
    old.registered_at = Some(ago(1_000));
    old.last_activity = ago(900);
    old.pane = Some(stale_stamp);
    let resumed_pane = PaneRef {
        command: Some("codex".to_owned()),
        pane_process_start: Some(ago(1)),
        resumed_session_id: Some("sess-old".into()),
        ..pane("term1", "codex", "/repo/main")
    };

    let snapshot = room(Vec::new(), vec![old]).with_live_panes(vec![resumed_pane], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "sess-old");
    assert_eq!(
        rows[0].pane.as_ref().unwrap().pane_process_start,
        Some(ago(1))
    );
}

#[test]
fn paneless_codex_sessions_pair_by_latest_process_start_before_first_event() {
    let mut older = paneless_codex("sess-old", "/repo/main", 1_000);
    older.registered_at = Some(ago(3_000));
    older.last_activity = ago(2_000);
    let mut newer = paneless_codex("sess-new", "/repo/main", 2_000);
    newer.registered_at = Some(ago(8));
    newer.last_activity = ago(1);
    let old_pane = PaneRef {
        pane_process_start: Some(ago(3_600)),
        ..pane("terminal_4", "codex", "/repo/main")
    };
    let new_pane = PaneRef {
        pane_process_start: Some(ago(9)),
        ..pane("terminal_58", "codex", "/repo/main")
    };

    for (agents, panes) in [
        (
            vec![older.clone(), newer.clone()],
            vec![old_pane.clone(), new_pane.clone()],
        ),
        (vec![newer, older], vec![new_pane, old_pane]),
    ] {
        let snapshot = room(Vec::new(), agents).with_live_panes(panes, None);
        assert_eq!(
            row(&snapshot, "sess-old")
                .pane
                .as_ref()
                .unwrap()
                .pane_id
                .raw(),
            "terminal_4"
        );
        assert_eq!(
            row(&snapshot, "sess-new")
                .pane
                .as_ref()
                .unwrap()
                .pane_id
                .raw(),
            "terminal_58"
        );
    }
}

#[test]
fn stale_session_ask_does_not_render_or_steal_a_pane() {
    // Reproduces the live bug: a pending permission ask whose claude
    // session has ended must not become attention, and must not latch onto
    // a freshly launched codex sharing the worktree.
    let stale = agent_ask(FeedKind::Permission, "claude", "ended-claude");

    // Only a live codex session remains in the rollup.
    let codex = agent("codex", "sess-codex", AgentStatus::Idle, 2_000)
        .worktree("/repo/main")
        .in_pane("%1");

    let snapshot = room(vec![stale], vec![codex])
        .with_live_panes(vec![pane("%1", "codex", "/repo/main")], None);

    assert!(
        snapshot.needs_attention.is_empty(),
        "stale ask is not attention"
    );
    assert_eq!(snapshot.worktree_groups.len(), 1);
    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1, "only the live codex renders");
    assert_eq!(rows[0].name, "codex");
    assert_eq!(rows[0].status(), Some(AgentStatus::Idle));
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "%1");
}

#[test]
fn superseded_zombie_ask_yields_pane_to_the_fresh_session() {
    // Live reproduction: a pidless `SessionStart`-only claude never ends and
    // never gets reaped, so it lingers in the rollup with an old pending
    // ask. A freshly launched claude shares the worktree. The ask must not
    // render as attention or pin the dead session's stale timestamp onto the
    // live pane — the fresh session binds it idle.
    let stale = agent_ask(FeedKind::Permission, "claude", "zombie-claude");

    let zombie = agent("claude", "zombie-claude", AgentStatus::Idle, 1_000).worktree("/repo/main");
    // Only the fresh session stamped the live pane; the zombie holds none.
    let fresh = agent("claude", "fresh-claude", AgentStatus::Idle, 2_000)
        .worktree("/repo/main")
        .in_pane("%1");

    let snapshot = room(vec![stale], vec![zombie, fresh])
        .with_live_panes(vec![pane("%1", "claude", "/repo/main")], None);

    assert!(
        snapshot.needs_attention.is_empty(),
        "the superseded session's ask is not attention"
    );
    assert_eq!(snapshot.worktree_groups.len(), 1);
    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1, "only the fresh session renders");
    assert_eq!(rows[0].id, "fresh-claude");
    assert_eq!(rows[0].status(), Some(AgentStatus::Idle));
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "%1");
}

#[test]
fn live_codex_command_does_not_corroborate_claude_attention() {
    // Live reproduction: an old Claude ask still has a ledger session, but
    // the only live pane in the worktree is `node /usr/bin/codex`. The
    // pane must remain Codex-shaped instead of inheriting Claude's model
    // and stale ask age.
    let stale = agent_ask(FeedKind::Permission, "claude", "stale-claude");

    let mut claude =
        agent("claude", "stale-claude", AgentStatus::Idle, 1_000).worktree("/repo/main");
    claude.model = Some("claude-opus-4-7".to_owned());

    let snapshot = room(vec![stale], vec![claude])
        .with_live_panes(vec![pane("%1", "node /usr/bin/codex", "/repo/main")], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_process());
    assert_eq!(rows[0].name, "codex");
    assert!(snapshot.worktree_groups[0].status_counts.is_empty());
}

/// User's reported scenario: ledger carries a pile of stale claude
/// observations from killed sessions (no SessionEnd ever fired), all
/// claiming the same worktree path. A fresh claude pane lands. The fresh
/// agent must still bind to its pane — stale count does not block live
/// presence.
#[test]
fn live_claude_pane_binds_despite_pile_of_stale_ledger_ghosts() {
    let stale =
        |id: &str, rank: i64| agent("claude", id, AgentStatus::Idle, rank).worktree("/repo/main");
    let live = agent("claude", "live", AgentStatus::Running, i64::from(u32::MAX))
        .worktree("/repo/main")
        .in_pane("%1");

    let snapshot = room(
        Vec::new(),
        vec![
            stale("stale-a", 1_000),
            stale("stale-b", 1_001),
            stale("stale-c", 1_002),
            live,
        ],
    )
    .with_live_panes(vec![pane("%1", "claude", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    let agent_rows: Vec<_> = rows.iter().filter(|r| r.is_agent()).collect();
    assert_eq!(agent_rows.len(), 1, "only the live claude renders");
    assert_eq!(agent_rows[0].id, "live");
}
