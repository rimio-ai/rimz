//! Length-framed append-only event log.
//!
//! `events.log.jsonl` is the canonical history of everything that happened in
//! the workspace. This module owns framing, recovery, rotation, and archive
//! retention; snapshot reconciliation across rotations lives in
//! [`crate::ledger::snapshot`].

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::ledger::atomic::{self, sync_dir};
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
    #[error("crc mismatch at offset {offset}: claimed {claimed:08x}, computed {computed:08x}")]
    Crc {
        offset: u64,
        claimed: u32,
        computed: u32,
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

impl EventLogErr {
    /// A frame-level corruption a [`repair`] truncation heals — distinct from
    /// an environment failure (io, serialization) repair cannot help.
    pub fn is_corruption(&self) -> bool {
        matches!(
            self,
            Self::Torn { .. } | Self::FrameLength { .. } | Self::Crc { .. }
        )
    }
}

pub type Result<T> = std::result::Result<T, EventLogErr>;

/// The active-log extent a derived rollup reflects: the rotation generation
/// and the byte offset after the last folded frame. This is the snapshot
/// freshness stamp — a cached rollup is served exactly when its extent
/// matches the live log, an O(1) stat with none of mtime's granularity or
/// write-ordering hazards.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogExtent {
    pub generation: u64,
    pub offset: u64,
}

#[must_use = "durability barrier; check the result"]
pub fn append<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let payload = serde_json::to_vec(value).map_err(atomic::AtomicErr::Json)?;
    Ok(atomic::append_record_bytes(path, &encode_frame(&payload))?)
}

/// Encode one frame: `<decimal payload length> <crc32 of the payload,
/// 8 lowercase hex chars> <payload>\n`. The CRC covers the payload bytes
/// alone — the length is validated structurally on read — and makes
/// post-power-cut recovery deterministic: a frame whose content writeback
/// was lost reads as `Crc`, never as a JSON parse coin-flip. One encoder for
/// the per-record append and the wholesale rewrite, beside its decoder
/// ([`decode_line`]), so the format cannot drift. The decoder also accepts
/// the pre-CRC two-field form; rotation ages legacy frames out.
fn encode_frame(payload: &[u8]) -> Vec<u8> {
    let mut line = Vec::with_capacity(payload.len() + 24);
    line.extend_from_slice(payload.len().to_string().as_bytes());
    line.push(b' ');
    line.extend_from_slice(format!("{:08x}", crc32fast::hash(payload)).as_bytes());
    line.push(b' ');
    line.extend_from_slice(payload);
    line.push(b'\n');
    line
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

    // Relaxed appends leave the newest frames riding the page cache; flush
    // them before the rename publishes this file as the immutable archive.
    atomic::sync_file_data(events_log)?;
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
        bytes.extend_from_slice(&encode_frame(&payload));
    }
    atomic::write_bytes_atomically(path, &bytes)?;
    Ok(())
}

/// Read every parseable record. A torn trailing record (length mismatch or
/// JSON parse failure) is logged and skipped; we never propagate it as a
/// hard error because that's what a power cut mid-append leaves behind.
pub fn read_all(path: &Path) -> Result<Vec<EventEnvelope>> {
    Ok(read_from_offset(path, 0)?.0)
}

/// Read every parseable record starting at byte `start` — the incremental
/// twin of [`read_all`] for a reader resuming from a persisted fold base.
///
/// Returns the events and the offset after the last complete frame: the
/// extent a derived rollup may claim to reflect. An unterminated or
/// undecodable tail frame is not yet committed (an in-flight append, or a
/// power-cut corpse), so reading stops in front of it and the returned
/// offset never claims bytes the fold skipped — the append that completes
/// or follows it moves the live extent past the stamp and triggers the next
/// fold. A torn record *followed by more frames* is corruption and stays a
/// hard error.
pub fn read_from_offset(path: &Path, start: u64) -> Result<(Vec<EventEnvelope>, u64)> {
    if !path.exists() {
        // No log, no extent — a fresh workspace folds nothing.
        return Ok((Vec::new(), 0));
    }
    let rows = read_rows(path, start)?;
    let mut events = Vec::new();
    let mut end = start;
    let last_index = rows.len().saturating_sub(1);
    for (idx, (at, terminated, bytes)) in rows.into_iter().enumerate() {
        let frame_len = bytes.len() as u64 + u64::from(terminated);
        match decode_row(at, terminated, &bytes) {
            Ok(event) => {
                events.push(event);
                end = at + frame_len;
            }
            Err(err) if err.is_corruption() && idx == last_index => {
                if terminated {
                    warn!(offset = at, error = %err, "skipping torn trailing event-log record");
                } else {
                    // An in-flight append a lock-free reader raced — folded by
                    // the wakeup that follows its completion. Routine, not noise.
                    debug!(offset = at, "stopping before an in-flight tail frame");
                }
                break;
            }
            Err(err) => return Err(err),
        }
    }
    Ok((events, end))
}

