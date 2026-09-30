use super::*;

/// A resumed session is bound to exactly one pane at a time, and the
/// replacement stamps that binding before it spawns its provider. The
/// exiting wrapper's argv-identity fallback therefore reads the binding to
/// tell itself apart from a replacement that already took the session
/// over: superseded, it ends nothing and retires nothing.
#[test]
fn a_session_bound_to_another_pane_supersedes_the_exiting_wrapper() {
    let kind = AgentKind::new_unchecked("claude");
    let session = AgentSessionId::from("resumed");
    let pane = |id: &str| rimz::ids::PaneId::parse(id).expect("normalized pane id");
    let bound = |id: Option<&str>| {
        let mut agent = rimz::testkit::agent_state("claude", "resumed", jiff::Timestamp::now());
        agent.pane = id.map(|id| rimz::pane::PaneRef::from_id(pane(id)));
        agent
    };
    let own = pane("tmux:%1");

    assert!(
        session_bound_to_another_pane(&[bound(Some("tmux:%2"))], &kind, &session, Some(&own)),
        "the replacement's pane supersedes this wrapper"
    );
    assert!(
        !session_bound_to_another_pane(&[bound(Some("tmux:%1"))], &kind, &session, Some(&own)),
        "the wrapper's own binding is not a supersession"
    );
    assert!(
        !session_bound_to_another_pane(&[bound(None)], &kind, &session, Some(&own)),
        "an unbound session leaves the fallback to decide"
    );
    assert!(
        !session_bound_to_another_pane(&[], &kind, &session, Some(&own)),
        "no row for the session is no evidence of a replacement"
    );
    assert!(
        session_bound_to_another_pane(&[bound(Some("tmux:%1"))], &kind, &session, None),
        "a wrapper with no pane of its own can claim no binding"
    );
    assert!(
        !session_bound_to_another_pane(
            &[bound(Some("tmux:%2"))],
            &AgentKind::new_unchecked("codex"),
            &session,
            Some(&own)
        ),
        "another provider's binding on the same session id is not this one"
    );
}

/// The reporter's workspace, scoped to a fixture's own tempdir.
fn test_workspace(root: &std::path::Path) -> rimz::ResolvedWorkspace {
    rimz::ResolvedWorkspace {
        workspace_id: rimz::WorkspaceId::from_project_root(root),
        project_root: root.to_owned(),
        cwd_project_root: None,
        root_class: rimz::workspace::RootClass::Directory,
        worktree_root: root.to_owned(),
        worktree_branch: None,
        session_name: "room".to_owned(),
        mux_hint: None,
    }
}

#[test]
fn monitor_settles_stranded_park_with_and_without_self_cleanup() {
    for self_cleanup in [false, true] {
        let state = tempfile::tempdir().unwrap();
        let workspace_id = rimz::WorkspaceId::from_project_root(state.path());
        let paths = rimz::StatePaths::under(workspace_id.clone(), state.path()).unwrap();
        let runtime =
            rimz::RuntimePaths::under(workspace_id.clone(), &state.path().join("rt")).unwrap();
        let store = rimz::Store::open(paths, runtime).unwrap();
        let mut record = rimz::store::run::RunRecord::new(
            workspace_id,
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "check".to_owned(),
            state.path().to_owned(),
        );
        record.status = rimz::store::run::RunStatus::Running;
        record.agent_id = Some("session".into());
        record.parked_at = Some(jiff::Timestamp::now());
        rimz::harness::run::create(store.paths(), &record).unwrap();
        let context = RunExecContext {
            run_id: record.run_id.clone(),
            store,
            session_name: "room".to_owned(),
            workspace: test_workspace(state.path()),
        };
        let now = Instant::now();
        let mut monitor = RunMonitor {
            self_cleanup,
            stop_policy: StopPolicy::RunTerminal,
            awaiting_reopen: None,
            next_receipt_check: now,
            reported_revision: None,
            next_park_check: now,
            previous: None,
        };
        assert!(!monitor.poll(&context, now));
        assert_eq!(monitor.previous, record.parked_at);
        assert!(!monitor.poll(&context, now + RUN_MONITOR_POLL));
        assert!(
            !context.is_terminal(),
            "record ticks must not accelerate strand checks"
        );
        assert!(!monitor.poll(&context, now + PARK_STRAND_POLL));
        let failed = context.load_record().unwrap();
        assert_eq!(failed.status, rimz::store::run::RunStatus::Failed);
        assert!(failed.failure_tail.is_some());
        assert_eq!(failed.parked_at, None);
        assert_eq!(monitor.previous, None);
        assert_eq!(
            monitor.poll(&context, now + PARK_STRAND_POLL + RUN_MONITOR_POLL),
            self_cleanup
        );
    }
}

