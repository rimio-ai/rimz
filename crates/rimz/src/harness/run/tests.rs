use super::*;
use std::path::Path;

use crate::agents::AgentState;
use crate::agents::LifecycleSignal;
use crate::ids::{AgentKind, MuxName, WorkspaceId};
use crate::pane::PaneRef;
use tempfile::tempdir;

fn setup() -> (tempfile::TempDir, StatePaths, RunRecord) {
    setup_for("claude")
}

fn setup_for(kind: &str) -> (tempfile::TempDir, StatePaths, RunRecord) {
    let dir = tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
    let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let record = RunRecord::new(
        workspace_id,
        AgentKind::new_unchecked(kind),
        PermissionMode::Auto,
        "go".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    create(&paths, &record).unwrap();
    (dir, paths, record)
}

#[test]
fn durable_deadline_defaults_for_old_records_and_times_out_once_due() {
    let (_dir, paths, record) = setup();
    let mut old_json = serde_json::to_value(&record).expect("serialize run");
    old_json
        .as_object_mut()
        .expect("run object")
        .remove("deadline_at");
    let old: RunRecord = serde_json::from_value(old_json).expect("deserialize old run");
    assert_eq!(old.deadline_at, None);

    let deadline = record.started_at + std::time::Duration::from_secs(30);
    let mut timed = record.clone();
    timed.deadline_at = Some(deadline);
    create(&paths, &timed).expect("write deadline");

    let (_, wrote) = timeout_if_due(
        &paths,
        &record.run_id,
        deadline - std::time::Duration::from_secs(1),
    )
    .expect("check early deadline");
    assert!(!wrote);

    let (settled, wrote) =
        timeout_if_due(&paths, &record.run_id, deadline).expect("settle due deadline");
    assert!(wrote);
    assert_eq!(settled.status, RunStatus::TimedOut);
    assert_eq!(settled.completed_at, Some(deadline));

    let (_, wrote) = timeout_if_due(&paths, &record.run_id, deadline).expect("repeat due deadline");
    assert!(!wrote);
}

#[test]
fn stranded_park_requires_two_checks_and_preserves_racing_turns() {
    let (dir, paths, mut record) = setup();
    let runtime =
        crate::RuntimePaths::under(record.workspace_id.clone(), &dir.path().join("rt")).unwrap();
    let store = Store::open(paths, runtime).unwrap();
    let socket = std::os::unix::net::UnixDatagram::bind(crate::store::run::run_socket_path(
        store.runtime_paths(),
        &record.run_id,
    ))
    .unwrap();
    socket.set_nonblocking(true).unwrap();
    let mut frame = [0; 1024];
    record.status = RunStatus::Running;
    record.agent_id = Some("session".into());
    create(store.paths(), &record).unwrap();
    assert_eq!(
        settle_stranded_park(&store, &record, None).unwrap(),
        ParkCheck::Live
    );
    assert_eq!(load(store.paths(), &record.run_id).unwrap(), record);

    let at = Timestamp::now();
    record.parked_at = Some(at);
    create(store.paths(), &record).unwrap();
    assert_eq!(
        settle_stranded_park(&store, &record, None).unwrap(),
        ParkCheck::Stranded(at)
    );
    assert_eq!(load(store.paths(), &record.run_id).unwrap(), record);

    for parked_at in [None, Some(at + std::time::Duration::from_secs(1))] {
        let mut revived = record.clone();
        revived.parked_at = parked_at;
        create(store.paths(), &revived).unwrap();
        assert_eq!(
            settle_stranded_park(&store, &record, Some(at)).unwrap(),
            ParkCheck::Live
        );
        assert_eq!(load(store.paths(), &record.run_id).unwrap(), revived);
    }
    assert_eq!(
        socket.recv(&mut frame).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );

    for tail in [None, Some("earlier evidence".to_owned())] {
        record.failure_tail = tail.clone();
        create(store.paths(), &record).unwrap();
        assert_eq!(
            settle_stranded_park(&store, &record, Some(at)).unwrap(),
            ParkCheck::Settled
        );
        let failed = load(store.paths(), &record.run_id).unwrap();
        assert_eq!(failed.status, RunStatus::Failed);
        assert_eq!(failed.parked_at, None);
        let count = socket.recv(&mut frame).unwrap();
        let crate::store::run::WakeupFrame::RunCompleted {
            workspace_id,
            run_id,
            status,
        } = serde_json::from_slice(&frame[..count]).unwrap();
        assert_eq!(workspace_id, record.workspace_id);
        assert_eq!(run_id, record.run_id);
        assert_eq!(status, RunStatus::Failed);
        assert_eq!(
            failed.failure_tail.as_deref(),
            Some(tail.as_deref().unwrap_or(STRANDED_PARK_REASON))
        );
        assert_eq!(
            settle_stranded_park(&store, &failed, Some(at)).unwrap(),
            ParkCheck::Live
        );
        assert_eq!(
            settle_stranded_park(&store, &record, Some(at)).unwrap(),
            ParkCheck::Live
        );
    }
}

#[test]
fn fold_lifecycle_maps_each_disposition_to_its_run_status() {
    for (signal, expected) in [
        (
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
                turn_id: None,
            },
            RunStatus::Completed,
        ),
        (
            LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
                turn_id: None,
            },
            RunStatus::Failed,
        ),
        (
            LifecycleSignal::TurnInterrupted { turn_id: None },
            RunStatus::Canceled,
        ),
        (LifecycleSignal::Ended, RunStatus::Failed),
    ] {
        for owed in [None, Some(OwedWake::Wait)] {
            let (_dir, paths, record) = setup();
            let observation = AgentLifecycleObservation::new(
                Some(AgentSessionId::from("sess-1")),
                signal.clone(),
            );
            let mut consulted = false;
            let completed =
                record_lifecycle(&paths, &record.run_id, "claude", &observation, None, || {
                    let _guard = WorkspaceLock::try_acquire(&paths.workspace_lock)
                        .unwrap()
                        .expect("owed reads must precede the workspace lock");
                    consulted = true;
                    owed
                })
                .unwrap();
            assert_eq!(consulted, expected == RunStatus::Completed);
            let parked = expected == RunStatus::Completed && owed.is_some();
            assert_eq!(completed.is_none(), parked);
            let persisted = load(&paths, &record.run_id).unwrap();
            assert_eq!(
                persisted.status,
                if parked { RunStatus::Running } else { expected },
                "{:?}",
                observation.signal
            );
            assert_eq!(persisted.parked_at.is_some(), parked);
            assert_eq!(persisted.completed_at.is_none(), parked);
        }
    }
}

