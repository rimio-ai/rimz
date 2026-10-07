#[cfg(unix)]
use super::super::tests::minimal_exec_request;
use super::*;
use rimz::agents::PermissionMode;

#[cfg(unix)]
#[test]
fn provider_command_round_trip_preserves_overlay_unsets_and_bytes() {
    use std::os::unix::ffi::OsStringExt;
    let dir = tempfile::tempdir().unwrap();
    let arg = OsString::from_vec(vec![b'x', 0xff]);
    let mut original = Command::new("/bin/provider");
    original.args([OsString::from("--prompt"), arg]);
    original
        .env("PARK_TEST_VALUE", "value")
        .env_remove("PARK_TEST_UNSET");
    original.current_dir(dir.path());
    let bytes = serde_json::to_vec(&SavedCommand::capture(&original)).unwrap();
    let saved: SavedCommand = serde_json::from_slice(&bytes).unwrap();
    let restored = saved.command();
    assert_eq!(restored.get_program(), original.get_program());
    assert_eq!(
        restored.get_args().collect::<Vec<_>>(),
        original.get_args().collect::<Vec<_>>()
    );
    assert_eq!(
        restored.get_envs().collect::<Vec<_>>(),
        original.get_envs().collect::<Vec<_>>()
    );
    assert_eq!(restored.get_current_dir(), original.get_current_dir());
}

#[cfg(unix)]
#[test]
fn thin_argv_retains_globals_and_original_exec_envelope() {
    let original: Vec<OsString> = [
        "rimz-original",
        "--root",
        "/a room",
        "--mux",
        "tmux",
        "agents",
        "exec",
        "claude",
        "--worktree-path",
        "/a checkout",
        "--request",
        "{\"v\":1}",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    let command = reexec_command(
        Path::new("/new/rimz"),
        &original,
        Path::new("/a unit/park.json"),
    );
    assert_eq!(command.get_program(), "/new/rimz");
    let args: Vec<_> = command.get_args().map(OsString::from).collect();
    let mut expected = original[1..].to_vec();
    expected.extend([
        OsString::from("--supervise"),
        OsString::from("/a unit/park.json"),
    ]);
    assert_eq!(args, expected);
}

#[cfg(unix)]
#[test]
fn saved_termios_round_trip_keeps_provider_starting_modes() {
    let pty = nix::pty::openpty(None, None).unwrap();
    let terminal = ProviderTerminal {
        saved: Some(nix::sys::termios::tcgetattr(&pty.slave).unwrap()),
    };
    let bytes = serde_json::to_vec(&SavedTermios::capture(&terminal).unwrap()).unwrap();
    let saved: SavedTermios = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(saved.terminal().unwrap().saved, terminal.saved);
}

#[cfg(unix)]
#[test]
fn unsupported_park_state_names_the_file_and_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("park-state.json");
    let request = minimal_exec_request(
        "claude",
        rimz::harness::launch::ExecAction::Resume {
            session_id: "session".into(),
            extra_args: Vec::new(),
        },
    );
    let mut state = ParkState {
        v: PARK_VERSION + 1,
        request,
        identity: None,
        provider: SavedCommand::capture(&Command::new("/bin/provider")),
        provider_pid: 42,
        spawned_at: jiff::Timestamp::now(),
        relaunch_cap: 0,
        relaunch_wait_ms: 0,
        fresh_launch: false,
        termios: None,
        keep: false,
        survives_parent: false,
        awaiting_reopen: None,
        watchdog: None,
        entered_worktree: None,
        cwd: dir.path().to_owned(),
        isolation: rimz::config::Isolation::Host,
        project_root: dir.path().to_owned(),
    };
    rimz::disk::atomic::write_temp_then_rename_cache(&path, &state).unwrap();
    assert!(
        ParkState::read(&path).is_err(),
        "unsupported version must fail"
    );
    let error = ParkState::read(&path).err().unwrap().to_string();
    assert!(error.contains(&path.display().to_string()), "{error}");
    state.v = PARK_VERSION;
    state.relaunch_cap = 256;
    rimz::disk::atomic::write_temp_then_rename_cache(&path, &state).unwrap();
    assert!(
        ParkState::read(&path).is_err(),
        "relaunch count must fit the retry contract"
    );
}

