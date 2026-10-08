use std::fs;

use super::*;
use crate::disk::paths::RuntimePaths;
use crate::ids::WorkspaceId;
use crate::store::Store;

fn paths(dir: &std::path::Path) -> StatePaths {
    let paths = StatePaths::under(WorkspaceId::from_project_root(dir), dir).unwrap();
    paths.ensure_dirs().unwrap();
    fs::create_dir_all(paths.hook_drain_cursor.parent().unwrap()).unwrap();
    paths
}

fn frame() -> HookIngress {
    HookIngress {
        schema_version: "1".into(),
        event_id: EventId::new(),
        ts: "2026-01-01T00:00:00Z".parse().unwrap(),
        source: AgentKind::new_unchecked("codex"),
        event: None,
        payload: "{\"hook_event_name\":\"Stop\",\"text\":\"line\\nλ\"}\n".into(),
        cwd: "/tmp/project".into(),
        hook_pid: 1234,
        env: BTreeMap::from([("RIMZ_RUN_ID".into(), "run-1".into())]),
    }
}

fn seed(paths: &StatePaths, frame: &HookIngress) -> u64 {
    let lock = WorkspaceLock::acquire(&paths.hook_ingress_lock).unwrap();
    append(paths, frame, &lock).unwrap()
}

#[test]
fn frames_round_trip_with_resumable_byte_offsets() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(dir.path());
    let lock = WorkspaceLock::acquire(&paths.hook_ingress_lock).unwrap();
    let first = frame();
    let second = frame();
    let first_end = append(&paths, &first, &lock).unwrap();
    let end = append(&paths, &second, &lock).unwrap();
    drop(lock);
    let (frames, read_end) = read_from_offset(&paths, 0).unwrap();
    assert_eq!(frames, vec![(first, first_end), (second.clone(), end)]);
    assert_eq!(read_end, end);
    assert_eq!(end, fs::metadata(&paths.hook_ingress_log).unwrap().len());
    assert_eq!(
        read_from_offset(&paths, first_end).unwrap(),
        (vec![(second, end)], end)
    );
}

#[test]
fn torn_tail_keeps_the_last_complete_offset() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(dir.path());
    let _lock = WorkspaceLock::acquire(&paths.workspace_lock).unwrap();
    let first = frame();
    let end = seed(&paths, &first);
    crate::disk::atomic::append_record_bytes(&paths.hook_ingress_log, b"20 deadbeef {\"partial\"")
        .unwrap();
    assert_eq!(
        read_from_offset(&paths, 0).unwrap(),
        (vec![(first, end)], end)
    );
}

#[test]
fn corruption_before_another_frame_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(dir.path());
    let _lock = WorkspaceLock::acquire(&paths.workspace_lock).unwrap();
    crate::disk::atomic::append_record_bytes(&paths.hook_ingress_log, b"2 deadbeef {}\n").unwrap();
    seed(&paths, &frame());
    assert!(read_from_offset(&paths, 0).is_err());
}

#[test]
fn cursor_preserves_claim_and_recovers_a_lost_truncation_reset() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(dir.path());
    assert_eq!(read_cursor(&paths).unwrap(), HookDrainCursor::default());
    let lock = WorkspaceLock::acquire(&paths.workspace_lock).unwrap();
    let frame = frame();
    let end = seed(&paths, &frame);
    let cursor = HookDrainCursor {
        applied: end,
        claimed: end,
        applied_event_id: Some(frame.event_id.clone()),
        claimed_event_id: Some(frame.event_id),
        log_extent_at_claim: LogExtent {
            generation: 3,
            offset: 42,
        },
        ..HookDrainCursor::default()
    };
    write_cursor(&paths, &cursor, &lock).unwrap();
    assert_eq!(read_cursor(&paths).unwrap(), cursor);
    crate::disk::atomic::truncate_file(&paths.hook_ingress_log, 0).unwrap();
    assert_eq!(read_cursor(&paths).unwrap(), HookDrainCursor::default());
}

