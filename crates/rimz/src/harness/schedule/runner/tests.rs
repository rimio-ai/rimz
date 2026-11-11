use std::path::Path;

use super::*;
use crate::disk::lease::assert_waits_for_gate;

/// Every test process under `cargo xtask test` shares one RIMZ_HOME, so a machine task's arming
/// and strike key would be shared with every sibling built here; an instance key is scoped to
/// the test's own root.
fn backlog_fire<'a>(root: &Path, catalog: &'a TaskCatalog) -> TaskFire<'a> {
    TaskFire::new(
        "pending",
        LoadedTask::new(
            "pending",
            TaskEntry {
                root: root.to_owned(),
                check: Some("true".into()),
                every: Some("1h".into()),
                overlap: Some(crate::config::OverlapPolicy::Coalesce),
                ..Default::default()
            },
            catalog::TaskSource::Instance,
        ),
        catalog,
        LoopRunMode::Scheduled,
        false,
        Timestamp::now(),
        Arc::new(MachineConfig::default()),
        None,
        Instant::now(),
    )
    .unwrap()
}

fn seed_backlog(root: &Path) {
    let paths = StatePaths::for_project_root(root).unwrap();
    crate::disk::atomic::write_temp_then_rename(
        &super::super::backlog::path(&paths),
        &serde_json::json!({"version":1, "tasks":{"pending":{"triggers":[{"arrived_at":Timestamp::now(),"signal":null,"absorbed":0}]}}})
    ).unwrap();
}

#[test]
fn terminal_records_name_the_lock_that_refused_other_fires() {
    for unreadable_backlog in [false, true] {
        let root = tempfile::tempdir().unwrap();
        record_room(root.path()).unwrap();
        let paths = StatePaths::for_project_root(root.path()).unwrap();
        let catalog = TaskCatalog::load(Some(root.path())).unwrap();
        let mut fire = backlog_fire(root.path(), &catalog);
        assert!(fire.prepare_run_lock().unwrap().is_none());
        let (_, Some(info)) = held_run_lock("pending", root.path()).unwrap().unwrap() else {
            panic!("holder is recorded")
        };
        let mut refused = backlog_fire(root.path(), &catalog);
        let refused = refused.prepare_run_lock().unwrap().unwrap();
        assert_eq!(refused.record.result, LoopRunResult::Deferred);
        assert_eq!(refused.record.holder, Some(info));
        assert_eq!(refused.record.run_lock, None);
        std::fs::write(
            backlog::path(&paths),
            if unreadable_backlog {
                "not json"
            } else {
                r#"{"version":1,"tasks":{}}"#
            },
        )
        .unwrap();

        let done = fire.record_terminal_with(
            LoopRunResult::Completed,
            LoopRunPresentation::default(),
            TaskFireNotice::None,
            None,
            |_| {},
        );
        assert_eq!(done.record.run_lock, Some(info));
        let recorded = run_log::task_records(
            &crate::disk::paths::logs_dir(),
            "pending",
            Some(root.path()),
        );
        assert_eq!(recorded.last().unwrap().run_lock, Some(info));
    }
}

#[test]
fn nonretaining_agent_decisions_tolerate_an_unreadable_backlog() {
    for contents in ["not json", r#"{"version":2,"tasks":{}}"#] {
        for mode in [LoopRunMode::Scheduled, LoopRunMode::Manual] {
            for overlap in [
                None,
                Some(OverlapPolicy::Skip),
                Some(OverlapPolicy::Coalesce),
                Some(OverlapPolicy::Queue),
                Some(OverlapPolicy::Replace),
            ] {
                let root = tempfile::tempdir().unwrap();
                record_room(root.path()).unwrap();
                let paths = StatePaths::for_project_root(root.path()).unwrap();
                let catalog = TaskCatalog::load(Some(root.path())).unwrap();
                let name = "unreadable-backlog-agent";
                let mut fire = TaskFire::new(
                    name,
                    LoadedTask::new(
                        name,
                        TaskEntry {
                            root: root.path().to_owned(),
                            agent: Some("claude".into()),
                            prompt: Some("work".into()),
                            every: Some("1h".into()),
                            overlap,
                            ..Default::default()
                        },
                        catalog::TaskSource::Config,
                    ),
                    &catalog,
                    mode,
                    false,
                    Timestamp::now(),
                    Arc::new(MachineConfig::default()),
                    None,
                    Instant::now(),
                )
                .unwrap();
                fire.prepare_run_lock().unwrap();
                let path = backlog::path(&paths);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, contents).unwrap();
                let result = fire.reconcile_backlog();
                if mode == LoopRunMode::Scheduled
                    && overlap.unwrap_or_default() != OverlapPolicy::Skip
                {
                    assert!(
                        result.is_err(),
                        "a retaining scheduled fire must stay strict"
                    );
                    continue;
                }
                assert!(
                    matches!(result, Ok(None)),
                    "an unrelated unreadable backlog must not cause an errored strike: {mode:?}, {overlap:?}"
                );
                let done = fire.record_terminal_with(
                    LoopRunResult::Completed,
                    LoopRunPresentation::default(),
                    TaskFireNotice::None,
                    None,
                    |_| {},
                );
                assert_eq!(done.record.result, LoopRunResult::Completed);
                assert_eq!(done.transition, RunTransition::Recorded);
                assert!(!super::super::strikes::load().contains_key(&fire.task.key(name)));
            }
        }
    }
}

#[test]
fn reconciliation_keeps_firing_order_on_acquisition_and_after_admission() {
    for after_admission in [false, true] {
        let root = tempfile::tempdir().unwrap();
        record_room(root.path()).unwrap();
        let paths = StatePaths::for_project_root(root.path()).unwrap();
        let catalog = TaskCatalog::load(Some(root.path())).unwrap();
        let mut fire = backlog_fire(root.path(), &catalog);
        let (own_time, own_from, own_to, waiting_time, waiting_from, waiting_to) =
            if after_admission {
                (2, "b", "c", 1, "a", "b")
            } else {
                (1, "a", "b", 2, "b", "c")
            };
        fire.now = Timestamp::from_second(own_time).unwrap();
        fire.signal = Some(
            serde_json::from_value(serde_json::json!({
                "name": "trunk.moved", "source": "git",
                "payload": {"from": own_from, "to": own_to}
            }))
            .unwrap(),
        );
        if after_admission {
            fire.prepare_run_lock().unwrap();
        }
        let mut locked = LockedBacklog::acquire(&paths).unwrap();
        locked.record.tasks.insert("pending".into(), serde_json::from_value(serde_json::json!({
            "triggers": [{"arrived_at": Timestamp::from_second(waiting_time).unwrap(),
                "signal": {"name": "trunk.moved", "payload": {"from": waiting_from, "to": waiting_to}},
                "absorbed": 0}]
        })).unwrap());
        locked.save().unwrap();
        drop(locked);
        if after_admission {
            assert!(fire.reconcile_backlog().unwrap().is_none());
        } else {
            assert!(fire.prepare_run_lock().unwrap().is_none());
        }
        assert_eq!(
            fire.signal.as_ref().unwrap().payload,
            *serde_json::json!({"from": "a", "to": "c"})
                .as_object()
                .unwrap()
        );
        assert_eq!(fire.waiting_trigger().arrived_at.as_second(), 2);
        assert_eq!(fire.absorbed, 1);
    }
}

#[test]
fn cold_runtime_holder_is_visible_without_a_room_record() {
    let root = tempfile::tempdir().unwrap();
    let paths = StatePaths::for_project_root(root.path()).unwrap();
    let runtime = RuntimePaths::for_state(&paths).unwrap();
    let path = runtime.lock_path(run_lock_file_name("pending", None));
    let RunLockAttempt::Acquired(_guard) =
        acquire_run_lock_file(&path, Some(TaskKind::Routine), None).unwrap()
    else {
        panic!("fixture takes lock")
    };
    assert!(
        matches!(
            RunLocks::list(root.path())
                .unwrap()
                .state("pending")
                .unwrap(),
            RunLockState::Held(_)
        ),
        "stop must find a cold runtime-only holder"
    );
    assert!(in_flight_run("pending", root.path()).unwrap().is_some());
    assert!(
        !paths.workspace_record.exists(),
        "lookup must not birth a room"
    );
}

#[test]
fn stop_marks_retype_the_holder_once_and_replacements_never_strike() {
    for cause in [backlog::StopCause::Stop, backlog::StopCause::Replace] {
        let root = tempfile::tempdir().unwrap();
        record_room(root.path()).unwrap();
        let paths = StatePaths::for_project_root(root.path()).unwrap();
        let catalog = TaskCatalog::load(Some(root.path())).unwrap();
        for strike in 1..=3 {
            let mut fire = backlog_fire(root.path(), &catalog);
            fire.prepare_run_lock().unwrap();
            let (_, Some(info)) = held_run_lock("pending", root.path()).unwrap().unwrap() else {
                panic!("holder is recorded")
            };
            let mut locked = LockedBacklog::acquire(&paths).unwrap();
            locked
                .record
                .tasks
                .entry("pending".into())
                .or_default()
                .stopping = Some(backlog::StopMark {
                pid: info.pid,
                started_at: info.started_at,
                cause,
            });
            locked.save().unwrap();
            drop(locked);
            let done = fire.record_terminal_with(
                LoopRunResult::Canceled,
                LoopRunPresentation::default(),
                TaskFireNotice::None,
                None,
                |_| {},
            );
            assert_eq!(done.record.result, LoopRunResult::Canceled);
            assert_eq!(
                done.record.error.as_deref(),
                Some(if cause == backlog::StopCause::Replace {
                    "replaced by a newer trigger"
                } else {
                    "stopped by rimz loop stop"
                })
            );
            assert!(
                !backlog::load(&paths).unwrap().tasks.contains_key("pending"),
                "the recording runner must clear its mark"
            );
            assert!(
                !matches!(done.transition, RunTransition::AutoDisabled { .. }),
                "stop {strike} by {cause:?} must not strike"
            );
        }
    }
}

#[test]
fn arrival_reprobes_a_released_lock_and_absorbs_the_waiting_slot() {
    let root = tempfile::tempdir().unwrap();
    record_room(root.path()).unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    seed_backlog(root.path());
    let mut fire = backlog_fire(root.path(), &catalog);
    assert!(fire.prepare_run_lock().unwrap().is_none());
    assert!(
        fire.run_lock.is_some(),
        "an arrival that finds the lock free runs"
    );
    assert_eq!(
        serde_json::to_value(fire.terminal_record(LoopRunResult::Completed)).unwrap()["absorbed"],
        1
    );
}

#[test]
fn release_unlocks_under_the_backlog_lock_and_spawns_only_when_triggers_wait() {
    for pending in [false, true] {
        let root = tempfile::tempdir().unwrap();
        record_room(root.path()).unwrap();
        let paths = StatePaths::for_project_root(root.path()).unwrap();
        let catalog = TaskCatalog::load(Some(root.path())).unwrap();
        let mut fire = backlog_fire(root.path(), &catalog);
        fire.mode = LoopRunMode::Manual;
        fire.prepare_run_lock().unwrap();
        if pending {
            seed_backlog(root.path());
        }
        let mut spawns = 0;
        fire.release_run_lock_with(|_, _, _| {
            spawns += 1;
            assert!(
                crate::disk::lock::WorkspaceLock::try_acquire(
                    &paths.lock_path("loop-backlog.lock")
                )
                .unwrap()
                .is_none()
            );
            assert!(matches!(
                probe_run_lock_path(
                    &run_lock_path("loop-run-pending.lock", &fire_entry(root.path())).unwrap()
                )
                .unwrap(),
                RunLockState::Held(_)
            ));
            Ok(())
        })
        .unwrap();
        assert_eq!(spawns, usize::from(pending));
        assert!(
            fire.run_lock.is_none(),
            "release must let go even when nothing waits"
        );
    }
}

