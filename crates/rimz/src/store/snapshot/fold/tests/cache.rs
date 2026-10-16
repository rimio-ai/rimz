use std::time::{Duration, SystemTime};

use super::*;

#[test]
fn pre_workflow_cache_refolds_attempt_identity() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let workflow = crate::agents::WorkflowAttemptRef {
        run: crate::ids::WorkflowRunId::new(),
        seq: 7,
        role: crate::agents::WorkflowAttemptRole::Step,
    };
    event_log::append(
        &paths.events_log,
        &EventEnvelope::new(
            workspace.clone(),
            "session",
            "claude",
            "agent",
            "agent.launched",
            serde_json::json!({"agent_id":"attempt", "agent_name":"sweeper", "workflow":workflow}),
        ),
    )
    .unwrap();
    event_log::append(
        &paths.events_log,
        &EventEnvelope::agent_lifecycle(
            workspace,
            "session",
            "claude",
            "UserPromptSubmit",
            &crate::agents::AgentLifecycleObservation::new(
                Some("attempt".into()),
                crate::agents::LifecycleSignal::TurnStarted { turn_id: None },
            ),
        ),
    )
    .unwrap();
    write_rollup_cache(
        &paths.rollup_cache,
        &RollupCache {
            version: 32,
            extent: event_log::LogExtent {
                generation: 0,
                offset: std::fs::metadata(&paths.events_log).unwrap().len(),
            },
            raw_agents: vec![agent("claude", "attempt", AgentStatus::Running, 0)],
            resume_outcomes: Vec::new(),
            agent_identity: AgentIdentityState::default(),
            saw_session_rebirth: false,
        },
    )
    .unwrap();
    let (_, agents, _) = catch_up_rollup(&paths).unwrap();
    assert_eq!(
        agents
            .iter()
            .find(|agent| agent.agent_id == "attempt")
            .unwrap()
            .workflow
            .as_ref(),
        Some(&workflow)
    );
}

#[test]
fn pre_molt_rollup_cache_refolds_the_succession_link() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    event_log::append(&paths.events_log, &EventEnvelope::new(
        workspace, "session", "claude", "agent", "agent.launched",
        serde_json::json!({"agent_id": "launch_new", "agent_name": "successor", "molted_from": "launch_old"}),
    )).unwrap();
    write_rollup_cache(
        &paths.rollup_cache,
        &RollupCache {
            // The last version written without `molted_from`.
            version: 30,
            extent: event_log::LogExtent {
                generation: 0,
                offset: std::fs::metadata(&paths.events_log).unwrap().len(),
            },
            raw_agents: vec![agent("claude", "launch_new", AgentStatus::Running, 0)],
            resume_outcomes: Vec::new(),
            agent_identity: AgentIdentityState::default(),
            saw_session_rebirth: false,
        },
    )
    .unwrap();
    let (_, agents, _) = catch_up_rollup(&paths).unwrap();
    assert_eq!(
        agents
            .iter()
            .find(|agent| agent.agent_id == "launch_new")
            .unwrap()
            .molted_from
            .as_deref(),
        Some("launch_old")
    );
}

