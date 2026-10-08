use std::fs;
use std::io;
use std::path::Path;

use jiff::Timestamp;

use crate::disk::paths::remove_state_dir_with;
use crate::store::run::{self as run, RunRecord, RunStatus};

use super::super::{Result, Store, StoreErr, event_log, snapshot};
use super::{ResetRecordsOutcome, RollupInvalidation, remove_file_if_exists};

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

fn remove_owned_except_runs(
    paths: &super::super::StatePaths,
    count: &impl Fn(&Path) -> Result<usize>,
) -> Result<usize> {
    let owned = crate::disk::paths::Class::Owned.path_under(&paths.root);
    let entries = match fs::read_dir(&owned) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(source) => {
            return Err(StoreErr::Io {
                path: owned,
                source,
            });
        }
    };
    // Listed whole before the first removal: detaching a child reuses the
    // name an interrupted reset's leftover may hold in this same directory.
    let mut children = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| StoreErr::Io {
            path: owned.clone(),
            source,
        })?;
        let path = entry.path();
        if path == paths.runs_dir {
            continue;
        }
        let kind = entry.file_type().map_err(|source| StoreErr::Io {
            path: path.clone(),
            source,
        })?;
        children.push((path, kind));
    }
    let mut removed = 0;
    for (path, kind) in children {
        if kind.is_dir() {
            removed += remove_state_dir_with(&path, count)?.unwrap_or(0);
        } else {
            remove_file_if_exists(&path)?;
            removed += 1;
        }
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
    /// its provider launch defaults in the same transaction.
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
        self.reset_records_counting_with(hard, unfreeze_logins, rotate, count_dir_entries_recursive)
    }

    /// `count` walks each removed directory after it is detached from its
    /// canonical path, so it never reads a tree another process can write.
    fn reset_records_counting_with<F, C>(
        &self,
        hard: bool,
        unfreeze_logins: bool,
        rotate: F,
        count: C,
    ) -> Result<ResetRecordsOutcome>
    where
        F: FnOnce(&Path, &Path, u64) -> event_log::Result<event_log::RotationOutcome>,
        C: Fn(&Path) -> Result<usize>,
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

            // Reset ends every agent, and a soft reset leaves them resumable:
            // a surviving request would stop the resumed session.
            remove_file_if_exists(&paths.idle_stop_requests)?;
            {
                let _ingress = crate::disk::lock::WorkspaceLock::acquire(&paths.hook_ingress_lock)?;
                remove_file_if_exists(&paths.hook_ingress_log)?;
                remove_file_if_exists(&paths.hook_drain_cursor)?;
            }

            // A reset removes every account pin; subsequent launches inherit.
            if unfreeze_logins
                && let Some(mut record) =
                    crate::workspace::record::read_optional(&paths.workspace_record)?
                && !record.pins.is_empty()
            {
                record.pins.clear();
                crate::workspace::record::write(paths, &record)?;
            }

            let mut state_entries_removed =
                remove_state_dir_with(&paths.cache_dir, &count)?.unwrap_or(0);

            let rotation = rotate(&paths.events_log, &paths.events_archive_dir, 0)?;
            if hard {
                // The rotation above archived the log; hard reset drops the
                // active file and keeps the archive, as it always has.
                remove_file_if_exists(&paths.events_log)?;
                for class in [
                    crate::disk::paths::Class::Audit,
                    crate::disk::paths::Class::Tmp,
                    crate::disk::paths::Class::Out,
                    crate::disk::paths::Class::Shared,
                ] {
                    state_entries_removed +=
                        remove_state_dir_with(&class.path_under(&paths.root), &count)?.unwrap_or(0);
                }
                state_entries_removed += remove_owned_except_runs(paths, &count)?;
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
        outcome.runtime_removed = self.inner.runtime.remove_disposable_dirs()?;
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::disk::paths::{RuntimePaths, StatePaths};
    use crate::ids::WorkspaceId;
    use crate::store::event::EventEnvelope;

    #[test]
    fn reset_applies_lifetime_classes() {
        for hard in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let id = WorkspaceId::from_project_root(dir.path());
            let paths = StatePaths::under(id.clone(), dir.path()).unwrap();
            let runtime = RuntimePaths::under(id, dir.path()).unwrap();
            let store = Store::open(paths.clone(), runtime).unwrap();
            let audit = crate::diag::DiagSink::under(
                paths.root.clone(),
                paths.workspace_id.clone(),
                "test",
                None,
            )
            .log_path()
            .unwrap();
            let owned = paths.agents_dir.join("retired/scratch/note");
            let tmp = paths.temp_unit_dir(Some("retired")).join("note");
            let out = paths.out_reader_dir(Some("retired")).join("child.output");
            let shared = paths.room_shared_dir.join("task/note");
            let cache = paths.cache_dir.join("obsolete.json");
            let record = paths.fleet_budget_record.clone();
            for path in [&audit, &owned, &tmp, &out, &shared, &cache, &record] {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, b"retained").unwrap();
            }
            store.reset_records(hard).unwrap();
            assert_eq!(audit.exists(), !hard, "audit retention follows reset mode");
            assert_eq!(owned.exists(), !hard);
            assert_eq!(tmp.exists(), !hard);
            assert_eq!(out.exists(), !hard);
            assert_eq!(shared.exists(), !hard);
            assert!(!cache.exists());
            assert!(record.exists());
        }
    }

    #[test]
    fn reset_drops_every_pending_idle_stop() {
        for hard in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let id = WorkspaceId::from_project_root(dir.path());
            let paths = StatePaths::under(id.clone(), dir.path()).unwrap();
            let runtime = RuntimePaths::under(id, dir.path()).unwrap();
            let store = Store::open(paths.clone(), runtime).unwrap();
            for session in ["resting", "working"] {
                crate::store::idle_stop::arm(
                    &paths,
                    crate::store::idle_stop::IdleStopRequest {
                        kind: crate::ids::AgentKind::new_unchecked("claude"),
                        agent_id: session.into(),
                        stop: crate::agents::state::IdleStop {
                            after_secs: 180,
                            requested_at: Timestamp::UNIX_EPOCH,
                            requested_by: None,
                        },
                    },
                )
                .unwrap();
            }
            store.reset_records(hard).unwrap();
            assert!(
                crate::store::idle_stop::read(&paths).is_empty(),
                "hard={hard}: a resumed agent must not inherit a stop"
            );
        }
    }

    #[test]
    fn reset_preserves_another_threads_lock() {
        let dir = tempfile::tempdir().unwrap();
        let id = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(id.clone(), dir.path()).unwrap();
        let runtime = RuntimePaths::under(id, dir.path()).unwrap();
        let lock_path = runtime.lock_path("loop-watch-held.lock");
        let store = Store::open(paths, runtime).unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let thread_path = lock_path.clone();
        let holder = std::thread::spawn(move || {
            let _guard = crate::disk::lock::WorkspaceLock::acquire(&thread_path).unwrap();
            ready_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        ready_rx.recv().unwrap();
        for hard in [false, true] {
            store
                .reset_records_with(hard, true, event_log::rotate)
                .unwrap();
            assert!(lock_path.exists());
            assert!(
                crate::disk::lock::WorkspaceLock::try_acquire(&lock_path)
                    .unwrap()
                    .is_none()
            );
        }
        release_tx.send(()).unwrap();
        holder.join().unwrap();
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

    fn open_store(dir: &Path) -> (Store, StatePaths) {
        let id = WorkspaceId::from_project_root(dir);
        let paths = StatePaths::under(id.clone(), dir).unwrap();
        let runtime = RuntimePaths::under(id, dir).unwrap();
        (Store::open(paths.clone(), runtime).unwrap(), paths)
    }

    fn seed(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"seeded").unwrap();
    }

    fn reset_sibling(dir: &Path) -> PathBuf {
        let mut name = dir.file_name().unwrap().to_os_string();
        name.push(".reset");
        dir.with_file_name(name)
    }

    #[test]
    fn reset_survives_writers_inside_each_removal_window() {
        for hard in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let (store, paths) = open_store(dir.path());
            let owned = crate::disk::paths::Class::Owned.path_under(&paths.root);
            let audit = crate::disk::paths::Class::Audit.path_under(&paths.root);
            let mut removed_dirs = vec![paths.cache_dir.clone()];
            let mut seeded = vec![paths.cache_dir.join("obsolete.json")];
            if hard {
                removed_dirs.extend([
                    audit.clone(),
                    paths.tmp_dir.clone(),
                    paths.out_dir.clone(),
                    paths.room_shared_dir.clone(),
                    paths.agents_dir.clone(),
                    owned.join("unit.v2"),
                ]);
                seeded.extend([
                    audit.join("test/log.jsonl"),
                    paths.temp_unit_dir(Some("retired")).join("note"),
                    paths.out_reader_dir(Some("retired")).join("child.output"),
                    paths.room_shared_dir.join("task/note"),
                    paths.agents_dir.join("retired/scratch/note"),
                    owned.join("unit.v2/note"),
                ]);
            }
            seeded.iter().for_each(|path| seed(path));
            let seeded_entries: usize = removed_dirs
                .iter()
                .map(|dir| count_dir_entries_recursive(dir).unwrap())
                .sum();

            let late = RefCell::new(Vec::new());
            let outcome = store
                .reset_records_counting_with(hard, true, event_log::rotate, |tree| {
                    // Force a writer into the window: it addresses the directory
                    // by its canonical path while the reset is removing it.
                    let canonical = removed_dirs
                        .iter()
                        .find(|dir| *dir == tree || reset_sibling(dir) == tree)
                        .unwrap_or_else(|| panic!("unexpected removal of {}", tree.display()));
                    fs::create_dir_all(canonical).unwrap();
                    let file = canonical.join("late");
                    fs::write(&file, b"late").unwrap();
                    late.borrow_mut().push(file);
                    count_dir_entries_recursive(tree)
                })
                .unwrap_or_else(|err| panic!("hard={hard}: reset failed on a late writer: {err}"));

            let late = late.into_inner();
            assert_eq!(late.len(), removed_dirs.len(), "hard={hard}");
            for file in &late {
                assert!(file.exists(), "hard={hard}: {} was removed", file.display());
            }
            for path in &seeded {
                assert!(!path.exists(), "hard={hard}: {} survived", path.display());
            }
            for dir in &removed_dirs {
                assert!(!reset_sibling(dir).exists(), "hard={hard}");
            }
            assert_eq!(outcome.state_entries_removed, seeded_entries, "hard={hard}");
            assert!(paths.runs_dir.exists(), "hard={hard}");
            assert!(!reset_sibling(&paths.runs_dir).exists(), "hard={hard}");
            assert!(paths.workspace_lock.exists(), "hard={hard}");
        }
    }

    #[test]
    fn reset_removes_an_interrupted_resets_leftovers_uncounted() {
        for hard in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let (store, paths) = open_store(dir.path());
            let owned = crate::disk::paths::Class::Owned.path_under(&paths.root);
            let audit = crate::disk::paths::Class::Audit.path_under(&paths.root);
            // `audit/` and `owned/agents/` are absent: only their leftovers exist.
            assert!(!audit.exists() && !paths.agents_dir.exists());
            let mut leftovers = vec![reset_sibling(&paths.cache_dir)];
            if hard {
                leftovers.extend([reset_sibling(&audit), reset_sibling(&paths.agents_dir)]);
            }
            for leftover in &leftovers {
                seed(&leftover.join("nested/old"));
            }
            seed(&owned.join("unit/note"));
            let live_entries =
                count_dir_entries_recursive(&paths.cache_dir).unwrap() + if hard { 1 } else { 0 };

            let outcome = store.reset_records(hard).unwrap();

            for leftover in &leftovers {
                assert!(!leftover.exists(), "hard={hard}: {}", leftover.display());
            }
            assert!(!reset_sibling(&reset_sibling(&paths.agents_dir)).exists());
            assert_eq!(outcome.state_entries_removed, live_entries, "hard={hard}");
            assert_eq!(owned.join("unit/note").exists(), !hard);
        }
    }
}