fn fire_entry(root: &Path) -> TaskEntry {
    TaskEntry {
        root: root.to_owned(),
        ..Default::default()
    }
}

#[test]
fn a_cold_check_observes_the_run_lock_before_it_can_execute() {
    let root = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    let paths = StatePaths::for_project_root(root.path()).unwrap();
    let runtime = RuntimePaths::for_state(&paths).unwrap();
    let mut fire = backlog_fire(root.path(), &catalog);
    fire.entry.overlap = None;
    let mut checked = false;
    fire.prepare(&mut |_| {
        checked = true;
        let path = runtime.lock_path(run_lock_file_name("pending", None));
        assert!(
            matches!(probe_run_lock_path(&path).unwrap(), RunLockState::Held(_)),
            "a cold check must already own the run lock"
        );
        Ok(())
    })
    .unwrap();
    assert!(checked);
}

#[test]
fn a_drainer_claims_nothing_while_held_then_pops_exactly_one_when_free() {
    let root = tempfile::tempdir().unwrap();
    record_room(root.path()).unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    seed_backlog(root.path());
    let lock_path = run_lock_path("loop-run-pending.lock", &fire_entry(root.path())).unwrap();
    let RunLockAttempt::Acquired(held) =
        acquire_run_lock_file(&lock_path, Some(TaskKind::Routine), None).unwrap()
    else {
        panic!("fixture takes lock")
    };
    let mut fire = backlog_fire(root.path(), &catalog);
    assert!(!fire.take_backlog().unwrap());
    assert!(!fire.finished, "held drainer writes no row");
    drop(held);
    let paths = StatePaths::for_project_root(root.path()).unwrap();
    let arrived_at = backlog::load(&paths).unwrap().tasks["pending"].triggers[0].arrived_at;
    assert!(
        fire.take_backlog().unwrap(),
        "free drainer must claim waiting work"
    );
    assert_eq!(fire.waiting_trigger().arrived_at, arrived_at);
    assert!(!fire.finished);
}

fn record_room(root: &Path) -> Result<()> {
    let workspace = crate::workspace::WorkspaceResolver::resolve(root, None)?;
    let paths = StatePaths::for_project_root(root)?;
    let runtime = RuntimePaths::for_state(&paths)?;
    crate::Store::open(paths, runtime)?.record_workspace(&workspace)?;
    Ok(())
}

#[test]
fn scheduled_workflow_rechecks_launch_memory_under_early_and_late_run_locks() {
    let mut outcomes = Vec::new();
    for late in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let paths = StatePaths::for_project_root(root.path()).unwrap();
        let seed = || {
            crate::disk::atomic::write_temp_then_rename(
                &super::super::launch_ledger::workflow_path(&paths),
                &serde_json::json!({"version":1,"tasks":{"land":{root.path().to_str().unwrap():{
                    "at":Timestamp::now(), "run":crate::ids::WorkflowRunId::new(),
                    "scope":"worktree"
                }}}}),
            )
            .unwrap();
        };
        if !late {
            record_room(root.path()).unwrap();
            seed();
        }
        let catalog = TaskCatalog::load(Some(root.path())).unwrap();
        let mut fire = TaskFire::new(
            "land",
            LoadedTask::new(
                "land",
                TaskEntry {
                    root: root.path().to_owned(),
                    workflow: Some("review".into()),
                    when: Some(vec!["pr=open".into()]),
                    ..Default::default()
                },
                catalog::TaskSource::Config,
            ),
            &catalog,
            LoopRunMode::Scheduled,
            false,
            Timestamp::now(),
            Arc::new(MachineConfig::default()),
            None,
            Instant::now(),
        )
        .unwrap();
        let mut calls = 0;
        let result = fire.prepare(&mut |_| {
            calls += 1;
            if !late {
                bail!("room birth must not be reached");
            }
            record_room(root.path())?;
            seed();
            Ok(())
        });
        outcomes.push((
            matches!(result, Ok(TaskFirePlan::AlreadyLaunched)),
            calls,
            fire.run_lock.is_some(),
        ));
    }
    // Launch memory is read before the lease, then again under it after the room is born.
    assert_eq!(outcomes, vec![(true, 0, false), (true, 1, true)]);
}

#[test]
fn resident_fire_obeys_fleet_budget_and_deadline_before_launch() {
    let root = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    let now: Timestamp = "2026-06-02T12:00:00Z".parse().unwrap();
    let runtime = RuntimePaths::for_project_root(root.path()).unwrap();
    crate::agents::spending::write_workspace_spending_cache(
        &runtime.workspace_spending_path("resident"),
        &crate::agents::spending::WorkspaceSpendingCache {
            scope_hash: "resident".into(),
            day: crate::agents::spending::SpendWindow {
                usd: 6.0,
                ..Default::default()
            },
            day_cutoff_secs: "2026-06-02T00:00:00Z"
                .parse::<Timestamp>()
                .unwrap()
                .as_second() as u64,
            ..Default::default()
        },
    );
    for (config, deadline, expected) in [
        (
            toml::from_str("timezone = \"UTC\"\n[harness]\nbudget = \"5/day\"\n").unwrap(),
            None,
            LoopRunResult::BudgetSkipped,
        ),
        (
            MachineConfig::default(),
            Some(Timestamp::UNIX_EPOCH),
            LoopRunResult::Expired,
        ),
    ] {
        let entry = TaskEntry {
            root: root.path().to_owned(),
            agent: Some("claude,codex".into()),
            prompt: Some("repair".into()),
            stay: true,
            deadline,
            ..Default::default()
        };
        let mut fire = TaskFire::new(
            "resident-gates",
            LoadedTask::new("resident-gates", entry, catalog::TaskSource::Config),
            &catalog,
            LoopRunMode::Manual,
            false,
            now,
            Arc::new(config),
            None,
            Instant::now(),
        )
        .unwrap();
        let plan = fire
            .prepare(&mut |_| panic!("gate must precede room birth"))
            .unwrap();
        assert!(matches!(plan, TaskFirePlan::Done(done) if done.record.result == expected));
    }
}

#[test]
fn at_lock_refusals_store_the_holder_without_freezing_its_age() {
    let holder = RunLockInfo {
        pid: 42424,
        started_at: Timestamp::now() - jiff::SignedDuration::from_secs(25 * 60),
    };
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (mode, policy) in [
        (LoopRunMode::Scheduled, OverlapPolicy::Coalesce),
        (LoopRunMode::Scheduled, OverlapPolicy::Skip),
        (LoopRunMode::Manual, OverlapPolicy::Coalesce),
    ] {
        for named in [true, false] {
            let root = tempfile::tempdir().unwrap();
            record_room(root.path()).unwrap();
            let catalog = TaskCatalog::load(Some(root.path())).unwrap();
            let mut fire = backlog_fire(root.path(), &catalog);
            fire.mode = mode;
            fire.entry.overlap = Some(policy);
            let path = fire.fire_run_lock_path().unwrap();
            std::fs::write(
                &path,
                if named {
                    serde_json::to_vec(&holder).unwrap()
                } else {
                    Vec::new()
                },
            )
            .unwrap();
            let lock = File::open(&path).unwrap();
            lock.try_lock().unwrap();
            let finished = fire.prepare_run_lock().unwrap().unwrap();
            let value = serde_json::to_value(&finished.record).unwrap();
            let TaskFireNotice::Overlap { detail } = finished.notice else {
                panic!("expected overlap notice");
            };
            actual.push((value["holder"].clone(), value["error"].clone(), detail));
            let suffix = if mode == LoopRunMode::Scheduled && policy == OverlapPolicy::Coalesce {
                assert_eq!(finished.record.result, LoopRunResult::Deferred);
                "waiting"
            } else {
                assert_eq!(finished.record.result, LoopRunResult::Overlapped);
                "skipped"
            };
            expected.push((
                if named {
                    serde_json::to_value(holder).unwrap()
                } else {
                    serde_json::Value::Null
                },
                serde_json::json!(format!("previous run still active — {suffix}")),
                if named {
                    Some(format!(
                        "previous run still active (pid 42424, started 25m ago) — {suffix}"
                    ))
                } else if suffix == "waiting" {
                    Some("previous run still active — waiting".into())
                } else {
                    None
                },
            ));
        }
    }
    assert_eq!(actual, expected);
}

#[test]
fn task_lookup_reports_the_earliest_holder_among_its_held_locks() {
    let root = tempfile::tempdir().unwrap();
    record_room(root.path()).unwrap();
    let locks = RuntimePaths::for_project_root(root.path())
        .unwrap()
        .locks_dir;
    std::fs::create_dir_all(&locks).unwrap();
    let write = |file: &str, pid: u32, started_at: i64| {
        let path = locks.join(file);
        let info = RunLockInfo {
            pid,
            started_at: Timestamp::from_second(started_at).unwrap(),
        };
        std::fs::write(&path, serde_json::to_vec(&info).unwrap()).unwrap();
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap()
    };
    let later = write("loop-run-fixer-ws_0123456789abcdef01234567.lock", 11, 200);
    let earlier = write("loop-run-fixer-ws_89abcdef0123456789abcdef.lock", 22, 100);
    let _free = write("loop-run-fixer.lock", 33, 50);
    let other_task = write("loop-run-fixer-nightly.lock", 44, 10);
    other_task.try_lock().unwrap();
    let holder = || match RunLocks::list(root.path()).unwrap().state("fixer").unwrap() {
        RunLockState::Held(holder) => holder.map(|info| info.pid),
        RunLockState::Available => None,
    };

    assert_eq!(holder(), None, "a free lock file is no run");
    later.try_lock().unwrap();
    assert_eq!(holder(), Some(11));
    earlier.try_lock().unwrap();
    assert_eq!(holder(), Some(22));
    let holderless = locks.join("loop-run-fixer-ws_ffffffffffffffffffffffff.lock");
    std::fs::write(&holderless, "").unwrap();
    let holderless = std::fs::File::open(holderless).unwrap();
    holderless.try_lock().unwrap();
    assert_eq!(holder(), Some(22), "a holderless lock sorts last");
}

#[test]
fn held_locks_no_row_claims_are_listed_under_their_whole_stem() {
    let root = tempfile::tempdir().unwrap();
    record_room(root.path()).unwrap();
    let locks = RuntimePaths::for_project_root(root.path())
        .unwrap()
        .locks_dir;
    std::fs::create_dir_all(&locks).unwrap();
    let hold = |file: &str, pid: u32| {
        let path = locks.join(file);
        let info = RunLockInfo {
            pid,
            started_at: Timestamp::from_second(100).unwrap(),
        };
        std::fs::write(&path, serde_json::to_vec(&info).unwrap()).unwrap();
        let file = File::open(path).unwrap();
        file.try_lock().unwrap();
        file
    };
    let fan_out = "fan-ws_0123456789abcdef01234567";
    assert_eq!(
        run_lock_stem(&run_lock_file_name(fan_out, None)),
        Some(fan_out)
    );
    assert_eq!(run_lock_stem("loop-watch-x.lock"), None);
    let _later = hold(&run_lock_file_name("later", None), 11);
    let _fan = hold(&run_lock_file_name(fan_out, None), 22);
    let _watcher = hold("loop-watch-x.lock", 33);
    std::fs::write(locks.join(run_lock_file_name("free", None)), "").unwrap();
    std::fs::create_dir(locks.join(run_lock_file_name("ghost", None))).unwrap();
    let rowless = |rows: &[&str]| {
        RunLocks::list(root.path())
            .unwrap()
            .rowless(rows)
            .into_iter()
            .map(|(name, holder)| (name, holder.map(|info| info.pid)))
            .collect::<Vec<_>>()
    };

    assert_eq!(
        rowless(&[]),
        [
            (fan_out.to_owned(), Some(22)),
            ("later".to_owned(), Some(11))
        ]
    );
    assert_eq!(rowless(&["fan"]), [("later".to_owned(), Some(11))]);
    assert_eq!(
        rowless(&["later", "ghost"]),
        [(fan_out.to_owned(), Some(22))]
    );

    let snapshot = RunLocks::list(root.path()).unwrap();
    assert!(snapshot.state("ghost").is_err());
    assert!(matches!(
        snapshot.state("later").unwrap(),
        RunLockState::Held(Some(info)) if info.pid == 11
    ));
    assert!(
        in_flight_run("later", root.path()).unwrap().is_some(),
        "another name's unopenable lock does not fail the lookup"
    );
}

