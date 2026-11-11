use super::super::ScopeKind;
use super::*;
use std::fs;

#[test]
fn spawn_arguments_name_only_the_current_hook_frame() {
    use crate::harness::hook_drain::{FrameAttempt, with_frame_env};
    use crate::ids::{AgentKind, EventId};
    use crate::store::ingress::HookIngress;

    let (_home, _state, record, _spec) = fixture(
        "[workflow]\nscope='worktree'\nstart='pr'\n[checks.pr]\nrun='true'\non.ok='@done'\n",
    );
    let ordinary: Vec<std::ffi::OsString> = vec![
        "--root".into(),
        record.project_root.as_os_str().into(),
        "workflow".into(),
        "advance".into(),
        record.id.as_str().into(),
    ];
    assert_eq!(spawn_args(&record), ordinary);
    let frame = HookIngress {
        boot_id: None,
        schema_version: "1".into(),
        event_id: EventId::new(),
        ts: Timestamp::UNIX_EPOCH,
        source: AgentKind::new_unchecked("codex"),
        event: Some("Stop".into()),
        payload: "{}".into(),
        cwd: record.project_root.clone(),
        hook_pid: std::process::id(),
        env: BTreeMap::new(),
    };
    let mut woken = ordinary.clone();
    woken.extend(["--woken-by".into(), frame.event_id.to_string().into()]);
    for attempt in [
        FrameAttempt::First,
        FrameAttempt::Redo,
        FrameAttempt::Replay,
    ] {
        with_frame_env(&frame, attempt, || {
            assert_eq!(spawn_args(&record), woken);
        });
        assert_eq!(spawn_args(&record), ordinary);
    }
}

