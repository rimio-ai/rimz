use super::super::tests::test_workspace;
use super::*;
use rimz::agents::PermissionMode;

#[test]
fn card_evidence_stops_after_the_provider_registers() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = test_workspace(dir.path());
    let paths = rimz::StatePaths::under(workspace.workspace_id.clone(), dir.path()).unwrap();
    let rt = rimz::RuntimePaths::under(workspace.workspace_id.clone(), dir.path()).unwrap();
    let store = rimz::Store::open(paths, rt).unwrap();
    let identity = LaunchIdentity {
        launch_id: None,
        kind: AgentKind::new_unchecked("codex"),
        agent_id: "launch_child".into(),
        name: "child".into(),
        name_explicit: false,
        launch: Default::default(),
        run_id: None,
        prompt: None,
    };
    let pane = PaneId::parse("tmux:%1").unwrap();
    store
        .bind_agent_launch(&identity, "room", dir.path(), &pane, None)
        .unwrap();
    assert!(run_card_evidence(&store, &identity.kind, &identity.agent_id, &identity.name).unwrap());
    let mut observation = rimz::agents::AgentLifecycleObservation::new(
        Some("provider-session".into()),
        rimz::agents::LifecycleSignal::Registered,
    );
    observation.pane_id = Some(pane);
    observation.agent_name = Some(identity.name.clone());
    store
        .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
            session_name: "room",
            agent_kind: identity.kind.clone(),
            event_name: "test",
            observation: &observation,
            spawned_subagents: &[],
        })
        .unwrap();
    assert!(
        !run_card_evidence(&store, &identity.kind, &identity.agent_id, &identity.name).unwrap()
    );
}

#[test]
fn strand_duty_settles_stranded_park() {
    {
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
        let mut previous = None;
        assert!(!run_strand_once(&context, &mut previous).unwrap());
        assert_eq!(previous, record.parked_at);
        assert!(!run_strand_once(&context, &mut previous).unwrap());
        let failed = context.load_record().unwrap();
        assert_eq!(failed.status, rimz::store::run::RunStatus::Failed);
        assert!(failed.failure_tail.is_some());
        assert_eq!(failed.parked_at, None);
        assert_eq!(previous, None);
        assert!(run_strand_once(&context, &mut previous).unwrap());
    }
}

/// A park on a message that was withdrawn before it opened a turn completes
/// with the parked turn's answer, and the launcher's digest reports it so.
#[test]
fn strand_duty_completes_a_withdrawn_message_park() {
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
    record.subagent = true;
    record.agent_id = Some("session".into());
    record.agent_name = Some("calm-fox".to_owned());
    record.last_message = Some("the parked answer".to_owned());
    record.parked_at = Some(jiff::Timestamp::now());
    record.parked_on = Some(rimz::store::run::ParkCause::Message);
    rimz::harness::run::create(store.paths(), &record).unwrap();
    let context = RunExecContext {
        run_id: record.run_id.clone(),
        store,
        session_name: "room".to_owned(),
        workspace: test_workspace(state.path()),
    };
    let mut previous = None;
    assert!(!run_strand_once(&context, &mut previous).unwrap());
    assert!(!run_strand_once(&context, &mut previous).unwrap());
    let completed = context.load_record().unwrap();
    assert_eq!(completed.status, rimz::store::run::RunStatus::Completed);
    assert_eq!(completed.failure_tail, None);
    assert_eq!(completed.last_message.as_deref(), Some("the parked answer"));
    let mut child = rimz::testkit::agent_state("claude", "session", completed.started_at);
    child.name = None;
    let row = rimz::harness::run::report::compose_digest_row(&child, &completed, None);
    assert!(row.starts_with("- @calm-fox: completed in "), "{row}");
}

/// The one park nothing else can end is a settled fleet whose digest was
/// lost: the child's reporter swallowed its error and the parent is at
/// rest waiting for exactly that digest. The parked wrapper therefore
/// repairs the fleet before each strand check, and the run lives to
/// complete on the digest's turn instead of failing stranded.
#[test]
fn strand_duty_repairs_a_lost_fleet_digest_instead_of_failing() {
    assert_strand_duty_repairs_report(false);
}

#[test]
fn strand_duty_settles_a_dead_team_and_waits_for_its_report() {
    assert_strand_duty_repairs_report(true);
}