/// Split the log into raw `(offset, terminated, line bytes)` rows from byte
/// `start` — the scan [`read_from_offset`] folds and [`repair`] validates.
fn read_rows(path: &Path, start: u64) -> Result<Vec<(u64, bool, Vec<u8>)>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut file = File::open(path).map_err(|e| EventLogErr::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    file.seek(SeekFrom::Start(start))
        .map_err(|source| EventLogErr::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let mut reader = BufReader::new(file);
    let mut rows: Vec<(u64, bool, Vec<u8>)> = Vec::new();
    let mut offset = start;
    loop {
        let mut buf = Vec::new();
        let read = reader
            .read_until(b'\n', &mut buf)
            .map_err(|source| EventLogErr::Io {
                path: path.to_path_buf(),
                source,
            })?;
        if read == 0 {
            break;
        }
        let terminated = buf.last() == Some(&b'\n');
        if terminated {
            buf.pop();
        }
        rows.push((offset, terminated, buf));
        offset += read as u64;
    }
    testkit::count_bytes_read(offset - start);
    Ok(rows)
}

/// Test-only observability seam: bytes the row scan actually read, so the
/// performance tier can prove a warm fold is O(new bytes) rather than
/// O(log) from the integration binary. Per-process and relaxed, like
/// [`crate::ledger::atomic::testkit`].
#[doc(hidden)]
pub mod testkit {
    use std::sync::atomic::{AtomicU64, Ordering};

    static BYTES_READ: AtomicU64 = AtomicU64::new(0);

    /// Event-log bytes scanned since process start.
    pub fn bytes_read() -> u64 {
        BYTES_READ.load(Ordering::Relaxed)
    }

    pub(super) fn count_bytes_read(n: u64) {
        BYTES_READ.fetch_add(n, Ordering::Relaxed);
    }
}

/// Decode one raw row into its event: unterminated and non-UTF-8 rows read
/// as torn, terminated ones go through the frame decoder.
fn decode_row(at: u64, terminated: bool, bytes: &[u8]) -> Result<EventEnvelope> {
    if !terminated {
        return Err(EventLogErr::Torn {
            offset: at,
            reason: "unterminated frame".into(),
        });
    }
    match std::str::from_utf8(bytes) {
        Ok(line) => decode_line(line, at),
        Err(err) => Err(EventLogErr::Torn {
            offset: at,
            reason: format!("utf8: {err}"),
        }),
    }
}

fn decode_line(line: &str, offset: u64) -> Result<EventEnvelope> {
    let (len, rest) = line.split_once(' ').ok_or_else(|| EventLogErr::Torn {
        offset,
        reason: "no length prefix".into(),
    })?;
    let claimed: u64 = len.parse().map_err(|_| EventLogErr::Torn {
        offset,
        reason: format!("bad length `{len}`"),
    })?;
    // CRC form `<len> <crc> <json>` vs the pre-CRC `<len> <json>`: an 8-char
    // lowercase-hex second token is the CRC — a JSON payload always opens
    // with `{`, so the forms cannot be confused. A mis-split would shift the
    // payload by nine bytes and fail the length check below, erring safe.
    let (crc, payload) = match rest.split_once(' ') {
        Some((token, payload))
            if token.len() == 8
                && token
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) =>
        {
            // The guard admits only 8 lowercase-hex bytes, so the parse holds.
            (u32::from_str_radix(token, 16).ok(), payload)
        }
        _ => (None, rest),
    };
    let available = payload.len() as u64;
    if claimed != available {
        return Err(EventLogErr::FrameLength {
            offset,
            claimed,
            available,
        });
    }
    if let Some(claimed_crc) = crc {
        let computed = crc32fast::hash(payload.as_bytes());
        if claimed_crc != computed {
            return Err(EventLogErr::Crc {
                offset,
                claimed: claimed_crc,
                computed,
            });
        }
    }
    serde_json::from_str(payload).map_err(|e| EventLogErr::Torn {
        offset,
        reason: format!("json: {e}"),
    })
}

