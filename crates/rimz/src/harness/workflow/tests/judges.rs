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

/// An answer one second past the judge's timeout is refused, so the check fails `timeout`
/// rather than settling by an answer the engine would read first.
#[test]
fn submit_refuses_an_answer_past_the_judges_timeout_and_the_check_fails_timeout() {
    let (_dir, state, record) = setup(JUDGE);
    let host = FakeHost::new();
    let mut live = launched(&state, &record, &host);
    let attempt = live.attempt.as_mut().unwrap();
    attempt.started_at -= jiff::SignedDuration::from_secs(3_601);
    let seq = attempt.seq;
    update(&state, &live);
    let file = state.workflow(&record.id).record;
    let before = fs::read(&file).unwrap();
    let error = submit(&state, &record.id, seq, answer("pass", json!({})), None)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("has passed the timeout of the judge check review"),
        "{error}"
    );
    assert_eq!(fs::read(&file).unwrap(), before);
    let ended = advanced(&state, &record, &host);
    assert_eq!(ended.state, RunState::Failed, "{ended:#?}");
    let input = failure_input(&state, &record, "timeout").expect("a timeout failure");
    assert_eq!(input["reason"], json!("review ran past its timeout 1h"));
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

/// The input of the failure route the run journaled under `cause`, if it took one.
fn failure_input(state: &StatePaths, record: &RunRecord, cause: &str) -> Option<Value> {
    records::journal(&state.workflow(&record.id))
        .unwrap()
        .into_iter()
        .find_map(|entry| match entry.event {
            JournalEvent::Route { outcome, input, .. } if outcome == cause => Some(input),
            _ => None,
        })
}

/// The run's journal as its readers project it at the judge: each route hop out of `review` by
/// its outcome and target, and each evaluation's routed target.
fn judge_hops(state: &StatePaths, record: &RunRecord) -> (Vec<(String, String)>, Vec<bool>) {
    use crate::harness::workflow::inspect::{HopKind, evaluation_target, hops};
    let paths = state.workflow(&record.id);
    let journal = records::journal(&paths).unwrap();
    let spec = spec::load(&record.definition_dir(&paths)).unwrap();
    let routes = hops(&spec, &journal, None)
        .into_iter()
        .filter(|hop| hop.node == "review" && hop.kind == HopKind::Route)
        .map(|hop| {
            (
                hop.outcome.unwrap_or_default(),
                hop.target
                    .map(|target| target.as_str().to_owned())
                    .unwrap_or_default(),
            )
        })
        .collect();
    let evaluations = journal
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            entry.node == "review" && matches!(entry.event, JournalEvent::Evaluation { .. })
        })
        .map(|(index, entry)| {
            evaluation_target(&spec, entry, &journal[index + 1..], None).is_some()
        })
        .collect();
    (routes, evaluations)
}

/// What the judge's pane shows it stopped on at the next observation.
#[derive(Clone, Copy, Debug)]
enum Stop {
    Nothing,
    LeaderAsks,
    /// Its subagent asks while the judge is also stopped on a limit.
    SubagentAsks,
    Limit,
    /// Its subagent hit a limit before the judge hit its own, whose reset is the one read.
    ChildLimitFirst,
    Budget,
    Errored,
}

fn stops_on(host: &FakeHost, stop: Stop) {
    use crate::agents::TurnErrorClass;
    use crate::harness::workflow::{StopKind, Stopped};
    use crate::store::workflow::{Asked, Asking, Limited};
    let now = jiff::Timestamp::now();
    let asks = |handle: &str| Asking {
        since: now,
        ask: Asked {
            text: "Which branch?".into(),
            kind: "question".into(),
            handle: handle.into(),
        },
    };
    let stopped = |kind, class, text: Option<&str>| Stopped {
        since: now,
        kind,
        handle: Some("@judge".into()),
        class,
        text: text.map(str::to_owned),
    };
    let limit = stopped(
        StopKind::Limit,
        Some(TurnErrorClass::PausedRateLimit),
        Some("usage limit"),
    );
    let mut observed = host.observed.borrow_mut();
    match stop {
        Stop::Nothing => {}
        Stop::LeaderAsks => observed.ask = Some(asks("@judge")),
        Stop::SubagentAsks => {
            observed.ask = Some(asks("@scout"));
            observed.stopped = Some(limit);
        }
        Stop::Limit | Stop::ChildLimitFirst => {
            observed.stopped = Some(Stopped {
                handle: Some(
                    if matches!(stop, Stop::Limit) {
                        "@judge"
                    } else {
                        "@child"
                    }
                    .into(),
                ),
                ..limit
            });
            observed.limited = Some(Limited {
                reset_at: Some(RESET.parse().unwrap()),
            });
            observed.limited_by = Some("@judge".into());
        }
        Stop::Budget => observed.stopped = Some(stopped(StopKind::Budget, None, None)),
        Stop::Errored => {
            // The leader's own errored turn is also the merged stop.
            let error = stopped(
                StopKind::Error,
                Some(TurnErrorClass::Failed),
                Some("API Error: 500"),
            );
            observed.stopped = Some(error.clone());
            observed.errored = Some(error);
        }
    }
}

