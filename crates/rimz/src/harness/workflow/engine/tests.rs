use super::*;
use std::fs;

fn fixture(text: &str) -> (tempfile::TempDir, StatePaths, RunRecord, WorkflowSpec) {
    let home = tempfile::tempdir().unwrap();
    let state = StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
    let record: RunRecord = serde_json::from_value(json!({
        "version":1, "id":WorkflowRunId::new(), "workflow":"review",
        "spec_dir":home.path(), "spec_digest":"abc", "prompt":null,
        "scope":{"kind":"worktree","path":home.path()}, "project_root":home.path(),
        "created_at":"2000-01-01T00:00:00Z", "state":"active", "park":null, "node":"pr",
        "entered_at":"2000-01-01T00:00:00Z", "input":null, "key":null, "recheck_at":null,
        "wait":null, "visits":{}, "visit_grant":0, "deadline":null,
        "deadline_fired":false, "next_seq":1, "attempt":null, "prev_attempt":null,
        "terminal_at":null
    }))
    .unwrap();
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    workflow::initialize(&state, &record, text.as_bytes(), &guard).unwrap();
    let spec = toml::from_str(text).unwrap();
    (home, state, record, spec)
}

#[test]
fn bound_routes_stop_when_waiting_or_inactive() {
    for target in ["@wait", "@user", "@done", "@failed"] {
        let text = format!(
            "[workflow]\nscope='worktree'\nstart='pr'\n[checks.pr]\nrun='true'\ntimeout='1s'\non.timeout='{target}'\n"
        );
        let (_home, state, record, spec) = fixture(&text);
        let decision = prepare(
            &state,
            &state.workflow(&record.id),
            &spec,
            &mut BTreeSet::new(),
        )
        .unwrap();
        assert!(
            matches!(decision, Decision::Stop),
            "a bound routed to {target} must stop"
        );
        assert_eq!(
            workflow::journal(&state.workflow(&record.id))
                .unwrap()
                .iter()
                .filter(|entry| matches!(entry.event, JournalEvent::Route { .. }))
                .count(),
            1
        );
    }
}

#[test]
fn create_validates_the_spec_it_reads_before_publication() {
    let text = "[workflow]\nscope='worktree'\nstart='pr'\n[checks.pr]\nrun='true'\ntimeout='tomorrow'\non.ok='@done'\n";
    let (home, state, record, _) = fixture(text);
    fs::write(crate::disk::paths::workflow_spec_file(home.path()), text).unwrap();
    let error = create(&state, home.path(), home.path(), home.path(), None).unwrap_err();
    assert!(
        error.to_string().contains("checks.pr.timeout"),
        "the bytes to seal must be validated first: {error}"
    );
    assert_eq!(workflow::list(&state).unwrap().len(), 1);
    assert!(state.workflow(&record.id).home.exists());
}

#[test]
fn invalid_legacy_timeout_returns_an_error_without_panicking() {
    let text = "[workflow]\nscope='worktree'\nstart='pr'\n[checks.pr]\nrun='true'\ntimeout='tomorrow'\non.ok='@done'\n";
    let (_home, state, record, _) = fixture(text);
    let result = std::panic::catch_unwind(|| advance(&state, &record.id));
    assert!(
        matches!(result, Ok(Err(_))),
        "an invalid sealed timeout must return an error, not panic"
    );
}
