use std::fs::File;
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
                args: vec!["true".to_owned()],
                timeout: Some("1m".to_owned()),
                every: Some("1h".into()),
                overlap: Some(crate::config::OverlapPolicy::Coalesce),
                ..Default::default()
            },
            catalog::TaskSource::Instance,
        ),
        catalog,
        LoopRunMode::Scheduled,
        Timestamp::now(),
        Arc::new(MachineConfig::default()),
        None,
        Instant::now(),
    )
    .unwrap()
}

fn seed_backlog(root: &Path, task: &TaskId) {
    let paths = StatePaths::for_project_root(root).unwrap();
    crate::disk::atomic::write_temp_then_rename(
        &super::super::backlog::path(&paths),
        &serde_json::json!({"version":1, "tasks":{task.as_str():{"triggers":[{"arrived_at":Timestamp::now(),"signal":null,"absorbed":0}]}}})
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
            &fire.task_id,
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
                            args: vec!["true".to_owned()],
                            timeout: Some("1m".to_owned()),
                            every: Some("1h".into()),
                            overlap,
                            ..Default::default()
                        },
                        catalog::TaskSource::Config,
                    ),
                    &catalog,
                    mode,
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
                assert!(
                    !super::super::strikes::load()
                        .unwrap()
                        .contains_key(&fire.task.key(name))
                );
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
        locked.record.tasks.insert(fire.task_id.to_string(), serde_json::from_value(serde_json::json!({
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
                .entry(fire.task_id.to_string())
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
                !backlog::load(&paths)
                    .unwrap()
                    .tasks
                    .contains_key(fire.task_id.as_str()),
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
    let mut fire = backlog_fire(root.path(), &catalog);
    seed_backlog(root.path(), &fire.task_id);
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
            seed_backlog(root.path(), &fire.task_id);
        }
        let lock_file = run_lock_file_name(fire.task_id.as_str(), None);
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
                probe_run_lock_path(&run_lock_path(&lock_file, &fire_entry(root.path())).unwrap())
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
    let task = fire.task_id.clone();
    let mut checked = false;
    fire.prepare(&mut |_| {
        checked = true;
        let path = runtime.lock_path(run_lock_file_name(task.as_str(), None));
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
    let mut fire = backlog_fire(root.path(), &catalog);
    seed_backlog(root.path(), &fire.task_id);
    let lock_path = run_lock_path(
        &run_lock_file_name(fire.task_id.as_str(), None),
        &fire_entry(root.path()),
    )
    .unwrap();
    let RunLockAttempt::Acquired(held) =
        acquire_run_lock_file(&lock_path, Some(TaskKind::Routine), None).unwrap()
    else {
        panic!("fixture takes lock")
    };
    assert!(!fire.take_backlog().unwrap());
    assert!(!fire.finished, "held drainer writes no row");
    drop(held);
    let paths = StatePaths::for_project_root(root.path()).unwrap();
    let arrived_at =
        backlog::load(&paths).unwrap().tasks[fire.task_id.as_str()].triggers[0].arrived_at;
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
    // A machine task's record is shared by every test process.
    if super::super::test_homes::reexec_with_private_homes(
        "harness::schedule::runner::tests::scheduled_workflow_rechecks_launch_memory_under_early_and_late_run_locks",
    ) {
        return;
    }
    let mut outcomes = Vec::new();
    for late in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let seed = || {
            let binding =
                task_record::Binding::for_task("land", catalog::TaskSource::Config, root.path());
            task_record_store::mutate(|records| {
                let (task, record) = records.bind(&binding);
                record.marks.insert(
                    root.path().to_owned(),
                    FireMark::open(&task, crate::store::workflow::ScopeKind::Worktree),
                );
            })
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
                    args: vec!["true".to_owned()],
                    timeout: Some("1m".to_owned()),
                    once_per_checkout: Some(true),
                    when: Some(vec!["pr=open".into()]),
                    ..Default::default()
                },
                catalog::TaskSource::Config,
            ),
            &catalog,
            LoopRunMode::Scheduled,
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
fn deadline_expiry_uses_injected_time() {
    let now = Timestamp::from_second(200_000).expect("now");
    let expired = TaskEntry {
        deadline: Some(Timestamp::from_second(199_999).expect("deadline")),
        ..TaskEntry::default()
    };

    assert!(deadline_expired_at(&expired, now));
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
    let outcome =
        |passed, timed_out, code| CheckOutcome::new(passed, timed_out, String::new(), code);
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
            args: vec!["true".to_owned()],
            timeout: Some("1m".to_owned()),
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
        args: vec!["true".to_owned()],
        timeout: Some("1m".to_owned()),
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
    let run = crate::store::workflow::live_runs(&state)
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
fn check_room_hook_is_after_lock_and_deadline_and_records_failure() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(dir.path())).unwrap();
    let entry = TaskEntry {
        args: vec!["touch".to_owned(), "check-ran".to_owned()],
        timeout: Some("1m".to_owned()),
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
fn condition_evidence_reaches_prompt_and_terminal_record() {
    let entry = TaskEntry {
        args: vec!["true".to_owned()],
        timeout: Some("1m".to_owned()),
        when: Some(vec!["team.stage=Done".to_owned()]),
        repeat: true,
        ..TaskEntry::default()
    };
    let catalog = TaskCatalog::load(None).unwrap();
    let evidence = crate::harness::when::ConditionEvidence {
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
        Timestamp::now(),
        Arc::new(MachineConfig::default()),
        None,
        Instant::now(),
    )
    .unwrap()
    .with_condition(Some(evidence.clone()));
    assert_eq!(
        fire.resolve_effect_prompt().unwrap(),
        "Rule `ship` fired here. ci.log: \"red\\nlog\" · pr.state: unknown · team.stage: Done"
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
    assert_eq!(
        fire.resolve_effect_prompt().unwrap(),
        "",
        "no evidence, no trigger text"
    );
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
    let resolve = |span| {
        after_reset(
            span,
            Some("claude"),
            Some(&native),
            dir.path(),
            &runtime,
            now,
        )
    };
    assert_eq!(
        after_reset(five, None, None, dir.path(), &runtime, now),
        Err(WindowRefusal::NoProvider)
    );
    let week = window(Some(10), Some(86_400), seven);
    publish_windows(
        &runtime,
        "claude",
        vec![window(Some(30), Some(3_600), five), week.clone()],
    );
    let started = resolve(five).unwrap();
    assert_eq!(
        started.fire_at,
        now + jiff::SignedDuration::from_secs(3_600)
    );
    assert_eq!((started.left, started.started), (Some(70), true));
    assert_eq!(resolve(seven).unwrap().fire_at, week.resets_at.unwrap());

    let fresh = window(Some(0), Some(i64::from(five.minutes()) * 60), five);
    publish_windows(&runtime, "claude", vec![fresh.clone(), week.clone()]);
    let not_started = resolve(five).unwrap();
    assert_eq!((not_started.fire_at, not_started.started), (now, false));
    assert_eq!(not_started.resets_at, fresh.resets_at.unwrap());

    let lifted = RateLimitWindow {
        lifted: true,
        ..window(None, None, five)
    };
    publish_windows(&runtime, "claude", vec![lifted, week.clone()]);
    let refusal = resolve(five).unwrap_err();
    assert!(
        refusal.to_string().contains("--after-reset 7d"),
        "{refusal}"
    );
    publish_windows(&runtime, "claude", vec![window(Some(30), None, five)]);
    assert!(matches!(
        resolve(five),
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
        args: vec!["true".to_owned()],
        timeout: Some("1m".to_owned()),
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

fn seed_deploy_trigger(root: &Path, task: &TaskId, env: &str, arrived_at: Timestamp) {
    let paths = StatePaths::for_project_root(root).unwrap();
    crate::disk::atomic::write_temp_then_rename(
        &backlog::path(&paths),
        &serde_json::json!({"version": 1, "tasks": {task.as_str(): {"triggers": [{
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

/// Fires a command row on a `deploy.done {"env":"b"}` arrival and checks that the run's prompt,
/// which its command reads as `RIMZ_LOOP_TRIGGER`, names the trigger its record names: `env` is
/// the payload the fire serves.
fn assert_sealed_prompt_names_the_served_trigger(name: &str, switch: TriggerSwitch, env: &str) {
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
                args: vec!["true".to_owned()],
                timeout: Some("1m".to_owned()),
                signal: Some("deploy.done".into()),
                repeat: true,
                overlap: Some(overlap),
                ..Default::default()
            },
            catalog::TaskSource::Instance,
        ),
        &catalog,
        LoopRunMode::Scheduled,
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
    let task = fire.task_id.clone();
    if switch == TriggerSwitch::RunLock {
        seed_deploy_trigger(root.path(), &task, "a", at(-1));
    }
    let plan = fire
        .prepare(&mut |_| {
            if switch == TriggerSwitch::Reconcile {
                seed_deploy_trigger(root.path(), &task, "c", at(1));
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
    let run = crate::store::workflow::live_runs(&state)
        .unwrap()
        .into_iter()
        .find(|run| done.record.run_id.as_deref() == Some(run.id.as_str()))
        .expect("created workflow run");
    let prompt = run.prompt.clone().unwrap_or_default();
    let signal = done.record.signal.as_ref().unwrap();
    let shown = format!("sealed prompt:\n{prompt}\nrecord signal: {signal:?}");
    assert_eq!(signal.payload["env"], env, "{shown}");
    assert_eq!(
        done.record.absorbed,
        u64::from(switch == TriggerSwitch::Reconcile),
        "{shown}"
    );
    assert_eq!(
        prompt,
        format!("Rule `{name}` fired here. deploy.done · env: {env}"),
        "{shown}"
    );
}

#[test]
fn a_queued_fire_seals_the_prompt_of_the_waiting_trigger_it_serves() {
    let name = "harness::schedule::runner::tests::a_queued_fire_seals_the_prompt_of_the_waiting_trigger_it_serves";
    if super::super::test_homes::reexec_with_private_homes(name) {
        return;
    }
    assert_sealed_prompt_names_the_served_trigger("queued", TriggerSwitch::RunLock, "a");
}

#[test]
fn a_coalesced_fire_seals_the_prompt_of_the_newer_trigger_it_serves() {
    let name = "harness::schedule::runner::tests::a_coalesced_fire_seals_the_prompt_of_the_newer_trigger_it_serves";
    if super::super::test_homes::reexec_with_private_homes(name) {
        return;
    }
    assert_sealed_prompt_names_the_served_trigger("coalesced", TriggerSwitch::Reconcile, "c");
}

#[test]
fn a_fire_with_nothing_waiting_seals_the_prompt_of_its_arrival() {
    let name = "harness::schedule::runner::tests::a_fire_with_nothing_waiting_seals_the_prompt_of_its_arrival";
    if super::super::test_homes::reexec_with_private_homes(name) {
        return;
    }
    assert_sealed_prompt_names_the_served_trigger("arrival", TriggerSwitch::None, "b");
}

/// Every fire's mark names its run and who booked the row, so the sidebar's bookings lane can
/// rebuild a booked launch's fired entry once the fire consumed the row; only a row that dedupes
/// its checkout marks it held.
#[test]
fn a_fire_mark_names_its_run_and_its_booker() {
    let launch = |words: &[&str]| words.iter().map(|word| (*word).to_owned()).collect();
    let booker = crate::agents::LaunchedBy {
        kind: crate::ids::AgentKind::new_unchecked("claude"),
        agent_id: "booker".into(),
    };
    let cases: [(&str, TaskEntry, bool); 5] = [
        (
            "booked",
            TaskEntry {
                args: launch(&["rimz", "agents", "coder", "--detach"]),
                booked_by: vec![booker.clone()],
                ..TaskEntry::default()
            },
            false,
        ),
        (
            "deduped",
            TaskEntry {
                args: launch(&["true"]),
                once_per_checkout: Some(true),
                ..TaskEntry::default()
            },
            true,
        ),
        (
            "unbooked",
            TaskEntry {
                args: launch(&["rimz", "agents", "coder", "--detach"]),
                ..TaskEntry::default()
            },
            false,
        ),
        (
            "not-a-launch",
            TaskEntry {
                args: launch(&["true"]),
                booked_by: vec![booker.clone()],
                ..TaskEntry::default()
            },
            false,
        ),
        (
            "repeating",
            TaskEntry {
                args: launch(&["rimz", "teams", "forge"]),
                booked_by: vec![booker],
                when: Some(vec!["pr=open".to_owned()]),
                repeat: true,
                ..TaskEntry::default()
            },
            false,
        ),
    ];
    for (name, entry, dedupes) in cases {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        record_room(&root).unwrap();
        let catalog = TaskCatalog::load(Some(&root)).unwrap();
        let entry = TaskEntry {
            root: root.clone(),
            timeout: Some("1m".to_owned()),
            fire_at: entry.when.is_none().then(Timestamp::now),
            ..entry
        };
        let fire = TaskFire::new(
            name,
            LoadedTask::new(name, entry, catalog::TaskSource::Instance),
            &catalog,
            LoopRunMode::Scheduled,
            Timestamp::now(),
            Arc::new(MachineConfig::default()),
            None,
            Instant::now(),
        )
        .unwrap();
        let run = crate::ids::WorkflowRunId::new();
        let record: crate::store::workflow::RunRecord = serde_json::from_value(serde_json::json!({
            "version":crate::store::workflow::VERSION, "id":run, "workflow":name,
            "origin":"inline", "task":name,
            "scope":{"kind":"worktree","path":root}, "project_root":root,
            "created_at":"2026-01-01T00:00:00Z", "state":"active", "node":"command",
            "entered_at":"2026-01-01T00:00:00Z", "visits":{}, "visit_grant":0,
            "deadline_fired":false, "next_seq":1, "loop_report":"owed"
        }))
        .unwrap();
        let (fire_id, _) = fire.open_fire().unwrap();
        assert_eq!(
            fire.note_launched(&fire_id, Some(&record.id), record.created_at),
            None,
            "{name}"
        );
        let records = task_record::load().unwrap();
        let mark = &records.get(&fire.task_id).unwrap().marks[&fire.launch_checkout()];
        assert_eq!(
            (&mark.fire, mark.run.as_ref(), &mark.booked_by, mark.once),
            (&fire_id, Some(&run), &fire.entry.booked_by, dedupes),
            "{name}"
        );
    }
}

/// A wake prepared before a swap goes to the session holding the agent when
/// it is sent: the provisional replacement by its own name, then the session
/// it registers as. An older target naming the first session follows too.
#[test]
fn a_prepared_delivery_resolves_its_holder_when_sent() {
    let dir = tempfile::tempdir().unwrap();
    let target = |session: &str| TaskTarget {
        kind: crate::ids::AgentKind::new_unchecked("claude"),
        session: session.into(),
        handle: "@stored".to_owned(),
    };
    let prepared = |session: &str| PreparedDelivery {
        root: dir.path().to_owned(),
        task_root: dir.path().to_owned(),
        target: target(session),
        prompt: "wake".to_owned(),
        supersedes: Vec::new(),
        intent: DeliveryIntent::SelfWait,
    };
    let holder = |session: &str| {
        let holder = prepared(session).holder().unwrap().expect("a holder");
        (holder.session.to_string(), holder.handle)
    };
    let mut seen = Vec::new();
    super::super::tests::swapped_store_with(dir.path(), |stage| {
        seen.push((stage.to_owned(), holder("launch_a")));
    });
    seen.push(("registered s2".to_owned(), holder("launch_a")));
    seen.push(("older target".to_owned(), holder("s1")));
    let expected = [
        ("registered", "s1", "@stored"),
        ("bound", "launch_b", "@coder"),
        ("registered s2", "s2", "@stored"),
        ("older target", "s2", "@stored"),
    ]
    .map(|(stage, session, handle)| (stage.to_owned(), (session.to_owned(), handle.to_owned())));
    assert_eq!(seen, expected);
}

/// A fire of a launch task named uniquely, so the shared unit-test home's machine records
/// never collide; `kind` and `target` are its declared launch.
fn launch_fire<'a>(
    root: &Path,
    catalog: &'a TaskCatalog,
    entry: TaskEntry,
    mode: LoopRunMode,
) -> TaskFire<'a> {
    let name = format!("launch-{}", TaskId::new().short());
    let entry = TaskEntry {
        root: root.to_owned(),
        every: Some("1h".into()),
        ..entry
    };
    TaskFire::new(
        name.clone(),
        LoadedTask::new(&name, entry, catalog::TaskSource::Instance),
        catalog,
        mode,
        Timestamp::now(),
        Arc::new(MachineConfig::default()),
        None,
        Instant::now(),
    )
    .unwrap()
}

fn agent_entry() -> TaskEntry {
    TaskEntry {
        kind: Some(LaunchKind::Agent),
        target: Some("worker".to_owned()),
        args: vec!["-p".to_owned(), "go".to_owned()],
        ..TaskEntry::default()
    }
}

/// A launch mark for `fire` in `checkout`, open, its processes gone.
fn dead_mark(fire: FireId, child: bool) -> FireMark {
    FireMark {
        fire,
        at: Timestamp::now(),
        booked_by: Vec::new(),
        once: false,
        run: None,
        scope: crate::store::workflow::ScopeKind::Worktree,
        result: None,
        kind: Some(LaunchKind::Agent),
        target: None,
        manual: false,
        opener: None,
        child: child.then_some(task_record::ProcessMark {
            pid: 1,
            start: None,
        }),
    }
}

/// A workflow fire whose launch child exited 0 published its run (`rimz workflow run` returns
/// only after `create`), even when the run has left the live directory before the fire reads:
/// `launched` with the run the child printed, and the fresh tree is the run's.
#[test]
fn a_workflow_launch_that_exited_0_is_launched_wherever_its_run_is() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    git_in(&root, &["init", "-q", "-b", "main"]);
    git_in(&root, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let root = root.canonicalize().unwrap();
    record_room(&root).unwrap();
    let catalog = TaskCatalog::load(Some(&root)).unwrap();
    let mut fire = launch_fire(
        &root,
        &catalog,
        TaskEntry {
            kind: Some(LaunchKind::Workflow),
            target: Some("smoke".to_owned()),
            fresh_worktree: Some("night".to_owned()),
            ..TaskEntry::default()
        },
        LoopRunMode::Scheduled,
    );
    fire.prepare_fire_tree().unwrap();
    let tree = fire.fire_dir();
    let (fire_id, _) = fire.open_fire().unwrap();
    let state = StatePaths::for_project_root(&root).unwrap();
    let run = crate::ids::WorkflowRunId::new();
    let plan = fire
        .launch_outcome(
            LaunchKind::Workflow,
            &state,
            fire_id,
            super::super::launch::Ended::Exited {
                code: Some(0),
                tail: String::new(),
                run: Some(run.clone()),
            },
        )
        .unwrap();
    let TaskFirePlan::Done(done) = plan else {
        panic!("expected TaskFirePlan::Done")
    };
    assert_eq!(
        done.record.result,
        LoopRunResult::Launched,
        "{:?}",
        done.record
    );
    assert_eq!(done.record.run_id.as_deref(), Some(run.as_str()));
    drop(fire);
    assert!(tree.exists(), "the run's tree is handed over, not removed");
}

/// The `canceled` row a stop appends carries the task id its key binds, and settles that
/// task's open launch mark, never another task's of the same name.
#[test]
fn a_stop_row_carries_its_task_id_and_settles_that_tasks_mark() {
    for target in 0..2 {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        record_room(&root).unwrap();
        let paths = StatePaths::for_project_root(&root).unwrap();
        let name = format!("same-{}", TaskId::new().short());
        let scopes = [
            task_record::Scope::Machine,
            task_record::Scope::Workspace(WorkspaceId::from_project_root(&root)),
        ];
        let ids = scopes.map(|scope| {
            task_record_store::mutate(|records| {
                let (id, record) = records.bind(&task_record::Binding {
                    scope,
                    name: name.clone(),
                });
                record
                    .marks
                    .insert(root.clone(), dead_mark(FireId::new(&id), true));
                id
            })
            .unwrap()
        });
        let info = RunLockInfo {
            pid: 4242,
            started_at: Timestamp::now(),
        };
        let mut backlog = LockedBacklog::acquire(&paths).unwrap();
        backlog
            .record
            .tasks
            .entry(ids[target].to_string())
            .or_default()
            .stopping = Some(StopMark {
            pid: info.pid,
            started_at: info.started_at,
            cause: StopCause::Stop,
        });
        append_stopped_record(
            ids[target].as_str(),
            &name,
            &root,
            info,
            None,
            None,
            &mut backlog,
        )
        .unwrap();
        drop(backlog);
        let records = task_record::load().unwrap();
        for (index, id) in ids.iter().enumerate() {
            let mark = &records.get(id).unwrap().marks[&root];
            assert_eq!(mark.result.is_some(), index == target, "{target}: {mark:?}");
        }
        let rows = run_log::task_records(&logs_dir(), &ids[target], Some(&root));
        assert!(
            rows.iter().any(|row| row.result == LoopRunResult::Canceled),
            "{target}: {rows:?}"
        );
    }
}

/// A manual fire whose processes died is settled by the pass as the manual fire it was, so it
/// is never read as a scheduled attempt.
#[test]
fn a_dead_manual_fire_settles_as_a_manual_row() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    record_room(&root).unwrap();
    let catalog = TaskCatalog::load(Some(&root)).unwrap();
    let fire = launch_fire(&root, &catalog, agent_entry(), LoopRunMode::Manual);
    let (fire_id, _) = fire.open_fire().unwrap();
    task_record_store::mutate(|records| {
        for mark in records.get_mut(&fire.task_id).unwrap().marks.values_mut() {
            mark.opener = None;
        }
    })
    .unwrap();
    assert_eq!(run_log::settle_open_fires(&root), 1);
    let rows = run_log::task_records(&logs_dir(), &fire.task_id, None);
    let row = rows
        .iter()
        .find(|row| row.fire.as_ref() == Some(&fire_id))
        .unwrap_or_else(|| panic!("{rows:?}"));
    assert_eq!(row.mode, Some(LoopRunMode::Manual), "{row:?}");
}

/// A scheduled `-p` agent fire stamps its task's launch proof at the fire, so a clock pass
/// that reads it before the settle pass sees the attempt.
#[test]
fn a_scheduled_agent_fire_leaves_its_attempt_proof_at_the_fire() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    record_room(&root).unwrap();
    let catalog = TaskCatalog::load(Some(&root)).unwrap();
    let mut fire = launch_fire(&root, &catalog, agent_entry(), LoopRunMode::Scheduled);
    let (fire_id, _) = fire.open_fire().unwrap();
    let state = StatePaths::for_project_root(&root).unwrap();
    let since = fire.now;
    let plan = fire
        .launch_outcome(
            LaunchKind::Agent,
            &state,
            fire_id,
            super::super::launch::Ended::Published(RunId::new()),
        )
        .unwrap();
    assert!(matches!(plan, TaskFirePlan::Done(_)));
    let key = fire.task.key(&fire.name);
    assert!(fires::scheduled_since(&key, None, since).unwrap());
}

/// A fire that displaces its checkout's open mark whose processes died settles that mark
/// first, so its outcome and strike are kept.
#[test]
fn a_fire_settles_the_dead_mark_it_displaces() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    record_room(&root).unwrap();
    let catalog = TaskCatalog::load(Some(&root)).unwrap();
    let fire = launch_fire(&root, &catalog, agent_entry(), LoopRunMode::Scheduled);
    let old = FireId::new(&fire.task_id);
    task_record_store::mutate(|records| {
        records
            .get_mut(&fire.task_id)
            .unwrap()
            .marks
            .insert(fire.launch_checkout(), dead_mark(old.clone(), false));
    })
    .unwrap();
    let (fresh, _) = fire.open_fire().unwrap();
    assert_ne!(fresh, old);
    let rows = run_log::task_records(&logs_dir(), &fire.task_id, None);
    assert!(
        rows.iter()
            .any(|row| row.fire.as_ref() == Some(&old) && row.result == LoopRunResult::Errored),
        "{rows:?}"
    );
    let records = task_record::load().unwrap();
    assert_eq!(records.get(&fire.task_id).unwrap().strikes, 1);
}

/// A fire opens its mark on the task id it cached at its start: a rename mid-fire mints no
/// second id.
#[test]
fn a_fire_opens_its_mark_on_its_cached_task_id() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    record_room(&root).unwrap();
    let catalog = TaskCatalog::load(Some(&root)).unwrap();
    let fire = launch_fire(&root, &catalog, agent_entry(), LoopRunMode::Scheduled);
    task_record_store::mutate(|records| {
        records.get_mut(&fire.task_id).unwrap().binding.name = format!("{}-renamed", fire.name);
    })
    .unwrap();
    let (fire_id, _) = fire.open_fire().unwrap();
    assert_eq!(fire_id.task(), fire.task_id);
    let records = task_record::load().unwrap();
    assert!(
        records
            .get(&fire.task_id)
            .unwrap()
            .marks
            .contains_key(&fire.launch_checkout())
    );
}