#[test]
fn parked_run_resumes_and_completes_once() {
    let (_dir, paths, record) = setup();
    let mut ended = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
    );
    ended.transcript_path = Some("/tmp/parked.jsonl".to_owned());
    assert!(
        record_lifecycle(
            &paths,
            &record.run_id,
            "claude",
            &ended,
            Some("waiting".to_owned()),
            || Some(OwedWake::Wait)
        )
        .unwrap()
        .is_none()
    );
    let parked = load(&paths, &record.run_id).unwrap();
    assert_eq!(parked.status, RunStatus::Running);
    assert!(parked.parked_at.is_some());
    assert_eq!(parked.completed_at, None);
    assert_eq!(parked.transcript_path, ended.transcript_path);
    assert_eq!(parked.last_message.as_deref(), Some("waiting"));

    let mut observation = ended.clone();
    observation.signal = LifecycleSignal::ToolUsed {
        mutates: false,
        edits: false,
        name: None,
        native_key: None,
        turn_id: None,
    };
    record_lifecycle(&paths, &record.run_id, "claude", &observation, None, || {
        panic!("non-completion must not read owed")
    })
    .unwrap();
    assert_eq!(
        load(&paths, &record.run_id).unwrap().parked_at,
        parked.parked_at
    );
    observation.signal = LifecycleSignal::TurnStarted { turn_id: None };
    record_lifecycle(&paths, &record.run_id, "claude", &observation, None, || {
        panic!("turn start must not read owed")
    })
    .unwrap();
    let running = load(&paths, &record.run_id).unwrap();
    assert_eq!(running.parked_at, None);
    assert_eq!(running.status, RunStatus::Running);

    let completed = record_lifecycle(
        &paths,
        &record.run_id,
        "claude",
        &ended,
        Some("done".to_owned()),
        || None,
    )
    .unwrap()
    .expect("terminal update");
    assert_eq!(completed.status, RunStatus::Completed);
    assert_eq!(completed.parked_at, None);
    assert_eq!(completed.last_message.as_deref(), Some("done"));
    assert!(
        record_lifecycle(&paths, &record.run_id, "claude", &ended, None, || None)
            .unwrap()
            .is_none()
    );
    assert_eq!(load(&paths, &record.run_id).unwrap(), completed);
}