#[test]
fn lost_truncation_reset_does_not_skip_frames_after_regrowth() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(dir.path());
    let lock = WorkspaceLock::acquire(&paths.workspace_lock).unwrap();
    let old = frame();
    let old_end = seed(&paths, &old);
    let cursor: HookDrainCursor = serde_json::from_value(serde_json::json!({
        "version": 1, "applied": old_end, "claimed": old_end,
        "applied_event_id": old.event_id, "claimed_event_id": old.event_id,
        "log_extent_at_claim": { "generation": 0, "offset": 0 }
    }))
    .unwrap();
    write_cursor(&paths, &cursor, &lock).unwrap();
    crate::disk::atomic::truncate_file(&paths.hook_ingress_log, 0).unwrap();
    let first = frame();
    let second = frame();
    let first_end = seed(&paths, &first);
    let end = seed(&paths, &second);
    assert_eq!(first_end, old_end);
    assert!(end > old_end);
    let recovered = read_cursor(&paths).unwrap();
    assert_eq!(recovered, HookDrainCursor::default());
    assert_eq!(
        read_from_offset(&paths, recovered.applied).unwrap().0,
        vec![(first, first_end), (second, end)]
    );
}

#[test]
fn stale_claim_is_rejected_even_when_the_applied_frame_still_matches() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(dir.path());
    let lock = WorkspaceLock::acquire(&paths.workspace_lock).unwrap();
    let first = frame();
    let first_end = seed(&paths, &first);
    let end = seed(&paths, &frame());
    let cursor: HookDrainCursor = serde_json::from_value(serde_json::json!({
        "version": 1, "applied": first_end, "claimed": end,
        "applied_event_id": first.event_id, "claimed_event_id": EventId::new(),
        "log_extent_at_claim": { "generation": 0, "offset": 0 }
    }))
    .unwrap();
    write_cursor(&paths, &cursor, &lock).unwrap();
    assert_eq!(read_cursor(&paths).unwrap(), HookDrainCursor::default());
}

#[test]
fn missing_log_with_an_old_cursor_starts_at_zero() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(dir.path());
    let lock = WorkspaceLock::acquire(&paths.workspace_lock).unwrap();
    let cursor = HookDrainCursor {
        applied: 50,
        claimed: 50,
        ..HookDrainCursor::default()
    };
    write_cursor(&paths, &cursor, &lock).unwrap();
    assert!(paths.hook_drain_cursor.exists());
    assert_eq!(read_cursor(&paths).unwrap(), HookDrainCursor::default());
    assert_eq!(read_from_offset(&paths, 0).unwrap(), (vec![], 0));
}

#[test]
fn invalid_cursor_is_not_silently_replayed() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(dir.path());
    fs::write(&paths.hook_drain_cursor, b"not json").unwrap();
    assert!(read_cursor(&paths).is_err());
    fs::write(&paths.hook_drain_cursor, br#"{"version":2,"applied":0,"claimed":0,"log_extent_at_claim":{"generation":0,"offset":0}}"#).unwrap();
    assert!(read_cursor(&paths).is_err());
}