const RESET: &str = "2026-11-16T18:00:00Z";

/// Push the live judge's launch `minutes` into the past.
fn launched_ago(state: &StatePaths, record: &RunRecord, minutes: i64) {
    let mut live = records::load(&state.workflow(&record.id)).unwrap();
    live.attempt.as_mut().unwrap().started_at -= jiff::SignedDuration::from_mins(minutes);
    update(state, &live);
}

/// The reason and detail of a judge's `timeout` failure while it was stopped on `stop`.
fn timed_out(stop: Stop) -> (String, Value) {
    let past = "review ran past its timeout 1h";
    let asked = |handle: &str| {
        (
            format!("{past} while {handle} asked a question: Which branch?"),
            json!({"asked":{"text":"Which branch?", "kind":"question", "handle":handle}}),
        )
    };
    match stop {
        Stop::Nothing => (past.to_owned(), json!({})),
        Stop::LeaderAsks => asked("@judge"),
        Stop::SubagentAsks => asked("@scout"),
        Stop::Limit => (
            format!(
                "{past} while @judge was stopped on a provider limit (rate limit): usage limit \
                 (resets {RESET})"
            ),
            json!({
                "stopped":{"kind":"limit", "handle":"@judge", "class":"paused_rate_limit",
                    "text":"usage limit"},
                "reset_at":RESET,
            }),
        ),
        Stop::ChildLimitFirst => (
            format!(
                "{past} while @child was stopped on a provider limit (rate limit): usage limit"
            ),
            json!({
                "stopped":{"kind":"limit", "handle":"@child", "class":"paused_rate_limit",
                    "text":"usage limit"},
            }),
        ),
        Stop::Budget => (
            format!("{past} while @judge was parked on its budget"),
            json!({"stopped":{"kind":"budget", "handle":"@judge"}}),
        ),
        Stop::Errored => (
            format!("{past} while @judge was stopped on a provider error: API Error: 500"),
            json!({"stopped":{"kind":"error", "handle":"@judge", "class":"failed",
                "text":"API Error: 500"}}),
        ),
    }
}

/// The judge's `timeout` failure input while it was stopped on `stop`.
fn timeout_input(stop: Stop) -> Value {
    let (reason, detail) = timed_out(stop);
    json!({
        "outcome":"failed", "cause":"timeout", "node":"review", "reason":reason, "detail":detail,
    })
}

/// The judge's `errored` failure input.
fn errored_input() -> Value {
    json!({
        "outcome":"failed", "cause":"errored", "node":"review", "reason":"API Error: 500",
        "detail":{"errored":{"text":"API Error: 500", "class":"failed", "handle":"@judge"}},
    })
}

#[test]
fn a_judges_timeout_runs_from_its_launch_while_it_asks_or_is_held_and_names_its_stop() {
    for (stop, shown) in [
        (Stop::Nothing, (false, false)),
        (Stop::LeaderAsks, (true, false)),
        (Stop::SubagentAsks, (true, true)),
        (Stop::Limit, (false, true)),
        (Stop::ChildLimitFirst, (false, true)),
        (Stop::Budget, (false, true)),
    ] {
        let (_dir, state, record) = setup(JUDGE);
        let host = FakeHost::new();
        launched(&state, &record, &host);
        stops_on(&host, stop);
        let mut live = advanced(&state, &record, &host);
        let spec = spec::load(&live.definition_dir(&state.workflow(&live.id))).unwrap();
        let attempt = live
            .attempt
            .as_ref()
            .expect("a stopped judge stays in its pane");
        assert_eq!(
            (attempt.asking.is_some(), attempt.held_on.is_some()),
            shown,
            "{stop:?}: {live:#?}"
        );
        assert_eq!(
            timeout_at(&spec, &live).unwrap(),
            Some(attempt.started_at + jiff::SignedDuration::from_hours(1)),
            "{stop:?}: nothing the judge is stopped on pauses its clock"
        );
        // An hour and a half in, whatever held time an older RimZ credited it.
        let attempt = live.attempt.as_mut().unwrap();
        attempt.started_at -= jiff::SignedDuration::from_mins(90);
        attempt.asked_ms = 3_600_000;
        update(&state, &live);
        let ended = advanced(&state, &record, &host);
        assert_eq!(ended.state, RunState::Failed, "{stop:?}: {ended:#?}");
        assert_eq!(host.stops.get(), 1, "{stop:?}");
        let input = failure_input(&state, &record, "timeout").expect("a timeout failure");
        let (reason, detail) = timed_out(stop);
        assert_eq!(
            (&input["reason"], &input["detail"]),
            (&json!(reason), &detail),
            "{stop:?}"
        );
        assert_eq!(
            judge_hops(&state, &record),
            (vec![("timeout".into(), "@failed".into())], vec![true]),
            "{stop:?}"
        );
    }

    // The run's deadline still fires while the judge is held.
    let (_dir, state, record) = setup(JUDGE);
    let host = FakeHost::new();
    launched(&state, &record, &host);
    stops_on(&host, Stop::Limit);
    let mut held = advanced(&state, &record, &host);
    assert!(
        held.attempt.as_ref().unwrap().held_since.is_some(),
        "{held:#?}"
    );
    held.deadline = Some(jiff::Timestamp::now());
    update(&state, &held);
    let ended = advanced(&state, &record, &host);
    assert_eq!(ended.state, RunState::Failed, "{ended:#?}");
    assert!(
        failure_input(&state, &record, "deadline").is_some(),
        "the run deadline fires while the judge is held"
    );
}

