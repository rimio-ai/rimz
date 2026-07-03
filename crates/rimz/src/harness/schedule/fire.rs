//! Elder-owned loop task firing.
//!
//! The elected sidebar elder keeps time for loop tasks while a room
//! is open. The durable state arms tasks on first sight and records each fire
//! before spawning the detached `rimz loop run <name>` helper, so a hot tick does
//! not spawn the same occurrence twice.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use jiff::{Timestamp, Zoned};

use super::instances;
use crate::RuntimePaths;
use crate::agents::longest_window_reset_at;
use crate::config::TaskEntry;
use crate::harness::schedule;
use crate::harness::spec;
use crate::ids::WorkspaceId;
use crate::ledger::atomic::write_temp_then_rename_cache;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Arm,
    Fire,
}

pub(crate) fn fire_due_tasks(runtime: &RuntimePaths, now: &Zoned) {
    let tasks = workspace_tasks(
        instances::load_all()
            .into_iter()
            .map(|(name, (entry, _))| (name, entry))
            .collect(),
        &runtime.workspace_id,
    );
    let path = state_path(runtime);
    let state = read_state(&path);
    let resets = reset_occurrences(runtime, &tasks);
    let (actions, next_state) = plan(&tasks, &state, now, &resets);
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
            spawn_loop_run(runtime, &name);
        }
    }
}

