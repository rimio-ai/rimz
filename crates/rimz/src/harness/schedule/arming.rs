//! Machine-local arming state for loop tasks.
//!
//! Arming overlays every task source without editing its durable definition.
//! Project tasks default disabled until this machine records an explicit
//! enable. An ended pause and a fresh enable remain effective last-fire edges
//! so schedules do not replay occurrences missed while held.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::catalog::TaskSource;
use super::overlay_store::{OverlayError, OverlayStore};
use crate::ids::WorkspaceId;
use crate::store::paths::state_home;

const STORE: OverlayStore = OverlayStore::new("loop-arming.json", "loop-arming.lock");
const LEGACY_PAUSE_STORE: OverlayStore = OverlayStore::new("loop-pauses.json", "loop-pauses.lock");
const MACHINE_SCOPE: &str = "machine::";

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct ArmingError(ArmingErrorKind);

#[derive(Debug, thiserror::Error)]
enum ArmingErrorKind {
    #[error(transparent)]
    Overlay(OverlayError),
    #[error(transparent)]
    Io(std::io::Error),
}

impl From<OverlayError> for ArmingError {
    fn from(value: OverlayError) -> Self {
        Self(ArmingErrorKind::Overlay(value))
    }
}

impl From<std::io::Error> for ArmingError {
    fn from(value: std::io::Error) -> Self {
        Self(ArmingErrorKind::Io(value))
    }
}

type Result<T> = std::result::Result<T, ArmingError>;

/// Machine-local arming record for one task.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Arming {
    pub enabled: bool,
    /// When this enable or disable took effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_until: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strikes: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArmState {
    Live,
    Disabled(DisabledReason),
    Paused(Timestamp),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisabledReason {
    /// A project task with no local record: the default-off case.
    NotEnabledHere,
    Manual,
    Strikes(u32),
}

/// Scoped overlay key construction for task definitions.
pub struct TaskKey;

impl TaskKey {
    pub fn for_task(name: &str, source: TaskSource, root: &Path) -> String {
        format!("{}{name}", Self::scope(source, root))
    }

    pub fn known_scopes(project_root: Option<&Path>) -> BTreeSet<String> {
        let mut scopes = BTreeSet::from([MACHINE_SCOPE.to_owned()]);
        if let Some(root) = project_root {
            scopes.insert(Self::project_scope(root));
        }
        scopes
    }

    fn scope(source: TaskSource, root: &Path) -> String {
        match source {
            TaskSource::Config | TaskSource::Instance => MACHINE_SCOPE.to_owned(),
            TaskSource::Project { .. } => Self::project_scope(root),
        }
    }

    fn project_scope(root: &Path) -> String {
        format!("{}::", WorkspaceId::from_project_root(root))
    }
}

impl ArmState {
    pub fn resolve(record: Option<&Arming>, source: TaskSource, now: Timestamp) -> Self {
        let Some(record) = record else {
            return if matches!(source, TaskSource::Project { .. }) {
                Self::Disabled(DisabledReason::NotEnabledHere)
            } else {
                Self::Live
            };
        };
        if !record.enabled {
            return Self::Disabled(
                record
                    .strikes
                    .map_or(DisabledReason::Manual, DisabledReason::Strikes),
            );
        }
        if let Some(until) = record.pause_until.filter(|until| *until > now) {
            return Self::Paused(until);
        }
        Self::Live
    }
}

pub fn path(state_root: &Path) -> PathBuf {
    STORE.path(state_root)
}

pub fn load() -> BTreeMap<String, Arming> {
    load_from(&state_home())
}

pub fn enable(key: &str) -> Result<Arming> {
    enable_in(&state_home(), key, Timestamp::now())
}

pub fn disable(key: &str, strikes: Option<u32>) -> Result<()> {
    disable_in(&state_home(), key, strikes, Timestamp::now())
}

/// Pause a task without changing its effective enablement.
///
/// A missing record inherits its source default before the pause is written.
pub fn pause(key: &str, source: TaskSource, until: Timestamp) -> Result<()> {
    pause_in(&state_home(), key, source, until)
}

pub(super) fn disable_if_live(
    key: &str,
    source: TaskSource,
    strikes: Option<u32>,
    now: Timestamp,
) -> Result<bool> {
    disable_if_live_in(&state_home(), key, source, strikes, now)
}

pub(super) fn remove(key: &str) -> Result<bool> {
    remove_from(&state_home(), key)
}

pub(super) fn rename(old: &str, new: &str) -> Result<bool> {
    rename_in(&state_home(), old, new)
}

pub(super) fn prune_orphans(known: &BTreeSet<String>, scopes: &BTreeSet<String>) -> Result<usize> {
    prune_orphans_in(&state_home(), known, scopes)
}

