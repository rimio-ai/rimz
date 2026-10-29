use super::*;
use crate::agents::{AgentLifecycleObservation, AgentState, AgentStatus, LifecycleSignal};
use crate::disk::lock::{SharedAppendLock, WorkspaceLock};
use crate::ids::{AgentKind, RunId, WorkflowRunId};
use crate::store::ingress::{self, HookIngress};
use crate::store::message::{DeliveryGate, HarnessNotice, MessageRecord, MessageSender};
use crate::{RuntimePaths, StatePaths, Store, WorkspaceId};
use jiff::Timestamp;

fn fixture() -> (tempfile::TempDir, Store, AgentState) {
    let dir = tempfile::tempdir().unwrap();
    let id = WorkspaceId::from_project_root(dir.path());
    let state = StatePaths::under(id.clone(), &dir.path().join("state")).unwrap();
    let runtime = RuntimePaths::under(id, &dir.path().join("runtime")).unwrap();
    let store = Store::open(state, runtime).unwrap();
    let now = Timestamp::now();
    let mut agent = AgentState::seed(
        AgentKind::new_unchecked("claude"),
        "leader".into(),
        AgentStatus::Idle,
        now,
    );
    agent.name = Some("leader".into());
    agent.turn_started_at = Some(now);
    agent.turn_ended_at = Some(now);
    (dir, store, agent)
}

#[test]
fn leader_turn_requires_a_closed_turn_and_accepts_equal_stamps() {
    let (_dir, store, agent) = fixture();
    let run = WorkflowRunId::new();
    let session = RunId::new();
    let mut actual = Vec::new();
    for (started, ended) in [
        (None, None),
        (agent.turn_started_at, None),
        (None, agent.turn_ended_at),
        (
            Some(Timestamp::now() + std::time::Duration::from_secs(1)),
            agent.turn_ended_at,
        ),
        (agent.turn_started_at, agent.turn_ended_at),
    ] {
        let mut row = agent.clone();
        row.turn_started_at = started;
        row.turn_ended_at = ended;
        let _guard = WorkspaceLock::acquire(&store.paths().workspace_lock).unwrap();
        actual.push(
            leader_turn(&store, &row, &run, &session, None)
                .unwrap()
                .map(|turn| turn.at_rest),
        );
    }
    assert_eq!(actual, vec![None, None, None, None, Some(true)]);
}

#[test]
fn leader_turn_checks_debt_and_per_run_ingress_under_the_workspace_lock() {
    let mut actual = Vec::new();
    for cause in [
        "none",
        "own run",
        "other run",
        "message",
        "owed wake",
        "shell",
        "compacting",
        "own ingress",
        "other ingress",
        "waking ingress",
        "waking and later ingress",
    ] {
        let (dir, store, mut agent) = fixture();
        let run = WorkflowRunId::new();
        let mut session = RunId::new();
        let mut woken_by = None;
        if matches!(cause, "own run" | "other run") {
            let mut record = crate::store::run::RunRecord::new(
                store.paths().workspace_id.clone(),
                agent.kind.clone(),
                crate::agents::PermissionMode::Auto,
                "work".into(),
                dir.path().into(),
            );
            record.agent_id = Some(agent.agent_id.clone());
            record.status = RunStatus::Running;
            if cause == "own run" {
                session = record.run_id.clone();
            }
            crate::harness::run::create(store.paths(), &record).unwrap();
        }
        if matches!(cause, "message" | "owed wake") {
            let observation = AgentLifecycleObservation::new(
                Some(agent.agent_id.clone()),
                LifecycleSignal::Registered,
            );
            store
                .append_event(&crate::EventEnvelope::agent_lifecycle(
                    store.paths().workspace_id.clone(),
                    "turn-test",
                    "claude",
                    "test",
                    &observation,
                ))
                .unwrap();
            let mut message = MessageRecord::new(
                store.paths().workspace_id.clone(),
                &agent,
                "work".into(),
                DeliveryGate::Done,
            );
            if cause == "owed wake" {
                message = message.with_sender(MessageSender::Harness {
                    notice: HarnessNotice::Wait,
                });
            }
            store.queue_message(&message, "turn-test").unwrap();
            if cause == "owed wake" {
                assert!(
                    crate::harness::owed::owed_wake(&store, &agent.kind, &agent.agent_id)
                        .unwrap()
                        .is_some()
                );
            }
        }
        if cause == "shell" {
            agent
                .background_shells
                .push(crate::agents::BackgroundShell {
                    id: "shell".into(),
                    command: None,
                    description: None,
                    started_at: Timestamp::now(),
                });
        }
        if cause == "compacting" {
            agent.compacting_since = Some(Timestamp::now());
        }
        if matches!(
            cause,
            "own ingress" | "other ingress" | "waking ingress" | "waking and later ingress"
        ) {
            let frame = HookIngress {
                boot_id: None,
                schema_version: "1".into(),
                event_id: crate::ids::EventId::new(),
                ts: Timestamp::now(),
                source: agent.kind.clone(),
                event: Some("Stop".into()),
                payload: "{}".into(),
                cwd: dir.path().into(),
                hook_pid: std::process::id(),
                env: [(
                    "RIMZ_WORKFLOW_RUN".into(),
                    if cause == "other ingress" {
                        WorkflowRunId::new().to_string()
                    } else {
                        run.to_string()
                    },
                )]
                .into(),
            };
            let guard = SharedAppendLock::acquire(&store.paths().hook_ingress_lock).unwrap();
            ingress::append(store.paths(), &frame, &guard).unwrap();
            if matches!(cause, "waking ingress" | "waking and later ingress") {
                woken_by = Some(frame.event_id.clone());
            }
            if cause == "waking and later ingress" {
                let mut later = frame.clone();
                later.event_id = crate::ids::EventId::new();
                later.event = Some("UserPromptSubmit".into());
                ingress::append(store.paths(), &later, &guard).unwrap();
            }
        }
        let _guard = WorkspaceLock::acquire(&store.paths().workspace_lock).unwrap();
        actual.push((
            cause,
            leader_turn(&store, &agent, &run, &session, woken_by.as_ref())
                .unwrap()
                .map(|turn| turn.at_rest),
        ));
    }
    assert_eq!(
        actual,
        vec![
            ("none", Some(true)),
            ("own run", Some(true)),
            ("other run", Some(false)),
            ("message", Some(false)),
            ("owed wake", Some(false)),
            ("shell", Some(false)),
            ("compacting", Some(false)),
            ("own ingress", Some(false)),
            ("other ingress", Some(true)),
            ("waking ingress", Some(true)),
            ("waking and later ingress", Some(false)),
        ]
    );
}
