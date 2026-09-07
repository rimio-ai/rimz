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
}

type Result<T> = std::result::Result<T, InstanceErr>;

pub(super) fn path(state_root: &Path) -> PathBuf {
    state_root.join("loop-instances.json")
}

fn lock_path(state_root: &Path) -> PathBuf {
    state_root.join("loop-instances.lock")
}

pub(super) fn insert(state_root: &Path, name: &str, entry: &TaskEntry) -> Result<()> {
    insert_into(state_root, name, entry)
}

pub(super) fn remove(state_root: &Path, name: &str) -> Result<bool> {
    remove_from(state_root, name)
}

pub(super) fn rename(state_root: &Path, old: &str, new: &str) -> Result<bool> {
    rename_from(state_root, old, new)
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
            current.entry(name).or_insert(entry);
        }
        destinations.push((root, guard, current));
    }
    // Keep destination locks until the source is cleared, including across all
    // publishes, so another writer cannot remove a row before migration commits.
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

fn insert_into(state_root: &Path, name: &str, entry: &TaskEntry) -> Result<()> {
    mutate(state_root, |tasks| {
        tasks.insert(name.to_owned(), entry.clone());
        Ok(((), true))
    })
}

fn remove_from(state_root: &Path, name: &str) -> Result<bool> {
    mutate(state_root, |tasks| {
        let removed = tasks.remove(name).is_some();
        Ok((removed, removed))
    })
}

