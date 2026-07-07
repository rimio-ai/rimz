use super::*;

fn lost_event(workspace: &WorkspaceId, id: &str) -> EventEnvelope {
    lifecycle_at(
        workspace,
        "claude",
        "rimz.agent-lost",
        id,
        lifecycle::LifecycleSignal::Lost,
    )
}

#[test]
fn lost_markers_are_collected_in_the_rollup_cache() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    event_log::append(&paths.events_log, &lost_event(&workspace, "sess-a")).unwrap();

    let (cache, agents, _) = catch_up_rollup(&paths).unwrap();

    assert!(agents.is_empty(), "lost alone is a side-channel marker");
    assert_eq!(
        cache.lost,
        vec![(AgentKind::new_unchecked("claude"), "sess-a".into())]
    );
}

#[test]
fn rebirth_boundary_clears_only_earlier_lost_markers_in_the_delta() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    event_log::append(&paths.events_log, &lost_event(&workspace, "before")).unwrap();
    event_log::append(
        &paths.events_log,
        &EventEnvelope::session_rebirth(workspace.clone(), "session"),
    )
    .unwrap();
    event_log::append(&paths.events_log, &lost_event(&workspace, "after")).unwrap();

    let (cache, _, _) = catch_up_rollup(&paths).unwrap();

    assert_eq!(
        cache.lost,
        vec![(AgentKind::new_unchecked("claude"), "after".into())]
    );
}

#[test]
fn cached_rebirth_does_not_clear_lost_markers_recorded_after_it() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    event_log::append(
        &paths.events_log,
        &EventEnvelope::session_rebirth(workspace.clone(), "session"),
    )
    .unwrap();
    event_log::append(&paths.events_log, &lost_event(&workspace, "after")).unwrap();
    let (cache, _, _) = catch_up_rollup(&paths).unwrap();
    assert!(cache.saw_session_rebirth);
    write_rollup_cache(&paths.rollup_cache, &cache).unwrap();

    let (next, _, _) = catch_up_rollup(&paths).unwrap();

    assert_eq!(
        next.lost,
        vec![(AgentKind::new_unchecked("claude"), "after".into())]
    );
}

#[test]
fn lost_markers_survive_rotation_carryover_until_rebirth() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    write_carryover(
        &paths.agents_carryover,
        &EventCarryover {
            lost: vec![(AgentKind::new_unchecked("claude"), "carried".into())],
            ..EventCarryover::default()
        },
    )
    .unwrap();
    reseed_rollup_cache_for_rotation(&paths).unwrap();

    let (carried, _, _) = catch_up_rollup(&paths).unwrap();
    assert_eq!(
        carried.lost,
        vec![(AgentKind::new_unchecked("claude"), "carried".into())]
    );

    event_log::append(
        &paths.events_log,
        &EventEnvelope::session_rebirth(workspace, "session"),
    )
    .unwrap();
    let (cleared, _, _) = catch_up_rollup(&paths).unwrap();
    assert!(cleared.lost.is_empty());
}

#[test]
fn old_rollup_cache_without_lost_field_reads_as_empty_lost_set() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollup.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "version": ROLLUP_CACHE_VERSION,
            "extent": { "generation": 0, "offset": 0 },
            "raw_agents": [],
            "resume_outcomes": [],
            "agent_identity": {},
            "saw_session_rebirth": false,
            "tombstones": []
        })
        .to_string(),
    )
    .unwrap();

    let cache = read_rollup_cache(&path).unwrap();

    assert!(cache.lost.is_empty());
}
