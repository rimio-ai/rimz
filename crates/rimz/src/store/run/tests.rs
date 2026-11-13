use super::*;
use crate::agents::PermissionMode;
use crate::ids::{AgentKind, WorkspaceId};
use tempfile::tempdir;

fn listing_record() -> RunRecord {
    RunRecord::new(
        WorkspaceId::from_project_root(Path::new("/repo")),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "task".into(),
        "/repo".into(),
    )
}

fn set_directory_mtime(path: &Path, modified: std::time::SystemTime) {
    fs::File::open(path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
}

#[test]
fn run_listing_aged_directory_skips_record_stats_and_enumeration() {
    let dir = tempdir().unwrap();
    write(dir.path(), &listing_record()).unwrap();
    set_directory_mtime(dir.path(), std::time::SystemTime::UNIX_EPOCH);
    let cold = listing(dir.path()).unwrap();
    let before = super::super::sidecar::testkit::read_counts();
    let warm = listing(dir.path()).unwrap();
    assert_eq!(super::super::sidecar::testkit::read_counts(), before);
    assert_eq!(warm.generation, cold.generation);
    assert!(Arc::ptr_eq(&cold.records, &warm.records));
}

#[test]
fn run_listing_sees_write_after_aged_reuse() {
    let dir = tempdir().unwrap();
    let mut record = listing_record();
    write(dir.path(), &record).unwrap();
    set_directory_mtime(dir.path(), std::time::SystemTime::UNIX_EPOCH);
    let cold = listing(dir.path()).unwrap();
    assert_eq!(listing(dir.path()).unwrap().generation, cold.generation);
    record.prompt = "new task".into();
    write(dir.path(), &record).unwrap();
    let changed = listing(dir.path()).unwrap();
    assert_eq!(changed[0].prompt, "new task");
    assert_eq!(changed.generation, cold.generation + 1);
}

#[test]
fn run_listing_sees_same_tick_write_inside_margin() {
    let dir = tempdir().unwrap();
    let mut record = listing_record();
    write(dir.path(), &record).unwrap();
    let modified = std::time::SystemTime::now();
    set_directory_mtime(dir.path(), modified);
    let cold = listing(dir.path()).unwrap();
    record.prompt = "new task".into();
    write(dir.path(), &record).unwrap();
    set_directory_mtime(dir.path(), modified);
    let changed = listing(dir.path()).unwrap();
    assert_eq!(changed[0].prompt, "new task");
    assert_eq!(changed.generation, cold.generation + 1);
}

#[test]
fn run_listing_reuses_unchanged_records_per_directory() {
    let first = tempdir().unwrap();
    let second = tempdir().unwrap();
    write(first.path(), &listing_record()).unwrap();
    let cold = listing(first.path()).unwrap();
    listing(second.path()).unwrap();
    super::super::sidecar::testkit::reset_parse_reads();
    let warm = listing(first.path()).unwrap();
    assert_eq!(super::super::sidecar::testkit::parse_reads(), 0);
    assert_eq!(cold.generation, 1);
    assert_eq!(warm.generation, cold.generation);
    assert!(std::sync::Arc::ptr_eq(&cold.records, &warm.records));
}

#[test]
fn run_listing_reloads_reopened_terminal_record() {
    let dir = tempdir().unwrap();
    let mut record = listing_record();
    record.status = RunStatus::Completed;
    write(dir.path(), &record).unwrap();
    let cold = listing(dir.path()).unwrap();
    record.status = RunStatus::Running;
    write(dir.path(), &record).unwrap();
    super::super::sidecar::testkit::reset_parse_reads();
    let reopened = listing(dir.path()).unwrap();
    assert_eq!(reopened.generation, cold.generation + 1);
    assert_eq!(reopened[0].status, RunStatus::Running);
    assert_eq!(super::super::sidecar::testkit::parse_reads(), 1);
}

#[test]
fn run_listing_detects_equal_length_and_timestamp_replacement() {
    let dir = tempdir().unwrap();
    let mut record = listing_record();
    write(dir.path(), &record).unwrap();
    let path = run_path(dir.path(), &record.run_id);
    let metadata = fs::metadata(&path).unwrap();
    let cold = listing(dir.path()).unwrap();
    record.prompt = "work".into();
    write(dir.path(), &record).unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(metadata.modified().unwrap()))
        .unwrap();
    assert_eq!(fs::metadata(&path).unwrap().len(), metadata.len());
    let changed = listing(dir.path()).unwrap();
    assert_eq!(changed.generation, cold.generation + 1);
    assert_eq!(changed[0].prompt, "work");
}

