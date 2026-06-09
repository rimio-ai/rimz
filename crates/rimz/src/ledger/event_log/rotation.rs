use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use uuid::Uuid;

use crate::ledger::atomic::{self, sync_dir};

use super::{EventLogErr, Result};

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
/// chronologically. Returns `Skipped` if the log is missing, empty, or below
/// the threshold so the caller can decide whether to prune archives without a
/// full lock dance.
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
/// Only files whose names match the archive pattern (`events.<uuid>.jsonl`) are
/// considered; foreign files are ignored so a misconfigured operator can't
/// lose data by dropping unrelated content into the archive directory.
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
