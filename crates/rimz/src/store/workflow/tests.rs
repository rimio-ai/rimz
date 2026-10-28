use super::*;
use serde_json::json;
use std::fs;

fn record() -> RunRecord {
    serde_json::from_value(json!({
        "version":1, "id":"wf_0190b8172c0070008000000000000001", "workflow":"review",
        "spec_dir":"/definitions/review", "spec_digest":"abc", "prompt":"Review this PR",
        "scope":{"kind":"worktree","path":"/project"}, "project_root":"/project",
        "created_at":"2026-01-01T00:00:00Z", "state":"active", "park":null, "node":"pr",
        "entered_at":"2026-01-01T00:00:00Z", "input":null, "key":null, "recheck_at":null,
        "wait":null, "visits":{"build":{"total":2,"keys":[{"key":42,"visits":1},{"key":"42","visits":1}]}},
        "visit_grant":0, "deadline":null, "deadline_fired":false, "next_seq":2,
        "attempt":{"kind":"script","seq":1,"node":"pr","started_at":"2026-01-01T00:00:00Z","host":{"pid":123,"start":"token"}},
        "prev_attempt":null, "terminal_at":null
    })).unwrap()
}

#[test]
fn until_session_round_trips_with_its_verdict_and_version_fence() {
    let turn = json!({
        "agent_id":"leader", "started_at":"2026-01-01T00:00:00Z",
        "ended_at":"2026-01-01T00:00:01Z"
    });
    let verdict = json!({
        "node":"pr", "entered_at":"2026-01-01T00:00:01Z", "target":"@done",
        "object":{"outcome":"merged"}, "reason":"user"
    });
    let mut value = serde_json::to_value(record()).unwrap();
    value["version"] = json!(2);
    value["held"] = json!({
        "attempt":value["attempt"], "entered_at":"2026-01-01T00:00:00Z",
        "verdict":verdict
    });
    value["held"]["attempt"]["judged"] = turn.clone();
    value["attempt"]["judged"] = turn;
    value["attempt"]["ending"] = json!({
        "outcome":"ended", "cause":"lifecycle", "route":verdict
    });
    let decoded: RunRecord = serde_json::from_value(value.clone()).unwrap();
    let encoded = serde_json::to_value(&decoded).unwrap();
    assert_eq!(
        (&encoded["held"], &encoded["attempt"]),
        (&value["held"], &value["attempt"])
    );
    let home = tempfile::tempdir().unwrap();
    let state =
        StatePaths::for_project_root_under(std::path::Path::new("/project"), home.path()).unwrap();
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    initialize(&state, &decoded, b"[workflow]", &guard).unwrap();
    let loaded = load(&state.workflow(&decoded.id));
    assert!(loaded.is_ok(), "version 2 must load: {loaded:?}");
    assert_eq!(loaded.unwrap(), decoded);
}

#[test]
fn until_turn_end_journal_preserves_each_decision() {
    let results: Vec<_> = ["kept", "closed", "discarded", "interrupted"]
        .into_iter()
        .map(|action| {
            let row = json!({
                "event":"turn_end", "seq":1, "at":"2026-01-01T00:00:01Z", "node":"watch",
                "action":action, "check":"pr", "outcome":"merged", "target":"@done",
                "reason":"leader moved on", "turn":{
                    "agent_id":"leader", "started_at":"2026-01-01T00:00:00Z",
                    "ended_at":"2026-01-01T00:00:01Z"
                }
            });
            let decoded = serde_json::from_value::<JournalEntry>(row.clone());
            (
                action,
                decoded
                    .ok()
                    .map(|entry| serde_json::to_value(entry).unwrap())
                    == Some(row),
            )
        })
        .collect();
    assert_eq!(
        results,
        vec![
            ("kept", true),
            ("closed", true),
            ("discarded", true),
            ("interrupted", true)
        ]
    );
}