fn assert_strand_duty_repairs_report(team: bool) {
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
            instance: "forge#x".parse().unwrap(),
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
    let mut previous = None;
    assert!(!run_strand_once(&context, &mut previous).unwrap());

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
    assert_eq!(previous, None, "a repaired park is live, not stranded");
}

#[test]
fn idle_close_enforces_every_condition_and_ignores_parent_turn_state() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = test_workspace(dir.path());
    let paths = rimz::StatePaths::under(workspace.workspace_id.clone(), dir.path()).unwrap();
    let runtime =
        rimz::RuntimePaths::under(workspace.workspace_id.clone(), &dir.path().join("rt")).unwrap();
    let store = rimz::Store::open(paths, runtime).unwrap();
    let kind = AgentKind::new_unchecked("codex");
    let child_pane = PaneId::parse("tmux:%2").unwrap();
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
    let bound_at = jiff::Timestamp::now();
    let linger = Some(Duration::from_secs(10 * 60));
    let after_linger = |base: jiff::Timestamp| {
        base.checked_add(jiff::SignedDuration::from_secs(10 * 60 + 5))
            .unwrap()
    };
    let before_linger = |base: jiff::Timestamp| {
        base.checked_add(jiff::SignedDuration::from_secs(9 * 60))
            .unwrap()
    };

    let mut record = rimz::store::run::RunRecord::new(
        workspace.workspace_id.clone(),
        kind.clone(),
        PermissionMode::Auto,
        "work".into(),
        dir.path().into(),
    );
    record.agent_id = Some("child".into());
    record.agent_name = Some("child".into());
    record.pane_id = Some(child_pane.clone());
    record.subagent = true;
    record.status = rimz::store::run::RunStatus::Running;
    rimz::harness::run::create(store.paths(), &record).unwrap();
    let context = RunExecContext {
        run_id: record.run_id.clone(),
        store: store.clone(),
        session_name: "room".into(),
        workspace,
    };

    // Condition 1: non-terminal run holds close open even past linger.
    assert!(
        !run_receipt_once_with(&context, true, bound_at, linger, || after_linger(bound_at))
            .unwrap(),
        "non-terminal run must hold close open"
    );

    // Condition 2: terminal run that owes a report with report=false holds close open.
    record.status = rimz::store::run::RunStatus::Completed;
    rimz::harness::run::create(store.paths(), &record).unwrap();
    assert!(
        !run_receipt_once_with(&context, false, bound_at, linger, || after_linger(bound_at))
            .unwrap(),
        "unreported run must hold close open"
    );

    // Condition 2 (R2): report stamped on run but neither queued nor in history holds close open.
    record.report_message_id = Some(rimz::ids::MessageId::new());
    rimz::harness::run::create(store.paths(), &record).unwrap();
    assert!(
        !run_receipt_once_with(&context, false, bound_at, linger, || after_linger(bound_at))
            .unwrap(),
        "stamped report neither queued nor in history must hold close open (R2)"
    );

    // Clear bogus stamp and let run_receipt_once queue the real fleet report.
    record.report_message_id = None;
    rimz::harness::run::create(store.paths(), &record).unwrap();

    // Case (a): parent ended its receiving turn, child rested shorter than linger -> stays open.
    observe("parent", LifecycleSignal::TurnStarted { turn_id: None });
    observe("parent", ended());
    assert!(
        !run_receipt_once_with(&context, true, bound_at, linger, || before_linger(bound_at))
            .unwrap(),
        "case (a): parent rested but child rested shorter than linger must stay open"
    );
    let reported = context.load_record().unwrap();
    assert!(
        reported.report_message_id.is_some(),
        "duty must still queue the owed report before linger expires"
    );
    record = reported;

    // Case (b) & (c): parent mid-turn AND report only queued (undelivered), child rested >= linger -> closes!
    observe("parent", LifecycleSignal::TurnStarted { turn_id: None });
    assert!(
        run_receipt_once_with(&context, false, bound_at, linger, || after_linger(bound_at))
            .unwrap(),
        "cases (b)+(c): queued report and mid-turn parent must close once child rested >= linger"
    );

    // Case (d): parent ended, child rested shorter than linger -> stays open under this rule.
    observe("parent", LifecycleSignal::Ended);
    assert!(
        !run_receipt_once_with(&context, false, bound_at, linger, || before_linger(
            bound_at
        ))
        .unwrap(),
        "case (d): ended parent does not short-circuit linger in receipt duty"
    );
    observe("parent", LifecycleSignal::Registered);

    // Condition 6 (R4): rest counted from wrapper's bound_at even when snapshot timestamps are older.
    let later_bind = after_linger(bound_at);
    assert!(
        !run_receipt_once_with(&context, false, later_bind, linger, || before_linger(
            later_bind
        ))
        .unwrap(),
        "rest shorter than linger from wrapper's bind must hold close open (R4)"
    );
    assert!(
        run_receipt_once_with(&context, false, later_bind, linger, || after_linger(
            later_bind
        ))
        .unwrap(),
        "rest past linger from wrapper's bind closes"
    );

    // Condition 5 (R3): fresh focused pane holds close open; stale, wrong-session, unreadable, missing hold nothing.
    let check_now = after_linger(bound_at);
    let check_now_ms = u64::try_from(check_now.as_millisecond()).unwrap();
    let frame_path = store.runtime_paths().pane_frame_path();
    let write_frame = |session: &str, produced_at_ms: u64, focused: Option<PaneId>| {
        let frame: rimz::sidebar::frame::PaneFrame = serde_json::from_value(serde_json::json!({
            "produced_at_ms": produced_at_ms,
            "observed_at_ms": produced_at_ms,
            "session_name": session,
            "tabs": [],
            "focused_pane": focused,
        }))
        .unwrap();
        rimz::disk::atomic::write_temp_then_rename_cache(&frame_path, &frame).unwrap();
    };
    write_frame("room", check_now_ms - 1_000, Some(child_pane.clone()));
    assert!(
        !run_receipt_once_with(&context, false, bound_at, linger, || check_now).unwrap(),
        "fresh focus on child pane must hold close open (R3)"
    );
    // The host reuses a pane frame up to 10 s and republishes a tick later (R3).
    write_frame("room", check_now_ms - 12_000, Some(child_pane.clone()));
    assert!(
        !run_receipt_once_with(&context, false, bound_at, linger, || check_now).unwrap(),
        "a frame the host is about to republish must hold close open (R3)"
    );
    write_frame("room", check_now_ms - 60_000, Some(child_pane.clone()));
    assert!(
        run_receipt_once_with(&context, false, bound_at, linger, || check_now).unwrap(),
        "stale focus frame must not hold close open (R3)"
    );
    write_frame("other-room", check_now_ms - 1_000, Some(child_pane.clone()));
    assert!(
        run_receipt_once_with(&context, false, bound_at, linger, || check_now).unwrap(),
        "wrong-session focus frame must not hold close open"
    );
    std::fs::write(&frame_path, b"not json").unwrap();
    assert!(
        run_receipt_once_with(&context, false, bound_at, linger, || check_now).unwrap(),
        "unreadable focus frame must hold nothing"
    );
    let _ = std::fs::remove_file(&frame_path);

    // Condition 3: open turn on child holds close open even past linger.
    record.report_message_id = None;
    record.joined_at = Some(bound_at);
    rimz::harness::run::create(store.paths(), &record).unwrap();
    observe("child", LifecycleSignal::TurnStarted { turn_id: None });
    assert!(
        !run_receipt_once_with(&context, false, bound_at, linger, || after_linger(bound_at))
            .unwrap(),
        "open turn on child must hold close open"
    );
    observe("child", ended());
    let settled_at = jiff::Timestamp::now();
    assert!(
        run_receipt_once_with(&context, false, bound_at, linger, || after_linger(
            settled_at
        ))
        .unwrap(),
        "joined child with no open turn closes once rested past linger"
    );
    // An open raised ask keeps the child's pane for the user's answer.
    let resting = store
        .snapshot_cached()
        .unwrap()
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "child")
        .cloned()
        .unwrap();
    let ask = rimz::store::raised::prepare_raise(
        store.paths(),
        &resting,
        false,
        Some("Ship now?".into()),
        None,
        jiff::Timestamp::now(),
    )
    .unwrap();
    rimz::store::raised::raise(store.paths(), ask).unwrap();
    assert!(
        !run_receipt_once_with(&context, false, bound_at, linger, || after_linger(
            settled_at
        ))
        .unwrap(),
        "open raised ask holds the child"
    );
    rimz::store::raised::withdraw(store.paths(), &kind, &resting.agent_id, None).unwrap();
    assert!(
        run_receipt_once_with(&context, false, bound_at, linger, || after_linger(
            settled_at
        ))
        .unwrap(),
        "withdrawn raised ask releases the child"
    );

    // Condition 4: non-terminal message addressed to child holds close open even past linger.
    let child = store
        .snapshot_cached()
        .unwrap()
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "child")
        .cloned()
        .unwrap();
    let message = rimz::store::message::MessageRecord::new(
        record.workspace_id.clone(),
        &child,
        "follow up".into(),
        rimz::store::message::DeliveryGate::Done,
    );
    store.queue_message(&message, "room").unwrap();
    assert!(
        !run_receipt_once_with(&context, false, bound_at, linger, || after_linger(
            settled_at
        ))
        .unwrap(),
        "queued follow-up holds the child"
    );
    store.record_sent_batch(&[message], "room").unwrap();
    assert!(
        !run_receipt_once_with(&context, false, bound_at, linger, || after_linger(
            settled_at
        ))
        .unwrap(),
        "sent follow-up before delivery still holds the child"
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
    assert!(
        run_receipt_once_with(&context, false, bound_at, linger, || after_linger(
            settled_at
        ))
        .unwrap(),
        "terminal follow-up releases the child once rested past linger"
    );

    // Unreadable message queue returns Err so the duty leaves the pane open and retries later.
    let queue_path = store.paths().message_queue_file();
    std::fs::remove_file(&queue_path).unwrap();
    std::fs::create_dir(&queue_path).unwrap();
    assert!(
        run_receipt_once_with(&context, false, bound_at, linger, || after_linger(
            settled_at
        ))
        .is_err(),
        "unreadable message queue must fail duty rather than close the pane"
    );
    std::fs::remove_dir(&queue_path).unwrap();
}

