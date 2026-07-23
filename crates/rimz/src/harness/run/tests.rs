use super::*;
use std::path::Path;

use crate::agents::AgentState;
use crate::agents::LifecycleSignal;
use crate::ids::MuxName;
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
fn lifecycle_completion_writes_terminal_record_once() {
    let (_dir, paths, record) = setup();
    let observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        },
    );
    let completed = record_lifecycle(
        &paths,
        &record.run_id,
        "claude",
        &observation,
        Some("done".to_owned()),
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
        },
    );
    observation.parent_agent_id = Some(AgentSessionId::from("sess-parent"));

    let update = record_lifecycle(
        &paths,
        &record.run_id,
        "claude",
        &observation,
        Some("child done".to_owned()),
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
        LifecycleSignal::TurnStarted,
    );
    record_lifecycle(&paths, &record.run_id, "claude", &parent, None).unwrap();

    let child = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-child")),
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        },
    );
    let update = record_lifecycle(
        &paths,
        &record.run_id,
        "claude",
        &child,
        Some("child done".to_owned()),
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
fn retry_link_round_trips_and_defaults_when_absent() {
    let (_dir, _paths, mut record) = setup();
    let prior = RunId::new();
    record.retry_of = Some(prior.clone());

    let mut value = serde_json::to_value(&record).unwrap();
    let decoded: RunRecord = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(decoded.retry_of.as_ref(), Some(&prior));

    value.as_object_mut().unwrap().remove("retry_of");
    let decoded: RunRecord = serde_json::from_value(value).unwrap();
    assert_eq!(decoded.retry_of, None);
}

#[test]
fn verify_transitions_reopen_completed_runs_and_finish_once() {
    let (_dir, paths, record) = setup();
    let completed = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        },
    );
    record_lifecycle(&paths, &record.run_id, "claude", &completed, None)
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

    record_lifecycle(&paths, &record.run_id, "claude", &completed, None)
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
        LifecycleSignal::TurnStarted,
    );

    assert!(
        record_lifecycle(&paths, &record.run_id, "codex", &started, None)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        load(&paths, &record.run_id).unwrap().status,
        RunStatus::Pending
    );

    record_lifecycle(&paths, &record.run_id, "claude", &started, None).unwrap();
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
        LifecycleSignal::TurnStarted,
    );
    started.transcript_path = Some("/tmp/first.jsonl".to_owned());
    assert!(
        record_lifecycle(&paths, &record.run_id, "claude", &started, None)
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
        },
    );
    tool.transcript_path = Some("/tmp/second.jsonl".to_owned());
    record_lifecycle(&paths, &record.run_id, "claude", &tool, None).unwrap();
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
        },
    );
    stopped.transcript_path = Some("/tmp/second.jsonl".to_owned());
    record_lifecycle(
        &paths,
        &record.run_id,
        "claude",
        &stopped,
        Some("done".to_owned()),
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
        LifecycleSignal::TurnStarted,
    );
    record_lifecycle(&paths, &record.run_id, "codex", &started, None).unwrap();
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
        },
    );
    tool.transcript_path = Some("/tmp/late.jsonl".to_owned());
    record_lifecycle(&paths, &record.run_id, "codex", &tool, None).unwrap();
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

#[test]
fn retry_prompt_includes_the_latest_failure_tail() {
    let prompt = retry_prompt("fix it", Some("error: broken\nlast line"));

    assert!(prompt.starts_with("fix it\n\n<previous-attempt-failure>"));
    assert!(prompt.contains("The tail of its terminal output:\nerror: broken\nlast line"));
    assert!(prompt.ends_with("</previous-attempt-failure>"));
}

#[test]
fn retry_prompt_explains_when_no_tail_was_captured() {
    let prompt = retry_prompt("fix it", None);

    assert!(prompt.contains("no terminal output was captured"));
}

#[test]
fn retry_prompt_recomposes_from_the_base_without_nesting() {
    let first = retry_prompt("fix it", Some("first failure"));
    let second = retry_prompt("fix it", Some("second failure"));

    assert!(first.contains("first failure"));
    assert!(!second.contains("first failure"));
    assert_eq!(second.matches("<previous-attempt-failure>").count(), 1);
}

#[test]
fn verify_reprompt_formats_status_and_caps_the_output_tail() {
    let output = format!("old{}latest", "x".repeat(FAILURE_TAIL_CAP));

    let prompt = verify_reprompt("cargo xtask test auth", "1", &output);

    assert!(prompt.starts_with("Verification failed — the task is not done yet."));
    assert!(prompt.contains("--- verify `cargo xtask test auth` exited 1 ---"));
    assert!(!prompt.contains("old"));
    assert!(prompt.ends_with("latest"));
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