#[test]
fn an_ask_leaves_a_judge_in_its_pane_until_it_submits_its_answer() {
    let (_dir, state, record) = setup(JUDGE);
    let host = FakeHost::new();
    let live = launched(&state, &record, &host);
    stops_on(&host, Stop::LeaderAsks);
    let asking = advanced(&state, &record, &host);
    assert!(
        asking.attempt.as_ref().unwrap().asking.is_some(),
        "{asking:#?}"
    );
    assert_eq!(host.stops.get(), 0);
    // Answered in its pane, the judge goes on to submit.
    host.observed.borrow_mut().ask = None;
    let seq = live.attempt.as_ref().unwrap().seq;
    submit(&state, &record.id, seq, answer("pass", json!({})), None).unwrap();
    let ended = advanced(&state, &record, &host);
    assert_eq!(ended.state, RunState::Done, "{ended:#?}");
    assert!(evaluations(&state, &record).contains(&(EvaluationOrigin::Check, "pass".into())));
}

#[test]
fn a_judges_errored_turn_fails_it_at_once_through_the_failure_chain() {
    let routes = |on: &str| JUDGE.replace("fail='fix'", &format!("fail='fix',{on}"));
    let header = JUDGE.replace("start='review'\n", "start='review'\non.failed='@done'\n");
    for (text, node, state_after, target) in [
        (
            routes("errored='@done',failed='fix'"),
            "review",
            RunState::Done,
            "@done",
        ),
        (routes("failed='fix'"), "fix", RunState::Active, "fix"),
        (header, "review", RunState::Done, "@done"),
        (JUDGE.to_owned(), "review", RunState::Failed, "@failed"),
    ] {
        let (_dir, state, record) = setup(&text);
        let host = FakeHost::new();
        launched(&state, &record, &host);
        stops_on(&host, Stop::Errored);
        let ended = advanced(&state, &record, &host);
        assert_eq!(
            (ended.node.as_str(), ended.state),
            (node, state_after),
            "{text}"
        );
        assert_eq!(host.stops.get(), 1, "{text}");
        assert_eq!(
            failure_input(&state, &record, "errored"),
            Some(errored_input()),
            "{text}"
        );
        assert!(
            evaluations(&state, &record).contains(&(EvaluationOrigin::Engine, "errored".into())),
            "{text}"
        );
        assert_eq!(
            judge_hops(&state, &record),
            (vec![("errored".into(), target.into())], vec![true]),
            "{text}"
        );
    }
}

/// A timeout that has passed wins over the errored turn the same advance observes: the judge
/// fails `timeout`, naming the provider error it was stopped on, and routes no `errored`.
#[test]
fn a_judges_timeout_wins_over_an_errored_turn_seen_at_the_same_advance() {
    let (_dir, state, record) = setup(JUDGE);
    let host = FakeHost::new();
    launched(&state, &record, &host);
    launched_ago(&state, &record, 90);
    stops_on(&host, Stop::Errored);
    let ended = advanced(&state, &record, &host);
    assert_eq!(ended.state, RunState::Failed, "{ended:#?}");
    assert_eq!(
        failure_input(&state, &record, "timeout"),
        Some(timeout_input(Stop::Errored))
    );
    assert_eq!(failure_input(&state, &record, "errored"), None);
    assert_eq!(
        judge_hops(&state, &record),
        (vec![("timeout".into(), "@failed".into())], vec![true])
    );
}

