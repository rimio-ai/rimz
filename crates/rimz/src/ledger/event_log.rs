//! Length-framed append-only event log.
//!
//! `events.log.jsonl` is the canonical history of everything that happened in
//! the workspace. Rotation, retention, and snapshot-vs-log reconciliation
//! belong in later phases; this module owns framing and recovery.

use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde::Serialize;
use tracing::warn;

use crate::ledger::atomic::{self, append_framed_record};
use crate::schema::event::EventEnvelope;

#[derive(Debug, thiserror::Error)]
pub enum EventLogErr {
    #[error("torn record at offset {offset}: {reason}")]
    Torn { offset: u64, reason: String },
    #[error("frame length mismatch at offset {offset}: claimed {claimed}, available {available}")]
    FrameLength {
        offset: u64,
        claimed: u64,
        available: u64,
    },
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub type Result<T> = std::result::Result<T, EventLogErr>;

#[must_use = "durability barrier; check the result"]
pub fn append<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    Ok(append_framed_record(path, value)?)
}

/// Read every parseable record. A torn trailing record (length mismatch or
/// JSON parse failure) is logged at `warn` and skipped; we never propagate
/// it as a hard error because that's what a process kill mid-write leaves.
pub fn read_all(path: &Path) -> Result<Vec<EventEnvelope>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(path).map_err(|e| EventLogErr::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let reader = BufReader::new(file);
    let mut rows = Vec::new();
    let mut offset: u64 = 0;
    for (line_no, line) in reader.lines().enumerate() {
        let line = line.map_err(|source| EventLogErr::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let line_len = line.len() as u64 + 1; // include trailing '\n'
        rows.push((line_no, offset, line));
        offset += line_len;
    }

    let mut events = Vec::new();
    let last_index = rows.len().saturating_sub(1);
    for (idx, (line_no, offset, line)) in rows.into_iter().enumerate() {
        match decode_line(&line, offset) {
            Ok(event) => events.push(event),
            Err(EventLogErr::Torn { offset: at, reason }) if idx == last_index => {
                warn!(
                    offset = at,
                    line = line_no,
                    reason = %reason,
                    "skipping torn trailing event-log record"
                );
            }
            Err(EventLogErr::Torn { offset, reason }) => {
                return Err(EventLogErr::Torn { offset, reason });
            }
            Err(EventLogErr::FrameLength {
                offset: at,
                claimed,
                available,
            }) if idx == last_index => {
                warn!(
                    offset = at,
                    line = line_no,
                    claimed,
                    available,
                    "skipping trailing event-log record with frame length mismatch"
                );
            }
            Err(EventLogErr::FrameLength {
                offset,
                claimed,
                available,
            }) => {
                return Err(EventLogErr::FrameLength {
                    offset,
                    claimed,
                    available,
                });
            }
            Err(other) => return Err(other),
        }
    }
    Ok(events)
}

fn decode_line(line: &str, offset: u64) -> Result<EventEnvelope> {
    let (len, payload) = line.split_once(' ').ok_or_else(|| EventLogErr::Torn {
        offset,
        reason: "no length prefix".into(),
    })?;
    let claimed: u64 = len.parse().map_err(|_| EventLogErr::Torn {
        offset,
        reason: format!("bad length `{len}`"),
    })?;
    let available = payload.len() as u64;
    if claimed != available {
        return Err(EventLogErr::FrameLength {
            offset,
            claimed,
            available,
        });
    }
    serde_json::from_str(payload).map_err(|e| EventLogErr::Torn {
        offset,
        reason: format!("json: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::ids::WorkspaceId;

    #[test]
    fn append_then_read_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.log.jsonl");
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let event = EventEnvelope::new(
            workspace,
            "session",
            "rimz",
            "cli",
            "event.emit",
            json!({ "a": 1 }),
        );
        append(&path, &event).unwrap();
        let events = read_all(&path).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].method, "event.emit");
    }

    #[test]
    fn torn_trailing_record_is_skipped() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.log.jsonl");
        // Write one good record then a torn one (claimed length larger than body).
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/y"));
        let event = EventEnvelope::new(
            workspace,
            "session",
            "rimz",
            "cli",
            "event.emit",
            json!({ "a": 1 }),
        );
        append(&path, &event).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"999 {\"oops\":true}\n")
            .unwrap();
        let events = read_all(&path).unwrap();
        assert_eq!(events.len(), 1, "torn trailing record skipped");
    }

    #[test]
    fn torn_middle_record_is_an_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.log.jsonl");
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/y"));
        let event = EventEnvelope::new(
            workspace,
            "session",
            "rimz",
            "cli",
            "event.emit",
            json!({ "a": 1 }),
        );
        append(&path, &event).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"999 {\"oops\":true}\n")
            .unwrap();
        append(&path, &event).unwrap();

        let err = read_all(&path).unwrap_err();
        assert!(matches!(err, EventLogErr::FrameLength { .. }));
    }
}
