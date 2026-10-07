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
        .bind_agent_launch(&identity, "room", dir.path(), &pane)
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
    record.status = rimz::store::run::RunStatus::Running;
    rimz::harness::run::create(store.paths(), &record).unwrap();
    assert!(
        !run_receipt_once(&context, true).unwrap(),
        "launch waits for completion"
    );
    record.status = rimz::store::run::RunStatus::Completed;
    rimz::harness::run::create(store.paths(), &record).unwrap();
    assert!(
        !run_receipt_once(&context, true).unwrap(),
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
    assert!(!run_receipt_once(&context, true).unwrap());
    observe("parent", ended());
    assert!(run_receipt_once(&context, true).unwrap());
    record.report_message_id = None;
    record.joined_at = Some(jiff::Timestamp::now());
    rimz::harness::run::create(store.paths(), &record).unwrap();
    observe("child", LifecycleSignal::TurnStarted { turn_id: None });
    assert!(!run_receipt_once(&context, true).unwrap());
    observe("child", ended());
    observe("parent", LifecycleSignal::Ended);
    assert!(run_receipt_once(&context, true).unwrap());
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
    assert!(
        !run_receipt_once(&context, true).unwrap(),
        "queued follow-up holds the child"
    );
    store.record_sent_batch(&[message], "room").unwrap();
    assert!(
        !run_receipt_once(&context, true).unwrap(),
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
    assert!(
        run_receipt_once(&context, true).unwrap(),
        "terminal follow-up releases the child"
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
                .any(|member| member.agent_id.as_str() == "NEW" && !member.ended)
        );
    }
}
