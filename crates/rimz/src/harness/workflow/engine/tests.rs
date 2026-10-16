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
            &super::super::tests::FakeHost::new(),
            &WindowReadings::fixed(Timestamp::now(), BTreeMap::new()),
            &mut None,
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
    let result = std::panic::catch_unwind(|| {
        advance(&state, &record.id, &super::super::tests::FakeHost::new())
    });
    assert!(
        matches!(result, Ok(Err(_))),
        "an invalid sealed timeout must return an error, not panic"
    );
}

#[test]
fn provider_windows_close_and_reopen_on_fresh_capacity() {
    use crate::agents::{ProviderCapacity, RateLimitWindow};
    use crate::harness::schedule::when::WindowReadings;
    use crate::ids::LoginKey;
    let now = "2026-01-01T00:00:00Z".parse::<Timestamp>().unwrap();
    let kind = crate::ids::AgentKind::new_unchecked("claude");
    let (_home, state, mut record, spec) = fixture(
        "[workflow]\nscope='worktree'\nstart='pr'\n[steps.pr]\nagent='claude'\nprompt-file='prompt.md'\nthen='after'\nwindow='5h.left >= 20'\n[checks.after]\nrun='true'\non.ok='@done'\n",
    );
    fs::write(record.spec_dir.join("prompt.md"), "Build").unwrap();
    record.key = Some(workflow::Key::Integer(7));
    record.input = Some(json!({"outcome":"work", "key":7}));
    {
        let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
        workflow::write(&state.workflow(&record.id), &record, &guard).unwrap();
    }
    let host = super::super::tests::FakeHost::new();
    for (used, open) in [(100, false), (10, true)] {
        let windows = WindowReadings::fixed(
            now,
            BTreeMap::from([(
                LoginKey::default_for(kind.clone()),
                ProviderCapacity::from_windows(vec![RateLimitWindow {
                    used_percentage: Some(used),
                    resets_at: Some(now + jiff::SignedDuration::from_secs(3600)),
                    duration_mins: Some(300),
                    ..Default::default()
                }]),
            )]),
        );
        assert_eq!(
            window_matches(
                "window.5h.left >= 20",
                Path::new("/project"),
                &kind,
                None,
                &windows
            )
            .unwrap(),
            open
        );
        for _ in 0..if open { 1 } else { 2 } {
            let decision = prepare(
                &state,
                &state.workflow(&record.id),
                &spec,
                &mut BTreeSet::new(),
                &host,
                &windows,
                &mut None,
            )
            .unwrap();
            assert_eq!(matches!(decision, Decision::Launch(_)), open);
        }
        let current = workflow::load(&state.workflow(&record.id)).unwrap();
        assert_eq!(current.key, record.key);
        assert_eq!(current.input, record.input);
        if !open {
            assert!(current.visits.is_empty());
            assert_eq!(current.wait.as_ref().unwrap().outcome, "window");
            assert_eq!(current.wait.as_ref().unwrap().count, 2);
        } else {
            assert_eq!(current.visits["pr"].total, 1);
            assert!(current.wait.is_none());
            assert!(workflow::journal(&state.workflow(&record.id)).unwrap().iter().any(|row| matches!(&row.event, JournalEvent::Wait { outcome, count:2, .. } if outcome == "window")));
        }
    }
    let (home, state, mut record, spec) = fixture(
        "[workflow]\nscope='worktree'\nstart='pr'\n[checks.pr]\nagent='claude'\nprompt-file='prompt.md'\non.ok='@done'\n",
    );
    fs::write(home.path().join("prompt.md"), "Judge").unwrap();
    record.check_retry = 1;
    record.check_retry_context = Some(json!({"outcome":"died", "reason":"no object"}));
    {
        let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
        workflow::write(&state.workflow(&record.id), &record, &guard).unwrap();
    }
    prepare(
        &state,
        &state.workflow(&record.id),
        &spec,
        &mut BTreeSet::new(),
        &host,
        &WindowReadings::fixed(now, BTreeMap::new()),
        &mut None,
    )
    .unwrap();
    let paths = state.workflow(&record.id);
    let ctx: serde_json::Value =
        serde_json::from_slice(&fs::read(&paths.context).unwrap()).unwrap();
    assert_eq!(ctx["output_path"], json!(paths.check_output(1)));
    assert_eq!(ctx["retry"], record.check_retry_context.clone().unwrap());
    let mut current = workflow::load(&paths).unwrap();
    current.attempt.as_mut().unwrap().host = Host {
        pid: u32::MAX,
        start: None,
    };
    {
        let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
        workflow::write(&paths, &current, &guard).unwrap();
    }
    let windows = WindowReadings::fixed(now, BTreeMap::new());
    assert!(matches!(
        prepare(
            &state,
            &paths,
            &spec,
            &mut BTreeSet::new(),
            &host,
            &windows,
            &mut None
        )
        .unwrap(),
        Decision::Again
    ));
    let current = workflow::load(&paths).unwrap();
    assert!(current.attempt.is_none());
    assert_eq!(current.check_retry, 1, "dead hosting is a free retry");
    assert_eq!(current.check_retry_context, record.check_retry_context);
    assert!(matches!(
        prepare(
            &state,
            &paths,
            &spec,
            &mut BTreeSet::new(),
            &host,
            &windows,
            &mut None
        )
        .unwrap(),
        Decision::Evaluate(_)
    ));
    let ctx: serde_json::Value =
        serde_json::from_slice(&fs::read(&paths.context).unwrap()).unwrap();
    assert_eq!(ctx["seq"], 2);
    assert_eq!(ctx["retry"], record.check_retry_context.unwrap());
}