#[test]
fn idle_close_samples_clock_only_after_store_reads() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = test_workspace(dir.path());
    let paths = rimz::StatePaths::under(workspace.workspace_id.clone(), dir.path()).unwrap();
    let runtime =
        rimz::RuntimePaths::under(workspace.workspace_id.clone(), &dir.path().join("rt")).unwrap();
    let store = rimz::Store::open(paths, runtime).unwrap();
    let kind = AgentKind::new_unchecked("codex");
    let mut observation = rimz::agents::AgentLifecycleObservation::new(
        Some("child".into()),
        rimz::agents::LifecycleSignal::Registered,
    );
    observation.agent_name = Some("child".into());
    store
        .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
            session_name: "room",
            agent_kind: kind.clone(),
            event_name: "test",
            observation: &observation,
            spawned_subagents: &[],
        })
        .unwrap();
    let mut record = rimz::store::run::RunRecord::new(
        workspace.workspace_id.clone(),
        kind,
        PermissionMode::Auto,
        "work".into(),
        dir.path().into(),
    );
    record.agent_id = Some("child".into());
    record.agent_name = Some("child".into());
    record.subagent = true;
    record.status = rimz::store::run::RunStatus::Completed;
    record.joined_at = Some(jiff::Timestamp::now());
    rimz::harness::run::create(store.paths(), &record).unwrap();
    let context = RunExecContext {
        run_id: record.run_id.clone(),
        store,
        session_name: "room".into(),
        workspace,
    };
    let bound_at = jiff::Timestamp::now();
    let clock_calls = std::cell::Cell::new(0usize);
    let _ = run_receipt_once_with(
        &context,
        false,
        bound_at,
        Some(Duration::from_secs(60)),
        || {
            clock_calls.set(clock_calls.get() + 1);
            bound_at
                .checked_add(jiff::SignedDuration::from_secs(120))
                .unwrap()
        },
    )
    .unwrap();
    assert_eq!(
        clock_calls.get(),
        1,
        "idle close must take exactly one instant after the reads it judges"
    );
}