#[test]
fn vanished_delivery_root_still_resolves_and_finds_no_active_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("vanished");
    std::fs::create_dir(&root).unwrap();
    record_room(&root).unwrap();
    std::fs::remove_dir(&root).unwrap();
    let entry = TaskEntry {
        root: root.clone(),
        ..TaskEntry::default()
    };
    let target = TaskTarget {
        kind: crate::ids::AgentKind::new_unchecked("claude"),
        session: "session".into(),
        handle: "@claude".to_owned(),
    };

    let context = FireContext::resolve(&entry, TaskAction::Deliver(target))
        .expect("resolve persisted delivery root");

    assert!(context.scope.is_some());
    assert!(
        in_flight_run("vanished-root", &root)
            .expect("probe the vanished root's run lock")
            .is_none()
    );
}

#[test]
fn loop_check_identity_excludes_agent_and_room_environments() {
    assert_eq!(
        check_task_from_identity(Some("nightly".to_owned()), false).as_deref(),
        Some("nightly")
    );
    assert_eq!(
        check_task_from_identity(Some("nightly".to_owned()), true),
        None
    );
    assert_eq!(check_task_from_identity(Some(String::new()), false), None);
    assert_eq!(check_task_from_identity(None, false), None);
}

#[test]
fn spawn_timeout_prefers_task_then_config_then_builtin() {
    use LoopRunMode::{Manual, Scheduled};
    let task = Duration::from_secs(30);
    let configured = Duration::from_secs(60);

    assert_eq!(
        effective_spawn_timeout(Scheduled, Some(task), Some(configured)),
        Some(task),
        "task timeout outranks config"
    );
    assert_eq!(
        effective_spawn_timeout(Scheduled, Some(task), None),
        Some(task),
        "task timeout applies without a configured default"
    );
    assert_eq!(
        effective_spawn_timeout(Scheduled, None, Some(configured)),
        Some(configured),
        "config timeout applies when the task is silent"
    );
    assert_eq!(
        effective_spawn_timeout(Scheduled, None, None),
        Some(SCHEDULED_RUN_DEFAULT_TIMEOUT),
        "scheduled runs always carry a deadline"
    );
    assert_eq!(
        effective_spawn_timeout(Manual, None, Some(configured)),
        None,
        "manual runs stay untimed even with a configured default"
    );
}

#[test]
fn deadline_expiry_uses_injected_time() {
    let now = Timestamp::from_second(200_000).expect("now");
    let expired = TaskEntry {
        deadline: Some(Timestamp::from_second(199_999).expect("deadline")),
        ..TaskEntry::default()
    };

    assert!(deadline_expired_at(&expired, now));
}

fn surplus_entry(surplus: Option<&str>, surplus_after: Option<&str>) -> TaskEntry {
    TaskEntry {
        surplus: surplus.map(ToOwned::to_owned),
        surplus_after: surplus_after.map(ToOwned::to_owned),
        ..TaskEntry::default()
    }
}

fn reading(elapsed_days: i64, headroom: f64) -> WindowSurplus {
    WindowSurplus {
        duration_mins: 7 * 24 * 60,
        elapsed: jiff::SignedDuration::from_secs(elapsed_days * 86_400),
        headroom,
    }
}

#[test]
fn surplus_gate_covers_every_branch() {
    // Reason `surplus`/`surplus-after` gave for holding a fire back, if any.
    let gate = |surplus, after, reading| surplus_gate_in(surplus, after, "claude", reading);

    assert_eq!(
        surplus_gate_in(None, None, "claude", None),
        None,
        "an ungated task never consults the window"
    );
    assert_eq!(
        gate(Some("1.5x"), None, None).as_deref(),
        Some("no claude budget-window reading; surplus gate stays closed"),
        "a gate with no reading fails closed"
    );
    assert_eq!(
        gate(Some("1.5x"), Some("3d"), Some(reading(2, 2.0))).as_deref(),
        Some("claude 7d window 2d elapsed; fires after 3d"),
        "ample headroom still waits for the elapsed floor"
    );
    assert_eq!(
        gate(Some("1.5x"), Some("3d"), Some(reading(4, 1.4))).as_deref(),
        Some("claude 7d window surplus 1.4x below 1.5x")
    );
    assert_eq!(
        gate(Some("1.5x"), Some("3d"), Some(reading(4, 1.5))),
        None,
        "headroom exactly at the threshold fires"
    );
    assert_eq!(
        surplus_gate_in(None, Some("3d"), "codex", Some(reading(4, 0.9))).as_deref(),
        Some("codex 7d window surplus 0.9x below 1.0x"),
        "a bare elapsed floor still demands sustainable headroom"
    );
}

#[test]
fn surplus_reads_each_cell_of_a_multi_cell_step_instead_of_refusing_it() {
    let root = tempfile::tempdir().unwrap();
    record_room(root.path()).unwrap();
    let workflow = format!("cells-{}", crate::ids::WorkflowRunId::new());
    let dir = crate::disk::paths::workflows_dir().join(&workflow);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        crate::disk::paths::workflow_spec_file(&dir),
        "[workflow]\nscope='worktree'\nstart='fix'\n[steps.fix]\nagent='claude,codex'\nprompt='fix'\nthen='@done'\n",
    )
    .unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    let entry = TaskEntry {
        root: root.path().to_owned(),
        workflow: Some(workflow),
        surplus: Some("1.5x".into()),
        ..Default::default()
    };
    let mut fire = TaskFire::new(
        "cells",
        LoadedTask::new("cells", entry, catalog::TaskSource::Config),
        &catalog,
        LoopRunMode::Manual,
        false,
        Timestamp::now(),
        Arc::new(MachineConfig::default()),
        None,
        Instant::now(),
    )
    .unwrap();
    let plan = fire.prepare(&mut |_| Ok(()));
    std::fs::remove_dir_all(&dir).unwrap();
    let reason = match &plan {
        Ok(TaskFirePlan::Done(done)) if done.record.result == LoopRunResult::SurplusSkipped => {
            done.record.error.clone()
        }
        _ => None,
    };
    assert_eq!(
        reason.as_deref(),
        Some("no claude budget-window reading; surplus gate stays closed"),
        "{:?}",
        plan.err()
    );
}

#[test]
fn an_over_cap_workflow_fire_records_its_skip_before_birthing_a_room() {
    let root = tempfile::tempdir().unwrap();
    let workflow = format!("over-cap-{}", crate::ids::WorkflowRunId::new());
    let dir = crate::disk::paths::workflows_dir().join(&workflow);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        crate::disk::paths::workflow_spec_file(&dir),
        "[workflow]\nscope='worktree'\nstart='fix'\n[steps.fix]\nagent='claude'\nprompt='fix'\nbudget='$6'\nthen='@done'\n",
    )
    .unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    let entry = TaskEntry {
        root: root.path().to_owned(),
        workflow: Some(workflow),
        budget_per_day: Some("$5".into()),
        ..Default::default()
    };
    let mut fire = TaskFire::new(
        "over-cap",
        LoadedTask::new("over-cap", entry, catalog::TaskSource::Config),
        &catalog,
        LoopRunMode::Scheduled,
        false,
        Timestamp::now(),
        Arc::new(MachineConfig::default()),
        None,
        Instant::now(),
    )
    .unwrap();
    let mut births = 0;
    let plan = fire.prepare(&mut |_| {
        births += 1;
        Ok(())
    });
    std::fs::remove_dir_all(&dir).unwrap();
    let skipped = matches!(
        &plan,
        Ok(TaskFirePlan::Done(done)) if done.record.result == LoopRunResult::BudgetSkipped
    );
    assert_eq!((skipped, births), (true, 0), "{:?}", plan.err());
}

#[test]
fn month_surplus_reasons_use_the_window_label() {
    let reasons: Vec<_> = [(None, Some("3d")), (Some("1.5x"), None)]
        .into_iter()
        .map(|(surplus, after)| {
            surplus_gate_in(
                surplus,
                after,
                "qwen",
                Some(WindowSurplus {
                    duration_mins: 43_200,
                    ..reading(2, 0.9)
                }),
            )
        })
        .collect();
    assert_eq!(
        reasons,
        [
            Some("qwen mo window 2d elapsed; fires after 3d".to_owned()),
            Some("qwen mo window surplus 0.9x below 1.5x".to_owned())
        ]
    );
}

#[test]
fn run_lock_probes_never_defeat_a_starting_run() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("task.lock");
    std::fs::write(&path, b"").unwrap();
    let state = assert_waits_for_gate(&path, || probe_run_lock_path(&path)).unwrap();
    assert!(matches!(state, RunLockState::Available));
    let claim = assert_waits_for_gate(&path, || acquire_run_lock_file(&path, None, None)).unwrap();
    assert!(matches!(claim, RunLockAttempt::Acquired(_)));
}

#[test]
fn run_lock_reports_holder_metadata_and_accepts_empty_legacy_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing_path = dir.path().join("missing.lock");
    assert!(matches!(
        probe_run_lock_path(&missing_path).expect("probe missing lock"),
        RunLockState::Available
    ));
    assert!(
        !missing_path.exists(),
        "probing should not create a lock file"
    );

    let path = dir.path().join("task.lock");
    let guard = match acquire_run_lock_file(&path, None, None).expect("acquire lock") {
        RunLockAttempt::Acquired(guard) => guard,
        RunLockAttempt::Held(_) => panic!("fresh lock should be acquired"),
    };
    let written: RunLockInfo =
        serde_json::from_slice(&std::fs::read(&path).expect("read lock")).expect("parse lock info");
    assert_eq!(written.pid, std::process::id());

    match acquire_run_lock_file(&path, None, None).expect("contend for lock") {
        RunLockAttempt::Held(Some(info)) => assert_eq!(info, written),
        RunLockAttempt::Held(None) => panic!("holder metadata should be readable"),
        RunLockAttempt::Acquired(_) => panic!("held lock should reject contender"),
    }

    drop(guard);
    let before_probe = std::fs::read(&path).expect("read lock before probe");
    assert!(matches!(
        probe_run_lock_path(&path).expect("probe available lock"),
        RunLockState::Available
    ));
    assert_eq!(
        std::fs::read(&path).expect("read lock after probe"),
        before_probe,
        "probing an available lock should not rewrite its metadata"
    );

    let empty_path = dir.path().join("legacy.lock");
    let holder = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&empty_path)
        .expect("open lock");
    holder.try_lock().expect("hold empty lock");
    assert!(
        matches!(
            probe_run_lock_path(&empty_path).expect("probe empty lock"),
            RunLockState::Held(None)
        ),
        "a held lock with no metadata is still held"
    );
}

#[test]
fn wait_for_run_lock_release_observes_guard_drop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("task.lock");
    let guard = match acquire_run_lock_file(&path, None, None).expect("acquire lock") {
        RunLockAttempt::Acquired(guard) => guard,
        RunLockAttempt::Held(_) => panic!("fresh lock should be acquired"),
    };
    let holder = Some(guard.info);
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        drop(guard);
    });

    assert!(
        wait_for_run_lock_release_path(&path, holder, Duration::from_secs(1))
            .expect("wait for release")
    );
    releaser.join().expect("release thread");
}

#[test]
fn wait_for_process_exit_reads_the_holder_dead_within_the_grace() {
    let mut exited = std::process::Command::new("true")
        .spawn()
        .expect("spawn true");
    exited.wait().expect("reap true");
    assert!(wait_for_process_exit(exited.id(), Duration::ZERO));

    let mut live = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep");
    let waited = wait_for_process_exit(live.id(), Duration::from_millis(50));
    live.kill().expect("kill sleep");
    live.wait().expect("reap sleep");
    assert!(!waited, "a live process outlasts the grace");
}

