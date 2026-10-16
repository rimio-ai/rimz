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
    assert_eq!(list(&state).unwrap(), vec![record.clone()]);
    let mut next = record.clone();
    next.next_seq = 3;
    write(&paths, &next, &guard).unwrap();
    assert_eq!(load(&paths).unwrap().next_seq, 3);
    let value = serde_json::to_value(record).unwrap();
    insta::assert_json_snapshot!("record", value);
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
