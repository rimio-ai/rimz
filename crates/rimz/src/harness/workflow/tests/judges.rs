//! Judge checks: one agent in a pane, answering through `rimz workflow submit`.

use super::attempts::{setup, update};
use super::*;
use crate::agents::WorkflowAttemptRole;
use crate::disk::paths::StatePaths;
use crate::store::workflow::{
    self as records, AttemptKind, AttemptOutcome, EvaluationOrigin, JournalEvent, RunRecord,
    RunState, StepLifecycle,
};
use serde_json::{Map, Value, json};

const JUDGE: &str = "[workflow]\nscope='worktree'\nstart='review'\n\
    [judge.review]\nagent='claude'\nsummary='Reviews the change'\nprompt-file='prompt.md'\n\
    timeout='1h'\non={pass='@done',fail='fix'}\n\
    [agent.fix]\nagent='claude'\nlifecycle='turn'\nprompt-file='prompt.md'\nmax-visits=3\n\
    then='review'\n";

fn advanced(state: &StatePaths, record: &RunRecord, host: &FakeHost) -> RunRecord {
    advance(state, &record.id, host).unwrap();
    records::load(&state.workflow(&record.id)).unwrap()
}

fn answer(outcome: &str, extra: Value) -> Map<String, Value> {
    let Value::Object(mut object) = extra else {
        panic!("an answer is an object");
    };
    object.insert("outcome".into(), outcome.into());
    object
}

/// The judge's live attempt after its launch.
fn launched(state: &StatePaths, record: &RunRecord, host: &FakeHost) -> RunRecord {
    let live = advanced(state, record, host);
    let attempt = live.attempt.as_ref().expect("a live judge");
    assert_eq!(attempt.kind, AttemptKind::Judge);
    assert!(attempt.run.is_some(), "{live:#?}");
    live
}

