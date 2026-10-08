//! Durable hook ingress frames and the drainer's byte cursor.
//!
//! Appends share the blocking ingress lock, cursor writes the workspace lock. Length snapshots and truncation take ingress exclusively; truncation holds both, workspace then ingress. Ingress is pending work, separate from the event log that readers fold; only a fully applied and unclaimed tail may be truncated.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::disk::atomic;
use crate::disk::lock::{IngressAppendLock, WorkspaceLock};
use crate::disk::paths::{RuntimePaths, StatePaths};
use crate::ids::{AgentKind, EventId};

use super::event_log::{self, EventLogErr, LogExtent};

const CURSOR_VERSION: u32 = 1;

pub(crate) struct DrainerStop {
    _spawn: WorkspaceLock,
    _lifetime: WorkspaceLock,
}

pub(crate) fn stop_drainer(runtime: &RuntimePaths) -> Result<DrainerStop, EventLogErr> {
    use std::io::Write as _;
    use std::os::unix::net::UnixStream;

    let spawn = WorkspaceLock::acquire(&runtime.hook_drainer_spawn_lock())
        .map_err(|error| io_error(&runtime.hook_drainer_spawn_lock(), io::Error::other(error)))?;
    if let Ok(mut stream) = UnixStream::connect(runtime.hook_drainer_socket_path()) {
        stream
            .set_write_timeout(Some(std::time::Duration::from_millis(200)))
            .map_err(|error| io_error(&runtime.hook_drainer_socket_path(), error))?;
        let request = serde_json::json!({"through": 0, "reply_for": null, "stop": true});
        let _ = writeln!(stream, "{request}");
    }
    let lifetime = WorkspaceLock::acquire(&runtime.hook_drainer_lock())
        .map_err(|error| io_error(&runtime.hook_drainer_lock(), io::Error::other(error)))?;
    Ok(DrainerStop {
        _spawn: spawn,
        _lifetime: lifetime,
    })
}

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

/// Append a raw hook frame while the caller shares `paths.hook_ingress_lock`, returning the log length after it (possibly including concurrent appends).
#[must_use = "durability barrier; check the result"]
pub fn append(
    paths: &StatePaths,
    frame: &HookIngress,
    _lock: &IngressAppendLock,
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

/// Repair and read pending ingress while holding workspace, then exclusive ingress. Corrupt terminated rows retain their offsets and become skipped records; an unterminated tail is cut before application.
pub fn repair_and_read(
    paths: &StatePaths,
    start: u64,
    _workspace: &WorkspaceLock,
    wait: std::time::Duration,
) -> Result<Vec<(Option<HookIngress>, u64)>, EventLogErr> {
    use crate::diag::hook_drain::{self, HookDrainEvent};

    let _ingress = WorkspaceLock::acquire_with_timeout(&paths.hook_ingress_lock, wait)
        .map_err(|error| io_error(&paths.hook_ingress_lock, io::Error::other(error)))?;
    let mut records = Vec::new();
    for (at, terminated, bytes) in event_log::frame::read_rows(&paths.hook_ingress_log, start)? {
        let end = at + bytes.len() as u64 + u64::from(terminated);
        if !terminated {
            atomic::truncate_file(&paths.hook_ingress_log, at)?;
            hook_drain::append(paths, HookDrainEvent::TruncateTail { start: at, end });
            break;
        }
        match event_log::frame::decode_record(at, terminated, &bytes) {
            Ok(frame) => records.push((Some(frame), end)),
            Err(error) if error.is_corruption() => {
                let fused = fused_frame(at, &bytes);
                hook_drain::append(
                    paths,
                    HookDrainEvent::SkipRecord {
                        start: at,
                        end: fused.as_ref().map_or(end, |(start, _)| *start),
                        error: error.to_string(),
                    },
                );
                records.push((fused.map(|(_, frame)| frame), end));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(records)
}

/// The whole frame a later appender wrote straight after a torn one, which shares its line.
fn fused_frame(at: u64, row: &[u8]) -> Option<(u64, HookIngress)> {
    (1..row.len())
        .filter(|&start| row[start].is_ascii_digit())
        .find_map(|start| {
            let start_at = at + start as u64;
            let frame = event_log::frame::decode_record(start_at, true, &row[start..]).ok()?;
            Some((start_at, frame))
        })
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
        for (at, terminated, bytes) in event_log::frame::read_rows(&paths.hook_ingress_log, 0)? {
            let end = at + bytes.len() as u64 + u64::from(terminated);
            if end > cursor.applied.max(cursor.claimed) {
                break;
            }
            let id = match event_log::frame::decode_record::<HookIngress>(at, terminated, &bytes) {
                Ok(frame) => Some(frame.event_id),
                Err(error) if error.is_corruption() && terminated => {
                    fused_frame(at, &bytes).map(|(_, frame)| frame.event_id)
                }
                Err(error) => return Err(error),
            };
            if end == cursor.applied {
                applied_matches = cursor.applied_event_id == id;
            }
            if end == cursor.claimed {
                claimed_matches = cursor.claimed_event_id == id;
            }
        }
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