/// What [`repair`] found and cut.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RepairOutcome {
    /// Valid frames surviving ahead of the cut (the whole log when intact).
    pub frames_kept: usize,
    /// Bytes removed from the first invalid frame to end of file.
    pub bytes_truncated: u64,
    /// Offset the log was cut at; `None` when the log was already intact.
    pub truncated_at: Option<u64>,
}

/// Truncate the log at its first invalid frame — the post-power-cut corpse a
/// mid-file framing/CRC error indicates — keeping the valid prefix. Frames
/// behind the cut are lost; resyncing past a hole would need frame magic the
/// format deliberately omits (see the rejected `O_APPEND` candidate in
/// performance.md). An intact, empty, or missing log is a no-op.
///
/// Stricter than [`read_from_offset`]: an invalid *tail* frame is cut too.
/// Caller holds the workspace lock, so no append can be in flight — under
/// the lock an incomplete tail is always a corpse, never a race.
#[must_use = "durability barrier; check the result"]
pub fn repair(path: &Path) -> Result<RepairOutcome> {
    let rows = read_rows(path, 0)?;
    let mut frames_kept = 0usize;
    let mut valid_end = 0u64;
    let mut invalid_at: Option<u64> = None;
    for (at, terminated, bytes) in rows {
        match decode_row(at, terminated, &bytes) {
            Ok(_) => {
                frames_kept += 1;
                valid_end = at + bytes.len() as u64 + u64::from(terminated);
            }
            Err(err) if err.is_corruption() => {
                invalid_at = Some(at);
                break;
            }
            Err(err) => return Err(err),
        }
    }
    let Some(at) = invalid_at else {
        return Ok(RepairOutcome {
            frames_kept,
            ..RepairOutcome::default()
        });
    };
    debug_assert_eq!(at, valid_end, "frames are contiguous");
    let total = fs::metadata(path)
        .map_err(|source| EventLogErr::Io {
            path: path.to_path_buf(),
            source,
        })?
        .len();
    atomic::truncate_file(path, at)?;
    Ok(RepairOutcome {
        frames_kept,
        bytes_truncated: total.saturating_sub(at),
        truncated_at: Some(at),
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
    fn rotate_syncs_the_log_before_renaming() {
        // The per-record fsync is gone, so the rotation owns making the
        // archive complete: exactly one fdatasync of the log ahead of the
        // rename, then the two directory syncs that make the rename durable.
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

    fn test_event(method: &str) -> EventEnvelope {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        EventEnvelope::new(
            workspace,
            "session",
            "rimz",
            "cli",
            method,
            json!({ "a": 1 }),
        )
    }

    #[test]
    fn read_from_offset_resumes_after_complete_frames() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.log.jsonl");
        append(&path, &test_event("event.first")).unwrap();
        let first_end = fs::metadata(&path).unwrap().len();
        append(&path, &test_event("event.second")).unwrap();
        append(&path, &test_event("event.third")).unwrap();
        let full_len = fs::metadata(&path).unwrap().len();

        let (delta, end) = read_from_offset(&path, first_end).unwrap();
        assert_eq!(
            delta.iter().map(|e| e.method.as_str()).collect::<Vec<_>>(),
            ["event.second", "event.third"],
            "resume folds exactly the frames appended past the start offset"
        );
        assert_eq!(
            end, full_len,
            "extent advances to the end of the last complete frame"
        );
    }

    #[test]
    fn read_from_offset_zero_matches_read_all() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.log.jsonl");
        append(&path, &test_event("event.first")).unwrap();
        append(&path, &test_event("event.second")).unwrap();
        let full_len = fs::metadata(&path).unwrap().len();

        let (from_zero, end) = read_from_offset(&path, 0).unwrap();
        let all = read_all(&path).unwrap();
        assert_eq!(
            from_zero.iter().map(|e| &e.method).collect::<Vec<_>>(),
            all.iter().map(|e| &e.method).collect::<Vec<_>>(),
        );
        assert_eq!(end, full_len);
    }

    #[test]
    fn read_from_offset_on_a_missing_log_is_empty_at_zero() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.log.jsonl");
        let (events, end) = read_from_offset(&path, 64).unwrap();
        assert!(events.is_empty());
        assert_eq!(
            end, 0,
            "no log, no extent — a fresh workspace folds nothing"
        );
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
        std::fs::OpenOptions::new()
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
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"999 {\"oops\":true}\n")
            .unwrap();

        let (events, end) = read_from_offset(&path, 0).unwrap();
        assert_eq!(events.len(), 1, "torn trailing record skipped");
        assert_eq!(end, committed, "extent stops at the last complete frame");
    }

    #[test]
    fn frame_wire_format_is_len_crc_payload() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.log.jsonl");
        append(&path, &test_event("event.emit")).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
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
    fn legacy_two_field_frames_still_decode() {
        // A log written before the CRC field: `<len> <json>` frames decode
        // unchanged, and a mixed log (legacy prefix, CRC suffix) folds whole.
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.log.jsonl");
        let payload = serde_json::to_vec(&test_event("event.legacy")).unwrap();
        let mut legacy = Vec::new();
        legacy.extend_from_slice(payload.len().to_string().as_bytes());
        legacy.push(b' ');
        legacy.extend_from_slice(&payload);
        legacy.push(b'\n');
        std::fs::write(&path, &legacy).unwrap();
        append(&path, &test_event("event.new")).unwrap();

        let events = read_all(&path).unwrap();
        assert_eq!(
            events.iter().map(|e| e.method.as_str()).collect::<Vec<_>>(),
            ["event.legacy", "event.new"],
        );
    }

    #[test]
    fn crc_mismatch_is_a_skipped_tail_and_a_hard_middle_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.log.jsonl");
        append(&path, &test_event("event.first")).unwrap();
        let committed = fs::metadata(&path).unwrap().len();
        append(&path, &test_event("event.second")).unwrap();

        // Flip one payload byte of the trailing frame in place — the length
        // still matches, only the CRC catches it.
        let mut bytes = std::fs::read(&path).unwrap();
        let len = bytes.len();
        let flip = len - 3; // inside the tail frame's JSON payload
        bytes[flip] = if bytes[flip] == b'x' { b'y' } else { b'x' };
        std::fs::write(&path, &bytes).unwrap();

        let (events, end) = read_from_offset(&path, 0).unwrap();
        assert_eq!(events.len(), 1, "the corrupt tail frame is skipped");
        assert_eq!(end, committed);

        // The same corruption mid-file is a hard error.
        append(&path, &test_event("event.third")).unwrap();
        let err = read_all(&path).unwrap_err();
        assert!(matches!(err, EventLogErr::Crc { .. }), "got {err:?}");
        assert!(err.is_corruption());
    }

    #[test]
    fn repair_keeps_the_valid_prefix_and_cuts_the_corpse() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.log.jsonl");
        append(&path, &test_event("event.first")).unwrap();
        append(&path, &test_event("event.second")).unwrap();
        let committed = fs::metadata(&path).unwrap().len();
        // A power-cut corpse mid-file: a zeroed frame followed by a valid one.
        std::fs::OpenOptions::new()
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
                truncated_at: Some(committed),
            }
        );
        let events = read_all(&path).unwrap();
        assert_eq!(
            events.iter().map(|e| e.method.as_str()).collect::<Vec<_>>(),
            ["event.first", "event.second"],
            "the valid prefix survives; frames behind the hole are cut"
        );
    }

    #[test]
    fn repair_cuts_an_invalid_tail_frame_too() {
        // Under the workspace lock no append can be in flight, so an
        // unterminated tail is always a corpse — repair is stricter than the
        // tolerant read.
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.log.jsonl");
        append(&path, &test_event("event.first")).unwrap();
        let committed = fs::metadata(&path).unwrap().len();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"47 {\"half\":")
            .unwrap();

        let outcome = repair(&path).unwrap();
        assert_eq!(outcome.frames_kept, 1);
        assert_eq!(outcome.truncated_at, Some(committed));
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
                truncated_at: None,
            }
        );
        assert_eq!(fs::metadata(&path).unwrap().len(), len, "log untouched");
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