/// The one park nothing else can end is a settled fleet whose digest was
/// lost: the child's reporter swallowed its error and the parent is at
/// rest waiting for exactly that digest. The parked wrapper therefore
/// repairs the fleet before each strand check, and the run lives to
/// complete on the digest's turn instead of failing stranded.
#[test]
fn parked_monitor_repairs_a_lost_fleet_digest_instead_of_failing() {
    assert_parked_monitor_repairs_report(false);
}

#[test]
fn parked_monitor_settles_a_dead_team_and_waits_for_its_report() {
    assert_parked_monitor_repairs_report(true);
}

fn assert_parked_monitor_repairs_report(team: bool) {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = rimz::WorkspaceId::from_project_root(dir.path());
    let paths = rimz::StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap();
    let runtime = rimz::RuntimePaths::under(workspace_id.clone(), &dir.path().join("rt")).unwrap();
    let store = rimz::Store::open(paths, runtime).unwrap();
    let kind = AgentKind::new_unchecked("codex");
    let register = |name: &str, pane: &str, parent: Option<&str>| {
        let mut observation = rimz::agents::AgentLifecycleObservation::new(
            Some(AgentSessionId::from(name)),
            rimz::agents::LifecycleSignal::Registered,
        );
        observation.agent_name = Some(name.to_owned());
        observation.pane_id = Some(rimz::ids::PaneId::parse(pane).unwrap());
        if let Some(parent) = parent {
            if team {
                observation.launch.team = Some("forge".into());
                observation.launch.channel = Some("x".into());
                observation.launch.launched_by = Some(Box::new(rimz::agents::LaunchedBy {
                    kind: kind.clone(),
                    agent_id: parent.into(),
                }));
            } else {
                observation.launch.parent_agent_id = Some(AgentSessionId::from(parent));
                observation.launch.parent_agent_kind = Some(kind.clone());
            }
            observation.launch.launch_depth = Some(1);
        }
        store
            .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
                session_name: "room",
                agent_kind: kind.clone(),
                event_name: "test",
                observation: &observation,
                spawned_subagents: &[],
            })
            .unwrap();
    };
    register("parent", "tmux:%1", None);
    register("child", "tmux:%2", Some("parent"));

    if team {
        store
            .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
                session_name: "room",
                agent_kind: kind.clone(),
                event_name: "test",
                observation: &rimz::agents::AgentLifecycleObservation::new(
                    Some("child".into()),
                    rimz::agents::LifecycleSignal::Ended,
                ),
                spawned_subagents: &[],
            })
            .unwrap();
    }

    let run = |session: &str, status: rimz::store::run::RunStatus| {
        let mut record = rimz::store::run::RunRecord::new(
            workspace_id.clone(),
            kind.clone(),
            PermissionMode::Auto,
            "work".to_owned(),
            dir.path().to_owned(),
        );
        record.status = status;
        record.agent_id = Some(AgentSessionId::from(session));
        record.agent_name = Some(session.to_owned());
        record
    };
    let mut parent_run = run("parent", rimz::store::run::RunStatus::Running);
    parent_run.parked_at = Some(jiff::Timestamp::now());
    let mut child_run = run("child", rimz::store::run::RunStatus::Completed);
    if team {
        child_run.status = rimz::store::run::RunStatus::Running;
        child_run.team = Some(rimz::store::run::TeamRun {
            launch_id: "child".into(),
            instance: "forge#x".into(),
        });
    } else {
        child_run.subagent = true;
    }
    for record in [&parent_run, &child_run] {
        rimz::harness::run::create(store.paths(), record).unwrap();
    }
    let parent_id = AgentSessionId::from("parent");
    if !team {
        assert_eq!(
            rimz::harness::owed::owed_wake(&store, &kind, &parent_id).unwrap(),
            Some(rimz::harness::owed::OwedWake::Subagents),
            "a settled child nobody reported holds the park",
        );
    }

    let context = RunExecContext {
        run_id: parent_run.run_id.clone(),
        store,
        session_name: "room".to_owned(),
        workspace: test_workspace(dir.path()),
    };
    let now = Instant::now();
    let mut monitor = RunMonitor {
        self_cleanup: false,
        stop_policy: StopPolicy::RunTerminal,
        awaiting_reopen: None,
        next_receipt_check: now,
        reported_revision: None,
        next_park_check: now,
        previous: None,
    };
    assert!(!monitor.poll(&context, now));

    assert_eq!(
        rimz::harness::owed::owed_wake(&context.store, &kind, &parent_id).unwrap(),
        Some(rimz::harness::owed::OwedWake::WakeInFlight),
        "the repair turns the owed fleet into a digest in flight",
    );
    assert_eq!(context.store.list_messages().unwrap().len(), 1);
    if team {
        let messages = context.store.list_messages().unwrap();
        assert_eq!(
            messages[0].sender,
            rimz::store::message::MessageSender::Harness {
                notice: rimz::store::message::HarnessNotice::TeamReport,
            }
        );
        assert_eq!(
            rimz::harness::run::load(context.store.paths(), &child_run.run_id)
                .unwrap()
                .status,
            rimz::store::run::RunStatus::Failed
        );
    }
    let parked = context.load_record().unwrap();
    assert_eq!(parked.status, rimz::store::run::RunStatus::Running);
    assert_eq!(parked.parked_at, parent_run.parked_at);
    assert_eq!(
        monitor.previous, None,
        "a repaired park is live, not stranded"
    );
}