#[test]
fn accounting_uses_closed_waits_and_evaluation_origin() {
    use crate::harness::assist_log;
    if std::env::var_os("RIMZ_TEST_WORKFLOW_ACCOUNTING").is_none() {
        let home = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "harness::workflow::engine::tests::accounting_uses_closed_waits_and_evaluation_origin", "--nocapture"])
            .env("RIMZ_TEST_WORKFLOW_ACCOUNTING", "1")
            .env("RIMZ_UNIT_TEST_RIMZ_HOME", home.path())
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let (_home, state, record, spec) = fixture(
        "[workflow]\nscope='worktree'\nstart='pr'\n[checks.pr]\nrun='true'\non.pending='@wait'\n",
    );
    let paths = state.workflow(&record.id);
    let mut t = Transition {
        record,
        spec: &spec,
        paths: &paths,
        now: Timestamp::UNIX_EPOCH,
        entries: Vec::new(),
    };
    let evaluation = json!({"event":"evaluation", "seq":1, "node":"pr", "at":t.now, "outcome":"pending", "object":{}, "digests":{}, "started_at":t.now, "ended_at":t.now, "origin":"check"});
    t.entries
        .push(serde_json::from_value(evaluation.clone()).unwrap());
    t.record.wait = Some(
        serde_json::from_value(
            json!({"node":"pr", "outcome":"pending", "first":t.now, "last":t.now, "count":3}),
        )
        .unwrap(),
    );
    accounting::transition(&t);
    assert!(
        assist_log::recent(&crate::disk::paths::logs_dir(), None).is_empty(),
        "an open check wait is not an assist"
    );
    t.entries.clear();
    t.close_wait();
    accounting::transition(&t);
    let records = assist_log::recent(&crate::disk::paths::logs_dir(), None);
    assert_eq!(records.len(), 1, "{records:?}");
    let value = serde_json::to_value(&records[0]).unwrap();
    assert_eq!(value["to"], "@wait");
    assert_eq!(value["count"], 3);
    let mut canceled = evaluation.clone();
    canceled["origin"] = json!("engine");
    canceled["outcome"] = json!("canceled");
    t.entries = vec![serde_json::from_value(canceled).unwrap()];
    accounting::transition(&t);
    let mut invalid = evaluation;
    invalid["origin"] = json!("engine");
    invalid["outcome"] = json!("invalid");
    t.entries = vec![
        serde_json::from_value(invalid).unwrap(),
        serde_json::from_value(json!({"event":"route", "seq":1, "node":"pr", "at":t.now, "outcome":"invalid", "input":null})).unwrap(),
    ];
    t.record.wait = Some(
        serde_json::from_value(
            json!({"node":"pr", "outcome":"invalid", "first":t.now, "last":t.now, "count":1}),
        )
        .unwrap(),
    );
    accounting::transition(&t);
    assert_eq!(
        assist_log::recent(&crate::disk::paths::logs_dir(), None).len(),
        1
    );
    t.record.wait = None;
    let workspace = crate::workspace::WorkspaceResolver::resolve(
        &t.record.scope.path,
        Some(t.record.project_root.clone()),
    )
    .unwrap();
    let diagnostics_before = crate::diag::recent_records(workspace.workspace_id.clone())
        .unwrap()
        .1
        .len();
    assert_eq!(diagnostics_before, 1);
    let mut actual = Vec::new();
    for (action, target) in [
        ("kept", "work"),
        ("closed", "@done"),
        ("kept", "@wait"),
        ("discarded", "@done"),
        ("interrupted", "@done"),
    ] {
        t.entries = vec![
            serde_json::from_value(json!({
                "event":"evaluation", "seq":2, "node":"pr", "at":t.now,
                "outcome":"canceled", "origin":"engine", "object":null, "digests":{},
                "started_at":t.now, "ended_at":t.now
            }))
            .unwrap(),
            serde_json::from_value(json!({
                "event":"route", "seq":2, "node":"pr", "at":t.now,
                "outcome":"done", "input":null
            }))
            .unwrap(),
            serde_json::from_value(json!({
                "event":"attempt", "seq":1, "node":"work", "at":t.now,
                "outcome":"ended", "visit":1, "key":null, "digests":{},
                "started_at":t.now, "ended_at":t.now
            }))
            .unwrap(),
            serde_json::from_value(json!({
                "event":"turn_end", "seq":1, "node":"work", "at":t.now, "action":action,
                "turn":{"agent_id":"leader", "started_at":t.now, "ended_at":t.now},
                "check":"pr", "outcome":"done", "target":target
            }))
            .unwrap(),
        ];
        if action == "interrupted" {
            t.entries.retain(|entry| {
                !matches!(
                    entry.event,
                    JournalEvent::Route { .. } | JournalEvent::Attempt { .. }
                )
            });
        }
        accounting::transition(&t);
        let routes: Vec<_> = assist_log::recent(&crate::disk::paths::logs_dir(), None)
            .into_iter()
            .map(|record| serde_json::to_value(record).unwrap())
            .filter(|record| record["assist"] == "workflow_route")
            .collect();
        actual.push((
            action,
            target,
            routes.len(),
            routes
                .iter()
                .filter(|route| route["count"].is_null())
                .all(|route| {
                    route["node"] == "pr" && (route["to"] == "work" || route["to"] == "@done")
                }),
        ));
    }
    assert_eq!(
        crate::diag::recent_records(workspace.workspace_id)
            .unwrap()
            .1
            .len(),
        diagnostics_before,
        "an interrupted held check must not report a rerun"
    );
    assert_eq!(
        actual,
        vec![
            ("kept", "work", 2, true),
            ("closed", "@done", 3, true),
            ("kept", "@wait", 3, true),
            ("discarded", "@done", 3, true),
            ("interrupted", "@done", 3, true)
        ]
    );
    t.record.held = Some(
        serde_json::from_value(json!({
            "attempt":{
                "kind":"agent", "lifecycle":"until", "seq":1, "node":"work",
                "started_at":t.now, "host":{"pid":1, "start":null},
                "run":{"run_id":crate::ids::RunId::new(), "handle":"@leader"},
                "judged":{"agent_id":"leader", "started_at":t.now, "ended_at":t.now}
            },
            "entered_at":t.now
        }))
        .unwrap(),
    );
    let mut actual = Vec::new();
    for (origin, route) in [("check", false), ("engine", false), ("engine", true)] {
        t.entries = vec![
            serde_json::from_value(json!({
                "event":"evaluation", "seq":2, "node":"pr", "at":t.now,
                "outcome":"invalid", "origin":origin, "object":null, "digests":{},
                "started_at":t.now, "ended_at":t.now
            }))
            .unwrap(),
        ];
        if route {
            t.push(
                None,
                JournalEvent::Route {
                    outcome: "invalid".into(),
                    input: serde_json::Value::Null,
                },
            );
        }
        accounting::transition(&t);
        actual.push((
            origin,
            route,
            assist_log::recent(&crate::disk::paths::logs_dir(), None).len(),
        ));
    }
    assert_eq!(
        actual,
        vec![
            ("check", false, 3),
            ("engine", false, 3),
            ("engine", true, 3)
        ]
    );
}

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
fn detaching_a_task_preserves_foreign_and_finished_runs() {
    for (run_state, live) in [
        (RunState::Active, true),
        (RunState::Parked, true),
        (RunState::Done, false),
        (RunState::Failed, false),
        (RunState::Canceled, false),
    ] {
        for task in ["review", "other"] {
            let (_home, state, mut record, _) = fixture(
                "[workflow]\nscope='worktree'\nstart='pr'\n[checks.pr]\nrun='true'\non.pending='@wait'\n",
            );
            record.task = Some(task.into());
            record.state = run_state;
            let paths = state.workflow(&record.id);
            {
                let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
                workflow::write(&paths, &record, &guard).unwrap();
            }
            let expected = if live && task == "review" {
                record.task = None;
                vec![record.id.clone()]
            } else {
                Vec::new()
            };
            assert_eq!(reassign_task(&state, "review", None).unwrap(), expected);
            assert_eq!(workflow::load(&paths).unwrap(), record);
        }
    }
}