#[test]
fn truncate_requires_applied_and_claimed_to_equal_the_log_length() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(dir.path());
    let lock = WorkspaceLock::acquire(&paths.workspace_lock).unwrap();
    let first = frame();
    let second = frame();
    let first_end = seed(&paths, &first);
    let end = seed(&paths, &second);
    let event_id = |offset| match offset {
        0 => None,
        offset if offset == first_end => Some(first.event_id.clone()),
        _ => Some(second.event_id.clone()),
    };
    for (applied, claimed) in [
        (0, first_end),
        (first_end, first_end),
        (first_end, end),
        (end, first_end),
    ] {
        let cursor = HookDrainCursor {
            applied,
            claimed,
            applied_event_id: event_id(applied),
            claimed_event_id: event_id(claimed),
            ..HookDrainCursor::default()
        };
        write_cursor(&paths, &cursor, &lock).unwrap();
        assert!(!truncate_drained(&paths, &lock).unwrap());
        assert_eq!(read_cursor(&paths).unwrap(), cursor);
        assert_eq!(fs::metadata(&paths.hook_ingress_log).unwrap().len(), end);
    }
    write_cursor(
        &paths,
        &HookDrainCursor {
            applied: end,
            claimed: end,
            applied_event_id: event_id(end),
            claimed_event_id: event_id(end),
            ..HookDrainCursor::default()
        },
        &lock,
    )
    .unwrap();
    let inode =
        std::os::unix::fs::MetadataExt::ino(&fs::metadata(&paths.hook_ingress_log).unwrap());
    assert!(truncate_drained(&paths, &lock).unwrap());
    assert_eq!(fs::metadata(&paths.hook_ingress_log).unwrap().len(), 0);
    assert_eq!(
        std::os::unix::fs::MetadataExt::ino(&fs::metadata(&paths.hook_ingress_log).unwrap()),
        inode
    );
    assert_eq!(read_cursor(&paths).unwrap(), HookDrainCursor::default());
}

#[test]
fn truncation_waits_for_an_ingress_append_and_keeps_its_frame() {
    use std::sync::mpsc;
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let paths = paths(dir.path());
    let first = frame();
    let end = seed(&paths, &first);
    let workspace = WorkspaceLock::acquire(&paths.workspace_lock).unwrap();
    let cursor = HookDrainCursor {
        applied: end,
        claimed: end,
        applied_event_id: Some(first.event_id.clone()),
        claimed_event_id: Some(first.event_id.clone()),
        ..HookDrainCursor::default()
    };
    write_cursor(&paths, &cursor, &workspace).unwrap();
    drop(workspace);
    let ingress = WorkspaceLock::acquire(&paths.hook_ingress_lock).unwrap();
    let second = frame();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (finished_tx, finished_rx) = mpsc::channel();
    let other = paths.clone();
    let truncater = std::thread::spawn(move || {
        let workspace = WorkspaceLock::acquire(&other.workspace_lock).unwrap();
        entered_tx.send(()).unwrap();
        finished_tx
            .send(truncate_drained(&other, &workspace).unwrap())
            .unwrap();
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let early = finished_rx.recv_timeout(Duration::from_millis(100));
    let second_end = append(&paths, &second, &ingress).unwrap();
    drop(ingress);
    let truncated = early
        .as_ref()
        .copied()
        .unwrap_or_else(|_| finished_rx.recv_timeout(Duration::from_secs(5)).unwrap());
    truncater.join().unwrap();
    assert!(
        early.is_err(),
        "truncation must wait for an ingress appender"
    );
    assert!(!truncated, "the newly appended frame is not applied");
    assert_eq!(
        read_from_offset(&paths, 0).unwrap(),
        (vec![(first, end), (second, second_end)], second_end)
    );
    assert_eq!(read_cursor(&paths).unwrap(), cursor);
}

#[test]
fn reset_removes_ingress_and_cursor_in_both_modes() {
    for hard in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let runtime = RuntimePaths::under(paths.workspace_id.clone(), dir.path()).unwrap();
        let store = Store::open(paths.clone(), runtime).unwrap();
        {
            let _lock = WorkspaceLock::acquire(&paths.workspace_lock).unwrap();
            seed(&paths, &frame());
            crate::disk::atomic::write_temp_then_rename(
                &paths.hook_drain_cursor,
                &HookDrainCursor::default(),
            )
            .unwrap();
        }
        assert!(paths.hook_ingress_log.exists() && paths.hook_drain_cursor.exists());
        store.reset_records(hard).unwrap();
        assert!(!paths.hook_ingress_log.exists());
        assert!(!paths.hook_drain_cursor.exists());
    }
}