#[test]
fn run_listing_drops_deleted_records_and_ignores_temps() {
    let dir = tempdir().unwrap();
    let record = listing_record();
    write(dir.path(), &record).unwrap();
    let cold = listing(dir.path()).unwrap();
    fs::write(dir.path().join("run.json.tmp.1.2"), "invalid").unwrap();
    assert_eq!(listing(dir.path()).unwrap().generation, cold.generation);
    fs::remove_file(run_path(dir.path(), &record.run_id)).unwrap();
    let empty = listing(dir.path()).unwrap();
    assert_eq!(empty.generation, cold.generation + 1);
    assert!(empty.is_empty());
}

#[test]
fn status_label_is_the_wire_form_with_spaces() {
    for status in [
        RunStatus::Pending,
        RunStatus::Running,
        RunStatus::Completed,
        RunStatus::Failed,
        RunStatus::VerifyFailed,
        RunStatus::TimedOut,
        RunStatus::BudgetExceeded,
        RunStatus::Canceled,
    ] {
        assert_eq!(status.label(), status.as_str().replace('_', " "));
    }
}

#[test]
fn peer_run_codec_and_prompt_origin() {
    let record = RunRecord::new(
        WorkspaceId::from_project_root(Path::new("/repo")),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "task".into(),
        "/repo".into(),
    );
    let mut value = serde_json::to_value(&record).unwrap();
    assert!(value.get("peer").is_none());
    assert!(value.get("follow_ups").is_none());
    let old: RunRecord = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(old.follow_ups, 0);
    assert_eq!(old.prompt_origin(), RunPromptOrigin::Human);
    for field in ["follow_up", "opened_by", "earlier_answers", "report_to"] {
        assert!(value.get(field).is_none());
    }
    assert_eq!(old.report_to, ReportTo::Launcher);
    let mut detached = value.clone();
    detached["report_to"] = serde_json::json!("nobody");
    let decoded: RunRecord = serde_json::from_value(detached.clone()).unwrap();
    assert_eq!(decoded.report_to, ReportTo::Nobody);
    assert_eq!(serde_json::to_value(decoded).unwrap(), detached);
    let mut answers = value.clone();
    answers["follow_up"] =
        serde_json::json!({"started_at": record.started_at, "prompt": "follow up"});
    answers["opened_by"] = serde_json::json!([crate::ids::MessageId::new()]);
    answers["earlier_answers"] = serde_json::json!([{
        "ordinal": 1, "status": "completed", "started_at": record.started_at,
        "completed_at": record.started_at, "report_message_id": crate::ids::MessageId::new()
    }]);
    let decoded: RunRecord = serde_json::from_value(answers.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), answers);
    value["peer"] = serde_json::json!({"launch_id": "peer-launch"});
    let peer: RunRecord = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(peer.prompt_origin(), RunPromptOrigin::Parent);
    assert_eq!(serde_json::to_value(&peer).unwrap(), value);
    value["peer"]["opened_by"] = serde_json::json!([crate::ids::MessageId::new()]);
    let peer: RunRecord = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(peer).unwrap(), value);
}

#[test]
fn waiter_liveness_distinguishes_bound_stale_and_absent_socket() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let id = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(id, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let run_id = RunId::new();
    assert!(!run_waiter_is_live(&runtime, &run_id).unwrap());
    let path = run_socket_path(&runtime, &run_id);
    let waiter = StdUnixDatagram::bind(&path).unwrap();
    assert!(run_waiter_is_live(&runtime, &run_id).unwrap());
    drop(waiter);
    assert!(path.exists());
    assert!(!run_waiter_is_live(&runtime, &run_id).unwrap());
}

