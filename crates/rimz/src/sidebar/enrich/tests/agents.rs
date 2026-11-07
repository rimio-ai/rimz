//! Agent-row admission during the fold: codex daemon reaping, cleared-session
//! collapse, and the producer's lazy-pairing binding log.

use super::*;

#[test]
fn due_messages_project_every_sender_to_its_card_and_clear_when_queue_is_missing() {
    use crate::store::message::{
        DeliveryGate, HarnessNotice, MessageRecord, MessageSender, MessageStatus,
    };
    let (_dir, runtime, snapshot) = runtime();
    let state = StatePaths::under(runtime.workspace_id.clone(), &runtime.root).unwrap();
    state.ensure_dirs().unwrap();
    std::fs::create_dir_all(&state.messages_dir).unwrap();
    let mut first = root_agent("claude", "first", None);
    first.name = Some("coder".to_owned());
    let mut second = root_agent("codex", "second", None);
    let panes = vec![
        pane("terminal_1", "claude", "/repo/main"),
        pane("terminal_2", "codex", "/repo/main"),
    ];
    first.pane = Some(panes[0].clone());
    second.pane = Some(panes[1].clone());
    let frame = crate::sidebar::frame::assemble_frame(panes, 1_000, "rimz-test");
    let mut provisional = first.clone();
    provisional.agent_id = "launch_1".into();
    let senders = [
        MessageSender::Human,
        MessageSender::Agent {
            kind: second.kind.clone(),
            agent_id: None,
            name: None,
            profile: None,
            role: None,
            channel: None,
        },
        MessageSender::Harness {
            notice: HarnessNotice::Wait,
        },
        MessageSender::System,
    ];
    let mut records = Vec::new();
    for (index, sender) in senders.into_iter().enumerate() {
        let mut record = MessageRecord::new(
            snapshot.workspace_id.clone(),
            &provisional,
            "queued".to_owned(),
            DeliveryGate::Resume,
        )
        .with_sender(sender);
        if index == 1 {
            record.status = MessageStatus::Claimed;
        }
        records.push(record);
    }
    records.push(MessageRecord::new(
        snapshot.workspace_id.clone(),
        &second,
        "other".to_owned(),
        DeliveryGate::Any,
    ));
    let queue = records
        .iter()
        .map(|record| serde_json::to_string(record).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(
        state.messages_dir.join("messages.jsonl"),
        format!("{queue}\n"),
    )
    .unwrap();
    let snapshot = SidebarSnapshot::build_with_agents(
        snapshot.workspace_id,
        vec![first, second],
        snapshot.now,
    );
    let counts = |snapshot: &SidebarSnapshot| {
        snapshot
            .agents
            .iter()
            .map(|agent| {
                (
                    agent.due_messages,
                    snapshot
                        .rows()
                        .find(|row| {
                            row.pane.as_ref().is_some_and(|pane| {
                                pane.pane_id == agent.pane.as_ref().unwrap().pane_id
                            })
                        })
                        .unwrap()
                        .as_agent()
                        .unwrap()
                        .due_messages,
                )
            })
            .collect::<Vec<_>>()
    };
    let mut actual = Vec::new();
    for opts in [cached_opts(), producing_opts()] {
        actual.push(counts(&fold(
            snapshot.clone(),
            Some(&frame),
            &runtime,
            opts,
        )));
    }
    std::fs::remove_file(state.messages_dir.join("messages.jsonl")).unwrap();
    actual.push(counts(&fold_cached(snapshot, Some(&frame), &runtime)));
    assert_eq!(
        actual,
        vec![
            vec![(4, 4), (1, 1)],
            vec![(4, 4), (1, 1)],
            vec![(0, 0), (0, 0)]
        ]
    );
}

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
    atomic::write_temp_then_rename_cache(
        &codex_daemon_reap_path(&runtime_paths),
        &CodexDaemonReap {
            produced_at_ms: crate::utils::time::unix_now_ms()
                - crate::sidebar::timing::CODEX_DAEMON_REAP_TTL.as_millis() as u64
                - 1,
            daemon_pids: BTreeSet::from([77]),
            loaded: Some(BTreeSet::from(["open".to_owned()])),
        },
    )
    .unwrap();

    let due = fold_cached(snapshot.clone(), None, &runtime_paths);

    assert_eq!(
        due.agents
            .iter()
            .map(|agent| agent.agent_id.as_str())
            .collect::<Vec<_>>(),
        vec!["open"],
        "a due publication still reaps while its replacement is being probed"
    );

    atomic::write_temp_then_rename_cache(
        &codex_daemon_reap_path(&runtime_paths),
        &CodexDaemonReap {
            produced_at_ms: crate::utils::time::unix_now_ms()
                - crate::sidebar::timing::CODEX_DAEMON_REAP_STALE.as_millis() as u64
                - 1,
            daemon_pids: BTreeSet::from([77]),
            loaded: Some(BTreeSet::from(["open".to_owned()])),
        },
    )
    .unwrap();
    let stale = fold_cached(snapshot, None, &runtime_paths);
    assert_eq!(
        stale
            .agents
            .iter()
            .map(|agent| agent.agent_id.as_str())
            .collect::<Vec<_>>(),
        vec!["closed", "open"],
        "stale cache reaps nothing"
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
    atomic::write_temp_then_rename_cache(
        &codex_daemon_reap_path(&runtime_paths),
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
        atomic::read_json_cache::<Option<CodexDaemonReap>>(&codex_daemon_reap_path(&runtime_paths))
            .expect("codex reap cache")
            .produced_at_ms,
        1
    );
}

#[test]
fn producer_binding_log_dedups_unchanged_lazy_pairing_ambiguity() {
    let (dir, runtime, snapshot) = runtime();
    let state = crate::StatePaths::under(snapshot.workspace_id.clone(), dir.path()).unwrap();
    let store = Store::open(state.clone(), runtime.clone()).unwrap();
    let fold = |snapshot, frame| {
        enrich(
            snapshot,
            Some(frame),
            &state,
            &runtime,
            Some(&store),
            None,
            producing_opts(),
            &crate::diag::DiagSink::disabled(),
        )
    };
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

    let _ = fold(snapshot.clone(), &frame);
    let _ = fold(snapshot.clone(), &frame);

    assert_eq!(binding_log_lines(&state), 1);

    let mut active_agent = agent.clone();
    active_agent.last_activity += SignedDuration::from_secs(1);
    let active_snapshot = SidebarSnapshot::build_with_agents(
        snapshot.workspace_id.clone(),
        vec![active_agent],
        Timestamp::now(),
    );
    let _ = fold(active_snapshot, &frame);

    assert_eq!(binding_log_lines(&state), 1);

    let mut later_pane = pane("terminal_2", "codex", worktree);
    later_pane.pane_process_start =
        Some(agent.registered_at.unwrap_or(agent.last_activity) - SignedDuration::from_secs(1));
    let changed_frame = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_1", "codex", worktree), later_pane],
        1_000,
        "rimz-test",
    );
    let _ = fold(snapshot, &changed_frame);

    assert_eq!(binding_log_lines(&state), 2);
}
