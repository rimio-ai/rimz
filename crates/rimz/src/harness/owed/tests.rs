use super::*;
use crate::agents::{AgentLifecycleObservation, LifecycleSignal, PermissionMode};
use crate::store::message::{
    DeliveryGate, HarnessNotice, MessageRecord, MessageSender, MessageStatus,
};
use crate::store::run::{ReportTo, RunRecord, RunStatus};
use crate::store::writer::AgentLifecycleIntent;
use crate::{RuntimePaths, StatePaths, WorkspaceId};

fn fixture() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let id = WorkspaceId::from_project_root(dir.path());
    let state = StatePaths::under(id.clone(), &dir.path().join("state")).unwrap();
    let runtime = RuntimePaths::under(id, &dir.path().join("runtime")).unwrap();
    let store = Store::open(state, runtime).unwrap();
    (dir, store)
}

fn register(store: &Store, name: &str, parent: Option<&str>) {
    let mut observation =
        AgentLifecycleObservation::new(Some(name.into()), LifecycleSignal::Registered);
    observation.agent_name = Some(name.to_owned());
    observation.pane_id = Some(
        crate::ids::PaneId::parse(match name {
            "parent" => "tmux:%1",
            "child" => "tmux:%2",
            "other" => "tmux:%3",
            _ => unreachable!("fixture names have distinct panes"),
        })
        .unwrap(),
    );
    if let Some(parent) = parent {
        observation.launch.parent_agent_id = Some(parent.into());
        observation.launch.parent_agent_kind = Some(AgentKind::new_unchecked("codex"));
        observation.launch.launch_depth = Some(1);
    }
    store
        .append_agent_lifecycle(AgentLifecycleIntent {
            session_name: "owed-test",
            agent_kind: AgentKind::new_unchecked("codex"),
            event_name: "test",
            observation: &observation,
            spawned_subagents: &[],
        })
        .unwrap();
}

#[test]
fn compaction_continue_keeps_only_its_live_card_owed_until_it_settles_or_clears() {
    use crate::store::message::SoftCompactStep;

    let turn_end = |errored| LifecycleSignal::TurnEnded {
        errored,
        parked_on_background: false,
        turn_id: None,
    };
    for (queued, terminal, after, expected) in [
        (false, false, None, None),
        (true, false, None, Some("soft compaction")),
        (true, true, None, None),
        (true, false, Some(turn_end(false)), Some("soft compaction")),
        (true, false, Some(turn_end(true)), Some("soft compaction")),
        (
            true,
            false,
            Some(LifecycleSignal::TurnInterrupted { turn_id: None }),
            None,
        ),
        (true, false, Some(LifecycleSignal::Ended), None),
    ] {
        let (_dir, store) = fixture();
        register(&store, "parent", None);
        register(&store, "other", None);
        let kind = AgentKind::new_unchecked("codex");
        let lifecycle = |signal| {
            let observation = AgentLifecycleObservation::new(Some("parent".into()), signal);
            store
                .append_agent_lifecycle(AgentLifecycleIntent {
                    session_name: "owed-test",
                    agent_kind: kind.clone(),
                    event_name: "test",
                    observation: &observation,
                    spawned_subagents: &[],
                })
                .unwrap();
        };
        lifecycle(LifecycleSignal::TurnStarted { turn_id: None });
        let agent = store
            .agent_with_context(&kind, &"parent".into(), crate::RuntimeScope::Runtime)
            .unwrap()
            .unwrap();
        let mut reminder = MessageRecord::new(
            store.paths().workspace_id.clone(),
            &agent,
            "compact yourself".into(),
            DeliveryGate::Any,
        );
        reminder.soft_compact = Some(SoftCompactStep::WrapUp);
        reminder.status = MessageStatus::Sent;
        store.queue_message(&reminder, "owed-test").unwrap();
        if queued {
            let mut continuation = MessageRecord::new(
                store.paths().workspace_id.clone(),
                &agent,
                "continue".into(),
                DeliveryGate::Done,
            );
            continuation.soft_compact = Some(SoftCompactStep::Continue);
            continuation.sender = crate::store::message::MessageSender::System;
            store.queue_message(&continuation, "owed-test").unwrap();
            if terminal {
                store
                    .cancel_message(&continuation.message_id, "owed-test", "test settled")
                    .unwrap();
            }
        }
        let label = format!("queued={queued}, terminal={terminal}, after={after:?}");
        let owed = store
            .audit_rows()
            .unwrap()
            .find(&kind, &agent.agent_id)
            .and_then(|agent| agent.soft_compact.clone())
            .is_some_and(|mark| mark.continue_owed.is_some());
        assert_eq!(owed, queued && !terminal, "{label}");
        if let Some(signal) = after {
            lifecycle(signal);
        }
        assert_eq!(
            owed_wake(&store, &kind, &agent.agent_id)
                .unwrap()
                .map(OwedWake::as_str),
            expected,
            "{label}"
        );
        assert_eq!(owed_wake(&store, &kind, &"other".into()).unwrap(), None);
    }
}