#[test]
fn check_polarity_truth_table() {
    let outcome = |passed, timed_out, code| CheckOutcome {
        passed,
        timed_out,
        interrupted: false,
        terminated: false,
        output: String::new(),
        code,
        stderr_line: String::new(),
    };
    let passed = outcome(true, false, Some(0));
    let failed = outcome(false, false, Some(1));
    let timed_out = outcome(false, true, None);

    assert!(!polarity_fires(Some(CheckOn::Fail), &passed));
    assert!(polarity_fires(Some(CheckOn::Fail), &failed));
    assert!(polarity_fires(Some(CheckOn::Fail), &timed_out));
    assert!(polarity_fires(Some(CheckOn::Success), &passed));
    assert!(!polarity_fires(Some(CheckOn::Success), &failed));
    assert!(!polarity_fires(Some(CheckOn::Success), &timed_out));
}

#[test]
fn a_scheduled_gate_skip_removes_a_fire_at_row_and_leaves_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    record_room(dir.path()).unwrap();
    let state = StatePaths::for_project_root(dir.path()).unwrap();
    use LoopRunResult::{AccountSkipped, SurplusSkipped};
    for (name, fire_at, mode, result, kept) in [
        (
            "after-reset",
            true,
            LoopRunMode::Scheduled,
            SurplusSkipped,
            false,
        ),
        (
            "pinned",
            true,
            LoopRunMode::Scheduled,
            AccountSkipped,
            false,
        ),
        (
            "bare-at",
            false,
            LoopRunMode::Scheduled,
            SurplusSkipped,
            true,
        ),
        (
            "manual-run",
            true,
            LoopRunMode::Manual,
            SurplusSkipped,
            true,
        ),
    ] {
        let entry = TaskEntry {
            check: Some("true".into()),
            root: dir.path().to_path_buf(),
            at: (!fire_at).then(|| "07:00".to_owned()),
            fire_at: fire_at.then_some(Timestamp::UNIX_EPOCH),
            ..TaskEntry::default()
        };
        crate::harness::schedule::instances::insert(&state, name, &entry).unwrap();
        let catalog = TaskCatalog::load(Some(dir.path())).unwrap();
        let mut fire = skipped_fire(name, &catalog, None);
        fire.mode = mode;
        let finished = fire.record_gate(result, "no surplus".to_owned());
        assert_eq!(finished.record.result, result, "{name}");
        assert!(
            matches!(finished.notice, TaskFireNotice::Gate { .. }),
            "{name}"
        );
        assert_eq!(
            crate::harness::schedule::instances::load_from(&state.root)
                .0
                .contains_key(name),
            kept,
            "{name}"
        );
    }
}

fn skipped_fire<'a>(
    name: &str,
    catalog: &'a TaskCatalog,
    signal: Option<TriggerSignal>,
) -> TaskFire<'a> {
    let task = catalog.for_run(name).cloned().expect("loaded fixture");
    let root = task.entry().resolved_root();
    let mut fire = TaskFire::new(
        name,
        task,
        catalog,
        LoopRunMode::Scheduled,
        false,
        Timestamp::now(),
        Arc::new(MachineConfig::default()),
        signal,
        Instant::now(),
    )
    .expect("construct task fire");
    fire.context = Some(FireContext { root, scope: None });
    fire
}

#[test]
fn check_room_hook_precedes_the_run_and_carries_the_rows_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let project_root = dir.path().canonicalize().unwrap();
    record_room(&project_root).unwrap();
    let catalog = TaskCatalog::load(Some(&project_root)).unwrap();
    let worktree = tempfile::tempdir().unwrap();
    let worktree_root = worktree.path().canonicalize().unwrap();
    let entry = TaskEntry {
        check: Some("true".into()),
        root: project_root.clone(),
        dir: Some(worktree_root.clone()),
        every: Some("1m".to_owned()),
        ..TaskEntry::default()
    };
    let mut fire = TaskFire::new(
        "room-check",
        LoadedTask::new("room-check", entry, catalog::TaskSource::Config),
        &catalog,
        LoopRunMode::Scheduled,
        false,
        Timestamp::now(),
        Arc::new(MachineConfig::default()),
        None,
        Instant::now(),
    )
    .unwrap();
    let mut calls = 0;
    let TaskFirePlan::Done(done) = fire
        .prepare(&mut |root| {
            calls += 1;
            assert_eq!(root, &project_root);
            Ok(())
        })
        .unwrap()
    else {
        panic!("a check row starts a workflow run")
    };
    assert_eq!(calls, 1);
    assert_eq!(done.record.result, LoopRunResult::Launched);
    let state = StatePaths::for_project_root(&project_root).unwrap();
    let run = crate::store::workflow::list(&state)
        .unwrap()
        .into_iter()
        .find(|run| done.record.run_id.as_deref() == Some(run.id.as_str()))
        .expect("the launched run is recorded");
    assert_eq!(run.task.as_deref(), Some("room-check"));
    assert_eq!(
        run.scope.path, worktree_root,
        "the row's dir is the run's checkout"
    );
}

#[test]
fn a_fired_run_keeps_its_rows_surplus_gate_for_inline_and_named_workflows() {
    let workflow = format!("surplus-{}", crate::ids::WorkflowRunId::new());
    let dir = crate::disk::paths::workflows_dir().join(&workflow);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        crate::disk::paths::workflow_spec_file(&dir),
        "[workflow]\nscope='worktree'\nstart='check'\n[checks.check]\nshell='true'\n\
         on={pass='@done',fail='@failed',invalid='@failed'}\n",
    )
    .unwrap();
    let mut kept = Vec::new();
    for named in [false, true] {
        for gated in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let project_root = root.path().canonicalize().unwrap();
            record_room(&project_root).unwrap();
            let catalog = TaskCatalog::load(Some(&project_root)).unwrap();
            let entry = TaskEntry {
                root: project_root.clone(),
                check: (!named).then(|| "true".into()),
                workflow: named.then(|| workflow.clone()),
                every: Some("1m".to_owned()),
                surplus: gated.then(|| "1.5x".to_owned()),
                surplus_after: gated.then(|| "2h".to_owned()),
                ..TaskEntry::default()
            };
            let mut fire = TaskFire::new(
                "gated",
                LoadedTask::new("gated", entry, catalog::TaskSource::Config),
                &catalog,
                LoopRunMode::Scheduled,
                false,
                Timestamp::now(),
                Arc::new(MachineConfig::default()),
                None,
                Instant::now(),
            )
            .unwrap();
            let plan = fire.prepare(&mut |_| Ok(()));
            let Ok(TaskFirePlan::Done(done)) = plan else {
                panic!("named={named}: {:?}", plan.err());
            };
            let state = StatePaths::for_project_root(&project_root).unwrap();
            let run = crate::store::workflow::list(&state)
                .unwrap()
                .into_iter()
                .find(|run| done.record.run_id.as_deref() == Some(run.id.as_str()))
                .unwrap_or_else(|| panic!("named={named}: {:?}", done.record));
            kept.push((named, run.surplus, run.surplus_after));
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
    let (surplus, after) = (Some("1.5x".to_owned()), Some("2h".to_owned()));
    assert_eq!(
        kept,
        [
            (false, surplus.clone(), after.clone()),
            (false, None, None),
            (true, surplus, after),
            (true, None, None),
        ]
    );
}

#[test]
fn check_room_hook_is_after_lock_and_deadline_and_records_failure() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(dir.path())).unwrap();
    let entry = TaskEntry {
        check: Some("touch check-ran".into()),
        root: dir.path().to_path_buf(),
        every: Some("1m".to_owned()),
        ..TaskEntry::default()
    };
    let make_fire = |entry: TaskEntry| {
        let mut fire = TaskFire::new(
            "room-gates",
            LoadedTask::new("room-gates", entry, catalog::TaskSource::Config),
            &catalog,
            LoopRunMode::Manual,
            false,
            Timestamp::now(),
            Arc::new(MachineConfig::default()),
            None,
            Instant::now(),
        )
        .unwrap();
        fire.run_lock_path = |file, entry| {
            let runtime =
                RuntimePaths::under(WorkspaceId::from_project_root(&entry.root), &entry.root)?;
            Ok(runtime.lock_path(file))
        };
        fire
    };
    let mut fire = make_fire(entry.clone());
    let error = fire
        .prepare(&mut |_| bail!("room unavailable"))
        .unwrap_err();
    let mut overlap = make_fire(entry.clone());
    assert!(
        matches!(overlap.prepare(&mut |_| panic!("lock refuses before room birth")).unwrap(), TaskFirePlan::Done(done) if done.record.result == LoopRunResult::Overlapped)
    );
    drop(overlap);
    assert_eq!(
        fire.finish_error(&error).record.result,
        LoopRunResult::Errored
    );
    drop(fire);
    let mut expired = make_fire(TaskEntry {
        deadline: Some(Timestamp::UNIX_EPOCH),
        ..entry.clone()
    });
    assert!(
        matches!(expired.prepare(&mut |_| panic!("expired before room birth")).unwrap(), TaskFirePlan::Done(done) if done.record.result == LoopRunResult::Expired)
    );
    assert!(!dir.path().join("check-ran").exists());

    drop(expired);
    let vanished = dir.path().join("vanished-worktree");
    std::fs::create_dir(&vanished).unwrap();
    let mut fire = make_fire(TaskEntry {
        dir: Some(vanished.clone()),
        ..entry
    });
    std::fs::remove_dir(&vanished).unwrap();
    let error = fire.prepare(&mut |_| Ok(())).unwrap_err();
    assert!(
        format!("{error:#}").contains(&vanished.to_string_lossy().to_string()),
        "{error:#}"
    );
    assert_eq!(
        fire.finish_error(&error).record.result,
        LoopRunResult::Errored
    );
    assert!(!dir.path().join("check-ran").exists());
}

#[test]
fn run_check_captures_output_status_and_timeout() {
    let dir = tempfile::tempdir().expect("tempdir");
    let check = |cmd: &str, timeout| {
        run_check(
            dir.path(),
            cmd,
            timeout,
            CheckEcho::Capture,
            &BTreeMap::new(),
        )
        .expect("check ran")
    };

    let passed = check("printf out; printf err >&2", Duration::from_secs(1));
    assert!(passed.passed);
    assert_eq!(passed.code, Some(0));
    assert!(passed.output.contains("out"), "stdout is captured");
    assert!(passed.output.contains("err"), "stderr is captured too");
    assert_eq!(passed.stderr_line(), "err");

    let failed = check("printf nope; exit 1", Duration::from_secs(1));
    assert!(!failed.passed);
    assert!(!failed.timed_out);
    assert_eq!(failed.code, Some(1));
    assert!(failed.output.contains("nope"));

    let expired = check("(sleep 1; printf leaked) & wait", Duration::from_millis(50));
    assert!(!expired.passed);
    assert!(expired.timed_out);
    assert!(
        expired.output.is_empty(),
        "timed-out descendants cannot keep writing to the check pipes"
    );
    let orphan = check(
        "( (sleep 1; printf orphaned) & ); sleep 30",
        Duration::from_millis(50),
    );
    assert!(orphan.timed_out);
    assert!(
        orphan.output.is_empty(),
        "a reparented pipe holder cannot prevent timeout recording"
    );
}

