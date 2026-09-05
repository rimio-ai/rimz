//! RimZ-owned loop task instances and merged loop task reads.
//!
//! Durable recurring definitions live in `loop.toml`. Machine-generated
//! one-shots, self-wakes, and poll-until instances live here as state, using
//! the same task entry shape without turning runtime churn into user config
//! edits. Readers merge both backings here; durable config wins when both
//! stores contain a name.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::overlay_store::OverlayStore;
use crate::config::{TaskEntry, Tasks};
use crate::disk::atomic::{AtomicErr, write_temp_then_rename};
use crate::disk::lock::{LockErr, WorkspaceLock};
use crate::disk::paths::state_home;
use anyhow::Context;
use jiff::Timestamp;

const STORE: OverlayStore = OverlayStore::new("loop-instances.json", "loop-instances.lock");

#[derive(Debug, thiserror::Error)]
pub(super) enum InstanceErr {
    #[error(transparent)]
    Lock(#[from] LockErr),
    #[error(transparent)]
    Write(#[from] AtomicErr),
    #[error("signal wake has no timeout")]
    MissingTimeout,
    #[error("invalid signal wake timeout: {0}")]
    Timeout(String),
    #[error("resolving signal wake deadline: {0}")]
    Deadline(#[from] jiff::Error),
}

type Result<T> = std::result::Result<T, InstanceErr>;

pub(super) fn path(state_root: &Path) -> PathBuf {
    STORE.path(state_root)
}

pub(super) fn load() -> Tasks {
    load_from(&state_home())
}

pub(super) fn insert(name: &str, entry: &TaskEntry) -> Result<()> {
    insert_into(&state_home(), name, entry)
}

pub(super) fn remove(name: &str) -> Result<bool> {
    remove_from(&state_home(), name)
}

pub(super) fn rename(old: &str, new: &str) -> Result<bool> {
    rename_from(&state_home(), old, new)
}

pub(super) fn load_from(state_root: &Path) -> Tasks {
    Tasks(STORE.load(state_root))
}

pub(super) fn load_strict_from(state_root: &Path) -> anyhow::Result<Tasks> {
    let path = path(state_root);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("reading {}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Tasks::default()),
        Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
    }
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
    let _guard = WorkspaceLock::acquire(&STORE.lock_path(state_root))?;
    let mut entries = STORE.load(state_root);
    let (result, changed) = edit(&mut entries)?;
    if changed {
        write_temp_then_rename(&path(state_root), &entries)?;
    }
    Ok(result)
}

pub(super) fn observe_signal_wake(
    name: &str,
    candidate: &TaskEntry,
    now: Timestamp,
) -> Result<bool> {
    observe_signal_wake_in(&state_home(), name, candidate, now)
}

fn observe_signal_wake_in(
    state_root: &Path,
    name: &str,
    candidate: &TaskEntry,
    now: Timestamp,
) -> Result<bool> {
    mutate(state_root, |tasks| {
        let Some(current) = tasks.get_mut(name) else {
            return Ok((false, false));
        };
        if !same_subscription(current, candidate) {
            return Ok((false, false));
        }
        let timeout = super::runner::parse_task_timeout(
            current
                .timeout
                .as_deref()
                .ok_or(InstanceErr::MissingTimeout)?,
        )
        .map_err(InstanceErr::Timeout)?;
        current.deadline = Some(now.checked_add(timeout)?);
        if let Some(meta) = &mut current.wake_meta {
            meta.last_observed_at = Some(now);
        }
        Ok((true, true))
    })
}

pub(super) fn remove_signal_wake(name: &str, candidate: &TaskEntry) -> Result<bool> {
    mutate(&state_home(), |tasks| {
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
    name: &str,
    candidate: &TaskEntry,
    now: Timestamp,
) -> Result<Option<TaskEntry>> {
    claim_expired_in(&state_home(), name, candidate, now)
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
    entry: &TaskEntry,
    taken: &BTreeSet<String>,
    now: Timestamp,
) -> Result<(String, TaskEntry, bool)> {
    arm_signal_wake_in(&state_home(), entry, taken, now)
}

fn arm_signal_wake_in(
    state_root: &Path,
    entry: &TaskEntry,
    taken: &BTreeSet<String>,
    now: Timestamp,
) -> Result<(String, TaskEntry, bool)> {
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
            let timeout = super::runner::parse_task_timeout(
                current
                    .timeout
                    .as_deref()
                    .ok_or(InstanceErr::MissingTimeout)?,
            )
            .map_err(InstanceErr::Timeout)?;
            current.deadline = Some(now.checked_add(timeout)?);
            return Ok(((name.clone(), current.clone(), true), true));
        }
        let petname = crate::harness::petname::mint(
            tasks
                .keys()
                .chain(taken)
                .filter_map(|name| name.strip_prefix("wake-")),
        );
        let name = format!("wake-{petname}");
        tasks.insert(name.clone(), entry.clone());
        Ok(((name, entry.clone(), false), true))
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
                last_observed_at: None,
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
    fn stale_expiry_candidate_rechecks_refreshed_removed_and_replaced_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let candidate = signal_wake();
        let now = candidate.deadline.expect("deadline");
        insert_into(dir.path(), "wake-test", &candidate).expect("insert");
        assert!(observe_signal_wake_in(dir.path(), "wake-test", &candidate, now).expect("observe"));
        assert!(
            claim_expired_in(dir.path(), "wake-test", &candidate, now)
                .expect("stale expiry")
                .is_none()
        );
        let refreshed = load_from(dir.path()).0["wake-test"].clone();
        assert_eq!(
            refreshed.wake_meta.as_ref().expect("meta").last_observed_at,
            Some(now)
        );
        assert!(
            claim_expired_in(
                dir.path(),
                "wake-test",
                &candidate,
                refreshed.deadline.expect("deadline")
            )
            .expect("expiry")
            .is_some()
        );
        assert!(
            !observe_signal_wake_in(dir.path(), "wake-test", &candidate, now)
                .expect("late observation")
        );
        assert!(
            claim_expired_in(dir.path(), "wake-test", &candidate, now)
                .expect("duplicate expiry")
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
            assert!(
                !observe_signal_wake_in(dir.path(), "wake-test", &candidate, now)
                    .expect("replaced observation")
            );
            assert_eq!(load_from(dir.path()).0["wake-test"], replacement);
        }
    }

    #[test]
    fn identical_signal_wake_arm_restarts_clock_preserving_note_and_timeout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = signal_wake();
        let (name, _, reused) =
            arm_signal_wake_in(dir.path(), &entry, &BTreeSet::new(), Timestamp::UNIX_EPOCH)
                .expect("first arm");
        assert!(!reused);
        let mut replacement = entry.clone();
        replacement.prompt = Some("replacement note".to_owned());
        replacement.timeout = Some("1m".to_owned());
        replacement.matches = Some(BTreeMap::new());
        replacement.wake.as_mut().expect("target").handle = "@renamed".to_owned();
        let now = Timestamp::from_second(120).expect("now");
        let (reused_name, rearmed, reused) =
            arm_signal_wake_in(dir.path(), &replacement, &BTreeSet::new(), now).expect("rearm");
        assert!(reused);
        assert_eq!(reused_name, name);
        let mut expected = entry;
        expected.deadline = Some(Timestamp::from_second(3660).expect("new deadline"));
        assert_eq!(rearmed, expected);
        assert_eq!(load_from(dir.path()).0, BTreeMap::from([(name, expected)]));
    }

    #[test]
    fn concurrent_signal_wake_arms_publish_one_instance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let writers = [0, 1].map(|_| {
            let root = dir.path().to_path_buf();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                arm_signal_wake_in(
                    &root,
                    &signal_wake(),
                    &BTreeSet::new(),
                    Timestamp::UNIX_EPOCH,
                )
                .expect("atomic arm")
            })
        });
        barrier.wait();
        let results = writers.map(|writer| writer.join().expect("writer"));
        assert_eq!(results[0].0, results[1].0);
        assert_ne!(results[0].2, results[1].2);
        assert_eq!(load_from(dir.path()).0.len(), 1);
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
            let (name, _, reused) =
                arm_signal_wake_in(dir.path(), &entry, &taken, Timestamp::UNIX_EPOCH)
                    .expect("distinct arm");
            assert!(!reused, "{field}");
            assert_ne!(name, "wake-old");
            assert_eq!(load_from(dir.path()).0["wake-old"], old);
            remove_from(dir.path(), &name).expect("remove fresh");
            remove_from(dir.path(), "wake-old").expect("remove old");
        }
        let (_, _, reused) = arm_signal_wake_in(dir.path(), &entry, &taken, Timestamp::UNIX_EPOCH)
            .expect("arm after retirement");
        assert!(!reused);
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
