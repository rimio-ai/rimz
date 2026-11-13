//! Machine-local per-task fire record: the launch proof of each firing checkout's newest
//! scheduled launch, and the summary of the task's quiet fires, whose runs leave no history row
//! and no home.
//!
//! Keyed and kept like the strike counter (`LoadedTask::key`; cleared, renamed and pruned with
//! the task's strikes, but not by `loop enable`). Reads take no lock, since the clock pass reads
//! the launch proof from the sidebar's import graph; a corrupt file reads as empty.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::overlay_store::{OverlayError, OverlayStore};
use super::run_log::{LoopRunRecord, QuietFires};
use crate::disk::paths::loops_dir;

const STORE: OverlayStore = OverlayStore::new("loop-fires.json", "loop-fires.lock");

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct TaskFires {
    /// The instant of the newest scheduled launch by firing checkout; the empty key stands for a
    /// fire whose row names no checkout.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) scheduled: BTreeMap<String, Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) quiet: Option<QuietFires>,
}

pub(super) type Result<T> = std::result::Result<T, OverlayError>;

fn checkout_key(checkout: Option<&Path>) -> String {
    checkout.map_or_else(String::new, |path| path.display().to_string())
}

pub(super) fn load() -> BTreeMap<String, TaskFires> {
    STORE.load(&loops_dir())
}

/// Whether a scheduled launch was stamped at or after `since`: by `checkout` when one is given,
/// by any checkout otherwise.
pub(super) fn scheduled_since(key: &str, checkout: Option<&Path>, since: Timestamp) -> bool {
    let fires = load();
    let Some(stamps) = fires.get(key).map(|fires| &fires.scheduled) else {
        return false;
    };
    match checkout {
        Some(_) => stamps
            .get(&checkout_key(checkout))
            .is_some_and(|at| *at >= since),
        None => stamps.values().any(|at| *at >= since),
    }
}

pub(super) fn stamp_scheduled(key: &str, checkout: Option<&Path>, at: Timestamp) -> Result<()> {
    STORE.mutate(&loops_dir(), |fires: &mut BTreeMap<String, TaskFires>| {
        let stamps = &mut fires.entry(key.to_owned()).or_default().scheduled;
        let key = checkout_key(checkout);
        let newer = stamps.get(&key).is_none_or(|stamp| *stamp < at);
        if newer {
            stamps.insert(key, at);
        }
        ((), newer)
    })
}

/// Count one more quiet fire, whose row `last` replaces the stored one unless that is newer: a
/// collection that lands late still counts.
pub(super) fn note_quiet(key: &str, last: LoopRunRecord) -> Result<()> {
    STORE.mutate(&loops_dir(), |fires: &mut BTreeMap<String, TaskFires>| {
        let quiet = &mut fires.entry(key.to_owned()).or_default().quiet;
        let (count, streak) = quiet
            .as_ref()
            .map_or((0, 0), |quiet| (quiet.count, quiet.streak));
        let last = match quiet.take() {
            Some(stored) if stored.last.at > last.at => stored.last,
            _ => last,
        };
        *quiet = Some(QuietFires {
            count: count.saturating_add(1),
            streak: streak.saturating_add(1),
            last,
        });
        ((), true)
    })
}

/// A failure ends the task's quiet streak; its count and last output stay.
pub(super) fn reset_streak(key: &str) -> Result<()> {
    if load()
        .get(key)
        .and_then(|fires| fires.quiet.as_ref())
        .is_none_or(|quiet| quiet.streak == 0)
    {
        return Ok(());
    }
    STORE.mutate(&loops_dir(), |fires: &mut BTreeMap<String, TaskFires>| {
        let Some(quiet) = fires.get_mut(key).and_then(|fires| fires.quiet.as_mut()) else {
            return ((), false);
        };
        let changed = quiet.streak != 0;
        quiet.streak = 0;
        ((), changed)
    })
}

pub(super) fn clear(key: &str) -> Result<bool> {
    STORE.remove::<TaskFires>(&loops_dir(), key)
}

pub(super) fn rename(old: &str, new: &str) -> Result<bool> {
    STORE.rename::<TaskFires>(&loops_dir(), old, new)
}

pub(super) fn prune_orphans(known: &BTreeSet<String>, scopes: &BTreeSet<String>) -> Result<usize> {
    STORE.prune_orphans_in_scopes::<TaskFires>(&loops_dir(), known, scopes)
}