#[test]
fn a_task_cancels_only_its_live_runs_that_have_not_reached_an_agent() {
    let mut selected = Vec::new();
    for (task, run_state, visited) in [
        ("review", RunState::Active, &[][..]),
        ("review", RunState::Active, &["pick"][..]),
        ("review", RunState::Parked, &["pick"][..]),
        ("review", RunState::Active, &["pick", "fix"][..]),
        ("review", RunState::Done, &["pick"][..]),
        ("other", RunState::Active, &["pick"][..]),
    ] {
        let (_home, state, mut record, _) = fixture(
            "[workflow]\nscope='worktree'\nstart='pick'\n[checks.pick]\nrun='true'\non.go='fix'\n[steps.fix]\nagent='claude'\nprompt-file='prompt.md'\nthen='@done'\n",
        );
        record.task = Some(task.into());
        record.state = run_state;
        for step in visited {
            record.visits.insert(
                (*step).into(),
                serde_json::from_value(json!({"total": 1, "outcomes": []})).unwrap(),
            );
        }
        {
            let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
            workflow::write(&state.workflow(&record.id), &record, &guard).unwrap();
        }
        selected.push(unacted_task_runs(&state, "review").unwrap() == vec![record.id]);
    }
    assert_eq!(selected, [true, true, true, false, false, false]);
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
            &mut Admission::default(),
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
    let text = "[workflow]\nscope='worktree'\nstart='pr'\n[checks.pr]\nrun='true'\ntimeout='tomorrow'\non.ok='missing'\n";
    let (home, state, record, _) = fixture(text);
    let dir = home.path().join("example");
    fs::create_dir(&dir).unwrap();
    fs::write(crate::disk::paths::workflow_spec_file(&dir), text).unwrap();
    let error = create(
        &state,
        DefinitionSource::Deployed {
            dir: &dir,
            task: None,
            surplus: None,
            surplus_after: None,
        },
        ScopeRoots {
            checkout: home.path().into(),
            project: home.path().into(),
        },
        || Ok(None),
        false,
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("checks.pr.on.ok"),
        "the bytes to seal must be validated first: {error}"
    );
    assert_eq!(workflow::list(&state).unwrap().len(), 1);
    assert!(state.workflow(&record.id).home.exists());
}