#[test]
fn picker_revive_links_survive_rollup_cache_and_carryover() {
    for carryover in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        for (method, params) in [
            (
                "agent.launched",
                serde_json::json!({"agent_id": "picked-id", "agent_name": "old-handle", "launch_id": "launch_old"}),
            ),
            (
                "agent.launched",
                serde_json::json!({"agent_id": "child-1", "agent_name": "child-handle", "parent_agent_id": "launch_old", "launch_depth": 1, "pane_id": "tmux:%8"}),
            ),
            (
                "agent.lifecycle",
                serde_json::json!({"agent_id": "picked-id", "signal": {"signal": "ended"}}),
            ),
            (
                "agent.launched",
                serde_json::json!({"agent_id": "launch_picker", "agent_name": "new-handle", "launch_id": "launch_picker", "pane_id": "tmux:%7"}),
            ),
            (
                "agent.lifecycle",
                serde_json::json!({"agent_id": "picked-id", "agent_name": "new-handle", "pane_id": "tmux:%7", "signal": {"signal": "registered"}}),
            ),
        ] {
            let event = EventEnvelope::new(
                workspace.clone(),
                "session",
                "claude",
                "agent",
                method,
                params,
            );
            event_log::append(&paths.events_log, &event).unwrap();
        }
        let (cache, agents, _) = catch_up_rollup(&paths).unwrap();
        let resumed_paths = if carryover {
            let resumed =
                StatePaths::under(workspace.clone(), &dir.path().join("resumed")).unwrap();
            resumed.ensure_dirs().unwrap();
            write_carryover(
                &resumed.agents_carryover,
                &EventCarryover {
                    agents,
                    ..EventCarryover::default()
                },
            )
            .unwrap();
            resumed
        } else {
            write_rollup_cache(&paths.rollup_cache, &cache).unwrap();
            paths
        };
        event_log::append(
            &resumed_paths.events_log,
            &lifecycle_at(
                &workspace,
                "claude",
                "UserPromptSubmit",
                "picked-id",
                lifecycle::LifecycleSignal::TurnStarted { turn_id: None },
            ),
        )
        .unwrap();
        let (_, agents, _) = catch_up_rollup(&resumed_paths).unwrap();
        let revived = agents
            .iter()
            .find(|agent| agent.agent_id == "picked-id")
            .unwrap();
        let child = agents
            .iter()
            .find(|agent| agent.agent_id == "child-1")
            .unwrap();
        assert!(child.parent_is(revived), "carryover={carryover}");
        assert_eq!(
            crate::address::launched_parent(&agents, child),
            Some(revived)
        );
        assert_eq!(
            crate::address::launched_children(&agents, revived),
            vec![child]
        );
        assert_eq!(
            revived.prior_launch_ids,
            vec![AgentSessionId::from("launch_old")]
        );
    }
}

#[test]
fn derived_carryover_is_published_only_by_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let paths = StatePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    write_carryover(
        &paths.agents_carryover,
        &EventCarryover {
            agents: vec![agent("claude", "old", AgentStatus::Idle, 1_000)],
            ..EventCarryover::default()
        },
    )
    .unwrap();
    let derived = paths.snapshots_dir.join("carryover.json");
    crate::store::snapshot::build_from(&paths).unwrap();
    assert!(!derived.exists(), "lock-free readers persist nothing");
    crate::store::snapshot::rebuild(&paths).unwrap();
    assert!(derived.exists(), "rebuild publishes the trimmed carryover");
    let first = StampedPath::of(&derived);
    crate::store::snapshot::rebuild(&paths).unwrap();
    assert_eq!(first, StampedPath::of(&derived));
    assert_eq!(prune_carryover(&paths, Duration::ZERO).unwrap(), 1);
    crate::store::snapshot::rebuild(&paths).unwrap();
    assert_ne!(first, StampedPath::of(&derived));
}

