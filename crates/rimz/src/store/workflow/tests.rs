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
    let record = record();
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
    future.version = 2;
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
