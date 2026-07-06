use std::fs;
use std::io;
use std::path::Path;

use jiff::Timestamp;

use crate::harness::run::{RunRecord, RunStatus};

use super::super::{
    Ledger, LedgerErr, ResetRecordsOutcome, Result, event_log, lock, run_store, snapshot,
};

fn remove_file_if_exists(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(LedgerErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn remove_dir_if_exists(path: &Path) -> Result<bool> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(true),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(LedgerErr::Io {
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
            return Err(LedgerErr::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let mut count = 0;
    for entry in entries {
        let entry = entry.map_err(|source| LedgerErr::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let child = entry.path();
        count += 1;
        let meta = fs::symlink_metadata(&child).map_err(|source| LedgerErr::Io {
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

fn remove_matching_files(root: &Path, prefixes: &[&str]) -> Result<usize> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(source) => {
            return Err(LedgerErr::Io {
                path: root.to_path_buf(),
                source,
            });
        }
    };
    let mut removed = 0;
    for entry in entries {
        let entry = entry.map_err(|source| LedgerErr::Io {
            path: root.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !prefixes.iter().any(|prefix| name.starts_with(prefix)) {
            continue;
        }
        let meta = fs::symlink_metadata(&path).map_err(|source| LedgerErr::Io {
            path: path.clone(),
            source,
        })?;
        if meta.is_dir() {
            fs::remove_dir_all(&path).map_err(|source| LedgerErr::Io {
                path: path.clone(),
                source,
            })?;
        } else {
            fs::remove_file(&path).map_err(|source| LedgerErr::Io {
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
    for mut record in run_store::list(&paths.runs_dir)? {
        if record.status.is_terminal() {
            continue;
        }
        let now = Timestamp::now();
        record.status = RunStatus::Canceled;
        record.updated_at = now;
        record.completed_at = Some(now);
        run_store::write(&paths.runs_dir, &record)?;
        canceled.push(record);
    }
    Ok(canceled)
}

impl Ledger {
    /// Archive the room's active records and clear coordination/debug state for
    /// a user-requested room reset. The mux teardown has already killed panes;
    /// this method terminal-wakes any surviving waiters and makes the ledger
    /// match that product boundary.
    #[must_use = "durability barrier; check the result"]
    pub fn reset_records(&self, session_name: &str, hard: bool) -> Result<ResetRecordsOutcome> {
        self.reset_records_with(session_name, hard, event_log::rotate)
    }

    fn reset_records_with<F>(
        &self,
        _session_name: &str,
        hard: bool,
        rotate: F,
    ) -> Result<ResetRecordsOutcome>
    where
        F: FnOnce(&Path, &Path, u64) -> event_log::Result<event_log::RotationOutcome>,
    {
        let (mut outcome, canceled_runs) = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let _publish_guard = lock::WorkspaceLock::acquire(&self.inner.paths.publish_lock)?;

            let canceled_runs = cancel_active_runs_for_reset_locked(&self.inner.paths)?;
            let runs_canceled = canceled_runs.len();

            let carryover_agents = if hard {
                remove_file_if_exists(&self.inner.paths.agents_carryover)?;
                0
            } else {
                super::stage_agent_carryover_for_rotation(&self.inner.paths, 0)?
            };

            let rotation = rotate(
                &self.inner.paths.events_log,
                &self.inner.paths.events_archive_dir,
                0,
            )?;

            if hard {
                super::publish::retract_publish_stamp(&self.inner.paths);
                remove_file_if_exists(&self.inner.paths.latest_snapshot)?;
                remove_file_if_exists(&self.inner.paths.events_log)?;
                remove_file_if_exists(&self.inner.paths.rollup_cache)?;
            } else if rotation.is_rotated() {
                super::invalidate_snapshot_caches(
                    &self.inner.paths,
                    super::RollupInvalidation::Reseed,
                )?;
            } else {
                super::publish::retract_publish_stamp(&self.inner.paths);
                remove_file_if_exists(&self.inner.paths.latest_snapshot)?;
            }

            self.inner.paths.ensure_dirs()?;

            let mut state_entries_removed = 0;
            state_entries_removed += remove_matching_files(&self.inner.paths.root, &["diag.log"])?;
            state_entries_removed += remove_dir_counting_entries(&crate::diag::frames_dir_under(
                &self.inner.paths.root,
            ))?;

            if hard {
                remove_file_if_exists(&self.inner.paths.latest_snapshot)?;
            } else {
                snapshot::rebuild(&self.inner.paths)?;
            }

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
            )
        };

        for record in &canceled_runs {
            self.wake_run_best_effort(record);
        }
        outcome.runtime_removed = remove_dir_if_exists(&self.inner.runtime.root)?;
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use serde_json::json;

    use super::*;
    use crate::ids::WorkspaceId;
    use crate::ledger::event::EventEnvelope;
    use crate::ledger::paths::{RuntimePaths, StatePaths};

    #[test]
    fn soft_reset_writes_carryover_before_archiving_active_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime paths");
        let ledger = Ledger::open(paths.clone(), runtime).expect("open ledger");
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
        ledger
            .reset_records_with("rimz-test", false, |events_log, archive_dir, min_bytes| {
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