#[test]
fn terminal_self_cleanup_defers_to_waiter_and_survives_rearm() {
    let state = tempfile::tempdir().unwrap();
    let runtime_root = tempfile::tempdir_in("/tmp").unwrap();
    let workspace_id = rimz::WorkspaceId::from_project_root(state.path());
    let paths = rimz::StatePaths::under(workspace_id.clone(), state.path()).unwrap();
    let runtime = rimz::RuntimePaths::under(workspace_id.clone(), runtime_root.path()).unwrap();
    paths.ensure_dirs().unwrap();
    runtime.ensure_dirs().unwrap();
    let mut record = rimz::store::run::RunRecord::new(
        workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "check".to_owned(),
        state.path().to_owned(),
    );
    record.status = rimz::store::run::RunStatus::Completed;
    rimz::harness::run::create(&paths, &record).unwrap();
    let context = RunPaths {
        run_id: record.run_id.clone(),
        paths,
        runtime,
    };
    let now = Instant::now();
    let mut monitor = RunMonitor::new(true, StopPolicy::RunTerminal, None, now);
    let mut spawn = |_| panic!("terminal cleanup needs no helper");
    assert!(
        monitor.poll(&context, now, false, &mut spawn),
        "background run has no waiter"
    );
    let waiter = rimz::harness::run_wake::RunWaiter::bind(
        &context.runtime,
        rimz::harness::run_wake::ExpectedRunFrame {
            workspace_id,
            run_id: record.run_id.clone(),
        },
        rimz::harness::run::RunCancellation::new(),
    )
    .unwrap();
    assert!(
        !monitor.poll(&context, now, false, &mut spawn),
        "live waiter owns verification and evidence capture"
    );
    record.status = rimz::store::run::RunStatus::Running;
    rimz::harness::run::create(&context.paths, &record).unwrap();
    drop(waiter);
    assert!(
        !monitor.poll(&context, now, false, &mut spawn),
        "rearmed run remains active even without a waiter"
    );
    record.status = rimz::store::run::RunStatus::Completed;
    rimz::harness::run::create(&context.paths, &record).unwrap();
    assert!(
        monitor.poll(&context, now, false, &mut spawn),
        "terminal run is reclaimed once waiter leaves"
    );
}

#[test]
fn a_relaunch_is_asked_again_until_its_wait_ends() {
    use rimz::harness::run::StartupRelaunch::{Due, No, Spent};

    assert_eq!(
        startup_relaunch_at(Instant::now(), CHILD_WAIT_POLL, || Due),
        Due
    );
    assert_eq!(
        startup_relaunch_at(Instant::now(), CHILD_WAIT_POLL, || Spent),
        Spent
    );
    assert_eq!(
        startup_relaunch_at(Instant::now(), CHILD_WAIT_POLL, || No),
        No,
        "a zero wait still asks once"
    );

    let asked = std::cell::Cell::new(0);
    let started = Instant::now();
    let answer = startup_relaunch_at(started + Duration::from_secs(60), CHILD_WAIT_POLL, || {
        asked.set(asked.get() + 1);
        if asked.get() < 3 { Due } else { No }
    });
    assert_eq!(answer, No, "the answer changed during the wait");
    assert_eq!(asked.get(), 3, "the wait ends at the first refusal");
    assert!(started.elapsed() < Duration::from_secs(10));

    let asked = std::cell::Cell::new(0);
    let answer = startup_relaunch_at(
        Instant::now() + CHILD_WAIT_POLL * 3,
        CHILD_WAIT_POLL,
        || {
            asked.set(asked.get() + 1);
            Due
        },
    );
    assert_eq!(answer, Due);
    assert!(asked.get() >= 2, "asked after the wait, not only before it");
}

#[test]
fn folding_card_evidence_asks_have_a_quarter_second_floor() {
    use rimz::harness::run::StartupRelaunch::Due;
    let mut asks = Vec::new();
    startup_relaunch_at(
        Instant::now() + Duration::from_millis(550),
        Duration::from_millis(250),
        || {
            asks.push(Instant::now());
            Due
        },
    );
    assert!(asks.len() >= 2);
    assert!(
        asks.windows(2)
            .all(|pair| pair[1].duration_since(pair[0]) >= Duration::from_millis(250)),
        "card evidence asks must be at least 250 ms apart: {asks:?}"
    );
}

