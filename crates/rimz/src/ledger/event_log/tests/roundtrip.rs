use std::fs;
use std::io::Write;

use tempfile::tempdir;

use super::*;

#[test]
fn append_read_and_replace_round_trip() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("events.log.jsonl");
    let old = test_event("event.emit");
    let new = test_event("event.new");

    append(&path, &old).unwrap();
    assert_eq!(methods(&read_all(&path).unwrap()), ["event.emit"]);

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
fn read_from_offset_stops_before_inflight_or_torn_tail() {
    for (label, tail) in [
        ("in-flight unterminated tail", b"47 {\"half\":".as_slice()),
        ("torn terminated tail", b"999 {\"oops\":true}\n".as_slice()),
    ] {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.log.jsonl");
        append(&path, &test_event("event.first")).unwrap();
        append(&path, &test_event("event.second")).unwrap();
        let committed = fs::metadata(&path).unwrap().len();
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(tail)
            .unwrap();

        let (events, end) = read_from_offset(&path, 0).unwrap();
        assert_eq!(events.len(), 2, "{label}: tail is not folded");
        assert_eq!(
            end, committed,
            "{label}: extent stops at the last complete frame"
        );
    }
}