#[test]
fn window_wait_evidence_preserves_strict_and_inclusive_boundaries() {
    for (comparison, reading, expected) in [
        (">=42", Some("12"), "window.5h.left=12<42"),
        (">42", Some("42"), "window.5h.left=42<=42"),
        ("<=42", Some("80"), "window.5h.left=80>42"),
        ("<42", Some("42"), "window.5h.left=42>=42"),
        (">=42", None, "window.5h.left=unknown>=42"),
    ] {
        let expr = spec::parse_window(&format!("5h.left{comparison}")).unwrap();
        let verdict = crate::harness::when::Verdict {
            ok: false,
            readings: BTreeMap::from([("window.5h.left".into(), reading.map(str::to_owned))]),
            unread: Default::default(),
        };
        assert_eq!(expr.blocking_evidence(&verdict), expected);
    }
    for (raw, expected) in [
        ("5h.left>=10 && 7d.left>=42", "window.7d.left=12<42"),
        ("!(5h.left<42)", "!window.5h.left=12<42"),
        ("!(5h.left>=42 || !(7d.left>=42))", "window.7d.left=12<42"),
    ] {
        let expr = spec::parse_window(raw).unwrap();
        let verdict = crate::harness::when::Verdict {
            ok: false,
            readings: BTreeMap::from([
                ("window.5h.left".into(), Some("12".into())),
                ("window.7d.left".into(), Some("12".into())),
            ]),
            unread: Default::default(),
        };
        assert_eq!(expr.blocking_evidence(&verdict), expected);
    }
}

#[test]
fn active_and_parked_duplicates_return_the_existing_run_as_a_typed_outcome() {
    let text = "[workflow]\nscope='worktree'\nstart='pr'\n[checks.pr]\nrun='true'\non.ok='@done'\n";
    let (home, state, mut record, _) = fixture(text);
    let dir = home.path().join("review");
    fs::create_dir(&dir).unwrap();
    record.spec_dir = dir.clone();
    fs::write(crate::disk::paths::workflow_spec_file(&dir), text).unwrap();
    fs::write(&state.workspace_record, serde_json::to_vec(&json!({
        "layout":crate::disk::paths::WORKSPACE_LAYOUT, "workspace_id":crate::ids::WorkspaceId::from_project_root(home.path()),
        "project_root":home.path(), "session_name":"test", "updated_at":record.created_at
    })).unwrap()).unwrap();
    for status in [RunState::Active, RunState::Parked] {
        record.state = status;
        record.task = Some("land".into());
        record.scope.kind = ScopeKind::Project;
        {
            let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
            workflow::write(&state.workflow(&record.id), &record, &guard).unwrap();
        }
        let error = create(
            &state,
            DefinitionSource::Deployed {
                dir: &dir,
                task: None,
                surplus: None,
                surplus_after: None,
            },
            ScopeRoots {
                checkout: home.path().into(),
                project: home.path().into(),
            },
            || Ok(None),
            false,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "workflow review already has run {} for {}",
                record.id,
                home.path().display()
            )
        );
        assert!(
            matches!(&error, WorkflowErr::Exists { run, workflow, scope, task, kind }
            if run == &record.id && workflow == &record.workflow && scope == home.path()
                && task == &record.task && kind == &record.scope.kind),
            "{error:?}"
        );
    }
}

#[test]
fn held_scope_kind_controls_early_admission_after_definition_edits() {
    for (kind, held_checkout, duplicate) in [
        (ScopeKind::Project, false, true),
        (ScopeKind::Worktree, true, true),
        (ScopeKind::Worktree, false, false),
    ] {
        let (home, state, mut record, _) = fixture(
            "[workflow]\nscope='worktree'\nstart='pr'\n[checks.pr]\nrun='true'\non.ok='@done'\n",
        );
        let dir = home.path().join("review");
        let checkout = home.path().join("feature");
        fs::create_dir(&dir).unwrap();
        fs::create_dir(&checkout).unwrap();
        record.scope.kind = kind;
        if held_checkout {
            record.scope.path = checkout.clone();
        }
        let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
        workflow::write(&state.workflow(&record.id), &record, &guard).unwrap();
        drop(guard);
        let error = create(
            &state,
            DefinitionSource::Deployed {
                dir: &dir,
                task: None,
                surplus: None,
                surplus_after: None,
            },
            ScopeRoots {
                checkout,
                project: home.path().into(),
            },
            || Err("prompt was read".into()),
            false,
        )
        .unwrap_err();
        assert_eq!(
            matches!(error, WorkflowErr::Exists { .. }),
            duplicate,
            "{error}"
        );
        if !duplicate {
            assert!(error.to_string().contains("prompt was read"), "{error}");
        }
    }
}

