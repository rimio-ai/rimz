use super::*;

#[test]
fn rotation_commits_reborn_ordinals_before_the_next_registration() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let mut continuing = agent("claude", "continuing", AgentStatus::Idle, 1_000);
    continuing.last_seen = recent(1);
    continuing.name = Some("continuing-one".to_owned());
    continuing.kind_ordinal = Some(10);
    write_carryover(
        &paths.agents_carryover,
        &EventCarryover {
            agents: vec![continuing],
            ..EventCarryover::default()
        },
    )
    .unwrap();
    event_log::append(
        &paths.events_log,
        &EventEnvelope::session_rebirth(workspace.clone(), "session"),
    )
    .unwrap();
    event_log::append(
        &paths.events_log,
        &lifecycle_at(
            &workspace,
            "claude",
            "SessionStart",
            "live-session",
            lifecycle::LifecycleSignal::Registered,
        ),
    )
    .unwrap();
    let (cache, _, _) = catch_up_rollup(&paths).unwrap();
    write_rollup_cache(&paths, &cache).unwrap();
    stage_carryover_for_rotation(&paths, 1).unwrap();
    rotate(&paths);
    event_log::append(
        &paths.events_log,
        &lifecycle_at(
            &workspace,
            "claude",
            "SessionStart",
            "later-session",
            lifecycle::LifecycleSignal::Registered,
        ),
    )
    .unwrap();
    let (_, agents, _) = RollupCursor::new().fold(&paths).unwrap();
    let ordinal = |id: &str| {
        agents
            .iter()
            .find(|agent| agent.agent_id.as_str() == id)
            .unwrap()
            .kind_ordinal
    };
    assert_eq!(ordinal("continuing"), Some(2));
    assert_eq!(ordinal("live-session"), Some(1));
    assert_eq!(
        ordinal("later-session"),
        Some(3),
        "the next generation must not reuse a committed continuing ordinal"
    );
}

#[test]
fn legacy_ended_carryover_folds_and_stages_only_labels() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let mut carried = agent("claude", "ended", AgentStatus::Success, 1_000);
    carried.last_seen = recent(1);
    carried.ended_at = Some(carried.last_seen);
    carried.name = Some("lucid-atlas".to_owned());
    carried.kind_ordinal = Some(1);
    carried.first_prompt = Some(format!("first\n {}", "x".repeat(300)));
    carried.prompt = Some("latest\n prompt".to_owned());
    carried.recent_prompts = vec!["first prompt".to_owned(), "latest prompt".to_owned()];
    let mut stamp = pane("%7", "claude", "/repo");
    stamp.foreground_cmdline = Some("foreground prompt".to_owned());
    stamp.spawn_command = Some("birth prompt".to_owned());
    carried.pane = Some(stamp);
    write_carryover(
        &paths.agents_carryover,
        &EventCarryover {
            agents: vec![carried],
            ..EventCarryover::default()
        },
    )
    .unwrap();
    let before = read_carryover(&paths.agents_carryover).unwrap();
    let mut expected = before.agents[0].clone();
    expected.first_prompt = Some(format!("first {}", "x".repeat(154)));
    expected.prompt = Some("latest prompt".to_owned());
    expected.recent_prompts.clear();
    let stamp = expected.pane.as_mut().unwrap();
    stamp.foreground_cmdline = None;
    stamp.spawn_command = None;
    let mut cursor = RollupCursor::new();
    let (_, folded, _) = cursor.fold(&paths).unwrap();
    assert_eq!(folded.to_vec(), vec![expected.clone()]);
    assert_eq!(catch_up_rollup(&paths).unwrap().1, vec![expected.clone()]);
    assert_eq!(
        thread_catch_up(&paths).unwrap().1.to_vec(),
        vec![expected.clone()]
    );
    assert_eq!(
        read_carryover(&paths.agents_carryover).unwrap().agents,
        before.agents
    );
    event_log::append(
        &paths.events_log,
        &EventEnvelope::new(
            workspace.clone(),
            "session",
            "rimz",
            "cli",
            "test.noop",
            serde_json::json!({}),
        ),
    )
    .unwrap();
    cursor.fold(&paths).unwrap();
    stage_carryover_for_rotation(&paths, 1).unwrap();
    assert_eq!(
        read_carryover(&paths.agents_carryover).unwrap().agents,
        vec![expected.clone()]
    );
    rotate(&paths);
    event_log::append(
        &paths.events_log,
        &EventEnvelope::new(
            workspace,
            "session",
            "claude",
            "agent",
            "agent.launch_warnings",
            serde_json::json!({"agent_id": "ended", "warnings": ["new warning"]}),
        ),
    )
    .unwrap();
    cursor.fold(&paths).unwrap();
    stage_carryover_for_rotation(&paths, 1).unwrap();
    let after = read_carryover(&paths.agents_carryover).unwrap();
    expected.launch_warnings = vec!["new warning".to_owned()];
    assert_eq!(after.agents, vec![expected]);
}