#[test]
fn skip_linger_closes_immediately_while_keeping_every_other_condition() {
    use rimz::agents::LifecycleSignal;

    let dir = tempfile::tempdir().unwrap();
    let workspace = test_workspace(dir.path());
    let paths = rimz::StatePaths::under(workspace.workspace_id.clone(), dir.path()).unwrap();
    let runtime =
        rimz::RuntimePaths::under(workspace.workspace_id.clone(), &dir.path().join("rt")).unwrap();
    let store = rimz::Store::open(paths, runtime).unwrap();
    let kind = AgentKind::new_unchecked("codex");
    let child_pane = PaneId::parse("tmux:%2").unwrap();
    let observe = |signal| {
        let mut observation =
            rimz::agents::AgentLifecycleObservation::new(Some("child".into()), signal);
        observation.agent_name = Some("child".into());
        observation.pane_id = Some(child_pane.clone());
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
    observe(LifecycleSignal::Registered);
    let bound_at = jiff::Timestamp::now();

    let mut record = rimz::store::run::RunRecord::new(
        workspace.workspace_id.clone(),
        kind.clone(),
        PermissionMode::Auto,
        "work".into(),
        dir.path().into(),
    );
    record.agent_id = Some("child".into());
    record.agent_name = Some("child".into());
    record.pane_id = Some(child_pane.clone());
    record.subagent = true;
    record.status = rimz::store::run::RunStatus::Completed;
    rimz::harness::run::create(store.paths(), &record).unwrap();
    let context = RunExecContext {
        run_id: record.run_id.clone(),
        store: store.clone(),
        session_name: "room".into(),
        workspace,
    };

    // Unreported run still holds even when linger is skipped.
    assert!(
        !run_receipt_once_with(&context, false, bound_at, None, || bound_at).unwrap(),
        "unreported run must still hold when linger is skipped"
    );
    record.report_message_id = Some(rimz::ids::MessageId::new());
    rimz::harness::run::create(store.paths(), &record).unwrap();
    assert!(
        !run_receipt_once_with(&context, false, bound_at, None, || bound_at).unwrap(),
        "undurable report must still hold when linger is skipped"
    );

    // Joined run closes immediately with zero elapsed time (even now == bound_at).
    record.report_message_id = None;
    record.joined_at = Some(bound_at);
    rimz::harness::run::create(store.paths(), &record).unwrap();
    assert!(
        run_receipt_once_with(&context, false, bound_at, None, || bound_at).unwrap(),
        "skip_linger must close immediately once non-linger conditions hold"
    );
    assert!(
        !run_receipt_once_with(
            &context,
            false,
            bound_at,
            Some(Duration::from_secs(60)),
            || bound_at,
        )
        .unwrap(),
        "ordinary linger must still hold at bound_at"
    );

    // Fresh focus still holds when linger is skipped.
    let now_ms = u64::try_from(bound_at.as_millisecond()).unwrap();
    let frame_path = store.runtime_paths().pane_frame_path();
    let frame: rimz::sidebar::frame::PaneFrame = serde_json::from_value(serde_json::json!({
        "produced_at_ms": now_ms.saturating_sub(1_000),
        "observed_at_ms": now_ms.saturating_sub(1_000),
        "session_name": "room",
        "tabs": [],
        "focused_pane": Some(child_pane.clone()),
    }))
    .unwrap();
    rimz::disk::atomic::write_temp_then_rename_cache(&frame_path, &frame).unwrap();
    assert!(
        !run_receipt_once_with(&context, false, bound_at, None, || bound_at).unwrap(),
        "focused pane must still hold when linger is skipped"
    );
    let _ = std::fs::remove_file(&frame_path);

    // Open turn still holds when linger is skipped.
    observe(LifecycleSignal::TurnStarted { turn_id: None });
    assert!(
        !run_receipt_once_with(&context, false, bound_at, None, || bound_at).unwrap(),
        "open turn must still hold when linger is skipped"
    );
    observe(LifecycleSignal::TurnEnded {
        errored: false,
        parked_on_background: false,
        turn_id: None,
    });
    assert!(
        run_receipt_once_with(&context, false, bound_at, None, || bound_at).unwrap(),
        "closing the open turn allows immediate close when linger is skipped"
    );

    // Non-terminal queued message still holds when linger is skipped.
    let child = store
        .snapshot_cached()
        .unwrap()
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "child")
        .cloned()
        .unwrap();
    let message = rimz::store::message::MessageRecord::new(
        record.workspace_id.clone(),
        &child,
        "follow up".into(),
        rimz::store::message::DeliveryGate::Done,
    );
    store.queue_message(&message, "room").unwrap();
    assert!(
        !run_receipt_once_with(&context, false, bound_at, None, || bound_at).unwrap(),
        "non-terminal queued message must still hold when linger is skipped"
    );
    store
        .cancel_message(&message.message_id, "room", "canceled")
        .unwrap();
    assert!(
        run_receipt_once_with(&context, false, bound_at, None, || bound_at).unwrap(),
        "settling the queued message allows immediate close when linger is skipped"
    );
}

