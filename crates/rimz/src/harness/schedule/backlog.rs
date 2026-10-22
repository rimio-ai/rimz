//! Read-only durable waiting triggers, shared with the scheduler tick.

use std::collections::{BTreeMap, VecDeque};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::run_log::SignalRecord;
use crate::config::OverlapPolicy;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Backlog {
    pub version: u32,
    pub tasks: BTreeMap<String, TaskBacklog>,
}

impl Default for Backlog {
    fn default() -> Self {
        Self {
            version: 1,
            tasks: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskBacklog {
    pub triggers: VecDeque<WaitingTrigger>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopping: Option<StopMark>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitingTrigger {
    pub arrived_at: Timestamp,
    pub signal: Option<SignalRecord>,
    pub absorbed: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) from_arrived_at: Option<Timestamp>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopMark {
    pub pid: u32,
    pub started_at: Timestamp,
    pub cause: StopCause,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StopCause {
    Replace,
    Stop,
}

impl TaskBacklog {
    pub(super) fn enqueue(
        &mut self,
        policy: OverlapPolicy,
        limit: usize,
        mut trigger: WaitingTrigger,
    ) -> bool {
        match policy {
            OverlapPolicy::Skip => return false,
            OverlapPolicy::Queue if self.triggers.len() >= limit => return false,
            OverlapPolicy::Queue => {}
            OverlapPolicy::Coalesce | OverlapPolicy::Replace => {
                for older in self.triggers.drain(..).rev() {
                    trigger.absorb(older);
                }
            }
        }
        self.triggers.push_back(trigger);
        self.triggers
            .make_contiguous()
            .sort_by_key(|trigger| trigger.arrived_at);
        true
    }
}

impl WaitingTrigger {
    pub(super) fn absorb(&mut self, mut older: Self) {
        if older.arrived_at > self.arrived_at {
            std::mem::swap(self, &mut older);
        }
        self.absorbed += older.absorbed + 1;
        let older_from = older.from_arrived_at.unwrap_or(older.arrived_at);
        let (Some(new), Some(old)) = (&mut self.signal, older.signal) else {
            return;
        };
        if new.name == old.name
            && older_from <= self.from_arrived_at.unwrap_or(self.arrived_at)
            && new.payload.contains_key("from")
            && new.payload.contains_key("to")
            && old.payload.contains_key("from")
            && old.payload.contains_key("to")
            && new.payload.len() == old.payload.len()
            && new.payload.iter().all(|(key, value)| {
                key == "from" || key == "to" || old.payload.get(key) == Some(value)
            })
            && let Some(from) = old.payload.get("from")
        {
            new.payload.insert("from".into(), from.clone());
            self.from_arrived_at = Some(older_from);
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BacklogErr {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("unsupported loop backlog version {0}")]
    Version(u32),
}

pub(super) fn path(paths: &crate::StatePaths) -> std::path::PathBuf {
    crate::StatePaths::class_path(
        &paths.root,
        crate::disk::paths::Class::Records,
        "loop-backlog.json",
    )
}

pub fn load(paths: &crate::StatePaths) -> Result<Backlog, BacklogErr> {
    let bytes = match std::fs::read(path(paths)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Backlog::default()),
        Err(error) => return Err(error.into()),
    };
    let record: Backlog = serde_json::from_slice(&bytes)?;
    if record.version != 1 {
        return Err(BacklogErr::Version(record.version));
    }
    Ok(record)
}
