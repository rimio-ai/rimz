//! Machine-local pause state for loop tasks.
//!
//! Pauses overlay every task source without editing its durable definition.
//! An ended pause remains as the effective last-fire edge so resumed schedules
//! do not replay occurrences missed while the elder clock was held.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::store::atomic::{AtomicErr, write_temp_then_rename_cache};
use crate::store::lock::{LockErr, WorkspaceLock};
use crate::store::paths::state_home;

const NAME: &str = "loop-pauses.json";
const LOCK_NAME: &str = "loop-pauses.lock";

#[derive(Debug, thiserror::Error)]
pub enum PauseError {
    #[error(transparent)]
    Lock(#[from] LockErr),
    #[error(transparent)]
    Write(#[from] AtomicErr),
}

type Result<T> = std::result::Result<T, PauseError>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PauseEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strikes: Option<u32>,
}

pub fn path(state_root: &Path) -> PathBuf {
    state_root.join("rimz").join(NAME)
}

fn lock_path(state_root: &Path) -> PathBuf {
    state_root.join("rimz").join(LOCK_NAME)
}

pub fn load() -> BTreeMap<String, PauseEntry> {
    load_from(&state_home())
}

pub fn set(name: &str, entry: PauseEntry) -> Result<()> {
    set_in(&state_home(), name, entry)
}

pub fn set_if_inactive(name: &str, entry: PauseEntry, now: Timestamp) -> Result<bool> {
    set_if_inactive_in(&state_home(), name, entry, now)
}

pub fn remove(name: &str) -> Result<bool> {
    remove_from(&state_home(), name)
}

pub fn rename(old: &str, new: &str) -> Result<bool> {
    rename_in(&state_home(), old, new)
}

pub fn prune_orphans(known: &BTreeSet<String>) -> Result<usize> {
    prune_orphans_in(&state_home(), known)
}

pub fn is_active(entry: &PauseEntry, now: Timestamp) -> bool {
    entry.until.is_none_or(|until| until > now)
}

pub fn effective_last_fire(
    stamp: Timestamp,
    pause: Option<&PauseEntry>,
    now: Timestamp,
) -> Timestamp {
    pause
        .and_then(|entry| entry.until)
        .filter(|until| *until <= now)
        .map_or(stamp, |until| stamp.max(until))
}

fn load_from(state_root: &Path) -> BTreeMap<String, PauseEntry> {
    let Ok(bytes) = std::fs::read(path(state_root)) else {
        return BTreeMap::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn set_in(state_root: &Path, name: &str, entry: PauseEntry) -> Result<()> {
    let _guard = WorkspaceLock::acquire(&lock_path(state_root))?;
    let mut pauses = load_from(state_root);
    pauses.insert(name.to_owned(), entry);
    write_temp_then_rename_cache(&path(state_root), &pauses)?;
    Ok(())
}

fn set_if_inactive_in(
    state_root: &Path,
    name: &str,
    entry: PauseEntry,
    now: Timestamp,
) -> Result<bool> {
    let _guard = WorkspaceLock::acquire(&lock_path(state_root))?;
    let mut pauses = load_from(state_root);
    if pauses
        .get(name)
        .is_some_and(|current| is_active(current, now))
    {
        return Ok(false);
    }
    pauses.insert(name.to_owned(), entry);
    write_temp_then_rename_cache(&path(state_root), &pauses)?;
    Ok(true)
}

fn remove_from(state_root: &Path, name: &str) -> Result<bool> {
    let _guard = WorkspaceLock::acquire(&lock_path(state_root))?;
    let mut pauses = load_from(state_root);
    let removed = pauses.remove(name).is_some();
    if removed {
        write_temp_then_rename_cache(&path(state_root), &pauses)?;
    }
    Ok(removed)
}

fn rename_in(state_root: &Path, old: &str, new: &str) -> Result<bool> {
    let _guard = WorkspaceLock::acquire(&lock_path(state_root))?;
    let mut pauses = load_from(state_root);
    let Some(entry) = pauses.remove(old) else {
        return Ok(false);
    };
    pauses.insert(new.to_owned(), entry);
    write_temp_then_rename_cache(&path(state_root), &pauses)?;
    Ok(true)
}

fn prune_orphans_in(state_root: &Path, known: &BTreeSet<String>) -> Result<usize> {
    let _guard = WorkspaceLock::acquire(&lock_path(state_root))?;
    let mut pauses = load_from(state_root);
    let before = pauses.len();
    pauses.retain(|name, _| known.contains(name));
    let removed = before - pauses.len();
    if removed > 0 {
        write_temp_then_rename_cache(&path(state_root), &pauses)?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(second: i64) -> Timestamp {
        Timestamp::from_second(second).expect("timestamp")
    }

    #[test]
    fn missing_or_corrupt_file_loads_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(load_from(dir.path()).is_empty());

        std::fs::create_dir_all(dir.path().join("rimz")).expect("state dir");
        std::fs::write(path(dir.path()), b"not json").expect("corrupt state");
        assert!(load_from(dir.path()).is_empty());
    }

    #[test]
    fn store_edits_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = PauseEntry {
            until: Some(ts(20)),
            strikes: Some(3),
        };

        set_in(dir.path(), "nightly", entry).expect("set");
        assert_eq!(load_from(dir.path()).get("nightly"), Some(&entry));

        assert!(rename_in(dir.path(), "nightly", "weekly").expect("rename"));
        assert!(!rename_in(dir.path(), "missing", "other").expect("rename absent"));
        assert_eq!(load_from(dir.path()).get("weekly"), Some(&entry));

        assert!(remove_from(dir.path(), "weekly").expect("remove"));
        assert!(!remove_from(dir.path(), "weekly").expect("remove absent"));
        assert!(load_from(dir.path()).is_empty());
    }

    #[test]
    fn conditional_set_preserves_an_active_pause() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manual = PauseEntry {
            until: Some(ts(20)),
            strikes: None,
        };
        let automatic = PauseEntry {
            until: None,
            strikes: Some(3),
        };
        set_in(dir.path(), "nightly", manual).expect("manual pause");

        assert!(
            !set_if_inactive_in(dir.path(), "nightly", automatic, ts(10)).expect("active pause")
        );
        assert_eq!(load_from(dir.path()).get("nightly"), Some(&manual));
        assert!(set_if_inactive_in(dir.path(), "nightly", automatic, ts(20)).expect("ended pause"));
        assert_eq!(load_from(dir.path()).get("nightly"), Some(&automatic));
    }

    #[test]
    fn prune_keeps_only_known_tasks() {
        let dir = tempfile::tempdir().expect("tempdir");
        set_in(dir.path(), "keep", PauseEntry::default()).expect("set keep");
        set_in(dir.path(), "gone", PauseEntry::default()).expect("set gone");

        let removed = prune_orphans_in(
            dir.path(),
            &BTreeSet::from(["keep".to_owned(), "other".to_owned()]),
        )
        .expect("prune");

        assert_eq!(removed, 1);
        assert_eq!(
            load_from(dir.path()).keys().collect::<Vec<_>>(),
            vec!["keep"]
        );
    }

    #[test]
    fn active_pause_has_no_end_at_or_before_now() {
        assert!(is_active(&PauseEntry::default(), ts(10)));
        assert!(is_active(
            &PauseEntry {
                until: Some(ts(11)),
                strikes: None,
            },
            ts(10)
        ));
        assert!(!is_active(
            &PauseEntry {
                until: Some(ts(10)),
                strikes: None,
            },
            ts(10)
        ));
        assert!(!is_active(
            &PauseEntry {
                until: Some(ts(9)),
                strikes: None,
            },
            ts(10)
        ));
    }

    #[test]
    fn ended_pause_advances_the_effective_stamp() {
        let ended = PauseEntry {
            until: Some(ts(20)),
            strikes: None,
        };
        let active = PauseEntry {
            until: Some(ts(40)),
            strikes: None,
        };

        assert_eq!(effective_last_fire(ts(10), Some(&ended), ts(30)), ts(20));
        assert_eq!(effective_last_fire(ts(25), Some(&ended), ts(30)), ts(25));
        assert_eq!(effective_last_fire(ts(10), Some(&active), ts(30)), ts(10));
        assert_eq!(effective_last_fire(ts(10), None, ts(30)), ts(10));
    }
}
