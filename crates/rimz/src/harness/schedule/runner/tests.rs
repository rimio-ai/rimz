use std::path::Path;

use super::*;

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
            CheckEcho::Capture,
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
fn worktree_run_locks_do_not_overlap_other_checkouts() {
    let root = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    let make = |checkout: &str| {
        let entry = TaskEntry {
            root: root.path().to_owned(),
            agent: Some("claude".into()),
            prompt: Some("repair".into()),
            stay: true,
            each_worktree: true,
            ..Default::default()
        };
        let mut fire = TaskFire::new(
            "fixer",
            LoadedTask::new("fixer", entry, catalog::TaskSource::Config),
            &catalog,
            LoopRunMode::Scheduled,
            false,
            Timestamp::now(),
            Arc::new(MachineConfig::default()),
            None,
            CheckEcho::Capture,
            Instant::now(),
        )
        .unwrap()
        .with_checkout(Some(root.path().join(checkout)));
        fire.run_lock_path = |name, entry| Ok(entry.root.join(format!("{name}.lock")));
        fire
    };
    let mut first = make("a");
    assert!(matches!(
        first.prepare(&mut |_| Ok(())).unwrap(),
        TaskFirePlan::Resident { .. }
    ));
    let mut second = make("b");
    assert!(matches!(
        second.prepare(&mut |_| Ok(())).unwrap(),
        TaskFirePlan::Resident { .. }
    ));
    let mut same = make("a");
    assert!(
        matches!(same.prepare(&mut |_| Ok(())).unwrap(), TaskFirePlan::Done(done) if done.record.result == LoopRunResult::Overlapped)
    );
}

#[test]
fn vanished_delivery_root_still_resolves_and_finds_no_active_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("vanished");
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
fn spawn_requests_share_loop_cleanup_without_changing_manual_placement() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(dir.path())).unwrap();
    for mode in [LoopRunMode::Scheduled, LoopRunMode::Manual] {
        for keep in [false, true] {
            let entry = TaskEntry {
                agent: Some("claude".to_owned()),
                prompt: Some("check".to_owned()),
                root: dir.path().to_owned(),
                ..TaskEntry::default()
            };
            let fire = TaskFire::new(
                "nightly",
                LoadedTask::new("nightly", entry, catalog::TaskSource::Config),
                &catalog,
                mode,
                keep,
                Timestamp::now(),
                Arc::new(MachineConfig::default()),
                None,
                CheckEcho::Capture,
                Instant::now(),
            )
            .unwrap();
            let request = fire
                .compile_spawn_request(
                    "claude".to_owned(),
                    "check".to_owned(),
                    ManagedLaunchState::Unsupported,
                )
                .unwrap();
            assert_eq!(request.loop_task.as_deref(), Some("nightly"));
            assert_eq!(request.self_cleanup_on_completion, !keep);
            assert_eq!(request.loop_zone, mode == LoopRunMode::Scheduled);
            assert_eq!(
                request.timeout,
                (mode == LoopRunMode::Scheduled).then_some(SCHEDULED_RUN_DEFAULT_TIMEOUT)
            );
        }
    }
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
fn budget_refusal_finishes_as_a_recorded_gate() {
    let check = CheckRecord {
        code: Some(1),
        timed_out: false,
        output: "guard failed".to_owned(),
        output_path: None,
    };
    let mut record = LoopRunRecord::new(
        "nightly",
        LoopRunResult::Completed,
        LoopRunMode::Scheduled,
        12,
    );

    let (presentation, notice) = finish_spawn_effect(
        &mut record,
        SupervisedRunOutcome::BudgetExceeded {
            reason: "room budget reached".to_owned(),
        },
        Some(check.clone()),
        true,
    );

    assert_eq!(record.result, LoopRunResult::BudgetSkipped);
    assert_eq!(
        record.check.as_ref(),
        Some(&check),
        "the guard record survives the refusal"
    );
    assert_eq!(record.error.as_deref(), Some("room budget reached"));
    assert_eq!(presentation, LoopRunPresentation::default());
    assert!(matches!(
        notice,
        TaskFireNotice::Gate { reason } if reason == "room budget reached"
    ));
}