#[test]
fn hosted_check_publishes_its_process_group_and_tees_both_streams() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("check.log");
    let mut published = None;
    let output = run_command(
        dir.path(),
        "printf out; printf err >&2; exit 7",
        WatchDeadline::Hosted {
            argv: None,
            interrupted: &|| false,
            spawned: &mut |pid| {
                assert_eq!(
                    nix::unistd::getpgid(Some(Pid::from_raw(pid as i32)))?.as_raw(),
                    pid as i32
                );
                published = Some(pid);
                Ok(())
            },
        },
        CheckEcho::Tee {
            file: File::create(&path).unwrap(),
        },
        &BTreeMap::new(),
        |_, _| panic!("hosted checks have no check-in"),
        &mut None,
    );
    assert!(
        output.is_ok(),
        "hosted checks must run and publish their PID"
    );
    let output = output.unwrap();
    assert!(published.is_some());
    assert_eq!(output.code, Some(7));
    assert!(!output.passed);
    assert!(!output.timed_out);
    assert!(!output.interrupted);
    let log = std::fs::read_to_string(path).unwrap();
    assert!(log.contains("out") && log.contains("err"), "{log}");
}

#[test]
fn hosted_check_argv_preserves_literal_words_without_a_shell() {
    let dir = tempfile::tempdir().unwrap();
    let argv: Vec<std::ffi::OsString> = [
        "sh",
        "-c",
        "printf '%s\\n' \"$@\"; test ! -t 0",
        "script",
        "two words",
        "$HOME",
        "*",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    let output = run_command(
        dir.path(),
        "printf shell-was-used",
        WatchDeadline::Hosted {
            argv: Some(&argv),
            interrupted: &|| false,
            spawned: &mut |_| Ok(()),
        },
        CheckEcho::Capture,
        &BTreeMap::new(),
        |_, _| panic!("hosted commands have no check-in"),
        &mut None,
    )
    .unwrap();
    assert_eq!(output.output, "two words\n$HOME\n*\n");
    assert!(output.passed, "{}", output.output);
}

#[test]
fn hosted_check_keeps_an_exited_childs_status_when_the_interrupt_is_due() {
    let dir = tempfile::tempdir().unwrap();
    let output = run_command(
        dir.path(),
        "exec >/dev/null 2>&1; sleep 0.1; exit 7",
        WatchDeadline::Hosted {
            argv: None,
            interrupted: &|| true,
            spawned: &mut |pid| {
                let deadline = Instant::now() + Duration::from_secs(5);
                while crate::proc::process_is_live(pid, None) {
                    assert!(Instant::now() < deadline, "child {pid} did not exit");
                    std::thread::sleep(CHECK_POLL_INTERVAL);
                }
                Ok(())
            },
        },
        CheckEcho::Capture,
        &BTreeMap::new(),
        |_, _| panic!("hosted commands have no check-in"),
        &mut None,
    )
    .unwrap();
    assert_eq!(output.code, Some(7));
    assert!(!output.interrupted, "an exited child was not interrupted");
}

#[test]
fn hosted_check_interrupt_kills_the_group_without_a_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let polls = std::cell::Cell::new(0);
    let mut published = None;
    let output = run_command(
        dir.path(),
        "sleep 60 & echo $! > descendant.pid; wait",
        WatchDeadline::Hosted {
            argv: None,
            interrupted: &|| {
                polls.set(polls.get() + 1);
                polls.get() >= 3
                    && std::fs::read_to_string(dir.path().join("descendant.pid"))
                        .ok()
                        .is_some_and(|pid| pid.trim().parse::<u32>().is_ok())
            },
            spawned: &mut |pid| {
                published = Some(pid);
                Ok(())
            },
        },
        CheckEcho::Capture,
        &BTreeMap::new(),
        |_, _| panic!("hosted checks have no check-in"),
        &mut None,
    );
    assert!(output.is_ok(), "hosted interruption must return an outcome");
    let output = output.unwrap();
    assert!(output.interrupted);
    assert!(!output.timed_out);
    assert!(!output.passed);
    assert!(!crate::proc::process_is_live(published.unwrap(), None));
    let descendant = std::fs::read_to_string(dir.path().join("descendant.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        wait_for_process_exit(descendant, Duration::from_secs(2)),
        "descendant {descendant} outlived the group kill"
    );
}

#[test]
fn hosted_check_interrupts_pipe_holders_after_the_shell_exits() {
    let dir = tempfile::tempdir().unwrap();
    let polls = std::cell::Cell::new(0);
    let output = run_command(
        dir.path(),
        "sleep 60 & echo $! > descendant.pid",
        WatchDeadline::Hosted {
            argv: None,
            interrupted: &|| {
                polls.set(polls.get() + 1);
                polls.get() >= 3
                    && std::fs::read_to_string(dir.path().join("descendant.pid"))
                        .ok()
                        .is_some_and(|pid| pid.trim().parse::<u32>().is_ok())
            },
            spawned: &mut |_| Ok(()),
        },
        CheckEcho::Capture,
        &BTreeMap::new(),
        |_, _| panic!("hosted checks have no check-in"),
        &mut None,
    );
    assert!(
        output.is_ok(),
        "hosted checks must keep polling while draining"
    );
    let output = output.unwrap();
    assert!(output.interrupted);
    assert!(!output.passed);
    let descendant = std::fs::read_to_string(dir.path().join("descendant.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        wait_for_process_exit(descendant, Duration::from_secs(2)),
        "descendant {descendant} outlived the group kill"
    );
}

#[test]
fn hosted_check_publication_error_kills_the_group_and_returns_the_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut published = None;
    let output = run_command(
        dir.path(),
        "sleep 60 & echo $! > descendant.pid; wait",
        WatchDeadline::Hosted {
            argv: None,
            interrupted: &|| false,
            spawned: &mut |pid| {
                published = Some(pid);
                let deadline = Instant::now() + Duration::from_secs(5);
                while !std::fs::read_to_string(dir.path().join("descendant.pid"))
                    .ok()
                    .is_some_and(|pid| pid.trim().parse::<u32>().is_ok())
                {
                    assert!(Instant::now() < deadline, "child did not start");
                    std::thread::sleep(CHECK_POLL_INTERVAL);
                }
                Err(std::io::Error::other("publication refused"))
            },
        },
        CheckEcho::Capture,
        &BTreeMap::new(),
        |_, _| panic!("hosted checks have no check-in"),
        &mut None,
    );
    assert!(
        published.is_some(),
        "hosted checks must call the publication hook"
    );
    assert!(output.is_err_and(|error| format!("{error:#}").contains("publication refused")));
    assert!(!crate::proc::process_is_live(published.unwrap(), None));
    let descendant = std::fs::read_to_string(dir.path().join("descendant.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        wait_for_process_exit(descendant, Duration::from_secs(2)),
        "descendant {descendant} outlived the group kill"
    );
}

#[test]
fn polled_watch_probe_runs_to_exit_without_checkin() {
    let dir = tempfile::tempdir().unwrap();
    let output = run_command(
        dir.path(),
        "printf probe; exit 3",
        WatchDeadline::Watch(None),
        CheckEcho::Capture,
        &BTreeMap::new(),
        |_, _| panic!("a probe never checks in"),
        &mut None,
    )
    .unwrap();
    assert_eq!(output.code, Some(3));
    assert_eq!(output.output, "probe");
    assert!(!output.timed_out);
}

#[test]
fn watch_exit_during_checkin_delivery_is_not_lost() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("watch.log");
    let mut notices = 0;
    let output = run_command(
        dir.path(),
        "printf interim; printf '%s' \"$$\" > command.pid; while [ ! -e release ]; do sleep 0.01; done; printf final; exit 3",
        WatchDeadline::Watch(Some(Duration::from_millis(100))),
        CheckEcho::Tee { file: File::create(&path).unwrap() },
        &BTreeMap::new(),
        |elapsed_ms, tail| {
            notices += 1;
            assert!(elapsed_ms >= 100);
            assert_eq!(tail, "interim");
            std::fs::write(dir.path().join("release"), "").unwrap();
            let pid = std::fs::read_to_string(dir.path().join("command.pid")).unwrap().parse().unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while crate::proc::process_is_live(pid, None) {
                assert!(Instant::now() < deadline, "command did not exit during notice");
                std::thread::sleep(Duration::from_millis(10));
            }
        },
        &mut None,
    ).unwrap();
    assert_eq!(notices, 1);
    assert_eq!(output.code, Some(3));
    assert!(!output.timed_out);
    assert_eq!(output.output, "interimfinal");
    assert_eq!(std::fs::read_to_string(path).unwrap(), output.output);
}

#[test]
fn watched_check_keeps_full_output_and_a_bounded_tail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("watch.log");
    let output = run_check(
        dir.path(),
        "seq 1 5000; printf stderr >&2",
        Duration::from_secs(5),
        CheckEcho::Tee {
            file: File::create(&path).unwrap(),
        },
        &BTreeMap::new(),
    )
    .unwrap();
    let full = std::fs::read_to_string(path).unwrap();
    assert!(output.passed());
    assert!(full.contains("1\n2\n3\n"));
    assert!(full.contains("4999\n5000\n"));
    assert!(full.contains("stderr"));
    assert!(full.len() > WAIT_TAIL_CAP);
    assert!(output.output.len() <= WAIT_TAIL_CAP);
    assert!(full.ends_with(&output.output));
    assert!(output.output.contains("5000"));
}

#[test]
fn check_capture_bounds_chatty_chunks_before_decoding() {
    let mut capture = CheckCapture {
        file: None,
        tail: Vec::new(),
        cap: WAIT_TAIL_CAP,
    };
    for chunk in [
        vec![b'a'; WAIT_TAIL_CAP * 3],
        vec![b'b'; 17],
        vec![b'c'; WAIT_TAIL_CAP],
    ] {
        capture.push(&chunk).unwrap();
        assert!(capture.tail.len() <= WAIT_TAIL_CAP);
    }
    assert_eq!(capture.tail, vec![b'c'; WAIT_TAIL_CAP]);
}

#[test]
fn capacity_first_stderr_line_truncates_at_a_character_boundary() {
    let line = format!("{}é rest\n", "x".repeat(1023));
    let capture = Arc::new(Mutex::new(CheckCapture {
        file: None,
        tail: Vec::new(),
        cap: CHECK_OUTPUT_CAP,
    }));
    let first = drain_pipe(Some(std::io::Cursor::new(line.into_bytes())), capture, None)
        .join()
        .unwrap()
        .unwrap();
    assert_eq!(first, "x".repeat(1023));
}

#[test]
fn pipe_forward_buffers_partial_lines_and_terminates_the_tail() {
    let mut pending = b"first".to_vec();
    assert_eq!(take_complete_line(&mut pending), None);

    pending.extend_from_slice(b" line\nsecond");
    assert_eq!(
        take_complete_line(&mut pending),
        Some(b"first line\n".to_vec())
    );
    assert_eq!(pending, b"second");
    assert_eq!(take_trailing_line(&mut pending), Some(b"second\n".to_vec()));
    assert!(pending.is_empty());
}

#[test]
fn wait_prompt_is_optional_but_spawn_prompt_is_required() {
    let mut entry = TaskEntry {
        wait: Some(TaskTarget {
            kind: crate::ids::AgentKind::new_unchecked("claude"),
            session: "session".into(),
            handle: "@coder".to_owned(),
        }),
        ..TaskEntry::default()
    };
    assert_eq!(resolve_task_prompt("wait-test", &entry).unwrap(), "");
    entry.wait = None;
    entry.agent = Some("claude".to_owned());
    assert!(
        resolve_task_prompt("spawn-test", &entry)
            .unwrap_err()
            .to_string()
            .contains("loop task `spawn-test` has no prompt")
    );
}