#[test]
fn parent_probe_confirms_ended_launch_but_preserves_live_successor_and_legacy_alias() {
    use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
    use rimz::store::event::{AgentLaunchPayload, EventEnvelope};
    for legacy in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let workspace = test_workspace(dir.path());
        let paths = rimz::StatePaths::under(workspace.workspace_id.clone(), dir.path()).unwrap();
        let rt = rimz::RuntimePaths::under(workspace.workspace_id.clone(), dir.path()).unwrap();
        let store = rimz::Store::open(paths, rt).unwrap();
        let kind = AgentKind::new_unchecked("codex");
        let launch = |id: &str, launch_id: &str, parent: Option<&str>| {
            let payload: AgentLaunchPayload = serde_json::from_value(serde_json::json!({
                "agent_id": id, "agent_name": id, "launch_id": launch_id, "state": "bound",
                "parent_agent_id": parent, "parent_agent_kind": parent.map(|_| "codex"),
                "launch_depth": parent.map(|_| 1),
            }))
            .unwrap();
            store
                .append_event(&EventEnvelope::agent_launched(
                    workspace.workspace_id.clone(),
                    "room",
                    &kind,
                    payload,
                ))
                .unwrap();
        };
        launch("OLD", "L", None);
        launch(
            "child",
            "child-launch",
            Some(if legacy { "OLD" } else { "L" }),
        );
        store
            .append_event(&EventEnvelope::agent_lifecycle(
                workspace.workspace_id.clone(),
                "room",
                "codex",
                "test",
                &AgentLifecycleObservation::new(Some("OLD".into()), LifecycleSignal::Ended),
            ))
            .unwrap();
        let probe = || {
            run_parent_probe(
                &store,
                kind.clone(),
                "child-launch".into(),
                None,
                "room".into(),
                false,
            )
            .unwrap()
        };
        assert!(probe().0, "every known launch member has ended");
        launch("NEW", "L", None);
        let (ended, answer) = probe();
        assert!(
            !ended,
            "a successor on the parent launch protects the child"
        );
        assert!(
            answer
                .members
                .iter()
                .any(|(agent_id, ended)| agent_id.as_str() == "NEW" && !ended)
        );
        store
            .append_event(&EventEnvelope::agent_attached(
                workspace.workspace_id.clone(),
                "room",
                &kind,
                rimz::store::event::AgentAttachPayload {
                    epoch: None,
                    agent_id: "NEW".into(),
                    launch_id: Some("L".into()),
                    pane_id: PaneId::parse("tmux:%77").unwrap(),
                    record: None,
                    tier: None,
                    mode: None,
                    isolation: None,
                    effective_isolation: None,
                    login: None,
                    pane_pid: None,
                    runtime_owner: rimz::store::runtime::current_process_owner(
                        rimz::pane::RuntimeOwnerKind::Agent,
                        "NEW",
                    ),
                },
            ))
            .unwrap();
        let answer = run_parent_probe(
            &store,
            kind.clone(),
            "child-launch".into(),
            None,
            "room".into(),
            false,
        );
        assert!(
            answer.is_ok_and(|(ended, _)| !ended),
            "a follower nomination must preserve a live successor without requiring a pane read"
        );
    }
}

