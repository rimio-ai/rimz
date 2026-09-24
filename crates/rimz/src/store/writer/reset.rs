use std::fs;
use std::io;
use std::path::Path;

use jiff::Timestamp;

use crate::store::run::{self as run, RunRecord, RunStatus};

use super::super::{Result, Store, StoreErr, event_log, snapshot};
use super::{ResetRecordsOutcome, RollupInvalidation, remove_file_if_exists};

fn remove_dir_if_exists(path: &Path) -> Result<bool> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(true),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(StoreErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn remove_runtime_dir_with(
    path: &Path,
    remove: impl FnOnce(&Path) -> Result<bool>,
) -> Result<bool> {
    // Late runtime writers use the canonical path even after mux teardown.
    // Detach the old tree first so those writes cannot repopulate the tree
    // being recursively removed. Runtime hints need no durability barrier.
    let detached = path.with_extension(format!("reset-{}", uuid::Uuid::now_v7().simple()));
    match fs::rename(path, &detached) {
        Ok(()) => {
            remove(&detached)?;
            Ok(true)
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(StoreErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn count_dir_entries_recursive(path: &Path) -> Result<usize> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(source) => {
            return Err(StoreErr::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let mut count = 0;
    for entry in entries {
        let entry = entry.map_err(|source| StoreErr::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let child = entry.path();
        count += 1;
        let meta = fs::symlink_metadata(&child).map_err(|source| StoreErr::Io {
            path: child.clone(),
            source,
        })?;
        if meta.is_dir() {
            count += count_dir_entries_recursive(&child)?;
        }
    }
    Ok(count)
}

fn remove_dir_counting_entries(path: &Path) -> Result<usize> {
    let count = count_dir_entries_recursive(path)?;
    remove_dir_if_exists(path)?;
    Ok(count)
}

fn remove_diag_logs(root: &Path) -> Result<usize> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(source) => {
            return Err(StoreErr::Io {
                path: root.to_path_buf(),
                source,
            });
        }
    };
    let mut removed = 0;
    for entry in entries {
        let entry = entry.map_err(|source| StoreErr::Io {
            path: root.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with("diag.log") {
            continue;
        }
        let meta = fs::symlink_metadata(&path).map_err(|source| StoreErr::Io {
            path: path.clone(),
            source,
        })?;
        if meta.is_dir() {
            fs::remove_dir_all(&path).map_err(|source| StoreErr::Io {
                path: path.clone(),
                source,
            })?;
        } else {
            fs::remove_file(&path).map_err(|source| StoreErr::Io {
                path: path.clone(),
                source,
            })?;
        }
        removed += 1;
    }
    Ok(removed)
}

fn cancel_active_runs_for_reset_locked(paths: &super::super::StatePaths) -> Result<Vec<RunRecord>> {
    let mut canceled = Vec::new();
    for mut record in run::list(&paths.runs_dir)? {
        if !record.mark_terminal(RunStatus::Canceled, Timestamp::now()) {
            continue;
        }
        run::write(&paths.runs_dir, &record)?;
        canceled.push(record);
    }
    Ok(canceled)
}

impl Store {
    /// Archive the room's active records and clear coordination/debug state for
    /// a user-requested room reset. The mux teardown has already killed panes;
    /// this method terminal-wakes any surviving waiters and makes the store
    /// match that product boundary.
    #[must_use = "durability barrier; check the result"]
    pub fn reset_records(&self, hard: bool) -> Result<ResetRecordsOutcome> {
        self.reset_records_with(hard, true, event_log::rotate)
    }

    /// Soft-reset records for a recovery that rebuilds the same room, keeping
    /// its frozen provider accounts in the same transaction.
    #[must_use = "durability barrier; check the result"]
    pub fn reset_records_keeping_logins(&self) -> Result<ResetRecordsOutcome> {
        self.reset_records_with(false, false, event_log::rotate)
    }

    fn reset_records_with<F>(
        &self,
        hard: bool,
        unfreeze_logins: bool,
        rotate: F,
    ) -> Result<ResetRecordsOutcome>
    where
        F: FnOnce(&Path, &Path, u64) -> event_log::Result<event_log::RotationOutcome>,
    {
        let (mut outcome, canceled_runs) = self.commit_boundary(|paths| {
            let canceled_runs = cancel_active_runs_for_reset_locked(paths)?;
            let runs_canceled = canceled_runs.len();

            let carryover_agents = if hard {
                remove_file_if_exists(&paths.agents_carryover)?;
                0
            } else {
                snapshot::stage_carryover_for_rotation(paths, 0)?
            };

            paths.ensure_dirs()?;

            // A reset unfreezes the room's provider accounts, so the next
            // birth is free to select again.
            if unfreeze_logins
                && let Some(mut record) =
                    crate::workspace::record::read_optional(&paths.workspace_record)?
                && record.logins.take().is_some()
            {
                crate::workspace::record::write(paths, &record)?;
            }

            let mut state_entries_removed = 0;
            state_entries_removed += remove_diag_logs(&paths.root)?;
            state_entries_removed +=
                remove_dir_counting_entries(&crate::diag::frames_dir_under(&paths.root))?;

            let rotation = rotate(&paths.events_log, &paths.events_archive_dir, 0)?;
            if hard {
                remove_file_if_exists(&paths.events_log)?;
            }
            let rollup = if hard {
                RollupInvalidation::Forget
            } else if rotation.is_rotated() {
                RollupInvalidation::Reseed
            } else {
                RollupInvalidation::Keep
            };

            Ok((
                (
                    ResetRecordsOutcome {
                        runs_canceled,
                        state_entries_removed,
                        runtime_removed: false,
                        rotation,
                        carryover_agents,
                        hard,
                    },
                    canceled_runs,
                ),
                Some(rollup),
            ))
        })?;

        for record in &canceled_runs {
            crate::store::run::wake_run(&self.inner.runtime, record);
        }
        outcome.runtime_removed =
            remove_runtime_dir_with(&self.inner.runtime.root, remove_dir_if_exists)?;
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use serde_json::json;

    use super::*;
    use crate::disk::paths::{RuntimePaths, StatePaths};
    use crate::ids::WorkspaceId;
    use crate::store::event::EventEnvelope;

    #[test]
    fn runtime_cleanup_does_not_race_canonical_path_writers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = dir.path().join("runtime");
        fs::create_dir(&runtime).expect("runtime directory");
        fs::write(runtime.join("old-hint"), b"old").expect("old runtime hint");

        let removed = remove_runtime_dir_with(&runtime, |detached| {
            // Force the late writer into the recursive-cleanup window.
            fs::create_dir_all(&runtime).expect("late writer recreates runtime");
            fs::write(runtime.join("late-hint"), b"late").expect("late runtime hint");
            assert!(detached.join("old-hint").exists());
            let removed = remove_dir_if_exists(detached)?;
            assert!(!detached.exists());
            Ok(removed)
        })
        .expect("runtime cleanup succeeds despite late writer");

        assert!(removed);
        assert!(!runtime.join("old-hint").exists());
        assert_eq!(fs::read(runtime.join("late-hint")).unwrap(), b"late");
    }

    #[test]
    fn runtime_cleanup_accepts_an_absent_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(
            !remove_runtime_dir_with(&dir.path().join("absent"), |_| {
                panic!("an absent runtime must not need recursive cleanup")
            })
            .expect("absent runtime is already clean")
        );
    }

    #[test]
    fn soft_reset_writes_carryover_before_archiving_active_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime paths");
        let store = Store::open(paths.clone(), runtime).expect("open store");
        event_log::append(
            &paths.events_log,
            &EventEnvelope::new(
                workspace_id,
                "rimz-test",
                "rimz",
                "cli",
                "test.event",
                json!({}),
            ),
        )
        .expect("seed event");

        let rotate_called = Cell::new(false);
        store
            .reset_records_with(false, true, |events_log, archive_dir, min_bytes| {
                rotate_called.set(true);
                assert!(
                    paths.agents_carryover.exists(),
                    "soft reset must persist carryover before archiving the only active-log copy"
                );
                event_log::rotate(events_log, archive_dir, min_bytes)
            })
            .expect("reset records");

        assert!(rotate_called.get(), "test rotate hook should run");
    }
}
