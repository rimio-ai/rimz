use super::*;

#[test]
fn paneless_codex_cwd_fallback_binds_only_the_exact_codex_worktree_pane() {
    for case in [
        (
            "same worktree binds as agent",
            "/repo/main",
            "codex",
            "/repo/main",
            true,
        ),
        (
            "other worktree stays process",
            "/repo/other",
            "codex",
            "/repo/main",
            false,
        ),
        (
            "nested worktree stays process",
            "/repo",
            "codex",
            "/repo/sub",
            false,
        ),
        (
            "non-codex pane stays process",
            "/repo/main",
            "zsh",
            "/repo/main",
            false,
        ),
    ] {
        let (label, agent_worktree, pane_command, pane_cwd, expect_agent) = case;
        let snapshot = room(vec![paneless_codex("sess-1", agent_worktree, 1_000)])
            .with_live_panes(vec![pane("term1", pane_command, pane_cwd)], None);

        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1, "{label}");
        assert_eq!(rows[0].is_agent(), expect_agent, "{label}");
        assert_eq!(rows[0].is_process(), !expect_agent, "{label}");
        if expect_agent {
            assert_eq!(rows[0].name, "codex", "{label}");
            assert_eq!(rows[0].id, "sess-1", "{label}");
        }
        assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "term1");
    }
}

#[test]
fn supervised_codex_uses_wrapper_worktree_path_for_idle_and_process_rows() {
    // A first mux read can know the command before it knows the pane cwd. The
    // wrapper carries the same worktree path Rimz used to launch the pane, so
    // both the idle-agent and process-row paths use it as their grouping
    // fallback instead of flashing the row under external.
    let root = "/repo/rimz";
    let worktree = "/repo/rimz/.claude/worktrees/feature-x";

    for (label, cwd, wired_agent, expect_agent) in [
        ("missing cwd renders idle agent", None, true, true),
        (
            "empty cwd renders process row",
            Some(String::new()),
            false,
            false,
        ),
    ] {
        let mut pane = pane(
            "term1",
            &format!("/home/me/.cargo/bin/rimz agents exec codex --worktree-path {worktree}"),
            "/ignored",
        );
        pane.cwd = cwd;

        let mut snapshot = room(Vec::new()).with_project_root(Some(PathBuf::from(root)));
        if wired_agent {
            snapshot.wired_kinds = vec!["codex".to_owned()];
        }
        let snapshot = snapshot.with_live_panes(vec![pane], None);

        let group = &snapshot.worktree_groups[0];
        assert_eq!(group.kind, SidebarWorktreeKind::Worktree, "{label}");
        assert_eq!(group.key, worktree, "{label}");
        assert_eq!(group.rows[0].is_agent(), expect_agent, "{label}");
        assert_eq!(group.rows[0].name, "codex", "{label}");
        assert_eq!(
            group.rows[0].worktree_path.as_deref(),
            Some(worktree),
            "{label}",
        );
    }
}