#[test]
fn capacity_admission_precedes_takeover_without_publishing_an_attempt() {
    let (_home, state, mut record, spec) = fixture(
        "[workflow]\nscope='worktree'\nstart='pr'\n[steps.pr]\nagent='claude'\nprompt-file='prompt.md'\ntakeover=true\nthen='@done'\n",
    );
    record.task = Some("nightly".into());
    // A fire's cutoff stands, however late in the run the first hold comes.
    let cutoff = Timestamp::now()
        .checked_add(jiff::SignedDuration::from_hours(1))
        .unwrap();
    record.capacity_until = Some(cutoff);
    let paths = state.workflow(&record.id);
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    workflow::write(&paths, &record, &guard).unwrap();
    drop(guard);
    let host = super::super::tests::FakeHost::new();
    *host.occupancy.borrow_mut() = Occupancy::Occupied(vec!["sweeper".into()]);
    let mut admission = Admission::default();
    let decision = prepare(
        &state,
        &paths,
        &spec,
        &mut BTreeSet::new(),
        &host,
        &WindowReadings::fixed(Timestamp::now(), BTreeMap::new()),
        &mut admission,
    )
    .unwrap();
    assert!(
        matches!(decision, Decision::Admit(_)),
        "capacity must be decided before eviction"
    );
    admission.capacity = Some((
        record.next_seq,
        record.node.clone(),
        Ok(throttle::Attempt::Held(
            serde_json::from_value(
                json!({"kind":"capacity", "text":"no capacity", "kinds":["claude"]}),
            )
            .unwrap(),
        )),
    ));
    let decision = prepare(
        &state,
        &paths,
        &spec,
        &mut BTreeSet::new(),
        &host,
        &WindowReadings::fixed(Timestamp::now(), BTreeMap::new()),
        &mut admission,
    )
    .unwrap();
    assert!(matches!(decision, Decision::Stop));
    let current = workflow::load(&paths).unwrap();
    assert!(current.attempt.is_none());
    assert!(current.visits.is_empty());
    let wait = current.wait.unwrap();
    assert_eq!(wait.outcome, "capacity");
    assert_eq!(serde_json::to_value(&wait).unwrap()["hold"], "capacity");
    assert_eq!(
        wait.gives_up_at,
        Some(cutoff),
        "the run's cutoff bounds the hold"
    );
    prepare(
        &state,
        &paths,
        &spec,
        &mut BTreeSet::new(),
        &host,
        &WindowReadings::fixed(Timestamp::now(), BTreeMap::new()),
        &mut admission,
    )
    .unwrap();
    let current = workflow::load(&paths).unwrap();
    assert_eq!(current.wait.as_ref().unwrap().count, 2);
    assert_eq!(
        serde_json::to_value(&current.wait).unwrap()["hold"],
        "capacity"
    );
    let mut t = Transition {
        record: current,
        paths: &paths,
        spec: &spec,
        now: Timestamp::now(),
        entries: Vec::new(),
    };
    t.close_wait();
    assert_eq!(
        serde_json::to_value(&t.entries.last().unwrap().event).unwrap()["hold"],
        "capacity"
    );
}

#[test]
fn min_disk_is_read_where_a_steps_worktree_will_be_made() {
    for worktree in [Some("feat"), None] {
        let line = worktree.map_or_else(String::new, |name| format!("worktree='{name}'\n"));
        let (home, state, mut record, spec) = fixture(&format!(
            "[workflow]\nscope='worktree'\nstart='pr'\n[steps.pr]\nagent='claude'\n\
             prompt-file='prompt.md'\n{line}then='@done'\n"
        ));
        record.task = Some("nightly".into());
        let paths = state.workflow(&record.id);
        let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
        workflow::write(&paths, &record, &guard).unwrap();
        drop(guard);
        let decision = prepare(
            &state,
            &paths,
            &spec,
            &mut BTreeSet::new(),
            &super::super::tests::FakeHost::new(),
            &WindowReadings::fixed(Timestamp::now(), BTreeMap::new()),
            &mut Admission::default(),
        )
        .unwrap();
        let Decision::Admit(admit) = decision else {
            panic!("a task's agent step is admitted first")
        };
        // The default worktree home sits beside the checkout and does not exist yet, so its
        // nearest existing ancestor is the filesystem the launch will write to.
        let expected = match worktree {
            Some(_) => home.path().parent().unwrap(),
            None => home.path(),
        };
        assert_eq!(
            admit.2.disk.canonicalize().unwrap(),
            expected.canonicalize().unwrap(),
            "worktree {worktree:?}"
        );
    }
}