#[cfg(unix)]
#[test]
fn park_errors_before_supervision_fail_the_run_and_end_the_provider() {
    use super::super::tests::test_workspace;
    for invalid_workspace in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let workspace = test_workspace(dir.path());
        let paths = rimz::StatePaths::for_project_root(dir.path()).unwrap();
        let runtime = rimz::RuntimePaths::for_state(&paths).unwrap();
        paths.ensure_dirs().unwrap();
        runtime.ensure_dirs().unwrap();
        let record = rimz::store::run::RunRecord::new(
            workspace.workspace_id.clone(),
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "work".into(),
            dir.path().into(),
        );
        rimz::harness::run::create(&paths, &record).unwrap();
        let mut request = minimal_exec_request(
            "codex",
            rimz::harness::launch::ExecAction::Launch {
                prompt: None,
                extra_args: Vec::new(),
            },
        );
        request.run_id = Some(record.run_id.clone());
        let mut command = Command::new("/bin/sleep");
        command.arg("60");
        let mut child = command.spawn().unwrap();
        let state = ParkState {
            v: PARK_VERSION,
            request,
            identity: None,
            provider: SavedCommand::capture(&command),
            provider_pid: child.id(),
            spawned_at: jiff::Timestamp::now(),
            relaunch_cap: 0,
            relaunch_wait_ms: 0,
            fresh_launch: false,
            termios: None,
            keep: false,
            survives_parent: false,
            awaiting_reopen: None,
            watchdog: None,
            entered_worktree: None,
            cwd: dir.path().into(),
            isolation: rimz::config::Isolation::Host,
            project_root: dir.path().into(),
        };
        let path = paths.park_state_file(Some("test"), None, std::process::id());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        rimz::disk::atomic::write_temp_then_rename_cache(&path, &state).unwrap();
        let args = ExecArgs {
            kind: "codex".into(),
            worktree_path: None,
            request: "invalid envelope".into(),
            supervise: Some(path),
        };
        let globals = GlobalFlags {
            mux: None,
            zellij: false,
            tmux: false,
            color: crate::cli::ColorWhen::Auto,
            root: Some(if invalid_workspace {
                dir.path().join("missing")
            } else {
                dir.path().into()
            }),
        };
        assert!(run_exec(args, &globals).is_err());
        let stopped = rimz::proc::comm_and_ppid(child.id()).is_none();
        let failed = rimz::harness::run::load(&paths, &record.run_id).unwrap();
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(
            failed.status,
            rimz::store::run::RunStatus::Failed,
            "early park error must fail a retained run (workspace error={invalid_workspace})"
        );
        assert!(stopped, "early park error must end the retained provider");
    }
}