#[test]
fn deadline_expiry_and_relative_age_use_injected_time() {
    let now = Timestamp::from_second(200_000).expect("now");
    let expired = TaskEntry {
        deadline: Some(Timestamp::from_second(199_999).expect("deadline")),
        ..TaskEntry::default()
    };

    assert!(deadline_expired_at(&expired, now));
    assert_eq!(
        relative_age(Timestamp::from_second(198_500).expect("started"), now),
        "25m ago"
    );
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
    let gate = |surplus, after, reading| {
        surplus_gate_in(&surplus_entry(surplus, after), "claude", reading)
    };

    assert_eq!(
        surplus_gate_in(&TaskEntry::default(), "claude", None),
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
        surplus_gate_in(
            &surplus_entry(None, Some("3d")),
            "codex",
            Some(reading(4, 0.9))
        )
        .as_deref(),
        Some("codex 7d window surplus 0.9x below 1.0x"),
        "a bare elapsed floor still demands sustainable headroom"
    );
}

#[test]
fn run_lock_reports_holder_metadata_and_accepts_empty_legacy_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let open = |path: &Path, create: bool| {
        std::fs::OpenOptions::new()
            .create(create)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .expect("open lock")
    };

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
    let guard = match acquire_run_lock_file(open(&path, true), &path).expect("acquire lock") {
        RunLockAttempt::Acquired(guard) => guard,
        RunLockAttempt::Held(_) => panic!("fresh lock should be acquired"),
    };
    let written: RunLockInfo =
        serde_json::from_slice(&std::fs::read(&path).expect("read lock")).expect("parse lock info");
    assert_eq!(written.pid, std::process::id());

    match acquire_run_lock_file(open(&path, false), &path).expect("contend for lock") {
        RunLockAttempt::Held(Some(info)) => assert_eq!(info, written),
        RunLockAttempt::Held(None) => panic!("holder metadata should be readable"),
        RunLockAttempt::Acquired(_) => panic!("held lock should reject contender"),
    }

    drop(guard);
    let before_probe = std::fs::read(&path).expect("read lock before probe");
    assert!(matches!(
        probe_run_lock_file(open(&path, false), &path).expect("probe available lock"),
        RunLockState::Available
    ));
    assert_eq!(
        std::fs::read(&path).expect("read lock after probe"),
        before_probe,
        "probing an available lock should not rewrite its metadata"
    );

    let empty_path = dir.path().join("legacy.lock");
    let holder = open(&empty_path, true);
    holder.try_lock().expect("hold empty lock");
    assert!(
        matches!(
            probe_run_lock_file(open(&empty_path, false), &empty_path).expect("probe empty lock"),
            RunLockState::Held(None)
        ),
        "a held lock with no metadata is still held"
    );
}

#[test]
fn wait_for_run_lock_release_observes_guard_drop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("task.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .expect("open lock");
    let guard = match acquire_run_lock_file(file, &path).expect("acquire lock") {
        RunLockAttempt::Acquired(guard) => guard,
        RunLockAttempt::Held(_) => panic!("fresh lock should be acquired"),
    };
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        drop(guard);
    });

    assert!(
        wait_for_run_lock_release_path(&path, Duration::from_secs(1)).expect("wait for release")
    );
    releaser.join().expect("release thread");
}