#[test]
fn terminal_writes_clear_parked_runs() {
    for status in [RunStatus::Canceled, RunStatus::TimedOut, RunStatus::Failed] {
        let (_dir, paths, record) = setup();
        let mut observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("sess-1")),
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
                turn_id: None,
            },
        );
        record_lifecycle(&paths, &record.run_id, "claude", &observation, None, || {
            Some(OwedWake::Subagents)
        })
        .unwrap();
        assert!(load(&paths, &record.run_id).unwrap().parked_at.is_some());
        match status {
            RunStatus::Canceled => {
                cancel(&paths, &record.run_id).unwrap();
            }
            RunStatus::TimedOut => {
                timeout(&paths, &record.run_id).unwrap();
            }
            _ => {
                observation.signal = LifecycleSignal::Ended;
                record_lifecycle(&paths, &record.run_id, "claude", &observation, None, || {
                    panic!("failure must not read owed")
                })
                .unwrap()
                .expect("terminal update");
            }
        }
        let terminal = load(&paths, &record.run_id).unwrap();
        assert_eq!(terminal.status, status);
        assert_eq!(terminal.parked_at, None);
    }
}

#[test]
fn lifecycle_completion_writes_terminal_record_once() {
    let (_dir, paths, record) = setup();
    let observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
    );
    let completed = record_lifecycle(
        &paths,
        &record.run_id,
        "claude",
        &observation,
        Some("done".to_owned()),
        || None,
    )
    .unwrap()
    .expect("terminal update");
    assert_eq!(completed.status, RunStatus::Completed);
    assert_eq!(completed.last_message.as_deref(), Some("done"));
    assert_eq!(completed.agent_id.as_deref(), Some("sess-1"));

    let repeated = record_lifecycle(
        &paths,
        &record.run_id,
        "claude",
        &observation,
        Some("done".to_owned()),
        || None,
    )
    .unwrap();
    assert!(repeated.is_none());
}

#[test]
fn subagent_observation_does_not_complete_parent_run() {
    let (_dir, paths, record) = setup();
    let mut observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("child-1")),
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
    );
    observation.parent_agent_id = Some(AgentSessionId::from("sess-parent"));

    let update = record_lifecycle(
        &paths,
        &record.run_id,
        "claude",
        &observation,
        Some("child done".to_owned()),
        || panic!("subagent observation must not read owed"),
    )
    .unwrap();
    assert!(update.is_none());
    let after = load(&paths, &record.run_id).unwrap();
    assert_eq!(after.status, RunStatus::Pending);
    assert_eq!(after.last_message, None);
}

#[test]
fn same_kind_child_process_does_not_complete_bound_parent_run() {
    let (_dir, paths, record) = setup();
    let parent = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-parent")),
        LifecycleSignal::TurnStarted { turn_id: None },
    );
    record_lifecycle(&paths, &record.run_id, "claude", &parent, None, || None).unwrap();

    let child = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-child")),
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
    );
    let update = record_lifecycle(
        &paths,
        &record.run_id,
        "claude",
        &child,
        Some("child done".to_owned()),
        || None,
    )
    .unwrap();

    assert!(update.is_none());
    let after = load(&paths, &record.run_id).unwrap();
    assert_eq!(after.status, RunStatus::Running);
    assert_eq!(after.agent_id.as_deref(), Some("sess-parent"));
    assert_eq!(after.last_message, None);
}