#[test]
fn legacy_records_omit_until_fields() {
    let value = serde_json::to_value(record()).unwrap();
    assert_eq!(value["version"], 1);
    assert!(value.get("held").is_none());
    assert!(value["attempt"].get("judged").is_none());
    assert_eq!(
        serde_json::from_value::<RunRecord>(value).unwrap(),
        record()
    );
}

#[test]
fn provenance_and_window_evidence_survive_record_and_journal_round_trips() {
    let legacy = serde_json::to_value(record()).unwrap();
    let mut value = legacy.clone();
    value["task"] = json!("babysit-pr");
    value["wait"] = json!({
        "node":"pr", "outcome":"window", "detail":"5h.left=12<42",
        "first":"2026-01-01T00:00:00Z", "last":"2026-01-01T00:00:00Z",
        "count":2, "recheck_at":null
    });
    let decoded: RunRecord = serde_json::from_value(value.clone()).unwrap();
    let encoded = serde_json::to_value(decoded).unwrap();
    assert_eq!(encoded["task"], value["task"]);
    assert_eq!(encoded["wait"]["detail"], value["wait"]["detail"]);
    let event = json!({
        "event":"wait", "seq":null, "at":"2026-01-01T00:00:00Z", "node":"pr",
        "outcome":"window", "detail":"5h.left=12<42", "count":2,
        "first":"2026-01-01T00:00:00Z", "last":"2026-01-01T00:00:00Z", "recheck_at":null
    });
    let decoded: JournalEntry = serde_json::from_value(event.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(decoded).unwrap()["detail"],
        event["detail"]
    );
    let decoded: RunRecord = serde_json::from_value(legacy).unwrap();
    assert!(serde_json::to_value(decoded).unwrap()["task"].is_null());
}

#[test]
fn durable_record_round_trip_and_json_shape() {
    let home = tempfile::tempdir().unwrap();
    let state =
        StatePaths::for_project_root_under(std::path::Path::new("/project"), home.path()).unwrap();
    let mut value = serde_json::to_value(record()).unwrap();
    value["prev_attempt"] = json!({
        "step":"merge", "outcome":"died", "seq":1, "visit":1, "key":null,
        "started_at":"2026-01-01T00:00:00Z", "ended_at":"2026-01-01T00:00:01Z",
        "handle":null, "run_id":null, "claim_path":"/work/1.claim.json",
        "head_start":"abc", "head_end":"def", "children":null,
        "code":7, "log":"/work/1.step.log"
    });
    for field in ["limited", "asked", "cost_usd"] {
        value["prev_attempt"][field] = Value::Null;
    }
    let record: RunRecord = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(&record).unwrap()["prev_attempt"],
        value["prev_attempt"]
    );
    let mut legacy = value["prev_attempt"].clone();
    legacy.as_object_mut().unwrap().remove("code");
    legacy.as_object_mut().unwrap().remove("log");
    let legacy: PreviousAttempt = serde_json::from_value(legacy).unwrap();
    let legacy = serde_json::to_value(legacy).unwrap();
    assert_eq!(
        (legacy.get("code"), legacy.get("log")),
        (Some(&Value::Null), Some(&Value::Null))
    );
    let event = json!({
        "event":"attempt", "seq":1, "at":"2026-01-01T00:00:01Z", "node":"merge",
        "outcome":"died", "visit":1, "key":null, "digests":{},
        "started_at":"2026-01-01T00:00:00Z", "ended_at":"2026-01-01T00:00:01Z",
        "code":7, "log":"/work/1.step.log"
    });
    let decoded: JournalEntry = serde_json::from_value(event.clone()).unwrap();
    let encoded = serde_json::to_value(decoded).unwrap();
    assert_eq!(
        (&encoded["code"], &encoded["log"]),
        (&event["code"], &event["log"])
    );
    let mut legacy = event;
    legacy.as_object_mut().unwrap().remove("code");
    legacy.as_object_mut().unwrap().remove("log");
    let decoded: JournalEntry = serde_json::from_value(legacy).unwrap();
    let encoded = serde_json::to_value(decoded).unwrap();
    assert_eq!(
        (encoded.get("code"), encoded.get("log")),
        (Some(&Value::Null), Some(&Value::Null))
    );
    let paths = state.workflow(&record.id);
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    initialize(&state, &record, b"[workflow]", &guard).unwrap();
    assert!(
        paths.record.is_file(),
        "initialization must publish the record"
    );
    assert_eq!(load(&paths).unwrap(), record);
    assert_eq!(fs::read(&paths.spec).unwrap(), b"[workflow]");
    assert!(paths.work.is_dir());
    let children: Vec<_> = fs::read_dir(&paths.work)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(
        children.contains(&"notes".into()),
        "nodes need notes/ at creation"
    );
    assert!(
        children.contains(&"signs".into()),
        "judges need signs/ at creation"
    );
    assert_eq!(list(&state).unwrap(), vec![record.clone()]);
    let mut next = record.clone();
    next.next_seq = 3;
    write(&paths, &next, &guard).unwrap();
    assert_eq!(load(&paths).unwrap().next_seq, 3);
    let value = serde_json::to_value(record).unwrap();
    insta::assert_json_snapshot!("record", value);
}