#[test]
fn condition_evidence_reaches_prompt_and_terminal_record() {
    let entry = TaskEntry {
        agent: Some("claude".to_owned()),
        when: Some(vec!["team.stage=Done".to_owned()]),
        prompt: Some("Inspect {{branch}}".to_owned()),
        ..TaskEntry::default()
    };
    let catalog = TaskCatalog::load(None).unwrap();
    let evidence = super::super::when::ConditionEvidence {
        since: None,
        when: "team.stage=Done".to_owned(),
        hold: Some("30m".to_owned()),
        held_ms: 1_800_000,
        readings: std::collections::BTreeMap::from([
            ("ci.log".to_owned(), Some("red\nlog".to_owned())),
            ("pr.state".to_owned(), None),
            ("team.stage".to_owned(), Some("Done".to_owned())),
        ]),
    };
    let mut fire = TaskFire::new(
        "ship",
        LoadedTask::new("ship", entry, catalog::TaskSource::Config),
        &catalog,
        LoopRunMode::Scheduled,
        false,
        Timestamp::now(),
        Arc::new(MachineConfig::default()),
        None,
        Instant::now(),
    )
    .unwrap()
    .with_condition(Some(evidence.clone()));
    assert_eq!(
        fire.resolve_effect_prompt().unwrap(),
        "Rule `ship` fired here. ci.log: \"red\\nlog\" · pr.state: unknown · team.stage: Done\n\nInspect {{branch}}"
    );
    let record = fire.terminal_record(LoopRunResult::Completed);
    assert_eq!(
        record.kind,
        Some(crate::harness::schedule::TaskKind::Routine)
    );
    assert_eq!(
        serde_json::to_value(&record).unwrap()["condition"],
        serde_json::to_value(&evidence).unwrap()
    );
    fire.condition = None;
    assert_eq!(fire.resolve_effect_prompt().unwrap(), "Inspect {{branch}}");
}

#[test]
fn vanished_task_root_keeps_its_persisted_workspace_identity() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("vanished");

    let (workspace, project_root) = stop_workspace(&root).expect("resolve stop workspace");

    assert!(workspace.is_none());
    assert_eq!(
        WorkspaceId::from_project_root(&project_root),
        WorkspaceId::from_project_root(&root)
    );
}

fn publish_windows(runtime: &RuntimePaths, kind: &str, windows: Vec<RateLimitWindow>) {
    publish_windows_for(
        runtime,
        crate::ids::LoginKey::default_for(AgentKind::new_unchecked(kind)),
        windows,
    );
}

fn publish_windows_for(
    runtime: &RuntimePaths,
    key: crate::ids::LoginKey,
    windows: Vec<RateLimitWindow>,
) {
    crate::disk::atomic::write_temp_then_rename_cache(
        &crate::agents::account::RateLimitsCache::shared_path(runtime),
        &crate::agents::RateLimitsCache {
            entries: std::collections::BTreeMap::from([(
                key.into(),
                crate::agents::RateLimitCacheEntry {
                    limits: crate::agents::AgentRateLimits { windows },
                    ..Default::default()
                },
            )]),
            ..Default::default()
        },
    )
    .unwrap();
}

#[test]
fn window_triggers_refuse_at_add_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();
    let now = Timestamp::now();
    let window = |used, resets_in: i64, span: WindowSpan| RateLimitWindow {
        used_percentage: used,
        resets_at: Some(now + jiff::SignedDuration::from_secs(resets_in)),
        duration_mins: Some(span.minutes()),
        ..RateLimitWindow::default()
    };
    let native = crate::agents::RoomLoginSet::new(
        Some(Default::default()),
        None,
        std::collections::BTreeMap::new(),
    );
    let at_add = |kind: Option<&str>, span, logins: &crate::agents::RoomLoginSet| {
        let key = kind.and_then(|kind| logins.default_key(kind));
        window_at_add(kind, span, key.as_ref(), dir.path(), &runtime, now)
    };
    let five = WindowSpan::FiveHour;
    let seven = WindowSpan::SevenDay;
    assert_eq!(at_add(None, five, &native), Err(WindowRefusal::NoProvider));
    let refusal = at_add(Some("qwen"), five, &native).unwrap_err();
    assert!(matches!(refusal, WindowRefusal::ManagedAccount { .. }));
    assert!(
        refusal.to_string().contains("do not support qwen"),
        "{refusal}"
    );
    let unresolvable = crate::agents::RoomLoginSet::new(
        Some(
            std::collections::BTreeMap::from([(
                AgentKind::new_unchecked("claude"),
                "work".parse().unwrap(),
            )])
            .into(),
        ),
        None,
        std::collections::BTreeMap::new(),
    );
    let refusal = at_add(Some("claude"), five, &unresolvable).unwrap_err();
    assert!(
        refusal.to_string().ends_with("run `rimz accounts list`"),
        "{refusal}"
    );
    let refusal = at_add(Some("claude"), five, &native).unwrap_err();
    assert_eq!(
        refusal.to_string(),
        "no current claude 5h window reading; open the room's sidebar or run `rimz accounts --refresh`"
    );
    publish_windows(&runtime, "claude", vec![window(Some(30), -60, five)]);
    assert!(matches!(
        at_add(Some("claude"), five, &native),
        Err(WindowRefusal::NoReading { .. })
    ));
    publish_windows(&runtime, "claude", vec![window(Some(30), 3_600, five)]);
    assert_eq!(
        at_add(Some("claude"), seven, &native)
            .unwrap_err()
            .to_string(),
        "claude has no 7d window"
    );
    let started = at_add(Some("claude"), five, &native).unwrap();
    assert_eq!(started.kind.as_str(), "claude");
    assert_eq!(started.window, window(Some(30), 3_600, five));
    let lifted = RateLimitWindow {
        lifted: true,
        resets_at: None,
        used_percentage: None,
        ..window(None, 0, five)
    };
    publish_windows(&runtime, "codex", vec![lifted.clone()]);
    assert_eq!(at_add(Some("codex"), five, &native).unwrap().window, lifted);
    let lifted_past_reset = RateLimitWindow {
        lifted: true,
        ..window(Some(30), -60, five)
    };
    publish_windows(&runtime, "codex", vec![lifted_past_reset.clone()]);
    assert_eq!(
        at_add(Some("codex"), five, &native).unwrap().window,
        lifted_past_reset,
        "a lifted window passes the reading check whatever its stale reset"
    );

    // A pinned task reads its own account's stored window, not the room's.
    let work =
        crate::ids::LoginKey::new(AgentKind::new_unchecked("claude"), "work".parse().unwrap());
    publish_windows_for(&runtime, work.clone(), vec![window(Some(55), 3_600, five)]);
    assert!(matches!(
        at_add(Some("claude"), five, &native),
        Err(WindowRefusal::NoReading { .. })
    ));
    assert_eq!(
        window_at_add(Some("claude"), five, Some(&work), dir.path(), &runtime, now)
            .unwrap()
            .window,
        window(Some(55), 3_600, five)
    );
    // So does its surplus gate at fire time.
    let claude = AgentKind::new_unchecked("claude");
    let accounts = toml::from_str("[claude.work]\nhome = \"/srv/work\"\n").unwrap();
    let surplus_gate = |name: &str| {
        let login =
            crate::agents::session_login(&claude, Some(&name.parse().unwrap()), &accounts).unwrap();
        let state = StatePaths::under(runtime.workspace_id.clone(), dir.path()).unwrap();
        FireScope::new(claude.clone(), runtime.clone(), state, Some(login))
            .surplus_gate(&surplus_entry(Some("1.5x"), None), now)
    };
    let no_reading = "no claude budget-window reading; surplus gate stays closed";
    assert_eq!(surplus_gate("default").as_deref(), Some(no_reading));
    assert_ne!(surplus_gate("work").as_deref(), Some(no_reading));
}

#[test]
fn after_reset_fires_at_the_reset_or_now_and_refuses_what_has_none() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();
    let now = Timestamp::now();
    let native = crate::ids::LoginKey::default_for(AgentKind::new_unchecked("claude"));
    let five = WindowSpan::FiveHour;
    let seven = WindowSpan::SevenDay;
    let window = |used, resets_in: Option<i64>, span: WindowSpan| RateLimitWindow {
        used_percentage: used,
        resets_at: resets_in.map(|secs| now + jiff::SignedDuration::from_secs(secs)),
        duration_mins: Some(span.minutes()),
        ..RateLimitWindow::default()
    };
    let resolve = |span, surplus| {
        after_reset(
            span,
            Some("claude"),
            Some(&native),
            surplus,
            dir.path(),
            &runtime,
            now,
        )
    };
    assert_eq!(
        after_reset(five, None, None, false, dir.path(), &runtime, now),
        Err(WindowRefusal::NoProvider)
    );
    let week = window(Some(10), Some(86_400), seven);
    publish_windows(
        &runtime,
        "claude",
        vec![window(Some(30), Some(3_600), five), week.clone()],
    );
    let started = resolve(five, true).unwrap();
    assert_eq!(
        started.fire_at,
        now + jiff::SignedDuration::from_secs(3_600)
    );
    assert_eq!((started.left, started.started), (Some(70), true));
    let refusal = resolve(seven, true).unwrap_err();
    assert!(
        matches!(refusal, WindowRefusal::SurplusAtLongestReset { .. }),
        "{refusal}"
    );
    assert_eq!(
        resolve(seven, false).unwrap().fire_at,
        week.resets_at.unwrap()
    );

    let fresh = window(Some(0), Some(i64::from(five.minutes()) * 60), five);
    publish_windows(&runtime, "claude", vec![fresh.clone(), week.clone()]);
    let not_started = resolve(five, false).unwrap();
    assert_eq!((not_started.fire_at, not_started.started), (now, false));
    assert_eq!(not_started.resets_at, fresh.resets_at.unwrap());

    let lifted = RateLimitWindow {
        lifted: true,
        ..window(None, None, five)
    };
    publish_windows(&runtime, "claude", vec![lifted, week.clone()]);
    let refusal = resolve(five, false).unwrap_err();
    assert!(
        refusal.to_string().contains("--after-reset 7d"),
        "{refusal}"
    );
    publish_windows(&runtime, "claude", vec![window(Some(30), None, five)]);
    assert!(matches!(
        resolve(five, false),
        Err(WindowRefusal::NoReading { .. })
    ));
}

#[test]
fn window_conditions_record_the_provider_their_terms_read() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();
    let now = Timestamp::now();
    let provider = |when: &str, kind: Option<&str>| {
        let key =
            kind.map(|kind| crate::ids::LoginKey::default_for(AgentKind::new_unchecked(kind)));
        window_condition_provider(
            &[when.to_owned()],
            kind,
            key.as_ref(),
            dir.path(),
            &runtime,
            now,
        )
    };
    assert_eq!(provider("ci=passed", None), Ok(None));
    assert_eq!(provider("ci=passed", Some("claude")), Ok(None));
    assert_eq!(provider("ci=", None), Ok(None));
    assert_eq!(
        provider("ci=passed || window.5h.left>=40", None),
        Err(WindowRefusal::NoProvider)
    );
    assert!(matches!(
        provider("window.5h.left>=40", Some("claude")),
        Err(WindowRefusal::NoReading { .. })
    ));
    publish_windows(
        &runtime,
        "claude",
        vec![RateLimitWindow {
            used_percentage: Some(92),
            resets_at: Some(now + jiff::SignedDuration::from_secs(3_600)),
            duration_mins: Some(WindowSpan::FiveHour.minutes()),
            ..RateLimitWindow::default()
        }],
    );
    assert_eq!(
        provider("window.5h.left>=40", Some("claude")),
        Ok(Some(AgentKind::new_unchecked("claude")))
    );
    assert_eq!(
        provider("window.5h.left>=40 && window.7d.left>=10", Some("claude"))
            .unwrap_err()
            .to_string(),
        "claude has no 7d window"
    );
}