#[test]
fn parent_probe_rereads_the_parent_after_its_pane_goes_missing() {
    use rimz::store::event::{AgentAttachPayload, AgentLaunchPayload, EventEnvelope};
    for swapped in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let workspace = test_workspace(dir.path());
        let paths = rimz::StatePaths::under(workspace.workspace_id.clone(), dir.path()).unwrap();
        let rt = rimz::RuntimePaths::under(workspace.workspace_id.clone(), dir.path()).unwrap();
        let store = rimz::Store::open(paths, rt).unwrap();
        let launch = |kind: &str, id: &str, launch_id: &str, epoch: u32, parent: Option<&str>| {
            let payload: AgentLaunchPayload = serde_json::from_value(serde_json::json!({
                "agent_id": id, "agent_name": "lead", "launch_id": launch_id, "state": "bound",
                "epoch": epoch, "parent_agent_id": parent,
                "parent_agent_kind": parent.map(|_| "codex"), "launch_depth": parent.map(|_| 1),
            }))
            .unwrap();
            store
                .append_event(&EventEnvelope::agent_launched(
                    workspace.workspace_id.clone(),
                    "room",
                    &AgentKind::new_unchecked(kind),
                    payload,
                ))
                .unwrap();
        };
        let attach = |kind: &str, id: &str, epoch: u32, pane: &str| {
            store
                .append_event(&EventEnvelope::agent_attached(
                    workspace.workspace_id.clone(),
                    "room",
                    &AgentKind::new_unchecked(kind),
                    AgentAttachPayload {
                        epoch: Some(epoch),
                        agent_id: id.into(),
                        launch_id: Some("L".into()),
                        pane_id: PaneId::parse(pane).unwrap(),
                        record: None,
                        tier: None,
                        mode: None,
                        isolation: None,
                        effective_isolation: None,
                        login: None,
                        pane_pid: None,
                        runtime_owner: rimz::store::runtime::current_process_owner(
                            rimz::pane::RuntimeOwnerKind::Agent,
                            id,
                        ),
                    },
                ))
                .unwrap();
        };
        launch("codex", "OLD", "L", 0, None);
        attach("codex", "OLD", 0, "tmux:%77");
        launch("codex", "child", "child-launch", 0, Some("L"));
        let child = ParentProbeChild {
            kind: AgentKind::new_unchecked("codex"),
            launch_id: "child-launch".into(),
            pane: None,
            session_name: "room".into(),
        };
        let (ended, answer) = parent_probe(&store, &child, true, |seed, pane| {
            assert_eq!(seed.parent_pane.as_ref(), Some(pane));
            if swapped {
                // A cross-kind molt lands between the first read and the listing.
                launch("claude", "NEW", "L", 1, None);
                attach("claude", "NEW", 1, "tmux:%78");
            }
            Ok(false)
        })
        .unwrap();
        assert_eq!(ended, !swapped, "swapped: {swapped}");
        if swapped {
            assert_eq!(answer.parent_pane, Some(PaneId::parse("tmux:%78").unwrap()));
        }
    }
}