#[test]
fn only_terminal_subagent_resumes_await_reopening() {
    use rimz::harness::launch::{ExecAction, ExecRequest};
    use rimz::store::run::{RunRecord, RunStatus};

    let mut request = ExecRequest {
        isolation_default: None,
        kind: AgentKind::new_unchecked("codex"),
        action: ExecAction::Resume {
            session_id: "child".into(),
            extra_args: Vec::new(),
        },
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        team_prompt: None,
        skills: None,
        allowed_tools: None,
        provider_account: Default::default(),
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        subagent: true,
        identity: Default::default(),
    };
    let mut record = RunRecord::new(
        rimz::WorkspaceId::from_project_root(Path::new("/project")),
        request.kind.clone(),
        PermissionMode::Auto,
        "work".into(),
        Path::new("/project").into(),
    );
    record.status = RunStatus::Completed;
    record.follow_ups = 7;
    assert_eq!(resumed_run_follow_ups(&request, &record), Some(7));
    request.subagent = false;
    assert_eq!(resumed_run_follow_ups(&request, &record), None);
    request.subagent = true;
    record.status = RunStatus::Running;
    assert_eq!(resumed_run_follow_ups(&request, &record), None);
    record.status = RunStatus::Completed;
    for action in [
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
        ExecAction::Fork {
            session_id: "child".into(),
            extra_args: Vec::new(),
        },
    ] {
        request.action = action;
        assert_eq!(resumed_run_follow_ups(&request, &record), None);
    }
}

