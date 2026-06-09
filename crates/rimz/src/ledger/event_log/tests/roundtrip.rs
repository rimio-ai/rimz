use std::fs;
use std::io::Write;

use tempfile::tempdir;

use super::*;

#[test]
fn append_then_read_round_trip() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("events.log.jsonl");
    let event = test_event("event.emit");
    append(&path, &event).unwrap();
    let events = read_all(&path).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].method, "event.emit");
}

#[test]
fn replace_all_rewrites_framed_log() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("events.log.jsonl");
    let old = test_event("event.old");
    let new = test_event("event.new");

    append(&path, &old).unwrap();
    replace_all(&path, std::slice::from_ref(&new)).unwrap();

    let events = read_all(&path).unwrap();
    assert_eq!(methods(&events), ["event.new"]);
}

#[test]
fn read_from_offset_resume_cases() {
    let dir = tempdir().unwrap();
    let missing = dir.path().join("missing.log.jsonl");
    let (events, end) = read_from_offset(&missing, 64).unwrap();
    assert!(events.is_empty());
    assert_eq!(end, 0, "no log, no extent");

    let path = dir.path().join("events.log.jsonl");
    append(&path, &test_event("event.first")).unwrap();
    let first_end = fs::metadata(&path).unwrap().len();
    append(&path, &test_event("event.second")).unwrap();
    append(&path, &test_event("event.third")).unwrap();
    let full_len = fs::metadata(&path).unwrap().len();

    let (from_zero, zero_end) = read_from_offset(&path, 0).unwrap();
    let all = read_all(&path).unwrap();
    assert_eq!(methods(&from_zero), methods(&all));
    assert_eq!(zero_end, full_len);

    let (delta, delta_end) = read_from_offset(&path, first_end).unwrap();
    assert_eq!(
        methods(&delta),
        ["event.second", "event.third"],
        "resume folds exactly the frames appended past the start offset"
    );
    assert_eq!(delta_end, full_len);
}

#[test]
fn read_from_offset_stops_before_an_inflight_unterminated_tail() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("events.log.jsonl");
    append(&path, &test_event("event.first")).unwrap();
    append(&path, &test_event("event.second")).unwrap();
    let committed = fs::metadata(&path).unwrap().len();
    // A lock-free reader racing a writer mid-append: bytes present, no
    // terminator yet.
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"47 {\"half\":")
        .unwrap();

    let (events, end) = read_from_offset(&path, 0).unwrap();
    assert_eq!(events.len(), 2, "the in-flight frame is not folded");
    assert_eq!(
        end, committed,
        "the extent never claims bytes the fold skipped, so the completing append re-triggers the fold"
    );
}

#[test]
fn read_from_offset_reports_offset_before_a_torn_terminated_tail() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("events.log.jsonl");
    append(&path, &test_event("event.first")).unwrap();
    let committed = fs::metadata(&path).unwrap().len();
    // A power-cut corpse: terminated frame whose claimed length is wrong.
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"999 {\"oops\":true}\n")
        .unwrap();

    let (events, end) = read_from_offset(&path, 0).unwrap();
    assert_eq!(events.len(), 1, "torn trailing record skipped");
    assert_eq!(end, committed, "extent stops at the last complete frame");
}