fn evaluations(state: &StatePaths, record: &RunRecord) -> Vec<(EvaluationOrigin, String)> {
    records::journal(&state.workflow(&record.id))
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.event {
            JournalEvent::Evaluation {
                origin, outcome, ..
            } => Some((origin, outcome)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_judge_launches_one_pane_agent_whose_accepted_answer_routes_with_its_cost() {
    let (_dir, state, record) = setup(JUDGE);
    let host = FakeHost::new();
    let live = launched(&state, &record, &host);
    {
        let launches = host.launched.borrow();
        let [launch] = launches.as_slice() else {
            panic!("one launch: {}", launches.len());
        };
        assert_eq!(launch.workflow.role, WorkflowAttemptRole::Judge);
        assert_eq!(launch.lifecycle, StepLifecycle::Session);
        assert!(!launch.keep && !launch.takeover && launch.evict.is_none());
        assert!(
            launch
                .prompt
                .starts_with("Build it\n\nAnswer this check by running `rimz workflow submit"),
            "{}",
            launch.prompt
        );
        assert!(launch.prompt.contains("`pass`") && launch.prompt.contains("`fail`"));
    }
    assert!(live.visits.is_empty(), "a judge charges no visit");
    let seq = live.attempt.as_ref().unwrap().seq;
    // Nothing ends a judge before its answer: the turn ending alone does not.
    assert_eq!(advanced(&state, &record, &host).attempt, live.attempt);
    submit(
        &state,
        &record.id,
        seq,
        answer("pass", json!({"reason":"clean", "note":"kept"})),
        None,
    )
    .unwrap();
    let stored = records::load(&state.workflow(&record.id)).unwrap();
    assert_eq!(
        stored.attempt.unwrap().answer,
        Some(json!({"outcome":"pass", "reason":"clean", "note":"kept"})),
        "the answer is on the record before it settles"
    );
    let ended = advanced(&state, &record, &host);
    assert_eq!(ended.state, RunState::Done, "{ended:#?}");
    assert_eq!(host.stops.get(), 1);
    let journal = records::journal(&state.workflow(&record.id)).unwrap();
    let cost = journal.iter().find_map(|entry| match &entry.event {
        JournalEvent::Evaluation {
            origin: EvaluationOrigin::Check,
            outcome,
            cost_usd,
            object,
            ..
        } if outcome == "pass" => Some((*cost_usd, object["note"].clone())),
        _ => None,
    });
    assert_eq!(cost, Some((Some(0.25), json!("kept"))), "{journal:#?}");
}

#[test]
fn submit_refuses_what_the_run_cannot_take_and_leaves_the_record_unchanged() {
    let (_dir, state, record) = setup(JUDGE);
    let host = FakeHost::new();
    let live = launched(&state, &record, &host);
    let seq = live.attempt.as_ref().unwrap().seq;
    let file = state.workflow(&record.id).record;
    let before = fs::read(&file).unwrap();
    for (seq, outcome, why) in [
        (seq + 1, "pass", "no longer waiting on this judge"),
        (
            seq,
            "maybe",
            "unrouted outcome \"maybe\"; OUTCOME is one of `fail`, `pass`",
        ),
        (seq, "errored", "reserved or unrouted outcome"),
    ] {
        let error = submit(&state, &record.id, seq, answer(outcome, json!({})), None)
            .unwrap_err()
            .to_string();
        assert!(error.contains(why), "{outcome}: {error}");
        assert_eq!(fs::read(&file).unwrap(), before, "{outcome}");
    }
    submit(&state, &record.id, seq, answer("pass", json!({})), None).unwrap();
    let answered = fs::read(&file).unwrap();
    let error = submit(&state, &record.id, seq, answer("fail", json!({})), None).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("already has this judge's answer")
    );
    assert_eq!(fs::read(&file).unwrap(), answered);

    // A definition that no longer admits refuses with the definition's error.
    let (_dir, state, record) = setup(JUDGE);
    let live = launched(&state, &record, &FakeHost::new());
    fs::write(
        super::attempts::definition_file(&record),
        JUDGE.replace("summary='Reviews the change'\n", ""),
    )
    .unwrap();
    let file = state.workflow(&record.id).record;
    let before = fs::read(&file).unwrap();
    let seq = live.attempt.unwrap().seq;
    let error = submit(&state, &record.id, seq, answer("pass", json!({})), None).unwrap_err();
    assert!(error.to_string().contains("summary"), "{error}");
    assert_eq!(fs::read(&file).unwrap(), before);
}

#[test]
fn submit_flags_resolve_recheck_at_now_over_the_object_and_keep_the_current_key() {
    let (_dir, state, mut record) = setup(&JUDGE.replace("fail='fix'", "fail='fix',later='@wait'"));
    record.key = Some(records::Key::String("pr-7".into()));
    update(&state, &record);
    let live = launched(&state, &record, &FakeHost::new());
    let seq = live.attempt.unwrap().seq;
    let before = jiff::Timestamp::now();
    submit(
        &state,
        &record.id,
        seq,
        answer("later", json!({"recheck_at":"2000-01-01T00:00:00Z"})),
        Some(parse_when("30m").unwrap()),
    )
    .unwrap();
    let stored = records::load(&state.workflow(&record.id)).unwrap();
    let object = stored.attempt.unwrap().answer.unwrap();
    let at: jiff::Timestamp = object["recheck_at"].as_str().unwrap().parse().unwrap();
    assert!(at >= before + jiff::SignedDuration::from_mins(30), "{at}");
    assert_eq!(object["key"], "pr-7");
}

#[test]
fn a_judge_resting_without_an_answer_is_reminded_once_and_never_before_its_first_turn_ends() {
    let (_dir, state, record) = setup(JUDGE);
    let host = FakeHost::new();
    launched(&state, &record, &host);
    advanced(&state, &record, &host);
    assert!(host.push_calls.borrow().is_empty(), "no turn has ended yet");
    host.observed.borrow_mut().turn = Some(LeaderTurn {
        end: crate::store::workflow::TurnEnd {
            agent_id: "leader".into(),
            started_at: jiff::Timestamp::UNIX_EPOCH,
            ended_at: jiff::Timestamp::UNIX_EPOCH + std::time::Duration::from_secs(1),
        },
        at_rest: true,
    });
    let reminded = advanced(&state, &record, &host);
    assert!(reminded.attempt.as_ref().unwrap().reminded);
    assert_eq!(host.push_calls.borrow().len(), 1);
    let text = host.pushes.borrow().values().next().cloned().unwrap();
    assert!(text.contains("still needs its answer") && text.contains("rimz workflow submit"));
    advanced(&state, &record, &host);
    advanced(&state, &record, &host);
    assert_eq!(
        host.push_calls.borrow().len(),
        1,
        "one reminder per attempt"
    );
    assert_eq!(advanced(&state, &record, &host).state, RunState::Active);
}

#[test]
fn a_judges_timeout_runs_from_its_launch_and_pauses_while_it_is_held() {
    let (_dir, state, record) = setup(JUDGE);
    let host = FakeHost::new();
    let mut live = launched(&state, &record, &host);
    let attempt = live.attempt.as_mut().unwrap();
    attempt.started_at -= jiff::SignedDuration::from_mins(90);
    // Held for an hour of it: half an hour of its clock remains.
    attempt.asked_ms = 3_600_000;
    update(&state, &live);
    let at = timeout_at(
        &spec::load(&live.definition_dir(&state.workflow(&live.id))).unwrap(),
        &live,
    )
    .unwrap()
    .unwrap();
    let remaining = at.duration_since(jiff::Timestamp::now());
    assert!(
        remaining > jiff::SignedDuration::from_mins(29)
            && remaining <= jiff::SignedDuration::from_mins(30),
        "{remaining}"
    );
    live.attempt.as_mut().unwrap().asked_ms = 0;
    update(&state, &live);
    let ended = advanced(&state, &record, &host);
    assert_eq!(ended.state, RunState::Failed, "{ended:#?}");
    assert!(
        evaluations(&state, &record)
            .iter()
            .any(|(origin, outcome)| *origin == EvaluationOrigin::Engine && outcome == "timeout"),
        "{:#?}",
        evaluations(&state, &record)
    );
    assert_eq!(host.stops.get(), 1);

    // A judge held on a provider limit has no timeout, and the run's deadline still fires.
    let (_dir, state, record) = setup(JUDGE);
    let host = FakeHost::new();
    launched(&state, &record, &host);
    {
        let mut observed = host.observed.borrow_mut();
        observed.status = crate::store::run::RunStatus::Pending;
        observed.stopped = Some(crate::harness::workflow::Stopped {
            since: jiff::Timestamp::now(),
            kind: crate::harness::workflow::StopKind::Limit,
            handle: Some("@judge".into()),
            class: None,
            text: Some("usage limit".into()),
        });
    }
    let mut held = advanced(&state, &record, &host);
    assert!(
        held.attempt.as_ref().unwrap().held_since.is_some(),
        "{held:#?}"
    );
    let spec = spec::load(&held.definition_dir(&state.workflow(&held.id))).unwrap();
    assert_eq!(
        timeout_at(&spec, &held).unwrap(),
        None,
        "a held judge's clock is paused"
    );
    held.deadline = Some(jiff::Timestamp::now());
    update(&state, &held);
    let ended = advanced(&state, &record, &host);
    assert_eq!(ended.state, RunState::Failed, "{ended:#?}");
    assert!(
        records::journal(&state.workflow(&record.id))
            .unwrap()
            .iter()
            .any(|entry| matches!(
                &entry.event,
                JournalEvent::Route { outcome, .. } if outcome == "deadline"
            )),
        "the run deadline fires while the judge is held"
    );
}

/// The judge's pane shows an ask, or its leader's own errored turn, at the next observation.
fn stops_on(host: &FakeHost, outcome: &str) {
    use crate::harness::workflow::{StopKind, Stopped};
    use crate::store::workflow::{Asked, Asking};
    if outcome == "asked" {
        host.observed.borrow_mut().ask = Some(Asking {
            since: jiff::Timestamp::now(),
            ask: Asked {
                text: "Which branch?".into(),
                kind: "question".into(),
                handle: "@builder".into(),
            },
        });
        return;
    }
    // The leader's own errored turn is also the merged stop.
    let stop = Stopped {
        since: jiff::Timestamp::now(),
        kind: StopKind::Error,
        handle: Some("@builder".into()),
        class: Some(crate::agents::TurnErrorClass::Failed),
        text: Some("API Error: 500".into()),
    };
    let mut observed = host.observed.borrow_mut();
    observed.stopped = Some(stop.clone());
    observed.errored = Some(stop);
}

#[test]
fn an_ask_or_error_holds_a_judge_unless_the_check_routes_it() {
    for (outcome, routes) in [
        ("asked", false),
        ("asked", true),
        ("errored", false),
        ("errored", true),
    ] {
        let text = if routes {
            JUDGE.replace("fail='fix'", &format!("fail='fix',{outcome}='fix'"))
        } else {
            JUDGE.to_owned()
        };
        let (_dir, state, record) = setup(&text);
        let host = FakeHost::new();
        launched(&state, &record, &host);
        stops_on(&host, outcome);
        let after = advanced(&state, &record, &host);
        if !routes {
            let attempt = after.attempt.as_ref().expect("a held judge stays live");
            assert!(attempt.held_since.is_some(), "{outcome}: {after:#?}");
            assert_eq!(host.stops.get(), 0, "{outcome}");
            continue;
        }
        assert_eq!(after.node, "fix", "{outcome}: {after:#?}");
        assert_eq!(after.input.as_ref().unwrap()["outcome"], outcome);
        assert!(
            after.prev_attempt.is_none(),
            "a judge's ending is not a step's previous attempt"
        );
        assert!(
            evaluations(&state, &record).contains(&(EvaluationOrigin::Engine, outcome.to_owned())),
            "{outcome}"
        );
    }
}

#[test]
fn a_judges_launch_failures_and_exit_without_an_answer_fail_its_check() {
    // A budget-refused launch is limited; a failed launch died at once; a session that exits
    // without an answer died.
    let limited = {
        let (_dir, state, record) = setup(JUDGE);
        let host = FakeHost::new();
        *host.failure.borrow_mut() = Some(LaunchFailure {
            outcome: AttemptOutcome::Limited,
            reason: "codex is over its daily budget".into(),
        });
        (
            advanced(&state, &record, &host).state,
            evaluations(&state, &record),
        )
    };
    assert_eq!(limited.0, RunState::Failed);
    assert!(
        limited
            .1
            .contains(&(EvaluationOrigin::Engine, "limited".into())),
        "{limited:?}"
    );

    let (_dir, state, record) = setup(JUDGE);
    let host = FakeHost::new();
    *host.failure.borrow_mut() = Some(LaunchFailure {
        outcome: AttemptOutcome::Died,
        reason: "pane closed".into(),
    });
    let died = advanced(&state, &record, &host);
    assert_eq!(died.state, RunState::Failed, "{died:#?}");
    assert_eq!(host.launched.borrow().len(), 1);
    assert!(evaluations(&state, &record).contains(&(EvaluationOrigin::Engine, "died".into())));

    let (_dir, state, record) = setup(JUDGE);
    let host = FakeHost::new();
    launched(&state, &record, &host);
    host.observed.borrow_mut().status = crate::store::run::RunStatus::Completed;
    assert_eq!(advanced(&state, &record, &host).state, RunState::Failed);
    assert!(evaluations(&state, &record).contains(&(EvaluationOrigin::Engine, "died".into())));
}

#[test]
fn an_answer_before_the_exit_wins_and_the_deadline_wins_a_tie_with_an_answer() {
    for deadline in [false, true] {
        let (_dir, state, record) = setup(JUDGE);
        let host = FakeHost::new();
        let live = launched(&state, &record, &host);
        let seq = live.attempt.as_ref().unwrap().seq;
        submit(&state, &record.id, seq, answer("pass", json!({})), None).unwrap();
        host.observed.borrow_mut().status = crate::store::run::RunStatus::Completed;
        if deadline {
            let mut late = records::load(&state.workflow(&record.id)).unwrap();
            late.deadline = Some(jiff::Timestamp::now());
            update(&state, &late);
        }
        let ended = advanced(&state, &record, &host);
        let outcomes = evaluations(&state, &record);
        if deadline {
            assert_eq!(ended.state, RunState::Failed, "{ended:#?}");
            assert!(!outcomes.contains(&(EvaluationOrigin::Check, "pass".into())));
        } else {
            assert_eq!(ended.state, RunState::Done, "{ended:#?}");
            assert!(outcomes.contains(&(EvaluationOrigin::Check, "pass".into())));
        }
    }
}

#[test]
fn an_answer_an_edit_left_unroutable_is_an_engine_invalid() {
    let (_dir, state, record) = setup(&JUDGE.replace("fail='fix'", "fail='fix',later='@wait'"));
    let host = FakeHost::new();
    let live = launched(&state, &record, &host);
    let seq = live.attempt.as_ref().unwrap().seq;
    submit(&state, &record.id, seq, answer("later", json!({})), None).unwrap();
    fs::write(super::attempts::definition_file(&record), JUDGE).unwrap();
    let ended = advanced(&state, &record, &host);
    assert_eq!(ended.state, RunState::Failed, "{ended:#?}");
    assert!(
        evaluations(&state, &record).contains(&(EvaluationOrigin::Engine, "invalid".into())),
        "{:#?}",
        evaluations(&state, &record)
    );
}

/// A judge whose definition is edited out from under it: the run ends as a misfit, and the
/// judge's cost still counts in its journal and its outcome.
#[test]
fn a_judge_settled_by_a_misfit_keeps_its_cost() {
    let (_dir, state, record) = setup(JUDGE);
    let host = FakeHost::new();
    launched(&state, &record, &host);
    fs::write(
        super::attempts::definition_file(&record),
        "[workflow]\nscope='worktree'\nstart='other'\n\
         [test.other]\ntimeout='1h'\nshell='true'\non={pass='@done',fail='@failed'}\n",
    )
    .unwrap();
    let ended = advanced(&state, &record, &host);
    let reason = "review: the workflow definition changed under the run: it is no longer in the \
                  definition";
    assert_eq!(ended.state, RunState::Failed, "{ended:#?}");
    let outcome = ended.outcome.as_ref().unwrap();
    assert_eq!(
        (outcome.reason.as_deref(), outcome.cost_usd),
        (Some(reason), Some(0.25)),
        "{ended:#?}"
    );
    let journal = records::journal(&state.workflow(&record.id)).unwrap();
    let evaluations: Vec<_> = journal
        .iter()
        .filter_map(|entry| match &entry.event {
            JournalEvent::Evaluation {
                outcome, cost_usd, ..
            } => Some((outcome.as_str(), *cost_usd)),
            _ => None,
        })
        .collect();
    // The fault names the misfit; the judge's abandoned evaluation carries its cost, once.
    assert_eq!(
        evaluations,
        [("fault", None), ("canceled", Some(0.25))],
        "{journal:#?}"
    );
}

/// The supervised run record a judge's launch leaves, as the launcher writes it.
fn judge_run(state: &StatePaths, record: &RunRecord, run: &records::AttemptRun, host: &FakeHost) {
    let mut supervised = crate::store::run::RunRecord::new(
        state.workspace_id.clone(),
        crate::ids::AgentKind::new_unchecked("claude"),
        crate::agents::PermissionMode::Auto,
        "Build it".into(),
        record.scope.path.clone(),
    );
    supervised.run_id = run.run_id.clone();
    supervised.agent_name = Some(run.handle.trim_start_matches('@').into());
    supervised.workflow = Some(host.launched.borrow()[0].workflow.clone());
    crate::harness::run::create(state, &supervised).unwrap();
}

/// The judge-decline assists written for this run's checkout.
fn declines(record: &RunRecord) -> Vec<(String, String)> {
    crate::harness::assist_log::recent(&crate::disk::paths::logs_dir(), None)
        .into_iter()
        .filter_map(|assist| match assist.assist {
            crate::harness::assist_log::Assist::JudgeDecline {
                task,
                checkout,
                reason,
                ..
            } if checkout == record.scope.path => Some((task, reason)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_fresh_answer_that_holds_a_loop_runs_act_back_is_one_judge_decline_assist() {
    // `pass` ends the run without its act (`fix`), and `fail` leads into it; only a
    // loop-started run's declining answer is an assist.
    for (task, outcome, expected) in [
        (Some("nightly"), "pass", true),
        (Some("nightly"), "fail", false),
        (None, "pass", false),
    ] {
        let (_dir, state, mut record) = setup(JUDGE);
        record.task = task.map(str::to_owned);
        update(&state, &record);
        let host = FakeHost::new();
        let live = launched(&state, &record, &host);
        let attempt = live.attempt.as_ref().unwrap();
        judge_run(&state, &record, attempt.run.as_ref().unwrap(), &host);
        let reason = format!("{outcome} for {}", record.id);
        submit(
            &state,
            &record.id,
            attempt.seq,
            answer(outcome, json!({ "reason": reason })),
            None,
        )
        .unwrap();
        advanced(&state, &record, &host);
        let expected: Vec<_> = task
            .filter(|_| expected)
            .map(|task| (task.to_owned(), reason.clone()))
            .into_iter()
            .collect();
        assert_eq!(declines(&record), expected, "{task:?} {outcome}");
    }
}

#[test]
fn a_dead_launchers_unpublished_judge_is_found_by_its_judge_pin_and_stopped() {
    let (_dir, state, record) = setup(JUDGE);
    let host = FakeHost::new();
    let mut live = launched(&state, &record, &host);
    let attempt = live.attempt.as_mut().unwrap();
    let run = attempt.run.take().unwrap();
    attempt.host = records::Host {
        pid: u32::MAX,
        start: None,
    };
    // A step-role run at the same seq is not the judge's.
    let mut step = host.launched.borrow()[0].workflow.clone();
    step.role = WorkflowAttemptRole::Step;
    let mut decoy = crate::store::run::RunRecord::new(
        state.workspace_id.clone(),
        crate::ids::AgentKind::new_unchecked("claude"),
        crate::agents::PermissionMode::Auto,
        "Build it".into(),
        record.scope.path.clone(),
    );
    decoy.agent_name = Some("decoy".into());
    decoy.workflow = Some(step);
    crate::harness::run::create(&state, &decoy).unwrap();
    judge_run(&state, &record, &run, &host);
    let mut own = host.observed.borrow().clone();
    own.cost_usd = Some(0.5);
    host.observed_runs
        .borrow_mut()
        .insert(run.run_id.clone(), own);
    update(&state, &live);
    let ended = advanced(&state, &record, &host);
    assert_eq!(
        host.stops.get(),
        1,
        "the orphaned judge stops before settling"
    );
    // A dead launcher's attempt ends canceled, and a canceled judge evaluates afresh.
    assert_eq!(host.launched.borrow().len(), 2, "{ended:#?}");
    assert_eq!(ended.attempt.as_ref().unwrap().kind, AttemptKind::Judge);
    // The settled cost is the judge's own run's, not the decoy's.
    let died = records::journal(&state.workflow(&record.id))
        .unwrap()
        .into_iter()
        .find_map(|entry| match entry.event {
            JournalEvent::Evaluation {
                outcome, cost_usd, ..
            } if outcome == "canceled" => Some(cost_usd),
            _ => None,
        });
    assert_eq!(died, Some(Some(0.5)));
}

/// A pass that staged a judge's routed ask or error died before settling it, and an edit then
/// removed that route: the run ends as a misfit instead of routing by the old graph.
#[test]
fn a_staged_judge_ending_whose_route_an_edit_removed_ends_the_run_as_a_misfit() {
    for outcome in ["asked", "errored"] {
        let (_dir, state, record) =
            setup(&JUDGE.replace("fail='fix'", &format!("fail='fix',{outcome}='fix'")));
        let host = FakeHost::new();
        launched(&state, &record, &host);
        stops_on(&host, outcome);
        host.stop_error.set(true);
        assert!(advance(&state, &record.id, &host).is_err(), "{outcome}");
        let mut staged = records::load(&state.workflow(&record.id)).unwrap();
        let attempt = staged.attempt.as_mut().unwrap();
        assert!(attempt.ending.is_some(), "{outcome}: {staged:#?}");
        // The process that staged the ending and failed its stop has exited.
        attempt.host = records::Host {
            pid: u32::MAX,
            start: None,
        };
        update(&state, &staged);
        fs::write(super::attempts::definition_file(&record), JUDGE).unwrap();
        let ended = advanced(&state, &record, &host);
        assert_eq!(ended.state, RunState::Failed, "{outcome}: {ended:#?}");
        let reason = format!(
            "review: the workflow definition changed under the run: it no longer routes {outcome}"
        );
        assert_eq!(
            ended.outcome.as_ref().unwrap().reason.as_deref(),
            Some(reason.as_str()),
            "{ended:#?}"
        );
    }
}

/// A signal the check subscribes to lands while its judge reviews, and the judge then answers
/// `later`: the check evaluates again at once rather than sleeping on its recheck time. A judge
/// no signal reached keeps that time.
#[test]
fn a_judge_woken_while_it_runs_that_answers_later_evaluates_again_at_once() {
    for woken in [false, true] {
        let text = JUDGE.replace(
            "on={pass='@done',fail='fix'}",
            "on={pass='@done',fail='fix',later='@wait'}\nwake=['review.ready']",
        );
        let (_dir, state, record) = setup(&text);
        let host = FakeHost::new();
        let live = launched(&state, &record, &host);
        let seq = live.attempt.as_ref().unwrap().seq;
        if woken {
            let signal = crate::harness::signal::Signal {
                name: "review.ready".parse().unwrap(),
                payload: Map::new(),
                source: crate::store::event::SignalSource::Cli,
                watch: None,
            };
            wake(&state, &signal, &host).unwrap();
        }
        submit(
            &state,
            &record.id,
            seq,
            answer("later", json!({})),
            Some(parse_when("1h").unwrap()),
        )
        .unwrap();
        let after = advanced(&state, &record, &host);
        assert_eq!(after.state, RunState::Active, "{woken}: {after:#?}");
        if woken {
            assert_eq!(host.launched.borrow().len(), 2, "{after:#?}");
            assert_eq!(after.attempt.as_ref().unwrap().kind, AttemptKind::Judge);
            assert!(after.recheck_at.is_none(), "{after:#?}");
        } else {
            assert_eq!(host.launched.borrow().len(), 1, "{after:#?}");
            assert!(after.attempt.is_none(), "{after:#?}");
            let wait = after.wait.as_ref().expect("a judge's later waits");
            assert!(
                wait.recheck_at.is_some() && after.recheck_at.is_some(),
                "{after:#?}"
            );
        }
    }
}

/// An edit lands while the judge's pane is stopped: its accepted answer settles against the
/// definition read after the stop, failing `invalid` once the edit left it unroutable and
/// ending the run as a misfit once the edit removed the check.
#[test]
fn a_judges_answer_settles_against_the_definition_read_after_its_stop() {
    let other = "[workflow]\nscope='worktree'\nstart='other'\n\
         [test.other]\ntimeout='1h'\nshell='true'\non={pass='@done',fail='@failed'}\n";
    for (outcome, edited) in [("later", JUDGE), ("pass", other)] {
        let (_dir, state, record) = setup(&JUDGE.replace("fail='fix'", "fail='fix',later='@wait'"));
        let host = FakeHost::new();
        let live = launched(&state, &record, &host);
        let seq = live.attempt.as_ref().unwrap().seq;
        submit(&state, &record.id, seq, answer(outcome, json!({})), None).unwrap();
        let file = super::attempts::definition_file(&record);
        let edit = edited.to_owned();
        *host.on_stop.borrow_mut() = Some(Box::new(move || fs::write(&file, &edit).unwrap()));
        let ended = advanced(&state, &record, &host);
        assert_eq!(ended.state, RunState::Failed, "{outcome}: {ended:#?}");
        if outcome == "later" {
            assert!(
                evaluations(&state, &record)
                    .contains(&(EvaluationOrigin::Engine, "invalid".into())),
                "{:#?}",
                evaluations(&state, &record)
            );
        } else {
            assert_eq!(
                ended.outcome.as_ref().unwrap().reason.as_deref(),
                Some(
                    "review: the workflow definition changed under the run: it is no longer in \
                     the definition"
                ),
                "{ended:#?}"
            );
        }
    }
}