fn rename_from(state_root: &Path, old: &str, new: &str) -> Result<bool> {
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
    mutate(state_root, |tasks| {
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
                        .wake
                        .as_ref()
                        .zip(entry.wake.as_ref())
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
            return Ok(((name.clone(), true), false));
        }
        let name = name.map(ToOwned::to_owned).unwrap_or_else(|| {
            let petname = crate::agents::petname::mint(
                tasks
                    .keys()
                    .chain(taken)
                    .filter_map(|name| name.strip_prefix("wake-")),
            );
            format!("wake-{petname}")
        });
        tasks.insert(name.clone(), entry.clone());
        Ok(((name, false), true))
    })
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
                entry.wake.as_ref().is_some_and(|target| {
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

pub(super) fn remove_signal_wake(
    state_root: &Path,
    name: &str,
    candidate: &TaskEntry,
) -> Result<bool> {
    mutate(state_root, |tasks| {
        if !tasks
            .get(name)
            .is_some_and(|current| same_subscription(current, candidate))
        {
            return Ok((false, false));
        }
        tasks.remove(name);
        Ok((true, true))
    })
}

pub(super) fn claim_expired(
    state_root: &Path,
    name: &str,
    candidate: &TaskEntry,
    now: Timestamp,
) -> Result<Option<TaskEntry>> {
    claim_expired_in(state_root, name, candidate, now)
}

fn claim_expired_in(
    state_root: &Path,
    name: &str,
    candidate: &TaskEntry,
    now: Timestamp,
) -> Result<Option<TaskEntry>> {
    mutate(state_root, |tasks| {
        let Some(current) = tasks.get(name) else {
            return Ok((None, false));
        };
        if !same_subscription(current, candidate) || !super::fire::deadline_expired_at(current, now)
        {
            return Ok((None, false));
        }
        Ok((tasks.remove(name), true))
    })
}

fn same_subscription(current: &TaskEntry, candidate: &TaskEntry) -> bool {
    current.resolved_root() == candidate.resolved_root()
        && current.signal == candidate.signal
        && current.matches == candidate.matches
        && current.wake == candidate.wake
        && current
            .wake_meta
            .as_ref()
            .zip(candidate.wake_meta.as_ref())
            .is_some_and(|(a, b)| a.armed_at == b.armed_at)
}

pub(super) fn arm_signal_wake(
    state_root: &Path,
    entry: &TaskEntry,
    taken: &BTreeSet<String>,
    now: Timestamp,
) -> Result<String> {
    arm_signal_wake_in(state_root, entry, taken, now)
}

fn arm_signal_wake_in(
    state_root: &Path,
    entry: &TaskEntry,
    taken: &BTreeSet<String>,
    now: Timestamp,
) -> Result<String> {
    mutate(state_root, |tasks| {
        if let Some((name, current)) = tasks.iter_mut().find(|(name, current)| {
            !taken.contains(*name)
                && current.wake_meta.is_some()
                && current.deadline.is_some_and(|deadline| deadline > now)
                && current.wake.as_ref().zip(entry.wake.as_ref()).is_some_and(
                    |(current, target)| {
                        current.kind == target.kind && current.session == target.session
                    },
                )
                && current.signal == entry.signal
                && current
                    .matches
                    .iter()
                    .flatten()
                    .eq(entry.matches.iter().flatten())
                && current.resolved_root() == entry.resolved_root()
        }) {
            *current = entry.clone();
            return Ok((name.clone(), true));
        }
        let petname = crate::agents::petname::mint(
            tasks
                .keys()
                .chain(taken)
                .filter_map(|name| name.strip_prefix("wake-")),
        );
        let name = format!("wake-{petname}");
        tasks.insert(name.clone(), entry.clone());
        Ok((name, true))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal_wake() -> TaskEntry {
        TaskEntry {
            wake: Some(crate::config::TaskTarget {
                kind: "claude".to_owned(),
                session: "session-1".to_owned(),
                handle: "@claude".to_owned(),
            }),
            wake_meta: Some(crate::config::WakeMeta {
                armed_by: crate::config::WakeArmer::Human,
                armed_at: Timestamp::UNIX_EPOCH,
                delay: None,
            }),
            root: PathBuf::from("/repo"),
            prompt: Some("original note".to_owned()),
            signal: Some("ci.failed".to_owned()),
            timeout: Some("59m".to_owned()),
            deadline: Some(Timestamp::from_second(3540).expect("deadline")),
            ..TaskEntry::default()
        }
    }

    #[test]
    fn stale_expiry_candidate_rechecks_removed_and_replaced_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let candidate = signal_wake();
        let now = candidate.deadline.expect("deadline");
        insert_into(dir.path(), "wake-test", &candidate).expect("insert");
        remove_from(dir.path(), "wake-test").expect("remove");
        assert!(
            claim_expired_in(dir.path(), "wake-test", &candidate, now)
                .expect("removed expiry")
                .is_none()
        );
        for replacement in [
            TaskEntry {
                root: PathBuf::from("/other"),
                ..candidate.clone()
            },
            TaskEntry {
                wake_meta: Some(crate::config::WakeMeta {
                    armed_at: now,
                    ..candidate.wake_meta.clone().expect("meta")
                }),
                ..candidate.clone()
            },
        ] {
            insert_into(dir.path(), "wake-test", &replacement).expect("replacement");
            assert!(
                claim_expired_in(dir.path(), "wake-test", &candidate, now)
                    .expect("replaced expiry")
                    .is_none()
            );
            assert_eq!(load_from(dir.path()).0["wake-test"], replacement);
        }
    }

    #[test]
    fn signal_wake_expiry_claim_removes_row_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let candidate = signal_wake();
        insert_into(dir.path(), "wake-test", &candidate).expect("insert");
        assert!(
            claim_expired_in(dir.path(), "wake-test", &candidate, Timestamp::UNIX_EPOCH)
                .expect("early expiry")
                .is_none()
        );
        let now = candidate.deadline.expect("deadline");
        assert_eq!(
            claim_expired_in(dir.path(), "wake-test", &candidate, now).expect("expiry"),
            Some(candidate.clone())
        );
        assert!(load_from(dir.path()).0.is_empty());
        assert!(
            claim_expired_in(dir.path(), "wake-test", &candidate, now)
                .expect("duplicate expiry")
                .is_none()
        );
    }

    #[test]
    fn identical_signal_wake_arm_replaces_row_in_place() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = signal_wake();
        let name = arm_signal_wake_in(dir.path(), &entry, &BTreeSet::new(), Timestamp::UNIX_EPOCH)
            .expect("first arm");
        assert_eq!(
            load_from(dir.path()).0,
            BTreeMap::from([(name.clone(), entry.clone())])
        );
        let mut replacement = entry.clone();
        replacement.prompt = Some("replacement note".to_owned());
        replacement.timeout = Some("1m".to_owned());
        replacement.matches = Some(BTreeMap::new());
        replacement.wake.as_mut().expect("target").handle = "@renamed".to_owned();
        let now = Timestamp::from_second(120).expect("now");
        replacement.wake_meta.as_mut().expect("meta").armed_at = now;
        replacement.wake_meta.as_mut().expect("meta").armed_by = crate::config::WakeArmer::Agent {
            handle: "@planner".to_owned(),
        };
        replacement.deadline = Some(Timestamp::from_second(180).expect("new deadline"));
        let reused_name =
            arm_signal_wake_in(dir.path(), &replacement, &BTreeSet::new(), now).expect("rearm");
        assert_eq!(reused_name, name);
        assert_eq!(
            load_from(dir.path()).0,
            BTreeMap::from([(name.clone(), replacement.clone())])
        );
        assert!(
            claim_expired_in(dir.path(), &name, &entry, entry.deadline.expect("deadline"))
                .expect("stale expiry after rearm")
                .is_none()
        );
        replacement.prompt = None;
        replacement.prompt_file = Some(PathBuf::from("/repo/note.md"));
        let reused_name = arm_signal_wake_in(dir.path(), &replacement, &BTreeSet::new(), now)
            .expect("rearm with note file");
        assert_eq!(reused_name, name);
        assert_eq!(
            load_from(dir.path()).0,
            BTreeMap::from([(name, replacement)])
        );
    }

    #[test]
    fn concurrent_signal_wake_arms_publish_one_instance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let candidates = [0, 1].map(|index| {
            let mut entry = signal_wake();
            entry.prompt = Some(format!("note {index}"));
            entry.timeout = Some(format!("{}m", index + 1));
            entry.deadline = Some(Timestamp::from_second(60 * (index + 1)).expect("deadline"));
            entry
        });
        let writers = candidates.clone().map(|entry| {
            let root = dir.path().to_path_buf();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                arm_signal_wake_in(&root, &entry, &BTreeSet::new(), Timestamp::UNIX_EPOCH)
                    .expect("atomic arm")
            })
        });
        barrier.wait();
        let results = writers.map(|writer| writer.join().expect("writer"));
        assert_eq!(results[0], results[1]);
        let stored = load_from(dir.path()).0;
        assert_eq!(stored.len(), 1);
        assert!(candidates.contains(&stored[&results[0]]));
    }

    #[test]
    fn signal_wake_arm_does_not_reuse_expired_removed_or_different_subscriptions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = signal_wake();
        let taken = BTreeSet::new();
        for field in ["deadline", "root", "target", "selector", "match", "meta"] {
            let mut old = entry.clone();
            match field {
                "deadline" => old.deadline = Some(Timestamp::UNIX_EPOCH),
                "root" => old.root = PathBuf::from("/other"),
                "target" => old.wake.as_mut().expect("target").session = "other".to_owned(),
                "selector" => old.signal = Some("ci.*".to_owned()),
                "match" => {
                    old.matches = Some(BTreeMap::from([("branch".to_owned(), "main".to_owned())]))
                }
                "meta" => old.wake_meta = None,
                _ => unreachable!("fixed test cases"),
            }
            insert_into(dir.path(), "wake-old", &old).expect("old subscription");
            let name = arm_signal_wake_in(dir.path(), &entry, &taken, Timestamp::UNIX_EPOCH)
                .expect("distinct arm");
            assert_ne!(name, "wake-old", "{field}");
            assert_eq!(
                load_from(dir.path()).0,
                BTreeMap::from([("wake-old".to_owned(), old), (name.clone(), entry.clone())])
            );
            remove_from(dir.path(), &name).expect("remove fresh");
            remove_from(dir.path(), "wake-old").expect("remove old");
        }
        let name = arm_signal_wake_in(dir.path(), &entry, &taken, Timestamp::UNIX_EPOCH)
            .expect("arm after retirement");
        assert_eq!(load_from(dir.path()).0, BTreeMap::from([(name, entry)]));
    }

    fn task() -> TaskEntry {
        TaskEntry {
            agent: Some("claude".to_owned()),
            prompt: Some("wake".to_owned()),
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

        insert_into(dir.path(), "wake", &entry).expect("insert");
        let encoded = std::fs::read_to_string(path(dir.path())).expect("serialized instances");
        let value: serde_json::Value = serde_json::from_str(&encoded).expect("instances json");
        assert_eq!(value["wake"]["agent"], "claude");
        assert_eq!(value["wake"]["prompt"], "wake");
        assert_eq!(value["wake"]["root"], "/repo");
        assert_eq!(value["wake"]["at"], "07:00");
        assert_eq!(
            load_from(dir.path())
                .0
                .get("wake")
                .map(|entry| entry.prompt.as_deref()),
            Some(Some("wake"))
        );

        assert!(remove_from(dir.path(), "wake").expect("remove"));
        assert!(load_from(dir.path()).0.is_empty());
        assert!(!remove_from(dir.path(), "wake").expect("remove absent"));
    }

    #[test]
    fn rename_moves_existing_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = task();

        insert_into(dir.path(), "wake", &entry).expect("insert");

        assert!(rename_from(dir.path(), "wake", "nudge").expect("rename"));
        let tasks = load_from(dir.path());
        assert!(!tasks.0.contains_key("wake"));
        assert_eq!(
            tasks.0.get("nudge").map(|entry| entry.prompt.as_deref()),
            Some(Some("wake"))
        );
        assert!(!rename_from(dir.path(), "wake", "later").expect("rename absent"));
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
                insert_into(&root, name, &task()).expect("insert instance");
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