#[test]
fn terminal_transitions_are_once_only_and_map_exit_codes() {
    let (_dir, paths, record) = setup();
    let timed_out = timeout(&paths, &record.run_id).unwrap();
    assert_eq!(timed_out.status, RunStatus::TimedOut);
    assert!(timed_out.completed_at.is_some());
    assert_eq!(timed_out.status.exit_code(), 124);

    let still_timed_out = fail(&paths, &record.run_id).unwrap();
    assert_eq!(still_timed_out.status, RunStatus::TimedOut);

    let (_dir, paths, record) = setup();
    let (canceled, wrote) = cancel(&paths, &record.run_id).unwrap();
    assert!(wrote);
    assert_eq!(canceled.status, RunStatus::Canceled);
    assert!(canceled.completed_at.is_some());
    assert_eq!(canceled.status.exit_code(), 130);

    let (still_canceled, wrote) = cancel(&paths, &record.run_id).unwrap();
    assert!(!wrote);
    assert_eq!(still_canceled.status, RunStatus::Canceled);

    let (_dir, paths, record) = setup();
    let (budgeted, wrote) = budget_exceeded(&paths, &record.run_id, Some(5.25)).unwrap();
    assert!(wrote);
    assert_eq!(budgeted.status, RunStatus::BudgetExceeded);
    assert_eq!(budgeted.cost_usd, Some(5.25));
    assert_eq!(budgeted.status.exit_code(), 125);

    assert_eq!(RunStatus::Completed.exit_code(), 0);
    assert_eq!(RunStatus::Failed.exit_code(), 1);
    assert_eq!(RunStatus::VerifyFailed.exit_code(), 123);
    assert_eq!(RunStatus::BudgetExceeded.exit_code(), 125);
    assert!(RunStatus::Failed.is_retryable());
    assert!(RunStatus::VerifyFailed.is_terminal());
    assert!(!RunStatus::VerifyFailed.is_retryable());
    assert!(!RunStatus::Completed.is_retryable());
    assert!(!RunStatus::TimedOut.is_retryable());
    assert!(!RunStatus::BudgetExceeded.is_retryable());
    assert!(!RunStatus::Canceled.is_retryable());
    assert!(RunStatus::BudgetExceeded.is_terminal());
    assert!(RunStatus::Canceled.is_terminal());
}

#[test]
fn verify_transitions_reopen_completed_runs_and_finish_once() {
    let (_dir, paths, record) = setup();
    let completed = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
    );
    record_lifecycle(&paths, &record.run_id, "claude", &completed, None, || None)
        .unwrap()
        .expect("completed run");
    let first = RunVerify {
        cmd: "cargo xtask test run".to_owned(),
        attempts: 1,
        passed: false,
        code: Some(1),
        timed_out: false,
        output: "red".to_owned(),
    };

    let reopened = reopen_for_verify(&paths, &record.run_id, first.clone()).unwrap();
    assert_eq!(reopened.status, RunStatus::Running);
    assert_eq!(reopened.completed_at, None);
    assert_eq!(reopened.verify.as_ref(), Some(&first));
    assert!(reopen_for_verify(&paths, &record.run_id, first.clone()).is_err());

    record_lifecycle(&paths, &record.run_id, "claude", &completed, None, || None)
        .unwrap()
        .expect("second completed turn");
    let second = RunVerify {
        attempts: 2,
        output: "still red".to_owned(),
        ..first
    };
    let failed = verify_failed(&paths, &record.run_id, second.clone()).unwrap();
    assert_eq!(failed.status, RunStatus::VerifyFailed);
    assert_eq!(failed.verify.as_ref(), Some(&second));
    let updated_at = failed.updated_at;

    let repeated = verify_failed(&paths, &record.run_id, second).unwrap();
    assert_eq!(repeated.updated_at, updated_at);
}

#[test]
fn record_spend_persists_tokens_and_ignores_non_finite_cost() {
    let (_dir, paths, record) = setup();

    let updated = record_spend(
        &paths,
        &record.run_id,
        Some(f64::NAN),
        Some(1_200),
        Some(340),
    )
    .unwrap();

    assert_eq!(updated.cost_usd, None);
    assert_eq!(updated.input_tokens, Some(1_200));
    assert_eq!(updated.output_tokens, Some(340));
    let unchanged = record_spend(&paths, &record.run_id, None, None, None).unwrap();
    assert_eq!(unchanged, updated);

    for invalid in [f64::INFINITY, -1.0] {
        let unchanged = record_spend(&paths, &record.run_id, Some(invalid), None, None).unwrap();
        assert_eq!(unchanged, updated);
    }
}

