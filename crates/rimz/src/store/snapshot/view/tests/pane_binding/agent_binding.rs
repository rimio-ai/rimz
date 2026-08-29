use super::*;
use crate::agents::SessionOrigin;

#[test]
fn live_panes_overlay_only_matching_agent_rows() {
    for (label, stamped_pane, pane_command, expect_agent) in [
        (
            "stamped agent overlays matching pane",
            Some("%1"),
            "codex",
            true,
        ),
        (
            "unmatched store agent leaves process row",
            None,
            "zsh",
            false,
        ),
    ] {
        let mut codex =
            agent("codex", "sess-1", AgentStatus::Running, 1_000).worktree("/repo/main");
        if let Some(raw_pane) = stamped_pane {
            codex = codex.branch("main").in_pane(raw_pane);
        }
        let snapshot =
            room(vec![codex]).with_live_panes(vec![pane("%1", pane_command, "/repo/main")], None);

        assert_eq!(snapshot.worktree_groups.len(), 1, "{label}");
        assert_eq!(snapshot.worktree_groups[0].rows.len(), 1, "{label}");
        let row = &snapshot.worktree_groups[0].rows[0];
        assert_eq!(row.is_agent(), expect_agent, "{label}");
        assert_eq!(row.is_process(), !expect_agent, "{label}");
        assert_eq!(row.pane.as_ref().unwrap().pane_id.raw(), "%1", "{label}");
        if expect_agent {
            assert_eq!(row.id, "sess-1", "{label}");
        } else {
            assert_eq!(row.name, "zsh", "{label}");
        }
    }
}

#[test]
fn stamped_codex_returned_to_shell_without_hosted_process_renders_process_row() {
    // A stamped lazy agent holds through child foreground commands only while
    // the producer confirms its in-pane CLI process is still present. Once the
    // pane returns to a shell with no hosted agent process, the old Codex card
    // demotes even though the stale session remains in the rollup.
    let codex = agent("codex", "sess-1", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    let returned = PaneRef {
        spawn_command: Some("/bin/rimz agents exec codex --worktree-path /repo/main".to_owned()),
        ..pane("%1", "zsh", "/repo/main")
    };
    let snapshot = room(vec![codex]).with_live_panes(vec![returned], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_process());
    assert_eq!(rows[0].name, "zsh");
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "%1");
}

#[test]
fn forked_side_session_does_not_repaint_primary_card() {
    // Codex `/side` / `/btw` forks the conversation into a fresh session id in
    // the same pane and process. Both sessions stamp `%1` as root agents, but
    // the fork registered later and just posted the side question, so it holds
    // the newer `last_activity`. The card must stay on the primary (earliest-
    // registered) session — never flip to the fork.
    let mut main = agent("codex", "main-sess", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(120);
    main.registered_at = Some(ago(600));

    let mut fork = agent("codex", "fork-sess", AgentStatus::Running, 2_000)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(5);
    fork.registered_at = Some(ago(60));

    let snapshot =
        room(vec![main, fork]).with_live_panes(vec![pane("%1", "codex", "/repo/main")], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1, "one top-level row on the shared pane");
    assert!(rows[0].is_agent());
    assert_eq!(
        rows[0].id, "main-sess",
        "the card stays on the primary session, not the newer-activity fork",
    );
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "%1");
}

#[test]
fn antigravity_shared_pane_follows_latest_conversation() {
    let mut older = agent(
        "antigravity",
        "conversation-old",
        AgentStatus::Success,
        1_000,
    )
    .worktree("/repo/main")
    .in_pane("%1")
    .active_ago(120);
    older.registered_at = Some(ago(600));

    let mut newer = agent(
        "antigravity",
        "conversation-new",
        AgentStatus::Running,
        2_000,
    )
    .worktree("/repo/main")
    .in_pane("%1")
    .active_ago(5);
    newer.registered_at = Some(ago(60));

    let snapshot =
        room(vec![older, newer]).with_live_panes(vec![pane("%1", "agy", "/repo/main")], None);
    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "conversation-new");
}