#[test]
fn derived_carryover_preserves_the_raw_fold_and_rejects_stale_copies() {
    let dir = tempfile::tempdir().unwrap();
    let paths = StatePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    let runtime =
        crate::disk::paths::RuntimePaths::under(paths.workspace_id.clone(), dir.path()).unwrap();
    let store = crate::Store::open(paths.clone(), runtime).unwrap();
    crate::testkit::fleet::seed_ended_carryover(&store, 3_300).unwrap();
    let mut raw = read_carryover(&paths.agents_carryover).unwrap();
    raw.agents
        .push(agent("claude", "backfill", AgentStatus::Idle, 1_000));
    raw.agents
        .push(agent("claude", "backfill", AgentStatus::Running, 2_000));
    raw.resume_outcomes.push(ResumeOutcome {
        message_id: message_id(1),
        kind: AgentKind::new_unchecked("claude"),
        agent_id: "backfill".into(),
        agent_name: None,
        status: MessageStatus::Delivered,
        enqueued_at: Timestamp::now(),
        updated_at: Timestamp::now(),
    });
    write_carryover(&paths.agents_carryover, &raw).unwrap();
    let durable = fs::read(&paths.agents_carryover).unwrap();
    let expected = workspace_carryover(&paths).unwrap();
    let expected_fold = catch_up_rollup(&paths).unwrap();
    let expected_snapshot = crate::store::snapshot::build_from(&paths).unwrap();
    crate::store::snapshot::rebuild(&paths).unwrap();
    let derived = paths.snapshots_dir.join("carryover.json");
    assert!(derived.exists(), "rebuild publishes the fold-ready copy");
    let valid = fs::read(&derived).unwrap();
    assert!(valid.len() < durable.len());
    let cold_read = || {
        let paths = paths.clone();
        std::thread::spawn(move || {
            let before = testkit::carryover_bytes_parsed();
            let base = workspace_carryover(&paths).unwrap();
            let folded = catch_up_rollup(&paths).unwrap();
            let snapshot = crate::store::snapshot::build_from(&paths).unwrap();
            (
                base,
                folded,
                testkit::carryover_bytes_parsed() - before,
                snapshot,
            )
        })
        .join()
        .unwrap()
    };
    let (actual, actual_fold, bytes, mut actual_snapshot) = cold_read();
    assert_eq!(bytes, 0, "a current copy avoids the raw parse");
    assert_eq!(actual.agents, expected.agents);
    assert_eq!(actual.agent_identity, expected.agent_identity);
    assert_eq!(actual.resume_outcomes, expected.resume_outcomes);
    assert_eq!(actual_fold.1, expected_fold.1);
    assert_eq!(actual_fold.2, expected_fold.2);
    actual_snapshot.generated_at = expected_snapshot.generated_at;
    actual_snapshot.now = expected_snapshot.now;
    assert_eq!(
        serde_json::to_value(actual_snapshot).unwrap(),
        serde_json::to_value(expected_snapshot).unwrap()
    );
    let mut wrong_version: serde_json::Value = serde_json::from_slice(&valid).unwrap();
    wrong_version["version"] = serde_json::json!(0);
    for invalid in [
        b"{".to_vec(),
        b"garbage".to_vec(),
        serde_json::to_vec(&wrong_version).unwrap(),
    ] {
        fs::write(&derived, invalid).unwrap();
        let (actual, _, bytes, _) = cold_read();
        assert_eq!(bytes, durable.len() as u64);
        assert_eq!(actual.agents, expected.agents);
    }
    fs::write(&derived, &valid).unwrap();
    write_carryover(&paths.agents_carryover, &raw).unwrap();
    let (_, _, bytes, _) = cold_read();
    assert_eq!(
        bytes,
        durable.len() as u64,
        "replacement rejects the old stamp"
    );
    assert_eq!(fs::read(&paths.agents_carryover).unwrap(), durable);
    fs::remove_file(&paths.agents_carryover).unwrap();
    assert!(
        cold_read().0.agents.is_empty(),
        "a derived copy cannot resurrect absent truth"
    );
}

#[test]
fn materialized_rows_count_flattening_but_not_borrowed_iteration() {
    let rollup = AgentRollup::layered(
        Arc::new(FoldCarryover::from_raw(EventCarryover {
            agents: vec![agent("claude", "carried", AgentStatus::Idle, 1_000)],
            ..EventCarryover::default()
        })),
        &[agent("codex", "live", AgentStatus::Idle, 2_000)],
    );
    let before = testkit::rollup_rows_materialized();
    assert_eq!(rollup.iter().count(), 2);
    assert_eq!(testkit::rollup_rows_materialized(), before);
    assert_eq!(rollup.to_vec().len(), 2);
    assert_eq!(testkit::rollup_rows_materialized() - before, 2);
}

