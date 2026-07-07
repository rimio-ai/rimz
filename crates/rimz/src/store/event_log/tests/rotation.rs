use std::time::{Duration, SystemTime};

use tempfile::tempdir;

use super::*;
use crate::store::atomic;

#[test]
fn rotate_skips_missing_or_below_threshold_logs() {
    let dir = tempdir().unwrap();
    let missing = dir.path().join("missing.log.jsonl");
    let missing_archive = dir.path().join("missing.archive");
    let outcome = rotate(&missing, &missing_archive, 1).unwrap();
    assert_eq!(outcome, RotationOutcome::Skipped { current_bytes: 0 });

    let dir = tempdir().unwrap();
    let path = dir.path().join("events.log.jsonl");
    let archive_dir = dir.path().join("events.log.archive");
    append(&path, &test_event("event.emit")).unwrap();

    let outcome = rotate(&path, &archive_dir, 1_000_000).unwrap();
    assert!(matches!(outcome, RotationOutcome::Skipped { current_bytes } if current_bytes > 0));
    assert!(path.exists(), "active log preserved when below threshold");
    assert!(
        !archive_dir.exists(),
        "archive dir not created when skipped"
    );
}

#[test]
fn rotate_renames_active_log_into_archive() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("events.log.jsonl");
    let archive_dir = dir.path().join("events.log.archive");
    append(&path, &test_event("event.emit")).unwrap();

    let outcome = rotate(&path, &archive_dir, 1).unwrap();
    let RotationOutcome::Rotated {
        archive_path,
        bytes_rotated,
    } = outcome
    else {
        panic!("expected rotated outcome");
    };
    assert!(bytes_rotated > 0);
    assert!(!path.exists(), "active log moved");
    assert!(archive_path.exists(), "archive file present");
    let name = archive_path.file_name().unwrap().to_str().unwrap();
    assert!(name.starts_with("events.") && name.ends_with(".jsonl"));

    let archived = read_all(&archive_path).unwrap();
    assert_eq!(methods(&archived), ["event.emit"]);
}

#[test]
fn rotate_syncs_the_log_before_renaming() {
    // The per-record fsync is gone, so the rotation owns making the archive
    // complete: exactly one fdatasync of the log ahead of the rename, then the
    // two directory syncs that make the rename durable.
    let dir = tempdir().unwrap();
    let path = dir.path().join("events.log.jsonl");
    let archive_dir = dir.path().join("events.log.archive");
    append(&path, &test_event("event.emit")).unwrap();

    let before = atomic::testkit::fsync_count();
    let outcome = rotate(&path, &archive_dir, 1).unwrap();
    assert!(outcome.is_rotated());
    assert_eq!(
        atomic::testkit::fsync_count() - before,
        3,
        "one log fdatasync before the rename plus the two directory syncs"
    );
}

#[test]
fn prune_archive_removes_only_stale_files() {
    let dir = tempdir().unwrap();
    let archive_dir = dir.path().join("events.log.archive");
    std::fs::create_dir_all(&archive_dir).unwrap();

    let stale_name = format!("events.{}.jsonl", uuid::Uuid::now_v7().simple());
    let fresh_name = format!("events.{}.jsonl", uuid::Uuid::now_v7().simple());
    let unrelated_name = "operator-notes.txt";
    let stale = archive_dir.join(&stale_name);
    let fresh = archive_dir.join(&fresh_name);
    let unrelated = archive_dir.join(unrelated_name);
    std::fs::write(&stale, b"old\n").unwrap();
    std::fs::write(&fresh, b"new\n").unwrap();
    std::fs::write(&unrelated, b"keep me\n").unwrap();

    let old = SystemTime::now() - Duration::from_secs(7_200);
    std::fs::File::open(&stale)
        .unwrap()
        .set_modified(old)
        .unwrap();
    std::fs::File::open(&unrelated)
        .unwrap()
        .set_modified(old)
        .unwrap();

    let outcome = prune_archive(&archive_dir, Duration::from_secs(3_600)).unwrap();
    assert_eq!(outcome.files_removed, 1);
    assert!(outcome.bytes_removed > 0);
    assert!(!stale.exists());
    assert!(fresh.exists());
    assert!(
        unrelated.exists(),
        "foreign files in archive dir are left alone"
    );
}
