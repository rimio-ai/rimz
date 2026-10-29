use super::*;
use crate::RuntimePaths;
use crate::ids::MuxName;
use crate::launcher::test_backend as fake_backend;
use crate::store::run::RunStatus;

struct Fixture {
    _dir: tempfile::TempDir,
    fresh: FreshLaunch,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("launch")
            .tempdir_in("/tmp")
            .unwrap();
        let workspace_id = crate::WorkspaceId::from_project_root(dir.path());
        let store = crate::Store::open(
            crate::StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap(),
            RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap(),
        )
        .unwrap();
        let workspace = crate::ResolvedWorkspace {
            workspace_id,
            project_root: dir.path().to_owned(),
            cwd_project_root: None,
            root_class: crate::workspace::RootClass::Directory,
            worktree_root: dir.path().to_owned(),
            worktree_branch: None,
            session_name: "room".into(),
            mux_hint: Some(MuxName::Tmux),
        };
        let machine = Arc::new(crate::config::MachineConfig::default());
        let room = RoomContext::from_resolved(
            &workspace,
            machine.clone(),
            MuxName::Tmux,
            crate::room::RoomSizing::OrdinaryTab,
        )
        .unwrap();
        let launch = crate::worktree::resolve_launch_checkout(
            &workspace,
            &machine.agents.worktree,
            None,
            None,
            None,
            None,
            crate::worktree::LaunchBase::Configured,
        )
        .unwrap();
        let layout = crate::harness::spec::parse_layout_spec(
            "claude",
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
        Self {
            _dir: dir,
            fresh: FreshLaunch {
                context: LaunchContext {
                    workspace,
                    store,
                    machine,
                    mux: Some(MuxName::Tmux),
                    root: None,
                    channel: None,
                },
                layout,
                launch,
                checkout: None,
                name: Some("leader".into()),
                generated_name: None,
                team: None,
                channel: None,
                logins: vec!["default".parse().unwrap()],
                managed_launches: vec![crate::agents::ManagedLaunchState::Unsupported],
                ancestry: Some(LaunchAncestry::Peer {
                    launch_generation: 1,
                    launched_by: Some(crate::agents::LaunchedBy {
                        kind: crate::ids::AgentKind::new_unchecked("claude"),
                        agent_id: "caller".into(),
                    }),
                }),
                focus: LaunchFocus::Keep,
                prompt: Some("work".into()),
                prompt_index: Some(0),
                leader_index: Some(0),
                placement: Placement::NewTab,
                pick_session: false,
                loop_reminder: None,
                loop_task: None,
                description: None,
                report_to: ReportTo::Launcher,
                cleanup_worktree: false,
                inherit_cwd: true,
                force_new_tab: true,
                agent_shell_persistence: crate::harness::launch::AgentShellPersistence::Room,
                room: Some(Arc::new(room)),
                recovery: crate::room::AttendedRecovery::RequireExplicitReset,
                entry: RoomEntry::Birth,
                soft_name: false,
                throttle_turn: None,
            },
        }
    }

    fn leader(&self) -> LeaderRun {
        LeaderRun {
            lifecycle: crate::store::run::RunLifecycle::Turn,
            keep: false,
            report_to: ReportTo::Launcher,
            timeout: None,
            retry_of: None,
            cleanup: false,
            subagent: false,
            warn: vec![],
            grace: None,
            loop_task: None,
            workflow: None,
            throttle_turn: None,
            reader: None,
            response_receipt: false,
            completion: RunCompletion::Watched {
                cancellation: RunCancellation::new(),
                install_interrupts: |_| Ok(()),
                termination: None,
            },
        }
    }
}

#[test]
fn session_lifecycle_reaches_the_published_record() {
    let fixture = Fixture::new();
    let mut leader = fixture.leader();
    leader.lifecycle = crate::store::run::RunLifecycle::Session;
    let backend = fake_backend::Backend::default();
    let LaunchOutcome::Opened(opened) = open_fresh(
        fixture.fresh.clone(),
        Some(leader),
        &mut Unattended,
        &backend,
    )
    .unwrap() else {
        panic!("launch did not open");
    };
    let run = opened.run.unwrap();
    assert_eq!(
        crate::harness::run::load(run.store.paths(), &run.record.run_id)
            .unwrap()
            .lifecycle,
        crate::store::run::RunLifecycle::Session,
    );
}

