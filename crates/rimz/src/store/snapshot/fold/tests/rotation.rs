use super::*;

#[test]
fn cursor_reloads_across_a_rotation() {
    // Rotation renames the log away and recreates it; a regrown log can pass
    // the held offset, so the cursor's reload guard is the file identity, not
    // the length. After the swap the cursor must drop its in-memory base and
    // reload `rollup.json`, whose bumped generation rotation reseeded.
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

    // Rotate: carryover the rollup, swap the log file, reseed the base —
    // then regrow the new log *past* the held offset, the case a
    // length-only guard would misread as appended frames.
    let (cache, _, _) = catch_up_rollup(&paths).unwrap();
    write_carryover(
        &paths.agents_carryover,
        &EventCarryover {
            agents: cache.raw_agents.clone(),
            agent_identity: cache.agent_identity.clone(),
            resume_outcomes: Vec::new(),
            lost: Vec::new(),
        },
    )
    .unwrap();
    std::fs::remove_file(&paths.events_log).unwrap();
    reseed_rollup_cache_for_rotation(&paths).unwrap();
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
        sorted_value(merged),
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
fn reseed_for_rotation_bumps_generation_and_starts_an_empty_fold() {
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
    write_rollup_cache(&paths.rollup_cache, &cache).unwrap();
    assert_eq!(cache.extent.generation, 0);
    assert!(cache.extent.offset > 0);

    // Rotation: the old log's rollup moves into the carryover, the active
    // log is renamed away, and the fold base reseeds for the new generation.
    write_carryover(
        &paths.agents_carryover,
        &EventCarryover {
            agents: cache.raw_agents.clone(),
            agent_identity: cache.agent_identity.clone(),
            resume_outcomes: Vec::new(),
            lost: Vec::new(),
        },
    )
    .unwrap();
    std::fs::remove_file(&paths.events_log).unwrap();
    reseed_rollup_cache_for_rotation(&paths).unwrap();

    let (fresh, agents, _) = catch_up_rollup(&paths).unwrap();
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
