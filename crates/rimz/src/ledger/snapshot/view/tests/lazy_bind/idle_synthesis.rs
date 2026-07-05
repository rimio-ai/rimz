use super::*;

#[test]
fn wired_unprompted_codex_panes_render_idle_agent_rows() {
    for (label, command, configured_model, expected_model) in [
        ("bare codex command", "codex", None, "gpt-5.5-codex"),
        (
            "supervised wrapper command",
            "/home/me/.cargo/bin/rimz agents exec codex --worktree-path /repo/main",
            None,
            "gpt-5.5-codex",
        ),
        (
            "configured default model",
            "codex",
            Some("o4-mini"),
            "o4-mini",
        ),
    ] {
        let mut snapshot = room(Vec::new(), Vec::new());
        snapshot.wired_kinds = vec!["codex".to_owned()];
        if let Some(model) = configured_model {
            snapshot
                .wired_default_models
                .insert("codex".to_owned(), model.to_owned());
        }
        let snapshot = snapshot.with_live_panes(vec![pane("term1", command, "/repo/main")], None);

        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1, "{label}");
        assert!(rows[0].is_agent(), "{label}");
        assert_eq!(rows[0].name, "codex", "{label}");
        assert_eq!(rows[0].status(), Some(AgentStatus::Idle), "{label}");
        assert_eq!(rows[0].id, "tmux:term1", "{label}");
        assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "term1");
        assert_eq!(rows[0].model(), Some(expected_model), "{label}");
        assert_eq!(rows[0].context_window(), Some(272_000), "{label}");
    }
}

#[test]
fn idle_synthesis_gates_leave_unqualified_panes_as_process_rows() {
    for (label, command, wired_kinds) in [
        ("unwired codex remains a process", "codex", Vec::new()),
        (
            "unbound claude remains a process while codex is wired",
            "claude",
            vec!["codex".to_owned()],
        ),
    ] {
        let mut snapshot = room(Vec::new(), Vec::new());
        snapshot.wired_kinds = wired_kinds;
        let snapshot = snapshot.with_live_panes(vec![pane("term1", command, "/repo/main")], None);

        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1, "{label}");
        assert!(rows[0].is_process(), "{label}");
        assert_eq!(rows[0].name, command, "{label}");
    }
}

#[test]
fn two_codex_panes_one_agent_yields_one_real_one_idle() {
    // The multi-codex-per-worktree case: one prompted (pane-less) agent plus a
    // second still-unprompted `codex` pane in the same worktree. The agent
    // binds the first codex pane by cwd; the second synthesizes an idle row —
    // no codex pane is ever left as a process row.
    let mut snapshot = room(
        Vec::new(),
        vec![paneless_codex("sess-1", "/repo/main", 1_000)],
    );
    snapshot.wired_kinds = vec!["codex".to_owned()];
    let snapshot = snapshot.with_live_panes(
        vec![
            pane("term1", "codex", "/repo/main"),
            pane("term2", "codex", "/repo/main"),
        ],
        None,
    );

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter().all(|row| row.is_agent()),
        "neither codex pane is a process row",
    );
    assert!(
        rows.iter().any(|row| row.id == "sess-1"),
        "the prompted session binds one pane",
    );
    assert!(
        rows.iter()
            .any(|row| row.status() == Some(AgentStatus::Idle)),
        "the unprompted pane synthesizes an idle row",
    );
}

// ── Stale asks vs live presence ──────────────────────────────────────────────
