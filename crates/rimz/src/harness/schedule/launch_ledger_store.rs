//! Locked durable mutations of launch memory and capacity waits, and the lock a capped task's
//! budget admission holds.
//!
//! The catalog's per-task removals and renames (`remove_workflows`, `rename_workflows`,
//! `remove_capacity`, `rename_capacity`) read their file first and return
//! before taking the lock when it holds nothing for the task: taking the lock creates room state
//! in a project with no room (`loop_schedule::loop_rename_without_a_room_does_not_create_state`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::launch_ledger;

/// Held by a capped task's fire from its daily-budget read until `create` publishes its run, so
/// the next fire's read counts that run: each checkout of an each-worktree task fires under its
/// own lease.
pub(super) fn hold_budget(
    paths: &crate::StatePaths,
    task: &str,
) -> Result<crate::disk::lock::WorkspaceLock, crate::disk::lock::LockErr> {
    crate::disk::lock::WorkspaceLock::acquire(&paths.lock_path(&format!("loop-budget.{task}.lock")))
}

pub(super) fn record_workflow(
    paths: &crate::StatePaths,
    task: &str,
    checkout: &Path,
    record: launch_ledger::WorkflowLaunchRecord,
    replace_project: bool,
) -> Result<(), LedgerWriteErr> {
    mutate(
        paths,
        launch_ledger::workflow_path,
        "loop-workflow-launches.lock",
        launch_ledger::load_workflows,
        |ledger| &mut ledger.tasks,
        |tasks| {
            let launches = tasks.entry(task.to_owned()).or_default();
            if launches
                .get(checkout)
                .is_some_and(|previous| previous.run == record.run)
            {
                return;
            }
            if replace_project && record.scope == crate::store::workflow::ScopeKind::Worktree {
                launches.retain(|_, previous| {
                    previous.scope != crate::store::workflow::ScopeKind::Project
                });
            }
            launches.insert(checkout.to_owned(), record);
        },
    )
}

pub(super) fn remove_workflows(
    paths: &crate::StatePaths,
    task: &str,
) -> Result<(), LedgerWriteErr> {
    if !launch_ledger::load_workflows(paths)?
        .tasks
        .contains_key(task)
    {
        return Ok(());
    }
    mutate(
        paths,
        launch_ledger::workflow_path,
        "loop-workflow-launches.lock",
        launch_ledger::load_workflows,
        |ledger| &mut ledger.tasks,
        |tasks| {
            tasks.remove(task);
        },
    )
}

pub(super) fn rename_workflows(
    paths: &crate::StatePaths,
    task: &str,
    name: &str,
) -> Result<(), LedgerWriteErr> {
    if !launch_ledger::load_workflows(paths)?
        .tasks
        .contains_key(task)
    {
        return Ok(());
    }
    mutate(
        paths,
        launch_ledger::workflow_path,
        "loop-workflow-launches.lock",
        launch_ledger::load_workflows,
        |ledger| &mut ledger.tasks,
        |tasks| {
            if let Some(checkouts) = tasks.remove(task) {
                tasks.insert(name.to_owned(), checkouts);
            }
        },
    )
}

/// Drop the task's entries that remember one of `runs`, so the checkouts those runs held
/// launch again; every other entry of the task stays.
pub(super) fn forget_runs(
    paths: &crate::StatePaths,
    task: &str,
    runs: &[crate::ids::WorkflowRunId],
) -> Result<(), LedgerWriteErr> {
    let remembers = |record: &launch_ledger::WorkflowLaunchRecord| {
        record.run.as_ref().is_some_and(|run| runs.contains(run))
    };
    if !launch_ledger::load_workflows(paths)?
        .tasks
        .get(task)
        .is_some_and(|launches| launches.values().any(remembers))
    {
        return Ok(());
    }
    mutate(
        paths,
        launch_ledger::workflow_path,
        "loop-workflow-launches.lock",
        launch_ledger::load_workflows,
        |ledger| &mut ledger.tasks,
        |tasks| {
            if let Some(launches) = tasks.get_mut(task) {
                launches.retain(|_, record| !remembers(record));
            }
            tasks.retain(|_, launches| !launches.is_empty());
        },
    )
}

#[derive(Debug, thiserror::Error)]
pub(super) enum LedgerWriteErr {
    #[error(transparent)]
    Read(#[from] launch_ledger::LedgerErr),
    #[error(transparent)]
    Lock(#[from] crate::disk::lock::LockErr),
    #[error(transparent)]
    Write(#[from] crate::disk::atomic::AtomicErr),
}

fn mutate<T: Clone + PartialEq + serde::Serialize, R>(
    paths: &crate::StatePaths,
    path: fn(&crate::StatePaths) -> PathBuf,
    lock: &str,
    load: fn(&crate::StatePaths) -> Result<T, launch_ledger::LedgerErr>,
    records: fn(&mut T) -> &mut BTreeMap<String, BTreeMap<std::path::PathBuf, R>>,
    edit: impl FnOnce(&mut BTreeMap<String, BTreeMap<std::path::PathBuf, R>>),
) -> Result<(), LedgerWriteErr> {
    let _guard = crate::disk::lock::WorkspaceLock::acquire(&paths.lock_path(lock))?;
    let mut ledger = load(paths)?;
    let before = ledger.clone();
    let checkouts = records(&mut ledger);
    checkouts.retain(|_, launches| {
        launches.retain(|checkout, _| checkout.is_dir());
        !launches.is_empty()
    });
    edit(checkouts);
    if ledger != before {
        crate::disk::atomic::write_temp_then_rename(&path(paths), &ledger)?;
    }
    Ok(())
}

pub(super) fn remove_capacity(paths: &crate::StatePaths, task: &str) -> Result<(), LedgerWriteErr> {
    if !launch_ledger::load_capacity(paths)?.contains_key(task) {
        return Ok(());
    }
    mutate(
        paths,
        launch_ledger::capacity_path,
        "loop-capacity-waits.lock",
        launch_ledger::load_capacity,
        |waits| waits,
        |waits| {
            waits.remove(task);
        },
    )
}

pub(super) fn rename_capacity(
    paths: &crate::StatePaths,
    task: &str,
    name: &str,
) -> Result<(), LedgerWriteErr> {
    if !launch_ledger::load_capacity(paths)?.contains_key(task) {
        return Ok(());
    }
    mutate(
        paths,
        launch_ledger::capacity_path,
        "loop-capacity-waits.lock",
        launch_ledger::load_capacity,
        |waits| waits,
        |waits| {
            if let Some(checkouts) = waits.remove(task) {
                waits.insert(name.to_owned(), checkouts);
            }
        },
    )
}

#[cfg(test)]
#[path = "launch_ledger_store/tests.rs"]
mod tests;
