//! Loop task rows compiled by `harness::schedule::compile`, validated and run as workflows.

use super::*;
use crate::config::{MachineConfig, TaskEntry};
use crate::disk::paths::StatePaths;
use crate::harness::schedule::catalog::{LoadedTask, TaskSource};
use crate::harness::schedule::compile::{CompiledAction, InlineTask, TaskWorkflow};
use crate::store::workflow::{self as records, JournalEvent, RunRecord, RunState};
use serde_json::{Value, json};

fn loaded(name: &str, row: &str, root: &std::path::Path) -> LoadedTask {
    let mut entry: TaskEntry = toml::from_str(row).unwrap();
    entry.root = root.into();
    LoadedTask::new(name, entry, TaskSource::Config)
}

fn inline(task: &LoadedTask) -> &InlineTask {
    match task.compiled().unwrap() {
        CompiledAction::Workflow(TaskWorkflow::Inline(inline)) => inline,
        other => panic!("expected an inline workflow: {other:?}"),
    }
}

fn definition(name: &str, task: &LoadedTask) -> InlineDefinition {
    inline(task)
        .definition(name, task, &MachineConfig::default())
        .unwrap()
}

/// The materialized definition's validation, written where `workflow validate` reads one.
fn report(row: &str) -> (Validation, WorkflowSpec) {
    let home = tempfile::tempdir().unwrap();
    let task = loaded("nightly", row, home.path());
    let definition = definition("nightly", &task);
    let dir = home.path().join("nightly");
    for (name, body) in &definition.assets {
        let path = dir.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("workflow.toml"), &definition.text).unwrap();
    let spec = spec::parse(&definition.text).unwrap();
    (validate(&dir, &LoadedDefinitions::default(), None), spec)
}

fn errors(report: &Validation) -> Vec<(String, String)> {
    report
        .errors
        .iter()
        .map(|issue| (issue.path.clone(), issue.message.clone()))
        .collect()
}

fn route(spec: &WorkflowSpec, node: &str, outcome: &str) -> String {
    spec.target(node, outcome)
        .unwrap_or_else(|| panic!("{node}.{outcome} has no route"))
}

#[test]
fn every_stored_shape_validates_without_a_reachable_user() {
    let shapes = [
        "agent='claude'\nprompt='fix it'\nevery='1h'\n",
        "agent='claude'\nprompt='fix it'\nevery='1h'\nverify='true'\nmax-attempts=2\ntimeout='20m'\n",
        "agent='claude'\nprompt='fix it'\nevery='1h'\ncheck='true'\non='fail'\n",
        "agent='claude'\nprompt='fix it'\nevery='1h'\ncheck='true'\non='success'\njudge=[{agent='claude',prompt='ready?'},{agent='claude',prompt='still?',timeout='2m'}]\n",
        "agent='claude'\nprompt='fix it'\nevery='1h'\njudge={agent='claude',prompt='ready?'}\non='any'\n",
        "check='true'\nevery='1h'\ncheck-timeout='30s'\n",
        "wait={kind='claude',session='s-1',handle='@coder'}\nprompt='go'\ncheck='true'\non='success'\nsignal='ci.passed'\n",
        "agent='claude'\nprompt='fix it'\nevery='Mon,Tue,Wed,Thu,Fri'\nat='09:00'\ndeadline='2030-01-01T00:00:00Z'\ncheck='true'\non='success'\n",
        "agent='claude'\nprompt='settle'\nstay=true\neach-worktree=true\ntakeover=true\nworktree='review'\nwhen=['ci=failed']\nfor='10m'\nsubscribe=[{signal='ci.failed'},{signal='pr.merged',match={path='.'}}]\ncheck='true'\non='success'\njudge={agent='claude',prompt='ready?',recheck='1h'}\n",
        "agent='claude'\nprompt='settle'\nstay=true\neach-worktree=true\nwhen=['ci=failed']\njudge={agent='claude',prompt='ready?'}\non='success'\n",
        "agent='claude'\nprompt='settle'\nstay=true\nevery='1h'\njudge={agent='claude',prompt='ready?',recheck='30m'}\non='success'\n",
        "agent='claude'\nprompt='settle'\nstay=true\nevery='1h'\njudge={agent='claude',prompt='ready?',recheck='0'}\non='success'\n",
        "agent='claude'\nprompt='settle'\nstay=true\nevery='1h'\njudge={agent='claude',prompt='ready?'}\non='success'\n",
        "agent='claude'\nprompt='fix it'\nevery='1h'\nmode='yolo'\neffort='high'\nfast=true\nbudget='$2'\naccount='work'\nthrottle='off'\ncapacity-wait='10m'\nnotes=false\n",
    ];
    for row in shapes {
        let (report, _) = report(row);
        assert_eq!(errors(&report), Vec::<(String, String)>::new(), "{row}");
        let user = report
            .rows
            .iter()
            .flat_map(|node| node.edges.iter().map(move |edge| (&node.node, edge)))
            .filter(|(_, edge)| edge.target == "@user")
            .collect::<Vec<_>>();
        assert!(user.is_empty(), "{row}: {user:?}");
    }
}

