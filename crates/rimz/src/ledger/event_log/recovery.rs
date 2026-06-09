use std::fs;
use std::path::Path;

use crate::ledger::atomic;

use super::{EventLogErr, Result, frame};

/// What [`repair`] found and cut.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RepairOutcome {
    /// Valid frames surviving ahead of the cut (the whole log when intact).
    pub frames_kept: usize,
    /// Bytes removed from the first invalid frame to end of file; `0` when
    /// the log was already intact.
    pub bytes_truncated: u64,
}

impl RepairOutcome {
    /// Whether the repair cut anything. An invalid frame always has bytes, so
    /// a cut is exactly a non-zero truncation.
    pub fn truncated(&self) -> bool {
        self.bytes_truncated > 0
    }
}

/// Truncate the log at its first invalid frame — the post-power-cut corpse a
/// mid-file framing/CRC error indicates — keeping the valid prefix.
#[must_use = "durability barrier; check the result"]
pub fn repair(path: &Path) -> Result<RepairOutcome> {
    let rows = frame::read_rows(path, 0)?;
    let mut frames_kept = 0usize;
    let mut valid_end = 0u64;
    let mut invalid_at: Option<u64> = None;
    for (at, terminated, bytes) in rows {
        match frame::decode_row(at, terminated, &bytes) {
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
    })
}