#[test]
fn check_polarity_truth_table() {
    let outcome = |passed, timed_out, code| CheckOutcome {
        passed,
        timed_out,
        interrupted: false,
        output: String::new(),
        code,
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
fn skipped_check_preserves_poll_until_and_consumes_watch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let poll_name = "runner-skipped-poll-until";
    let watch_name = "runner-skipped-watch";
    let poll = TaskEntry {
        agent: Some("claude".to_owned()),
        prompt: Some("poll".to_owned()),
        check: Some("false".to_owned()),
        on: Some(CheckOn::Success),
        root: dir.path().to_path_buf(),
        every: Some("1m".to_owned()),
        deadline: Some(
            Timestamp::now()
                .checked_add(jiff::SignedDuration::from_hours(1))
                .expect("future deadline"),
        ),
        ..TaskEntry::default()
    };
    let watch = TaskEntry {
        agent: Some("claude".to_owned()),
        prompt: Some("watch".to_owned()),
        on: Some(CheckOn::Success),
        root: dir.path().to_path_buf(),
        watch: Some(crate::config::WatchSpec::Command("false".to_owned())),
        ..TaskEntry::default()
    };
    let state = StatePaths::for_project_root(dir.path()).expect("state paths");
    crate::harness::schedule::instances::insert(&state, poll_name, &poll).expect("insert poll");
    crate::harness::schedule::instances::insert(&state, watch_name, &watch).expect("insert watch");
    let catalog = TaskCatalog::load(Some(dir.path())).expect("load task catalog");

    let mut poll_fire = skipped_fire(poll_name, &catalog, None);
    let poll_check = poll_fire
        .prepare_check(&mut |_| Ok(()))
        .expect("run poll check");
    assert_eq!(
        poll_check
            .break_value()
            .expect("skipped poll result")
            .record
            .result,
        LoopRunResult::CheckSkipped
    );

    let signal = TriggerSignal {
        name: "wait.runner-skipped-watch".parse().expect("signal name"),
        payload: serde_json::Map::new(),
        source: crate::store::event::SignalSource::Watch,
        watch: Some(crate::harness::schedule::signal::WatchOutcome {
            verdict: WatchVerdict::Exited {
                code: Some(1),
                elapsed_ms: 1234,
            },
            output: String::new(),
            output_path: Some(dir.path().join("watch.log")),
            summary: crate::disk::summary::FileSummary::default(),
        }),
    };
    for on in [CheckOn::Success, CheckOn::Fail, CheckOn::Any] {
        let mut running = signal.clone();
        running.watch.as_mut().unwrap().verdict = WatchVerdict::Running { elapsed_ms: 1_000 };
        running.watch.as_mut().unwrap().output = "still running".to_owned();
        let mut fire = skipped_fire(watch_name, &catalog, Some(running));
        fire.entry.on = Some(on);
        fire.mode = LoopRunMode::Manual;
        let check = fire
            .prepare_check(&mut |_| panic!("supplied watch runs no check"))
            .expect("running watch always delivers");
        assert!(check.is_continue());
        let record = check
            .continue_value()
            .unwrap()
            .expect("watch evidence")
            .record;
        assert_eq!(record.code, None);
        assert!(!record.timed_out);
        assert_eq!(record.output, "still running");
        assert_eq!(record.output_path, Some(dir.path().join("watch.log")));
        assert!(fire.check_trip.is_none());
        fire.mode = LoopRunMode::Scheduled;
        fire.consume_ephemeral().expect("retain running watch");
        assert!(
            crate::harness::schedule::instances::load_from(&state.root)
                .0
                .contains_key(watch_name)
        );
    }
    let mut watch_fire = skipped_fire(watch_name, &catalog, Some(signal));
    let watch_check = watch_fire
        .prepare_check(&mut |_| panic!("supplied watch runs no check"))
        .expect("read watch check");
    let finished = watch_check.break_value().expect("skipped watch result");
    assert_eq!(finished.record.result, LoopRunResult::CheckSkipped);
    assert_eq!(
        finished.record.watch,
        Some(WatchVerdict::Exited {
            code: Some(1),
            elapsed_ms: 1234
        })
    );
    assert_eq!(
        finished.record.check.unwrap().output_path,
        Some(dir.path().join("watch.log"))
    );
    assert_eq!(finished.presentation.check_duration_ms, Some(1234));

    let instances = crate::harness::schedule::instances::load_from(&state.root);
    assert!(instances.0.contains_key(poll_name));
    assert!(!instances.0.contains_key(watch_name));
    crate::harness::schedule::instances::remove(&state, poll_name, None)
        .expect("remove poll fixture");
}

#[test]
fn check_only_terminals_consume_only_one_shots() {
    let dir = tempfile::tempdir().unwrap();
    let state = StatePaths::for_project_root(dir.path()).unwrap();
    for (name, once, command, result) in [
        ("once-pass", true, "true", LoopRunResult::Completed),
        ("standing-pass", false, "true", LoopRunResult::Completed),
        ("once-fail", true, "false", LoopRunResult::Failed),
    ] {
        let entry = TaskEntry {
            check: Some(command.to_owned()),
            root: dir.path().to_path_buf(),
            at: once.then(|| "07:00".to_owned()),
            every: (!once).then(|| "1m".to_owned()),
            ..TaskEntry::default()
        };
        crate::harness::schedule::instances::insert(&state, name, &entry).unwrap();
        let catalog = TaskCatalog::load(Some(dir.path())).unwrap();
        let mut fire = skipped_fire(name, &catalog, None);
        let finished = fire
            .prepare_check(&mut |_| Ok(()))
            .unwrap()
            .break_value()
            .unwrap();
        assert_eq!(finished.record.result, result);
        assert_eq!(finished.presentation.exit_code, None);
        assert_eq!(finished.record.error, None);
        assert!(finished.presentation.check_duration_ms.is_some());
        assert_eq!(
            crate::harness::schedule::instances::load_from(&state.root)
                .0
                .contains_key(name),
            !once
        );
        if !once {
            crate::harness::schedule::instances::remove(&state, name, None).unwrap();
        }
    }
}

#[test]
fn a_scheduled_gate_skip_removes_a_fire_at_row_and_leaves_the_rest() {
    let dir = tempfile::tempdir().unwrap();
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
            check: Some("true".to_owned()),
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
    let action = task.action().cloned().expect("valid fixture action");
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
        CheckEcho::Capture,
        Instant::now(),
    )
    .expect("construct task fire");
    fire.context = Some(FireContext {
        action,
        root,
        scope: None,
    });
    fire
}

