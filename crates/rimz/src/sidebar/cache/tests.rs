use super::*;
use crate::ids::WorkspaceId;
use crate::sidebar::FRESH_PANE_GRACE;
use crate::sidebar::frame::assemble_frame;
use crate::sidebar::refresh::git_stats::{DiffStats, DiffStatsCacheEntry, WorktreeRootsCache};
use crate::sidebar::test_support::pane;
use crate::sidebar::timing::{
    DIFF_STATS_IDLE_TTL, DIFF_STATS_TTL, EVENT_PANE_TTL, PRESENCE_STAMP_FRESH, SNAPSHOT_CACHE_TTL,
    WORKTREE_ROOTS_TTL, unix_now_ms,
};
use crate::store::atomic;
#[test]
fn pane_topology_cache_freshness_honors_requested_floor() {
    let cache = PaneTopologyCache {
        session_name: "rimz-test".to_owned(),
        produced_at_ms: 100,
        writer: None,
        focused_pane: None,
        clients: None,
        panes: Vec::new(),
    };

    assert!(pane_topology_cache_is_fresh(&cache, 101, Some(100)));
    assert!(!pane_topology_cache_is_fresh(&cache, 101, Some(101)));
}

#[test]
fn pane_topology_reap_floor_rejects_cache_older_than_fresh_pane_grace() {
    let now = unix_now_ms();
    let grace_ms = FRESH_PANE_GRACE.as_millis() as u64;
    let floor = now.saturating_sub(grace_ms);
    let old_cache = PaneTopologyCache {
        session_name: "rimz-test".to_owned(),
        produced_at_ms: floor.saturating_sub(1),
        writer: None,
        focused_pane: None,
        clients: None,
        panes: Vec::new(),
    };
    let fresh_cache = PaneTopologyCache {
        produced_at_ms: floor,
        ..old_cache.clone()
    };

    assert!(!pane_topology_cache_is_fresh(&old_cache, now, Some(floor)));
    assert!(pane_topology_cache_is_fresh(&fresh_cache, now, Some(floor)));
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
fn presence_stamp_age_handles_clock_skew_and_bad_files() {
    let future_dir = tempfile::tempdir().unwrap();
    let future_workspace = WorkspaceId::from_project_root(future_dir.path());
    let future_runtime = RuntimePaths::under(future_workspace, future_dir.path()).unwrap();
    let future = PresenceStamp {
        written_at_ms: unix_now_ms() + 60_000,
    };
    atomic::write_temp_then_rename_cache(&presence_stamp_path(&future_runtime), &future).unwrap();
    assert_eq!(
        presence_stamp_age_ms(&future_runtime),
        Some(0),
        "a stamp ahead of this reader's clock saturates to age 0, never poll mode"
    );

    let bad_dir = tempfile::tempdir().unwrap();
    let bad_workspace = WorkspaceId::from_project_root(bad_dir.path());
    let bad_runtime = RuntimePaths::under(bad_workspace, bad_dir.path()).unwrap();
    std::fs::create_dir_all(&bad_runtime.root).unwrap();
    std::fs::write(presence_stamp_path(&bad_runtime), b"{ not json").unwrap();
    assert_eq!(presence_stamp_age_ms(&bad_runtime), None);
    assert!(!presence_event_mode(presence_stamp_age_ms(&bad_runtime)));
}

fn cache_produced_at(produced_at_ms: u64) -> PaneFrame {
    assemble_frame(Vec::new(), produced_at_ms, "rimz-test")
}

#[test]
fn event_mode_serves_a_cache_poll_mode_would_reject() {
    // Stamp age selects the mode at the fresh boundary, and the mode picks the
    // pane TTL: fresh -> event, stale or absent -> poll.
    let fresh_edge = PRESENCE_STAMP_FRESH.as_millis() as u64;
    assert!(presence_event_mode(Some(0)));
    assert!(presence_event_mode(Some(fresh_edge)));
    assert!(!presence_event_mode(Some(fresh_edge + 1)));
    assert!(!presence_event_mode(None), "absent stamp is poll mode");
    assert_eq!(effective_pane_ttl(Some(0), false), EVENT_PANE_TTL);
    assert_eq!(
        effective_pane_ttl(Some(fresh_edge + 1), false),
        SNAPSHOT_CACHE_TTL
    );
    assert_eq!(effective_pane_ttl(None, false), SNAPSHOT_CACHE_TTL);
    assert_eq!(effective_pane_ttl(None, true), EVENT_PANE_TTL);
    assert_eq!(
        effective_pane_ttl(Some(fresh_edge + 1), true),
        EVENT_PANE_TTL
    );

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

    // A cache stamped ahead of this reader's clock saturates to age 0 and
    // serves rather than re-producing every call.
    let future = cache_produced_at(now + 60_000);
    assert!(
        snapshot_cache_is_fresh(&future, now, None, SNAPSHOT_CACHE_TTL),
        "a cache stamped ahead of this reader serves rather than re-producing every call"
    );
}

#[test]
fn forced_pane_freshness_uses_observed_topology_time() {
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

    let mut frame = cache_produced_at(now);
    frame.observed_at_ms = now - 5_000;
    assert!(
        !snapshot_cache_is_fresh(&frame, now, Some(now - 1_000), EVENT_PANE_TTL),
        "a frame freshly republished from stale topology must not satisfy a post-event floor"
    );
}

#[test]
fn read_snapshot_cache_normalizes_missing_observed_stamp() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snapshot.json");
    std::fs::write(
        &path,
        r#"{
            "produced_at_ms": 42,
            "session_name": "rimz-test",
            "tabs": []
        }"#,
    )
    .unwrap();

    let frame = read_snapshot_cache(&path, "rimz-test").expect("same-session frame");

    assert_eq!(frame.observed_at_ms, 42);
}