#[test]
fn terminal_child_waits_for_its_receiving_parent_turn() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = test_workspace(dir.path());
    let paths = rimz::StatePaths::under(workspace.workspace_id.clone(), dir.path()).unwrap();
    let runtime =
        rimz::RuntimePaths::under(workspace.workspace_id.clone(), &dir.path().join("rt")).unwrap();
    let store = rimz::Store::open(paths, runtime).unwrap();
    let kind = AgentKind::new_unchecked("codex");
    let observe = |name: &str, signal| {
        let mut observation =
            rimz::agents::AgentLifecycleObservation::new(Some(name.into()), signal);
        observation.agent_name = Some(name.into());
        observation.pane_id = Some(
            rimz::ids::PaneId::parse(if name == "child" {
                "tmux:%2"
            } else {
                "tmux:%1"
            })
            .unwrap(),
        );
        if name == "child" {
            observation.launch.parent_agent_id = Some("parent".into());
            observation.launch.parent_agent_kind = Some(kind.clone());
            observation.launch.launch_depth = Some(1);
        }
        store
            .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
                session_name: "room",
                agent_kind: kind.clone(),
                event_name: "test",
                observation: &observation,
                spawned_subagents: &[],
            })
            .unwrap();
    };
    use rimz::agents::LifecycleSignal;
    let ended = || LifecycleSignal::TurnEnded {
        errored: false,
        parked_on_background: false,
        turn_id: None,
    };
    observe("parent", LifecycleSignal::Registered);
    observe("child", LifecycleSignal::Registered);
    observe("child", ended());
    let mut record = rimz::store::run::RunRecord::new(
        workspace.workspace_id.clone(),
        kind.clone(),
        PermissionMode::Auto,
        "work".into(),
        dir.path().into(),
    );
    record.agent_id = Some("child".into());
    record.agent_name = Some("child".into());
    record.subagent = true;
    record.status = rimz::store::run::RunStatus::Completed;
    rimz::harness::run::create(store.paths(), &record).unwrap();
    let context = RunExecContext {
        run_id: record.run_id.clone(),
        store: store.clone(),
        session_name: "room".into(),
        workspace,
    };
    let mut now = Instant::now();
    let mut monitor = RunMonitor {
        self_cleanup: true,
        stop_policy: StopPolicy::ParentReceived,
        awaiting_reopen: None,
        next_receipt_check: now,
        reported_revision: None,
        next_park_check: now,
        previous: None,
    };
    record.status = rimz::store::run::RunStatus::Running;
    rimz::harness::run::create(store.paths(), &record).unwrap();
    assert!(!monitor.poll(&context, now), "launch waits for completion");
    record.status = rimz::store::run::RunStatus::Completed;
    rimz::harness::run::create(store.paths(), &record).unwrap();
    assert!(
        !monitor.poll(&context, now),
        "unreceived output holds the child"
    );
    let reported = context.load_record().unwrap();
    assert!(
        reported.report_message_id.is_some(),
        "report before provider exit"
    );
    record = reported;
    let digest = store
        .list_messages()
        .unwrap()
        .into_iter()
        .find(|message| Some(&message.message_id) == record.report_message_id.as_ref())
        .unwrap();
    store.record_sent_batch(&[digest], "room").unwrap();
    observe("parent", LifecycleSignal::TurnStarted { turn_id: None });
    store
        .confirm_delivered_for_card(
            &kind,
            &"parent".into(),
            Some("parent"),
            rimz::store::writer::DeliveryAck::TurnStarted { prompt: None },
            "room",
        )
        .unwrap();
    now += Duration::from_secs(1);
    assert!(!monitor.poll(&context, now));
    observe("parent", ended());
    now += Duration::from_secs(1);
    assert!(monitor.poll(&context, now));
    record.report_message_id = None;
    record.joined_at = Some(jiff::Timestamp::now());
    rimz::harness::run::create(store.paths(), &record).unwrap();
    observe("child", LifecycleSignal::TurnStarted { turn_id: None });
    now += Duration::from_secs(1);
    assert!(!monitor.poll(&context, now));
    observe("child", ended());
    observe("parent", LifecycleSignal::Ended);
    now += Duration::from_secs(1);
    assert!(monitor.poll(&context, now));
    observe("parent", LifecycleSignal::Registered);
    let child = store
        .snapshot_cached()
        .unwrap()
        .agents
        .into_iter()
        .find(|agent| agent.agent_id.as_str() == "child")
        .unwrap();
    let message = rimz::store::message::MessageRecord::new(
        record.workspace_id.clone(),
        &child,
        "follow up".into(),
        rimz::store::message::DeliveryGate::Done,
    );
    store.queue_message(&message, "room").unwrap();
    now += Duration::from_secs(1);
    assert!(
        !monitor.poll(&context, now),
        "queued follow-up holds the child"
    );
    store.record_sent_batch(&[message], "room").unwrap();
    now += Duration::from_secs(1);
    assert!(
        !monitor.poll(&context, now),
        "pane send before TurnStarted still holds the child"
    );
    store
        .confirm_delivered_for_card(
            &kind,
            &child.agent_id,
            child.name.as_deref(),
            rimz::store::writer::DeliveryAck::TurnStarted { prompt: None },
            "room",
        )
        .unwrap();
    now += Duration::from_secs(1);
    assert!(
        monitor.poll(&context, now),
        "terminal follow-up releases the child"
    );
    monitor.awaiting_reopen = Some(record.follow_ups);
    now += Duration::from_secs(1);
    assert!(
        !monitor.poll(&context, now),
        "resumed wrapper must wait for a new turn even before dispatch queues a message"
    );
    for signal in [LifecycleSignal::TurnStarted { turn_id: None }, ended()] {
        observe("child", signal.clone());
        rimz::harness::run::record_lifecycle(
            store.paths(),
            &record.run_id,
            kind.as_str(),
            &rimz::agents::AgentLifecycleObservation::new(Some("child".into()), signal),
            None,
            || None,
        )
        .unwrap();
    }
    record = context.load_record().unwrap();
    assert_eq!(record.follow_ups, 1);
    assert!(record.status.is_terminal());
    record.joined_at = Some(jiff::Timestamp::now());
    rimz::harness::run::create(store.paths(), &record).unwrap();
    now += Duration::from_secs(1);
    assert!(
        monitor.poll(&context, now),
        "newly completed and received answer releases the resumed child, even between polls"
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
    let context = RunExecContext {
        run_id: record.run_id.clone(),
        store: rimz::Store::open(paths, runtime).unwrap(),
        session_name: "room".to_owned(),
        workspace: test_workspace(state.path()),
    };
    assert!(
        context.ready_for_self_cleanup(&context.load_record().unwrap()),
        "background run has no waiter"
    );
    let waiter = rimz::harness::run_wake::RunWaiter::bind(
        context.store.runtime_paths(),
        rimz::harness::run_wake::ExpectedRunFrame {
            workspace_id,
            run_id: record.run_id.clone(),
        },
        rimz::harness::run::RunCancellation::new(),
    )
    .unwrap();
    assert!(
        !context.ready_for_self_cleanup(&context.load_record().unwrap()),
        "live waiter owns verification and evidence capture"
    );
    record.status = rimz::store::run::RunStatus::Running;
    rimz::harness::run::create(context.store.paths(), &record).unwrap();
    drop(waiter);
    assert!(
        !context.ready_for_self_cleanup(&context.load_record().unwrap()),
        "rearmed run remains active even without a waiter"
    );
    record.status = rimz::store::run::RunStatus::Completed;
    rimz::harness::run::create(context.store.paths(), &record).unwrap();
    assert!(
        context.ready_for_self_cleanup(&context.load_record().unwrap()),
        "terminal run is reclaimed once waiter leaves"
    );
}