#[test]
fn check_room_hook_precedes_execution_and_pins_loop_identity() {
    let dir = tempfile::tempdir().unwrap();
    let project_root = dir.path().canonicalize().unwrap();
    let catalog = TaskCatalog::load(Some(&project_root)).unwrap();
    let worktree = tempfile::tempdir().unwrap();
    let worktree_root = worktree.path().canonicalize().unwrap();
    let entry = TaskEntry {
        check: Some("test -f \"$RIMZ_PROJECT_ROOT/room-ready\" && printf '%s|%s|%s|%s|%s|' \"$RIMZ_LOOP_TASK\" \"$RIMZ_PROJECT_ROOT\" \"$RIMZ_WORKSPACE_ID\" \"${RIMZ_AGENT_ID-unset}\" \"$RIMZ_WORKTREE_PATH\" && pwd -P".to_owned()),
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
        CheckEcho::Capture,
        Instant::now(),
    )
    .unwrap();
    let mut calls = 0;
    let TaskFirePlan::Done(done) = fire
        .prepare(&mut |root| {
            calls += 1;
            assert_eq!(root, &project_root);
            std::fs::write(root.join("room-ready"), "")?;
            Ok(())
        })
        .unwrap()
    else {
        panic!("check finishes without an effect")
    };
    assert_eq!(calls, 1);
    assert_eq!(done.record.result, LoopRunResult::Completed);
    assert_eq!(
        done.record.check.unwrap().output,
        format!(
            "room-check|{}|{}|unset|{}|{}\n",
            project_root.display(),
            WorkspaceId::from_project_root(&project_root),
            worktree_root.display(),
            worktree_root.display()
        )
    );
}

