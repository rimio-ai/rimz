use std::fs;

use tempfile::tempdir;

use super::*;

#[test]
fn pre_crc_frame_remains_readable() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("events.log.jsonl");
    let event = test_event("event.emit");
    let payload = serde_json::to_vec(&event).unwrap();
    let mut frame = format!("{} ", payload.len()).into_bytes();
    frame.extend_from_slice(&payload);
    frame.push(b'\n');
    fs::write(&path, frame).unwrap();

    assert_eq!(read_all(&path).unwrap(), vec![event]);
}

#[test]
fn frame_wire_format_is_len_crc_payload() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("events.log.jsonl");
    append(&path, &test_event("event.emit")).unwrap();

    let text = fs::read_to_string(&path).unwrap();
    let line = text.strip_suffix('\n').unwrap();
    let (len, rest) = line.split_once(' ').unwrap();
    let (crc, payload) = rest.split_once(' ').unwrap();
    assert_eq!(len.parse::<usize>().unwrap(), payload.len());
    assert_eq!(crc.len(), 8);
    assert_eq!(
        u32::from_str_radix(crc, 16).unwrap(),
        crc32fast::hash(payload.as_bytes()),
        "the crc token is the payload's crc32 in lowercase hex"
    );
}

#[test]
fn crc_mismatch_is_a_skipped_tail_and_a_hard_middle_error() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("events.log.jsonl");
    append(&path, &test_event("event.first")).unwrap();
    let committed = fs::metadata(&path).unwrap().len();
    append(&path, &test_event("event.second")).unwrap();

    // Flip one payload byte of the trailing frame in place — the length still
    // matches, only the CRC catches it.
    let mut bytes = fs::read(&path).unwrap();
    let len = bytes.len();
    let flip = len - 3; // inside the tail frame's JSON payload
    bytes[flip] = if bytes[flip] == b'x' { b'y' } else { b'x' };
    fs::write(&path, &bytes).unwrap();

    let (events, end) = read_from_offset(&path, 0).unwrap();
    assert_eq!(events.len(), 1, "the corrupt tail frame is skipped");
    assert_eq!(end, committed);

    // The same corruption mid-file is a hard error.
    append(&path, &test_event("event.third")).unwrap();
    let err = read_all(&path).unwrap_err();
    assert!(matches!(err, EventLogErr::Crc { .. }), "got {err:?}");
    assert!(err.is_corruption());
}
