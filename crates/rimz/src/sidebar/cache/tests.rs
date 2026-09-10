use super::*;
use crate::disk::atomic;
use crate::ids::WorkspaceId;
use crate::mux::PRESENCE_STAMP_FRESH;
use crate::sidebar::frame::assemble_frame;
use crate::sidebar::test_support::pane;
use crate::sidebar::timing::{EVENT_PANE_TTL, SNAPSHOT_CACHE_TTL};
use crate::utils::time::unix_now_ms;

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
    write_presence_stamp(&runtime, crate::MuxName::Tmux, Some("rimz-test"));
    let age = presence_stamp_age_ms(&runtime).expect("stamp written and readable");
    assert!(
        age < 1_000,
        "a just-written stamp reads as young, got {age}ms"
    );
    assert!(presence_event_mode(Some(age)));
    assert_eq!(
        read_presence_stamp(&runtime),
        Some(PresenceStamp {
            written_at_ms: read_presence_stamp(&runtime).unwrap().written_at_ms,
            mux: Some(crate::MuxName::Tmux),
            session_name: Some("rimz-test".to_owned()),
        })
    );
}

#[test]
fn presence_stamp_identity_is_backward_compatible() {
    let legacy: PresenceStamp = serde_json::from_str(r#"{"written_at_ms":42}"#).unwrap();
    assert_eq!(
        legacy,
        PresenceStamp {
            written_at_ms: 42,
            mux: None,
            session_name: None,
        }
    );
    assert_eq!(
        serde_json::to_string(&legacy).unwrap(),
        r#"{"written_at_ms":42}"#
    );
}

#[test]
fn presence_stamp_age_handles_clock_skew_and_bad_files() {
    let future_dir = tempfile::tempdir().unwrap();
    let future_workspace = WorkspaceId::from_project_root(future_dir.path());
    let future_runtime = RuntimePaths::under(future_workspace, future_dir.path()).unwrap();
    let future = PresenceStamp {
        written_at_ms: unix_now_ms() + 60_000,
        mux: None,
        session_name: None,
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

#[test]
fn presence_probe_stamp_round_trips_and_rejects_bad_files() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();

    assert_eq!(read_presence_probe_stamp(&runtime), None);
    let future = unix_now_ms() + 60_000;
    write_presence_probe_stamp(&runtime, future).unwrap();
    assert_eq!(read_presence_probe_stamp(&runtime), Some(future));

    std::fs::write(presence_probe_stamp_path(&runtime), b"{ not json").unwrap();
    assert_eq!(read_presence_probe_stamp(&runtime), None);
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