pub(super) fn remove_legacy_pauses() -> Result<usize> {
    remove_legacy_pauses_in(&state_home())
}

pub(super) fn effective_last_fire(
    stamp: Timestamp,
    record: Option<&Arming>,
    now: Timestamp,
) -> Timestamp {
    let Some(record) = record else {
        return stamp;
    };
    let enabled_at = record.enabled.then_some(record.at).flatten();
    let ended_pause = record.pause_until.filter(|until| *until <= now);
    enabled_at
        .into_iter()
        .chain(ended_pause)
        .fold(stamp, Timestamp::max)
}

fn load_from(state_root: &Path) -> BTreeMap<String, Arming> {
    STORE.load(state_root)
}

fn enable_in(state_root: &Path, key: &str, now: Timestamp) -> Result<Arming> {
    let entry = Arming {
        enabled: true,
        at: Some(now),
        pause_until: None,
        strikes: None,
    };
    set_in(state_root, key, entry)?;
    Ok(entry)
}

fn disable_in(state_root: &Path, key: &str, strikes: Option<u32>, now: Timestamp) -> Result<()> {
    set_in(
        state_root,
        key,
        Arming {
            enabled: false,
            at: Some(now),
            pause_until: None,
            strikes,
        },
    )
}

fn pause_in(state_root: &Path, key: &str, source: TaskSource, until: Timestamp) -> Result<()> {
    STORE
        .mutate(state_root, |entries: &mut BTreeMap<String, Arming>| {
            let entry = entries.entry(key.to_owned()).or_insert(Arming {
                enabled: !matches!(source, TaskSource::Project { .. }),
                at: None,
                pause_until: None,
                strikes: None,
            });
            let changed = entry.pause_until != Some(until);
            entry.pause_until = Some(until);
            ((), changed)
        })
        .map_err(Into::into)
}

fn disable_if_live_in(
    state_root: &Path,
    key: &str,
    source: TaskSource,
    strikes: Option<u32>,
    now: Timestamp,
) -> Result<bool> {
    STORE
        .mutate(state_root, |entries: &mut BTreeMap<String, Arming>| {
            if entries.get(key).is_some_and(|current| {
                !current.enabled || current.pause_until.is_some_and(|until| until > now)
            }) {
                return (false, false);
            }
            if entries.get(key).is_none() && matches!(source, TaskSource::Project { .. }) {
                return (false, false);
            }
            let entry = Arming {
                enabled: false,
                at: Some(now),
                pause_until: None,
                strikes,
            };
            let changed = entries.get(key) != Some(&entry);
            entries.insert(key.to_owned(), entry);
            (true, changed)
        })
        .map_err(Into::into)
}