#[test]
fn published_frame_unwatched_is_session_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();

    assert!(!published_frame_unwatched(&runtime, "rimz-test"));

    let mut cache = assemble_frame(Vec::new(), 1_700_000_000_000, "rimz-test");
    atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &cache).unwrap();
    assert!(published_frame_unwatched(&runtime, "rimz-test"));
    assert!(!published_frame_unwatched(&runtime, "other-session"));

    cache.viewed_panes = vec![pane("terminal_9", "zsh", "/tmp").pane_id];
    atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &cache).unwrap();
    assert!(!published_frame_unwatched(&runtime, "rimz-test"));
}

#[test]
fn read_snapshot_cache_reflects_a_changed_file() {
    // The thread-local parse cache must invalidate when the file changes, or
    // a consumer would serve a stale base forever. Keyed on (mtime, len), so
    // a differently-sized rewrite is caught even if the filesystem's mtime
    // granularity is too coarse to register two fast writes.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snapshot.json");

    let first = assemble_frame(Vec::new(), unix_now_ms(), "rimz-one");
    atomic::write_temp_then_rename_cache(&path, &first).unwrap();
    // Populate this thread's parse cache.
    assert_eq!(
        read_snapshot_cache(&path, "rimz-one").map(|c| c.to_pane_refs().len()),
        Some(0),
    );

    // Republish a longer, different-session frame in place.
    let second = assemble_frame(
        vec![pane("terminal_0", "zsh", "/tmp")],
        unix_now_ms() + 1,
        "rimz-two",
    );
    atomic::write_temp_then_rename_cache(&path, &second).unwrap();
    // The stale (rimz-one) entry must not be served; the fresh frame wins.
    assert!(read_snapshot_cache(&path, "rimz-one").is_none());
    assert_eq!(
        read_snapshot_cache(&path, "rimz-two").map(|c| c.to_pane_refs().len()),
        Some(1),
    );
}

#[test]
fn git_cache_freshness_boundaries_are_inclusive() {
    let entry = DiffStatsCacheEntry {
        refreshed_at_ms: 1_000,
        commit_refreshed_at_ms: Some(1_000),
        added: None,
        removed: None,
        commits: None,
        behind: None,
        trunk: None,
        branch: None,
        clean: None,
        landed: None,
        did_work: None,
        merge_in_progress: None,
        ..DiffStatsCacheEntry::default()
    };
    let fast = DIFF_STATS_TTL.as_millis() as u64;
    let idle = DIFF_STATS_IDLE_TTL.as_millis() as u64;

    assert!(entry.local_fresh_for(1_000 + fast, DIFF_STATS_TTL));
    assert!(!entry.local_fresh_for(1_001 + fast, DIFF_STATS_TTL));
    assert!(entry.commit_fresh_for(1_000 + fast, DIFF_STATS_TTL));
    assert!(!entry.commit_fresh_for(1_001 + fast, DIFF_STATS_TTL));
    assert!(entry.local_fresh_for(1_000 + idle, DIFF_STATS_IDLE_TTL));
    assert!(!entry.local_fresh_for(1_001 + idle, DIFF_STATS_IDLE_TTL));
    // The tiering's whole point: a hot-stale entry is idle-fresh, so an idle
    // worktree skips the forks a hot one pays.
    assert!(entry.local_fresh_for(1_001 + fast, DIFF_STATS_IDLE_TTL));

    // The populated fields round-trip through the cache entry.
    let populated = DiffStatsCacheEntry {
        refreshed_at_ms: 1_000,
        commit_refreshed_at_ms: Some(1_000),
        added: Some(2),
        removed: Some(1),
        commits: Some(4),
        behind: Some(2),
        trunk: Some("main".to_owned()),
        branch: Some("feature-migration".to_owned()),
        clean: Some(true),
        landed: Some(true),
        did_work: Some(true),
        merge_in_progress: Some(false),
        ..DiffStatsCacheEntry::default()
    };
    assert_eq!(
        populated.stats(),
        Some(DiffStats {
            added: 2,
            removed: 1,
        })
    );
    assert_eq!(populated.commits, Some(4));
    assert_eq!(populated.behind, Some(2));
    assert_eq!(populated.trunk.as_deref(), Some("main"));
    assert_eq!(populated.branch.as_deref(), Some("feature-migration"));
    assert_eq!(populated.clean, Some(true));
    assert_eq!(populated.landed, Some(true));

    // An old producer's cache entry predates the `clean`, `landed`, and
    // `from_pr` columns; serde defaults read them back as "not probed" (`None`),
    // never facts it cannot prove.
    let legacy: DiffStatsCacheEntry = serde_json::from_str(
            r#"{"refreshed_at_ms":1000,"added":0,"removed":0,"commits":0,"behind":3,"trunk":"main","branch":"feat"}"#,
        )
        .unwrap();
    assert_eq!(legacy.clean, None);
    assert_eq!(legacy.landed, None);
    assert_eq!(legacy.from_pr, None);
    assert_eq!(legacy.stats(), Some(DiffStats::default()));
    assert!(
        !legacy.commit_fresh_for(1_000, DIFF_STATS_TTL),
        "pre-split entries refresh commit facts once"
    );

    let cache = WorktreeRootsCache {
        refreshed_at_ms: 1_000,
        roots: Vec::new(),
    };
    let ttl = WORKTREE_ROOTS_TTL.as_millis() as u64;
    assert!(cache.is_fresh(1_000 + ttl));
    assert!(!cache.is_fresh(1_001 + ttl));
    // A clock that ran backwards reads fresh (saturating).
    assert!(cache.is_fresh(500));
}
