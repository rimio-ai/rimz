use std::fs;
use std::io::Write;

use tempfile::tempdir;

use super::*;

#[test]
fn repair_keeps_the_valid_prefix_and_cuts_the_corpse() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("events.log.jsonl");
    append(&path, &test_event("event.first")).unwrap();
    append(&path, &test_event("event.second")).unwrap();
    let committed = fs::metadata(&path).unwrap().len();
    // A power-cut corpse mid-file: a zeroed frame followed by a valid one.
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"999 deadbeef {\"oops\":true}\n")
        .unwrap();
    append(&path, &test_event("event.third")).unwrap();
    let total = fs::metadata(&path).unwrap().len();
    assert!(read_all(&path).is_err(), "pre-repair reads hard-error");

    let outcome = repair(&path).unwrap();
    assert_eq!(
        outcome,
        RepairOutcome {
            frames_kept: 2,
            bytes_truncated: total - committed,
        }
    );
    assert_eq!(
        fs::metadata(&path).unwrap().len(),
        committed,
        "cut lands exactly at the last valid frame"
    );
    let events = read_all(&path).unwrap();
    assert_eq!(
        methods(&events),
        ["event.first", "event.second"],
        "the valid prefix survives; frames behind the hole are cut"
    );
}

#[test]
fn repair_cuts_an_invalid_tail_frame_too() {
    // Under the workspace lock no append can be in flight, so an unterminated
    // tail is always a corpse — repair is stricter than the tolerant read.
    let dir = tempdir().unwrap();
    let path = dir.path().join("events.log.jsonl");
    append(&path, &test_event("event.first")).unwrap();
    let committed = fs::metadata(&path).unwrap().len();
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"47 {\"half\":")
        .unwrap();

    let outcome = repair(&path).unwrap();
    assert_eq!(outcome.frames_kept, 1);
    assert!(outcome.truncated());
    assert_eq!(fs::metadata(&path).unwrap().len(), committed);
}

#[test]
fn repair_of_an_intact_or_missing_log_is_a_noop() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("events.log.jsonl");
    assert_eq!(repair(&path).unwrap(), RepairOutcome::default());

    append(&path, &test_event("event.first")).unwrap();
    let len = fs::metadata(&path).unwrap().len();
    let outcome = repair(&path).unwrap();
    assert_eq!(
        outcome,
        RepairOutcome {
            frames_kept: 1,
            bytes_truncated: 0,
        }
    );
    assert_eq!(fs::metadata(&path).unwrap().len(), len, "log untouched");
}
