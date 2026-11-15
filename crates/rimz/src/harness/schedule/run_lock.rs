//! The run lock a loop task's runner holds, spelled once for the runner that
//! claims it and the clock pass that probes it. Read-only: it belongs to the
//! sidebar import graph and never claims or writes a lock.
//!
//! A runner claims its lock under its task id, so a renamed task keeps it; a lock under the
//! task's name, which an older RimZ claimed, still reads as the task's.

use std::collections::BTreeSet;

use super::task_record::TaskRecords;
use crate::RuntimePaths;
use crate::disk::lease::{self, LeaseState};
use crate::ids::{TaskId, WorkspaceId};

/// The one spelling of a run lock's file name, for a task id or (an older RimZ's) task name.
/// `checkout` names the launch checkout of a fan-out fire, which locks per checkout.
pub(super) fn file_name(name: &str, checkout: Option<&WorkspaceId>) -> String {
    match checkout {
        Some(checkout) => format!("loop-run-{name}-{checkout}.lock"),
        None => format!("loop-run-{name}.lock"),
    }
}

/// Whether a process holds the bare run lock of task `name` under any of its spellings,
/// through the same gated lease probe the runner's own probes use.
pub(super) fn held(runtime: &RuntimePaths, name: &str, ids: &[TaskId]) -> std::io::Result<bool> {
    for subject in ids.iter().map(TaskId::as_str).chain([name]) {
        let path = runtime.lock_path(file_name(subject, None));
        if matches!(lease::probe(&path)?, LeaseState::Held(_)) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether `file` is a run lock of task `name` under its name or one of `ids`.
pub(super) fn claims(file: &str, name: &str, ids: &[TaskId]) -> bool {
    is_run_lock_of(file, name) || ids.iter().any(|id| is_run_lock_of(file, id.as_str()))
}

/// Whether `file` is task `name`'s run lock, bare or per-checkout.
pub(super) fn is_run_lock_of(file: &str, name: &str) -> bool {
    let checkout = file
        .strip_suffix(".lock")
        .and_then(|stem| stem.rsplit_once('-'))
        .and_then(|(_, id)| WorkspaceId::parse(id).ok());
    file == file_name(name, None)
        || checkout.is_some_and(|checkout| file == file_name(name, Some(&checkout)))
}

/// Which of `names` a process holds a run lock of, bare or per-checkout, under the task's name
/// or the id of its live record: one listing of `locks/` and one probe of each lock a task of
/// `names` claims. A missing `locks/` holds none, and a lock whose probe fails reads as free.
pub(super) fn held_tasks<'a>(
    runtime: &RuntimePaths,
    records: &TaskRecords,
    names: &[&'a str],
) -> std::io::Result<BTreeSet<&'a str>> {
    let ids = names
        .iter()
        .map(|name| records.ids_named(name))
        .collect::<Vec<_>>();
    let entries = match std::fs::read_dir(&runtime.locks_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(error),
    };
    let mut held = BTreeSet::new();
    for entry in entries {
        let entry = entry?;
        let Some(file) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let owners = names
            .iter()
            .zip(&ids)
            .filter(|(name, ids)| claims(&file, name, ids))
            .map(|(name, _)| *name)
            .collect::<Vec<_>>();
        if !owners.is_empty() && matches!(lease::probe(&entry.path()), Ok(LeaseState::Held(_))) {
            held.extend(owners);
        }
    }
    Ok(held)
}