#[test]
fn owed_team_is_scoped_to_its_launcher_and_skips_done_boards() {
    for (own, status, done, detached, expected) in [
        (true, RunStatus::Running, false, false, Some(OwedWake::Team)),
        (true, RunStatus::Completed, false, false, None),
        (false, RunStatus::Running, false, false, None),
        (true, RunStatus::Running, true, false, None),
        (true, RunStatus::Running, false, true, None),
    ] {
        let (dir, store) = fixture();
        register(&store, "parent", None);
        let kind = AgentKind::new_unchecked("codex");
        let mut observation =
            AgentLifecycleObservation::new(Some("leader".into()), LifecycleSignal::Registered);
        observation.agent_name = Some("leader".into());
        observation.launch.team = Some("forge".into());
        observation.launch.launched_by = Some(Box::new(crate::agents::LaunchedBy {
            kind: kind.clone(),
            agent_id: if own { "parent" } else { "someone-else" }.into(),
        }));
        store
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: "owed-test",
                agent_kind: kind.clone(),
                event_name: "test",
                observation: &observation,
                spawned_subagents: &[],
            })
            .unwrap();
        let mut record = RunRecord::new(
            store.paths().workspace_id.clone(),
            kind.clone(),
            PermissionMode::Auto,
            "work".into(),
            dir.path().into(),
        );
        record.status = status;
        if detached {
            record.report_to = ReportTo::Nobody;
        }
        record.team = Some(crate::store::run::TeamRun {
            launch_id: "leader".into(),
            instance: "forge#external".parse().unwrap(),
        });
        super::super::run::create(store.paths(), &record).unwrap();
        if done {
            std::fs::create_dir_all(dir.path().join(".rimz/team")).unwrap();
            std::fs::write(dir.path().join(".rimz/team/blackboard.md"), "Stage: Done\n").unwrap();
        }
        assert_eq!(OwedWake::Team.as_str(), "team");
        assert_eq!(
            owed_wake(&store, &kind, &"parent".into()).unwrap(),
            expected
        );
    }
}

#[test]
fn a_run_the_agent_started_holds_it_until_its_report_is_sent() {
    for (launch, state, report, expected) in [
        ("parent", "active", "owed", Some(OwedWake::Workflow)),
        ("parent", "done", "owed", Some(OwedWake::Workflow)),
        ("parent", "done", "sent", None),
        ("parent", "active", "none", None),
        ("other", "active", "owed", None),
    ] {
        let (_dir, store) = fixture();
        register(&store, "parent", None);
        register(&store, "other", None);
        let id = crate::ids::WorkflowRunId::new();
        let paths = store.paths().workflow(&id);
        std::fs::create_dir_all(&paths.home).unwrap();
        let record = serde_json::json!({
            "version":crate::store::workflow::VERSION, "id":id, "workflow":"review",
            "spec_dir":"/definitions/review", "prompt":"Review",
            "scope":{"kind":"worktree","path":"/project"}, "project_root":"/project",
            "created_at":"2026-01-01T00:00:00Z", "state":state, "park":null, "node":"pr",
            "entered_at":"2026-01-01T00:00:00Z", "input":null, "key":null,
            "recheck_at":null, "wait":null, "visits":{}, "key_visits":{},
            "deadline":null, "deadline_fired":false, "next_seq":1,
            "attempt":null, "prev_attempt":null, "terminal_at":null,
            "starter":{"by":"agent", "kind":"codex", "launch":launch},
            "depth":1, "loop_report":report,
        });
        std::fs::write(&paths.record, serde_json::to_vec(&record).unwrap()).unwrap();
        assert_eq!(
            owed_wake(&store, &AgentKind::new_unchecked("codex"), &"parent".into()).unwrap(),
            expected,
            "{launch} {state} {report}"
        );
    }
    assert_eq!(OwedWake::Workflow.as_str(), "workflow run");
}

