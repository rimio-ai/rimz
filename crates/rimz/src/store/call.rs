//! The call record: one per launch asked for a `schema`, a `timeout`, or a rest, keyed by the
//! launch id of the launch's leader at `owned/calls/<launch-id>.json`. It is written before the
//! launch's provider starts and holds the answer schema, the deadline, the rest, every seat the
//! launch opened, and once handed in, the answer. Each change is a locked read-modify-write
//! published by atomic rename.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::disk::atomic::write_temp_then_rename;
use crate::disk::lock::WorkspaceLock;
use crate::disk::paths::StatePaths;
use crate::ids::{AgentKind, AgentSessionId, MessageId};

#[derive(Debug, thiserror::Error)]
pub enum CallStoreErr {
    #[error(transparent)]
    Atomic(#[from] crate::disk::atomic::AtomicErr),
    #[error(transparent)]
    Lock(#[from] crate::disk::lock::LockErr),
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("json parse error on {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

pub type Result<T> = std::result::Result<T, CallStoreErr>;

/// One seat a launch opened, by its own launch id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seat {
    pub kind: AgentKind,
    pub launch_id: AgentSessionId,
}

/// An answer handed in for the call's schema, and when.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HandedAnswer {
    pub value: Value,
    pub at: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CallRecord {
    /// The launch id of the launch's leader: the agent's own, or the team leader's.
    pub launch_id: AgentSessionId,
    /// Every seat the launch opened, the leader first.
    pub seats: Vec<Seat>,
    /// Whether the `agent` call's `stay` keeps the agent past its turn end; a team's call and
    /// a rest stay.
    pub stay: bool,
    /// A rest stay's rest: once the launch's first turn ends, the agent closes after resting
    /// this long.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rest_secs: Option<u64>,
    /// When the launch's first turn end booked the rest's close; at most once, so a later turn
    /// end books nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rest_booked_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<Map<String, Value>>,
    pub created_at: Timestamp,
    /// When the launch is stopped: `created_at` plus the call's `timeout`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<HandedAnswer>,
    /// The one notice that tells an agent whose turn ended with no answer how to hand it in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked: Option<MessageId>,
    /// When the deadline's stop claimed the launch; at most once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timed_out_at: Option<Timestamp>,
}

impl CallRecord {
    /// Whether the launch is a `stay: false` agent that still owes its answer, so its turn end
    /// leaves it open: it has a schema and no answer, and no deadline has stopped it.
    pub fn owes_answer(&self) -> bool {
        !self.stay && self.schema.is_some() && self.answer.is_none() && self.timed_out_at.is_none()
    }
}

/// Whether launch `launch_id` owes its answer ([`CallRecord::owes_answer`]); a launch with no
/// record owes none.
pub fn owes_answer(paths: &StatePaths, launch_id: &AgentSessionId) -> Result<bool> {
    Ok(load(paths, launch_id)?.is_some_and(|record| record.owes_answer()))
}

pub(crate) fn call_path(paths: &StatePaths, launch_id: &AgentSessionId) -> PathBuf {
    paths.calls_dir.join(format!("{launch_id}.json"))
}

/// Publish a new launch's record, the first one creating `owned/calls/` with its entry synced.
pub fn create(paths: &StatePaths, record: &CallRecord) -> Result<()> {
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    crate::disk::atomic::create_dir_all_mode(&paths.calls_dir, 0o700)?;
    write_temp_then_rename(&call_path(paths, &record.launch_id), record)?;
    Ok(())
}

/// The record of the launch `launch_id`, if it has one.
pub fn load(paths: &StatePaths, launch_id: &AgentSessionId) -> Result<Option<CallRecord>> {
    read(&call_path(paths, launch_id))
}

/// Every call record in the room; one that cannot be read or parsed warns and is skipped, so
/// it never holds up the passes over the rest.
pub fn list(paths: &StatePaths) -> Result<Vec<CallRecord>> {
    let dir = paths.calls_dir.clone();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(CallStoreErr::Io { path: dir, source }),
    };
    let mut records = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| CallStoreErr::Io {
            path: dir.clone(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        match read(&path) {
            Ok(record) => records.extend(record),
            Err(error) => tracing::warn!(%error, "skipping an unreadable call record"),
        }
    }
    Ok(records)
}

/// Change the record of `launch_id` under the workspace lock: `change` returns whether it
/// changed it, and only then is it written. None when the launch has no record.
pub fn update<T>(
    paths: &StatePaths,
    launch_id: &AgentSessionId,
    change: impl FnOnce(&mut CallRecord) -> Option<T>,
) -> Result<Option<(CallRecord, T)>> {
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    let path = call_path(paths, launch_id);
    let Some(mut record) = read(&path)? else {
        return Ok(None);
    };
    let Some(changed) = change(&mut record) else {
        return Ok(None);
    };
    write_temp_then_rename(&path, &record)?;
    Ok(Some((record, changed)))
}

fn read(path: &Path) -> Result<Option<CallRecord>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(CallStoreErr::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|source| CallStoreErr::Json {
            path: path.to_path_buf(),
            source,
        })
}