#[test]
fn legacy_evaluations_default_to_check_origin() {
    let row = json!({
        "event":"evaluation", "seq":1, "at":"2026-01-01T00:00:00Z", "node":"pr",
        "outcome":"ok", "object":{"outcome":"ok"}, "digests":{},
        "started_at":"2026-01-01T00:00:00Z", "ended_at":"2026-01-01T00:00:00Z"
    });
    let entry: JournalEntry = serde_json::from_value(row).unwrap();
    assert_eq!(serde_json::to_value(entry).unwrap()["origin"], "check");
}

#[test]
fn listing_skips_unreadable_records_but_named_loads_fail() {
    let home = tempfile::tempdir().unwrap();
    let state =
        StatePaths::for_project_root_under(std::path::Path::new("/project"), home.path()).unwrap();
    let record = record();
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    initialize(&state, &record, b"[workflow]", &guard).unwrap();
    let mut future = record.clone();
    future.version = 3;
    for contents in [
        None,
        Some(b"corrupt".to_vec()),
        Some(serde_json::to_vec(&future).unwrap()),
    ] {
        let paths = state.workflow(&WorkflowRunId::new());
        fs::create_dir_all(&paths.home).unwrap();
        if let Some(contents) = contents {
            fs::write(&paths.record, contents).unwrap();
        }
        assert!(load(&paths).is_err(), "named loads must still fail");
    }
    assert_eq!(
        list(&state).ok(),
        Some(vec![record]),
        "unreadable records must not hide healthy runs"
    );
}

#[test]
fn journal_preserves_entries_and_tolerates_torn_or_unknown_rows() {
    let home = tempfile::tempdir().unwrap();
    let state =
        StatePaths::for_project_root_under(std::path::Path::new("/project"), home.path()).unwrap();
    let record = record();
    let paths = state.workflow(&record.id);
    fs::create_dir_all(&paths.home).unwrap();
    let entry = JournalEntry {
        seq: Some(1),
        at: record.created_at,
        node: "pr".into(),
        event: JournalEvent::Route {
            outcome: "timeout".into(),
            input: json!({"outcome":"timeout"}),
        },
    };
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    append(&paths, &entry, &guard).unwrap();
    assert_eq!(journal(&paths).unwrap(), vec![entry.clone()]);
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&paths.journal)
        .unwrap();
    writeln!(file, "{{\"event\":\"future\"}}").unwrap();
    writeln!(file, "corrupt row").unwrap();
    let mut future_field = serde_json::to_value(&entry).unwrap();
    future_field["future"] = json!(true);
    writeln!(file, "{future_field}").unwrap();
    write!(file, "{{\"event\":").unwrap();
    assert_eq!(journal(&paths).unwrap(), vec![entry.clone(), entry]);
}