#[test]
fn a_pinned_account_that_cannot_run_skips_with_the_fix() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("work");
    std::fs::create_dir(&home).unwrap();
    let accounts: crate::config::AccountsConfig = toml::from_str(&format!(
        "[codex.work]\nhome = {home:?}\n[codex.gone]\nhome = {:?}\n",
        dir.path().join("gone")
    ))
    .unwrap();
    let ambient = BTreeMap::new();
    let gate = |kind: &str, name: &str, statuses: &crate::agents::account::AccountsCache| {
        pinned_login(
            &AgentKind::new_unchecked(kind),
            &name.parse().unwrap(),
            &accounts,
            &ambient,
            statuses,
        )
        .expect("account config loads")
    };
    let work =
        crate::ids::LoginKey::new(AgentKind::new_unchecked("codex"), "work".parse().unwrap());
    let record = |ok| crate::agents::account::ProviderRecord {
        consecutive_failures: 0,
        version_probed_at_ms: None,
        inputs_stamp: None,
        profile: None,
        probe_error: None,
        login: None,
        probed_at_ms: 1,
        ok,
        account: None,
    };
    let no_record = crate::agents::account::AccountsCache::default();
    let mut logged_out = no_record.clone();
    logged_out.logins.insert(work.clone(), record(true));
    let mut failed_probe = no_record.clone();
    failed_probe.logins.insert(work.clone(), record(false));

    // A home without hooks is the hooks preflight's error, not this gate's skip.
    for statuses in [&no_record, &failed_probe] {
        assert_eq!(gate("codex", "work", statuses).unwrap().key(), work);
    }
    assert!(gate("codex", "default", &no_record).unwrap().is_default());
    assert_eq!(
        gate("codex", "ghost", &no_record).unwrap_err(),
        "unknown codex account `ghost`; configured: default, gone, work; run `rimz accounts add codex ghost`"
    );
    assert_eq!(
        gate("grok", "work", &no_record).unwrap_err(),
        "grok has no named accounts; only `default` is available; remove `account = \"work\"` from the task"
    );
    let gone = gate("codex", "gone", &no_record).unwrap_err();
    assert!(
        gone.starts_with("codex account `gone` home ")
            && gone.contains("is not a directory; run `"),
        "{gone}"
    );
    let skipped = format!(
        "codex account `work` is logged out; log in with `CODEX_HOME={} codex`",
        home.display()
    );
    assert_eq!(gate("codex", "work", &logged_out).unwrap_err(), skipped);

    // A logout that kept the CLI version skips by its recorded outcome; the
    // same account without one is unknown and launches.
    use crate::agents::account::RecordedLogin;
    let version_only = |login| {
        let mut cache = no_record.clone();
        cache.logins.insert(
            work.clone(),
            crate::agents::account::ProviderRecord {
                consecutive_failures: 0,
                version_probed_at_ms: None,
                inputs_stamp: None,
                profile: None,
                probe_error: None,
                login,
                probed_at_ms: 1,
                ok: true,
                account: Some(crate::agents::AgentAccount {
                    version: Some("1.0.0".to_owned()),
                    ..Default::default()
                }),
            },
        );
        cache
    };
    assert_eq!(
        gate(
            "codex",
            "work",
            &version_only(Some(RecordedLogin::LoggedOut))
        )
        .unwrap_err(),
        skipped
    );
    for login in [Some(RecordedLogin::LoggedIn), None] {
        assert_eq!(
            gate("codex", "work", &version_only(login)).unwrap().key(),
            work
        );
    }
}

#[test]
fn the_account_daily_cap_reads_the_pinned_account_not_the_rooms() {
    use crate::agents::spending::{
        ProviderSpendingCache, SpendWindow, write_provider_spending_cache,
    };

    let dir = tempfile::tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    record_room(dir.path()).unwrap();
    let runtime = RuntimePaths::under(workspace_id, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let state = StatePaths::for_project_root(dir.path()).unwrap();
    let config: MachineConfig = toml::from_str(
        "timezone = \"UTC\"\n[accounts.budget]\nclaude = \"10/day\"\n[accounts.claude.solo]\nhome = \"/srv/loop-test-solo\"\nhistory = \"standalone\"\n",
    )
    .unwrap();
    let now: Timestamp = "2026-06-02T12:00:00Z".parse().unwrap();
    let claude = AgentKind::new_unchecked("claude");
    let entry = TaskEntry {
        check: Some("true".into()),
        root: dir.path().to_path_buf(),
        at: Some("07:00".to_owned()),
        ..TaskEntry::default()
    };
    crate::harness::schedule::instances::insert(&state, "capped", &entry).unwrap();
    let catalog = TaskCatalog::load(Some(dir.path())).unwrap();
    let mut fire = skipped_fire("capped", &catalog, None);
    fire.now = now;
    let refusal = |fire: &TaskFire<'_>, account: &str| {
        let login = crate::agents::session_login(
            &claude,
            Some(&account.parse().unwrap()),
            &config.accounts,
        )
        .unwrap();
        let scope = FireScope::new(claude.clone(), runtime.clone(), state.clone(), Some(login));
        fire.scope_refusal(&scope, true).map(|(result, _)| result)
    };
    fire.config = Arc::new(config.clone());
    for (solo_usd, room_usd, solo, room) in [
        (12.0, 2.0, Some(LoopRunResult::BudgetSkipped), None),
        (2.0, 12.0, None, Some(LoopRunResult::BudgetSkipped)),
    ] {
        let spent = |name: &str, usd| {
            (
                crate::ids::LoginKey::new(claude.clone(), name.parse().unwrap()),
                SpendWindow {
                    usd,
                    ..Default::default()
                },
            )
        };
        write_provider_spending_cache(
            &runtime.shared_provider_spending_path(),
            &ProviderSpendingCache {
                day_cutoff_secs: jiff::civil::date(2026, 6, 2)
                    .to_zoned(jiff::tz::TimeZone::UTC)
                    .unwrap()
                    .timestamp()
                    .as_second() as u64,
                day_by_login: BTreeMap::from([spent("solo", solo_usd), spent("default", room_usd)]),
                ..Default::default()
            },
        );
        assert_eq!(refusal(&fire, "solo"), solo, "pinned, solo at {solo_usd}");
        assert_eq!(
            refusal(&fire, "default"),
            room,
            "room, default at {room_usd}"
        );
    }
}

#[test]
fn a_pin_that_stops_resolving_at_launch_skips_and_every_other_error_strikes() {
    use crate::harness::schedule::strikes::{Signal, classify};

    let dir = tempfile::tempdir().unwrap();
    record_room(dir.path()).unwrap();
    let state = StatePaths::for_project_root(dir.path()).unwrap();
    let unresolved = |kind: &str| {
        let err = crate::agents::session_login(
            &AgentKind::new_unchecked(kind),
            Some(&"ghost".parse().unwrap()),
            &crate::config::AccountsConfig::default(),
        )
        .unwrap_err();
        anyhow::Error::from(err).context("launch codex")
    };
    let unknown =
        "unknown codex account `ghost`; configured: default; run `rimz accounts add codex ghost`";
    let unsupported = "grok has no named accounts; only `default` is available; remove `account = \"ghost\"` from the task";
    for (name, pinned, error, skip) in [
        ("unknown", true, unresolved("codex"), Some(unknown)),
        ("unsupported", true, unresolved("grok"), Some(unsupported)),
        ("other", true, anyhow::anyhow!("pane refused"), None),
        ("unpinned", false, unresolved("codex"), None),
    ] {
        let entry = TaskEntry {
            agent: Some("codex".to_owned()),
            prompt: Some("work".to_owned()),
            account: pinned.then(|| "ghost".parse().unwrap()),
            root: dir.path().to_path_buf(),
            fire_at: Some(Timestamp::UNIX_EPOCH),
            ..TaskEntry::default()
        };
        crate::harness::schedule::instances::insert(&state, name, &entry).unwrap();
        let catalog = TaskCatalog::load(Some(dir.path())).unwrap();
        let finished = skipped_fire(name, &catalog, None).finish_error(&error);
        match skip {
            Some(reason) => {
                assert_eq!(
                    finished.record.result,
                    LoopRunResult::AccountSkipped,
                    "{name}"
                );
                assert_eq!(finished.record.error.as_deref(), Some(reason), "{name}");
                assert!(
                    matches!(&finished.notice, TaskFireNotice::Gate { reason: told } if told == reason),
                    "{name}"
                );
                assert_eq!(classify(&finished.record), Signal::Neutral, "{name}");
            }
            None => {
                assert_eq!(finished.record.result, LoopRunResult::Errored, "{name}");
                assert_eq!(classify(&finished.record), Signal::Strike, "{name}");
            }
        }
        assert!(
            !crate::harness::schedule::instances::load_from(&state.root)
                .0
                .contains_key(name),
            "{name}: the one-shot row is consumed"
        );
    }
}

fn scheduled_fire<'a>(name: &str, entry: TaskEntry, catalog: &'a TaskCatalog) -> TaskFire<'a> {
    let mut fire = TaskFire::new(
        name,
        LoadedTask::new(name, entry, catalog::TaskSource::Config),
        catalog,
        LoopRunMode::Scheduled,
        false,
        Timestamp::now(),
        Arc::new(MachineConfig::default()),
        None,
        Instant::now(),
    )
    .expect("construct task fire");
    fire.run_lock_path = |file, entry| Ok(entry.root.join(file));
    fire
}

fn resident_entry(root: &Path) -> TaskEntry {
    TaskEntry {
        root: root.to_owned(),
        agent: Some("claude".into()),
        prompt: Some("repair".into()),
        stay: true,
        ..Default::default()
    }
}

#[test]
fn resident_saved_modes_refuse_invalid_values_before_room_birth_and_record_strikes() {
    use crate::harness::schedule::strikes::{Signal, classify};

    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for mode in [LoopRunMode::Manual, LoopRunMode::Scheduled] {
        for raw in ["plan", "yollo", "ASK", " plan "] {
            let root = tempfile::tempdir().unwrap();
            record_room(root.path()).unwrap();
            let paths = StatePaths::for_project_root(root.path()).unwrap();
            let catalog = TaskCatalog::load(Some(root.path())).unwrap();
            let entry = TaskEntry {
                mode: Some(raw.to_owned()),
                ..resident_entry(root.path())
            };
            let mut fire = scheduled_fire("bad-mode", entry, &catalog);
            fire.mode = mode;
            let error = fire
                .prepare(&mut |_| panic!("invalid saved mode must not birth a room"))
                .unwrap_err();
            let message = format!("unknown loop mode `{}`; use auto, ask, or yolo", raw.trim());
            actual.push(format!("{error:#}"));
            expected.push(message.clone());
            assert!(fire.pending.is_none(), "{mode:?}, {raw:?}");
            let finished = fire.finish_error(&error);
            assert_eq!(finished.record.result, LoopRunResult::Errored);
            assert_eq!(finished.record.error.as_deref(), Some(message.as_str()));
            assert_eq!(classify(&finished.record), Signal::Strike);
            let records = run_log::task_records(&logs_dir(), "bad-mode", Some(root.path()));
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].result, LoopRunResult::Errored);
            assert_eq!(records[0].error.as_deref(), Some(message.as_str()));
            assert!(
                super::super::launch_ledger::load_workflows(&paths)
                    .unwrap()
                    .tasks
                    .is_empty()
            );
        }
    }
    assert_eq!(actual, expected);
}

fn git_in(dir: &Path, args: &[&str]) {
    assert!(
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .status()
            .unwrap()
            .success(),
        "git {args:?}"
    );
}