#[test]
fn cursor_reloads_across_a_rotation() {
    // Rotation renames the log away and recreates it; a regrown log can pass
    // the held offset, so the cursor's reload guard is the file identity, not
    // the length. After the swap the cursor must drop its in-memory base and
    // reload the fold using the durable generation.
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let lifecycle = |kind: &str, id: &str| {
        lifecycle_at(
            &workspace,
            kind,
            "SessionStart",
            id,
            lifecycle::LifecycleSignal::Registered,
        )
    };
    event_log::append(&paths.events_log, &lifecycle("claude", "a")).unwrap();

    let mut cursor = RollupCursor::new();
    let (warm_extent, _, _) = cursor.fold(&paths).unwrap();
    assert_eq!(warm_extent.generation, 0);

    // Rotate: carry over the rollup, swap the log file, rebuild the base —
    // then regrow the new log *past* the held offset, the case a
    // length-only guard would misread as appended frames.
    let (cache, _, _) = catch_up_rollup(&paths).unwrap();
    write_carryover(
        &paths.agents_carryover,
        &EventCarryover {
            agents: cache.raw_agents.clone(),
            agent_identity: cache.agent_identity.clone(),
            resume_outcomes: Vec::new(),
        },
    )
    .unwrap();
    let rotation = rotate(&paths).rotation;
    assert!(
        rotation.is_rotated(),
        "the test must exercise the production rename-and-recreate rotation"
    );
    event_log::append(&paths.events_log, &lifecycle("codex", "b")).unwrap();
    event_log::append(&paths.events_log, &lifecycle("codex", "c")).unwrap();
    assert!(
        std::fs::metadata(&paths.events_log).unwrap().len() > warm_extent.offset,
        "the regrown log must outgrow the held offset for this test to bite"
    );

    let (extent, merged, _) = cursor.fold(&paths).unwrap();
    std::fs::remove_file(&paths.rollup_cache).unwrap();
    let (cold_cache, cold, _) = catch_up_rollup(&paths).unwrap();
    assert_eq!(extent.generation, 1, "the reloaded base carries the bump");
    assert_eq!(extent.offset, cold_cache.extent.offset);
    assert_eq!(
        sorted_value(merged.to_vec()),
        sorted_value(cold),
        "the post-rotation cursor fold equals the cold fold"
    );
}

#[test]
fn cursor_reloads_on_an_offset_regression() {
    // Same file identity, shorter log — a truncation the cursor's held offset
    // now overruns. The reload path falls back to the cold fold (the planted
    // base is past the log too) and the cursor re-folds what remains.
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let mut frame_ends = Vec::new();
    for id in ["a", "b"] {
        event_log::append(
            &paths.events_log,
            &lifecycle_at(
                &workspace,
                "claude",
                "SessionStart",
                id,
                lifecycle::LifecycleSignal::Registered,
            ),
        )
        .unwrap();
        frame_ends.push(std::fs::metadata(&paths.events_log).unwrap().len());
    }

    let mut cursor = RollupCursor::new();
    let (warm_extent, _, _) = cursor.fold(&paths).unwrap();
    assert_eq!(warm_extent.offset, frame_ends[1]);

    // Truncate in place: identity unchanged, length regressed.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&paths.events_log)
        .unwrap()
        .set_len(frame_ends[0])
        .unwrap();

    let (extent, merged, _) = cursor.fold(&paths).unwrap();
    assert_eq!(extent.offset, frame_ends[0]);
    let ids: Vec<&str> = merged.iter().map(|a| a.agent_id.as_str()).collect();
    assert_eq!(
        ids,
        ["a"],
        "the regressed fold reflects only the surviving frames"
    );
}

