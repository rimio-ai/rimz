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
use crate::disk::paths::StatePaths;
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
    #[error("clearing instance arming state: {0}")]
    Arming(#[from] super::arming::ArmingError),
    #[error("clearing instance strike state: {0}")]
    Strikes(#[from] super::strikes::StrikesError),
}

type Result<T> = std::result::Result<T, InstanceErr>;

pub(super) fn path(state_root: &Path) -> PathBuf {
    crate::StatePaths::class_path(
        state_root,
        crate::disk::paths::Class::Records,
        "loop-instances.json",
    )
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

pub(super) fn insert(paths: &StatePaths, name: &str, entry: &TaskEntry) -> Result<()> {
    mutate(paths, |tasks| {
        tasks.insert(name.to_owned(), entry.clone());
        Ok(((), true))
    })
}

pub(super) fn remove(paths: &StatePaths, name: &str, expected: Option<&TaskEntry>) -> Result<bool> {
    mutate(paths, |tasks| {
        if expected.is_some_and(|entry| tasks.get(name) != Some(entry)) {
            return Ok((false, false));
        }
        let removed = tasks.remove(name).is_some();
        Ok((removed, removed))
    })
}

pub(super) fn rename(paths: &StatePaths, old: &str, new: &str) -> Result<bool> {
    mutate(paths, |tasks| {
        let Some(entry) = tasks.remove(old) else {
            return Ok((false, false));
        };
        tasks.insert(new.to_owned(), entry);
        Ok((true, true))
    })
}

fn mutate<T>(
    paths: &StatePaths,
    edit: impl FnOnce(&mut BTreeMap<String, TaskEntry>) -> Result<(T, bool)>,
) -> Result<T> {
    let _guard = WorkspaceLock::acquire(&paths.lock_path("loop-instances.lock"))?;
    let mut entries = load_strict_from(&paths.root)?.0;
    let (result, changed) = edit(&mut entries)?;
    if changed {
        write_temp_then_rename(&path(&paths.root), &entries)?;
    }
    Ok(result)
}

pub(super) fn insert_delivery(
    paths: &StatePaths,
    name: Option<&str>,
    entry: &TaskEntry,
    taken: &BTreeSet<String>,
) -> Result<(String, bool)> {
    let _guard = WorkspaceLock::acquire(&paths.lock_path("loop-instances.lock"))?;
    let mut tasks = load_strict_from(&paths.root)?.0;
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
    write_temp_then_rename(&path(&paths.root), &tasks)?;
    super::arming::remove(&key)?;
    super::strikes::clear(&key)?;
    Ok((name, false))
}

pub(super) fn retire_session(
    paths: &StatePaths,
    kind: &crate::ids::AgentKind,
    session: &crate::ids::AgentSessionId,
    scope: super::arm::RetireScope,
) -> Result<Vec<(String, TaskEntry)>> {
    mutate(paths, |tasks| {
        let names = tasks
            .iter()
            .filter(|(_, entry)| entry.team.is_none() || scope == super::arm::RetireScope::Session)
            .filter(|(_, entry)| {
                entry.wait.as_ref().is_some_and(|target| {
                    target.kind == kind.as_str() && target.session == session.as_str()
                })
            })
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        let retired = names
            .into_iter()
            .filter_map(|name| tasks.remove(&name).map(|entry| (name, entry)))
            .collect::<Vec<_>>();
        let changed = !retired.is_empty();
        Ok((retired, changed))
    })
}

/// Every distinct session an instance row in this workspace is pinned to.
pub(super) fn pinned_sessions(
    state_root: &Path,
) -> BTreeSet<(crate::ids::AgentKind, crate::ids::AgentSessionId)> {
    load_from(state_root)
        .0
        .into_values()
        .filter_map(|entry| entry.wait)
        .map(|target| (target.kind, target.session))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::arm::RetireScope;
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
        std::fs::create_dir_all(path(dir.path()).parent().unwrap()).unwrap();
        std::fs::write(path(dir.path()), b"not json").expect("corrupt state");
        assert!(load_from(dir.path()).0.is_empty());
    }

    #[test]
    fn insert_and_remove_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = StatePaths::under(
            crate::ids::WorkspaceId::from_project_root(dir.path()),
            dir.path(),
        )
        .unwrap();
        let entry = task();

        insert(&paths, "wait", &entry).expect("insert");
        let encoded = std::fs::read_to_string(path(&paths.root)).expect("serialized instances");
        let value: serde_json::Value = serde_json::from_str(&encoded).expect("instances json");
        assert_eq!(value["wait"]["agent"], "claude");
        assert_eq!(value["wait"]["prompt"], "wait");
        assert_eq!(value["wait"]["root"], "/repo");
        assert_eq!(value["wait"]["at"], "07:00");
        assert_eq!(
            load_from(&paths.root)
                .0
                .get("wait")
                .map(|entry| entry.prompt.as_deref()),
            Some(Some("wait"))
        );

        assert!(remove(&paths, "wait", None).expect("remove"));
        assert!(load_from(&paths.root).0.is_empty());
        assert!(!remove(&paths, "wait", None).expect("remove absent"));
    }

    #[test]
    fn rename_moves_existing_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = StatePaths::under(
            crate::ids::WorkspaceId::from_project_root(dir.path()),
            dir.path(),
        )
        .unwrap();
        let entry = task();

        insert(&paths, "wait", &entry).expect("insert");

        assert!(rename(&paths, "wait", "nudge").expect("rename"));
        let tasks = load_from(&paths.root);
        assert!(!tasks.0.contains_key("wait"));
        assert_eq!(
            tasks.0.get("nudge").map(|entry| entry.prompt.as_deref()),
            Some(Some("wait"))
        );
        assert!(!rename(&paths, "wait", "later").expect("rename absent"));
    }

    /// A same-session `agents restart` is a continuation, so its retirement
    /// takes only what no later registration brings back. Everything the
    /// caller reports — the dropped count included — is derived from the rows
    /// this returns.
    #[test]
    fn unrestorable_scope_keeps_the_declared_rows_of_the_same_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = StatePaths::under(
            crate::ids::WorkspaceId::from_project_root(dir.path()),
            dir.path(),
        )
        .unwrap();
        let kind = crate::ids::AgentKind::new_unchecked("claude");
        let session = crate::ids::AgentSessionId::from("resumed");
        let pinned = |session_id: &str, team: Option<&str>| TaskEntry {
            wait: Some(crate::config::TaskTarget {
                kind: crate::ids::AgentKind::new_unchecked("claude"),
                session: crate::ids::AgentSessionId::from(session_id),
                handle: "@coder".to_owned(),
            }),
            team: team.map(|team| team.parse().expect("team instance id")),
            ..task()
        };
        for (name, entry) in [
            ("declared", pinned("resumed", Some("recon#lane"))),
            ("self-armed", pinned("resumed", None)),
            ("other-session", pinned("elsewhere", None)),
            ("unpinned", task()),
        ] {
            insert(&paths, name, &entry).expect("insert");
        }

        let retired =
            retire_session(&paths, &kind, &session, RetireScope::UnrestorableOnly).expect("retire");

        assert_eq!(
            retired
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["self-armed"]
        );
        assert_eq!(
            retired
                .iter()
                .filter(|(_, entry)| entry.team.is_none())
                .count(),
            1,
            "the count the restart line reports"
        );
        assert_eq!(
            load_from(&paths.root)
                .0
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["declared", "other-session", "unpinned"]
        );

        let retired =
            retire_session(&paths, &kind, &session, RetireScope::Session).expect("retire");

        assert_eq!(
            retired
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["declared"],
            "the session scope takes the declared row a fresh identity leaves behind"
        );
    }

    #[test]
    fn concurrent_inserts_preserve_both_instances() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = StatePaths::under(
            crate::ids::WorkspaceId::from_project_root(dir.path()),
            dir.path(),
        )
        .unwrap();
        let root = paths;
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

        let tasks = load_from(&root.root);
        assert!(tasks.0.contains_key("first"));
        assert!(tasks.0.contains_key("second"));
    }
}
