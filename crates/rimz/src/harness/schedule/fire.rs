//! Elder-owned loop task firing.
//!
//! The elected sidebar elder keeps time for loop tasks while a room
//! is open. The durable state arms tasks on first sight and records each fire
//! before spawning the detached `rimz loop run <name>` helper, so a hot tick does
//! not spawn the same occurrence twice.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::{Timestamp, Zoned};

use super::pauses::PauseEntry;
use super::{
    catalog::{LoadedTask, TaskCatalog},
    pauses,
};
use crate::RuntimePaths;
use crate::harness::schedule;
use crate::ids::WorkspaceId;
use crate::store::atomic::write_temp_then_rename_cache;
use crate::store::paths::StatePaths;
use crate::store::workspace_record;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Arm,
    Fire,
}

pub(crate) fn fire_due_tasks(runtime: &RuntimePaths, now: &Zoned) {
    let project_root = workspace_project_root(runtime);
    let tasks = workspace_tasks(
        TaskCatalog::load_lenient(project_root.as_deref())
            .runnable()
            .iter()
            .filter(|(_, task)| {
                !matches!(
                    task.source(),
                    super::catalog::TaskSource::Project { state }
                        if state != crate::trust::TrustState::Trusted
                )
            })
            .map(|(name, task)| (name.clone(), task.clone()))
            .collect(),
        &runtime.workspace_id,
    );
    let path = state_path(runtime);
    let state = read_state(&path);
    let pauses = pauses::load();
    let reset_signals = reset_signals(runtime, &tasks, now.timestamp());
    let (actions, next_state) = plan(&tasks, &state, &pauses, now, &reset_signals);
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
            spawn_loop_run(runtime, project_root.as_deref(), &name);
        }
    }
}

fn workspace_project_root(runtime: &RuntimePaths) -> Option<PathBuf> {
    let paths = StatePaths::for_workspace(runtime.workspace_id.clone()).ok()?;
    match workspace_record::read(&paths.workspace_record) {
        Ok(record) => Some(record.project_root),
        Err(err) => {
            tracing::debug!(
                workspace = %runtime.workspace_id,
                error = &err as &dyn std::error::Error,
                "loop elder skipped project tasks without workspace record"
            );
            None
        }
    }
}