#[test]
fn admission_rechecks_bounds_after_occupancy() {
    admission_after_occupancy(true);
}

#[test]
fn admission_stamps_the_attempt_after_occupancy() {
    admission_after_occupancy(false);
}

fn admission_after_occupancy(expires: bool) {
    use std::cell::Cell;
    use std::rc::Rc;

    let (home, state, mut record, spec) = fixture(
        "[workflow]\nscope='worktree'\nstart='pick'\n[checks.pick]\nrun='true'\non.go='pr'\n[steps.pr]\nagent='claude'\nprompt-file='prompt.md'\nthen='after'\n[checks.after]\nrun='true'\non.ok='@done'\n",
    );
    fs::write(home.path().join("prompt.md"), "Build").unwrap();
    let deadline = Timestamp::now() + jiff::SignedDuration::from_secs(1);
    record.deadline = expires.then_some(deadline);
    let paths = state.workflow(&record.id);
    {
        let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
        workflow::write(&paths, &record, &guard).unwrap();
    }
    let observed_at = Rc::new(Cell::new(None));
    let host = super::super::tests::FakeHost::new();
    let observed = observed_at.clone();
    *host.on_occupancy.borrow_mut() = Some(Box::new(move || {
        if expires {
            let remaining = deadline.duration_since(Timestamp::now());
            if remaining.is_positive() {
                std::thread::sleep(std::time::Duration::try_from(remaining).unwrap());
            }
        }
        observed.set(Some(Timestamp::now()));
    }));
    let decision = prepare(
        &state,
        &paths,
        &spec,
        &mut BTreeSet::new(),
        &host,
        &WindowReadings::fixed(Timestamp::now(), BTreeMap::new()),
        &mut Admission::default(),
    )
    .unwrap();
    let after_occupancy = observed_at.get().expect("admission must reach occupancy");
    let current = workflow::load(&paths).unwrap();
    if expires {
        assert!(
            matches!(decision, Decision::Again),
            "a newly due bound must route, not launch"
        );
        assert!(current.attempt.is_none());
        assert!(
            current.visits.is_empty(),
            "a refused admission is not a visit"
        );
        assert_eq!(current.next_seq, 1);
        assert_eq!(current.state, RunState::Parked);
        assert_eq!(current.park.unwrap().reason, ParkReason::Deadline);
    } else {
        assert!(matches!(decision, Decision::Launch(_)));
        assert!(current.attempt.as_ref().unwrap().started_at >= after_occupancy);
        assert_eq!(current.entered_at, current.attempt.unwrap().started_at);
    }
}

#[test]
fn invalid_legacy_timeout_is_ignored_without_panicking() {
    let text = "[workflow]\nscope='worktree'\nstart='pr'\n[checks.pr]\nshell='true'\ntimeout='tomorrow'\non={pass='@done',fail='@failed'}\n";
    let (_home, state, record, _) = fixture(text);
    let result = std::panic::catch_unwind(|| {
        advance(&state, &record.id, &super::super::tests::FakeHost::new())
    });
    assert!(
        matches!(result, Ok(Ok(()))),
        "an invalid optional sealed timeout must be ignored: {result:?}"
    );
    assert_eq!(
        workflow::load(&state.workflow(&record.id)).unwrap().state,
        RunState::Done
    );
}