#[test]
fn cursor_drops_rows_pruned_from_the_carryover_on_an_unchanged_log() {
    // GC prunes the carryover without touching the log; the cursor's
    // "nothing appended" fast path must still see the carryover change.
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let mut carried = agent("claude", "carried", AgentStatus::Idle, 1_000);
    carried.last_seen = recent(1);
    write_carryover(
        &paths.agents_carryover,
        &EventCarryover {
            agents: vec![carried],
            agent_identity: Default::default(),
            resume_outcomes: Vec::new(),
        },
    )
    .unwrap();
    event_log::append(
        &paths.events_log,
        &lifecycle_at(
            &workspace,
            "codex",
            "SessionStart",
            "live",
            lifecycle::LifecycleSignal::Registered,
        ),
    )
    .unwrap();

    let mut cursor = RollupCursor::new();
    let ids = |rollup: &AgentRollup| {
        let mut ids: Vec<String> = rollup
            .iter()
            .map(|a| a.agent_id.as_str().to_owned())
            .collect();
        ids.sort_unstable();
        ids
    };
    let (_, before, _) = cursor.fold(&paths).unwrap();
    assert_eq!(ids(&before), ["carried", "live"]);

    let log_len = std::fs::metadata(&paths.events_log).unwrap().len();
    write_carryover(
        &paths.agents_carryover,
        &EventCarryover {
            agents: Vec::new(),
            agent_identity: Default::default(),
            resume_outcomes: Vec::new(),
        },
    )
    .unwrap();
    assert_eq!(
        std::fs::metadata(&paths.events_log).unwrap().len(),
        log_len,
        "the log must stand still for this test to bite"
    );

    let (_, after, _) = cursor.fold(&paths).unwrap();
    assert_eq!(ids(&after), ["live"], "the pruned carryover row is gone");
}

#[test]
fn post_rotation_rebuild_equals_a_cold_fold_at_the_durable_generation() {
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
            "a",
            lifecycle::LifecycleSignal::Registered,
        ),
    )
    .unwrap();
    let (cache, _, _) = catch_up_rollup(&paths).unwrap();
    write_rollup_cache(&paths, &cache).unwrap();
    assert_eq!(cache.extent.generation, 0);
    assert!(cache.extent.offset > 0);

    rotate(&paths);

    let (fresh, agents, _) = catch_up_rollup(&paths).unwrap();
    std::fs::remove_file(&paths.rollup_cache).unwrap();
    let (cold, cold_agents, _) = catch_up_rollup(&paths).unwrap();
    assert_eq!(
        serde_json::to_value(&fresh).unwrap(),
        serde_json::to_value(cold).unwrap()
    );
    assert_eq!(sorted_value(agents.clone()), sorted_value(cold_agents));
    assert_eq!(
        fresh.extent,
        event_log::LogExtent {
            generation: 1,
            offset: 0,
        },
        "the new generation starts with an empty fold at offset zero"
    );
    assert!(fresh.raw_agents.is_empty());
    assert!(
        agents.iter().any(|a| a.agent_id == "a"),
        "the pre-rotation agent survives via the carryover merge"
    );

    // Appends to the fresh log fold under the bumped generation.
    event_log::append(
        &paths.events_log,
        &lifecycle_at(
            &workspace,
            "codex",
            "SessionStart",
            "b",
            lifecycle::LifecycleSignal::Registered,
        ),
    )
    .unwrap();
    let (next, agents, _) = catch_up_rollup(&paths).unwrap();
    assert_eq!(next.extent.generation, 1);
    assert!(next.extent.offset > 0);
    let ids: Vec<&str> = {
        let mut ids: Vec<&str> = agents.iter().map(|a| a.agent_id.as_str()).collect();
        ids.sort_unstable();
        ids
    };
    assert_eq!(ids, ["a", "b"], "carryover and fresh-log agents merge");
}

#[test]
fn thread_fold_serves_across_a_rotation() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let registered = |agent_id: &str| {
        lifecycle_at(
            &workspace,
            "claude",
            "SessionStart",
            agent_id,
            lifecycle::LifecycleSignal::Registered,
        )
    };
    event_log::append(&paths.events_log, &registered("before")).unwrap();
    let (held, _, _) = thread_catch_up(&paths).unwrap();
    write_rollup_cache(&paths, &held).unwrap();
    drop(held);

    stage_carryover_for_rotation(&paths, 1).unwrap();
    rotate(&paths);
    event_log::append(&paths.events_log, &registered("after")).unwrap();

    let (cache, served, _) = thread_catch_up(&paths).unwrap();
    let (cold_extent, cold, _) = RollupCursor::new().fold(&paths).unwrap();
    assert_eq!(cache.extent, cold_extent);
    assert_eq!(cache.extent.generation, 1, "the durable generation is read");
    assert_eq!(sorted_value(served.to_vec()), sorted_value(cold.to_vec()));
    let ids = served
        .iter()
        .map(|agent| agent.agent_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["after", "before"]);
}