/// Stage the judge's ending the way a pass whose stop failed leaves it, by a process that has
/// since exited.
fn staged(state: &StatePaths, record: &RunRecord, stop: Stop) {
    let host = FakeHost::new();
    launched(state, record, &host);
    if matches!(stop, Stop::Limit) {
        launched_ago(state, record, 90);
    }
    stops_on(&host, stop);
    host.stop_error.set(true);
    assert!(advance(state, &record.id, &host).is_err(), "{stop:?}");
    dead_host(state, record);
}

fn dead_host(state: &StatePaths, record: &RunRecord) {
    let mut staged = records::load(&state.workflow(&record.id)).unwrap();
    let attempt = staged.attempt.as_mut().unwrap();
    assert!(attempt.ending.is_some(), "{staged:#?}");
    attempt.host = records::Host {
        pid: u32::MAX,
        start: None,
    };
    update(state, &staged);
}

/// A judge's `errored` and stopped `timeout` endings settle alike whoever settles them: the
/// pass that staged them, with a second advance landing while it stops the judge, and the
/// dead-host reclaim of a pass that died before settling. A cancel over either staged ending
/// ends the run canceled instead.
#[test]
fn every_settler_settles_a_judges_errored_and_stopped_timeout_endings_alike() {
    for (stop, cause) in [(Stop::Errored, "errored"), (Stop::Limit, "timeout")] {
        let expected = if cause == "errored" {
            errored_input()
        } else {
            timeout_input(stop)
        };

        let (_dir, state, record) = setup(JUDGE);
        let host = FakeHost::new();
        launched(&state, &record, &host);
        if cause == "timeout" {
            launched_ago(&state, &record, 90);
        }
        stops_on(&host, stop);
        let (second, id) = (state.clone(), record.id.clone());
        *host.on_stop.borrow_mut() = Some(Box::new(move || {
            advance(&second, &id, &FakeHost::new()).unwrap();
        }));
        assert_eq!(advanced(&state, &record, &host).state, RunState::Failed);
        assert_eq!(
            failure_input(&state, &record, cause),
            Some(expected.clone()),
            "{stop:?}: settled by its own pass"
        );

        let (_dir, state, record) = setup(JUDGE);
        staged(&state, &record, stop);
        let ended = advanced(&state, &record, &FakeHost::new());
        assert_eq!(ended.state, RunState::Failed, "{stop:?}: {ended:#?}");
        assert_eq!(
            failure_input(&state, &record, cause),
            Some(expected),
            "{stop:?}: settled by the dead-host reclaim"
        );

        let (_dir, state, record) = setup(JUDGE);
        staged(&state, &record, stop);
        assert!(cancel(&state, &record.id, &FakeHost::new(), None).unwrap());
        let canceled = records::load(&state.workflow(&record.id)).unwrap();
        assert_eq!(
            canceled.state,
            RunState::Canceled,
            "{stop:?}: {canceled:#?}"
        );
        assert_eq!(failure_input(&state, &record, cause), None, "{stop:?}");
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

/// A judge ending an older RimZ staged and did not settle: its routed ask, whose route the
/// author has since removed, ends the run as a misfit, and its routed errored turn settles as
/// the `errored` failure.
#[test]
fn an_older_rimzs_staged_judge_ask_ends_as_a_misfit_and_its_errored_turn_as_the_failure() {
    use crate::store::workflow::{EndCause, Ending};
    for outcome in [AttemptOutcome::Asked, AttemptOutcome::Errored] {
        let (_dir, state, record) = setup(JUDGE);
        let mut live = launched(&state, &record, &FakeHost::new());
        let attempt = live.attempt.as_mut().unwrap();
        attempt.ending = Some(Ending {
            outcome,
            cause: EndCause::Lifecycle,
            route: None,
            reason: None,
        });
        attempt.evidence = Some(if outcome == AttemptOutcome::Asked {
            json!({"outcome":"asked", "reason":"Which branch?",
                "asked":{"text":"Which branch?", "kind":"question", "handle":"@judge"}})
        } else {
            json!({"outcome":"errored", "reason":"API Error: 500",
                "errored":{"text":"API Error: 500", "class":"failed", "handle":"@judge"}})
        });
        update(&state, &live);
        dead_host(&state, &record);
        let ended = advanced(&state, &record, &FakeHost::new());
        assert_eq!(ended.state, RunState::Failed, "{outcome:?}: {ended:#?}");
        if outcome == AttemptOutcome::Asked {
            assert_eq!(
                ended.outcome.as_ref().unwrap().reason.as_deref(),
                Some(
                    "review: the workflow definition changed under the run: it no longer \
                     routes asked"
                ),
                "{ended:#?}"
            );
        } else {
            assert_eq!(
                failure_input(&state, &record, "errored"),
                Some(errored_input())
            );
        }
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