#[test]
fn polarity_picks_which_verdict_acts_and_check_only_ignores_it() {
    for (on, pass, fail) in [
        ("fail", "@done", "agent"),
        ("success", "agent", "@done"),
        ("any", "agent", "agent"),
    ] {
        for guard in ["check='true'", "judge={agent='claude',prompt='ready?'}"] {
            let row = format!("agent='claude'\nprompt='go'\nevery='1h'\n{guard}\non='{on}'\n");
            let (report, spec) = report(&row);
            assert_eq!(errors(&report), Vec::<(String, String)>::new(), "{row}");
            let node = &spec.workflow.start;
            assert_eq!(
                (route(&spec, node, "pass"), route(&spec, node, "fail")),
                (pass.to_owned(), fail.to_owned()),
                "{row}"
            );
        }
        let row = format!("check='true'\nevery='1h'\non='{on}'\n");
        let (_, spec) = report(&row);
        assert_eq!(
            (route(&spec, "check", "pass"), route(&spec, "check", "fail")),
            ("@done".to_owned(), "@failed".to_owned()),
            "{row}"
        );
    }
}

#[test]
fn compiled_graphs_carry_each_shape_as_r1_describes() {
    let (_, spec) = report(
        "agent='claude'\nprompt='go'\nevery='1h'\ncheck='true'\ncheck-timeout='30s'\non='fail'\njudge=[{agent='claude',prompt='a'},{agent='claude',prompt='b'}]\n",
    );
    assert_eq!(spec.workflow.start, "check");
    assert_eq!(spec.checks["check"].timeout.as_deref(), Some("30s"));
    assert_eq!(
        (
            route(&spec, "check", "fail"),
            route(&spec, "judge-1", "fail"),
            route(&spec, "judge-2", "fail"),
            route(&spec, "check", "timeout"),
        ),
        (
            "judge-1".into(),
            "judge-2".into(),
            "agent".into(),
            "@failed".into()
        )
    );
    for (judge, next) in [("judge-1", "judge-2"), ("judge-2", "agent")] {
        for outcome in ["invalid", "timeout"] {
            let edge = spec
                .resolved_edges(judge)
                .unwrap()
                .into_iter()
                .find(|edge| edge.outcome == outcome)
                .unwrap();
            assert_eq!(
                (edge.target.as_str(), edge.source),
                (next, EdgeSource::Default)
            );
        }
    }
    let agent = &spec.steps["agent"];
    assert_eq!(
        (
            agent.lifecycle(),
            agent.then.as_slice(),
            agent.on.failed.as_deref()
        ),
        (
            crate::store::workflow::StepLifecycle::Turn,
            &["@done".to_owned()][..],
            Some("@failed")
        )
    );
    assert_eq!(
        agent.timeout.as_deref(),
        Some("7200s"),
        "loop's default timeout"
    );

    let (_, spec) = report("agent='claude'\nprompt='go'\nevery='1h'\nverify='make test'\n");
    let agent = &spec.steps["agent"];
    assert_eq!(
        (agent.lifecycle(), agent.max_visits, agent.on.cap.as_deref()),
        (
            crate::store::workflow::StepLifecycle::Until,
            Some(3),
            Some("@failed")
        )
    );
    assert_eq!(
        (
            route(&spec, "verify", "pass"),
            route(&spec, "verify", "fail")
        ),
        ("@done".into(), "agent".into())
    );

    let (_, spec) = report(
        "wait={kind='claude',session='s-1',handle='coder'}\nprompt='go'\ncheck='true'\non='fail'\nsignal='ci.passed'\n",
    );
    let deliver = &spec.steps["deliver"];
    assert_eq!(
        (
            deliver.message.as_deref(),
            deliver.session.as_ref().map(|session| session.as_str()),
            deliver.on.undelivered.as_deref(),
        ),
        (Some("@coder"), Some("s-1"), Some("@failed"))
    );

    let (_, spec) = report(
        "agent='claude'\nprompt='settle'\nstay=true\neach-worktree=true\nwhen=['ci=failed']\nfor='5m'\njudge={agent='claude',prompt='ready?',recheck='1h'}\non='success'\n",
    );
    assert_eq!(
        (
            route(&spec, "judge-1", "fail"),
            route(&spec, "held-judge-1", "holds"),
            route(&spec, "held-judge-1", "ended"),
            route(&spec, "held-judge-1", "timeout"),
            route(&spec, "rearm", "holds"),
            route(&spec, "rearm", "ended"),
        ),
        (
            "held-judge-1".into(),
            "@wait".into(),
            "rearm".into(),
            "judge-1".into(),
            "judge-1".into(),
            "@wait".into()
        )
    );
    let stay = &spec.steps["agent"];
    assert_eq!(
        (stay.lifecycle(), stay.timeout.as_deref()),
        (crate::store::workflow::StepLifecycle::Stay, None),
        "a resident takes no default timeout"
    );

    let (_, spec) = report(
        "agent='claude'\nprompt='go'\nevery='Mon,Fri'\nat='09:00'\ndeadline='2030-01-01T00:00:00Z'\ncheck='true'\non='success'\n",
    );
    assert_eq!(
        (
            route(&spec, "check", "fail"),
            spec.workflow.deadline.as_deref()
        ),
        ("@done".into(), None),
        "a declined poll ends its run and the graph carries no deadline"
    );
}