#[test]
fn a_replacement_that_fails_after_bound_ends_the_parent_in_either_order() {
    use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
    use rimz::store::event::{AgentLaunchPayload, EventEnvelope};
    for old_ends_first in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let workspace = test_workspace(dir.path());
        let paths = rimz::StatePaths::under(workspace.workspace_id.clone(), dir.path()).unwrap();
        let rt = rimz::RuntimePaths::under(workspace.workspace_id.clone(), dir.path()).unwrap();
        let store = rimz::Store::open(paths, rt).unwrap();
        let kind = AgentKind::new_unchecked("codex");
        let launch = |id: &str, launch_id: &str, state: &str, epoch: u32, parent: Option<&str>| {
            let payload: AgentLaunchPayload = serde_json::from_value(serde_json::json!({
                "agent_id": id, "agent_name": "lead", "launch_id": launch_id, "state": state,
                "epoch": epoch, "parent_agent_id": parent,
                "parent_agent_kind": parent.map(|_| "codex"), "launch_depth": parent.map(|_| 1),
            }))
            .unwrap();
            store
                .append_event(&EventEnvelope::agent_launched(
                    workspace.workspace_id.clone(),
                    "room",
                    &kind,
                    payload,
                ))
                .unwrap();
        };
        let old_ends = || {
            store
                .append_event(&EventEnvelope::agent_lifecycle(
                    workspace.workspace_id.clone(),
                    "room",
                    "codex",
                    "test",
                    &AgentLifecycleObservation::new(Some("OLD".into()), LifecycleSignal::Ended),
                ))
                .unwrap();
        };
        launch("OLD", "L", "bound", 0, None);
        launch("child", "child-launch", "bound", 0, Some("L"));
        launch("launch_new", "L", "bound", 1, None);
        let probe = || {
            run_parent_probe(
                &store,
                kind.clone(),
                "child-launch".into(),
                None,
                "room".into(),
                false,
            )
            .unwrap()
            .0
        };
        if old_ends_first {
            old_ends();
            assert!(!probe(), "the bound replacement holds the agent");
            launch("launch_new", "L", "failed", 1, None);
        } else {
            launch("launch_new", "L", "failed", 1, None);
            assert!(
                probe(),
                "a failed holder ends the agent before the old row's end"
            );
            old_ends();
        }
        assert!(probe(), "old_ends_first: {old_ends_first}");
    }
}

