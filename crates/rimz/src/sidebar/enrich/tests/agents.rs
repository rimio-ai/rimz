//! Agent-row admission during the fold: codex daemon reaping, cleared-session
//! collapse, and the producer's lazy-pairing binding log.

use super::*;

#[test]
fn cached_enrich_binds_reaped_codex_clear_session() {
    let (_dir, runtime, _) = runtime();
    let now = Timestamp::now();
    let mut old = codex_root("old", "/repo/main", "terminal_1");
    old.status = AgentStatus::Success;
    old.last_activity = now - SignedDuration::from_secs(120);
    old.origin = Some(SessionOrigin::Fresh);
    let mut new = codex_root("new", "/repo/main", "terminal_1");
    new.last_activity = now - SignedDuration::from_secs(60);
    new.origin = Some(SessionOrigin::Fresh);
    let mut snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(Path::new("/tmp/enrich")),
        vec![old, new],
        now,
    );
    snapshot.reap_stale_sessions();
    let frame = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_1", "codex", "/repo/main")],
        1_000,
        "rimz-test",
    );

    let snapshot = fold_cached(snapshot, Some(&frame), &runtime);

    assert_eq!(
        snapshot
            .agents
            .iter()
            .map(|agent| agent.agent_id.as_str())
            .collect::<Vec<_>>(),
        vec!["new"]
    );
}

#[test]
fn cached_enrich_uses_published_codex_daemon_reap_inputs() {
    let (_dir, runtime_paths, _) = runtime();
    let mut closed = root_agent("codex", "closed", None);
    closed.runtime_owner = Some(RuntimeOwner::new(
        RuntimeOwnerKind::Agent,
        "closed",
        77,
        None,
    ));
    let mut open = root_agent("codex", "open", None);
    open.runtime_owner = Some(RuntimeOwner::new(RuntimeOwnerKind::Agent, "open", 77, None));
    let snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(Path::new("/tmp/enrich")),
        vec![closed, open],
        Timestamp::now(),
    );
    write_codex_daemon_reap(
        &runtime_paths,
        &CodexDaemonReap {
            produced_at_ms: 1_000,
            daemon_pids: BTreeSet::from([77]),
            loaded: Some(BTreeSet::from(["open".to_owned()])),
        },
    )
    .unwrap();

    let snapshot = fold_cached(snapshot, None, &runtime_paths);

    assert_eq!(
        snapshot
            .agents
            .iter()
            .map(|agent| agent.agent_id.as_str())
            .collect::<Vec<_>>(),
        vec!["open"]
    );

    let (_empty_dir, empty_runtime, _) = runtime();
    let mut kept = root_agent("codex", "kept", None);
    kept.runtime_owner = Some(RuntimeOwner::new(RuntimeOwnerKind::Agent, "kept", 77, None));
    let snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(Path::new("/tmp/enrich")),
        vec![kept],
        Timestamp::now(),
    );
    let snapshot = fold_cached(snapshot, None, &empty_runtime);
    assert_eq!(snapshot.agents.len(), 1, "absent cache reaps nothing");
}

#[test]
fn project_lane_enrich_reads_stale_codex_daemon_reap_without_rewriting() {
    let (_dir, runtime_paths, _) = runtime();
    write_codex_daemon_reap(
        &runtime_paths,
        &CodexDaemonReap {
            produced_at_ms: 1,
            daemon_pids: BTreeSet::new(),
            loaded: None,
        },
    )
    .unwrap();
    let snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(Path::new("/tmp/enrich")),
        vec![root_agent("codex", "pane-less", None)],
        Timestamp::now(),
    );

    let _ = fold_producing(snapshot, None, &runtime_paths);

    assert_eq!(
        read_codex_daemon_reap(&runtime_paths)
            .expect("codex reap cache")
            .produced_at_ms,
        1
    );
}

#[test]
fn producer_binding_log_dedups_unchanged_lazy_pairing_ambiguity() {
    let (_dir, runtime, snapshot) = runtime();
    let worktree = "/repo/main";
    let mut agent = root_agent("codex", "lazy-session", None);
    agent.worktree_path = Some(worktree.to_owned());
    let snapshot = SidebarSnapshot::build_with_agents(
        snapshot.workspace_id.clone(),
        vec![agent.clone()],
        Timestamp::now(),
    );
    let frame = crate::sidebar::frame::assemble_frame(
        vec![
            pane("terminal_1", "codex", worktree),
            pane("terminal_2", "codex", worktree),
        ],
        1_000,
        "rimz-test",
    );

    let _ = fold_producing(snapshot.clone(), Some(&frame), &runtime);
    let _ = fold_producing(snapshot.clone(), Some(&frame), &runtime);

    assert_eq!(binding_log_lines(&runtime), 1);

    let mut active_agent = agent.clone();
    active_agent.last_activity += SignedDuration::from_secs(1);
    let active_snapshot = SidebarSnapshot::build_with_agents(
        snapshot.workspace_id.clone(),
        vec![active_agent],
        Timestamp::now(),
    );
    let _ = fold_producing(active_snapshot, Some(&frame), &runtime);

    assert_eq!(binding_log_lines(&runtime), 1);

    let mut later_pane = pane("terminal_2", "codex", worktree);
    later_pane.pane_process_start =
        Some(agent.registered_at.unwrap_or(agent.last_activity) - SignedDuration::from_secs(1));
    let changed_frame = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_1", "codex", worktree), later_pane],
        1_000,
        "rimz-test",
    );
    let _ = fold_producing(snapshot, Some(&changed_frame), &runtime);

    assert_eq!(binding_log_lines(&runtime), 2);
}