#[cfg(unix)]
#[test]
fn failed_settle_store_reopen_still_fails_the_run_without_folding() {
    use std::os::unix::process::ExitStatusExt as _;
    let dir = tempfile::tempdir().unwrap();
    let workspace = super::super::tests::test_workspace(dir.path());
    let paths = rimz::StatePaths::for_project_root(dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let record = rimz::store::run::RunRecord::new(
        workspace.workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "work".into(),
        dir.path().into(),
    );
    rimz::harness::run::create(&paths, &record).unwrap();
    let mut request = minimal_exec_request(
        "codex",
        rimz::harness::launch::ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    );
    request.run_id = Some(record.run_id.clone());
    std::fs::remove_dir_all(&paths.cache_dir).unwrap();
    std::fs::write(&paths.cache_dir, b"not a directory").unwrap();
    let exit = ParkExit {
        request,
        identity: None,
        cwd: dir.path().into(),
        isolation: rimz::config::Isolation::Host,
        entered_worktree: None,
        keep: false,
        outcome: ExecOutcome {
            status: ExitStatus::from_raw(1 << 8),
            abrupt: true,
            parent_ended: false,
            parent_watchdog: None,
        },
        terminal_grace: Duration::ZERO,
        startup_deaths: None,
    };
    let globals = GlobalFlags {
        mux: None,
        zellij: false,
        tmux: false,
        root: None,
        color: crate::cli::ColorWhen::Auto,
    };
    let before = rimz::testkit::carryover_bytes_parsed();
    assert!(settle_park_exit(exit, &workspace, &globals).is_err());
    assert_eq!(
        rimz::harness::run::load(&paths, &record.run_id)
            .unwrap()
            .status,
        rimz::store::run::RunStatus::Failed,
        "a failed reopen must not strand a nonterminal run"
    );
    assert_eq!(rimz::testkit::carryover_bytes_parsed(), before);
}

mod monitor {
    use super::*;
    use std::rc::Rc;

    fn fixture(
        status: rimz::store::run::RunStatus,
    ) -> (tempfile::TempDir, RunPaths, rimz::store::run::RunRecord) {
        let dir = tempfile::tempdir().unwrap();
        let id = rimz::WorkspaceId::from_project_root(dir.path());
        let paths = rimz::StatePaths::under(id.clone(), dir.path()).unwrap();
        let runtime = rimz::RuntimePaths::under(id.clone(), dir.path()).unwrap();
        rimz::Store::open(paths.clone(), runtime.clone()).unwrap();
        let mut record = rimz::store::run::RunRecord::new(
            id,
            AgentKind::new_unchecked("codex"),
            rimz::agents::PermissionMode::Auto,
            "work".into(),
            dir.path().into(),
        );
        record.status = status;
        rimz::harness::run::create(&paths, &record).unwrap();
        let context = RunPaths {
            run_id: record.run_id.clone(),
            paths,
            runtime,
        };
        (dir, context, record)
    }

    #[test]
    fn strand_is_single_flight_stops_with_its_phase_and_backs_off_failures() {
        let (_dir, context, mut record) = fixture(rimz::store::run::RunStatus::Running);
        record.parked_at = Some(jiff::Timestamp::now());
        rimz::harness::run::create(&context.paths, &record).unwrap();
        let now = Instant::now();
        let mut monitor = RunMonitor::new(false, StopPolicy::RunTerminal, None, now);
        let mut calls = Vec::new();
        let statuses = Rc::new(RefCell::new(Vec::new()));
        let children = statuses.clone();
        let mut spawn = |duty| {
            calls.push(duty);
            let status = Rc::new(RefCell::new(None));
            children.borrow_mut().push(status.clone());
            Ok(DutyChild::Fake(status))
        };
        assert!(!monitor.poll(&context, now, false, &mut spawn));
        assert_eq!(
            statuses.borrow().len(),
            1,
            "the park launches a strand duty"
        );
        assert!(!monitor.poll(&context, now + PARK_STRAND_POLL, false, &mut spawn));
        assert_eq!(
            statuses.borrow().len(),
            1,
            "a live strand is never duplicated"
        );
        *statuses.borrow()[0].borrow_mut() = Some(1);
        let failed = now + PARK_STRAND_POLL;
        monitor.poll(&context, failed, false, &mut spawn);
        monitor.poll(
            &context,
            failed + PARK_STRAND_POLL - RUN_MONITOR_POLL,
            false,
            &mut spawn,
        );
        assert_eq!(
            statuses.borrow().len(),
            1,
            "record ticks do not accelerate retry"
        );
        monitor.poll(&context, failed + PARK_STRAND_POLL, false, &mut spawn);
        assert_eq!(statuses.borrow().len(), 2);
        record.parked_at = None;
        rimz::harness::run::create(&context.paths, &record).unwrap();
        monitor.poll(
            &context,
            failed + PARK_STRAND_POLL + RUN_MONITOR_POLL,
            false,
            &mut spawn,
        );
        assert!(
            monitor.strand.is_some(),
            "leaving the park lets its resident helper finish"
        );
        *statuses.borrow()[1].borrow_mut() = Some(0);
        monitor.poll(&context, failed + 2 * PARK_STRAND_POLL, false, &mut spawn);
        assert!(monitor.strand.is_none(), "the finished helper is reaped");
        assert_eq!(
            calls,
            vec![
                Duty::Strand {
                    run_id: context.run_id.clone()
                };
                2
            ]
        );
    }

    #[test]
    fn receipt_is_triggered_and_spaced_and_a_new_parent_event_invalidates_success() {
        let (_dir, context, _record) = fixture(rimz::store::run::RunStatus::Completed);
        let now = Instant::now();
        let mut monitor = RunMonitor::new(true, StopPolicy::ParentReceived, None, now);
        let mut calls = Vec::new();
        let statuses = Rc::new(RefCell::new(Vec::new()));
        let children = statuses.clone();
        let mut spawn = |duty| {
            calls.push(duty);
            let status = Rc::new(RefCell::new(None));
            children.borrow_mut().push(status.clone());
            Ok(DutyChild::Fake(status))
        };
        assert!(!monitor.poll(&context, now, false, &mut spawn));
        assert_eq!(
            statuses.borrow().len(),
            1,
            "completion launches a receipt duty"
        );
        *statuses.borrow()[0].borrow_mut() = Some(3);
        assert!(!monitor.poll(&context, now + RUN_MONITOR_POLL, false, &mut spawn));
        assert!(!monitor.poll(&context, now + 2 * RUN_MONITOR_POLL, true, &mut spawn));
        assert_eq!(
            statuses.borrow().len(),
            1,
            "a trigger respects the one-second floor"
        );
        assert!(!monitor.poll(&context, now + PARENT_RECEIPT_POLL, false, &mut spawn));
        assert_eq!(statuses.borrow().len(), 2);
        *statuses.borrow()[1].borrow_mut() = Some(0);
        assert!(
            !monitor.poll(
                &context,
                now + PARENT_RECEIPT_POLL + RUN_MONITOR_POLL,
                true,
                &mut spawn
            ),
            "a newer parent event invalidates the receipt before cleanup"
        );
        assert!(!monitor.poll(&context, now + 2 * PARENT_RECEIPT_POLL, false, &mut spawn));
        *statuses.borrow()[2].borrow_mut() = Some(0);
        assert!(monitor.poll(
            &context,
            now + 2 * PARENT_RECEIPT_POLL + RUN_MONITOR_POLL,
            false,
            &mut spawn
        ));
        assert!(monitor.poll(&context, now + 3 * PARENT_RECEIPT_POLL, false, &mut spawn));
        assert_eq!(
            statuses.borrow().len(),
            3,
            "quiet receipts do not poll every second"
        );
        std::fs::create_dir_all(&context.paths.messages_dir).unwrap();
        std::fs::write(
            context.paths.messages_dir.join("messages.jsonl"),
            "queue changed",
        )
        .unwrap();
        assert!(!monitor.poll(&context, now + 3 * PARENT_RECEIPT_POLL, false, &mut spawn));
        assert_eq!(
            statuses.borrow().len(),
            4,
            "a queue stamp change starts a fresh check"
        );
        *statuses.borrow()[3].borrow_mut() = Some(3);
        assert!(!monitor.poll(&context, now + 4 * PARENT_RECEIPT_POLL, false, &mut spawn));
        monitor.poll(&context, now + Duration::from_secs(62), false, &mut spawn);
        assert_eq!(statuses.borrow().len(), 4);
        monitor.poll(&context, now + Duration::from_secs(63), false, &mut spawn);
        assert_eq!(
            statuses.borrow().len(),
            5,
            "the fallback eventually rechecks an unchanged queue"
        );
        assert_eq!(
            calls,
            vec![
                Duty::Receipt {
                    run_id: context.run_id.clone(),
                    report: true
                },
                Duty::Receipt {
                    run_id: context.run_id.clone(),
                    report: false
                },
                Duty::Receipt {
                    run_id: context.run_id.clone(),
                    report: false
                },
                Duty::Receipt {
                    run_id: context.run_id.clone(),
                    report: false
                },
                Duty::Receipt {
                    run_id: context.run_id.clone(),
                    report: false
                },
            ]
        );
    }

    #[test]
    fn receipt_after_waiter_leaves_requires_a_fresh_answer() {
        let (_dir, context, record) = fixture(rimz::store::run::RunStatus::Completed);
        let waiter = rimz::harness::run_wake::RunWaiter::bind(
            &context.runtime,
            rimz::harness::run_wake::ExpectedRunFrame {
                workspace_id: record.workspace_id.clone(),
                run_id: record.run_id.clone(),
            },
            rimz::harness::run::RunCancellation::new(),
        )
        .unwrap();
        let now = Instant::now();
        let mut monitor = RunMonitor::new(true, StopPolicy::ParentReceived, None, now);
        let statuses = Rc::new(RefCell::new(Vec::new()));
        let mut spawn = |_| {
            let status = Rc::new(RefCell::new(None));
            statuses.borrow_mut().push(status.clone());
            Ok(DutyChild::Fake(status))
        };
        assert!(!monitor.poll(&context, now, false, &mut spawn));
        *statuses.borrow()[0].borrow_mut() = Some(0);
        assert!(!monitor.poll(&context, now + RUN_MONITOR_POLL, false, &mut spawn));
        drop(waiter);
        assert!(
            !monitor.poll(&context, now + 2 * RUN_MONITOR_POLL, false, &mut spawn),
            "a pre-departure yes cannot close the pane"
        );
        assert!(!monitor.poll(&context, now + PARENT_RECEIPT_POLL, false, &mut spawn));
        assert_eq!(
            statuses.borrow().len(),
            2,
            "the departure requests a fresh answer"
        );
        *statuses.borrow()[1].borrow_mut() = Some(0);
        assert!(monitor.poll(
            &context,
            now + PARENT_RECEIPT_POLL + RUN_MONITOR_POLL,
            false,
            &mut spawn
        ));
    }

    #[test]
    fn parent_frame_reaches_receipt_consumer_within_one_poll() {
        use rimz::harness::parent_watch::{ParentWatchdog, ProbeConfirm, WatchdogSeed};
        let (_dir, context, _record) = fixture(rimz::store::run::RunStatus::Completed);
        let seed = WatchdogSeed {
            child_kind: AgentKind::new_unchecked("codex"),
            child_launch_id: "child".into(),
            parent_kind: AgentKind::new_unchecked("codex"),
            parent_refs: vec!["parent".into()],
            members: std::collections::BTreeMap::from([("parent".into(), false)]),
            parent_pane: None,
            child_pane: None,
            session_name: "room".into(),
            cursor: rimz::store::event_log::LogExtent {
                generation: 0,
                offset: 0,
            },
        };
        let watch =
            ParentWatchdog::from_seed(seed, context.paths.clone(), |_, _| ProbeConfirm::Unknown)
                .start();
        let now = Instant::now();
        let mut monitor = RunMonitor::new(true, StopPolicy::ParentReceived, None, now);
        let mut calls = 0;
        let mut spawn = |_| {
            calls += 1;
            Ok(DutyChild::Fake(Rc::new(RefCell::new(Some(3)))))
        };
        assert!(!monitor.poll(&context, now, watch.take_parent_changed(), &mut spawn));
        watch.set_receipt_waiting(true);
        assert!(!monitor.poll(
            &context,
            now + RUN_MONITOR_POLL,
            watch.take_parent_changed(),
            &mut spawn
        ));
        rimz::store::event_log::append(
            &context.paths.events_log,
            &rimz::store::event::EventEnvelope::agent_lifecycle(
                context.paths.workspace_id.clone(),
                "room",
                "codex",
                "test",
                &rimz::agents::AgentLifecycleObservation::new(
                    Some("parent".into()),
                    rimz::agents::LifecycleSignal::TurnEnded {
                        errored: false,
                        parked_on_background: false,
                        turn_id: None,
                    },
                ),
            ),
        )
        .unwrap();
        let deadline = Instant::now() + PARENT_RECEIPT_POLL + RUN_MONITOR_POLL;
        loop {
            monitor.poll(
                &context,
                Instant::now(),
                watch.take_parent_changed(),
                &mut spawn,
            );
            if monitor.last_receipt_check.is_some_and(|last| last > now) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "parent frame must request receipt within one poll"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        watch.set_receipt_waiting(false);
        assert_eq!(calls, 2);
    }

    #[test]
    fn detached_terminal_cleanup_respects_reopen_and_waiter_without_helpers() {
        let (_dir, context, mut record) = fixture(rimz::store::run::RunStatus::Completed);
        record.report_to = rimz::store::run::ReportTo::Nobody;
        rimz::harness::run::create(&context.paths, &record).unwrap();
        let now = Instant::now();
        let mut monitor = RunMonitor::new(
            true,
            StopPolicy::ParentReceived,
            Some(record.follow_ups),
            now,
        );
        let mut spawn = |_| panic!("detached cleanup needs no helper");
        assert!(!monitor.poll(&context, now, false, &mut spawn));
        record.follow_ups += 1;
        rimz::harness::run::create(&context.paths, &record).unwrap();
        assert!(monitor.poll(&context, now, false, &mut spawn));
        assert_eq!(monitor.awaiting_reopen, None);
    }
}