fn plan(
    tasks: &BTreeMap<String, TaskEntry>,
    state: &BTreeMap<String, Timestamp>,
    now: &Zoned,
    resets: &BTreeMap<String, Timestamp>,
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
            Some(last_fire)
                if parsed
                    .schedule
                    .due(last_fire, now, resets.get(name).copied()) =>
            {
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

fn reset_occurrences(
    runtime: &RuntimePaths,
    tasks: &BTreeMap<String, TaskEntry>,
) -> BTreeMap<String, Timestamp> {
    tasks
        .iter()
        .filter(|(_, entry)| entry.at_reset)
        .filter_map(|(name, entry)| {
            let kind = entry.spec.as_deref().and_then(spec::ping_kind)?;
            longest_window_reset_at(runtime, kind).map(|reset| (name.clone(), reset))
        })
        .collect()
}

fn workspace_tasks(
    tasks: BTreeMap<String, TaskEntry>,
    workspace_id: &WorkspaceId,
) -> BTreeMap<String, TaskEntry> {
    tasks
        .into_iter()
        .filter(|(_, entry)| {
            WorkspaceId::from_project_root(&entry.resolved_root()) == *workspace_id
        })
        .collect()
}

fn read_state(path: &Path) -> BTreeMap<String, Timestamp> {
    let Ok(bytes) = std::fs::read(path) else {
        return BTreeMap::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

pub fn last_stamps(runtime: &RuntimePaths) -> BTreeMap<String, Timestamp> {
    read_state(&state_path(runtime))
}

fn state_path(runtime: &RuntimePaths) -> PathBuf {
    runtime.root.join("loop-fire.json")
}

fn spawn_loop_run(runtime: &RuntimePaths, name: &str) {
    let exe = crate::proc::rimz_exe();
    let mut cmd = Command::new(exe);
    cmd.args(["loop", "run", name])
        .current_dir(&runtime.root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        task = name,
        "sidebar: firing loop task",
    );
    if let Err(err) = crate::child_process::spawn_detached_reaped(&mut cmd, "loop-run") {
        // The CWD anchor clears gc'd-worktree ENOENT; a bad RIMZ_BIN/PATH stays
        // an environment fact. Keep it at debug! so Sentry ignores it, and the
        // next elder tick retries.
        tracing::debug!(
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

    fn reset_task(root: &str) -> TaskEntry {
        TaskEntry {
            spec: Some("claude-ping".to_owned()),
            prompt: Some("ping".to_owned()),
            root: PathBuf::from(root),
            at_reset: true,
            ..TaskEntry::default()
        }
    }

    #[test]
    fn first_seen_task_arms_without_firing() {
        let now = zdt(2026, 6, 24, 8, 0, 0);
        let tasks = BTreeMap::from([("daily".to_owned(), task("/repo", "5m"))]);
        let (actions, next) = plan(&tasks, &BTreeMap::new(), &now, &BTreeMap::new());
        assert_eq!(actions, vec![("daily".to_owned(), Action::Arm)]);
        assert_eq!(next.get("daily"), Some(&now.timestamp()));
    }

    #[test]
    fn due_task_fires_and_refreshes_stamp() {
        let now = zdt(2026, 6, 24, 8, 5, 0);
        let tasks = BTreeMap::from([("daily".to_owned(), task("/repo", "5m"))]);
        let state = BTreeMap::from([("daily".to_owned(), seconds_before(now.timestamp(), 300))]);
        let (actions, next) = plan(&tasks, &state, &now, &BTreeMap::new());
        assert_eq!(actions, vec![("daily".to_owned(), Action::Fire)]);
        assert_eq!(next.get("daily"), Some(&now.timestamp()));
    }

    #[test]
    fn not_yet_due_task_carries_prior_stamp() {
        let now = zdt(2026, 6, 24, 8, 4, 0);
        let prior = seconds_before(now.timestamp(), 240);
        let tasks = BTreeMap::from([("daily".to_owned(), task("/repo", "5m"))]);
        let state = BTreeMap::from([("daily".to_owned(), prior)]);
        let (actions, next) = plan(&tasks, &state, &now, &BTreeMap::new());
        assert!(actions.is_empty());
        assert_eq!(next.get("daily"), Some(&prior));
    }

    #[test]
    fn stale_state_entry_is_pruned() {
        let now = zdt(2026, 6, 24, 8, 0, 0);
        let state = BTreeMap::from([("gone".to_owned(), seconds_before(now.timestamp(), 300))]);
        let (actions, next) = plan(&BTreeMap::new(), &state, &now, &BTreeMap::new());
        assert!(actions.is_empty());
        assert!(next.is_empty());
    }

    #[test]
    fn at_reset_task_arms_then_fires_once_per_observed_reset() {
        let reset = zdt(2026, 6, 24, 8, 0, 0).timestamp();
        let occurrence = reset
            .checked_add(schedule::RESET_PING_MARGIN)
            .expect("reset occurrence");
        let now = occurrence.to_zoned(jiff::tz::TimeZone::UTC);
        let tasks = BTreeMap::from([("w7".to_owned(), reset_task("/repo"))]);
        let resets = BTreeMap::from([("w7".to_owned(), reset)]);

        let (actions, next) = plan(&tasks, &BTreeMap::new(), &now, &resets);
        assert_eq!(actions, vec![("w7".to_owned(), Action::Arm)]);
        assert_eq!(next.get("w7"), Some(&occurrence));

        let state = BTreeMap::from([("w7".to_owned(), seconds_before(occurrence, 1))]);
        let (actions, next) = plan(&tasks, &state, &now, &resets);
        assert_eq!(actions, vec![("w7".to_owned(), Action::Fire)]);
        assert_eq!(next.get("w7"), Some(&occurrence));

        let state = BTreeMap::from([("w7".to_owned(), occurrence)]);
        let (actions, next) = plan(&tasks, &state, &now, &resets);
        assert!(actions.is_empty());
        assert_eq!(next.get("w7"), Some(&occurrence));
    }

    #[test]
    fn at_reset_task_without_cached_reset_never_fires() {
        let now = zdt(2026, 6, 24, 8, 1, 0);
        let tasks = BTreeMap::from([("w7".to_owned(), reset_task("/repo"))]);
        let state = BTreeMap::from([("w7".to_owned(), seconds_before(now.timestamp(), 120))]);

        let (actions, next) = plan(&tasks, &state, &now, &BTreeMap::new());

        assert!(actions.is_empty());
        assert_eq!(next.get("w7"), state.get("w7"));
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

    #[test]
    fn workspace_filter_expands_tilde_roots() {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        let home = home.canonicalize().unwrap_or(home);
        let workspace_id = WorkspaceId::from_project_root(&home);
        let tasks = BTreeMap::from([("home".to_owned(), task("~", "5m"))]);

        let filtered = workspace_tasks(tasks, &workspace_id);

        assert_eq!(filtered.keys().cloned().collect::<Vec<_>>(), vec!["home"]);
    }
}