#[test]
fn shared_pane_primary_is_stable_when_registration_ties() {
    for (label, a_active_ago, b_active_ago) in [
        ("agent-b is fresher", 120, 5),
        ("agent-a is fresher", 5, 120),
    ] {
        let mut agent_a = agent("codex", "agent-a", AgentStatus::Running, 1_000)
            .worktree("/repo/main")
            .in_pane("%1")
            .active_ago(a_active_ago);
        agent_a.registered_at = None;

        let mut agent_b = agent("codex", "agent-b", AgentStatus::Running, 2_000)
            .worktree("/repo/main")
            .in_pane("%1")
            .active_ago(b_active_ago);
        agent_b.registered_at = None;

        let snapshot = room(vec![agent_a, agent_b])
            .with_live_panes(vec![pane("%1", "codex", "/repo/main")], None);

        let rows = rows(&snapshot);
        assert_eq!(
            rows.len(),
            1,
            "{label}: one top-level row on the shared pane"
        );
        assert!(rows[0].is_agent(), "{label}");
        assert_eq!(
            rows[0].id, "agent-a",
            "{label}: registration ties pin to the stable lowest session id",
        );
        assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "%1");
    }
}

#[test]
fn forked_side_session_survives_the_reaper_and_keeps_primary_card() {
    // Production shape: the ghost reaper runs before the live-pane fold
    // (`assemble.rs`). A `/side` / `/btw` fork shares the primary's daemon owner
    // pid, so the same-pane supersession reaper must spare the primary instead
    // of collapsing it as a relaunch — otherwise `stamped_agent_for_pane` never
    // sees the primary and the card flips to the fork.
    let mut main = agent("codex", "main-sess", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(120);
    main.registered_at = Some(ago(600));
    main.runtime_owner = Some(RuntimeOwner::new(
        RuntimeOwnerKind::Agent,
        "main-sess",
        9_999,
        None,
    )); // shared app-server daemon pid

    let mut fork = agent("codex", "fork-sess", AgentStatus::Running, 2_000)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(5);
    fork.registered_at = Some(ago(60));
    fork.runtime_owner = Some(RuntimeOwner::new(
        RuntimeOwnerKind::Agent,
        "fork-sess",
        9_999,
        None,
    )); // same daemon pid — a fork, not a relaunch

    let mut snapshot = room(vec![main, fork]);
    snapshot.reap_stale_sessions();
    let snapshot = snapshot.with_live_panes(vec![pane("%1", "codex", "/repo/main")], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1, "one top-level row on the shared pane");
    assert!(rows[0].is_agent());
    assert_eq!(
        rows[0].id, "main-sess",
        "the reaper spares the fork's primary, so the card stays on it end-to-end",
    );
}

#[test]
fn cleared_fresh_session_reap_repins_shared_pane_but_fork_keeps_primary() {
    for (label, main_origin, replacement_origin, expected) in [
        (
            "fresh replacement",
            Some(SessionOrigin::Fresh),
            Some(SessionOrigin::Fresh),
            "new-sess",
        ),
        (
            "fork or unknown",
            Some(SessionOrigin::Fresh),
            Some(SessionOrigin::Forked),
            "main-sess",
        ),
    ] {
        let mut main = agent("codex", "main-sess", AgentStatus::Success, 1_000)
            .worktree("/repo/main")
            .in_pane("%1")
            .active_ago(120);
        main.registered_at = Some(ago(600));
        main.runtime_owner = Some(RuntimeOwner::new(
            RuntimeOwnerKind::Agent,
            "main-sess",
            9_999,
            None,
        ));
        main.origin = main_origin;

        let mut replacement = agent("codex", "new-sess", AgentStatus::Running, 2_000)
            .worktree("/repo/main")
            .in_pane("%1")
            .active_ago(5);
        replacement.registered_at = Some(ago(60));
        replacement.runtime_owner = Some(RuntimeOwner::new(
            RuntimeOwnerKind::Agent,
            "new-sess",
            9_999,
            None,
        ));
        replacement.origin = replacement_origin;

        let mut snapshot = room(vec![main, replacement]);
        snapshot.reap_stale_sessions();
        let snapshot = snapshot.with_live_panes(vec![pane("%1", "codex", "/repo/main")], None);

        let rows = rows(&snapshot);
        assert_eq!(rows.len(), 1, "{label}");
        assert!(rows[0].is_agent(), "{label}");
        assert_eq!(rows[0].id, expected, "{label}");
    }
}

#[test]
fn relaunch_in_reused_cwd_still_takes_over_the_card() {
    // The earliest-registered preference must not over-pin. A genuine relaunch
    // is a NEW process whose start postdates the dead predecessor's last
    // activity, so the process-start guard evicts the old session before the
    // primary tiebreak runs — and the fresh session binds even though the dead
    // one registered earlier.
    let live = PaneRef {
        pane_process_start: Some(ago(30)),
        ..pane("%1", "codex", "/repo/main")
    };

    let mut dead = agent("codex", "old-sess", AgentStatus::Success, 1_000)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(120); // predates the new process start
    dead.registered_at = Some(ago(600)); // earliest-registered, yet evicted

    let mut fresh = agent("codex", "new-sess", AgentStatus::Running, 2_000)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(5); // postdates the new process start
    fresh.registered_at = Some(ago(20));

    let snapshot = room(vec![dead, fresh]).with_live_panes(vec![live], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_agent());
    assert_eq!(
        rows[0].id, "new-sess",
        "a relaunch (new process) takes over even though the dead session registered earlier",
    );
}

#[test]
fn agent_binds_only_by_stamped_pane_id() {
    // The pane-keyed invariant: an agent stamped `%2`, but only `%1` is
    // live. `%1`'s command and cwd both match the agent — under the old
    // command/cwd fallback it would have bound. Stamped-id binding refuses
    // it, so `%1` stays a process row and the agent simply does not render.
    let claude = agent("claude", "sess-1", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%2");

    let snapshot =
        room(vec![claude]).with_live_panes(vec![pane("%1", "claude", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_process());
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "%1");
}

#[test]
fn live_agent_and_process_rows_are_pane_backed() {
    // In a live-pane fold, every visible top-level row is jumpable: agent
    // rows and process rows both carry a pane. A subagent that shares its
    // parent's pane nests in the parent card instead of becoming a second
    // top-level row with the same pane.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    let child = child_state("sess-root", "child-1", AgentStatus::Running, 5)
        .worktree("/repo/main")
        .in_pane("%1");

    let snapshot = room(vec![parent, child]).with_live_panes(
        vec![
            pane("%1", "claude", "/repo/main"),
            pane("%2", "zsh", "/repo/main"),
        ],
        None,
    );

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 2, "root agent + process pane render two rows");
    assert!(
        rows.iter().all(|row| row.pane.is_some()),
        "every visible live-pane row has a pane: {rows:?}",
    );
    assert_eq!(
        row_identity_violations(rows.iter().copied()),
        Vec::<String>::new()
    );
    assert!(
        rows.iter().all(|row| row.id != "child-1"),
        "the subagent is not a top-level row",
    );
    let parent = rows
        .iter()
        .find(|row| row.id == "sess-root")
        .expect("parent row present");
    assert_eq!(parent.sub_agents().len(), 1);
    assert_eq!(parent.sub_agents()[0].id, "child-1");
}

#[test]
fn live_launched_child_promotes_when_its_parent_has_no_row() {
    let mut child = agent("codex", "child-1", AgentStatus::Running, 1_000)
        .worktree("/repo/child")
        .in_pane("%2");
    child.parent_agent_id = Some("parent-gone".into());
    child.parent_agent_kind = Some(AgentKind::new_unchecked("claude"));
    child.launch_depth = Some(1);

    let snapshot =
        room(vec![child]).with_live_panes(vec![pane("%2", "codex", "/repo/child")], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "child-1");
    assert!(rows[0].is_agent());
    assert_eq!(
        rows[0].pane.as_ref().map(|pane| pane.pane_id.raw()),
        Some("%2")
    );
    assert_eq!(
        snapshot
            .agent_panes
            .iter()
            .find_map(|pane| pane.agent_id.as_deref()),
        Some("child-1"),
        "promotion preserves pane addressing"
    );
}