#[test]
fn lifecycle_and_assistant_messages_require_matching_live_root_run() {
    let (_dir, paths, record) = setup();
    let started = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::TurnStarted { turn_id: None },
    );

    assert!(
        record_lifecycle(&paths, &record.run_id, "codex", &started, None, || None)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        load(&paths, &record.run_id).unwrap().status,
        RunStatus::Pending
    );

    record_lifecycle(&paths, &record.run_id, "claude", &started, None, || None).unwrap();
    record_assistant_message(
        &paths,
        &record.run_id,
        "claude",
        &AgentSessionId::from("sess-1"),
        "matching".to_owned(),
    )
    .unwrap();
    for (kind, session) in [("codex", "sess-1"), ("claude", "sess-2")] {
        record_assistant_message(
            &paths,
            &record.run_id,
            kind,
            &AgentSessionId::from(session),
            "ignored".to_owned(),
        )
        .unwrap();
    }
    assert_eq!(
        load(&paths, &record.run_id)
            .unwrap()
            .last_message
            .as_deref(),
        Some("matching")
    );
}

#[test]
fn record_lifecycle_folds_transcript_path_on_run_writes() {
    let (_dir, paths, record) = setup();

    let mut started = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::TurnStarted { turn_id: None },
    );
    started.transcript_path = Some("/tmp/first.jsonl".to_owned());
    assert!(
        record_lifecycle(&paths, &record.run_id, "claude", &started, None, || None)
            .unwrap()
            .is_none()
    );
    let running = load(&paths, &record.run_id).unwrap();
    assert_eq!(running.status, RunStatus::Running);
    assert_eq!(running.transcript_path.as_deref(), Some("/tmp/first.jsonl"));

    let mut tool = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::ToolUsed {
            mutates: true,
            edits: true,
            name: None,
            native_key: None,
            turn_id: None,
        },
    );
    tool.transcript_path = Some("/tmp/second.jsonl".to_owned());
    record_lifecycle(&paths, &record.run_id, "claude", &tool, None, || None).unwrap();
    assert_eq!(
        load(&paths, &record.run_id)
            .unwrap()
            .transcript_path
            .as_deref(),
        Some("/tmp/first.jsonl"),
        "a non-terminal running observation does not add a run-store write"
    );

    let mut stopped = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
    );
    stopped.transcript_path = Some("/tmp/second.jsonl".to_owned());
    record_lifecycle(
        &paths,
        &record.run_id,
        "claude",
        &stopped,
        Some("done".to_owned()),
        || None,
    )
    .unwrap();
    assert_eq!(
        load(&paths, &record.run_id)
            .unwrap()
            .transcript_path
            .as_deref(),
        Some("/tmp/second.jsonl")
    );
}

#[test]
fn record_lifecycle_folds_first_late_transcript_path() {
    let (_dir, paths, record) = setup_for("codex");

    let started = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::TurnStarted { turn_id: None },
    );
    record_lifecycle(&paths, &record.run_id, "codex", &started, None, || None).unwrap();
    let running = load(&paths, &record.run_id).unwrap();
    assert_eq!(running.status, RunStatus::Running);
    assert_eq!(running.transcript_path, None);

    let mut tool = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::ToolUsed {
            mutates: true,
            edits: true,
            name: None,
            native_key: None,
            turn_id: None,
        },
    );
    tool.transcript_path = Some("/tmp/late.jsonl".to_owned());
    record_lifecycle(&paths, &record.run_id, "codex", &tool, None, || None).unwrap();
    assert_eq!(
        load(&paths, &record.run_id)
            .unwrap()
            .transcript_path
            .as_deref(),
        Some("/tmp/late.jsonl")
    );
}