#[test]
fn unguarded_waits_deliver_and_workflow_rows_name_their_definition() {
    let home = tempfile::tempdir().unwrap();
    let wait = loaded(
        "ping",
        "wait={kind='claude',session='s-1',handle='@coder'}\nprompt='go'\nsignal='ci.passed'\n",
        home.path(),
    );
    assert!(
        matches!(wait.compiled(), Ok(CompiledAction::Deliver(target)) if target.handle == "@coder")
    );
    let named = loaded("tidy", "workflow='babysit'\nevery='1h'\n", home.path());
    assert_eq!(
        named.compiled().unwrap(),
        &CompiledAction::Workflow(TaskWorkflow::Named("babysit".into()))
    );
    let broken = loaded(
        "broken",
        "agent='claude'\nwait={kind='claude',session='s',handle='@c'}\n",
        home.path(),
    );
    assert_eq!(broken.compiled().unwrap_err(), broken.action().unwrap_err());
}

#[test]
fn assets_are_read_when_a_fire_materializes_the_definition() {
    let home = tempfile::tempdir().unwrap();
    let prompt = home.path().join("prompt.md");
    let system = home.path().join("system.md");
    let judge = home.path().join("judge.md");
    let row = format!(
        "agent='claude'\nprompt-file='{}'\nsystem-prompt-file='{}'\nevery='1h'\non='fail'\njudge={{agent='claude',prompt-file='{}'}}\ntimeout='15m'\n",
        prompt.display(),
        system.display(),
        judge.display()
    );
    let task = loaded("nightly", &row, home.path());
    let compiled = inline(&task).clone();
    for (path, body) in [
        (&prompt, "first"),
        (&system, "be brief"),
        (&judge, "ready?"),
    ] {
        fs::write(path, body).unwrap();
    }
    let first = compiled
        .definition("nightly", &task, &MachineConfig::default())
        .unwrap();
    fs::write(&prompt, "second").unwrap();
    let second = compiled
        .definition("nightly", &task, &MachineConfig::default())
        .unwrap();
    let asset = |definition: &InlineDefinition, name: &str| {
        String::from_utf8(definition.assets[name].clone()).unwrap()
    };
    assert_eq!(
        (
            asset(&first, "prompts/prompt.md"),
            asset(&second, "prompts/prompt.md"),
            asset(&first, "prompts/system.md"),
            asset(&first, "prompts/judge-1.md"),
        ),
        (
            "first".into(),
            "second".into(),
            "be brief".into(),
            "ready?".into()
        )
    );
    let spec = spec::parse(&first.text).unwrap();
    assert_eq!(spec.steps["agent"].timeout.as_deref(), Some("15m"));
    fs::remove_file(&judge).unwrap();
    assert!(
        compiled
            .definition("nightly", &task, &MachineConfig::default())
            .is_err()
    );
}