#[test]
fn check_room_hook_is_after_lock_and_deadline_and_records_failure() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = TaskCatalog::load(Some(dir.path())).unwrap();
    let entry = TaskEntry {
        check: Some("touch check-ran".to_owned()),
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
            CheckEcho::Capture,
            Instant::now(),
        )
        .unwrap();
        fire.run_lock_path = |name, entry| {
            let runtime =
                RuntimePaths::under(WorkspaceId::from_project_root(&entry.root), &entry.root)?;
            Ok(runtime.lock_path(format!("loop-run-{name}.lock")))
        };
        fire
    };
    let mut fire = make_fire(entry.clone());
    let error = fire
        .prepare(&mut |_| bail!("room unavailable"))
        .unwrap_err();
    assert_eq!(
        fire.finish_error(&error).record.result,
        LoopRunResult::Errored
    );
    let mut overlap = make_fire(entry.clone());
    assert!(
        matches!(overlap.prepare(&mut |_| panic!("lock refuses before room birth")).unwrap(), TaskFirePlan::Done(done) if done.record.result == LoopRunResult::Overlapped)
    );
    drop(overlap);
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
        error
            .to_string()
            .contains(&vanished.to_string_lossy().to_string())
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
fn polled_watch_probe_runs_to_exit_without_checkin() {
    let dir = tempfile::tempdir().unwrap();
    let output = run_command(
        dir.path(),
        "printf probe; exit 3",
        WatchDeadline::Watch(None),
        CheckEcho::Capture,
        &BTreeMap::new(),
        |_, _| panic!("a probe never checks in"),
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
fn loop_signal_prompts_keep_braces_and_check_evidence() {
    let entry = TaskEntry {
        agent: Some("claude".to_owned()),
        signal: Some("deploy.failed".to_owned()),
        prompt: Some("Inspect {{branch}}".to_owned()),
        ..TaskEntry::default()
    };
    let catalog = TaskCatalog::load(None).unwrap();
    let signal = TriggerSignal {
        name: "deploy.failed".parse().unwrap(),
        payload: serde_json::from_value(serde_json::json!({"branch":"feature"})).unwrap(),
        source: crate::store::event::SignalSource::Cli,
        watch: None,
    };
    let mut fire = TaskFire::new(
        "deployment",
        LoadedTask::new("deployment", entry, catalog::TaskSource::Instance),
        &catalog,
        LoopRunMode::Scheduled,
        false,
        Timestamp::now(),
        Arc::new(MachineConfig::default()),
        Some(signal),
        CheckEcho::Capture,
        Instant::now(),
    )
    .unwrap();
    let outcome = CheckOutcome::new(false, false, "failed guard".to_owned(), Some(1));
    let check = FiredCheck {
        command: "false".to_owned(),
        record: check_record(&outcome),
        outcome,
    };
    let body = fire.resolve_effect_prompt(Some(&check)).unwrap();
    assert!(
        body.starts_with("waited on deploy.failed\nfired [deployment]\n"),
        "{body}"
    );
    assert!(
        body.contains("\n\nInspect {{branch}}\n\n--- check `false` exited 1 ---\nfailed guard"),
        "{body}"
    );
    assert!(!body.contains("armed by"));
    fire.signal = None;
    assert_eq!(
        fire.resolve_effect_prompt(None).unwrap(),
        "waited on deploy.failed\nfired by hand [deployment]\n\nInspect {{branch}}"
    );
    fire.entry.signal = None;
    assert_eq!(
        fire.resolve_effect_prompt(None).unwrap(),
        "Inspect {{branch}}"
    );
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
        when: "team.stage=Done".to_owned(),
        hold: Some("30m".to_owned()),
        held_ms: 1_800_000,
        readings: std::collections::BTreeMap::from([(
            "team.stage".to_owned(),
            Some("Done".to_owned()),
        )]),
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
        CheckEcho::Capture,
        Instant::now(),
    )
    .unwrap()
    .with_condition(Some(evidence.clone()));
    assert_eq!(
        fire.resolve_effect_prompt(None).unwrap(),
        "waited on team.stage=Done\nheld 30m [ship]\n{\"team.stage\":\"Done\"}\n\nInspect {{branch}}"
    );
    let record = fire.terminal_record(LoopRunResult::Completed);
    assert_eq!(
        serde_json::to_value(&record).unwrap()["condition"],
        serde_json::to_value(&evidence).unwrap()
    );
    fire.condition.as_mut().unwrap().hold = None;
    assert!(
        fire.resolve_effect_prompt(None)
            .unwrap()
            .contains("\nfired [ship]\n")
    );
    fire.condition = None;
    assert_eq!(
        fire.resolve_effect_prompt(None).unwrap(),
        "waited on team.stage=Done\nfired by hand [ship]\n\nInspect {{branch}}"
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
        &runtime.shared_rate_limits_path(),
        &crate::agents::RateLimitsCache {
            entries: std::collections::BTreeMap::from([(
                key,
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
        Some(std::collections::BTreeMap::from([(
            AgentKind::new_unchecked("claude"),
            "work".parse().unwrap(),
        )])),
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
        "no current claude 5h window reading; open the room's sidebar or run `rimz providers --refresh`"
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
        FireScope::new(claude.clone(), runtime.clone(), state, None, Some(login))
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
        gate("pi", "work", &no_record).unwrap_err(),
        "pi has no named accounts; only `default` is available; remove `account = \"work\"` from the task"
    );
    let gone = gate("codex", "gone", &no_record).unwrap_err();
    assert!(
        gone.starts_with("codex account `gone` home ")
            && gone.contains("is not a directory; run `"),
        "{gone}"
    );
    assert_eq!(
        gate("codex", "work", &logged_out).unwrap_err(),
        format!(
            "codex account `work` is logged out; log in with `CODEX_HOME={} codex`",
            home.display()
        )
    );
}

#[test]
fn the_account_daily_cap_reads_the_pinned_account_not_the_rooms() {
    use crate::agents::spending::{
        ProviderSpendingCache, SpendWindow, write_provider_spending_cache,
    };

    let dir = tempfile::tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(dir.path());
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
        check: Some("true".to_owned()),
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
        let scope = FireScope::new(
            claude.clone(),
            runtime.clone(),
            state.clone(),
            None,
            Some(login),
        );
        fire.scope_refusal(&scope).map(|(result, _)| result)
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
    let unsupported = "pi has no named accounts; only `default` is available; remove `account = \"ghost\"` from the task";
    for (name, pinned, error, skip) in [
        ("unknown", true, unresolved("codex"), Some(unknown)),
        ("unsupported", true, unresolved("pi"), Some(unsupported)),
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