#[test]
fn pre_rebirth_registration_names_agree_with_a_persisted_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let mut continuing = agent("claude", "continuing", AgentStatus::Idle, 1_000);
    continuing.name = Some("x".to_owned());
    continuing.kind_ordinal = Some(10);
    write_carryover(
        &paths.agents_carryover,
        &EventCarryover {
            agents: vec![continuing],
            ..EventCarryover::default()
        },
    )
    .unwrap();
    let mut observation = crate::agents::AgentLifecycleObservation::new(
        Some("registered".into()),
        lifecycle::LifecycleSignal::Registered,
    );
    observation.agent_name = Some("x".to_owned());
    event_log::append(
        &paths.events_log,
        &EventEnvelope::agent_lifecycle(
            workspace.clone(),
            "session",
            "claude",
            "SessionStart",
            &observation,
        ),
    )
    .unwrap();
    let (checkpoint, _, _) = catch_up_rollup(&paths).unwrap();
    write_rollup_cache(&paths.rollup_cache, &checkpoint).unwrap();
    event_log::append(
        &paths.events_log,
        &EventEnvelope::session_rebirth(workspace, "session"),
    )
    .unwrap();
    let (_, warm, _) = catch_up_rollup(&paths).unwrap();
    std::fs::remove_file(&paths.rollup_cache).unwrap();
    let (_, cold, _) = catch_up_rollup(&paths).unwrap();
    let name = |agents: &[AgentState]| {
        agents
            .iter()
            .find(|agent| agent.agent_id == "registered")
            .unwrap()
            .name
            .clone()
    };
    assert_eq!(name(&warm).as_deref(), Some("x"));
    assert_eq!(
        name(&cold),
        name(&warm),
        "fold boundaries must not reserve names early"
    );
    let names = |agents: &[AgentState]| {
        agents
            .iter()
            .map(|agent| agent.name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&cold), names(&warm), "carried names must also agree");
    assert_eq!(sorted_value(cold), sorted_value(warm));
}

#[test]
fn write_rollup_cache_emits_compact_json_and_sweeps_stale_temp_siblings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollup.json");
    let nonce = "00000000000000000000000000000000";
    let stale = dir.path().join(format!("rollup.json.tmp.1.{nonce}"));
    let other = dir.path().join(format!("other.json.tmp.1.{nonce}"));
    std::fs::write(&stale, b"stale").unwrap();
    std::fs::write(&other, b"other").unwrap();
    let old = SystemTime::now() - Duration::from_secs(3_700);
    std::fs::File::open(&stale)
        .unwrap()
        .set_modified(old)
        .unwrap();
    std::fs::File::open(&other)
        .unwrap()
        .set_modified(old)
        .unwrap();

    write_rollup_cache(
        &path,
        &RollupCache {
            version: ROLLUP_CACHE_VERSION,
            extent: event_log::LogExtent {
                generation: 0,
                offset: 10,
            },
            raw_agents: vec![agent("claude", "real", AgentStatus::Running, 1_000)],
            resume_outcomes: Vec::new(),
            agent_identity: AgentIdentityState::default(),
            saw_session_rebirth: false,
        },
    )
    .unwrap();

    let contents = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        contents.lines().count(),
        1,
        "rollup cache should be compact single-line JSON"
    );
    assert!(
        !stale.exists(),
        "rollup write should sweep stale temp siblings"
    );
    assert!(other.exists(), "sweep should not touch other cache temps");
}

#[test]
fn mismatched_rollup_cache_falls_back_to_the_cold_fold() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    event_log::append(
        &paths.events_log,
        &lifecycle_at(
            &workspace,
            "claude",
            "SessionStart",
            "real",
            lifecycle::LifecycleSignal::Registered,
        ),
    )
    .unwrap();
    let full_len = std::fs::metadata(&paths.events_log).unwrap().len();

    let ghost_cache = |version: u32, offset: u64| RollupCache {
        version,
        extent: event_log::LogExtent {
            generation: 7,
            offset,
        },
        raw_agents: vec![agent("claude", "ghost", AgentStatus::Running, 0)],
        resume_outcomes: Vec::new(),
        agent_identity: AgentIdentityState::default(),
        saw_session_rebirth: false,
    };
    let assert_cold = |label: &str| {
        let (cache, agents, _) = catch_up_rollup(&paths).unwrap();
        assert!(
            agents.iter().any(|a| a.agent_id == "real"),
            "{label}: the cold fold reads the log"
        );
        assert!(
            agents.iter().all(|a| a.agent_id != "ghost"),
            "{label}: the unusable cache contributes nothing"
        );
        assert_eq!(
            cache.extent,
            event_log::LogExtent {
                generation: 0,
                offset: full_len,
            },
            "{label}: the refreshed base restarts at generation zero"
        );
    };

    // A shape from a different version reads as absent.
    write_rollup_cache(
        &paths.rollup_cache,
        &ghost_cache(ROLLUP_CACHE_VERSION + 1, 0),
    )
    .unwrap();
    assert_cold("version mismatch");

    // An extent past the live log is a rotation this cache predates.
    write_rollup_cache(
        &paths.rollup_cache,
        &ghost_cache(ROLLUP_CACHE_VERSION, full_len + 999),
    )
    .unwrap();
    assert_cold("extent past the log");
}

