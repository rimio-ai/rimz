//! Machine-local pause state for loop tasks.
//!
//! Pauses overlay every task source without editing its durable definition.
//! An ended pause remains as the effective last-fire edge so resumed schedules
//! do not replay occurrences missed while the elder clock was held.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::overlay_store::{OverlayError, OverlayStore};
use crate::store::paths::state_home;

const STORE: OverlayStore = OverlayStore::new("loop-pauses.json", "loop-pauses.lock");

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct PauseError(#[from] OverlayError);

type Result<T> = std::result::Result<T, PauseError>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PauseEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strikes: Option<u32>,
}

pub fn path(state_root: &Path) -> PathBuf {
    STORE.path(state_root)
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
    STORE.load(state_root)
}

fn set_in(state_root: &Path, name: &str, entry: PauseEntry) -> Result<()> {
    STORE
        .mutate(state_root, |pauses| {
            let changed = pauses.get(name) != Some(&entry);
            pauses.insert(name.to_owned(), entry);
            ((), changed)
        })
        .map_err(Into::into)
}

fn set_if_inactive_in(
    state_root: &Path,
    name: &str,
    entry: PauseEntry,
    now: Timestamp,
) -> Result<bool> {
    STORE
        .mutate(state_root, |pauses| {
            if pauses
                .get(name)
                .is_some_and(|current| is_active(current, now))
            {
                return (false, false);
            }
            let changed = pauses.get(name) != Some(&entry);
            pauses.insert(name.to_owned(), entry);
            (true, changed)
        })
        .map_err(Into::into)
}

fn remove_from(state_root: &Path, name: &str) -> Result<bool> {
    STORE
        .remove::<PauseEntry>(state_root, name)
        .map_err(Into::into)
}

fn rename_in(state_root: &Path, old: &str, new: &str) -> Result<bool> {
    STORE
        .rename::<PauseEntry>(state_root, old, new)
        .map_err(Into::into)
}

fn prune_orphans_in(state_root: &Path, known: &BTreeSet<String>) -> Result<usize> {
    STORE
        .prune_orphans::<PauseEntry>(state_root, known)
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(second: i64) -> Timestamp {
        Timestamp::from_second(second).expect("timestamp")
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