#[test]
fn owed_messages_hold_only_their_card_until_terminal() {
    use crate::store::message::MessageBody;

    let peer = MessageSender::Agent {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: Some("peer".into()),
        name: Some("peer".to_owned()),
        profile: None,
        role: None,
        channel: None,
    };
    let senders = [
        HarnessNotice::Wait,
        HarnessNotice::Signal,
        HarnessNotice::SubagentReport,
        HarnessNotice::TeamReport,
        HarnessNotice::WorkflowReport,
        HarnessNotice::Stage,
        HarnessNotice::Deadline,
        HarnessNotice::SubagentRestarted,
        HarnessNotice::SubagentStalled,
        HarnessNotice::MessageUndelivered,
        HarnessNotice::MessageQueued,
    ]
    .map(|notice| (MessageSender::Harness { notice }, MessageBody::Prompt))
    .into_iter()
    .chain([
        (MessageSender::Human, MessageBody::Prompt),
        (peer.clone(), MessageBody::Prompt),
        (MessageSender::Human, MessageBody::Command),
        (peer, MessageBody::Command),
        (MessageSender::System, MessageBody::Prompt),
    ]);
    for (sender, body) in senders {
        for status in [
            MessageStatus::Queued,
            MessageStatus::Claimed,
            MessageStatus::Sent,
            MessageStatus::Delivered,
            MessageStatus::TimedOut,
        ] {
            let (_dir, store) = fixture();
            register(&store, "parent", None);
            register(&store, "other", None);
            let agent = store
                .snapshot_cached()
                .unwrap()
                .agents
                .iter()
                .find(|agent| agent.agent_id.as_str() == "parent")
                .cloned()
                .unwrap();
            let mut message = MessageRecord::new(
                store.paths().workspace_id.clone(),
                &agent,
                "wake".to_owned(),
                DeliveryGate::Done,
            )
            .with_sender(sender.clone());
            message.body = body;
            message.status = status;
            store.queue_message(&message, "owed-test").unwrap();
            let expected = match &sender {
                _ if status.is_terminal() => None,
                MessageSender::Harness {
                    notice:
                        HarnessNotice::Wait
                        | HarnessNotice::Signal
                        | HarnessNotice::SubagentReport
                        | HarnessNotice::TeamReport
                        | HarnessNotice::WorkflowReport,
                } => Some(OwedWake::WakeInFlight),
                MessageSender::Human | MessageSender::Agent { .. }
                    if body == MessageBody::Prompt =>
                {
                    Some(OwedWake::Message)
                }
                _ => None,
            };
            assert_eq!(
                owed_wake(&store, &agent.kind, &agent.agent_id).unwrap(),
                expected,
                "{sender:?} {body:?} {status:?}"
            );
            for id in ["other", "missing"] {
                assert_eq!(owed_wake(&store, &agent.kind, &id.into()).unwrap(), None);
            }
            assert_eq!(
                owed_wake(&store, &AgentKind::new_unchecked("claude"), &agent.agent_id).unwrap(),
                None
            );
        }
    }
}

#[test]
fn overdue_sent_wake_is_not_owed_without_reconciliation() {
    let (_dir, store) = fixture();
    register(&store, "parent", None);
    let agent = store
        .snapshot_cached()
        .unwrap()
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "parent")
        .cloned()
        .unwrap();
    let now = jiff::Timestamp::now();
    let mut message = MessageRecord::new(
        store.paths().workspace_id.clone(),
        &agent,
        "wake".to_owned(),
        DeliveryGate::Done,
    )
    .with_sender(MessageSender::Harness {
        notice: HarnessNotice::Wait,
    });
    message.status = MessageStatus::Sent;
    let overdue = now - message.body.delivery_window() - std::time::Duration::from_secs(60);
    for (sent_at, retry_after, compacting, expected) in [
        (overdue, None, false, None),
        (now, None, false, Some(OwedWake::WakeInFlight)),
        (
            overdue,
            Some(now + message.body.delivery_window()),
            false,
            Some(OwedWake::WakeInFlight),
        ),
        (overdue, None, true, Some(OwedWake::WakeInFlight)),
    ] {
        if compacting {
            let observation = AgentLifecycleObservation::new(
                Some(agent.agent_id.clone()),
                LifecycleSignal::Compacting,
            );
            store
                .append_agent_lifecycle(AgentLifecycleIntent {
                    session_name: "owed-test",
                    agent_kind: agent.kind.clone(),
                    event_name: "PreCompact",
                    observation: &observation,
                    spawned_subagents: &[],
                })
                .unwrap();
        }
        message.updated_at = sent_at;
        message.last_sent_at = Some(sent_at);
        message.retry_after = retry_after;
        store.queue_message(&message, "owed-test").unwrap();
        assert_eq!(
            owed_wake(&store, &agent.kind, &agent.agent_id).unwrap(),
            expected
        );
    }
}