#[test]
fn step_window_waits_for_a_closed_companion_provider() {
    use crate::agents::{ProviderCapacity, RateLimitWindow};
    use crate::ids::{AgentKind, LoginKey};
    let now = Timestamp::now();
    let (_home, state, record, spec) = fixture(
        "[workflow]\nscope='worktree'\nstart='pr'\n[steps.pr]\nagent='claude,codex,term'\nprompt-file='prompt.md'\nthen='after'\nwindow='5h.left>=20'\n[checks.after]\nrun='true'\non.ok='@done'\n",
    );
    fs::write(record.spec_dir.join("prompt.md"), "Build").unwrap();
    let windows = WindowReadings::fixed(
        now,
        [("claude", 10), ("codex", 100)]
            .into_iter()
            .map(|(kind, used)| {
                (
                    LoginKey::default_for(AgentKind::new_unchecked(kind)),
                    ProviderCapacity::from_windows(vec![RateLimitWindow {
                        used_percentage: Some(used),
                        resets_at: Some(now + jiff::SignedDuration::from_secs(3600)),
                        duration_mins: Some(300),
                        ..Default::default()
                    }]),
                )
            })
            .collect(),
    );
    let result = prepare(
        &state,
        &state.workflow(&record.id),
        &spec,
        &mut BTreeSet::new(),
        &super::super::tests::FakeHost::new(),
        &windows,
        &mut Admission::default(),
    );
    assert!(
        result.is_ok(),
        "a layout window must gate every provider, not refuse the layout: {:?}",
        result.as_ref().err()
    );
    assert!(matches!(result.unwrap(), Decision::Stop));
    let current = workflow::load(&state.workflow(&record.id)).unwrap();
    assert_eq!(
        current.wait.unwrap().detail.as_deref(),
        Some("5h.left=0<20")
    );
    assert!(current.attempt.is_none());
}

#[test]
fn provider_windows_close_and_reopen_on_fresh_capacity() {
    use crate::agents::{ProviderCapacity, RateLimitWindow};
    use crate::harness::when::WindowReadings;
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
            window_wait(
                "window.5h.left >= 20",
                Path::new("/project"),
                &kind,
                None,
                &windows
            )
            .unwrap()
            .is_none(),
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
                &mut Admission::default(),
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
            assert_eq!(
                serde_json::to_value(&current).unwrap()["wait"]["detail"],
                "5h.left=0<20"
            );
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
        &mut Admission::default(),
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
            &mut Admission::default()
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
            &mut Admission::default()
        )
        .unwrap(),
        Decision::Evaluate(_)
    ));
    let ctx: serde_json::Value =
        serde_json::from_slice(&fs::read(&paths.context).unwrap()).unwrap();
    assert_eq!(ctx["seq"], 2);
    assert_eq!(ctx["retry"], record.check_retry_context.unwrap());
}

const CHECK_ONLY: &str = "[workflow]\non-break='@failed'\nscope='worktree'\nstart='pick'\n[checks.pick]\nrun='true'\non.ok='@done'\n";
const ONE_TURN: &str = "[workflow]\non-break='@failed'\nscope='worktree'\nstart='fix'\n[steps.fix]\nagent='claude'\nprompt-file='prompts/fix.md'\nlifecycle='turn'\nthen='@done'\non.failed='@failed'\n";

fn bare_room() -> (tempfile::TempDir, StatePaths) {
    let home = tempfile::tempdir().unwrap();
    let state = StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
    (home, state)
}

fn open_room(state: &StatePaths, root: &Path) {
    fs::create_dir_all(state.workspace_record.parent().unwrap()).unwrap();
    fs::write(&state.workspace_record, serde_json::to_vec(&json!({
        "layout":crate::disk::paths::WORKSPACE_LAYOUT, "workspace_id":crate::ids::WorkspaceId::from_project_root(root),
        "project_root":root, "session_name":"test", "updated_at":Timestamp::now()
    })).unwrap()).unwrap();
}

fn inline(task: &str, text: &str, assets: &[(&str, &str)]) -> DefinitionSource<'static> {
    DefinitionSource::Inline {
        task: task.into(),
        definition: InlineDefinition {
            text: text.into(),
            assets: assets
                .iter()
                .map(|(name, body)| ((*name).to_owned(), body.as_bytes().to_vec()))
                .collect(),
        },
        fire_tree: false,
        capacity_until: None,
        surplus: None,
        surplus_after: None,
    }
}

fn start_at(state: &StatePaths, root: &Path, source: DefinitionSource<'_>) -> Result<RunRecord> {
    create(
        state,
        source,
        ScopeRoots {
            checkout: root.into(),
            project: root.into(),
        },
        || Ok(None),
        false,
    )
}