fn open_room(state: &StatePaths, root: &std::path::Path) {
    fs::create_dir_all(state.workspace_record.parent().unwrap()).unwrap();
    fs::write(&state.workspace_record, serde_json::to_vec(&json!({
        "layout":crate::disk::paths::WORKSPACE_LAYOUT, "workspace_id":crate::ids::WorkspaceId::from_project_root(root),
        "project_root":root, "session_name":"test", "updated_at":jiff::Timestamp::now()
    })).unwrap()).unwrap();
}

/// A task's fire: compile the row, materialize it, and start the run through `create`.
fn fire(row: &str) -> (tempfile::TempDir, StatePaths, RunRecord) {
    let home = tempfile::tempdir().unwrap();
    let state = StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
    open_room(&state, home.path());
    // Parallel tests share the machine's start queue; the caps still apply.
    let task = loaded("nightly", &format!("{row}throttle='off'\n"), home.path());
    let record = create(
        &state,
        DefinitionSource::Inline {
            task: "nightly".into(),
            definition: definition("nightly", &task),
        },
        ScopeRoots {
            checkout: home.path().into(),
            project: home.path().into(),
        },
        || Ok(None),
        false,
    )
    .unwrap();
    (home, state, record)
}

fn step(state: &StatePaths, record: &RunRecord, host: &FakeHost) -> RunRecord {
    advance(state, &record.id, host).unwrap();
    records::load(&state.workflow(&record.id)).unwrap()
}

fn routed(state: &StatePaths, record: &RunRecord) -> Vec<String> {
    records::journal(&state.workflow(&record.id))
        .unwrap()
        .into_iter()
        .filter_map(|row| match row.event {
            JournalEvent::Route { outcome, .. } => Some(outcome),
            _ => None,
        })
        .collect()
}

#[test]
fn a_plain_turn_fails_on_death_and_ends_done_on_a_clean_end() {
    for (status, expected) in [
        (RunStatus::Failed, RunState::Failed),
        (RunStatus::Completed, RunState::Done),
    ] {
        let (_home, state, record) = fire("agent='claude'\nprompt='fix it'\nevery='1h'\n");
        let host = FakeHost::new();
        let live = step(&state, &record, &host);
        let journal = records::journal(&state.workflow(&record.id)).unwrap();
        assert_eq!(host.launched.borrow().len(), 1, "{live:?} {journal:?}");
        assert_eq!(host.launched.borrow()[0].prompt, "fix it");
        host.observed.borrow_mut().status = status;
        assert_eq!(step(&state, &record, &host).state, expected, "{status:?}");
    }
}

fn turn_end(host: &FakeHost, n: u64) {
    host.observed.borrow_mut().turn = Some(LeaderTurn {
        end: crate::store::workflow::TurnEnd {
            agent_id: "leader".into(),
            started_at: jiff::Timestamp::UNIX_EPOCH,
            ended_at: jiff::Timestamp::UNIX_EPOCH + std::time::Duration::from_secs(n),
        },
        at_rest: true,
    });
}

#[test]
fn verify_passes_reprompts_the_same_session_and_fails_at_the_third_attempt() {
    let (home, state, record) =
        fire("agent='claude'\nprompt='fix it'\nevery='1h'\nverify='test -e passed'\n");
    let host = FakeHost::new();
    let live = step(&state, &record, &host);
    for n in 1..=2 {
        turn_end(&host, n);
        let pushed = step(&state, &record, &host);
        assert_eq!(pushed.node, "agent");
        assert_eq!(
            pushed.attempt.as_ref().unwrap().run,
            live.attempt.as_ref().unwrap().run
        );
    }
    assert_eq!(
        (host.push_calls.borrow().len(), host.launched.borrow().len()),
        (2, 1)
    );
    turn_end(&host, 3);
    let capped = step(&state, &record, &host);
    assert_eq!(capped.state, RunState::Failed, "{capped:?}");
    assert!(routed(&state, &record).contains(&"cap".to_owned()));

    let (home2, state, record) =
        fire("agent='claude'\nprompt='fix it'\nevery='1h'\nverify='test -e passed'\n");
    fs::write(home2.path().join("passed"), "").unwrap();
    let host = FakeHost::new();
    step(&state, &record, &host);
    turn_end(&host, 1);
    assert_eq!(step(&state, &record, &host).state, RunState::Done);
    drop(home);
}