fn plan(
    tasks: &BTreeMap<String, LoadedTask>,
    state: &BTreeMap<String, Timestamp>,
    pauses: &BTreeMap<String, PauseEntry>,
    now: &Zoned,
    reset_signals: &BTreeMap<String, schedule::ResetSignal>,
) -> (Vec<(String, Action)>, BTreeMap<String, Timestamp>) {
    let mut actions = Vec::new();
    let mut next_state = BTreeMap::new();
    for (name, task) in tasks {
        let parsed = match task.schedule() {
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
        let pause = pauses.get(name);
        match state.get(name).copied() {
            None => {
                actions.push((name.clone(), Action::Arm));
                next_state.insert(name.clone(), now.timestamp());
            }
            Some(last_fire)
                if pause.is_some_and(|entry| pauses::is_active(entry, now.timestamp())) =>
            {
                next_state.insert(name.clone(), last_fire);
            }
            Some(last_fire)
                if parsed.schedule.due(
                    pauses::effective_last_fire(last_fire, pause, now.timestamp()),
                    now,
                    reset_signals
                        .get(name)
                        .copied()
                        .unwrap_or(schedule::ResetSignal::Unknown),
                ) =>
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

fn reset_signals(
    runtime: &RuntimePaths,
    tasks: &BTreeMap<String, LoadedTask>,
    now: Timestamp,
) -> BTreeMap<String, schedule::ResetSignal> {
    tasks
        .iter()
        .filter_map(|(name, task)| {
            let kind = task.reset_ping_kind()?;
            Some((
                name.clone(),
                super::runner::window_reset_signal_in(runtime, task.entry(), kind.as_str(), now),
            ))
        })
        .collect()
}

fn workspace_tasks(
    tasks: BTreeMap<String, LoadedTask>,
    workspace_id: &WorkspaceId,
) -> BTreeMap<String, LoadedTask> {
    tasks
        .into_iter()
        .filter(|(_, task)| {
            WorkspaceId::from_project_root(&task.entry().resolved_root()) == *workspace_id
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

fn spawn_loop_run(runtime: &RuntimePaths, project_root: Option<&Path>, name: &str) {
    let mut cmd = crate::child_process::detached_rimz_command(crate::proc::rimz_exe(), runtime);
    if let Some(project_root) = project_root {
        cmd.arg("--root").arg(project_root);
    }
    cmd.args(["loop", "run", name]);
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        task = name,
        "sidebar: firing loop task",
    );
    if let Err(err) = crate::child_process::spawn_detached_reaped(&mut cmd, "loop-run") {
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
    use crate::config::TaskEntry;
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

    fn task(root: &str, every: &str) -> LoadedTask {
        let entry = TaskEntry {
            agent: Some("claude".to_owned()),
            prompt: Some("do it".to_owned()),
            root: PathBuf::from(root),
            every: Some(every.to_owned()),
            ..TaskEntry::default()
        };
        LoadedTask::new(
            "task",
            entry,
            super::super::catalog::TaskSource::Config,
            false,
        )
    }

    fn reset_task(root: &str) -> LoadedTask {
        let entry = TaskEntry {
            agent: Some("claude-ping".to_owned()),
            prompt: Some("ping".to_owned()),
            root: PathBuf::from(root),
            every: Some("reset".to_owned()),
            ..TaskEntry::default()
        };
        LoadedTask::new(
            "reset",
            entry,
            super::super::catalog::TaskSource::Config,
            false,
        )
    }

    #[test]
    fn first_seen_task_arms_without_firing() {
        let now = zdt(2026, 6, 24, 8, 0, 0);
        let tasks = BTreeMap::from([("daily".to_owned(), task("/repo", "5m"))]);
        let (actions, next) = plan(
            &tasks,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &now,
            &BTreeMap::new(),
        );
        assert_eq!(actions, vec![("daily".to_owned(), Action::Arm)]);
        assert_eq!(next.get("daily"), Some(&now.timestamp()));
    }

    #[test]
    fn due_task_fires_and_refreshes_stamp() {
        let now = zdt(2026, 6, 24, 8, 5, 0);
        let tasks = BTreeMap::from([("daily".to_owned(), task("/repo", "5m"))]);
        let state = BTreeMap::from([("daily".to_owned(), seconds_before(now.timestamp(), 300))]);
        let (actions, next) = plan(&tasks, &state, &BTreeMap::new(), &now, &BTreeMap::new());
        assert_eq!(actions, vec![("daily".to_owned(), Action::Fire)]);
        assert_eq!(next.get("daily"), Some(&now.timestamp()));
    }

    #[test]
    fn not_yet_due_task_carries_prior_stamp() {
        let now = zdt(2026, 6, 24, 8, 4, 0);
        let prior = seconds_before(now.timestamp(), 240);
        let tasks = BTreeMap::from([("daily".to_owned(), task("/repo", "5m"))]);
        let state = BTreeMap::from([("daily".to_owned(), prior)]);
        let (actions, next) = plan(&tasks, &state, &BTreeMap::new(), &now, &BTreeMap::new());
        assert!(actions.is_empty());
        assert_eq!(next.get("daily"), Some(&prior));
    }

    #[test]
    fn stale_state_entry_is_pruned() {
        let now = zdt(2026, 6, 24, 8, 0, 0);
        let state = BTreeMap::from([("gone".to_owned(), seconds_before(now.timestamp(), 300))]);
        let (actions, next) = plan(
            &BTreeMap::new(),
            &state,
            &BTreeMap::new(),
            &now,
            &BTreeMap::new(),
        );
        assert!(actions.is_empty());
        assert!(next.is_empty());
    }

    #[test]
    fn malformed_schedule_is_skipped_without_arming() {
        let entry = TaskEntry {
            agent: Some("claude".to_owned()),
            every: Some("5m".to_owned()),
            at: Some("07:00".to_owned()),
            ..TaskEntry::default()
        };
        let task = LoadedTask::new(
            "broken",
            entry,
            super::super::catalog::TaskSource::Config,
            false,
        );
        let now = zdt(2026, 6, 24, 8, 0, 0);
        let state = BTreeMap::from([("broken".to_owned(), seconds_before(now.timestamp(), 300))]);

        let (actions, next) = plan(
            &BTreeMap::from([("broken".to_owned(), task)]),
            &state,
            &BTreeMap::new(),
            &now,
            &BTreeMap::new(),
        );

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
        let resets = BTreeMap::from([("w7".to_owned(), schedule::ResetSignal::At(reset))]);

        let (actions, next) = plan(&tasks, &BTreeMap::new(), &BTreeMap::new(), &now, &resets);
        assert_eq!(actions, vec![("w7".to_owned(), Action::Arm)]);
        assert_eq!(next.get("w7"), Some(&occurrence));

        let state = BTreeMap::from([("w7".to_owned(), seconds_before(occurrence, 1))]);
        let (actions, next) = plan(&tasks, &state, &BTreeMap::new(), &now, &resets);
        assert_eq!(actions, vec![("w7".to_owned(), Action::Fire)]);
        assert_eq!(next.get("w7"), Some(&occurrence));

        let state = BTreeMap::from([("w7".to_owned(), occurrence)]);
        let (actions, next) = plan(&tasks, &state, &BTreeMap::new(), &now, &resets);
        assert!(actions.is_empty());
        assert_eq!(next.get("w7"), Some(&occurrence));
    }

    #[test]
    fn unknown_reset_task_never_fires() {
        let now = zdt(2026, 6, 24, 8, 1, 0);
        let tasks = BTreeMap::from([("w7".to_owned(), reset_task("/repo"))]);
        let state = BTreeMap::from([("w7".to_owned(), seconds_before(now.timestamp(), 120))]);

        let (actions, next) = plan(&tasks, &state, &BTreeMap::new(), &now, &BTreeMap::new());

        assert!(actions.is_empty());
        assert_eq!(next.get("w7"), state.get("w7"));
    }

    #[test]
    fn confirmed_down_reset_task_fires_hourly() {
        let now = zdt(2026, 6, 24, 8, 1, 0);
        let tasks = BTreeMap::from([("w7".to_owned(), reset_task("/repo"))]);
        let signals = BTreeMap::from([("w7".to_owned(), schedule::ResetSignal::ConfirmedDown)]);
        let not_due = BTreeMap::from([("w7".to_owned(), seconds_before(now.timestamp(), 3_599))]);
        let due = BTreeMap::from([("w7".to_owned(), seconds_before(now.timestamp(), 3_600))]);

        let (actions, next) = plan(&tasks, &not_due, &BTreeMap::new(), &now, &signals);
        assert!(actions.is_empty());
        assert_eq!(next, not_due);

        let (actions, next) = plan(&tasks, &due, &BTreeMap::new(), &now, &signals);
        assert_eq!(actions, vec![("w7".to_owned(), Action::Fire)]);
        assert_eq!(next.get("w7"), Some(&now.timestamp()));
    }

    #[test]
    fn qwen_reset_signal_requires_exact_bound_capacity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("runtime paths");
        runtime.ensure_dirs().expect("runtime dirs");
        let now = Timestamp::from_second(1_000_000).expect("now");
        let reset = now + jiff::SignedDuration::from_secs(2 * 86_400);
        let scope = crate::agents::ProviderAccountScope::sub_provider("alibaba", "international");
        let cache = crate::agents::account::RateLimitsCache {
            entries: BTreeMap::from([(
                "qwen".to_owned(),
                crate::agents::account::RateLimitCacheEntry {
                    scope: scope.clone(),
                    account_key: Some("owner".to_owned()),
                    limits: crate::agents::AgentRateLimits {
                        windows: vec![crate::agents::RateLimitWindow {
                            used_percentage: Some(40),
                            resets_at: Some(reset),
                            duration_mins: Some(7 * 24 * 60),
                            ..Default::default()
                        }],
                    },
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        crate::store::atomic::write_temp_then_rename_cache(
            &runtime.shared_rate_limits_path(),
            &cache,
        )
        .expect("rate-limit cache");

        let mismatch =
            crate::agents::ProviderAccountBinding::new(scope.clone(), "other".to_owned())
                .expect("mismatched binding");
        let matching = crate::agents::ProviderAccountBinding::new(scope, "owner".to_owned())
            .expect("matching binding");
        let unbound_entry = TaskEntry {
            agent: Some("qwen-ping".to_owned()),
            root: dir.path().to_path_buf(),
            worktree: Some("isolated".to_owned()),
            ..TaskEntry::default()
        };
        assert_eq!(
            [
                super::super::runner::window_reset_signal_in(&runtime, &unbound_entry, "qwen", now,),
                super::super::runner::reset_signal_for_test_binding(
                    &runtime,
                    "qwen",
                    Some(&mismatch),
                    now,
                ),
                super::super::runner::reset_signal_for_test_binding(
                    &runtime,
                    "qwen",
                    Some(&matching),
                    now,
                ),
            ],
            [
                schedule::ResetSignal::Unknown,
                schedule::ResetSignal::Unknown,
                schedule::ResetSignal::At(reset),
            ]
        );
    }

    #[test]
    fn ordinary_reset_signal_uses_unbound_capacity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("runtime paths");
        runtime.ensure_dirs().expect("runtime dirs");
        let now = Timestamp::from_second(1_000_000).expect("now");
        let reset = now + jiff::SignedDuration::from_hours(5);
        let cache = crate::agents::account::RateLimitsCache {
            entries: BTreeMap::from([(
                "claude".to_owned(),
                crate::agents::account::RateLimitCacheEntry {
                    limits: crate::agents::AgentRateLimits {
                        windows: vec![crate::agents::RateLimitWindow {
                            used_percentage: Some(40),
                            resets_at: Some(reset),
                            duration_mins: Some(5 * 60),
                            ..Default::default()
                        }],
                    },
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        crate::store::atomic::write_temp_then_rename_cache(
            &runtime.shared_rate_limits_path(),
            &cache,
        )
        .expect("rate-limit cache");

        assert_eq!(
            super::super::runner::window_reset_signal_in(
                &runtime,
                reset_task(dir.path().to_str().expect("utf8 path")).entry(),
                "claude",
                now,
            ),
            schedule::ResetSignal::At(reset),
        );
    }

    #[test]
    fn active_pause_holds_existing_stamp() {
        let now = zdt(2026, 6, 24, 8, 0, 0);
        let prior = seconds_before(now.timestamp(), 600);
        let tasks = BTreeMap::from([("daily".to_owned(), task("/repo", "5m"))]);
        let state = BTreeMap::from([("daily".to_owned(), prior)]);
        let pauses = BTreeMap::from([(
            "daily".to_owned(),
            PauseEntry {
                until: None,
                strikes: Some(3),
            },
        )]);

        let (actions, next) = plan(&tasks, &state, &pauses, &now, &BTreeMap::new());

        assert!(actions.is_empty());
        assert_eq!(next.get("daily"), Some(&prior));
    }

    #[test]
    fn first_seen_task_arms_while_paused() {
        let now = zdt(2026, 6, 24, 8, 0, 0);
        let tasks = BTreeMap::from([("daily".to_owned(), task("/repo", "5m"))]);
        let pauses = BTreeMap::from([("daily".to_owned(), PauseEntry::default())]);

        let (actions, next) = plan(&tasks, &BTreeMap::new(), &pauses, &now, &BTreeMap::new());

        assert_eq!(actions, vec![("daily".to_owned(), Action::Arm)]);
        assert_eq!(next.get("daily"), Some(&now.timestamp()));
    }

    #[test]
    fn ended_pause_sets_interval_edge() {
        let now = zdt(2026, 6, 24, 8, 0, 0);
        let pause_end = seconds_before(now.timestamp(), 240);
        let tasks = BTreeMap::from([("daily".to_owned(), task("/repo", "5m"))]);
        let state = BTreeMap::from([("daily".to_owned(), seconds_before(pause_end, 600))]);
        let pauses = BTreeMap::from([(
            "daily".to_owned(),
            PauseEntry {
                until: Some(pause_end),
                strikes: None,
            },
        )]);

        let (actions, next) = plan(&tasks, &state, &pauses, &now, &BTreeMap::new());
        assert!(actions.is_empty());
        assert_eq!(next.get("daily"), state.get("daily"));

        let due = zdt(2026, 6, 24, 8, 1, 0);
        let (actions, next) = plan(&tasks, &state, &pauses, &due, &BTreeMap::new());
        assert_eq!(actions, vec![("daily".to_owned(), Action::Fire)]);
        assert_eq!(next.get("daily"), Some(&due.timestamp()));
    }

    #[test]
    fn ended_pause_skips_crossed_calendar_occurrence() {
        let entry = TaskEntry {
            agent: Some("claude".to_owned()),
            prompt: Some("do it".to_owned()),
            root: PathBuf::from("/repo"),
            every: Some("day".to_owned()),
            at: Some("07:00".to_owned()),
            ..TaskEntry::default()
        };
        let task = LoadedTask::new(
            "daily",
            entry,
            super::super::catalog::TaskSource::Config,
            false,
        );
        let tasks = BTreeMap::from([("daily".to_owned(), task)]);
        let state = BTreeMap::from([("daily".to_owned(), zdt(2026, 6, 23, 6, 0, 0).timestamp())]);
        let pauses = BTreeMap::from([(
            "daily".to_owned(),
            PauseEntry {
                until: Some(zdt(2026, 6, 24, 7, 30, 0).timestamp()),
                strikes: None,
            },
        )]);

        let after_crossed_occurrence = zdt(2026, 6, 24, 8, 0, 0);
        let (actions, _) = plan(
            &tasks,
            &state,
            &pauses,
            &after_crossed_occurrence,
            &BTreeMap::new(),
        );
        assert!(actions.is_empty());

        let next_occurrence = zdt(2026, 6, 25, 7, 0, 0);
        let (actions, _) = plan(&tasks, &state, &pauses, &next_occurrence, &BTreeMap::new());
        assert_eq!(actions, vec![("daily".to_owned(), Action::Fire)]);
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
