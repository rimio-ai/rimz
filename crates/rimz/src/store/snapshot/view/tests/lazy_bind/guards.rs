use super::*;

#[test]
fn paneless_agent_process_start_guard_controls_cwd_recovery() {
    for (label, kind, active_secs_ago, expect_bind) in [
        ("stale Claude predates pane start", "claude", 60, false),
        ("stale Codex predates pane start", "codex", 60, false),
        ("fresh Claude follows pane start", "claude", -5, true),
        ("fresh Codex follows pane start", "codex", -5, true),
    ] {
        let session = match kind {
            "codex" => paneless_codex("sess-1", "/repo/main", 1_000).active_ago(active_secs_ago),
            "claude" => agent("claude", "sess-1", AgentStatus::Running, 1_000)
                .worktree("/repo/main")
                .active_ago(active_secs_ago),
            _ => unreachable!("test matrix names known agent kinds"),
        };
        let live_pane = PaneRef {
            pane_process_start: Some(epoch()),
            elevated_agent: None,
            ..pane("term1", kind, "/repo/main")
        };
        let snapshot = room(Vec::new(), vec![session]).with_live_panes(vec![live_pane], None);

        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1, "{label}");
        assert_eq!(rows[0].is_agent(), expect_bind, "{label}");
        assert_eq!(rows[0].is_process(), !expect_bind, "{label}");
        if expect_bind {
            assert_eq!(rows[0].id, "sess-1", "{label}");
            assert_eq!(
                rows[0].pane.as_ref().unwrap().pane_id.raw(),
                "term1",
                "{label}"
            );
        } else {
            assert_eq!(rows[0].name, kind, "{label}");
        }
    }
}

#[test]
fn elevated_foreign_claude_marker_blocks_cwd_recovery() {
    let claude = agent("claude", "sess-1", AgentStatus::Running, 1_000).worktree("/repo/main");
    let mut pane = pane("term1", "sudo claude", "/repo/main");
    pane.elevated_agent = Some(crate::pane::ElevatedAgent {
        kind: crate::ids::AgentKind::new_unchecked("claude"),
        uid: 0,
    });
    let snapshot = room(Vec::new(), vec![claude]).with_live_panes(vec![pane], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_process());
    assert_eq!(rows[0].name, "claude");
    assert!(
        rows[0]
            .as_process()
            .and_then(|process| process.foreign_user.as_deref())
            .is_some(),
        "foreign-user marker stays on the process card"
    );
    assert!(snapshot.worktree_groups[0].status_counts.is_empty());
}

#[test]
fn stale_codex_ghosts_predating_pane_start_render_idle_live_pane() {
    for (label, stamped, live_worktree, expected_group) in [
        (
            "paneless ghost in same worktree",
            false,
            "/repo/main",
            "main",
        ),
        (
            "stamped ghost in reused pane",
            true,
            "/repo/hook-trace",
            "hook-trace",
        ),
    ] {
        let mut ghost = paneless_codex("sess-old", "/repo/main", 1_000).active_ago(12 * 60 * 60);
        if stamped {
            ghost = ghost.in_pane("term1");
            ghost.prompt = Some("does using sidebar plugin increase performance?".to_owned());
        }
        ghost.status = AgentStatus::Success;
        ghost.total_tokens = Some(126_621);
        ghost.model = Some("gpt-5.5".to_owned());

        let mut snapshot = room(Vec::new(), vec![ghost]);
        snapshot.wired_kinds = vec!["codex".to_owned()];
        let fresh_pane = PaneRef {
            pane_process_start: Some(epoch()),
            elevated_agent: None,
            ..pane("term1", "codex", live_worktree)
        };
        let snapshot = snapshot.with_live_panes(vec![fresh_pane], None);

        assert_eq!(snapshot.worktree_groups.len(), 1, "{label}");
        assert_eq!(
            snapshot.worktree_groups[0].label, expected_group,
            "the row groups by the live pane cwd, not the ghost's worktree: {label}",
        );
        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1, "one live pane yields one row: {label}");
        assert!(rows[0].is_agent(), "{label}");
        assert_eq!(rows[0].id, "tmux:term1", "{label}");
        assert_ne!(rows[0].id, "sess-old", "{label}");
        assert_eq!(rows[0].name, "codex", "{label}");
        assert_eq!(rows[0].status(), Some(AgentStatus::Idle), "{label}");
        assert_eq!(
            rows[0].worktree_path.as_deref(),
            Some(live_worktree),
            "{label}"
        );
        assert_eq!(
            rows[0].total_tokens(),
            None,
            "the live pane must not inherit stale session stats: {label}",
        );
        assert_eq!(
            rows[0].model(),
            Some("gpt-5.5-codex"),
            "fresh Codex rows use the provider fallback model, not stale session stats: {label}",
        );
        assert_eq!(
            rows[0].context_window(),
            Some(272_000),
            "fresh Codex rows use the provider fallback window, not stale session stats: {label}",
        );
    }
}
