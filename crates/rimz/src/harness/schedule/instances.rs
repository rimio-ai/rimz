//! RimZ-owned loop task instances and merged loop task reads.
//!
//! Durable recurring definitions live in `loop.toml`. Session deliveries,
//! one-shots, and poll-until instances live here as workspace state, using
//! the same task entry shape without turning runtime churn into user config
//! edits. In the catalog, durable config wins only for the same name and root.

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
    Room(#[from] crate::workspace::WorkspaceErr),
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
    #[error(transparent)]
    TaskRecord(#[from] super::task_record::TaskRecordErr),
    #[error("clearing the instance's task record: {0}")]
    Retire(#[from] super::task_record_store::TaskStoreErr),
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

/// Seed a room's instance row for a test outside the scheduler.
#[cfg(test)]
pub(crate) fn seed(paths: &StatePaths, name: &str, entry: &TaskEntry) {
    insert(paths, name, entry).expect("seed the room's instance row");
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
    if crate::workspace::record::read(&paths.workspace_record).is_err() {
        let (result, changed) = edit(&mut BTreeMap::new())?;
        if !changed {
            return Ok(result);
        }
        return Err(crate::workspace::WorkspaceErr::NoRoom {
            location: format!("for workspace {}", paths.workspace_id),
        }
        .into());
    }
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
    if crate::workspace::record::read(&paths.workspace_record).is_err() {
        return Err(crate::workspace::WorkspaceErr::NoRoom {
            location: format!("for workspace {}", paths.workspace_id),
        }
        .into());
    }
    let _guard = WorkspaceLock::acquire(&paths.lock_path("loop-instances.lock"))?;
    let mut tasks = load_strict_from(&paths.root)?.0;
    let arming = super::arming::load()?;
    let now = Timestamp::now();
    // Rows place an older target under the launch its session belongs to; a
    // room whose rows cannot be read compares targets as written.
    let audit = std::cell::OnceCell::new();
    let rows = || {
        audit.get_or_init(|| {
            crate::store::runtime::audit_rows(paths)
                .inspect_err(|error| tracing::debug!(%error, "failed to read rows for wait dedupe"))
                .ok()
        })
    };
    if (entry.signal.is_some() || entry.when.is_some())
        && let Some((name, _)) = tasks.iter().find(|(name, current)| {
            let key = super::task_record::Binding::for_task(
                name,
                super::catalog::TaskSource::Instance,
                &current.resolved_root(),
            )
            .key();
            // A self wait and a delivery subscription never stand in for each other.
            !taken.contains(*name)
                && current.wait_meta.is_some() == entry.wait_meta.is_some()
                && super::arming::ArmState::resolve(
                    arming.get(&key),
                    super::catalog::TaskSource::Instance,
                    now,
                ) == super::arming::ArmState::Live
                && current
                    .wait
                    .as_ref()
                    .zip(entry.wait.as_ref())
                    .is_some_and(|(a, b)| {
                        let rows = rows()
                            .iter()
                            .flat_map(|rows| rows.iter())
                            .collect::<Vec<_>>();
                        super::pending::same_target(&rows, a, b)
                    })
                && current
                    .signal
                    .as_deref()
                    .and_then(|raw| raw.parse::<crate::harness::signal::SignalSelector>().ok())
                    == entry.signal.as_deref().and_then(|raw| raw.parse().ok())
                && current
                    .matches
                    .iter()
                    .flatten()
                    .eq(entry.matches.iter().flatten())
                && current.resolved_root() == entry.resolved_root()
                && current.when == entry.when
                && current.hold == entry.hold
                && (entry.loop_task.is_none() || current.loop_task == entry.loop_task)
                && (entry.when.is_none() || current.run_dir() == entry.run_dir())
        })
    {
        return Ok((name.clone(), true));
    }
    let mut name = name.map(ToOwned::to_owned).unwrap_or_else(|| {
        let petname = crate::agents::petname::mint(|name| {
            let name = format!("wait-{name}");
            tasks.contains_key(&name) || taken.contains(&name)
        });
        format!("wait-{petname}")
    });
    if entry.loop_task.is_some() {
        let prefix = name
            .rsplit_once('-')
            .map_or(name.as_str(), |(prefix, _)| prefix)
            .to_owned();
        let mut ordinal = 1;
        while taken.contains(&name)
            || tasks.get(&name).is_some_and(|current| {
                current.loop_task != entry.loop_task
                    || current.wait != entry.wait
                    || current.signal != entry.signal
                    || current.matches != entry.matches
            })
        {
            name = format!("{prefix}-{ordinal}");
            ordinal += 1;
        }
    }
    super::catalog::retire(
        &super::task_record::Binding::for_task(
            &name,
            super::catalog::TaskSource::Instance,
            &entry.resolved_root(),
        ),
        true,
    )?;
    tasks.insert(name.clone(), entry.clone());
    write_temp_then_rename(&path(&paths.root), &tasks)?;
    Ok((name, false))
}

pub(super) fn retire_session(
    paths: &StatePaths,
    kind: &crate::ids::AgentKind,
    session: &crate::ids::AgentSessionId,
) -> Result<Vec<(String, TaskEntry)>> {
    mutate(paths, |tasks| {
        let names = tasks
            .iter()
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
    use super::*;

    fn record_room(paths: &StatePaths, root: &Path) {
        let workspace = crate::workspace::WorkspaceResolver::resolve(root, None).unwrap();
        crate::workspace::record::write(
            paths,
            &crate::workspace::record::WorkspaceRecord::from_resolved(&workspace),
        )
        .unwrap();
    }

    fn task() -> TaskEntry {
        TaskEntry {
            args: vec!["true".to_owned()],
            timeout: Some("1m".to_owned()),
            root: PathBuf::from("/repo"),
            at: Some("07:00".to_owned()),
            ..TaskEntry::default()
        }
    }

    #[test]
    fn recordless_instance_removals_create_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = StatePaths::for_project_root_under(dir.path(), dir.path()).unwrap();
        assert!(!remove(&paths, "missing", None).unwrap());
        assert!(!rename(&paths, "missing", "renamed").unwrap());
        assert!(
            retire_session(
                &paths,
                &crate::ids::AgentKind::new_unchecked("claude"),
                &"missing".into(),
            )
            .unwrap()
            .is_empty()
        );
        assert!(!paths.root.exists());
    }

    #[test]
    fn recordless_instance_insert_refuses_without_creating_state() {
        let dir = tempfile::tempdir().unwrap();
        let paths = StatePaths::for_project_root_under(dir.path(), dir.path()).unwrap();
        assert!(insert(&paths, "probe", &task()).is_err());
        assert!(!paths.root.exists());
    }

    #[test]
    fn recordless_delivery_insert_refuses_without_creating_state() {
        let dir = tempfile::tempdir().unwrap();
        let paths = StatePaths::for_project_root_under(dir.path(), dir.path()).unwrap();
        assert!(insert_delivery(&paths, Some("probe"), &task(), &BTreeSet::new()).is_err());
        assert!(!paths.root.exists());
    }

    #[test]
    fn failed_record_clear_writes_no_delivery() {
        if std::env::var_os("RIMZ_TEST_DELIVERY_OVERLAY_HOME").is_none() {
            let home = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "harness::schedule::instances::tests::failed_record_clear_writes_no_delivery",
                    "--nocapture",
                ])
                .env("RIMZ_UNIT_TEST_RIMZ_HOME", home.path())
                .env("RIMZ_TEST_DELIVERY_OVERLAY_HOME", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let paths = StatePaths::for_project_root(dir.path()).unwrap();
        record_room(&paths, dir.path());
        // A record of an earlier `probe` the insert must delete, behind a lock it cannot take.
        let binding = super::super::task_record::Binding::for_task(
            "probe",
            super::super::catalog::TaskSource::Instance,
            &task().resolved_root(),
        );
        super::super::task_record_store::mutate(|records| {
            records.bind(&binding);
        })
        .unwrap();
        let lock = crate::disk::paths::loops_dir().join("tasks.lock");
        std::fs::remove_file(&lock).unwrap();
        std::fs::create_dir_all(&lock).unwrap();
        let result = insert_delivery(&paths, Some("probe"), &task(), &BTreeSet::new());
        std::fs::remove_dir(&lock).unwrap();
        assert!(matches!(result, Err(InstanceErr::Retire(_))), "{result:?}");
        assert!(load_strict_from(&paths.root).unwrap().0.is_empty());
        assert!(!path(&paths.root).exists());
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
        record_room(&paths, dir.path());
        let entry = task();

        insert(&paths, "wait", &entry).expect("insert");
        let encoded = std::fs::read_to_string(path(&paths.root)).expect("serialized instances");
        let value: serde_json::Value = serde_json::from_str(&encoded).expect("instances json");
        assert_eq!(value["wait"]["args"], serde_json::json!(["true"]));
        assert_eq!(value["wait"]["timeout"], "1m");
        assert_eq!(value["wait"]["root"], "/repo");
        assert_eq!(value["wait"]["at"], "07:00");
        assert_eq!(load_from(&paths.root).0.get("wait"), Some(&entry));

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
        record_room(&paths, dir.path());
        let entry = task();

        insert(&paths, "wait", &entry).expect("insert");

        assert!(rename(&paths, "wait", "nudge").expect("rename"));
        let tasks = load_from(&paths.root);
        assert!(!tasks.0.contains_key("wait"));
        assert_eq!(tasks.0.get("nudge"), Some(&entry));
        assert!(!rename(&paths, "wait", "later").expect("rename absent"));
    }

    #[test]
    fn resident_name_collision_preserves_both_subscribers() {
        let dir = tempfile::tempdir().unwrap();
        let paths = StatePaths::for_project_root(dir.path()).unwrap();
        record_room(&paths, dir.path());
        let subscriber = |task: &str, session: &str| TaskEntry {
            root: dir.path().to_owned(),
            loop_task: Some(task.into()),
            signal: Some("ci.failed".into()),
            wait: Some(crate::config::TaskTarget {
                kind: crate::ids::AgentKind::new_unchecked("claude"),
                session: session.into(),
                handle: session.into(),
            }),
            ..Default::default()
        };
        let first = subscriber("repair-a", "otter");
        let second = subscriber("repair", "a-otter");
        let name = "loop-repair-a-otter-1";
        let (a, _) = insert_delivery(&paths, Some(name), &first, &BTreeSet::new()).unwrap();
        let (b, _) = insert_delivery(&paths, Some(name), &second, &BTreeSet::new()).unwrap();
        assert_ne!(a, b);
        assert_eq!(load_from(&paths.root).0.len(), 2);
        for (entry, expected) in [(&first, &a), (&second, &b)] {
            let (actual, duplicate) =
                insert_delivery(&paths, Some(name), entry, &BTreeSet::new()).unwrap();
            assert_eq!(&actual, expected);
            assert!(duplicate);
        }
    }

    /// A subscription stored before targets held launch ids names the agent's
    /// first session: re-adding it for the agent's launch after a swap is
    /// already subscribed, while two kinds sharing a session string are two
    /// agents.
    #[test]
    fn subscription_dedupe_reads_an_older_session_as_its_launch_and_keeps_kinds_apart() {
        let dir = tempfile::tempdir().unwrap();
        let (paths, _store) = crate::harness::schedule::tests::swapped_store(dir.path());

        let subscriber = |kind: &str, session: &str| TaskEntry {
            root: dir.path().to_owned(),
            signal: Some("ci.failed".into()),
            wait: Some(crate::config::TaskTarget {
                kind: crate::ids::AgentKind::new_unchecked(kind),
                session: session.into(),
                handle: "@coder".into(),
            }),
            ..Default::default()
        };
        insert(&paths, "older", &subscriber("claude", "s1")).unwrap();
        let readd = insert_delivery(
            &paths,
            None,
            &subscriber("claude", "launch_a"),
            &BTreeSet::new(),
        );
        assert_eq!(readd.unwrap(), ("older".to_owned(), true));
        for plugin in ["plugin-a", "plugin-b"] {
            let (_, duplicate) = insert_delivery(
                &paths,
                None,
                &subscriber(plugin, "shared"),
                &BTreeSet::new(),
            )
            .unwrap();
            assert!(!duplicate, "{plugin}");
        }
        assert_eq!(load_from(&paths.root).0.len(), 3);
    }

    /// A retirement takes every row pinned to the session, declared or
    /// self-armed, and nothing pinned elsewhere.
    #[test]
    fn retirement_takes_every_row_of_the_session_and_nothing_else() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = StatePaths::under(
            crate::ids::WorkspaceId::from_project_root(dir.path()),
            dir.path(),
        )
        .unwrap();
        record_room(&paths, dir.path());
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

        let retired = retire_session(&paths, &kind, &session).expect("retire");

        assert_eq!(
            retired
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["declared", "self-armed"]
        );
        assert_eq!(
            load_from(&paths.root)
                .0
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["other-session", "unpinned"]
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
        record_room(&paths, dir.path());
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