/// A fresh task's fire cuts its tree just before `create`, and a tree no run took over goes with
/// the fire: every return before `hand_over_fire_tree`, a refused `create` among them.
#[test]
fn a_fresh_tree_no_run_took_over_is_removed_with_the_fire() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    git_in(&root, &["init", "-q", "-b", "main"]);
    git_in(&root, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let catalog = TaskCatalog::load(Some(dir.path())).unwrap();
    let entry = TaskEntry {
        check: Some("true".into()),
        every: Some("1h".to_owned()),
        root: root.clone(),
        fresh_worktree: Some("night".to_owned()),
        ..TaskEntry::default()
    };
    let mut kept = None;
    for launched in [false, true] {
        let mut fire = TaskFire::new(
            "nightly",
            LoadedTask::new("nightly", entry.clone(), catalog::TaskSource::Config),
            &catalog,
            LoopRunMode::Scheduled,
            false,
            "2026-01-02T03:04:05Z".parse().unwrap(),
            Arc::new(MachineConfig::default()),
            None,
            Instant::now(),
        )
        .unwrap();
        fire.prepare_fire_tree().unwrap();
        let tree = fire.fire_dir();
        assert_eq!(fire.launch_checkout(), root, "loop's keys stay at the root");
        let name = tree.file_name().unwrap().to_str().unwrap().to_owned();
        assert!(name.starts_with("night-20260102-030405"), "{name}");
        if launched {
            fire.hand_over_fire_tree();
        }
        drop(fire);
        assert_eq!(tree.exists(), launched, "{name}");
        if launched {
            kept = Some(tree);
        }
    }
    let marker = crate::worktree::read_marker_for_worktree(&kept.unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(marker.loop_task.as_deref(), Some("nightly"));
}

#[test]
fn a_live_run_reserves_its_sealed_budget_and_an_unreadable_seal_refuses() {
    let root = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let step = |budget: &str| {
        format!(
            "[workflow]\nscope='worktree'\nstart='fix'\n[steps.fix]\nagent='claude'\nprompt='fix'\nbudget='{budget}'\nthen='@done'\n"
        )
    };
    std::fs::write(
        crate::disk::paths::workflow_spec_file(source.path()),
        step("$3"),
    )
    .unwrap();
    let state = StatePaths::for_project_root(root.path()).unwrap();
    let record: crate::store::workflow::RunRecord = serde_json::from_value(serde_json::json!({
        "version":1, "id":crate::ids::WorkflowRunId::new(), "workflow":"review", "task":"sweep",
        "spec_dir":source.path(), "spec_digest":"abc",
        "scope":{"kind":"worktree","path":root.path()}, "project_root":root.path(),
        "created_at":"2000-01-01T00:00:00Z", "state":"active", "node":"fix",
        "entered_at":"2000-01-01T00:00:00Z", "visits":{}, "visit_grant":0,
        "deadline_fired":false, "next_seq":1, "spec_version":1
    }))
    .unwrap();
    let guard = crate::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    crate::store::workflow::initialize(&state, &record, step("$8").as_bytes(), &guard).unwrap();
    drop(guard);
    assert_eq!(live_reservation(root.path(), "sweep").unwrap(), 8.0);
    std::fs::remove_file(crate::disk::paths::workflow_spec_file(
        &state.workflow(&record.id).home,
    ))
    .unwrap();
    let error = live_reservation(root.path(), "sweep").unwrap_err();
    assert!(
        format!("{error:#}").contains(&format!("reading run {}'s sealed definition", record.id)),
        "{error:#}"
    );
}

const SIX_DOLLAR_STEP: &str = "[workflow]\nscope='worktree'\nstart='fix'\n[steps.fix]\nagent='claude'\nprompt='fix'\nbudget='$6'\nthen='@done'\n";

#[test]
fn a_capped_fire_reads_the_budget_only_after_a_concurrent_fire_publishes_its_run() {
    let root = tempfile::tempdir().unwrap();
    record_room(root.path()).unwrap();
    let state = StatePaths::for_project_root(root.path()).unwrap();
    let held = super::super::launch_ledger_store::hold_budget(&state, "capped").unwrap();
    let (root_path, publisher_state) = (root.path().to_owned(), state.clone());
    // The concurrent fire: it read the budget under the lock, and publishes its $6 run before
    // it lets go.
    let publisher = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        let record: crate::store::workflow::RunRecord = serde_json::from_value(serde_json::json!({
            "version":1, "id":crate::ids::WorkflowRunId::new(), "workflow":"capped",
            "task":"capped", "spec_dir":root_path, "spec_digest":"abc",
            "scope":{"kind":"worktree","path":root_path}, "project_root":root_path,
            "created_at":"2000-01-01T00:00:00Z", "state":"active", "node":"fix",
            "entered_at":"2000-01-01T00:00:00Z", "visits":{}, "visit_grant":0,
            "deadline_fired":false, "next_seq":1, "spec_version":1
        }))
        .unwrap();
        let guard =
            crate::disk::lock::WorkspaceLock::acquire(&publisher_state.workspace_lock).unwrap();
        crate::store::workflow::initialize(
            &publisher_state,
            &record,
            SIX_DOLLAR_STEP.as_bytes(),
            &guard,
        )
        .unwrap();
        drop(guard);
        drop(held);
    });
    let workflow = format!("capped-{}", crate::ids::WorkflowRunId::new());
    let dir = crate::disk::paths::workflows_dir().join(&workflow);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        crate::disk::paths::workflow_spec_file(&dir),
        SIX_DOLLAR_STEP,
    )
    .unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    let entry = TaskEntry {
        root: root.path().to_owned(),
        workflow: Some(workflow),
        budget_per_day: Some("$10".into()),
        ..Default::default()
    };
    let mut fire = TaskFire::new(
        "capped",
        LoadedTask::new("capped", entry, catalog::TaskSource::Config),
        &catalog,
        LoopRunMode::Manual,
        false,
        Timestamp::now(),
        Arc::new(MachineConfig::default()),
        None,
        Instant::now(),
    )
    .unwrap();
    let plan = fire.prepare(&mut |_| Ok(()));
    publisher.join().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    // An admitted fire goes on to `create`, which refuses this stub definition.
    let skipped = matches!(
        &plan,
        Ok(TaskFirePlan::Done(done)) if done.record.result == LoopRunResult::BudgetSkipped
    );
    assert!(
        skipped,
        "the second fire must count the first fire's $6 run against the $10 cap: {:?}",
        plan.err()
    );
}

/// Takes a fire to a created run only in a re-exec of `name` with private homes: the agent
/// row's scope gate preflights Claude hooks against this process's environment, and every
/// test process shares the unit-test home. Returns whether this is the parent, done.
fn reexec_with_private_homes(name: &str) -> bool {
    const CHILD: &str = "RIMZ_TEST_FIRE_PROMPT_CHILD";
    if std::env::var_os(CHILD).is_some() {
        return false;
    }
    // Under /tmp so the runtime root's socket paths stay short.
    let home = tempfile::Builder::new()
        .prefix("rimz-fire-prompt-")
        .tempdir_in("/tmp")
        .unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", name, "--nocapture"])
        .env(CHILD, "1")
        .env("HOME", home.path())
        .env("CLAUDE_CONFIG_DIR", home.path().join("claude"))
        .env_remove("RIMZ_CLAUDE_SETTINGS");
    for (key, leaf) in [
        ("RIMZ_HOME", ".rimz"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_RUNTIME_DIR", "runtime"),
    ] {
        let path = home.path().join(leaf);
        std::fs::create_dir_all(&path).unwrap();
        command
            .env(key, &path)
            .env(format!("RIMZ_UNIT_TEST_{key}"), path);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{name} failed in its re-exec: {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    true
}

fn seed_deploy_trigger(root: &Path, name: &str, env: &str, arrived_at: Timestamp) {
    let paths = StatePaths::for_project_root(root).unwrap();
    crate::disk::atomic::write_temp_then_rename(
        &backlog::path(&paths),
        &serde_json::json!({"version": 1, "tasks": {name: {"triggers": [{
            "arrived_at": arrived_at,
            "signal": {"name": "deploy.done", "payload": {"env": env}},
            "absorbed": 0
        }]}}}),
    )
    .unwrap();
}

#[derive(Clone, Copy, PartialEq)]
enum TriggerSwitch {
    None,
    RunLock,
    Reconcile,
}

/// Fires an inline agent row on a `deploy.done {"env":"b"}` arrival and checks that the run's
/// sealed prompt names the trigger its record names: `env` is the payload the fire serves.
fn assert_sealed_prompt_names_the_served_trigger(name: &str, switch: TriggerSwitch, env: &str) {
    let adapter = find_definition("claude").unwrap();
    let login_env = ambient_env();
    adapter
        .install_hooks(&login_env, Default::default())
        .unwrap();
    assert!(adapter.hooks_installed(&login_env));
    let root = tempfile::tempdir().unwrap();
    record_room(root.path()).unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    let now = Timestamp::now();
    let overlap = match switch {
        TriggerSwitch::Reconcile => OverlapPolicy::Coalesce,
        TriggerSwitch::None | TriggerSwitch::RunLock => OverlapPolicy::Queue,
    };
    let mut fire = TaskFire::new(
        name,
        LoadedTask::new(
            name,
            TaskEntry {
                root: root.path().to_owned(),
                agent: Some("claude".into()),
                prompt: Some("Check the deployment.".into()),
                signal: Some("deploy.done".into()),
                overlap: Some(overlap),
                ..Default::default()
            },
            catalog::TaskSource::Instance,
        ),
        &catalog,
        LoopRunMode::Scheduled,
        false,
        now,
        Arc::new(MachineConfig::default()),
        Some(
            serde_json::from_value(serde_json::json!({
                "name": "deploy.done", "source": "cli", "payload": {"env": "b"}
            }))
            .unwrap(),
        ),
        Instant::now(),
    )
    .unwrap();
    let at = |offset: i64| Timestamp::from_second(now.as_second() + offset).unwrap();
    if switch == TriggerSwitch::RunLock {
        seed_deploy_trigger(root.path(), name, "a", at(-1));
    }
    let plan = fire
        .prepare(&mut |_| {
            if switch == TriggerSwitch::Reconcile {
                seed_deploy_trigger(root.path(), name, "c", at(1));
            }
            Ok(())
        })
        .unwrap_or_else(|error| panic!("TaskFire::prepare: {error:#}"));
    let TaskFirePlan::Done(done) = plan else {
        panic!("expected TaskFirePlan::Done")
    };
    assert_eq!(
        done.record.result,
        LoopRunResult::Launched,
        "{:?}",
        done.record
    );
    let state = StatePaths::for_project_root(root.path()).unwrap();
    let run = crate::store::workflow::list(&state)
        .unwrap()
        .into_iter()
        .find(|run| done.record.run_id.as_deref() == Some(run.id.as_str()))
        .expect("created workflow run");
    let prompt = std::fs::read_to_string(run.spec_dir.join("prompts/prompt.md")).unwrap();
    let signal = done.record.signal.as_ref().unwrap();
    let shown = format!("sealed prompt:\n{prompt}\nrecord signal: {signal:?}");
    assert_eq!(signal.payload["env"], env, "{shown}");
    assert_eq!(
        done.record.absorbed,
        u64::from(switch == TriggerSwitch::Reconcile),
        "{shown}"
    );
    assert!(
        prompt.starts_with(&format!(
            "Rule `{name}` fired here. deploy.done · env: {env}\n"
        )),
        "{shown}"
    );
    assert!(prompt.contains("Check the deployment."), "{shown}");
}

#[test]
fn a_queued_fire_seals_the_prompt_of_the_waiting_trigger_it_serves() {
    let name = "harness::schedule::runner::tests::a_queued_fire_seals_the_prompt_of_the_waiting_trigger_it_serves";
    if reexec_with_private_homes(name) {
        return;
    }
    assert_sealed_prompt_names_the_served_trigger("queued", TriggerSwitch::RunLock, "a");
}

#[test]
fn a_coalesced_fire_seals_the_prompt_of_the_newer_trigger_it_serves() {
    let name = "harness::schedule::runner::tests::a_coalesced_fire_seals_the_prompt_of_the_newer_trigger_it_serves";
    if reexec_with_private_homes(name) {
        return;
    }
    assert_sealed_prompt_names_the_served_trigger("coalesced", TriggerSwitch::Reconcile, "c");
}

#[test]
fn a_fire_with_nothing_waiting_seals_the_prompt_of_its_arrival() {
    let name = "harness::schedule::runner::tests::a_fire_with_nothing_waiting_seals_the_prompt_of_its_arrival";
    if reexec_with_private_homes(name) {
        return;
    }
    assert_sealed_prompt_names_the_served_trigger("arrival", TriggerSwitch::None, "b");
}
