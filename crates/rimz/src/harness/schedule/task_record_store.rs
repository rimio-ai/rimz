//! Locked durable writes of the loop task record, `~/.rimz/loops/tasks.json`.
//!
//! Every write is one read-modify-write under `tasks.lock`, a leaf: nothing else is acquired
//! while it is held. A record is bound on its task's first state write, so a row written by
//! hand reads as default state until then.

use jiff::Timestamp;

use super::task_record::{self, Binding, TaskRecord, TaskRecordErr, TaskRecords};
use crate::ids::TaskId;

const LOCK: &str = "tasks.lock";

#[derive(Debug, thiserror::Error)]
pub enum TaskStoreErr {
    #[error(transparent)]
    Read(#[from] TaskRecordErr),
    #[error(transparent)]
    Lock(#[from] crate::disk::lock::LockErr),
    #[error(transparent)]
    Write(#[from] crate::disk::atomic::AtomicErr),
}

/// Apply `edit` to the record under its lock, writing it only when the edit changed it.
pub(super) fn mutate<T>(edit: impl FnOnce(&mut TaskRecords) -> T) -> Result<T, TaskStoreErr> {
    let path = task_record::path();
    let _guard =
        crate::disk::lock::WorkspaceLock::acquire(&crate::disk::paths::loops_dir().join(LOCK))?;
    let mut records = task_record::load_from(&path)?;
    let before = records.clone();
    let result = edit(&mut records);
    if records != before {
        crate::disk::atomic::write_temp_then_rename(&path, &records)?;
    }
    Ok(result)
}

impl TaskRecords {
    /// The live record bound to `binding`, minted when there is none.
    pub(super) fn bind(&mut self, binding: &Binding) -> (TaskId, &mut TaskRecord) {
        let id = self
            .live(&binding.key())
            .map_or_else(TaskId::new, |(id, _)| id.clone());
        let record = self
            .tasks
            .entry(id.clone())
            .or_insert_with(|| TaskRecord::new(binding.clone()));
        (id, record)
    }

    /// The live record bound to `key`, for an edit that binds nothing.
    pub(super) fn live_mut(&mut self, key: &str) -> Option<&mut TaskRecord> {
        self.tasks
            .values_mut()
            .find(|record| record.is_live() && record.binding.key() == key)
    }

    /// A new record for a task added over `binding`, retiring the one it replaces.
    pub(super) fn mint(&mut self, binding: &Binding, now: Timestamp) -> TaskId {
        self.retire(&binding.key(), now);
        let id = TaskId::new();
        self.tasks
            .insert(id.clone(), TaskRecord::new(binding.clone()));
        id
    }

    /// Retire the live record bound to `key`; the record stays for what its fires left.
    pub(super) fn retire(&mut self, key: &str, now: Timestamp) -> bool {
        let Some(record) = self.live_mut(key) else {
            return false;
        };
        record.retired_at = Some(now);
        true
    }

    /// Delete the live record bound to `key`.
    pub(super) fn delete(&mut self, key: &str) -> bool {
        let before = self.tasks.len();
        self.tasks
            .retain(|_, record| !(record.is_live() && record.binding.key() == key));
        self.tasks.len() != before
    }

    /// Move the live record bound to `old` onto `new`, retiring a live record already there.
    pub(super) fn rebind(&mut self, old: &str, new: Binding, now: Timestamp) -> bool {
        if self.live_mut(old).is_none() {
            return false;
        }
        if new.key() != old {
            self.retire(&new.key(), now);
        }
        if let Some(record) = self.live_mut(old) {
            record.binding = new;
        }
        true
    }

    pub(super) fn get_mut(&mut self, id: &TaskId) -> Option<&mut TaskRecord> {
        self.tasks.get_mut(id)
    }
}

#[cfg(test)]
#[path = "task_record_store/tests.rs"]
mod tests;