#[test]
fn overdue_sent_prompt_is_not_owed_without_reconciliation() {
    let (_dir, store) = fixture();
    register(&store, "parent", None);
    let agent = store
        .snapshot_cached()
        .unwrap()
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "parent")
        .cloned()
        .unwrap();
    let now = jiff::Timestamp::now();
    let mut message = MessageRecord::new(
        store.paths().workspace_id.clone(),
        &agent,
        "rebase first".to_owned(),
        DeliveryGate::Done,
    );
    message.status = MessageStatus::Sent;
    let overdue = now - message.body.delivery_window() - std::time::Duration::from_secs(60);
    for (sent_at, expected) in [(overdue, None), (now, Some(OwedWake::Message))] {
        message.updated_at = sent_at;
        message.last_sent_at = Some(sent_at);
        store.queue_message(&message, "owed-test").unwrap();
        assert_eq!(
            owed_wake(&store, &agent.kind, &agent.agent_id).unwrap(),
            expected
        );
    }
}

#[test]
fn owed_fleet_includes_ended_unreported_children() {
    for ((status, joined, reported, report_to, expected), held) in [
        (
            RunStatus::Running,
            false,
            false,
            ReportTo::Launcher,
            Some(OwedWake::Subagents),
        ),
        (
            RunStatus::Completed,
            false,
            false,
            ReportTo::Launcher,
            Some(OwedWake::Subagents),
        ),
        (RunStatus::Completed, true, false, ReportTo::Launcher, None),
        (RunStatus::Completed, false, true, ReportTo::Launcher, None),
        (RunStatus::Running, false, false, ReportTo::Nobody, None),
        (RunStatus::Completed, false, false, ReportTo::Nobody, None),
    ]
    .into_iter()
    .map(|row| (row, false))
    .chain([(
        (
            RunStatus::Running,
            false,
            false,
            ReportTo::Launcher,
            Some(OwedWake::Subagents),
        ),
        true,
    )]) {
        let (dir, store) = fixture();
        register(&store, "parent", None);
        register(&store, "child", Some("parent"));
        let kind = AgentKind::new_unchecked("codex");
        let mut run = RunRecord::new(
            store.paths().workspace_id.clone(),
            kind.clone(),
            PermissionMode::Auto,
            "work".to_owned(),
            dir.path().to_owned(),
        );
        run.agent_id = Some("child".into());
        run.status = status;
        run.report_to = report_to;
        run.joined_at = joined.then_some(jiff::Timestamp::now());
        run.report_message_id = reported.then(crate::MessageId::new);
        let at = jiff::Timestamp::now();
        run.limit_hold = held.then_some(crate::store::run::LimitHold {
            activity: at,
            since: at,
            until: at,
        });
        super::super::run::create(store.paths(), &run).unwrap();
        if status.is_terminal() {
            let observation =
                AgentLifecycleObservation::new(Some("child".into()), LifecycleSignal::Ended);
            store
                .append_agent_lifecycle(AgentLifecycleIntent {
                    session_name: "owed-test",
                    agent_kind: kind.clone(),
                    event_name: "test",
                    observation: &observation,
                    spawned_subagents: &[],
                })
                .unwrap();
        }
        assert_eq!(
            owed_wake(&store, &kind, &"parent".into()).unwrap(),
            expected,
            "{status:?} joined={joined} reported={reported} {report_to:?} held={held}"
        );
    }
}