#[test]
fn pending_record_and_bound_waiter_precede_an_instant_completion_at_open() {
    let fixture = Fixture::new();
    let store = fixture.fresh.context.store.clone();
    let backend = fake_backend::Backend {
        on_open: Some(Box::new(move || {
            let records = crate::harness::run::list(store.paths()).unwrap();
            assert_eq!(
                records.len(),
                1,
                "an explicit leader run must not also create a peer run"
            );
            let mut record = records.into_iter().next().unwrap();
            assert_eq!(record.status, RunStatus::Pending);
            assert!(
                crate::store::run::run_waiter_is_live(store.runtime_paths(), &record.run_id)
                    .unwrap()
            );
            record.mark_terminal(RunStatus::Completed, jiff::Timestamp::now());
            crate::harness::run::create(store.paths(), &record).unwrap();
            crate::store::run::wake_run(store.runtime_paths(), &record);
        })),
        ..Default::default()
    };
    let leader = fixture.leader();
    let LaunchOutcome::Opened(opened) = open_fresh(
        fixture.fresh.clone(),
        Some(leader),
        &mut Unattended,
        &backend,
    )
    .unwrap() else {
        panic!("launch did not open");
    };
    assert!(opened.peer.is_none());
    let mut run = opened.run.unwrap();
    let waiter = run.waiter.take().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let record = runtime
        .block_on(waiter.wait_terminal(&run.store, Some(Duration::from_secs(1)), None))
        .unwrap();
    assert_eq!(record.status, RunStatus::Completed);
    let previous = record.run_id;
    let retry = run.retry("retry work".into(), previous.clone());
    let PreparedState::Ready(fresh) = retry.state else {
        panic!("retry discarded preparation")
    };
    assert!(Arc::ptr_eq(
        fresh.room.as_ref().unwrap(),
        fixture.fresh.room.as_ref().unwrap()
    ));
    assert!(fresh.soft_name);
    assert_eq!(fresh.prompt.as_deref(), Some("retry work"));
    assert_eq!(retry.leader_run.unwrap().retry_of, Some(previous));
    let tabs = backend.tabs.lock().unwrap();
    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0].title, "run claude");
    assert!(!tabs[0].focus);
}

#[test]
fn waiter_bind_failure_publishes_no_run_and_opens_no_pane() {
    let fixture = Fixture::new();
    let store = &fixture.fresh.context.store;
    std::fs::remove_dir_all(&store.runtime_paths().sock_dir).unwrap();
    std::fs::write(&store.runtime_paths().sock_dir, "not a directory").unwrap();
    let backend = fake_backend::Backend::default();
    let error = match open_fresh(
        fixture.fresh.clone(),
        Some(fixture.leader()),
        &mut Unattended,
        &backend,
    ) {
        Err(error) => error,
        Ok(_) => panic!("bind unexpectedly succeeded"),
    };
    assert_eq!(error.to_string(), "binding run socket");
    assert!(crate::harness::run::list(store.paths()).unwrap().is_empty());
    assert!(backend.tabs.lock().unwrap().is_empty());
    assert!(backend.appends.lock().unwrap().is_empty());
}

#[test]
fn termination_slot_is_armed_before_the_pending_record_is_written() {
    let fixture = Fixture::new();
    let store = fixture.fresh.context.store.clone();
    let record = RunRecord::new(
        store.paths().workspace_id.clone(),
        crate::ids::AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "work".into(),
        fixture._dir.path().to_owned(),
    );
    let slot = Arc::new(Mutex::new(None));
    let completion = RunCompletion::Watched {
        cancellation: RunCancellation::new(),
        install_interrupts: |_| Ok(()),
        termination: Some(slot.clone()),
    };
    std::thread::scope(|scope| {
        let lock =
            crate::disk::lock::WorkspaceLock::acquire(&store.paths().workspace_lock).unwrap();
        let publish = scope.spawn(|| publish_run(&store, &record, &completion).unwrap());
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while slot.lock().unwrap().is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "termination was not armed before the run write took the workspace lock"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(crate::harness::run::list(store.paths()).unwrap().is_empty());
        assert!(
            crate::store::run::run_waiter_is_live(store.runtime_paths(), &record.run_id).unwrap()
        );
        drop(lock);
        assert!(publish.join().unwrap().is_some());
    });
    assert_eq!(
        crate::harness::run::load(store.paths(), &record.run_id)
            .unwrap()
            .status,
        RunStatus::Pending
    );
}
