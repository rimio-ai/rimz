use super::super::ScopeKind;
use super::*;
use crate::harness::workflow::timeout_at;
use std::fs;

#[test]
fn spawn_arguments_name_only_the_current_hook_frame() {
    use crate::harness::hook_drain::{FrameAttempt, with_frame_env};
    use crate::ids::{AgentKind, EventId};
    use crate::store::ingress::HookIngress;

    let (_home, _state, record, _spec) = fixture(
        "[workflow]\nscope='worktree'\nstart='pr'\n[script.pr]\ntimeout='365d'\nrun='true'\non.ok='@done'\n",
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
        "[workflow]\nscope='worktree'\nstart='pr'\n[script.pr]\ntimeout='365d'\nrun='true'\non.pending='@wait'\n",
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
    accounting::transition(&t, false);
    assert!(
        assist_log::recent(&crate::disk::paths::logs_dir(), None).is_empty(),
        "an open check wait is not an assist"
    );
    t.entries.clear();
    t.close_wait();
    accounting::transition(&t, false);
    let records = assist_log::recent(&crate::disk::paths::logs_dir(), None);
    assert_eq!(records.len(), 1, "{records:?}");
    let value = serde_json::to_value(&records[0]).unwrap();
    assert_eq!(value["to"], "@wait");
    assert_eq!(value["count"], 3);
    let mut canceled = evaluation.clone();
    canceled["origin"] = json!("engine");
    canceled["outcome"] = json!("canceled");
    t.entries = vec![serde_json::from_value(canceled).unwrap()];
    accounting::transition(&t, false);
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
    accounting::transition(&t, false);
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
        accounting::transition(&t, false);
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
                "kind":"agent", "lifecycle":"session", "seq":1, "node":"work",
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
        accounting::transition(&t, false);
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

/// A try's route assist names the target its answer decided, and its end assist says done.
#[test]
fn a_try_route_assist_names_the_decided_target() {
    use crate::harness::assist_log;
    if std::env::var_os("RIMZ_TEST_WORKFLOW_TRY_ACCOUNTING").is_none() {
        let home = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "harness::workflow::engine::tests::a_try_route_assist_names_the_decided_target",
                "--nocapture",
            ])
            .env("RIMZ_TEST_WORKFLOW_TRY_ACCOUNTING", "1")
            .env("RIMZ_UNIT_TEST_RIMZ_HOME", home.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let (_home, state, mut record, spec) = fixture(
        "[workflow]\nscope='worktree'\nstart='pr'\n[script.pr]\ntimeout='365d'\nrun='true'\non={red='fix',ok='@done'}\n[script.fix]\ntimeout='365d'\nrun='true'\non.ok='@done'\n",
    );
    record.once = true;
    let paths = state.workflow(&record.id);
    let mut t = Transition {
        record,
        spec: &spec,
        paths: &paths,
        now: Timestamp::UNIX_EPOCH,
        entries: Vec::new(),
    };
    t.entries.push(serde_json::from_value(json!({"event":"evaluation", "seq":1, "node":"pr", "at":t.now, "outcome":"red", "object":{"outcome":"red"}, "digests":{}, "started_at":t.now, "ended_at":t.now, "origin":"check"})).unwrap());
    t.route(&"fix".into(), json!({"outcome":"red"}), ParkReason::User)
        .unwrap();
    accounting::transition(&t, false);
    let values = assist_log::recent(&crate::disk::paths::logs_dir(), None)
        .iter()
        .map(|record| serde_json::to_value(record).unwrap())
        .collect::<Vec<_>>();
    let route = values
        .iter()
        .find(|value| value["assist"] == "workflow_route")
        .unwrap();
    let end = values
        .iter()
        .find(|value| value["assist"] == "workflow_end")
        .unwrap();
    assert_eq!(
        (&route["outcome"], &route["to"]),
        (&json!("red"), &json!("fix"))
    );
    assert_eq!(end["state"], "done");
}

#[test]
fn a_try_names_its_ending_from_the_record_alone() {
    let (_home, _state, mut record, _) = fixture(
        "[workflow]\nscope='worktree'\nstart='pr'\n[script.pr]\ntimeout='365d'\nrun='true'\non.ok='@done'\n",
    );
    record.state = RunState::Done;
    assert_eq!(
        outcome(&record, &[], false).ending,
        workflow::RunEnding::Done
    );
    record.once = true;
    assert_eq!(
        outcome(&record, &[], false).ending,
        workflow::RunEnding::Tried
    );
    record.state = RunState::Failed;
    assert_eq!(
        outcome(&record, &[], false).ending,
        workflow::RunEnding::Failed
    );
}

fn fixture(text: &str) -> (tempfile::TempDir, StatePaths, RunRecord, Definition) {
    let home = tempfile::tempdir().unwrap();
    let state = StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
    let record: RunRecord = serde_json::from_value(json!({
        "version":crate::store::workflow::VERSION, "id":WorkflowRunId::new(), "workflow":"review",
        "spec_dir":home.path(), "prompt":null,
        "scope":{"kind":"worktree","path":home.path()}, "project_root":home.path(),
        "created_at":"2000-01-01T00:00:00Z", "state":"active", "park":null, "node":"pr",
        "entered_at":"2000-01-01T00:00:00Z", "input":null, "key":null, "recheck_at":null,
        "wait":null, "visits":{}, "deadline":null,
        "deadline_fired":false, "next_seq":1, "attempt":null, "prev_attempt":null,
        "terminal_at":null
    }))
    .unwrap();
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    workflow::initialize(&state, &record, &guard).unwrap();
    fs::write(crate::disk::paths::workflow_spec_file(home.path()), text).unwrap();
    let spec = toml::from_str(text).unwrap();
    (home, state, record, spec)
}

#[test]
fn detaching_a_task_preserves_foreign_and_finished_runs() {
    let (review, other) = (crate::ids::TaskId::new(), crate::ids::TaskId::new());
    for (run_state, live) in [
        (RunState::Active, true),
        (RunState::Parked, true),
        (RunState::Done, false),
        (RunState::Failed, false),
        (RunState::Canceled, false),
    ] {
        for (task, id) in [("review", &review), ("other", &other)] {
            let (_home, state, mut record, _) = fixture(
                "[workflow]\nscope='worktree'\nstart='pr'\n[script.pr]\ntimeout='365d'\nrun='true'\non.pending='@wait'\n",
            );
            record.task = Some(task.into());
            record.loop_fire = Some(crate::ids::FireId::new(id));
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
            assert_eq!(reassign_task(&state, &review).unwrap(), expected);
            assert_eq!(workflow::load(&paths).unwrap(), record);
        }
    }
}

#[test]
fn a_task_cancels_only_its_live_runs_that_have_not_reached_an_agent() {
    let mut selected = Vec::new();
    let (review, other) = (crate::ids::TaskId::new(), crate::ids::TaskId::new());
    for (task, run_state, visited) in [
        ("review", RunState::Active, &[][..]),
        ("review", RunState::Active, &["pick"][..]),
        ("review", RunState::Parked, &["pick"][..]),
        ("review", RunState::Active, &["pick", "fix"][..]),
        ("review", RunState::Done, &["pick"][..]),
        ("other", RunState::Active, &["pick"][..]),
        ("review", RunState::Active, &["pick", "spread"][..]),
    ] {
        let (_home, state, mut record, _) = fixture(
            "[workflow]\nscope='worktree'\nstart='pick'\n[script.pick]\ntimeout='365d'\nrun='true'\non.go='fix'\non.fan='spread'\n[agent.fix]\nlifecycle='turn'\nagent='claude'\nprompt-file='prompt.md'\nthen='@done'\n[runs.spread]\nstart=[{workflow='child'}]\nthen='fix'\n",
        );
        record.task = Some(task.into());
        let id = if task == "review" { &review } else { &other };
        record.loop_fire = Some(crate::ids::FireId::new(id));
        record.state = run_state;
        for step in visited {
            record.visits.insert(
                (*step).into(),
                serde_json::from_value(serde_json::json!(1)).unwrap(),
            );
        }
        {
            let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
            workflow::write(&state.workflow(&record.id), &record, &guard).unwrap();
        }
        selected.push(unacted_task_runs(&state, &review).unwrap() == vec![record.id]);
    }
    assert_eq!(selected, [true, true, true, false, false, false, false]);
}

#[test]
fn bound_routes_stop_when_waiting_or_inactive() {
    for target in ["@wait", "@user", "@done", "@failed"] {
        let text = format!(
            "[workflow]\nscope='worktree'\nstart='pr'\n[script.pr]\nrun='true'\ntimeout='1s'\non.timeout='{target}'\n"
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
    let text = "[workflow]\nscope='worktree'\nstart='pr'\n[script.pr]\nrun='true'\ntimeout='tomorrow'\non.ok='missing'\n";
    let (home, state, record, _) = fixture(text);
    let dir = home.path().join("example");
    fs::create_dir(&dir).unwrap();
    fs::write(crate::disk::paths::workflow_spec_file(&dir), text).unwrap();
    let error = create(
        &state,
        DefinitionSource::Deployed {
            loop_fire: None,
            dir: &dir,
            task: None,
            fire: None,
            fire_facts: None,
        },
        ScopeRoots {
            checkout: home.path().into(),
            project: home.path().into(),
        },
        || Ok(None),
        false,
        Default::default(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("script.pr.on.ok"),
        "the bytes to seal must be validated first: {error}"
    );
    assert_eq!(workflow::live_runs(&state).unwrap().len(), 1);
    assert!(state.workflow(&record.id).home.exists());
}

#[test]
fn window_wait_evidence_preserves_strict_and_inclusive_boundaries() {
    for (comparison, reading, expected) in [
        (">=42", Some("12"), "window.5h.left=12<42"),
        (">42", Some("42"), "window.5h.left=42<=42"),
        ("<=42", Some("80"), "window.5h.left=80>42"),
        ("<42", Some("42"), "window.5h.left=42>=42"),
        (">=42", None, "window.5h.left unread"),
    ] {
        let expr = spec::parse_window(&format!("5h.left{comparison}")).unwrap();
        let verdict = crate::harness::when::Verdict {
            truth: Some(false),
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
            truth: Some(false),
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
fn window_wait_evidence_names_the_known_leaf_that_decides_over_an_unread_one() {
    let expr = spec::parse_window("!(5h.left>=40 || 7d.left>=40)").unwrap();
    let verdict = crate::harness::when::Verdict {
        truth: Some(false),
        readings: BTreeMap::from([
            ("window.5h.left".into(), None),
            ("window.7d.left".into(), Some("80".into())),
        ]),
        unread: Default::default(),
    };
    assert_eq!(expr.blocking_evidence(&verdict), "!window.7d.left=80>=40");
}

#[test]
fn active_and_parked_duplicates_return_the_existing_run_as_a_typed_outcome() {
    let text = "[workflow]\nscope='worktree'\nstart='pr'\n[script.pr]\ntimeout='365d'\nrun='true'\non.ok='@done'\n";
    let (home, state, mut record, _) = fixture(text);
    let dir = home.path().join("review");
    fs::create_dir(&dir).unwrap();
    record.spec_dir = Some(dir.clone());
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
                loop_fire: None,
                dir: &dir,
                task: None,
                fire: None,
                fire_facts: None,
            },
            ScopeRoots {
                checkout: home.path().into(),
                project: home.path().into(),
            },
            || Ok(None),
            false,
            Default::default(),
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
            "[workflow]\nscope='worktree'\nstart='pr'\n[script.pr]\ntimeout='365d'\nrun='true'\non.ok='@done'\n",
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
                loop_fire: None,
                dir: &dir,
                task: None,
                fire: None,
                fire_facts: None,
            },
            ScopeRoots {
                checkout,
                project: home.path().into(),
            },
            || Err("prompt was read".into()),
            false,
            Default::default(),
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
        "[workflow]\nscope='worktree'\nstart='pr'\n[agent.pr]\nlifecycle='turn'\nagent='claude'\nprompt-file='prompt.md'\nadmission.occupancy='takeover'\nthen='@done'\n",
    );
    // The run's give-up instant stands, however late in the run the first hold comes.
    let cutoff = Timestamp::now()
        .checked_add(jiff::SignedDuration::from_hours(1))
        .unwrap();
    record.give_up = Some(cutoff);
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
        "the run's give-up instant bounds the hold"
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
fn a_continued_restart_that_gives_up_on_capacity_leaves_no_session_to_the_routed_step() {
    let (_home, state, mut record, spec) = fixture(
        "[workflow]\nscope='worktree'\nstart='pr'\n[agent.pr]\nlifecycle='turn'\n\
         agent='claude'\nprompt-file='prompt.md'\nthen='@done'\non.capacity='fix'\n\
         [agent.fix]\nlifecycle='turn'\nagent='claude'\nprompt-file='prompt.md'\nthen='@done'\n",
    );
    record.give_up = Some(Timestamp::now() - std::time::Duration::from_secs(60));
    record.restart = Some(workflow::Restart {
        resume: Some("conversation".into()),
        settled: None,
    });
    fs::write(
        record.spec_dir.as_deref().unwrap().join("prompt.md"),
        "Build",
    )
    .unwrap();
    let paths = state.workflow(&record.id);
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    workflow::write(&paths, &record, &guard).unwrap();
    drop(guard);
    let host = super::super::tests::FakeHost::new();
    let mut admission = Admission::default();
    let mut restarted_at_fix = false;
    for _ in 0..10 {
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
        match decision {
            Decision::Admit(request) if request.1 == "pr" => {
                admission.capacity = Some((
                    request.0,
                    request.1,
                    Ok(throttle::Attempt::Held(
                        serde_json::from_value(
                            json!({"kind":"capacity", "text":"no capacity", "kinds":["claude"]}),
                        )
                        .unwrap(),
                    )),
                ));
            }
            Decision::Admit(request) => {
                if !restarted_at_fix {
                    let routed = workflow::load(&paths).unwrap();
                    assert_eq!(routed.node, "fix");
                    assert_eq!(routed.restart, None, "{routed:#?}");
                    restart(&state, &record.id, None, &host)
                        .expect("a restart of the routed step is accepted");
                    restarted_at_fix = true;
                }
                admission.capacity = Some((request.0, request.1, Ok(throttle::Attempt::Open)));
            }
            Decision::Launch(launch) => {
                agent::launch(&state, &paths, &spec, *launch, &host).unwrap();
                break;
            }
            Decision::Again => {}
            _ => panic!("unexpected decision"),
        }
    }
    let launched = host.launched.borrow();
    assert_eq!(launched.len(), 1, "only the routed step launches");
    assert_eq!(
        (launched[0].automation.clone(), launched[0].resume.clone()),
        (
            crate::agents::Automation::Workflow {
                workflow: record.workflow.clone(),
                step: "fix".into(),
                depth: Some(record.depth),
            },
            None
        )
    );
}

#[test]
fn every_origin_waits_at_a_hold_and_only_the_stored_skip_launches_past_admission() {
    let mut actual = Vec::new();
    for origin in ["hand", "loop fire", "scheduled", "restart"] {
        for capacity in [workflow::StartCapacity::Wait, workflow::StartCapacity::Skip] {
            for answer in ["held", "unreadable"] {
                let (_home, state, mut record, spec) = fixture(
                    "[workflow]\nscope='worktree'\nstart='pr'\n[agent.pr]\nlifecycle='turn'\n\
                     agent='claude'\nprompt-file='prompt.md'\nthen='@done'\n",
                );
                record.task = (origin != "hand").then(|| "nightly".into());
                record.manual = origin == "loop fire";
                record.restart = (origin == "restart").then_some(workflow::Restart {
                    resume: None,
                    settled: None,
                });
                record.capacity = capacity;
                fs::write(
                    record.spec_dir.as_deref().unwrap().join("prompt.md"),
                    "Build",
                )
                .unwrap();
                let paths = state.workflow(&record.id);
                let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
                workflow::write(&paths, &record, &guard).unwrap();
                drop(guard);
                let host = super::super::tests::FakeHost::new();
                let mut admission = Admission::default();
                let prepare = |admission: &mut Admission| {
                    prepare(
                        &state,
                        &paths,
                        &spec,
                        &mut BTreeSet::new(),
                        &host,
                        &WindowReadings::fixed(Timestamp::now(), BTreeMap::new()),
                        admission,
                    )
                    .unwrap()
                };
                let Decision::Admit(admit) = prepare(&mut admission) else {
                    panic!("{origin}: every agent start asks admission first")
                };
                assert_eq!(
                    admit.2.skip,
                    capacity == workflow::StartCapacity::Skip,
                    "{origin}"
                );
                admission.capacity = Some((
                    record.next_seq,
                    record.node.clone(),
                    match answer {
                        "held" => Ok(throttle::Attempt::Held(
                            serde_json::from_value(
                                json!({"kind":"capacity", "text":"no capacity", "kinds":["claude"]}),
                            )
                            .unwrap(),
                        )),
                        _ => Err(throttle::ThrottleError::Unreadable("no psi".into())),
                    },
                ));
                let decision = match prepare(&mut admission) {
                    Decision::Stop => "stop".to_owned(),
                    Decision::Launch(launch) => match &launch.1 {
                        Ok(_) => format!(
                            "launch past {:?}",
                            launch.0.attempt.as_ref().unwrap().started_past_limit
                        ),
                        Err(reason) => format!("fail: {reason}"),
                    },
                    _ => "other".to_owned(),
                };
                let wait = workflow::load(&paths)
                    .unwrap()
                    .wait
                    .map(|wait| wait.outcome);
                actual.push((origin, capacity, answer, decision, wait));
            }
        }
    }
    let mut expected = Vec::new();
    for origin in ["hand", "loop fire", "scheduled", "restart"] {
        expected.extend([
            (
                origin,
                workflow::StartCapacity::Wait,
                "held",
                "stop".to_owned(),
                Some("capacity".to_owned()),
            ),
            (
                origin,
                workflow::StartCapacity::Wait,
                "unreadable",
                "fail: no psi".to_owned(),
                None,
            ),
            (
                origin,
                workflow::StartCapacity::Skip,
                "held",
                "launch past Some(\"no capacity\")".to_owned(),
                None,
            ),
            (
                origin,
                workflow::StartCapacity::Skip,
                "unreadable",
                "launch past Some(\"no psi\")".to_owned(),
                None,
            ),
        ]);
    }
    assert_eq!(actual, expected);
}

#[test]
fn min_disk_is_read_where_a_steps_worktree_will_be_made() {
    for worktree in [Some("feat"), None] {
        let line = worktree.map_or_else(String::new, |name| format!("worktree='{name}'\n"));
        let (home, state, mut record, spec) = fixture(&format!(
            "[workflow]\nscope='worktree'\nstart='pr'\n[agent.pr]\nlifecycle='turn'\nagent='claude'\n\
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

/// `prepare` with admission answering open, as an unthrottled machine does.
fn prepare_open(
    state: &StatePaths,
    paths: &WorkflowPaths,
    spec: &Definition,
    host: &dyn AttemptHost,
    windows: &WindowReadings<'_>,
) -> Decision {
    let mut admission = Admission::default();
    loop {
        match prepare(
            state,
            paths,
            spec,
            &mut BTreeSet::new(),
            host,
            windows,
            &mut admission,
        )
        .unwrap()
        {
            Decision::Admit(admit) => {
                let (seq, node, _) = *admit;
                admission.capacity = Some((seq, node, Ok(throttle::Attempt::Open)));
            }
            decision => return decision,
        }
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
        "[workflow]\nscope='worktree'\nstart='pick'\n[script.pick]\ntimeout='365d'\nrun='true'\non.go='pr'\n[agent.pr]\nlifecycle='turn'\nagent='claude'\nprompt-file='prompt.md'\nthen='after'\n[script.after]\ntimeout='365d'\nrun='true'\non.ok='@done'\n",
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
    let reads = Cell::new(0);
    *host.on_occupancy.borrow_mut() = Some(Box::new(move || {
        // The bound falls due while the admitted pass reads occupancy, after admission asked.
        reads.set(reads.get() + 1);
        if expires && reads.get() == 2 {
            let remaining = deadline.duration_since(Timestamp::now());
            if remaining.is_positive() {
                std::thread::sleep(std::time::Duration::try_from(remaining).unwrap());
            }
        }
        observed.set(Some(Timestamp::now()));
    }));
    let decision = prepare_open(
        &state,
        &paths,
        &spec,
        &host,
        &WindowReadings::fixed(Timestamp::now(), BTreeMap::new()),
    );
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
        assert_eq!(current.state, RunState::Failed);
        assert!(current.park.is_none());
    } else {
        assert!(matches!(decision, Decision::Launch(_)));
        assert!(current.attempt.as_ref().unwrap().started_at >= after_occupancy);
        assert_eq!(current.entered_at, current.attempt.unwrap().started_at);
    }
}

#[test]
fn an_invalid_timeout_refuses_at_the_read_without_a_write_and_proceeds_once_fixed() {
    let text = "[workflow]\nscope='worktree'\nstart='pr'\n[test.pr]\nshell='true'\ntimeout='tomorrow'\non={pass='@done',fail='@failed'}\n";
    let (_home, state, record, _) = fixture(text);
    let paths = state.workflow(&record.id);
    let before = (
        fs::read(&paths.record).unwrap(),
        fs::read(&paths.journal).ok(),
    );
    let host = super::super::tests::FakeHost::new();
    let refused = advance(&state, &record.id, &host).unwrap_err();
    assert!(
        matches!(&refused, WorkflowErr::Refused(error) if error.contains("test.pr.timeout")),
        "{refused:?}"
    );
    assert_eq!(
        (
            fs::read(&paths.record).unwrap(),
            fs::read(&paths.journal).ok()
        ),
        before
    );
    fs::write(
        crate::disk::paths::workflow_spec_file(&record.definition_dir(&paths)),
        text.replace("tomorrow", "5m"),
    )
    .unwrap();
    advance(&state, &record.id, &host).unwrap();
    // The fixture entered `pr` in 2000, so the fixed timeout has long run out.
    let ended = workflow::load(&paths).unwrap();
    assert_eq!(
        ended.outcome.and_then(|outcome| outcome.reason).as_deref(),
        Some("pr ran past its timeout 5m")
    );
}

#[test]
fn step_window_waits_for_a_closed_companion_provider() {
    use crate::agents::{ProviderCapacity, RateLimitWindow};
    use crate::ids::{AgentKind, LoginKey};
    let now = Timestamp::now();
    let (_home, state, record, spec) = fixture(
        "[workflow]\nscope='worktree'\nstart='pr'\n[agent.pr]\nlifecycle='turn'\nagent='claude,codex,term'\nprompt-file='prompt.md'\nthen='after'\nadmission.window='5h.left>=20'\n[script.after]\ntimeout='365d'\nrun='true'\non.ok='@done'\n",
    );
    fs::write(
        record.spec_dir.as_deref().unwrap().join("prompt.md"),
        "Build",
    )
    .unwrap();
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
fn a_window_or_stagger_that_does_not_parse_reaches_its_consumer_as_an_error() {
    use crate::harness::when::WindowReadings;
    let spec: Definition = toml::from_str(
        "[workflow]\nscope='worktree'\nstart='build'\nlimit={stagger='soon'}\n[agent.build]\nlifecycle='turn'\n\
         agent='claude'\nprompt-file='p.md'\nadmission.window='window.5h.bogus>=x'\nthen='@done'\n",
    )
    .unwrap();
    let windows = WindowReadings::fixed(Timestamp::UNIX_EPOCH, BTreeMap::new());
    assert!(
        window_wait(
            spec.step("build").unwrap().window().unwrap(),
            Path::new("/project"),
            &crate::ids::AgentKind::new_unchecked("claude"),
            None,
            &windows,
        )
        .is_err()
    );
    assert!(matches!(
        start_limit(spec.limit().unwrap()).stagger,
        Some(Err(_))
    ));
}

#[test]
fn provider_windows_close_and_reopen_on_fresh_capacity() {
    use crate::agents::{ProviderCapacity, RateLimitWindow};
    use crate::harness::when::WindowReadings;
    use crate::ids::LoginKey;
    let now = "2026-01-01T00:00:00Z".parse::<Timestamp>().unwrap();
    let kind = crate::ids::AgentKind::new_unchecked("claude");
    let (_home, state, mut record, spec) = fixture(
        "[workflow]\nscope='worktree'\nstart='pr'\n[agent.pr]\nlifecycle='turn'\nagent='claude'\nprompt-file='prompt.md'\nthen='after'\nadmission.window='5h.left >= 20'\n[script.after]\ntimeout='365d'\nrun='true'\non.ok='@done'\n",
    );
    fs::write(
        record.spec_dir.as_deref().unwrap().join("prompt.md"),
        "Build",
    )
    .unwrap();
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
                &crate::harness::workflow::definition::window("window.5h.left >= 20"),
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
            let decision =
                prepare_open(&state, &state.workflow(&record.id), &spec, &host, &windows);
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
            assert_eq!(current.visits["pr"], 1);
            assert!(current.wait.is_none());
            assert!(workflow::journal(&state.workflow(&record.id)).unwrap().iter().any(|row| matches!(&row.event, JournalEvent::Wait { outcome, count:2, .. } if outcome == "window")));
        }
    }
    // An unread window is unknown, and the gate admits on true alone, negated or not.
    let unread = WindowReadings::fixed(now, BTreeMap::new());
    for raw in ["5h.left >= 20", "!(5h.left >= 20)"] {
        assert_eq!(
            window_wait(
                &crate::harness::workflow::definition::window(raw),
                Path::new("/project"),
                &kind,
                None,
                &unread
            )
            .unwrap()
            .as_deref(),
            Some("5h.left unread"),
            "{raw}"
        );
    }
}

/// A judge with a one-hour clock behind a usage window, entered now.
fn timed_judge(window: bool) -> (tempfile::TempDir, StatePaths, RunRecord, Definition) {
    let (home, state, mut record, spec) = fixture(&format!(
        "[workflow]\nscope='worktree'\nstart='pr'\n[judge.pr]\ntimeout='1h'\nagent='claude'\nprompt-file='prompt.md'\non.ok='@done'\n{}",
        if window {
            "admission.window='5h.left >= 20'\n"
        } else {
            ""
        }
    ));
    fs::write(home.path().join("prompt.md"), "Judge").unwrap();
    record.entered_at = Timestamp::now();
    rewrite(&state, &record);
    (home, state, record, spec)
}

fn rewrite(state: &StatePaths, record: &RunRecord) {
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    workflow::write(&state.workflow(&record.id), record, &guard).unwrap();
}

/// Move every stamp of a waiting judge `by` into the past, as if that time had passed.
fn age_pause(state: &StatePaths, record: &RunRecord, by: jiff::SignedDuration) -> RunRecord {
    let mut current = workflow::load(&state.workflow(&record.id)).unwrap();
    current.entered_at -= by;
    current.recheck_at = current.recheck_at.map(|at| at - by);
    rewrite(state, &current);
    current
}

fn prepare_judge(
    state: &StatePaths,
    record: &RunRecord,
    spec: &Definition,
    windows: &WindowReadings<'_>,
) -> Decision {
    prepare(
        state,
        &state.workflow(&record.id),
        spec,
        &mut BTreeSet::new(),
        &super::super::tests::FakeHost::new(),
        windows,
        &mut Admission::default(),
    )
    .unwrap()
}

#[test]
fn a_judge_waits_out_its_window_before_it_launches_and_its_clock_counts_from_the_launch() {
    use crate::agents::{ProviderCapacity, RateLimitWindow};
    let (_home, state, record, spec) = timed_judge(true);
    let now = Timestamp::now();
    let windows = |used| {
        WindowReadings::fixed(
            now,
            BTreeMap::from([(
                crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("claude")),
                ProviderCapacity::from_windows(vec![RateLimitWindow {
                    used_percentage: Some(used),
                    resets_at: Some(now + jiff::SignedDuration::from_hours(3)),
                    duration_mins: Some(300),
                    ..Default::default()
                }]),
            )]),
        )
    };
    assert!(matches!(
        prepare_judge(&state, &record, &spec, &windows(100)),
        Decision::Stop
    ));
    let waiting = workflow::load(&state.workflow(&record.id)).unwrap();
    assert_eq!(waiting.wait.as_ref().unwrap().outcome, "window");
    assert!(waiting.attempt.is_none());
    assert_eq!(timeout_at(&spec, &waiting).unwrap(), None);
    age_pause(&state, &record, jiff::SignedDuration::from_hours(2));
    assert!(
        matches!(
            prepare_judge(&state, &record, &spec, &windows(10)),
            Decision::Launch(_)
        ),
        "two hours in the window wait do not time out a one-hour judge"
    );
    let running = workflow::load(&state.workflow(&record.id)).unwrap();
    assert!(
        running.wait.is_none(),
        "the window wait closes at the launch"
    );
    let judge = running.attempt.clone().unwrap();
    assert_eq!(judge.kind, workflow::AttemptKind::Judge);
    let timeout = timeout_at(&spec, &running).unwrap().unwrap();
    assert_eq!(
        timeout,
        judge.started_at + jiff::SignedDuration::from_hours(1),
        "the clock counts from the launch"
    );
}

#[test]
fn a_judge_waits_out_a_budget_refusal_after_its_window_and_its_clock_counts_from_the_launch() {
    use crate::agents::{ProviderCapacity, RateLimitWindow};
    const REASON: &str = "fleet budget $5.00/day is spent";
    let now = Timestamp::now();
    let windows = |used| {
        WindowReadings::fixed(
            now,
            BTreeMap::from([(
                crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("claude")),
                ProviderCapacity::from_windows(vec![RateLimitWindow {
                    used_percentage: Some(used),
                    resets_at: Some(now + jiff::SignedDuration::from_hours(3)),
                    duration_mins: Some(300),
                    ..Default::default()
                }]),
            )]),
        )
    };
    for window in [true, false] {
        let (_home, state, record, spec) = timed_judge(window);
        let host = super::super::tests::FakeHost::new();
        let prepare_with = |windows: &WindowReadings<'_>| {
            prepare(
                &state,
                &state.workflow(&record.id),
                &spec,
                &mut BTreeSet::new(),
                &host,
                windows,
                &mut Admission::default(),
            )
            .unwrap()
        };
        let wait_of = || {
            let current = workflow::load(&state.workflow(&record.id)).unwrap();
            current.wait.map(|wait| (wait.outcome, wait.detail))
        };
        if window {
            assert!(matches!(prepare_with(&windows(100)), Decision::Stop));
            assert_eq!(wait_of().unwrap().0, "window");
        }
        *host.budget.borrow_mut() = Some(REASON.into());
        assert!(matches!(prepare_with(&windows(10)), Decision::Stop));
        assert_eq!(
            wait_of(),
            Some(("budget".into(), Some(REASON.into()))),
            "window={window}: a window wait gives way to the budget's"
        );
        let waiting = workflow::load(&state.workflow(&record.id)).unwrap();
        assert!(waiting.attempt.is_none());
        assert_eq!(timeout_at(&spec, &waiting).unwrap(), None);
        age_pause(&state, &record, jiff::SignedDuration::from_hours(2));
        host.budget.borrow_mut().take();
        assert!(
            matches!(prepare_with(&windows(10)), Decision::Launch(_)),
            "window={window}: two hours in the budget wait do not time out a one-hour judge"
        );
        let running = workflow::load(&state.workflow(&record.id)).unwrap();
        assert!(running.wait.is_none(), "window={window}: the wait closes");
        let judge = running.attempt.clone().unwrap();
        assert_eq!(
            timeout_at(&spec, &running).unwrap().unwrap(),
            judge.started_at + jiff::SignedDuration::from_hours(1),
            "window={window}: the clock counts from the launch"
        );
        let journal = workflow::journal(&state.workflow(&record.id)).unwrap();
        let waits: Vec<_> = journal
            .iter()
            .filter_map(|entry| match &entry.event {
                workflow::JournalEvent::Wait { outcome, .. } => Some(outcome.as_str()),
                _ => None,
            })
            .collect();
        let expected: &[&str] = if window {
            &["window", "budget"]
        } else {
            &["budget"]
        };
        assert_eq!(waits, expected);
    }
}

/// A refused step stops before capacity admission whatever its stored capacity, and a failed
/// budget read leaves the run as it was for the next tick.
#[test]
fn a_budget_refusal_asks_no_admission_and_a_failed_read_leaves_the_run_unchanged() {
    for capacity in [workflow::StartCapacity::Wait, workflow::StartCapacity::Skip] {
        let (_home, state, mut record, spec) = fixture(
            "[workflow]\nscope='worktree'\nstart='pr'\n[agent.pr]\nlifecycle='turn'\n\
             agent='claude'\nprompt-file='prompt.md'\nthen='@done'\n",
        );
        record.task = Some("nightly".into());
        record.capacity = capacity;
        rewrite(&state, &record);
        let host = super::super::tests::FakeHost::new();
        let prepare = |host: &super::super::tests::FakeHost| {
            prepare(
                &state,
                &state.workflow(&record.id),
                &spec,
                &mut BTreeSet::new(),
                host,
                &WindowReadings::fixed(Timestamp::now(), BTreeMap::new()),
                &mut Admission::default(),
            )
        };
        host.budget_error.set(true);
        assert!(prepare(&host).is_err(), "{capacity:?}");
        let unchanged = workflow::load(&state.workflow(&record.id)).unwrap();
        assert!(
            unchanged.attempt.is_none()
                && unchanged.wait.is_none()
                && unchanged.recheck_at.is_none(),
            "{capacity:?}: {unchanged:#?}"
        );
        host.budget_error.set(false);
        *host.budget.borrow_mut() = Some("account claude:work budget $3.00/day is spent".into());
        assert!(
            matches!(prepare(&host).unwrap(), Decision::Stop),
            "{capacity:?}: a refused start asks no admission"
        );
        assert_eq!(
            workflow::load(&state.workflow(&record.id))
                .unwrap()
                .wait
                .map(|wait| wait.outcome),
            Some("budget".into())
        );
    }
}

#[test]
fn any_cells_refused_login_refuses_the_launch() {
    let group = |kind: &str, account: Option<&str>| throttle::StartGroup {
        kind: crate::ids::AgentKind::new_unchecked(kind),
        account: account.map(|name| name.parse().unwrap()),
        agents: 1,
    };
    let groups = [group("claude", None), group("codex", Some("work"))];
    let asked = std::cell::RefCell::new(Vec::new());
    let refusal = first_refusal(&groups, |login| {
        asked.borrow_mut().push(login.map(ToString::to_string));
        login.map(|login| format!("{login} is spent"))
    });
    assert_eq!(refusal.as_deref(), Some("codex@work is spent"));
    assert_eq!(*asked.borrow(), [None, Some("codex@work".to_owned())]);
    assert_eq!(first_refusal(&groups, |_| None), None);
}

const CHECK_ONLY: &str = "[workflow]\nscope='worktree'\nstart='pick'\n[script.pick]\ntimeout='365d'\nrun='true'\non.ok='@done'\n";
const ONE_TURN: &str = "[workflow]\nscope='worktree'\nstart='fix'\n[agent.fix]\nagent='claude'\nprompt-file='prompts/fix.md'\nlifecycle='turn'\nthen='@done'\non.failed='@failed'\n";

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
        loop_fire: crate::ids::FireId::new(&crate::ids::TaskId::new()),
        task: task.into(),
        definition: InlineDefinition {
            text: text.into(),
            assets: assets
                .iter()
                .map(|(name, body)| ((*name).to_owned(), body.as_bytes().to_vec()))
                .collect(),
        },
        fire: None,
        fire_facts: None,
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
        Default::default(),
    )
    .map(|(record, _)| record)
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
    assert_eq!(record.spec_dir, None);
    assert_eq!(record.definition_dir(&paths), paths.definition);
    assert_eq!(
        fs::read(crate::disk::paths::workflow_spec_file(&paths.definition)).unwrap(),
        ONE_TURN.as_bytes()
    );
    assert!(!crate::disk::paths::workflow_spec_file(&paths.home).exists());
    assert!(
        record.notes.is_some(),
        "an agent run keeps its notes folder"
    );
    let asset = paths.definition.join("prompts/fix.md");
    fs::write(&asset, "fix the build, sealed copy").unwrap();
    let spec = spec::load(&record.definition_dir(&paths)).unwrap();
    assert_eq!(
        agent::compose(
            &record,
            &paths,
            spec.step("fix").unwrap(),
            &mut BTreeMap::new()
        )
        .unwrap(),
        "fix the build, sealed copy",
        "each attempt reads the prompt from the run's own definition root"
    );
}

fn deployed(dir: &Path) -> DefinitionSource<'_> {
    DefinitionSource::Deployed {
        loop_fire: None,
        dir,
        task: None,
        fire: None,
        fire_facts: None,
    }
}

/// A deployed run copies nothing: it records its directory as it was given, links kept, and
/// every read takes the directory as it is, so an entry the definition never names, even one
/// that resolves outside it or nowhere, no longer stops the start.
#[test]
fn a_deployed_run_reads_its_directory_live_and_keeps_no_copy() {
    let (home, state) = bare_room();
    open_room(&state, home.path());
    fs::write(home.path().join("secret.md"), "outside").unwrap();
    let dir = home.path().join("nightly");
    fs::create_dir_all(dir.join("prompts")).unwrap();
    fs::write(crate::disk::paths::workflow_spec_file(&dir), ONE_TURN).unwrap();
    fs::write(dir.join("prompts/fix.md"), "fix the build").unwrap();
    std::os::unix::fs::symlink(home.path().join("secret.md"), dir.join("prompts/leak.md")).unwrap();
    std::os::unix::fs::symlink(home.path().join("missing.md"), dir.join("prompts/gone.md"))
        .unwrap();
    // A start through a link records the link, as a pack workflow's entry link is recorded.
    let link = home.path().join("linked");
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    let record = start_at(&state, home.path(), deployed(&link)).unwrap();
    let paths = state.workflow(&record.id);
    assert_eq!(record.origin, RunOrigin::Deployed);
    assert_eq!(record.workflow, "nightly");
    assert_eq!(record.spec_dir, Some(link));
    assert!(!paths.definition.exists());
    // A `..` after a link stays in the record, so it names the directory validated.
    let real = home.path().join("real");
    fs::create_dir_all(real.join("sub")).unwrap();
    for flow in [real.join("flow"), home.path().join("flow")] {
        fs::create_dir_all(&flow).unwrap();
        fs::write(crate::disk::paths::workflow_spec_file(&flow), CHECK_ONLY).unwrap();
    }
    std::os::unix::fs::symlink(real.join("sub"), home.path().join("hop")).unwrap();
    let hopped = start_at(
        &state,
        home.path(),
        deployed(&home.path().join("hop/../flow")),
    )
    .unwrap();
    assert_eq!(
        hopped.spec_dir.unwrap().canonicalize().unwrap(),
        real.join("flow").canonicalize().unwrap()
    );
    assert!(!crate::disk::paths::workflow_spec_file(&paths.home).exists());
    fs::write(dir.join("prompts/fix.md"), "an edit after start").unwrap();
    let spec = spec::load(&record.definition_dir(&paths)).unwrap();
    assert_eq!(
        agent::compose(
            &record,
            &paths,
            spec.step("fix").unwrap(),
            &mut BTreeMap::new()
        )
        .unwrap(),
        "an edit after start",
        "a deployed run reads its prompt from the directory as it is"
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
            loop_fire: None,
            dir: &dir,
            task: None,
            fire: None,
            fire_facts: None,
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
            loop_fire: task
                .loop_fire
                .as_ref()
                .map(|fire| crate::ids::FireId::new(&fire.task())),
            dir: &other,
            task: Some("nightly".into()),
            fire: None,
            fire_facts: None,
        }),
        task.id,
        "a task that now names another workflow still overlaps its inline run"
    );
    assert_eq!(
        blocker(DefinitionSource::Deployed {
            loop_fire: None,
            dir: &dir,
            task: None,
            fire: None,
            fire_facts: None,
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
        loop_fire: None,
        dir: &dir,
        task: None,
        fire: None,
        fire_facts: None,
    };
    assert!(
        start_at(&state, home.path(), deployed)
            .unwrap()
            .notes
            .is_some(),
        "a deployed run in a room keeps its notes and history"
    );
}

/// A workflow task's fire written before loop tasks stopped cutting a tree each fire passed
/// `fire_tree` in its `--loop-fire-json`: such a payload still decodes.
#[test]
fn an_older_task_start_payload_with_fire_tree_still_decodes() {
    let fire = crate::ids::FireId::new(&crate::ids::TaskId::new());
    let start: TaskStart = serde_json::from_value(serde_json::json!({
        "task": "nightly",
        "loop_fire": fire.as_str(),
        "fire": {"kind": "manual", "task": "nightly"},
        "fire_facts": null,
        "fire_tree": true,
        "manual": true,
    }))
    .expect("older payload");
    assert_eq!(
        (start.task.as_str(), start.loop_fire, start.manual),
        ("nightly", fire, true)
    );
}

#[test]
fn a_start_takes_its_starter_and_depth_from_the_enclosing_run_and_the_calling_agent() {
    use crate::agents::{Automation, WorkflowAttemptRef, WorkflowAttemptRole};
    use crate::store::workflow::RunStarter;
    let (_home, _state, mut run, _) = fixture(ONE_TURN);
    run.depth = 2;
    let mut inline = run.clone();
    inline.origin = RunOrigin::Inline;
    let at = jiff::Timestamp::UNIX_EPOCH;
    let mut outside = crate::testkit::agent_state("claude", "outside", at);
    outside.launch_id = Some("launch-outside".into());
    let mut step = crate::testkit::agent_state("claude", "step", at);
    step.launch_id = Some("launch-step".into());
    step.workflow = Some(WorkflowAttemptRef {
        run: run.id.clone(),
        seq: 1,
        role: WorkflowAttemptRole::Step,
    });
    step.automation = Some(Automation::Workflow {
        workflow: "review".into(),
        step: "pr".into(),
        depth: Some(2),
    });
    let mut peer = crate::testkit::agent_state("codex", "peer", at);
    peer.automation = step.automation.clone();
    let mut older = peer.clone();
    older.automation = Some(Automation::Workflow {
        workflow: "review".into(),
        step: "pr".into(),
        depth: None,
    });
    let agent = |agent: &crate::agents::AgentState| RunStarter::Agent {
        kind: agent.kind.clone(),
        launch: agent
            .launch_id
            .clone()
            .unwrap_or_else(|| agent.agent_id.clone()),
    };
    let from_run = RunStarter::Run {
        run: run.id.clone(),
        node: "pr".into(),
    };
    // A kept step agent: its detach cleared the pin, its automation stamp and variables stay.
    let mut kept = step.clone();
    kept.workflow = None;
    let mut ended = run.clone();
    ended.state = RunState::Done;
    let cases = [
        (starting(Some(&inline), Some(&step), false), None, 1),
        (
            starting(Some(&run), Some(&step), false),
            Some(agent(&step)),
            3,
        ),
        (
            starting(Some(&run), Some(&kept), false),
            Some(agent(&kept)),
            3,
        ),
        (
            starting(Some(&ended), Some(&kept), false),
            Some(agent(&kept)),
            3,
        ),
        (
            starting(Some(&run), Some(&outside), false),
            Some(agent(&outside)),
            1,
        ),
        (starting(Some(&run), None, false), Some(from_run.clone()), 3),
        // A command step's child names the run, whatever agent its variables resolve to.
        (
            starting(Some(&run), Some(&step), true),
            Some(from_run.clone()),
            3,
        ),
        (
            starting(Some(&ended), Some(&kept), true),
            Some(from_run.clone()),
            3,
        ),
        (starting(Some(&run), None, true), Some(from_run), 3),
        (starting(None, Some(&kept), true), None, 1),
        (starting(None, Some(&peer), false), Some(agent(&peer)), 3),
        (starting(None, Some(&older), false), Some(agent(&older)), 2),
        (
            starting(None, Some(&outside), false),
            Some(agent(&outside)),
            1,
        ),
        (starting(None, None, false), None, 1),
    ];
    for (index, (got, starter, depth)) in cases.into_iter().enumerate() {
        assert_eq!((index, got.starter, got.depth), (index, starter, depth));
    }
}

#[test]
fn a_start_past_the_depth_limit_refuses_naming_the_depth_limit_and_key() {
    let at_limit = Starting {
        starter: None,
        depth: 3,
    };
    assert!(at_limit.admit(3).is_ok());
    let past = Starting {
        starter: None,
        depth: 4,
    };
    let error = past.admit(3).unwrap_err().to_string();
    assert!(
        error.contains("workflow depth 4")
            && error.contains("max-workflow-depth` of 3")
            && error.contains("nothing was created"),
        "{error}"
    );
}

#[test]
fn only_an_agent_starter_without_detach_or_a_runs_step_is_owed_a_report() {
    use crate::store::workflow::{ReportMark, RunStarter};
    let agent = RunStarter::Agent {
        kind: crate::ids::AgentKind::new_unchecked("claude"),
        launch: "launch-1".into(),
    };
    let from_run = RunStarter::Run {
        run: WorkflowRunId::new(),
        node: "pr".into(),
    };
    let cases = [
        (Some(agent.clone()), false, false, ReportMark::Owed),
        (Some(agent), true, false, ReportMark::None),
        (Some(from_run.clone()), false, false, ReportMark::None),
        (Some(from_run), false, true, ReportMark::Owed),
        (None, false, false, ReportMark::None),
    ];
    for (starter, detach, join, mark) in cases {
        let (home, state) = bare_room();
        open_room(&state, home.path());
        let dir = home.path().join("nightly");
        fs::create_dir_all(dir.join("prompts")).unwrap();
        fs::write(crate::disk::paths::workflow_spec_file(&dir), ONE_TURN).unwrap();
        fs::write(dir.join("prompts/fix.md"), "fix the build").unwrap();
        let (record, _) = create(
            &state,
            deployed(&dir),
            ScopeRoots {
                checkout: home.path().into(),
                project: home.path().into(),
            },
            || Ok(None),
            false,
            StartArgs {
                starting: Starting {
                    starter: starter.clone(),
                    depth: 2,
                },
                detach,
                join,
                ..Default::default()
            },
        )
        .unwrap();
        let stored = workflow::load(&state.workflow(&record.id)).unwrap();
        assert_eq!(
            (&stored.starter, stored.depth, stored.report),
            (&starter, 2, mark),
            "detach {detach}, join {join}"
        );
    }
}