#[test]
fn live_status_joins_agent_state() {
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
    let mut record = RunRecord::new(
        workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "go".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    record.status = RunStatus::Running;
    record.agent_id = Some(AgentSessionId::from("sess-1"));
    let pane_id = PaneId::from_parts(MuxName::Tmux, "%7");
    let mut pane = PaneRef::from_id(pane_id.clone());
    pane.session_name = "rimz-test".to_owned();
    let mut agent = agent_state("claude", "sess-1", AgentStatus::Waiting);
    agent.phase = TurnPhase::Idle;
    agent.pane = Some(pane);
    agent.usage.context_pct = Some(42);
    agent.waiting_since = Some(Timestamp::UNIX_EPOCH);
    let snapshot =
        SidebarSnapshot::build_with_agents(workspace_id, vec![agent], Timestamp::UNIX_EPOCH);

    let live = live_status(&record, &snapshot).expect("live status");
    assert_eq!(live.agent_status, AgentStatus::Waiting);
    assert_eq!(live.phase, TurnPhase::Idle);
    assert_eq!(live.pane_id.as_ref(), Some(&pane_id));
    assert_eq!(live.context_pct, Some(42));
}

#[test]
fn live_status_is_absent_for_unbound_or_terminal_runs() {
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
    let mut record = RunRecord::new(
        workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "go".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    record.status = RunStatus::Running;
    let snapshot = SidebarSnapshot::build_with_agents(
        workspace_id,
        vec![agent_state("claude", "sess-1", AgentStatus::Running)],
        Timestamp::UNIX_EPOCH,
    );
    assert!(live_status(&record, &snapshot).is_none());

    record.agent_id = Some(AgentSessionId::from("sess-1"));
    record.status = RunStatus::Completed;
    assert!(live_status(&record, &snapshot).is_none());
}

#[test]
fn record_pane_persists_launch_pane_id() {
    let (_dir, paths, record) = setup();
    let pane_id = PaneId::from_parts(MuxName::Tmux, "%7");

    let updated = record_pane(&paths, &record.run_id, pane_id.clone()).unwrap();
    assert_eq!(updated.pane_id.as_ref(), Some(&pane_id));
    assert_eq!(
        load(&paths, &record.run_id).unwrap().pane_id.as_ref(),
        Some(&pane_id)
    );
}

#[test]
fn record_provider_process_persists_pid_reuse_guard() {
    let (_dir, paths, record) = setup();

    let updated =
        record_provider_process(&paths, &record.run_id, 42, Some("start-42".to_owned())).unwrap();

    assert_eq!(updated.provider_pid, Some(42));
    assert_eq!(updated.provider_process_start.as_deref(), Some("start-42"));
    let loaded = load(&paths, &record.run_id).unwrap();
    assert_eq!(loaded.provider_pid, Some(42));
    assert_eq!(loaded.provider_process_start.as_deref(), Some("start-42"));
}

#[test]
fn record_failure_tail_persists_first_non_empty_tail() {
    let (_dir, paths, record) = setup();

    let stored = record_failure_tail(&paths, &record.run_id, "first\n\n").unwrap();
    assert_eq!(stored.failure_tail.as_deref(), Some("first"));

    let unchanged = record_failure_tail(&paths, &record.run_id, "second").unwrap();
    assert_eq!(unchanged.failure_tail.as_deref(), Some("first"));
    assert_eq!(
        load(&paths, &record.run_id)
            .unwrap()
            .failure_tail
            .as_deref(),
        Some("first")
    );
}

#[test]
fn record_failure_tail_ignores_empty_tail() {
    let (_dir, paths, record) = setup();

    let stored = record_failure_tail(&paths, &record.run_id, " \n\t").unwrap();

    assert_eq!(stored.failure_tail, None);
    assert_eq!(load(&paths, &record.run_id).unwrap().failure_tail, None);
}

#[test]
fn record_failure_tail_caps_stored_tail() {
    let (_dir, paths, record) = setup();
    let tail = format!("{}{}", "a".repeat(FAILURE_TAIL_CAP), "b".repeat(20));

    let stored = record_failure_tail(&paths, &record.run_id, &tail).unwrap();

    let stored = stored.failure_tail.expect("tail");
    assert_eq!(stored.len(), FAILURE_TAIL_CAP);
    assert!(stored.starts_with('a'));
    assert!(stored.ends_with('b'));
}

fn agent_state(kind: &str, id: &str, status: AgentStatus) -> AgentState {
    let mut agent = crate::sidebar::test_support::root_agent(kind, id, None);
    agent.name = None;
    agent.kind_ordinal = None;
    agent.status = status;
    agent.last_seen = Timestamp::UNIX_EPOCH;
    agent.last_activity = Timestamp::UNIX_EPOCH;
    agent.registered_at = Some(Timestamp::UNIX_EPOCH);
    agent
}