fn remove_legacy_pauses_in(state_root: &Path) -> Result<usize> {
    let mut removed = 0;
    for path in [
        LEGACY_PAUSE_STORE.path(state_root),
        LEGACY_PAUSE_STORE.lock_path(state_root),
    ] {
        match std::fs::remove_file(path) {
            Ok(()) => removed += 1,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
    }
    Ok(removed)
}

fn set_in(state_root: &Path, key: &str, entry: Arming) -> Result<()> {
    STORE
        .mutate(state_root, |entries| {
            let changed = entries.get(key) != Some(&entry);
            entries.insert(key.to_owned(), entry);
            ((), changed)
        })
        .map_err(Into::into)
}

fn remove_from(state_root: &Path, key: &str) -> Result<bool> {
    STORE.remove::<Arming>(state_root, key).map_err(Into::into)
}

fn rename_in(state_root: &Path, old: &str, new: &str) -> Result<bool> {
    STORE
        .rename::<Arming>(state_root, old, new)
        .map_err(Into::into)
}

fn prune_orphans_in(
    state_root: &Path,
    known: &BTreeSet<String>,
    scopes: &BTreeSet<String>,
) -> Result<usize> {
    STORE
        .prune_orphans_in_scopes::<Arming>(state_root, known, scopes)
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust::TrustState;

    fn ts(second: i64) -> Timestamp {
        Timestamp::from_second(second).expect("timestamp")
    }

    fn project() -> TaskSource {
        TaskSource::Project {
            state: TrustState::Trusted,
        }
    }

    fn record(enabled: bool) -> Arming {
        Arming {
            enabled,
            at: Some(ts(10)),
            pause_until: None,
            strikes: None,
        }
    }

    #[test]
    fn source_defaults_and_explicit_states_resolve() {
        assert_eq!(
            ArmState::resolve(None, TaskSource::Config, ts(20)),
            ArmState::Live
        );
        assert_eq!(
            ArmState::resolve(None, TaskSource::Instance, ts(20)),
            ArmState::Live
        );
        assert_eq!(
            ArmState::resolve(None, project(), ts(20)),
            ArmState::Disabled(DisabledReason::NotEnabledHere)
        );
        assert_eq!(
            ArmState::resolve(Some(&record(false)), TaskSource::Config, ts(20)),
            ArmState::Disabled(DisabledReason::Manual)
        );
        assert_eq!(
            ArmState::resolve(
                Some(&Arming {
                    strikes: Some(3),
                    ..record(false)
                }),
                TaskSource::Config,
                ts(20)
            ),
            ArmState::Disabled(DisabledReason::Strikes(3))
        );
    }

    #[test]
    fn disabled_precedes_pause_and_pause_expires() {
        let disabled = Arming {
            pause_until: Some(ts(30)),
            ..record(false)
        };
        let paused = Arming {
            pause_until: Some(ts(30)),
            ..record(true)
        };
        assert_eq!(
            ArmState::resolve(Some(&disabled), TaskSource::Config, ts(20)),
            ArmState::Disabled(DisabledReason::Manual)
        );
        assert_eq!(
            ArmState::resolve(Some(&paused), TaskSource::Config, ts(20)),
            ArmState::Paused(ts(30))
        );
        assert_eq!(
            ArmState::resolve(Some(&paused), TaskSource::Config, ts(30)),
            ArmState::Live
        );
    }

    #[test]
    fn pause_preserves_each_source_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        pause_in(dir.path(), "machine::nightly", TaskSource::Config, ts(30))
            .expect("machine pause");
        pause_in(
            dir.path(),
            "ws_0123456789abcdef01234567::nightly",
            project(),
            ts(30),
        )
        .expect("project pause");
        let entries = load_from(dir.path());
        assert!(entries["machine::nightly"].enabled);
        assert_eq!(entries["machine::nightly"].at, None);
        assert!(!entries["ws_0123456789abcdef01234567::nightly"].enabled);
    }

    #[test]
    fn effective_stamp_uses_only_live_enable_and_ended_pause_edges() {
        let enabled = Arming {
            at: Some(ts(20)),
            pause_until: Some(ts(30)),
            ..record(true)
        };
        let disabled = Arming {
            at: Some(ts(40)),
            pause_until: None,
            ..record(false)
        };
        assert_eq!(effective_last_fire(ts(10), Some(&enabled), ts(25)), ts(20));
        assert_eq!(effective_last_fire(ts(10), Some(&enabled), ts(35)), ts(30));
        assert_eq!(effective_last_fire(ts(32), Some(&enabled), ts(35)), ts(32));
        assert_eq!(effective_last_fire(ts(10), Some(&disabled), ts(50)), ts(10));
        assert_eq!(effective_last_fire(ts(10), None, ts(50)), ts(10));
    }

    #[test]
    fn automatic_disable_preserves_an_active_pause() {
        let dir = tempfile::tempdir().expect("tempdir");
        pause_in(dir.path(), "machine::nightly", TaskSource::Config, ts(30)).expect("pause");
        assert!(
            !disable_if_live_in(
                dir.path(),
                "machine::nightly",
                TaskSource::Config,
                Some(3),
                ts(20)
            )
            .expect("active pause")
        );
        assert!(
            disable_if_live_in(
                dir.path(),
                "machine::nightly",
                TaskSource::Config,
                Some(3),
                ts(30)
            )
            .expect("ended pause")
        );
        assert_eq!(
            ArmState::resolve(
                load_from(dir.path()).get("machine::nightly"),
                TaskSource::Config,
                ts(30)
            ),
            ArmState::Disabled(DisabledReason::Strikes(3))
        );
    }

    #[test]
    fn automatic_disable_respects_missing_record_source_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(
            !disable_if_live_in(
                dir.path(),
                "ws_0123456789abcdef01234567::nightly",
                project(),
                Some(3),
                ts(20),
            )
            .expect("project default")
        );
        assert!(
            disable_if_live_in(
                dir.path(),
                "machine::nightly",
                TaskSource::Config,
                Some(3),
                ts(20),
            )
            .expect("machine default")
        );
    }

    #[test]
    fn legacy_pause_cleanup_removes_data_and_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data = LEGACY_PAUSE_STORE.path(dir.path());
        let lock = LEGACY_PAUSE_STORE.lock_path(dir.path());
        std::fs::create_dir_all(data.parent().expect("legacy parent")).expect("state dir");
        std::fs::write(&data, "{}").expect("legacy data");
        std::fs::write(&lock, "").expect("legacy lock");

        assert_eq!(remove_legacy_pauses_in(dir.path()).expect("cleanup"), 2);
        assert!(!data.exists());
        assert!(!lock.exists());
        assert_eq!(remove_legacy_pauses_in(dir.path()).expect("idempotent"), 0);
    }
}
