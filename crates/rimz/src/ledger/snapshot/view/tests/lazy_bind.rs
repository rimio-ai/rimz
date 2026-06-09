use super::*;

// ── The lazy-codex bind: cwd fallback and idle synthesis ─────────────────────

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
        let snapshot = room(
            Vec::new(),
            vec![paneless_codex("sess-1", agent_worktree, 1_000)],
        )
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
fn paneless_claude_predating_pane_start_does_not_bind() {
    let stale = agent("claude", "sess-old", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .active_ago(60);
    let fresh_pane = PaneRef {
        pane_process_start: Some(epoch()),
        elevated_agent: None,
        ..pane("term1", "claude", "/repo/main")
    };
    let snapshot = room(Vec::new(), vec![stale]).with_live_panes(vec![fresh_pane], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_process());
    assert_eq!(rows[0].name, "claude");
}

#[test]
fn paneless_claude_does_not_bind_a_bare_node_pane() {
    // A bare node process is ambiguous; only a command that classifies as
    // Claude can recover the paneless session.
    let claude = agent("claude", "sess-1", AgentStatus::Running, 1_000).worktree("/repo/main");
    let snapshot = room(Vec::new(), vec![claude])
        .with_live_panes(vec![pane("term1", "node", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_process());
    assert_eq!(rows[0].name, "node");
}

#[test]
fn stamped_claude_wins_over_paneless_cwd_recovery() {
    let paneless = agent("claude", "paneless", AgentStatus::Running, 1_000).worktree("/repo/main");
    let stamped = agent("claude", "stamped", AgentStatus::Idle, 2_000)
        .worktree("/repo/main")
        .in_pane("term1");
    let snapshot = room(Vec::new(), vec![paneless, stamped])
        .with_live_panes(vec![pane("term1", "claude", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_agent());
    assert_eq!(rows[0].id, "stamped");
}

#[test]
fn elevated_foreign_claude_marker_blocks_cwd_recovery() {
    let claude = agent("claude", "sess-1", AgentStatus::Running, 1_000).worktree("/repo/main");
    let mut pane = pane("term1", "sudo claude", "/repo/main");
    pane.elevated_agent = Some(crate::feed::ElevatedAgent {
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
fn paneless_codex_predating_pane_start_does_not_bind() {
    // The defensive guard on the cwd fallback: when the backend reports the
    // pane's process start, a pane-less Codex session whose last activity
    // predates it belongs to an older instance that once ran in this worktree,
    // not the process now in the pane. A daemon-mode session records the shared
    // daemon pid, so process liveness can't tell the stale one from the live
    // one — so the bind is refused and the fresh pane stays a process row until
    // its own session reports.
    let stale = paneless_codex("sess-old", "/repo/main", 1_000).active_ago(60);
    let fresh_pane = PaneRef {
        pane_process_start: Some(epoch()),
        elevated_agent: None,
        ..pane("term1", "codex", "/repo/main")
    };
    let snapshot = room(Vec::new(), vec![stale]).with_live_panes(vec![fresh_pane], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].is_process(),
        "a session predating the pane start must not bind it",
    );
}

#[test]
fn paneless_codex_active_after_pane_start_binds() {
    // The guard never over-blocks: a session whose last activity is at or after
    // the pane's process start is the live occupant and binds normally.
    let live = paneless_codex("sess-1", "/repo/main", 1_000).active_ago(-5);
    let started_pane = PaneRef {
        pane_process_start: Some(epoch()),
        elevated_agent: None,
        ..pane("term1", "codex", "/repo/main")
    };
    let snapshot = room(Vec::new(), vec![live]).with_live_panes(vec![started_pane], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_agent());
    assert_eq!(rows[0].id, "sess-1");
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "term1");
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
fn fresh_codex_pane_with_proc_start_shows_idle_not_ghost() {
    // The ghost-stats regression. A completed daemon-mode Codex session lingers
    // in the rollup — its owner is the shared, still-alive app-server daemon, so
    // process liveness can never reap it, and the daemon still holds the thread
    // loaded so the loaded-set reap keeps it too. A fresh `codex` then starts in
    // the same worktree. On Zellij the backend reports no pane process start, so
    // the producer stamps the in-pane CLI's `/proc` start; fed that, the guard
    // refuses the stale session and the wired pane renders the synthesized idle
    // row (`○ codex`) — not yesterday's `success` stats — until its own first
    // turn binds a new session.
    let mut ghost = paneless_codex("sess-old", "/repo/main", 1_000).active_ago(12 * 60 * 60);
    ghost.status = AgentStatus::Success;
    ghost.total_tokens = Some(126_621);
    ghost.model = Some("gpt-5.5".to_owned());
    let mut snapshot = room(Vec::new(), vec![ghost]);
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
    let fresh_pane = PaneRef {
        pane_process_start: Some(epoch()),
        elevated_agent: None,
        ..pane("term1", "codex", "/repo/main")
    };
    let snapshot = snapshot.with_live_panes(vec![fresh_pane], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_agent());
    assert_eq!(rows[0].status(), Some(AgentStatus::Idle));
    // The synthesized idle row keys on the pane id, never the stale session, and
    // carries none of its stats.
    assert_eq!(rows[0].id, "tmux:term1");
    assert_ne!(rows[0].id, "sess-old");
    assert_eq!(
        rows[0].total_tokens(),
        None,
        "no ghost tokens on a fresh pane"
    );
    assert_eq!(
        rows[0].model(),
        Some("GPT-5.5"),
        "fresh Codex rows use the provider fallback model, not stale session stats"
    );
    assert_eq!(
        rows[0].context_window(),
        Some(258_000),
        "fresh Codex rows use the provider fallback window, not stale session stats"
    );
}

#[test]
fn stale_stamped_codex_predating_reused_pane_start_shows_idle_in_live_worktree() {
    // A recovered focus stamp is still just a pane id. After a mux rebirth, a
    // stale daemon-routed Codex session from one worktree can carry the same id
    // as a fresh Codex pane in another worktree. The lazy stamped path must use
    // the same process-start guard as the cwd fallback so the old tenant does
    // not capture the new pane and group under the old checkout.
    let mut ghost = paneless_codex("sess-old", "/repo/main", 1_000)
        .in_pane("term1")
        .active_ago(12 * 60 * 60);
    ghost.status = AgentStatus::Success;
    ghost.prompt = Some("does using sidebar plugin increase performance?".to_owned());
    ghost.total_tokens = Some(126_621);
    let mut snapshot = room(Vec::new(), vec![ghost]);
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
    let fresh_pane = PaneRef {
        pane_process_start: Some(epoch()),
        elevated_agent: None,
        ..pane("term1", "codex", "/repo/hook-trace")
    };

    let snapshot = snapshot.with_live_panes(vec![fresh_pane], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    assert_eq!(
        snapshot.worktree_groups[0].label, "hook-trace",
        "the row groups by the live pane cwd, not the ghost's worktree",
    );
    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1, "one live pane yields one row: {rows:?}");
    assert!(rows[0].is_agent());
    assert_eq!(rows[0].id, "tmux:term1");
    assert_eq!(rows[0].name, "codex");
    assert_eq!(rows[0].status(), Some(AgentStatus::Idle));
    assert_eq!(rows[0].worktree_path.as_deref(), Some("/repo/hook-trace"));
    assert_eq!(
        rows[0].total_tokens(),
        None,
        "the reused pane must not inherit stale session stats",
    );
}

#[test]
fn wired_unprompted_codex_pane_renders_as_idle_agent() {
    // Codex registers its session lazily — `SessionStart` rides in with the
    // first prompt — so a launched-but-never-prompted `codex` pane has no
    // agent state. When Codex is wired it must read as an idle agent (`○ codex`
    // with its gauge and a cockpit tally), not a bare, dim process row, the
    // moment it opens.
    let mut snapshot = room(Vec::new(), Vec::new());
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
    let snapshot = snapshot.with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_agent());
    assert_eq!(rows[0].name, "codex");
    assert_eq!(rows[0].status(), Some(AgentStatus::Idle));
    // No session id exists yet, so the row keys on the pane id (its full
    // mux-qualified form, as `row_from_process` does).
    assert_eq!(rows[0].id, "tmux:term1");
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "term1");
    assert_eq!(
        rows[0].model(),
        Some("GPT-5.5"),
        "the card can show Codex's default model before the first session event"
    );
    assert_eq!(
        rows[0].context_window(),
        Some(258_000),
        "the card can show Codex's context tier before the first session event"
    );
}

#[test]
fn wired_unprompted_supervised_codex_pane_renders_as_idle_agent() {
    // `rimz agents --worktree` launches Codex through the hidden cleanup
    // wrapper. During the pre-session window the pane command is still that
    // wrapper, but it is semantically a Codex pane and must take the same idle
    // lazy-agent path as a bare `codex` command.
    let mut snapshot = room(Vec::new(), Vec::new());
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
    let snapshot = snapshot.with_live_panes(
        vec![pane(
            "term1",
            "/home/me/.cargo/bin/rimz agents exec codex --worktree-path /repo/main",
            "/repo/main",
        )],
        None,
    );

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_agent());
    assert_eq!(rows[0].name, "codex");
    assert_eq!(rows[0].status(), Some(AgentStatus::Idle));
    assert_eq!(rows[0].id, "tmux:term1");
}

#[test]
fn supervised_codex_uses_wrapper_worktree_path_when_cwd_is_missing() {
    // A first mux read can know the command before it knows the pane cwd. The
    // wrapper carries the same worktree path Rimz used to launch the pane, so
    // use it as a grouping fallback instead of flashing the row under external.
    let root = "/repo/rimz";
    let worktree = "/repo/rimz/.claude/worktrees/feature-x";
    let mut pane = pane(
        "term1",
        &format!("/home/me/.cargo/bin/rimz agents exec codex --worktree-path {worktree}"),
        "/ignored",
    );
    pane.cwd = None;

    let mut snapshot = room(Vec::new(), Vec::new()).with_project_root(Some(PathBuf::from(root)));
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
    let snapshot = snapshot.with_live_panes(vec![pane], None);

    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Worktree);
    assert_eq!(group.key, worktree);
    assert!(group.rows[0].is_agent());
    assert_eq!(group.rows[0].name, "codex");
    assert_eq!(group.rows[0].worktree_path.as_deref(), Some(worktree));
}

#[test]
fn unwired_supervised_codex_process_row_uses_wrapper_worktree_path_when_cwd_is_empty() {
    // Empty-string cwd is the same mux race as a missing cwd. An unwired Codex
    // pane falls through to the process row, so that path must share the same
    // wrapper manifest fallback as the lazy-agent path.
    let root = "/repo/rimz";
    let worktree = "/repo/rimz/.claude/worktrees/feature-x";
    let mut pane = pane(
        "term1",
        &format!("/home/me/.cargo/bin/rimz agents exec codex --worktree-path {worktree}"),
        "/ignored",
    );
    pane.cwd = Some(String::new());

    let snapshot = room(Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_live_panes(vec![pane], None);

    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Worktree);
    assert_eq!(group.key, worktree);
    assert!(!group.rows[0].is_agent());
    assert_eq!(group.rows[0].name, "codex");
    assert_eq!(group.rows[0].worktree_path.as_deref(), Some(worktree));
}

#[test]
fn wired_unprompted_codex_uses_configured_default_model() {
    let mut snapshot = room(Vec::new(), Vec::new());
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
    snapshot
        .lazy_agent_default_models
        .insert("codex".to_owned(), "o4-mini".to_owned());
    let snapshot = snapshot.with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert!(row.is_agent());
    assert_eq!(row.model(), Some("o4-mini"));
    assert_eq!(row.context_window(), Some(258_000));
}

#[test]
fn non_lazy_agent_pane_is_never_idle_synthesized() {
    // The idle-instance synthesis is gated on the agent registering lazily
    // (`Capabilities::registers_lazily`), not merely on being wired. Claude
    // stamps a pane on every session, so an unbound `claude` pane stays a
    // process row even when the producer is told claude is a wired lazy kind —
    // the static descriptor gate refuses it. This is what keeps the lifecycle
    // agent-agnostic (a new lazy agent slots in by declaring the capability)
    // without changing how Claude renders.
    let mut snapshot = room(Vec::new(), Vec::new());
    snapshot.wired_lazy_kinds = vec!["claude".to_owned(), "codex".to_owned()];
    let snapshot = snapshot.with_live_panes(vec![pane("term1", "claude", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_process());
}

#[test]
fn unwired_codex_pane_stays_a_process_row() {
    // The consent invariant: an unwired Codex can report no status, so its
    // live pane stays a process row (agents are invisible until their hooks
    // are wired). `wired_lazy_kinds` left empty reproduces an un-onboarded
    // Codex.
    let snapshot = room(Vec::new(), Vec::new())
        .with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_process());
    assert_eq!(rows[0].name, "codex");
}

#[test]
fn bound_codex_pane_keeps_its_real_agent_over_idle_synthesis() {
    // The idle synthesis is a last resort: a `codex` pane that binds a real
    // (pane-less, cwd-matched) agent keeps that agent's identity and status,
    // never the synthesized idle row — even with Codex wired.
    let mut snapshot = room(
        Vec::new(),
        vec![paneless_codex("sess-1", "/repo/main", 1_000)],
    );
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
    let snapshot = snapshot.with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_agent());
    assert_eq!(
        rows[0].id, "sess-1",
        "the real agent binds, not a synthesis"
    );
    assert_eq!(rows[0].status(), Some(AgentStatus::Running));
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
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
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

#[test]
fn unbound_claude_pane_stays_a_process_row_even_when_codex_wired() {
    // The synthesis is Codex-only: Claude always stamps a live pane, so a
    // `claude` pane with no bound agent is a genuinely-ended session and must
    // read as a process row, never an idle agent — regardless of Codex wiring.
    let mut snapshot = room(Vec::new(), Vec::new());
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
    let snapshot = snapshot.with_live_panes(vec![pane("term1", "claude", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_process());
    assert_eq!(rows[0].name, "claude");
}

// ── Stale asks vs live presence ──────────────────────────────────────────────

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
