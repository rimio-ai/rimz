//! Length-framed append-only event log.
//!
//! `events.log.jsonl` is the canonical history of everything that happened in
//! the workspace. This module owns framing, recovery, rotation, and archive
//! retention; snapshot reconciliation across rotations lives in
//! [`crate::ledger::snapshot`].

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;
use tracing::warn;
use uuid::Uuid;

use crate::ledger::atomic::{self, append_framed_record, sync_dir};
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RotationOutcome {
    /// Active log is below the configured threshold; no archive written.
    Skipped { current_bytes: u64 },
    /// Active log was atomically renamed into the archive directory.
    Rotated {
        archive_path: PathBuf,
        bytes_rotated: u64,
    },
}

impl RotationOutcome {
    pub fn current_bytes(&self) -> u64 {
        match self {
            Self::Skipped { current_bytes } => *current_bytes,
            Self::Rotated { bytes_rotated, .. } => *bytes_rotated,
        }
    }

    pub fn is_rotated(&self) -> bool {
        matches!(self, Self::Rotated { .. })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PruneOutcome {
    pub files_removed: usize,
    pub bytes_removed: u64,
}

/// Rotate `events_log` into `archive_dir` when it crosses `min_bytes`.
///
/// The active log is renamed (atomic on the same filesystem) to
/// `events.<uuidv7>.jsonl` inside the archive directory; UUIDv7 names sort
/// chronologically. Returns `Skipped` if the log is missing, empty, or
/// below the threshold so the caller can decide whether to prune archives
/// without a full lock dance.
#[must_use = "durability barrier; check the result"]
pub fn rotate(events_log: &Path, archive_dir: &Path, min_bytes: u64) -> Result<RotationOutcome> {
    let current_bytes = match fs::metadata(events_log) {
        Ok(meta) => meta.len(),
        Err(err) if err.kind() == io::ErrorKind::NotFound => 0,
        Err(source) => {
            return Err(EventLogErr::Io {
                path: events_log.to_path_buf(),
                source,
            });
        }
    };
    if current_bytes == 0 || current_bytes < min_bytes {
        return Ok(RotationOutcome::Skipped { current_bytes });
    }

    fs::create_dir_all(archive_dir).map_err(|source| EventLogErr::Io {
        path: archive_dir.to_path_buf(),
        source,
    })?;

    let name = format!("events.{}.jsonl", Uuid::now_v7().simple());
    let archive_path = archive_dir.join(&name);
    fs::rename(events_log, &archive_path).map_err(|source| EventLogErr::Io {
        path: events_log.to_path_buf(),
        source,
    })?;
    if let Some(parent) = events_log.parent() {
        sync_dir(parent)?;
    }
    sync_dir(archive_dir)?;

    Ok(RotationOutcome::Rotated {
        archive_path,
        bytes_rotated: current_bytes,
    })
}

/// Remove archived event logs older than `older_than`.
///
/// Only files whose names match the archive pattern (`events.<uuid>.jsonl`)
/// are considered; foreign files are ignored so a misconfigured operator
/// can't lose data by dropping unrelated content into the archive
/// directory.
#[must_use = "maintenance report; surface it to the caller"]
pub fn prune_archive(archive_dir: &Path, older_than: Duration) -> Result<PruneOutcome> {
    let entries = match fs::read_dir(archive_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(PruneOutcome::default()),
        Err(source) => {
            return Err(EventLogErr::Io {
                path: archive_dir.to_path_buf(),
                source,
            });
        }
    };

    let now = SystemTime::now();
    let mut report = PruneOutcome::default();
    for entry in entries {
        let entry = entry.map_err(|source| EventLogErr::Io {
            path: archive_dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if !is_archive_name(&path) {
            continue;
        }
        let meta = fs::symlink_metadata(&path).map_err(|source| EventLogErr::Io {
            path: path.clone(),
            source,
        })?;
        let modified = meta.modified().map_err(|source| EventLogErr::Io {
            path: path.clone(),
            source,
        })?;
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        if age < older_than {
            continue;
        }
        let bytes = meta.len();
        fs::remove_file(&path).map_err(|source| EventLogErr::Io {
            path: path.clone(),
            source,
        })?;
        report.files_removed += 1;
        report.bytes_removed = report.bytes_removed.saturating_add(bytes);
    }
    Ok(report)
}

fn is_archive_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("events.") && name.ends_with(".jsonl"))
}

#[must_use = "durability barrier; check the result"]
pub fn replace_all(path: &Path, events: &[EventEnvelope]) -> Result<()> {
    let mut bytes = Vec::new();
    for event in events {
        let payload = serde_json::to_vec(event).map_err(atomic::AtomicErr::Json)?;
        bytes.extend_from_slice(payload.len().to_string().as_bytes());
        bytes.push(b' ');
        bytes.extend_from_slice(&payload);
        bytes.push(b'\n');
    }
    atomic::write_bytes_atomically(path, &bytes)?;
    Ok(())
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
    fn replace_all_rewrites_framed_log() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.log.jsonl");
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let old = EventEnvelope::new(
            workspace.clone(),
            "session",
            "rimz",
            "cli",
            "event.old",
            json!({ "a": 1 }),
        );
        let new = EventEnvelope::new(
            workspace,
            "session",
            "rimz",
            "cli",
            "event.new",
            json!({ "b": 2 }),
        );

        append(&path, &old).unwrap();
        replace_all(&path, std::slice::from_ref(&new)).unwrap();

        let events = read_all(&path).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].method, "event.new");
    }

    #[test]
    fn rotate_skips_when_below_threshold() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.log.jsonl");
        let archive_dir = dir.path().join("events.log.archive");
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/z"));
        let event = EventEnvelope::new(
            workspace,
            "session",
            "rimz",
            "cli",
            "event.emit",
            json!({ "a": 1 }),
        );
        append(&path, &event).unwrap();

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
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/z"));
        let event = EventEnvelope::new(
            workspace,
            "session",
            "rimz",
            "cli",
            "event.emit",
            json!({ "a": 1 }),
        );
        append(&path, &event).unwrap();

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
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].method, "event.emit");
    }

    #[test]
    fn rotate_missing_active_log_is_a_noop() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.log.jsonl");
        let archive_dir = dir.path().join("events.log.archive");
        let outcome = rotate(&path, &archive_dir, 1).unwrap();
        assert_eq!(outcome, RotationOutcome::Skipped { current_bytes: 0 });
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