#[test]
fn an_inline_run_seals_its_assets_and_reads_prompts_from_its_own_root() {
    let (home, state) = bare_room();
    open_room(&state, home.path());
    let record = start_at(
        &state,
        home.path(),
        inline("nightly", ONE_TURN, &[("prompts/fix.md", "fix the build")]),
    )
    .unwrap();
    let paths = state.workflow(&record.id);
    assert_eq!(
        (
            record.origin,
            record.workflow.as_str(),
            record.task.as_deref()
        ),
        (RunOrigin::Inline, "nightly", Some("nightly"))
    );
    assert_eq!(record.spec_dir, paths.definition.canonicalize().unwrap());
    assert_eq!(fs::read(&paths.spec).unwrap(), ONE_TURN.as_bytes());
    assert!(
        record.notes.is_some(),
        "an agent run keeps its notes folder"
    );
    let asset = paths.definition.join("prompts/fix.md");
    fs::write(&asset, "fix the build, sealed copy").unwrap();
    let spec = spec::load_run(&paths.home, &record).unwrap();
    assert_eq!(
        agent::compose(&record, &paths, &spec.steps["fix"], &mut BTreeMap::new()).unwrap(),
        "fix the build, sealed copy",
        "each attempt reads the prompt from the run's own definition root"
    );
}

#[test]
fn a_live_run_refuses_a_start_with_its_origin_and_name_or_its_task() {
    let (home, state) = bare_room();
    open_room(&state, home.path());
    let dir = home.path().join("nightly");
    let other = home.path().join("other");
    for dir in [&dir, &other] {
        fs::create_dir(dir).unwrap();
        fs::write(crate::disk::paths::workflow_spec_file(dir), CHECK_ONLY).unwrap();
    }
    let deployed = start_at(
        &state,
        home.path(),
        DefinitionSource::Deployed {
            dir: &dir,
            task: None,
            surplus: None,
            surplus_after: None,
        },
    )
    .unwrap();
    let task = start_at(&state, home.path(), inline("nightly", CHECK_ONLY, &[]))
        .expect("a deployed workflow and a task of the same name are different runs");
    let blocker = |source| match start_at(&state, home.path(), source) {
        Err(WorkflowErr::Exists { run, .. }) => run,
        other => panic!("expected a live run to refuse the start: {other:?}"),
    };
    assert_eq!(blocker(inline("nightly", CHECK_ONLY, &[])), task.id);
    assert_eq!(
        blocker(DefinitionSource::Deployed {
            dir: &other,
            task: Some("nightly".into()),
            surplus: None,
            surplus_after: None,
        }),
        task.id,
        "a task that now names another workflow still overlaps its inline run"
    );
    assert_eq!(
        blocker(DefinitionSource::Deployed {
            dir: &dir,
            task: None,
            surplus: None,
            surplus_after: None,
        }),
        deployed.id
    );
}

#[test]
fn a_task_started_run_needs_a_room_even_with_only_checks() {
    let (home, state) = bare_room();
    for source in [
        inline("probe", CHECK_ONLY, &[]),
        inline("fixer", ONE_TURN, &[("prompts/fix.md", "fix")]),
    ] {
        let error = start_at(&state, home.path(), source).unwrap_err();
        assert!(
            matches!(error, WorkflowErr::NoRoom(_)),
            "a fire in a root with no room refuses, as trunk's did: {error}"
        );
    }
    assert!(
        state.workflow_runs().map_or(true, |runs| runs.is_empty()),
        "a refused inline start leaves no sealed root behind"
    );
    open_room(&state, home.path());
    let probe = start_at(&state, home.path(), inline("probe2", CHECK_ONLY, &[]));
    assert_eq!(
        probe.unwrap().notes,
        None,
        "a task's check-only run allocates no notes folder"
    );
    let dir = home.path().join("checks");
    fs::create_dir(&dir).unwrap();
    fs::write(crate::disk::paths::workflow_spec_file(&dir), CHECK_ONLY).unwrap();
    let deployed = DefinitionSource::Deployed {
        dir: &dir,
        task: None,
        surplus: None,
        surplus_after: None,
    };
    assert!(
        start_at(&state, home.path(), deployed)
            .unwrap()
            .notes
            .is_some(),
        "a deployed run in a room keeps its notes and history"
    );
}