fn rested_child() -> (
    tempfile::TempDir,
    RunExecContext,
    rimz::store::run::RunRecord,
    jiff::Timestamp,
) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = test_workspace(dir.path());
    let paths = rimz::StatePaths::under(workspace.workspace_id.clone(), dir.path()).unwrap();
    let runtime =
        rimz::RuntimePaths::under(workspace.workspace_id.clone(), &dir.path().join("rt")).unwrap();
    let store = rimz::Store::open(paths, runtime).unwrap();
    let kind = AgentKind::new_unchecked("codex");
    let mut observation = rimz::agents::AgentLifecycleObservation::new(
        Some("child".into()),
        rimz::agents::LifecycleSignal::Registered,
    );
    observation.agent_name = Some("child".into());
    store
        .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
            session_name: "room",
            agent_kind: kind.clone(),
            event_name: "test",
            observation: &observation,
            spawned_subagents: &[],
        })
        .unwrap();
    let bound_at = jiff::Timestamp::now();
    let mut record = rimz::store::run::RunRecord::new(
        workspace.workspace_id.clone(),
        kind,
        PermissionMode::Auto,
        "work".into(),
        dir.path().into(),
    );
    record.agent_id = Some("child".into());
    record.agent_name = Some("child".into());
    record.subagent = true;
    record.status = rimz::store::run::RunStatus::Completed;
    let context = RunExecContext {
        run_id: record.run_id.clone(),
        store,
        session_name: "room".into(),
        workspace,
    };
    (dir, context, record, bound_at)
}

fn idle_close_answers(
    context: &RunExecContext,
    record: &rimz::store::run::RunRecord,
    bound_at: jiff::Timestamp,
) -> bool {
    rimz::harness::run::create(context.store.paths(), record).unwrap();
    run_receipt_once_with(
        context,
        false,
        bound_at,
        Some(Duration::from_secs(60)),
        || bound_at + Duration::from_secs(120),
    )
    .unwrap()
}

#[test]
fn a_run_settled_before_the_bind_closes_without_its_report_row() {
    let (_dir, context, mut record, bound_at) = rested_child();
    let before_bind = bound_at - Duration::from_secs(1);
    let after_bind = bound_at + Duration::from_secs(1);
    let closes = |record: &_| idle_close_answers(&context, record, bound_at);

    record.report_message_id = Some(rimz::ids::MessageId::new());
    record.completed_at = Some(after_bind);
    assert!(
        !closes(&record),
        "a run settled after the bind holds while its report is in neither place (R2)"
    );
    record.completed_at = Some(bound_at);
    assert!(!closes(&record), "a run settled at the bind holds too");
    record.completed_at = None;
    assert!(!closes(&record), "a run with no settle instant holds");
    record.completed_at = Some(before_bind);
    assert!(
        closes(&record),
        "a run settled before the bind has no stamp-before-queue window left"
    );
    record.report_message_id = None;
    record.report_to = rimz::store::run::ReportTo::Nobody;
    assert!(closes(&record), "with or without a report id");
    record.completed_at = Some(after_bind);
    assert!(
        !closes(&record),
        "one settled after the bind with no report id holds"
    );
}

#[test]
fn idle_close_finds_a_report_behind_the_newest_history_rows() {
    let (_dir, context, mut record, bound_at) = rested_child();
    let store = &context.store;
    let report = rimz::ids::MessageId::new();
    record.completed_at = Some(bound_at + Duration::from_secs(1));
    record.report_message_id = Some(report.clone());
    assert!(!idle_close_answers(&context, &record, bound_at));
    let mut row = rimz::store::message::MessageRecord::new(
        record.workspace_id.clone(),
        &store.snapshot_cached().unwrap().agents[0],
        "report".into(),
        rimz::store::message::DeliveryGate::Done,
    );
    row.status = rimz::store::message::MessageStatus::Delivered;
    row.message_id = report;
    let history = &store.paths().message_history_dir;
    std::fs::create_dir_all(history).unwrap();
    let line = |row: &rimz::store::message::MessageRecord| {
        format!("{}\n", serde_json::to_string(row).unwrap())
    };
    std::fs::write(history.join("1970-01-01.jsonl"), line(&row)).unwrap();
    let newer: String = (0..501)
        .map(|_| {
            row.message_id = rimz::ids::MessageId::new();
            line(&row)
        })
        .collect();
    std::fs::write(history.join("1970-01-08.jsonl"), newer).unwrap();
    assert!(
        idle_close_answers(&context, &record, bound_at),
        "a report behind more than 500 newer history rows is still in history"
    );
}