#[test]
fn write_load_and_list_runs() {
    let dir = tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
    let mut first = RunRecord::new(
        workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "first".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    let mut second = RunRecord::new(
        workspace_id,
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "second".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    first.status = RunStatus::Completed;
    second.updated_at = first.updated_at + std::time::Duration::from_secs(1);

    write(dir.path(), &first).unwrap();
    write(dir.path(), &second).unwrap();

    let loaded = load(dir.path(), &first.run_id).unwrap();
    assert_eq!(loaded.prompt, "first");
    let listed = list(dir.path()).unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].run_id, second.run_id);
}
#[test]
fn retention_and_report_fields_default_for_old_run_records() {
    let record = RunRecord::new(
        WorkspaceId::from_project_root(Path::new("/tmp/rimz-run")),
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "go".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    let mut old_json = serde_json::to_value(&record).expect("serialize run");
    old_json.as_object_mut().expect("run object").remove("keep");
    old_json
        .as_object_mut()
        .expect("run object")
        .remove("subagent");
    old_json
        .as_object_mut()
        .expect("run object")
        .remove("joined_at");
    old_json
        .as_object_mut()
        .expect("run object")
        .remove("report_message_id");
    old_json
        .as_object_mut()
        .expect("run object")
        .remove("provider_pid");
    old_json
        .as_object_mut()
        .expect("run object")
        .remove("provider_process_start");
    old_json
        .as_object_mut()
        .expect("run object")
        .remove("parked_at");

    let old: RunRecord = serde_json::from_value(old_json).expect("deserialize old run");

    assert!(!old.keep);
    assert!(!old.subagent);
    assert_eq!(old.joined_at, None);
    assert_eq!(old.report_message_id, None);
    assert_eq!(old.provider_pid, None);
    assert_eq!(old.provider_process_start, None);
    assert_eq!(old.parked_at, None);
    assert_eq!(old.timeout, None);
    assert_eq!(old.grace, None);
    assert!(old.warn.is_empty());
    assert_eq!(old.deadline_notice_at, None);
}

#[test]
fn a_detached_run_owes_no_answer_but_keeps_it_readable() {
    let mut record = RunRecord::new(
        WorkspaceId::from_project_root(Path::new("/tmp/rimz-run")),
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "go".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    record.status = RunStatus::Completed;
    assert!(record.owes_report());
    record.keep = true;
    assert!(record.owes_report());
    record.keep = false;
    record.report_to = ReportTo::Nobody;
    assert!(!record.owes_report());
    let claims = record.answer_claims().collect::<Vec<_>>();
    assert_eq!(claims.len(), 1, "the answer is still there to join");
    assert!(!claims[0].owed);
}

#[test]
fn retry_link_round_trips_and_defaults_when_absent() {
    let mut record = RunRecord::new(
        WorkspaceId::from_project_root(Path::new("/tmp/rimz-run")),
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "go".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
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
fn parked_on_round_trips_and_defaults_when_absent() {
    let mut record = RunRecord::new(
        WorkspaceId::from_project_root(Path::new("/tmp/rimz-run")),
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "go".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    assert!(
        serde_json::to_value(&record)
            .unwrap()
            .get("parked_on")
            .is_none()
    );
    for (cause, wire) in [
        (ParkCause::Wait, "wait"),
        (ParkCause::WakeInFlight, "wake_in_flight"),
        (ParkCause::Subagents, "subagents"),
        (ParkCause::Team, "team"),
        (ParkCause::SoftCompact, "soft_compact"),
        (ParkCause::Message, "message"),
    ] {
        record.parked_on = Some(cause);
        let mut value = serde_json::to_value(&record).unwrap();
        assert_eq!(value["parked_on"], wire);
        let decoded: RunRecord = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(decoded.parked_on, Some(cause));
        value.as_object_mut().unwrap().remove("parked_on");
        let decoded: RunRecord = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.parked_on, None);
    }
    record.status = RunStatus::Running;
    record.parked_at = Some(Timestamp::now());
    assert!(record.mark_terminal(RunStatus::Completed, Timestamp::now()));
    assert_eq!((record.parked_at, record.parked_on), (None, None));
}

#[test]
fn limit_hold_and_handover_round_trip_and_default_when_absent() {
    let mut record = RunRecord::new(
        WorkspaceId::from_project_root(Path::new("/tmp/rimz-run")),
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "go".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    let unset = serde_json::to_value(&record).unwrap();
    for key in [
        "limit_hold",
        "handed_over_from",
        "interrupted_at",
        "turn_opened_at",
    ] {
        assert!(unset.get(key).is_none(), "an unset {key} is omitted");
    }
    let pane = PaneId::parse("tmux:%7").unwrap();
    record.handed_over_from = Some(pane.clone());
    let at: Timestamp = "2026-01-01T01:00:00Z".parse().unwrap();
    record.interrupted_at = Some(at + Duration::from_secs(5));
    record.turn_opened_at = Some(at + Duration::from_secs(2));
    let hold = LimitHold {
        activity: at,
        since: at + Duration::from_secs(1),
        until: at + Duration::from_secs(3600),
    };
    record.limit_hold = Some(hold);

    let mut value = serde_json::to_value(&record).unwrap();
    let decoded: RunRecord = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(decoded.limit_hold, Some(hold));
    assert_eq!(decoded.handed_over_from, Some(pane));
    assert_eq!(decoded.interrupted_at, record.interrupted_at);
    assert_eq!(decoded.turn_opened_at, record.turn_opened_at);

    for key in [
        "limit_hold",
        "handed_over_from",
        "interrupted_at",
        "turn_opened_at",
    ] {
        value.as_object_mut().unwrap().remove(key);
    }
    let decoded: RunRecord = serde_json::from_value(value).unwrap();
    assert_eq!(decoded.limit_hold, None);
    assert_eq!(decoded.handed_over_from, None);
    assert_eq!(decoded.interrupted_at, None);
    assert_eq!(decoded.turn_opened_at, None);
}

#[test]
fn limit_failure_round_trips_on_both_records_and_defaults_when_absent() {
    let mut record = RunRecord::new(
        WorkspaceId::from_project_root(Path::new("/tmp/rimz-run")),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "go".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    assert!(
        serde_json::to_value(&record)
            .unwrap()
            .get("limit_failure")
            .is_none()
    );
    let failure = LimitFailure {
        cause: LimitFailureCause::RefillKnown,
        refill_at: Some("2026-11-11T20:00:00Z".parse().unwrap()),
        label: Some("Usage limit reached".to_owned()),
    };
    record.limit_failure = Some(failure.clone());
    record.earlier_answers.push(EarlierAnswer {
        ordinal: 1,
        status: RunStatus::Failed,
        started_at: record.started_at,
        completed_at: None,
        prompt: None,
        failure_tail: None,
        limit_failure: Some(LimitFailure {
            cause: LimitFailureCause::ContinueFailed,
            refill_at: None,
            label: None,
        }),
        opened_by: Vec::new(),
        report_message_id: None,
        joined_at: None,
    });

    let mut value = serde_json::to_value(&record).unwrap();
    assert_eq!(
        value["limit_failure"],
        serde_json::json!({
            "cause": "refill_known",
            "refill_at": "2026-11-11T20:00:00Z",
            "label": "Usage limit reached",
        })
    );
    assert_eq!(
        value["earlier_answers"][0]["limit_failure"],
        serde_json::json!({ "cause": "continue_failed" })
    );
    let decoded: RunRecord = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(decoded, record);

    value.as_object_mut().unwrap().remove("limit_failure");
    value["earlier_answers"][0]
        .as_object_mut()
        .unwrap()
        .remove("limit_failure");
    let decoded: RunRecord = serde_json::from_value(value).unwrap();
    assert_eq!(decoded.limit_failure, None);
    assert_eq!(decoded.earlier_answers[0].limit_failure, None);
}

#[test]
fn stopped_agent_match_bounds_names_but_not_session_ids() {
    let mut record = RunRecord::new(
        WorkspaceId::from_project_root(Path::new("/repo")),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "task".into(),
        "/repo".into(),
    );
    record.agent_id = Some("old".into());
    record.agent_name = Some("otter".to_owned());
    for (completed, registered, id, name, kind, expected) in [
        (Some(2000), None, "new", "otter", "codex", false),
        (Some(2000), Some(3000), "new", "otter", "codex", false),
        (Some(2000), Some(1000), "new", "otter", "codex", true),
        (Some(2000), Some(2000), "new", "otter", "codex", true),
        (None, None, "new", "otter", "codex", true),
        (None, Some(3000), "new", "otter", "codex", true),
        (Some(2000), None, "old", "renamed", "codex", true),
        (Some(2000), Some(3000), "old", "renamed", "codex", true),
        (Some(2000), Some(1000), "new", "other", "codex", false),
        (Some(2000), Some(1000), "old", "otter", "claude", false),
    ] {
        record.completed_at = completed.map(|seconds| Timestamp::from_second(seconds).unwrap());
        let mut agent = crate::agents::AgentState::stub(kind, id, crate::agents::AgentStatus::Idle);
        agent.name = Some(name.to_owned());
        agent.registered_at = registered.map(|seconds| Timestamp::from_second(seconds).unwrap());
        assert_eq!(
            record.matches_stopped_agent(&agent),
            expected,
            "completed={completed:?}, registered={registered:?}, id={id}, name={name}, kind={kind}"
        );
    }
}
