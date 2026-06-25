//! Elder-owned loop task firing.
//!
//! The elected sidebar elder keeps time for configured loop tasks while a room
//! is open. The durable state arms tasks on first sight and records each fire
//! before spawning the detached `rimz loop run <name>` helper, so a hot tick does
//! not spawn the same occurrence twice.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use jiff::{Timestamp, Zoned};

use crate::config::{MachineConfig, TaskEntry};
use crate::ids::WorkspaceId;
use crate::ledger::atomic::write_temp_then_rename_cache;
use crate::{RuntimePaths, schedule};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Arm,
    Fire,
}

pub(super) fn fire_due_tasks(runtime: &RuntimePaths, now: &Zoned) {
    let tasks = workspace_tasks(
        MachineConfig::load_lenient().agents.r#loop.tasks.0,
        &runtime.workspace_id,
    );
    let path = state_path(runtime);
    let state = read_state(&path);
    let (actions, next_state) = plan(&tasks, &state, now);
    if next_state != state
        && let Err(err) = write_temp_then_rename_cache(&path, &next_state)
    {
        tracing::warn!(
            workspace = %runtime.workspace_id,
            tags.operation = "loop_fire.write_state",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to record loop task fire state",
        );
        return;
    }
    for (name, action) in actions {
        if action == Action::Fire {
            spawn_loop_run(&name);
        }
    }
}

fn plan(
    tasks: &BTreeMap<String, TaskEntry>,
    state: &BTreeMap<String, Timestamp>,
    now: &Zoned,
) -> (Vec<(String, Action)>, BTreeMap<String, Timestamp>) {
    let mut actions = Vec::new();
    let mut next_state = BTreeMap::new();
    for (name, entry) in tasks {
        let parsed = match schedule::parse_schedule(name, entry) {
            Ok(parsed) => parsed,
            Err(err) => {
                tracing::debug!(
                    task = %name,
                    error = %err,
                    "invalid loop task skipped by elder fire"
                );
                continue;
            }
        };
        match state.get(name).copied() {
            None => {
                actions.push((name.clone(), Action::Arm));
                next_state.insert(name.clone(), now.timestamp());
            }
            Some(last_fire) if parsed.schedule.due(last_fire, now) => {
                actions.push((name.clone(), Action::Fire));
                next_state.insert(name.clone(), now.timestamp());
            }
            Some(last_fire) => {
                next_state.insert(name.clone(), last_fire);
            }
        }
    }
    (actions, next_state)
}

fn workspace_tasks(
    tasks: BTreeMap<String, TaskEntry>,
    workspace_id: &WorkspaceId,
) -> BTreeMap<String, TaskEntry> {
    tasks
        .into_iter()
        .filter(|(_, entry)| WorkspaceId::from_project_root(&entry.root) == *workspace_id)
        .collect()
}

fn read_state(path: &Path) -> BTreeMap<String, Timestamp> {
    let Ok(bytes) = std::fs::read(path) else {
        return BTreeMap::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn state_path(runtime: &RuntimePaths) -> PathBuf {
    runtime.root.join("loop-fire.json")
}

fn spawn_loop_run(name: &str) {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            tracing::warn!(
                task = name,
                tags.operation = "loop_fire.locate_exe",
                error = &err as &dyn std::error::Error,
                "sidebar: cannot locate rimz to fire loop task",
            );
            return;
        }
    };
    let mut cmd = Command::new(exe);
    cmd.args(["loop", "run", name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        task = name,
        "sidebar: firing loop task",
    );
    if let Err(err) = crate::child_process::spawn_detached_reaped(&mut cmd, "loop-run") {
        tracing::warn!(
            task = name,
            tags.operation = "loop_fire.spawn",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to spawn loop task",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn zdt(year: i16, month: i8, day: i8, hour: i8, minute: i8, second: i8) -> Zoned {
        date(year, month, day)
            .at(hour, minute, second, 0)
            .in_tz("UTC")
            .expect("zoned test time")
    }

    fn seconds_before(ts: Timestamp, seconds: i64) -> Timestamp {
        Timestamp::from_second(ts.as_second() - seconds).expect("shifted timestamp")
    }

    fn task(root: &str, every: &str) -> TaskEntry {
        TaskEntry {
            spec: Some("claude".to_owned()),
            prompt: Some("do it".to_owned()),
            root: PathBuf::from(root),
            every: Some(every.to_owned()),
            ..TaskEntry::default()
        }
    }

    #[test]
    fn first_seen_task_arms_without_firing() {
        let now = zdt(2026, 6, 24, 8, 0, 0);
        let tasks = BTreeMap::from([("daily".to_owned(), task("/repo", "5m"))]);
        let (actions, next) = plan(&tasks, &BTreeMap::new(), &now);
        assert_eq!(actions, vec![("daily".to_owned(), Action::Arm)]);
        assert_eq!(next.get("daily"), Some(&now.timestamp()));
    }

    #[test]
    fn due_task_fires_and_refreshes_stamp() {
        let now = zdt(2026, 6, 24, 8, 5, 0);
        let tasks = BTreeMap::from([("daily".to_owned(), task("/repo", "5m"))]);
        let state = BTreeMap::from([("daily".to_owned(), seconds_before(now.timestamp(), 300))]);
        let (actions, next) = plan(&tasks, &state, &now);
        assert_eq!(actions, vec![("daily".to_owned(), Action::Fire)]);
        assert_eq!(next.get("daily"), Some(&now.timestamp()));
    }

    #[test]
    fn not_yet_due_task_carries_prior_stamp() {
        let now = zdt(2026, 6, 24, 8, 4, 0);
        let prior = seconds_before(now.timestamp(), 240);
        let tasks = BTreeMap::from([("daily".to_owned(), task("/repo", "5m"))]);
        let state = BTreeMap::from([("daily".to_owned(), prior)]);
        let (actions, next) = plan(&tasks, &state, &now);
        assert!(actions.is_empty());
        assert_eq!(next.get("daily"), Some(&prior));
    }

    #[test]
    fn stale_state_entry_is_pruned() {
        let now = zdt(2026, 6, 24, 8, 0, 0);
        let state = BTreeMap::from([("gone".to_owned(), seconds_before(now.timestamp(), 300))]);
        let (actions, next) = plan(&BTreeMap::new(), &state, &now);
        assert!(actions.is_empty());
        assert!(next.is_empty());
    }

    #[test]
    fn workspace_filter_excludes_foreign_roots() {
        let owned_root = Path::new("/repo/owned");
        let workspace_id = WorkspaceId::from_project_root(owned_root);
        let tasks = BTreeMap::from([
            ("owned".to_owned(), task("/repo/owned", "5m")),
            ("foreign".to_owned(), task("/repo/foreign", "5m")),
        ]);
        let filtered = workspace_tasks(tasks, &workspace_id);
        assert_eq!(filtered.keys().cloned().collect::<Vec<_>>(), vec!["owned"]);
    }
}
