//! Durable hook ingress frames and the drainer's byte cursor.
//!
//! Appends hold the ingress lock, cursor writes the workspace lock. Truncation holds both, workspace then ingress. Ingress is pending work, separate from the event log that readers fold; only a fully applied and unclaimed tail may be truncated.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::disk::atomic;
use crate::disk::lock::WorkspaceLock;
use crate::disk::paths::StatePaths;
use crate::ids::{AgentKind, EventId};

use super::event_log::{self, EventLogErr, LogExtent};

const CURSOR_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookIngress {
    pub schema_version: String,
    pub event_id: EventId,
    pub ts: Timestamp,
    pub source: AgentKind,
    pub event: Option<String>,
    pub payload: String,
    pub cwd: PathBuf,
    pub hook_pid: u32,
    pub env: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookDrainCursor {
    pub version: u32,
    pub applied: u64,
    pub claimed: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_event_id: Option<EventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_event_id: Option<EventId>,
    pub log_extent_at_claim: LogExtent,
}

impl Default for HookDrainCursor {
    fn default() -> Self {
        Self {
            version: CURSOR_VERSION,
            applied: 0,
            claimed: 0,
            applied_event_id: None,
            claimed_event_id: None,
            log_extent_at_claim: LogExtent {
                generation: 0,
                offset: 0,
            },
        }
    }
}

/// Append a raw hook frame while the caller holds `paths.hook_ingress_lock`, returning the byte offset after it.
#[must_use = "durability barrier; check the result"]
pub fn append(
    paths: &StatePaths,
    frame: &HookIngress,
    _lock: &WorkspaceLock,
) -> Result<u64, EventLogErr> {
    event_log::append(&paths.hook_ingress_log, frame)?;
    log_len(paths)
}

/// Read frames through an ingress-locked length snapshot, returning the last complete offset; torn-tail and middle-corruption rules match the event log. The drainer holds the workspace lock to exclude truncation while reading, but releases the ingress lock before parsing.
pub fn read_from_offset(
    paths: &StatePaths,
    start: u64,
) -> Result<(Vec<(HookIngress, u64)>, u64), EventLogErr> {
    let through = {
        let _lock = WorkspaceLock::acquire(&paths.hook_ingress_lock)
            .map_err(|error| io_error(&paths.hook_ingress_lock, io::Error::other(error)))?;
        log_len(paths)?
    };
    let mut frames = Vec::new();
    let end = event_log::visit_records_through_offset(
        &paths.hook_ingress_log,
        start,
        through,
        |frame, end| {
            frames.push((frame, end));
        },
    )?;
    Ok((frames, end))
}

/// A missing cursor or one whose offsets no longer end at the recorded frame IDs starts at zero; malformed or unsupported cursors remain errors.
pub fn read_cursor(paths: &StatePaths) -> Result<HookDrainCursor, EventLogErr> {
    let path = &paths.hook_drain_cursor;
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(HookDrainCursor::default()),
        Err(source) => return Err(io_error(path, source)),
    };
    let cursor: HookDrainCursor =
        serde_json::from_slice(&bytes).map_err(atomic::AtomicErr::Json)?;
    if cursor.version != CURSOR_VERSION {
        return Err(io_error(
            path,
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported hook drain cursor version {}", cursor.version),
            ),
        ));
    }
    let len = log_len(paths)?;
    if cursor.applied > len || cursor.claimed > len {
        return Ok(HookDrainCursor::default());
    }
    let mut applied_matches = cursor.applied == 0 && cursor.applied_event_id.is_none();
    let mut claimed_matches = cursor.claimed == 0 && cursor.claimed_event_id.is_none();
    if !applied_matches || !claimed_matches {
        event_log::visit_records_through_offset::<HookIngress>(
            &paths.hook_ingress_log,
            0,
            cursor.applied.max(cursor.claimed),
            |frame, end| {
                if end == cursor.applied {
                    applied_matches = cursor.applied_event_id.as_ref() == Some(&frame.event_id);
                }
                if end == cursor.claimed {
                    claimed_matches = cursor.claimed_event_id.as_ref() == Some(&frame.event_id);
                }
            },
        )?;
    }
    if !applied_matches || !claimed_matches {
        return Ok(HookDrainCursor::default());
    }
    Ok(cursor)
}

/// Publish the cursor durably while the caller holds `paths.workspace_lock`.
#[must_use = "durability barrier; check the result"]
pub fn write_cursor(
    paths: &StatePaths,
    cursor: &HookDrainCursor,
    _lock: &WorkspaceLock,
) -> Result<(), EventLogErr> {
    let path = &paths.hook_drain_cursor;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
    }
    atomic::write_temp_then_rename(path, cursor)?;
    Ok(())
}

/// Truncate in place and reset the cursor while the caller holds the workspace lock, acquiring the ingress lock second. Only `applied == claimed == len` permits truncation; empty or unfinished logs return `false`.
#[must_use = "durability barrier; check the result"]
pub fn truncate_drained(paths: &StatePaths, lock: &WorkspaceLock) -> Result<bool, EventLogErr> {
    let _ingress = WorkspaceLock::acquire(&paths.hook_ingress_lock)
        .map_err(|error| io_error(&paths.hook_ingress_lock, io::Error::other(error)))?;
    let len = log_len(paths)?;
    if len == 0 {
        return Ok(false);
    }
    let cursor = read_cursor(paths)?;
    if cursor.applied != len || cursor.claimed != len {
        return Ok(false);
    }
    atomic::truncate_file(&paths.hook_ingress_log, 0)?;
    write_cursor(paths, &HookDrainCursor::default(), lock)?;
    Ok(true)
}

fn log_len(paths: &StatePaths) -> Result<u64, EventLogErr> {
    match fs::metadata(&paths.hook_ingress_log) {
        Ok(metadata) => Ok(metadata.len()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(source) => Err(io_error(&paths.hook_ingress_log, source)),
    }
}

fn io_error(path: &Path, source: io::Error) -> EventLogErr {
    EventLogErr::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests;
