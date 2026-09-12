//! RimZ-owned loop task instances and merged loop task reads.
//!
//! Durable recurring definitions live in `loop.toml`. Session deliveries,
//! one-shots, and poll-until instances live here as workspace state, using
//! the same task entry shape without turning runtime churn into user config
//! edits. Readers merge both backings here; durable config wins when both
//! stores contain a name.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::config::{TaskEntry, Tasks};
use crate::disk::atomic::{AtomicErr, write_temp_then_rename};
use crate::disk::lock::{LockErr, WorkspaceLock};
use crate::disk::paths::workspaces_dir_under;
use crate::ids::WorkspaceId;
use jiff::Timestamp;

#[derive(Debug, thiserror::Error)]
pub(super) enum InstanceErr {
    #[error(transparent)]
    Lock(#[from] LockErr),
    #[error(transparent)]
    Write(#[from] AtomicErr),
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("reading {path}: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("migrating instance arming state: {0}")]
    Arming(#[from] super::arming::ArmingError),
    #[error("migrating instance strike state: {0}")]
    Strikes(#[from] super::strikes::StrikesError),
}

type Result<T> = std::result::Result<T, InstanceErr>;

pub(super) fn path(state_root: &Path) -> PathBuf {
    state_root.join("loop-instances.json")
}

fn lock_path(state_root: &Path) -> PathBuf {
    state_root.join("loop-instances.lock")
}

pub(super) fn load_from(state_root: &Path) -> Tasks {
    load_strict_from(state_root).unwrap_or_default()
}

pub(super) fn load_strict_from(state_root: &Path) -> Result<Tasks> {
    let path = path(state_root);
    match std::fs::read(&path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|source| InstanceErr::Parse { path, source })
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Tasks::default()),
        Err(source) => Err(InstanceErr::Read { path, source }),
    }
}

pub(super) fn migrate_legacy(state_home: &Path) -> Result<()> {
    let legacy_root = state_home.join("rimz");
    let legacy_path = path(&legacy_root);
    match std::fs::metadata(&legacy_path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(InstanceErr::Read {
                path: legacy_path,
                source,
            });
        }
        Ok(_) => {}
    }
    let _guard = WorkspaceLock::acquire(&lock_path(&legacy_root))?;
    let legacy = load_strict_from(&legacy_root)?;
    let mut workspaces = BTreeMap::<PathBuf, BTreeMap<String, TaskEntry>>::new();
    let mut overlay_keys = Vec::new();
    for (name, entry) in legacy.0 {
        let root = workspaces_dir_under(state_home)
            .join(WorkspaceId::from_project_root(&entry.resolved_root()).as_str());
        workspaces.entry(root).or_default().insert(name, entry);
    }
    let mut destinations = Vec::new();
    for (root, entries) in workspaces {
        let guard = WorkspaceLock::acquire(&lock_path(&root))?;
        let mut current = load_strict_from(&root)?.0;
        for (name, entry) in entries {
            if current.get(&name).is_none_or(|existing| existing == &entry) {
                overlay_keys.push((
                    super::arming::TaskKey::for_task(
                        &name,
                        super::catalog::TaskSource::Config,
                        &entry.resolved_root(),
                    ),
                    super::arming::TaskKey::for_task(
                        &name,
                        super::catalog::TaskSource::Instance,
                        &entry.resolved_root(),
                    ),
                ));
            }
            current.entry(name).or_insert(entry);
        }
        destinations.push((root, guard, current));
    }
    // Keep destination locks until the source is cleared, including across all
    // publishes, so another writer cannot remove a row before migration commits.
    super::arming::migrate_instance_keys(state_home, &overlay_keys)?;
    super::strikes::migrate_instance_keys(state_home, &overlay_keys)?;
    for (root, _, entries) in &destinations {
        write_temp_then_rename(&path(root), entries)?;
    }
    // Persist an empty source before unlinking so a crash cannot resurrect rows
    // if the unlink itself has not reached disk.
    write_temp_then_rename(&legacy_path, &Tasks::default())?;
    std::fs::remove_file(&legacy_path).map_err(|source| InstanceErr::Read {
        path: legacy_path,
        source,
    })?;
    Ok(())
}

pub(super) fn insert(state_root: &Path, name: &str, entry: &TaskEntry) -> Result<()> {
    mutate(state_root, |tasks| {
        tasks.insert(name.to_owned(), entry.clone());
        Ok(((), true))
    })
}

pub(super) fn remove(state_root: &Path, name: &str, expected: Option<&TaskEntry>) -> Result<bool> {
    mutate(state_root, |tasks| {
        if expected.is_some_and(|entry| tasks.get(name) != Some(entry)) {
            return Ok((false, false));
        }
        let removed = tasks.remove(name).is_some();
        Ok((removed, removed))
    })
}

pub(super) fn rename(state_root: &Path, old: &str, new: &str) -> Result<bool> {
    mutate(state_root, |tasks| {
        let Some(entry) = tasks.remove(old) else {
            return Ok((false, false));
        };
        tasks.insert(new.to_owned(), entry);
        Ok((true, true))
    })
}

fn mutate<T>(
    state_root: &Path,
    edit: impl FnOnce(&mut BTreeMap<String, TaskEntry>) -> Result<(T, bool)>,
) -> Result<T> {
    let _guard = WorkspaceLock::acquire(&lock_path(state_root))?;
    let mut entries = load_strict_from(state_root)?.0;
    let (result, changed) = edit(&mut entries)?;
    if changed {
        write_temp_then_rename(&path(state_root), &entries)?;
    }
    Ok(result)
}

pub(super) fn insert_delivery(
    state_root: &Path,
    name: Option<&str>,
    entry: &TaskEntry,
    taken: &BTreeSet<String>,
) -> Result<(String, bool)> {
    let _guard = WorkspaceLock::acquire(&lock_path(state_root))?;
    let mut tasks = load_strict_from(state_root)?.0;
    let arming = super::arming::load();
    let now = Timestamp::now();
    if entry.signal.is_some()
        && let Some((name, _)) = tasks.iter().find(|(name, current)| {
            let key = super::arming::TaskKey::for_task(
                name,
                super::catalog::TaskSource::Instance,
                &current.resolved_root(),
            );
            !taken.contains(*name)
                && super::arming::ArmState::resolve(
                    arming.get(&key),
                    super::catalog::TaskSource::Instance,
                    now,
                ) == super::arming::ArmState::Live
                && current
                    .wait
                    .as_ref()
                    .zip(entry.wait.as_ref())
                    .is_some_and(|(a, b)| a.kind == b.kind && a.session == b.session)
                && current
                    .signal
                    .as_deref()
                    .and_then(|raw| raw.parse::<super::signal::SignalSelector>().ok())
                    == entry.signal.as_deref().and_then(|raw| raw.parse().ok())
                && current
                    .matches
                    .iter()
                    .flatten()
                    .eq(entry.matches.iter().flatten())
                && current.resolved_root() == entry.resolved_root()
        })
    {
        return Ok((name.clone(), true));
    }
    let name = name.map(ToOwned::to_owned).unwrap_or_else(|| {
        let petname = crate::agents::petname::mint(
            tasks
                .keys()
                .chain(taken)
                .filter_map(|name| name.strip_prefix("wait-")),
        );
        format!("wait-{petname}")
    });
    let key = super::arming::TaskKey::for_task(
        &name,
        super::catalog::TaskSource::Instance,
        &entry.resolved_root(),
    );
    tasks.insert(name.clone(), entry.clone());
    write_temp_then_rename(&path(state_root), &tasks)?;
    super::arming::remove(&key)?;
    super::strikes::clear(&key)?;
    Ok((name, false))
}

pub(super) fn retire_session(
    state_root: &Path,
    kind: &crate::ids::AgentKind,
    session: &crate::ids::AgentSessionId,
) -> Result<Vec<String>> {
    mutate(state_root, |tasks| {
        let names = tasks
            .iter()
            .filter(|(_, entry)| {
                entry.wait.as_ref().is_some_and(|target| {
                    target.kind == kind.as_str() && target.session == session.as_str()
                })
            })
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        for name in &names {
            tasks.remove(name);
        }
        let changed = !names.is_empty();
        Ok((names, changed))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task() -> TaskEntry {
        TaskEntry {
            agent: Some("claude".to_owned()),
            prompt: Some("wait".to_owned()),
            root: PathBuf::from("/repo"),
            at: Some("07:00".to_owned()),
            ..TaskEntry::default()
        }
    }

    #[test]
    fn missing_or_corrupt_file_loads_empty() {
        let dir = tempfile::tempdir().expect("tempdir");

        assert!(load_from(dir.path()).0.is_empty());
        std::fs::create_dir_all(dir.path().join("rimz")).expect("state dir");
        std::fs::write(path(dir.path()), b"not json").expect("corrupt state");
        assert!(load_from(dir.path()).0.is_empty());
    }

    #[test]
    fn insert_and_remove_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = task();

        insert(dir.path(), "wait", &entry).expect("insert");
        let encoded = std::fs::read_to_string(path(dir.path())).expect("serialized instances");
        let value: serde_json::Value = serde_json::from_str(&encoded).expect("instances json");
        assert_eq!(value["wait"]["agent"], "claude");
        assert_eq!(value["wait"]["prompt"], "wait");
        assert_eq!(value["wait"]["root"], "/repo");
        assert_eq!(value["wait"]["at"], "07:00");
        assert_eq!(
            load_from(dir.path())
                .0
                .get("wait")
                .map(|entry| entry.prompt.as_deref()),
            Some(Some("wait"))
        );

        assert!(remove(dir.path(), "wait", None).expect("remove"));
        assert!(load_from(dir.path()).0.is_empty());
        assert!(!remove(dir.path(), "wait", None).expect("remove absent"));
    }

    #[test]
    fn rename_moves_existing_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = task();

        insert(dir.path(), "wait", &entry).expect("insert");

        assert!(rename(dir.path(), "wait", "nudge").expect("rename"));
        let tasks = load_from(dir.path());
        assert!(!tasks.0.contains_key("wait"));
        assert_eq!(
            tasks.0.get("nudge").map(|entry| entry.prompt.as_deref()),
            Some(Some("wait"))
        );
        assert!(!rename(dir.path(), "wait", "later").expect("rename absent"));
    }

    #[test]
    fn concurrent_inserts_preserve_both_instances() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let writers = ["first", "second"].map(|name| {
            let root = root.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                insert(&root, name, &task()).expect("insert instance");
            })
        });
        barrier.wait();
        for writer in writers {
            writer.join().expect("writer thread");
        }

        let tasks = load_from(&root);
        assert!(tasks.0.contains_key("first"));
        assert!(tasks.0.contains_key("second"));
    }
}
