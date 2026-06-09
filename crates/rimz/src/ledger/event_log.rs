//! Length-framed append-only event log.
//!
//! `events.log.jsonl` is the canonical history of everything that happened in
//! the workspace. This module owns framing, recovery, rotation, and archive
//! retention; snapshot reconciliation across rotations lives in
//! [`crate::ledger::snapshot`].

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::ledger::atomic;
use crate::schema::event::EventEnvelope;

mod frame;
mod recovery;
mod rotation;

pub use recovery::{RepairOutcome, repair};
pub use rotation::{PruneOutcome, RotationOutcome, prune_archive, rotate};

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
    Ok(atomic::append_record_bytes(
        path,
        &frame::encode_frame(&payload),
    )?)
}

#[must_use = "durability barrier; check the result"]
pub fn replace_all(path: &Path, events: &[EventEnvelope]) -> Result<()> {
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
pub fn read_from_offset(path: &Path, start: u64) -> Result<(Vec<EventEnvelope>, u64)> {
    if !path.exists() {
        // No log, no extent — a fresh workspace folds nothing.
        return Ok((Vec::new(), 0));
    }
    let rows = frame::read_rows(path, start)?;
    let mut events = Vec::new();
    let mut end = start;
    let last_index = rows.len().saturating_sub(1);
    for (idx, (at, terminated, bytes)) in rows.into_iter().enumerate() {
        let frame_len = bytes.len() as u64 + u64::from(terminated);
        match frame::decode_row(at, terminated, &bytes) {
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

/// Test-only observability seam: bytes the row scan actually read, so the
/// performance tier can prove a warm fold is O(new bytes) rather than O(log)
/// from the integration binary. Per-process and relaxed, like
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

#[cfg(test)]
mod tests;