fn judge_at(node: &str) -> (crate::disk::paths::WorkflowPaths, RunRecord) {
    let state = StatePaths::for_project_root_under(
        std::path::Path::new("/scratch"),
        std::path::Path::new("/project"),
    )
    .unwrap();
    let record: RunRecord = serde_json::from_value(json!({
        "version":1,"id":crate::ids::WorkflowRunId::new(),"workflow":"nightly","spec_dir":"/spec","spec_digest":"test",
        "scope":{"kind":"worktree","path":"/project"},"project_root":"/project",
        "created_at":"2026-01-01T00:00:00Z","state":"active","node":node,"entered_at":"2026-01-01T00:00:00Z",
        "deadline_fired":false,"next_seq":2,"visits":{},"visit_grant":0,
        "attempt":{"kind":"judge","seq":1,"node":node,"started_at":"2026-01-01T00:00:00Z","host":{"pid":123,"start":null}}
    })).unwrap();
    (state.workflow(&record.id), record)
}

fn judged(
    spec: &WorkflowSpec,
    node: &str,
    results: &[(Result<Value, String>, &'static str)],
) -> String {
    let (paths, record) = judge_at(node);
    let mut t = super::super::state::Transition {
        now: record.entered_at,
        record,
        spec,
        paths: &paths,
        entries: Vec::new(),
    };
    let attempt = t.record.attempt.clone();
    for (object, failure) in results {
        t.record.attempt.clone_from(&attempt);
        t.finish_judge(
            1,
            super::super::script::Evaluation {
                object: object.clone(),
                failure,
                stderr_tail: None,
                limited: None,
                cost_usd: None,
            },
        )
        .unwrap();
    }
    if t.record.state == RunState::Active {
        t.record.node
    } else {
        format!("{:?}", t.record.state)
    }
}

#[test]
fn a_judge_chain_continues_on_fail_declines_on_pass_and_runs_on_after_a_timeout() {
    let (_, spec) = report(
        "agent='claude'\nprompt='go'\nevery='1h'\non='fail'\njudge=[{agent='claude',prompt='a'},{agent='claude',prompt='b'}]\n",
    );
    let verdict = |outcome: &str| (Ok(json!({"outcome":outcome,"reason":"read it"})), "invalid");
    assert_eq!(judged(&spec, "judge-1", &[verdict("fail")]), "judge-2");
    assert_eq!(judged(&spec, "judge-2", &[verdict("pass")]), "Done");
    let timeout = (Err("judge timed out".to_owned()), "timeout");
    assert_eq!(
        judged(&spec, "judge-1", &[timeout]),
        "judge-2",
        "a tier with no answer continues to the next tier"
    );
}

#[test]
fn a_resident_launches_at_its_destination_and_a_wait_delivers_to_its_pinned_session() {
    let (_home, state, record) = fire(
        "agent='claude'\nprompt='settle'\nstay=true\nevery='1h'\nworktree='review'\ntakeover=true\n",
    );
    let host = FakeHost::new();
    step(&state, &record, &host);
    let launch = &host.launched.borrow()[0];
    assert_eq!(
        (launch.lifecycle, launch.overrides.worktree.as_deref()),
        (crate::store::workflow::StepLifecycle::Stay, Some("review"))
    );

    for (answer, expected) in [
        (Ok("@coder".to_owned()), RunState::Done),
        (
            Err("@coder is now another session".to_owned()),
            RunState::Failed,
        ),
    ] {
        let (_home, state, record) = fire(
            "wait={kind='claude',session='s-1',handle='@coder'}\nprompt='go'\ncheck='true'\non='success'\nsignal='ci.passed'\n",
        );
        let host = FakeHost::new();
        *host.message_answer.borrow_mut() = Ok(answer);
        let mut current = step(&state, &record, &host);
        while current.state == RunState::Active && current.node != "deliver" {
            current = step(&state, &record, &host);
        }
        if current.state == RunState::Active {
            current = step(&state, &record, &host);
        }
        assert_eq!(current.state, expected, "{current:?}");
        assert_eq!(
            host.message_sessions.borrow().as_slice(),
            &[Some(crate::ids::AgentSessionId::from("s-1"))]
        );
    }
}
