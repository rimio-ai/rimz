use super::*;

#[test]
fn presence_event_mode_boundary_is_inclusive() {
    let fresh_edge = PRESENCE_STAMP_FRESH.as_millis() as u64;
    assert!(presence_event_mode(Some(0)));
    assert!(presence_event_mode(Some(fresh_edge)));
    assert!(!presence_event_mode(Some(fresh_edge + 1)));
    assert!(!presence_event_mode(None), "absent stamp is poll mode");
}

#[test]
fn effective_pane_ttl_selects_by_mode() {
    assert_eq!(effective_pane_ttl(Some(0)), EVENT_PANE_TTL);
    assert_eq!(
        effective_pane_ttl(Some(PRESENCE_STAMP_FRESH.as_millis() as u64 + 1)),
        SNAPSHOT_CACHE_TTL
    );
    assert_eq!(effective_pane_ttl(None), SNAPSHOT_CACHE_TTL);
}

#[test]
fn presence_stamp_round_trips_through_the_runtime_root() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();

    assert_eq!(
        presence_stamp_age_ms(&runtime),
        None,
        "no stamp yet: poll mode"
    );
    write_presence_stamp(&runtime);
    let age = presence_stamp_age_ms(&runtime).expect("stamp written and readable");
    assert!(
        age < 1_000,
        "a just-written stamp reads as young, got {age}ms"
    );
    assert!(presence_event_mode(Some(age)));
}

#[test]
fn presence_stamp_from_a_future_clock_saturates_to_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();

    let future = PresenceStamp {
        written_at_ms: unix_now_ms() + 60_000,
    };
    atomic::write_temp_then_rename_cache(&presence_stamp_path(&runtime), &future).unwrap();
    assert_eq!(
        presence_stamp_age_ms(&runtime),
        Some(0),
        "a stamp ahead of this reader's clock saturates to age 0, never poll mode"
    );
}

#[test]
fn unreadable_presence_stamp_reads_poll_mode() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();

    std::fs::create_dir_all(&runtime.root).unwrap();
    std::fs::write(presence_stamp_path(&runtime), b"{ not json").unwrap();
    assert_eq!(presence_stamp_age_ms(&runtime), None);
    assert!(!presence_event_mode(presence_stamp_age_ms(&runtime)));
}

fn cache_produced_at(produced_at_ms: u64) -> PaneFrame {
    assemble_frame(Vec::new(), produced_at_ms, "rimz-test")
}

#[test]
fn event_mode_serves_a_cache_poll_mode_would_reject() {
    let now = unix_now_ms();
    let five_seconds_old = cache_produced_at(now - 5_000);
    assert!(
        snapshot_cache_is_fresh(&five_seconds_old, now, None, EVENT_PANE_TTL),
        "5s-old cache serves under the 10s event TTL: no list-panes fork"
    );
    assert!(
        !snapshot_cache_is_fresh(&five_seconds_old, now, None, SNAPSHOT_CACHE_TTL),
        "the same cache misses under the 750ms poll TTL"
    );

    let one_second_old = cache_produced_at(now - 1_000);
    assert!(
        !snapshot_cache_is_fresh(&one_second_old, now, None, SNAPSHOT_CACHE_TTL),
        "a stale stamp reverts to poll mode: a 1s-old cache no longer serves"
    );
}

#[test]
fn forced_pane_freshness_overrides_event_mode() {
    let now = unix_now_ms();
    let five_seconds_old = cache_produced_at(now - 5_000);
    assert!(
        !snapshot_cache_is_fresh(&five_seconds_old, now, Some(now), EVENT_PANE_TTL),
        "a lifecycle/resize floor rejects a pre-signal cache regardless of TTL"
    );
    assert!(
        snapshot_cache_is_fresh(&five_seconds_old, now, Some(now - 5_000), EVENT_PANE_TTL),
        "a cache at the floor is usable"
    );
}

#[test]
fn snapshot_cache_age_saturates_on_a_future_producer_clock() {
    let now = unix_now_ms();
    let future = cache_produced_at(now + 60_000);
    assert!(
        snapshot_cache_is_fresh(&future, now, None, SNAPSHOT_CACHE_TTL),
        "a cache stamped ahead of this reader serves rather than re-producing every call"
    );
}
