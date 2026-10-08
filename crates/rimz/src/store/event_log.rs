//! Length-framed append-only event log.
//!
//! `events.log.jsonl` is the canonical history of everything that happened in
//! the workspace. This module owns framing, recovery, rotation, and archive
//! retention; snapshot reconciliation across rotations lives in
//! [`crate::store::snapshot`].

use std::io;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::disk::atomic;
use crate::store::event::EventEnvelope;

mod frame;
mod recovery;
mod rotation;

pub use recovery::RepairOutcome;
pub(super) use recovery::repair;
pub use rotation::{PruneOutcome, RotationOutcome};
pub(super) use rotation::{newest_archives, prune_archive, rotate};

pub use crate::disk::retention::{DEFAULT_RETENTION, DEFAULT_RETENTION_ARG};

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
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl EventLogErr {
    /// A frame-level corruption a repair truncation heals — distinct from
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
    let frame = frame::encode_frame(&payload);
    atomic::append_record_bytes(path, &frame)?;
    testkit::count_bytes_written(frame.len() as u64);
    Ok(())
}

/// Append one ordered group with a single filesystem write.
#[must_use = "durability barrier; check the result"]
pub(super) fn append_batch(path: &Path, events: &[EventEnvelope]) -> Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let mut bytes = Vec::new();
    for event in events {
        let payload = serde_json::to_vec(event).map_err(atomic::AtomicErr::Json)?;
        bytes.extend_from_slice(&frame::encode_frame(&payload));
    }
    atomic::append_record_bytes(path, &bytes)?;
    testkit::count_bytes_written(bytes.len() as u64);
    Ok(())
}

#[must_use = "durability barrier; check the result"]
pub(super) fn replace_all(path: &Path, events: &[EventEnvelope]) -> Result<()> {
    let mut bytes = Vec::new();
    for event in events {
        let payload = serde_json::to_vec(event).map_err(atomic::AtomicErr::Json)?;
        bytes.extend_from_slice(&frame::encode_frame(&payload));
    }
    atomic::write_bytes_atomically(path, &bytes)?;
    Ok(())
}

/// Read every parseable record. A torn trailing record (length mismatch or
/// JSON parse failure) is logged and skipped; we never propagate it as a hard
/// error because that's what a power cut mid-append leaves behind.
pub fn read_all(path: &Path) -> Result<Vec<EventEnvelope>> {
    Ok(read_from_offset(path, 0)?.0)
}

/// Read every parseable record starting at byte `start` — the incremental
/// twin of [`read_all`] for a reader resuming from a persisted fold base.
///
/// Returns the events and the offset after the last complete frame: the
/// extent a derived rollup may claim to reflect. An unterminated or
/// undecodable tail frame is not yet committed (an in-flight append, or a
/// power-cut corpse), so reading stops in front of it and the returned offset
/// never claims bytes the fold skipped. A torn record followed by more frames
/// is corruption and stays a hard error.
pub(crate) fn read_from_offset(path: &Path, start: u64) -> Result<(Vec<EventEnvelope>, u64)> {
    let mut events = Vec::new();
    let end = visit_from_offset(path, start, |event| events.push(event))?;
    Ok((events, end))
}

/// Visit complete frames without retaining the log tail in memory.
/// Returns the offset after the last committed frame, with the same torn-tail
/// and middle-corruption rules as the collecting reader.
pub fn visit_from_offset(
    path: &Path,
    start: u64,
    mut visit: impl FnMut(EventEnvelope),
) -> Result<u64> {
    visit_records_from_offset(path, start, |event, _| visit(event))
}

pub(super) fn visit_records_from_offset<T: DeserializeOwned>(
    path: &Path,
    start: u64,
    visit: impl FnMut(T, u64),
) -> Result<u64> {
    visit_records_through_offset(path, start, u64::MAX, visit)
}

pub(super) fn visit_records_through_offset<T: DeserializeOwned>(
    path: &Path,
    start: u64,
    through: u64,
    mut visit: impl FnMut(T, u64),
) -> Result<u64> {
    use std::io::{BufRead, BufReader, Read as _, Seek, SeekFrom};

    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(start),
        Err(source) => {
            return Err(EventLogErr::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let io_error = |source| EventLogErr::Io {
        path: path.to_path_buf(),
        source,
    };
    file.seek(SeekFrom::Start(start)).map_err(io_error)?;
    let mut reader = BufReader::new(file.take(through.saturating_sub(start)));
    let mut bytes = Vec::new();
    let mut end = start;
    loop {
        bytes.clear();
        let read = reader.read_until(b'\n', &mut bytes).map_err(io_error)?;
        if read == 0 {
            return Ok(end);
        }
        testkit::count_bytes_read(read as u64);
        let terminated = bytes.last() == Some(&b'\n');
        if terminated {
            bytes.pop();
        }
        match frame::decode_record(end, terminated, &bytes) {
            Ok(event) => {
                end += read as u64;
                visit(event, end);
            }
            Err(err) if err.is_corruption() && reader.fill_buf().map_err(io_error)?.is_empty() => {
                if terminated {
                    warn!(offset = end, error = %err, "skipping torn trailing event-log record");
                } else {
                    debug!(offset = end, "stopping before an in-flight tail frame");
                }
                return Ok(end);
            }
            Err(err) => return Err(err),
        }
    }
}

/// Always-on observability seam: bytes the row scan actually read, so the
/// performance tier and sidebar tick meter can prove a warm fold is O(new
/// bytes) rather than O(log) from the integration binary. Per-process and
/// relaxed, like [`crate::disk::atomic::testkit`] and
/// [`crate::sidebar::meter`].
#[doc(hidden)]
pub mod testkit {
    use std::sync::atomic::{AtomicU64, Ordering};

    static BYTES_READ: AtomicU64 = AtomicU64::new(0);
    static BYTES_WRITTEN: AtomicU64 = AtomicU64::new(0);

    /// Event-log bytes scanned since process start.
    pub fn bytes_read() -> u64 {
        BYTES_READ.load(Ordering::Relaxed)
    }

    /// Event-log bytes successfully appended since process start.
    pub fn bytes_written() -> u64 {
        BYTES_WRITTEN.load(Ordering::Relaxed)
    }

    pub(super) fn count_bytes_read(n: u64) {
        BYTES_READ.fetch_add(n, Ordering::Relaxed);
        crate::lane::count_event_log_bytes_read(n);
    }

    pub(super) fn count_bytes_written(n: u64) {
        BYTES_WRITTEN.fetch_add(n, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests;