#[test]
fn version_twelve_tombstone_cache_refolds_ended_row_from_live_log() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    for event in [
        lifecycle_at(
            &workspace,
            "claude",
            "SessionStart",
            "resumable",
            lifecycle::LifecycleSignal::Registered,
        ),
        lifecycle_at(
            &workspace,
            "claude",
            "SessionEnd",
            "resumable",
            lifecycle::LifecycleSignal::Ended,
        ),
    ] {
        event_log::append(&paths.events_log, &event).unwrap();
    }
    let full_len = std::fs::metadata(&paths.events_log).unwrap().len();
    std::fs::write(
        &paths.rollup_cache,
        serde_json::to_vec(&serde_json::json!({
            "version": 12,
            "extent": { "generation": 0, "offset": full_len },
            "raw_agents": [],
            "resume_outcomes": [],
            "agent_identity": {},
            "saw_session_rebirth": false,
            "tombstones": [["claude", "resumable"]],
        }))
        .unwrap(),
    )
    .unwrap();

    let (cache, agents, _) = catch_up_rollup(&paths).unwrap();

    assert_eq!(cache.version, ROLLUP_CACHE_VERSION);
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].agent_id, "resumable");
    assert!(agents[0].ended_at.is_some());
}

#[test]
fn rollup_parse_cache_hits_on_same_identity_and_misses_when_republish_changes_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollup.json");
    let cache_with = |id: &str| RollupCache {
        version: ROLLUP_CACHE_VERSION,
        extent: event_log::LogExtent {
            generation: 0,
            offset: 10,
        },
        raw_agents: vec![agent("claude", id, AgentStatus::Running, 1_000)],
        resume_outcomes: Vec::new(),
        agent_identity: AgentIdentityState::default(),
        saw_session_rebirth: false,
    };
    write_rollup_cache(&path, &cache_with("aaaa")).unwrap();
    let first = read_rollup_cache(&path).unwrap();
    assert_eq!(first.raw_agents[0].agent_id, "aaaa");

    // Identical (path, mtime, len): rewrite the bytes in place at equal
    // length and restore the mtime — the thread's parse cache must serve the
    // prior parse, proving the deserialize was skipped.
    let meta = std::fs::metadata(&path).unwrap();
    let mtime = meta.modified().unwrap();
    let swapped = std::fs::read_to_string(&path)
        .unwrap()
        .replace("aaaa", "bbbb");
    std::fs::write(&path, swapped).unwrap();
    std::fs::File::open(&path)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), meta.len());
    let hit = read_rollup_cache(&path).unwrap();
    assert_eq!(
        hit.raw_agents[0].agent_id, "aaaa",
        "identical identity serves the cached parse"
    );

    // A different-length republish changes the identity even when both writes
    // land in one filesystem mtime tick, so the read re-parses.
    write_rollup_cache(&path, &cache_with("cccc-longer")).unwrap();
    let miss = read_rollup_cache(&path).unwrap();
    assert_eq!(
        miss.raw_agents[0].agent_id, "cccc-longer",
        "a changed identity makes the read re-parse"
    );
}

