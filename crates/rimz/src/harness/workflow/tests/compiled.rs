//! Loop task rows compiled by `harness::schedule::compile`, validated and run as workflows.

use super::*;
use crate::config::{MachineConfig, TaskEntry};
use crate::disk::paths::StatePaths;
use crate::harness::schedule::catalog::{LoadedTask, TaskSource};
use crate::harness::schedule::compile::{CompiledAction, InlineTask, TaskWorkflow};
use crate::harness::schedule::run_log::LoopRunResult;
use crate::harness::schedule::signal::Signal;
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

    let (_, spec) = report("agent='claude'\nprompt='go'\nevery='1h'\nworktree='x'\n");
    assert_eq!(
        (spec.workflow.scope, spec.steps["agent"].worktree.as_deref()),
        (records::ScopeKind::Worktree, Some("x")),
        "a hand-written named tree is every fire's agent tree, under the row's checkout"
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
        "agent='claude'\nprompt='settle'\nstay=true\neach-worktree=true\nwhen=['ci=failed']\nfor='5m'\njudge={agent='claude',prompt='ready?',recheck='1h'}\non='success'\nsubscribe=[{signal='ci.failed',prompt='repair'}]\n",
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
    assert_eq!(
        stay.subscribe
            .iter()
            .flatten()
            .map(crate::harness::workflow::spec::Wake::prompt)
            .collect::<Vec<_>>(),
        [Some("repair")],
        "a binding's prompt reaches the step"
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
    let (state, record) = fire_at(home.path(), row);
    (home, state, record)
}

fn fire_at(home: &std::path::Path, row: &str) -> (StatePaths, RunRecord) {
    let state = StatePaths::for_project_root_under(home, home).unwrap();
    open_room(&state, home);
    // Parallel tests share the machine's start queue; the caps still apply.
    let task = loaded("nightly", &format!("{row}throttle='off'\n"), home);
    let record = create(
        &state,
        DefinitionSource::Inline {
            task: "nightly".into(),
            definition: definition("nightly", &task),
            fire_tree: false,
            capacity_until: None,
        },
        ScopeRoots {
            checkout: home.into(),
            project: home.into(),
        },
        || Ok(None),
        false,
    )
    .unwrap();
    (state, record)
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

#[test]
fn a_watchdog_reports_each_outcome_to_its_task_once() {
    let row = "agent='claude'\nprompt='fix it'\nevery='1h'\nverify='test -e passed'\n";
    let home = tempfile::tempdir().unwrap();
    fs::create_dir_all(home.path().join(".rimz")).unwrap();
    fs::write(
        home.path().join(".rimz/config.toml"),
        format!("[tasks.nightly]\n{row}"),
    )
    .unwrap();
    let catalog = crate::harness::schedule::catalog::TaskCatalog::load_lenient(Some(home.path()));
    let key = catalog.for_run("nightly").unwrap().key("nightly");
    let strikes = || crate::harness::schedule::strikes::load().get(&key).copied();
    let reports = || {
        crate::harness::schedule::run_log::task_records(
            &crate::disk::paths::logs_dir(),
            "nightly",
            Some(home.path()),
        )
    };

    let (state, failing) = fire_at(home.path(), row);
    assert_eq!(failing.loop_report, records::LoopReport::Owed);
    let host = FakeHost::new();
    step(&state, &failing, &host);
    for n in 1..=3 {
        turn_end(&host, n);
        step(&state, &failing, &host);
    }
    let failed = records::load(&state.workflow(&failing.id)).unwrap();
    assert_eq!(
        (failed.state, failed.loop_report),
        (RunState::Failed, records::LoopReport::Sent)
    );
    assert_eq!(strikes(), Some(1));
    // A crash between the report and its mark: the backstop's advance reports again.
    let mut owed = failed.clone();
    owed.loop_report = records::LoopReport::Owed;
    let guard = crate::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    records::write(&state.workflow(&failing.id), &owed, &guard).unwrap();
    drop(guard);
    let resent = step(&state, &failing, &host);
    assert_eq!(resent.loop_report, records::LoopReport::Sent);
    assert_eq!(strikes(), Some(1), "a run reported twice strikes once");
    let rows = reports();
    let failed_id = failing.id.to_string();
    assert_eq!(
        rows.iter()
            .map(|row| (row.result, row.run_id.as_deref(), row.acted))
            .collect::<Vec<_>>(),
        [(LoopRunResult::Failed, Some(failed_id.as_str()), Some(true))]
    );

    fs::write(home.path().join("passed"), "").unwrap();
    let (state, passing) = fire_at(home.path(), row);
    let host = FakeHost::new();
    step(&state, &passing, &host);
    turn_end(&host, 1);
    assert_eq!(step(&state, &passing, &host).state, RunState::Done);
    assert_eq!(strikes(), None, "a watchdog whose verify passes resets");
    assert_eq!(
        reports().last().map(|row| row.result),
        Some(LoopRunResult::Completed)
    );
}

#[test]
fn a_limited_attempt_ends_the_run_and_reports_as_the_budget_ended_it() {
    for verify in ["", "verify='true'\n"] {
        let row = format!("agent='claude'\nprompt='fix it'\nevery='1h'\n{verify}");
        let home = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join(".rimz")).unwrap();
        fs::write(
            home.path().join(".rimz/config.toml"),
            format!("[tasks.nightly]\n{row}"),
        )
        .unwrap();
        let catalog =
            crate::harness::schedule::catalog::TaskCatalog::load_lenient(Some(home.path()));
        let key = catalog.for_run("nightly").unwrap().key("nightly");
        let strikes = || crate::harness::schedule::strikes::load().get(&key).copied();
        let last = || {
            crate::harness::schedule::run_log::task_records(
                &crate::disk::paths::logs_dir(),
                "nightly",
                Some(home.path()),
            )
            .pop()
            .unwrap()
        };

        // A budget-refused launch runs no turn: the run fails without a strike.
        let (state, refused) = fire_at(home.path(), &row);
        let host = FakeHost::new();
        *host.failure.borrow_mut() = Some(LaunchFailure {
            outcome: AttemptOutcome::Limited,
            reason: "fleet budget $0.00/day is spent".into(),
        });
        let ended = step(&state, &refused, &host);
        assert_eq!(ended.state, RunState::Failed, "{verify}: {ended:?}");
        assert!(!ended.visits.contains_key("verify"), "{verify}: {ended:?}");
        let report = last();
        assert_eq!(
            (report.result, report.error.as_deref(), strikes()),
            (
                LoopRunResult::BudgetSkipped,
                Some("fleet budget $0.00/day is spent"),
                None
            ),
            "{verify}: {report:?}"
        );

        // A turn the budget stopped mid-run strikes, as a budget-exceeded run did.
        let (state, exceeded) = fire_at(home.path(), &row);
        let host = FakeHost::new();
        step(&state, &exceeded, &host);
        host.observed.borrow_mut().status = RunStatus::BudgetExceeded;
        let ended = step(&state, &exceeded, &host);
        assert_eq!(ended.state, RunState::Failed, "{verify}: {ended:?}");
        assert!(!ended.visits.contains_key("verify"), "{verify}: {ended:?}");
        assert_eq!(
            (last().result, strikes()),
            (LoopRunResult::BudgetExceeded, Some(1)),
            "{verify}"
        );
    }
}

#[test]
fn a_task_run_that_ran_only_a_command_step_reports_that_it_never_acted() {
    let home = tempfile::tempdir().unwrap();
    let state = StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
    open_room(&state, home.path());
    let text = "[workflow]\nscope='worktree'\nstart='build'\n[steps.build]\nshell='true'\n\
                timeout='1h'\nthen='done'\n[checks.done]\nshell='true'\n\
                on={pass='@done',fail='@failed',invalid='@failed'}\n";
    let record = create(
        &state,
        DefinitionSource::Inline {
            task: "nightly".into(),
            definition: InlineDefinition {
                text: text.into(),
                assets: Default::default(),
            },
            fire_tree: false,
            capacity_until: None,
        },
        ScopeRoots {
            checkout: home.path().into(),
            project: home.path().into(),
        },
        || Ok(None),
        false,
    )
    .unwrap();
    let host = FakeHost::new();
    let mut current = step(&state, &record, &host);
    for _ in 0..400 {
        if current.state != RunState::Active {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
        current = step(&state, &record, &host);
    }
    assert_eq!(current.state, RunState::Done, "{current:?}");
    assert!(current.visits.contains_key("build"), "{current:?}");
    let rows = crate::harness::schedule::run_log::task_records(
        &crate::disk::paths::logs_dir(),
        "nightly",
        Some(home.path()),
    );
    let id = record.id.to_string();
    assert_eq!(
        rows.iter()
            .map(|row| (row.result, row.run_id.as_deref(), row.acted))
            .collect::<Vec<_>>(),
        [(LoopRunResult::Completed, Some(id.as_str()), Some(false))],
        "a visited command step is not an act"
    );
    // A launcher that remembers the run only after this report keeps no memory of it.
    let launch = crate::harness::schedule::launch_ledger::WorkflowLaunchRecord {
        at: record.created_at,
        run: Some(record.id.clone()),
        scope: record.scope.kind,
        leader: None,
        destination: None,
    };
    let ledger =
        json!({"version": 2, "tasks": {"nightly": {home.path().to_str().unwrap(): launch}}});
    // The report forgets in the project's own store, as it does in production.
    let ledger_state = StatePaths::for_project_root(home.path()).unwrap();
    let file = crate::StatePaths::class_path(
        &ledger_state.root,
        crate::disk::paths::Class::Records,
        "loop-workflow-launches.json",
    );
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, ledger.to_string()).unwrap();
    crate::harness::workflow::forget_reported_launch(&state, &record.id).unwrap();
    let remembered =
        crate::harness::schedule::launch_ledger::load_workflows(&ledger_state).unwrap();
    assert!(
        !remembered.tasks.contains_key("nightly"),
        "an unacted run's late memory is forgotten: {remembered:?}"
    );
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
fn a_budget_refused_judge_after_a_shell_stage_fails_the_run_as_budget_skipped() {
    let row = "agent='claude'\nprompt='go'\nevery='1h'\ncheck='printf shell-evidence; exit 1'\n\
               on='fail'\njudge=[{agent='codex',prompt='Is there work?'}]\n";
    let home = tempfile::tempdir().unwrap();
    fs::create_dir_all(home.path().join(".rimz")).unwrap();
    fs::write(
        home.path().join(".rimz/config.toml"),
        format!("[tasks.nightly]\n{row}"),
    )
    .unwrap();
    let (state, record) = fire_at(home.path(), row);
    let (_, spec) = report(row);
    assert_eq!(route(&spec, "check", "fail"), "judge-1");
    let mut record = serde_json::to_value(record).unwrap();
    record["node"] = json!("judge-1");
    record["next_seq"] = json!(2);
    record["attempt"] = json!({"kind":"judge","seq":1,"node":"judge-1",
        "started_at":"2026-01-01T00:00:00Z","host":{"pid":123,"start":null}});
    let record: RunRecord = serde_json::from_value(record).unwrap();
    let paths = state.workflow(&record.id);
    let mut t = super::super::state::Transition {
        now: record.entered_at,
        record,
        spec: &spec,
        paths: &paths,
        entries: Vec::new(),
    };
    let reason = "account codex:default budget $10.00/day is spent";
    t.finish_judge(
        1,
        super::super::script::Evaluation {
            object: Err(reason.into()),
            failure: "budget",
            stderr_tail: None,
            limited: None,
            cost_usd: None,
        },
    )
    .unwrap();
    assert_eq!(
        (t.record.state, t.record.recheck_at),
        (RunState::Failed, None),
        "a refused judge ends the run instead of waiting or routing to the act"
    );
    assert_eq!(route(&spec, "judge-1", "budget"), "@failed");
    crate::harness::schedule::run_log::report_run(&t.record, &t.entries, false);
    let last = crate::harness::schedule::run_log::task_records(
        &crate::disk::paths::logs_dir(),
        "nightly",
        Some(home.path()),
    )
    .pop()
    .unwrap();
    assert_eq!(
        (last.result, last.error.as_deref()),
        (LoopRunResult::BudgetSkipped, Some(reason)),
        "{last:?}"
    );
}

#[test]
fn a_judges_account_over_its_day_cap_or_parked_is_refused_before_it_spawns() {
    use crate::agents::spending::{
        ProviderSpendingCache, SpendWindow, write_provider_spending_cache,
    };
    use crate::harness::budget::{BudgetParkStamp, DailyBudgetLedger, DailyBudgetScope};
    let now: jiff::Timestamp = "2026-06-02T12:00:00Z".parse().unwrap();
    for parked in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let state = StatePaths::for_project_root(root.path()).unwrap();
        let runtime = crate::disk::paths::RuntimePaths::for_state(&state).unwrap();
        runtime.ensure_dirs().unwrap();
        let config = MachineConfig {
            timezone: Some("UTC".into()),
            accounts: toml::from_str("[budget]\ncodex = '10/day'\n").unwrap(),
            ..Default::default()
        };
        let profile = crate::harness::headless::HeadlessProfile::resolve(
            &config,
            "codex",
            root.path(),
            &state,
            &runtime,
            now,
        )
        .unwrap();
        let key = crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("codex"));
        write_provider_spending_cache(
            &runtime.shared_provider_spending_path(),
            &ProviderSpendingCache {
                day_cutoff_secs: jiff::civil::date(2026, 6, 2)
                    .to_zoned(jiff::tz::TimeZone::UTC)
                    .unwrap()
                    .timestamp()
                    .as_second() as u64,
                day_by_login: std::collections::BTreeMap::from([(
                    key.clone(),
                    SpendWindow {
                        usd: if parked { 0.0 } else { 12.0 },
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            },
        );
        if parked {
            DailyBudgetScope::Account(key)
                .write_ledger(
                    &runtime,
                    &state,
                    &DailyBudgetLedger {
                        parked: Some(BudgetParkStamp {
                            at_cost: 12.0,
                            at: now,
                        }),
                        ..Default::default()
                    },
                )
                .unwrap();
        }
        let refusal = super::super::script::budget_refusal(
            &config,
            &profile,
            &crate::agents::ManagedLaunchState::Unsupported,
            None,
            &state,
            &runtime,
            now,
        );
        assert!(
            refusal
                .as_deref()
                .is_some_and(|reason| reason.contains("codex")),
            "parked={parked}: {refusal:?}"
        );
    }
}

#[test]
fn a_judge_on_a_spent_quota_binding_or_short_of_its_tasks_surplus_is_refused() {
    use crate::agents::account::AccountUsageKey;
    use crate::agents::{
        AgentRateLimits, ManagedLaunchState, ProviderAccountBinding, ProviderAccountScope,
        RateLimitCacheEntry, RateLimitWindow, RateLimitsCache,
    };
    let now = jiff::Timestamp::now();
    let root = tempfile::tempdir().unwrap();
    let state = StatePaths::for_project_root(root.path()).unwrap();
    let runtime = crate::disk::paths::RuntimePaths::for_state(&state).unwrap();
    runtime.ensure_dirs().unwrap();
    let config = MachineConfig::default();
    let profile = crate::harness::headless::HeadlessProfile::resolve(
        &config,
        "codex",
        root.path(),
        &state,
        &runtime,
        now,
    )
    .unwrap();
    let key = crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("codex"));
    let window = |used, resets_in: i64, mins| RateLimitWindow {
        used_percentage: Some(used),
        resets_at: Some(now + jiff::SignedDuration::from_secs(resets_in)),
        duration_mins: Some(mins),
        ..RateLimitWindow::default()
    };
    let scope = ProviderAccountScope::sub_provider("openai", "oauth");
    let binding = ProviderAccountBinding::decode(
        r#"{"scope":{"kind":"sub_provider","provider":"openai","variant":"oauth"},"account_key":"owner"}"#,
    )
    .unwrap();
    let week = 7 * 24 * 60;
    let entries = std::collections::BTreeMap::from([
        (
            AccountUsageKey::new(key.clone(), scope.clone()),
            RateLimitCacheEntry {
                scope,
                account_key: Some("owner".to_owned()),
                limits: AgentRateLimits {
                    windows: vec![window(100, 3_600, 300)],
                },
                ..Default::default()
            },
        ),
        (
            key.clone().into(),
            RateLimitCacheEntry {
                limits: AgentRateLimits {
                    windows: vec![window(90, i64::from(week) * 60 / 2, week)],
                },
                ..Default::default()
            },
        ),
    ]);
    crate::disk::atomic::write_temp_then_rename_cache(
        &RateLimitsCache::shared_path(&runtime),
        &RateLimitsCache {
            entries,
            ..Default::default()
        },
    )
    .unwrap();
    let refuse = |managed: &ManagedLaunchState, task| {
        super::super::script::budget_refusal(
            &config, &profile, managed, task, &state, &runtime, now,
        )
    };
    let quota = refuse(&ManagedLaunchState::Bound(binding), None);
    assert!(
        quota
            .as_deref()
            .is_some_and(|reason| reason.contains("exhausted")),
        "{quota:?}"
    );
    let gated = crate::config::TaskEntry {
        surplus: Some("1.5x".into()),
        ..Default::default()
    };
    let surplus = refuse(&ManagedLaunchState::Unsupported, Some(&gated));
    assert!(
        surplus
            .as_deref()
            .is_some_and(|reason| reason.contains("window surplus")),
        "{surplus:?}"
    );
    let ungated = crate::config::TaskEntry::default();
    assert_eq!(
        (
            refuse(&ManagedLaunchState::Unsupported, None),
            refuse(&ManagedLaunchState::Unsupported, Some(&ungated)),
        ),
        (None, None),
        "a kind-wide account has no bound quota, and surplus is read only for a task that sets it"
    );
}

#[test]
fn only_a_verdict_that_routes_away_from_the_act_is_a_decline() {
    let chain = "agent='claude'\nprompt='go'\nevery='1h'\non='fail'\njudge=[{agent='claude',prompt='a'},{agent='claude',prompt='b'}]\n";
    let (_, spec) = report(chain);
    let declines = |node, outcome| super::super::script::declines(&spec, node, outcome);
    assert!(declines("judge-1", "pass") && declines("judge-2", "pass"));
    assert!(!declines("judge-1", "fail") && !declines("judge-2", "fail"));
    assert!(!declines("judge-1", "unknown"));
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

#[test]
fn a_deduping_tasks_decline_waits_in_its_run_and_another_tasks_ends_it() {
    let guard = "check='true'\non='fail'\njudge={agent='claude',prompt='ready?',recheck='30m'}\n";
    for (row, expected) in [
        (
            format!("agent='claude'\nprompt='settle'\nstay=true\nevery='1h'\n{guard}"),
            "recheck-check",
        ),
        (
            format!("agent='claude'\nprompt='settle'\nstay=true\nwhen=['ci=failed']\n{guard}"),
            "held-check",
        ),
        (
            format!("agent='claude'\nprompt='fix it'\nevery='1h'\n{guard}"),
            "Done",
        ),
    ] {
        let (_home, state, record) = fire(&row);
        let host = FakeHost::new();
        let mut current = step(&state, &record, &host);
        for _ in 0..400 {
            if current.state != RunState::Active || current.node != "check" {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
            current = step(&state, &record, &host);
        }
        let at = if current.state == RunState::Active {
            current.node.clone()
        } else {
            format!("{:?}", current.state)
        };
        assert_eq!(at, expected, "{row}: {current:?}");
        assert!(host.launched.borrow().is_empty(), "{row}");
    }
}

/// Step until the run leaves `from` (or ends), as a shell check finishes off-thread.
fn settle(state: &StatePaths, record: &RunRecord, host: &FakeHost, from: &str) -> RunRecord {
    let mut current = step(state, record, host);
    for _ in 0..400 {
        if current.state != RunState::Active || current.node != from {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
        current = step(state, record, host);
    }
    current
}

/// Age the run's current node by `secs`, so its wait or hold times out at the next step; the
/// recheck time a sleep stamped from its entry is cleared, so the next step reads it again.
fn age(state: &StatePaths, current: &mut RunRecord, secs: u64) {
    current.entered_at = jiff::Timestamp::now() - std::time::Duration::from_secs(secs);
    current.recheck_at = None;
    super::attempts::update(state, current);
}

#[test]
fn a_shell_decline_reruns_the_shell_after_the_resident_retry_and_launches_once_it_passes() {
    for (cadence, wait) in [
        ("when=['ci=failed']\n", "held-check"),
        ("every='1h'\n", "recheck-check"),
    ] {
        let (home, state, record) = fire(&format!(
            "agent='claude'\nprompt='settle'\nstay=true\n{cadence}check='test -e ready'\non='success'\n"
        ));
        let host = FakeHost::new();
        let mut current = settle(&state, &record, &host, "check");
        assert_eq!(current.node, wait, "{cadence}: {current:?}");
        age(&state, &mut current, 299);
        assert_eq!(
            step(&state, &record, &host).node,
            wait,
            "{cadence}: the retry is not due yet"
        );
        fs::write(home.path().join("ready"), "").unwrap();
        let mut current = records::load(&state.workflow(&record.id)).unwrap();
        age(&state, &mut current, 301);
        let current = settle(&state, &record, &host, wait);
        let current = settle(&state, &record, &host, current.node.clone().as_str());
        assert_eq!(
            (current.node.as_str(), host.launched.borrow().len()),
            ("agent", 1),
            "{cadence}: the shell ran again, passed, and launched: {current:?}"
        );
    }
}

#[test]
fn a_judge_decline_waits_for_the_declining_judges_own_recheck() {
    let (_, spec) = report(
        "agent='claude'\nprompt='settle'\nstay=true\nwhen=['ci=failed']\ncheck='true'\non='success'\njudge=[{agent='claude',prompt='a',recheck='1h'},{agent='claude',prompt='b',recheck='10m'},{agent='claude',prompt='c'}]\n",
    );
    let wait = |node: &str| {
        let check = &spec.checks[node];
        (check.timeout.clone(), route(&spec, node, "timeout"))
    };
    assert_eq!(
        (
            route(&spec, "check", "fail"),
            route(&spec, "judge-1", "fail"),
            route(&spec, "judge-2", "fail"),
            route(&spec, "judge-3", "fail"),
        ),
        (
            "held-check".into(),
            "held-judge-1".into(),
            "held-judge-2".into(),
            "held-judge-3".into()
        )
    );
    assert_eq!(
        [
            wait("held-check"),
            wait("held-judge-1"),
            wait("held-judge-2"),
            wait("held-judge-3"),
        ],
        [
            (Some("300s".into()), "check".into()),
            (Some("1h".into()), "check".into()),
            (Some("10m".into()), "check".into()),
            (Some("365d".into()), "@done".into()),
        ],
        "each tier waits its own recheck; one without a recheck waits for the next edge"
    );
}

#[test]
fn a_clock_tasks_judge_decline_without_a_recheck_holds_its_run_untimed() {
    let row = |recheck: &str| {
        format!(
            "agent='claude'\nprompt='settle'\nstay=true\nevery='1h'\ncheck='true'\non='success'\njudge={{agent='claude',prompt='a'{recheck}}}\n"
        )
    };
    let (_, spec) = report(&row(""));
    assert_eq!(route(&spec, "judge-1", "fail"), "hold-judge-1");
    let hold = &spec.checks["hold-judge-1"];
    assert_eq!(
        (hold.sleep.as_deref(), hold.then.as_deref()),
        (Some("365d"), Some("@done")),
        "the live run is the hold: it never re-asks the judge"
    );
    let (_, spec) = report(&row(",recheck='0'"));
    assert_eq!(
        route(&spec, "judge-1", "fail"),
        "@done",
        "a zero recheck ends the run, so the next fire asks again"
    );
}

/// A trusted project whose store holds one `nightly` row; returns the store's text. Unit tests
/// share one RimZ home, so they never write the machine store.
fn project_task(root: &std::path::Path, row: &str) -> String {
    let text = format!("# project tasks\n\n[tasks.nightly]\n{row}");
    fs::create_dir_all(root.join(".rimz")).unwrap();
    fs::write(root.join(".rimz/config.toml"), &text).unwrap();
    crate::trust::grant(root).unwrap();
    text
}

fn project_catalog(root: &std::path::Path) -> crate::harness::schedule::catalog::TaskCatalog {
    crate::harness::schedule::catalog::TaskCatalog::load(Some(root)).unwrap()
}

/// A workflow name no other test in this run's shared home takes.
fn workflow_name() -> String {
    format!("ejected-{}", uuid::Uuid::now_v7().simple())
}

fn eject_nightly(root: &std::path::Path, workflow: &str) -> std::path::PathBuf {
    project_catalog(root)
        .eject(
            "nightly",
            workflow,
            &MachineConfig::default(),
            &LoadedDefinitions::default(),
        )
        .unwrap()
}

/// Every node with its resolved edges, in a comparable form.
fn graph(spec: &WorkflowSpec) -> Vec<(String, Value)> {
    spec.checks
        .keys()
        .chain(spec.steps.keys())
        .map(|node| {
            let edges = serde_json::to_value(spec.resolved_edges(node).unwrap()).unwrap();
            (node.clone(), edges)
        })
        .collect()
}

/// Every file under `dir`, by path relative to it.
fn files(dir: &std::path::Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut found = std::collections::BTreeMap::new();
    let mut pending = vec![dir.to_owned()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let name = path.strip_prefix(dir).unwrap().display().to_string();
                found.insert(name, fs::read(path).unwrap());
            }
        }
    }
    found
}

#[test]
fn an_ejected_task_keeps_its_graph_and_its_row_keeps_only_timing_and_admission() {
    let rows = [
        "agent='claude'\nprompt='fix it'\nevery='1h'\ncheck='true'\non='fail'\njudge=[{agent='claude',prompt='a'},{agent='claude',prompt='b',timeout='2m'}]\nbudget='$2'\nbudget-per-day='$8'\nmax-strikes=4\noverlap='queue'\noverlap-limit=3\n",
        "agent='claude'\nprompt='fix it'\nevery='1h'\nverify='make test'\nmax-attempts=2\ntimeout='20m'\nmode='yolo'\neffort='high'\nfast=true\naccount='work'\nthrottle='off'\ncapacity-wait='10m'\nnotes=false\nsurplus='1.5x'\n",
        "agent='claude'\nprompt='settle'\nworktree='review'\nsignal='ci.failed'\nmatch={branch='main'}\ncheck='true'\non='success'\njudge={agent='claude',prompt='ready?',recheck='1h'}\n",
        "check='true'\ncheck-timeout='30s'\ncron='0 * * * *'\n",
    ];
    for row in rows {
        let root = tempfile::tempdir().unwrap();
        project_task(root.path(), row);
        let catalog = project_catalog(root.path());
        let task = catalog.visible()["nightly"].clone();
        let before = task.entry().clone();
        let inline = definition("nightly", &task);
        let workflow = workflow_name();
        let dir = eject_nightly(root.path(), &workflow);
        assert_eq!(dir, crate::disk::paths::workflows_dir().join(&workflow));
        let ejected = load(&dir).unwrap();
        assert_eq!(
            graph(&ejected),
            graph(&spec::parse(&inline.text).unwrap()),
            "{row}"
        );
        let mut expected = inline.assets.clone();
        expected.insert(
            "workflow.toml".into(),
            fs::read(dir.join("workflow.toml")).unwrap(),
        );
        assert_eq!(files(&dir), expected, "{row}");

        let after = project_catalog(root.path()).visible()["nightly"].clone();
        assert_eq!(
            after.compiled().unwrap(),
            &CompiledAction::Workflow(TaskWorkflow::Named(workflow.clone())),
            "{row}"
        );
        let kept = TaskEntry {
            workflow: Some(workflow),
            root: before.root.clone(),
            dir: before.dir.clone(),
            label: before.label.clone(),
            each_worktree: before.each_worktree,
            max_strikes: before.max_strikes,
            budget_per_day: before.budget_per_day.clone(),
            surplus: before.surplus.clone(),
            surplus_after: before.surplus_after.clone(),
            every: before.every.clone(),
            cron: before.cron.clone(),
            signal: before.signal.clone(),
            matches: before.matches.clone(),
            overlap: before.overlap,
            overlap_limit: before.overlap_limit,
            once_per_checkout: Some(before.dedupes_checkout()),
            ..TaskEntry::default()
        };
        assert_eq!(after.entry(), &kept, "{row}");
        assert!(
            fs::read_to_string(root.path().join(".rimz/config.toml"))
                .unwrap()
                .starts_with("# project tasks\n"),
            "the store keeps what surrounds the row"
        );
    }
}

/// What one drive of a watchdog run shows: the node that re-prompts, the final state, each
/// launch's prompt, the push count, and the routes taken.
type Drive = (String, RunState, Vec<String>, usize, Vec<String>);

fn drive(state: &StatePaths, record: &RunRecord, checkout: &std::path::Path) -> Drive {
    let host = FakeHost::new();
    step(state, record, &host);
    turn_end(&host, 1);
    let pushed = step(state, record, &host);
    fs::write(checkout.join("passed"), "").unwrap();
    turn_end(&host, 2);
    let done = step(state, record, &host);
    let prompts = host
        .launched
        .borrow()
        .iter()
        .map(|launch| launch.prompt.clone())
        .collect::<Vec<_>>();
    (
        pushed.node,
        done.state,
        prompts,
        host.push_calls.borrow().len(),
        routed(state, record),
    )
}

/// A fire of `task` for `signal`, which composes the prompts its run starts with.
fn task_fire<'a>(
    task: &LoadedTask,
    catalog: &'a crate::harness::schedule::catalog::TaskCatalog,
    signal: Option<Signal>,
) -> crate::harness::schedule::runner::TaskFire<'a> {
    crate::harness::schedule::runner::TaskFire::new(
        "nightly",
        task.clone(),
        catalog,
        crate::harness::schedule::run_log::LoopRunMode::Scheduled,
        false,
        jiff::Timestamp::now(),
        std::sync::Arc::new(MachineConfig::default()),
        signal,
        std::time::Instant::now(),
    )
    .unwrap()
}

/// Row `row`'s inline graph and the workflow `dir` it was ejected into from `root`, each fired for `signal` and driven
/// through the same stubs.
fn inline_and_ejected(
    row: &str,
    root: &std::path::Path,
    dir: &std::path::Path,
    signal: Option<Signal>,
) -> (Drive, Drive) {
    let catalog = crate::harness::schedule::catalog::TaskCatalog::load(None).unwrap();
    let inline_home = tempfile::tempdir().unwrap();
    let state = StatePaths::for_project_root_under(inline_home.path(), inline_home.path()).unwrap();
    open_room(&state, inline_home.path());
    let task = loaded(
        "nightly",
        &format!("{row}throttle='off'\n"),
        inline_home.path(),
    );
    let record = create(
        &state,
        DefinitionSource::Inline {
            task: "nightly".into(),
            definition: task_fire(&task, &catalog, signal.clone())
                .inline_definition(inline(&task))
                .unwrap(),
            fire_tree: false,
            capacity_until: None,
        },
        ScopeRoots {
            checkout: inline_home.path().into(),
            project: inline_home.path().into(),
        },
        || Ok(None),
        false,
    )
    .unwrap();
    let inline = drive(&state, &record, inline_home.path());

    let ejected_home = tempfile::tempdir().unwrap();
    let state =
        StatePaths::for_project_root_under(ejected_home.path(), ejected_home.path()).unwrap();
    open_room(&state, ejected_home.path());
    let projects = project_catalog(root);
    let ejected = &projects.visible()["nightly"];
    let prompt = task_fire(ejected, &projects, signal).run_prompt().unwrap();
    let record = create(
        &state,
        DefinitionSource::Deployed {
            dir,
            task: Some("nightly".into()),
        },
        ScopeRoots {
            checkout: ejected_home.path().into(),
            project: ejected_home.path().into(),
        },
        || Ok(prompt),
        false,
    )
    .unwrap();
    (inline, drive(&state, &record, ejected_home.path()))
}

#[test]
fn an_ejected_watchdog_runs_like_its_inline_graph_and_a_rerun_finishes_a_crashed_eject() {
    let row = "agent='claude'\nprompt='fix it'\nevery='1h'\nverify='test -e passed'\n";
    let root = tempfile::tempdir().unwrap();
    let original = project_task(root.path(), &format!("{row}throttle='off'\n"));
    let workflow = workflow_name();
    let dir = eject_nightly(root.path(), &workflow);
    let published = files(&dir);
    // A crash after publishing and before the row rewrite: the store still holds the row.
    fs::write(root.path().join(".rimz/config.toml"), &original).unwrap();
    assert_eq!(
        eject_nightly(root.path(), &workflow),
        dir,
        "the rerun finishes the eject"
    );
    assert_eq!(
        files(&dir),
        published,
        "the rerun leaves the directory as published"
    );
    assert_eq!(
        project_catalog(root.path()).visible()["nightly"]
            .compiled()
            .unwrap(),
        &CompiledAction::Workflow(TaskWorkflow::Named(workflow))
    );
    let (inline, ejected) = inline_and_ejected(row, root.path(), &dir, None);
    assert_eq!(ejected, inline);
    assert_eq!(inline.1, RunState::Done, "{inline:?}");
}

#[test]
fn an_ejected_signal_task_still_hands_its_agent_the_signal_that_fired_it() {
    let row = "agent='claude'\nprompt='fix it'\nsignal='deploy.finished'\n\
               match={env='prod'}\nverify='test -e passed'\n";
    let root = tempfile::tempdir().unwrap();
    project_task(root.path(), &format!("{row}throttle='off'\n"));
    let dir = eject_nightly(root.path(), &workflow_name());
    let signal = Signal {
        name: "deploy.finished".parse().unwrap(),
        payload: json!({"env": "prod"}).as_object().unwrap().clone(),
        source: crate::store::event::SignalSource::Cli,
        watch: None,
    };
    let (inline, ejected) = inline_and_ejected(row, root.path(), &dir, Some(signal));
    let evidence = "Rule `nightly` fired here. deploy.finished · env: prod";
    assert_eq!(inline.2.len(), 1, "{inline:?}");
    assert!(
        inline.2[0].starts_with(&format!("{evidence}\n\nfix it")),
        "{inline:?}"
    );
    assert_eq!(ejected.2.len(), 1, "{ejected:?}");
    assert!(ejected.2[0].starts_with("fix it"), "{ejected:?}");
    assert!(
        ejected.2[0].ends_with(&format!("\n\n{evidence}")),
        "{ejected:?}"
    );
    let without_prompts = |(node, state, _, pushes, routes): Drive| (node, state, pushes, routes);
    assert_eq!(without_prompts(ejected), without_prompts(inline.clone()));
    assert_eq!(inline.1, RunState::Done, "{inline:?}");
}

#[test]
fn a_scheduled_recurring_fire_gives_up_on_capacity_before_its_next_fire() {
    let home = tempfile::tempdir().unwrap();
    let catalog = crate::harness::schedule::catalog::TaskCatalog::load(None).unwrap();
    let now: jiff::Timestamp = "2026-03-02T10:00:00Z".parse().unwrap();
    let cutoff = |row: &str, mode| {
        let task = loaded("nightly", row, home.path());
        crate::harness::schedule::runner::TaskFire::new(
            "nightly",
            task,
            &catalog,
            mode,
            false,
            now,
            std::sync::Arc::new(MachineConfig::default()),
            None,
            std::time::Instant::now(),
        )
        .unwrap()
        .default_capacity_cutoff()
        .unwrap()
        .map(|at| at.to_string())
    };
    use crate::harness::schedule::run_log::LoopRunMode::{Manual, Scheduled};
    let row = "agent='claude'\nprompt='fix it'\nevery='1h'\n";
    assert_eq!(
        cutoff(row, Scheduled).as_deref(),
        Some("2026-03-02T10:59:59Z"),
        "an unset wait gives up a second before the next fire"
    );
    assert_eq!(
        cutoff(
            &format!("{row}deadline='2026-03-02T10:30:00Z'\n"),
            Scheduled
        )
        .as_deref(),
        Some("2026-03-02T10:30:00Z"),
        "an earlier deadline bounds it"
    );
    assert_eq!(
        cutoff(&format!("{row}capacity-wait='forever'\n"), Scheduled),
        None,
        "a configured wait stands"
    );
    assert_eq!(cutoff(row, Manual), None, "a manual fire never waits");
    assert_eq!(
        cutoff(
            "agent='claude'\nprompt='fix it'\nsignal='deploy.finished'\n",
            Scheduled
        ),
        None,
        "a signal task waits as long as it takes"
    );
}