#[test]
fn carryover_parse_is_shared_per_identity_and_corruption_errors_until_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agents.carryover.json");
    let carryover_with = |id: &str| EventCarryover {
        agents: vec![agent("claude", id, AgentStatus::Idle, 1_000)],
        ..EventCarryover::default()
    };
    write_carryover(&path, &carryover_with("aaaa")).unwrap();
    let first = fold_carryover(&path, None, None).unwrap();
    assert!(
        Arc::ptr_eq(&first, &fold_carryover(&path, None, None).unwrap()),
        "an unchanged file identity serves the shared parse"
    );
    assert!(
        first.agents[0].kind_ordinal.is_some(),
        "the shared parse is fold-ready: identities backfilled"
    );

    // Atomic replacement with identical bytes still changes the inode.
    write_carryover(&path, &carryover_with("aaaa")).unwrap();
    assert!(
        !Arc::ptr_eq(&first, &fold_carryover(&path, None, None).unwrap()),
        "a replaced file re-parses"
    );

    let corrupt = dir.path().join("corrupt.tmp");
    std::fs::write(&corrupt, b"{\"agents\": [").unwrap();
    std::fs::rename(&corrupt, &path).unwrap();
    for attempt in 0..2 {
        assert!(
            matches!(
                fold_carryover(&path, None, None),
                Err(SnapshotErr::Json { .. })
            ),
            "corruption surfaces on fold {attempt}, never a cached default"
        );
    }
    write_carryover(&path, &carryover_with("bbbb")).unwrap();
    assert_eq!(
        fold_carryover(&path, None, None).unwrap().agents[0].agent_id,
        "bbbb"
    );

    std::fs::remove_file(&path).unwrap();
    assert!(fold_carryover(&path, None, None).unwrap().agents.is_empty());
}

#[test]
fn cursor_serves_the_held_fold_while_the_log_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    event_log::append(
        &paths.events_log,
        &lifecycle_at(
            &workspace,
            "claude",
            "SessionStart",
            "real",
            lifecycle::LifecycleSignal::Registered,
        ),
    )
    .unwrap();

    let mut cursor = RollupCursor::new();
    let (first_extent, first, _) = cursor.fold(&paths).unwrap();

    // Plant a ghost base on disk. A warm cursor over an unchanged log serves
    // its held fold — it never re-reads `rollup.json`, so the ghost cannot
    // leak into the merge.
    write_rollup_cache(
        &paths.rollup_cache,
        &RollupCache {
            version: ROLLUP_CACHE_VERSION,
            extent: event_log::LogExtent {
                generation: 0,
                offset: 0,
            },
            raw_agents: vec![agent("claude", "ghost", AgentStatus::Running, 0)],
            resume_outcomes: Vec::new(),
            agent_identity: AgentIdentityState::default(),
            saw_session_rebirth: false,
        },
    )
    .unwrap();

    let (held_extent, held, _) = cursor.fold(&paths).unwrap();
    assert_eq!(held_extent, first_extent);
    assert_eq!(sorted_value(held.to_vec()), sorted_value(first.to_vec()));
    assert!(
        held.iter().all(|a| a.agent_id != "ghost"),
        "an unchanged log serves the in-memory base, not the disk base"
    );
}

#[test]
fn cached_rebirth_continues_to_reset_carryover_identity_after_extent() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let mut carried =
        agent("claude", "old-session", AgentStatus::Idle, 1_000).in_pane("terminal_1");
    carried.name = Some("lucid-atlas".to_owned());
    carried.kind_ordinal = Some(1);
    write_carryover(
        &paths.agents_carryover,
        &EventCarryover {
            agents: vec![carried],
            agent_identity: AgentIdentityState::default(),
            resume_outcomes: Vec::new(),
        },
    )
    .unwrap();
    event_log::append(
        &paths.events_log,
        &EventEnvelope::session_rebirth(workspace.clone(), "session"),
    )
    .unwrap();
    let (rebirth_cache, first, _) = catch_up_rollup(&paths).unwrap();
    assert!(rebirth_cache.saw_session_rebirth);
    assert!(
        first
            .iter()
            .all(|agent| agent.pane.is_none() && agent.kind_ordinal.is_some()),
        "the rebirth fold clears stale pane stamps and backfills card identity"
    );
    write_rollup_cache(&paths.rollup_cache, &rebirth_cache).unwrap();

    event_log::append(
        &paths.events_log,
        &lifecycle_at(
            &workspace,
            "claude",
            "SessionStart",
            "new-session",
            lifecycle::LifecycleSignal::Registered,
        ),
    )
    .unwrap();
    let (next_cache, next, _) = catch_up_rollup(&paths).unwrap();
    let old = next
        .iter()
        .find(|agent| agent.agent_id.as_str() == "old-session")
        .expect("carryover survivor");
    let new = next
        .iter()
        .find(|agent| agent.agent_id.as_str() == "new-session")
        .expect("fresh active-log agent");

    assert!(next_cache.saw_session_rebirth);
    assert!(old.pane.is_none());
    assert_ne!(old.kind_ordinal, new.kind_ordinal);
    assert!(old.kind_ordinal.is_some());
    assert!(new.kind_ordinal.is_some());
}

fn registered_workspace(agent_id: &str) -> (tempfile::TempDir, WorkspaceId, StatePaths) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    append_registered(&workspace, &paths, agent_id);
    (dir, workspace, paths)
}

fn append_registered(workspace: &WorkspaceId, paths: &StatePaths, agent_id: &str) {
    event_log::append(
        &paths.events_log,
        &lifecycle_at(
            workspace,
            "claude",
            "SessionStart",
            agent_id,
            lifecycle::LifecycleSignal::Registered,
        ),
    )
    .unwrap();
}

fn log_len(paths: &StatePaths) -> u64 {
    std::fs::metadata(&paths.events_log).unwrap().len()
}

#[test]
fn thread_fold_holds_one_workspace_and_another_replaces_it() {
    let (_a_dir, _, a) = registered_workspace("a-agent");
    let (_b_dir, _, b) = registered_workspace("b-agent");
    let read = |paths: &StatePaths| {
        let before = event_log::testkit::bytes_read();
        let (_, agents, _) = thread_catch_up(paths).unwrap();
        let ids = agents
            .iter()
            .map(|agent| agent.agent_id.to_string())
            .collect::<Vec<_>>();
        (event_log::testkit::bytes_read() - before, ids)
    };

    assert_eq!(read(&a), (log_len(&a), vec!["a-agent".to_owned()]));
    assert_eq!(
        read(&a),
        (0, vec!["a-agent".to_owned()]),
        "a repeat read at an unchanged log is served by the thread's held fold"
    );
    assert_eq!(read(&b), (log_len(&b), vec!["b-agent".to_owned()]));
    assert_eq!(
        read(&a),
        (log_len(&a), vec!["a-agent".to_owned()]),
        "the one slot held only the other workspace, so this read folds cold"
    );
}

#[test]
fn thread_fold_reflects_an_append_made_in_the_same_thread() {
    let (_dir, workspace, paths) = registered_workspace("first");
    thread_catch_up(&paths).unwrap();
    let held = log_len(&paths);
    append_registered(&workspace, &paths, "second");

    let before = event_log::testkit::bytes_read();
    let (cache, agents, _) = thread_catch_up(&paths).unwrap();
    assert_eq!(
        event_log::testkit::bytes_read() - before,
        log_len(&paths) - held,
        "the held fold reads only the appended frame"
    );
    assert_eq!(cache.extent.offset, log_len(&paths));
    let ids = agents
        .iter()
        .map(|agent| agent.agent_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["first", "second"]);
}

#[test]
fn thread_fold_serves_a_replaced_carryover_at_an_unchanged_log() {
    let (_dir, _, paths) = registered_workspace("live");
    write_carryover(
        &paths.agents_carryover,
        &EventCarryover {
            agents: vec![agent("codex", "carried", AgentStatus::Success, 1_000)],
            ..EventCarryover::default()
        },
    )
    .unwrap();
    let (_, first, _) = thread_catch_up(&paths).unwrap();
    assert_eq!(first.len(), 2);

    // GC prunes or rotation rewrites the carryover while the log stands still.
    write_carryover(
        &paths.agents_carryover,
        &EventCarryover {
            agents: vec![agent("codex", "replacement", AgentStatus::Success, 2_000)],
            ..EventCarryover::default()
        },
    )
    .unwrap();
    let (_, served, _) = thread_catch_up(&paths).unwrap();
    let (_, cold, _) = RollupCursor::new().fold(&paths).unwrap();
    assert_eq!(sorted_value(served.to_vec()), sorted_value(cold.to_vec()));
    assert!(served.iter().any(|agent| agent.agent_id == "replacement"));
    assert!(served.iter().all(|agent| agent.agent_id != "carried"));
}
