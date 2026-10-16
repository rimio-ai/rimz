use super::*;
use crate::disk::lock::WorkspaceLock;
use crate::disk::paths::StatePaths;
use crate::ids::WorkflowRunId;
use crate::store::workflow::{
    self as records, AttemptOutcome, Host, JournalEvent, Key, RunRecord, RunState,
};
use serde_json::{Value, json};

const STEP: &str = "[workflow]\nscope='worktree'\nstart='build'\n[steps.build]\nagent='claude'\nprompt-file='prompt.md'\nthen='@done'\nmax-visits=2\n";

fn setup(text: &str) -> (tempfile::TempDir, StatePaths, RunRecord) {
    let dir = tempfile::tempdir().unwrap();
    let state = StatePaths::for_project_root_under(dir.path(), dir.path()).unwrap();
    fs::write(dir.path().join("prompt.md"), "Build it").unwrap();
    let spec: WorkflowSpec = toml::from_str(text).unwrap();
    let record: RunRecord = serde_json::from_value(json!({
        "version":1, "id":WorkflowRunId::new(), "workflow":"test", "spec_dir":dir.path(),
        "spec_digest":"test", "prompt":"Run request", "project_root":dir.path(),
        "scope":{"kind":"worktree","path":dir.path()},
        "created_at":"2026-01-01T00:00:00Z", "state":"active", "node":spec.workflow.start,
        "entered_at":jiff::Timestamp::now(), "deadline_fired":false, "next_seq":1, "visits":{}, "visit_grant":0
    })).unwrap();
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    records::initialize(&state, &record, text.as_bytes(), &guard).unwrap();
    (dir, state, record)
}

fn update(state: &StatePaths, record: &RunRecord) {
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    records::write(&state.workflow(&record.id), record, &guard).unwrap();
}

fn advance_ok(state: &StatePaths, id: &WorkflowRunId, host: &FakeHost) -> RunRecord {
    let result = advance(state, id, host);
    assert!(result.is_ok(), "agent steps must be admitted: {result:?}");
    records::load(&state.workflow(id)).unwrap()
}

#[test]
fn launch_counts_keyed_visits_and_composes_live_signs() {
    let (dir, state, mut record) = setup(STEP);
    record.key = Some(Key::Integer(7));
    record.input = Some(json!({"outcome":"work", "key":7}));
    update(&state, &record);
    let paths = state.workflow(&record.id);
    fs::create_dir_all(paths.signs(None)).unwrap();
    fs::write(paths.signs(Some("build")), "Keep the public API").unwrap();
    let host = FakeHost::new();
    let live = advance_ok(&state, &record.id, &host);
    assert_eq!(
        host.launched.borrow().len(),
        1,
        "a step launches a supervised attempt"
    );
    let launches = host.launched.borrow();
    assert_eq!(
        launches[0].prompt,
        "Build it\n\nKeep the public API\n\nRun request"
    );
    assert_eq!(launches[0].workflow.run, record.id);
    assert_eq!(launches[0].workflow.seq, 1);
    assert_eq!(launches[0].cwd, record.scope.path);
    assert_eq!(live.visits["build"].total, 1);
    let ctx: Value = serde_json::from_slice(&fs::read(&paths.context).unwrap()).unwrap();
    assert_eq!(ctx["visit"], 1);
    assert_eq!(ctx["visits"], 1);
    assert_eq!(ctx["input"], record.input.clone().unwrap());
    let attempt = live.attempt.unwrap();
    assert_eq!(
        attempt.digests["prompt.md"],
        super::super::script::digest(b"Build it")
    );
    assert_eq!(
        attempt.digests["signs/build.md"],
        super::super::script::digest(b"Keep the public API")
    );
    assert_eq!(
        attempt.digests["prompt"],
        super::super::script::digest(b"Run request")
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("prompt.md")).unwrap(),
        "Build it"
    );
    drop(launches);
    host.observed.borrow_mut().status = RunStatus::Completed;
    let mut ended = advance_ok(&state, &record.id, &host);
    assert!(records::journal(&paths).unwrap().iter().any(|row| matches!(&row.event, JournalEvent::Attempt { digests, .. } if digests["signs/build.md"] == super::super::script::digest(b"Keep the public API"))));
    ended.state = RunState::Active;
    ended.key = Some(Key::Integer(8));
    ended.input = Some(json!({"outcome":"work", "key":8}));
    update(&state, &ended);
    host.observed.borrow_mut().status = RunStatus::Running;
    fs::write(paths.signs(Some("build")), "Updated sign").unwrap();
    advance_ok(&state, &record.id, &host);
    let ctx: Value = serde_json::from_slice(&fs::read(&paths.context).unwrap()).unwrap();
    assert_eq!(ctx["visit"], 1, "each key has its own visit count");
    assert_eq!(ctx["visits"], 2);
    assert_eq!(ctx["prev_attempt"]["key"], 7);
    assert_eq!(
        host.launched.borrow()[1].prompt,
        "Build it\n\nUpdated sign\n\nRun request"
    );
    host.observed.borrow_mut().status = RunStatus::Completed;
    let mut ended = advance_ok(&state, &record.id, &host);
    ended.state = RunState::Active;
    update(&state, &ended);
    let mut capped = advance_ok(&state, &record.id, &host);
    assert_eq!(
        capped.park.as_ref().unwrap().reason,
        records::ParkReason::Cap
    );
    assert_eq!(
        capped.input,
        Some(json!({"outcome":"cap","step":"build","visits":2,"max":2,"key":8}))
    );
    assert_eq!(host.launched.borrow().len(), 2);
    capped.visit_grant = 1;
    capped.state = RunState::Active;
    capped.park = None;
    update(&state, &capped);
    host.observed.borrow_mut().status = RunStatus::Running;
    let extended = advance_ok(&state, &record.id, &host);
    assert_eq!(extended.visits["build"].total, 3);
    assert_eq!(extended.visits["build"].keys[1].visits, 2);
}

#[test]
fn signs_cannot_read_outside_the_node_writable_work_directory() {
    let (_dir, state, record) = setup(STEP);
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("private.md");
    fs::write(&secret, "Outside workflow work").unwrap();
    let paths = state.workflow(&record.id);
    fs::create_dir_all(paths.signs(None)).unwrap();
    std::os::unix::fs::symlink(secret, paths.signs(Some("build"))).unwrap();
    let host = FakeHost::new();
    let ended = advance_ok(&state, &record.id, &host);
    assert!(
        host.launched.borrow().is_empty(),
        "a sign cannot expose files outside workflow work"
    );
    assert_eq!(ended.prev_attempt.unwrap().outcome, AttemptOutcome::Died);
}

#[test]
fn terminal_runs_and_live_asks_limits_route_with_full_previous_attempt() {
    use crate::store::workflow::{Asked, ChildRef, Limited};
    for (status, expected) in [
        (RunStatus::Completed, AttemptOutcome::Ended),
        (RunStatus::Failed, AttemptOutcome::Died),
        (RunStatus::VerifyFailed, AttemptOutcome::Died),
        (RunStatus::TimedOut, AttemptOutcome::Died),
        (RunStatus::Canceled, AttemptOutcome::Canceled),
        (RunStatus::BudgetExceeded, AttemptOutcome::Limited),
        (RunStatus::Running, AttemptOutcome::Asked),
        (RunStatus::Pending, AttemptOutcome::Limited),
    ] {
        let (_dir, state, record) = setup(STEP);
        let host = FakeHost::new();
        let mut live = advance_ok(&state, &record.id, &host);
        live.attempt.as_mut().unwrap().head_start = Some("recorded-head".into());
        update(&state, &live);
        let reset = "2099-01-01T00:00:00Z".parse().unwrap();
        {
            let mut observed = host.observed.borrow_mut();
            observed.status = status;
            observed.children.stopped.push(ChildRef {
                handle: "child".into(),
                kind: crate::ids::AgentKind::new_unchecked("claude"),
                team: None,
            });
            observed.children.live.push(ChildRef {
                handle: "peer".into(),
                kind: crate::ids::AgentKind::new_unchecked("codex"),
                team: None,
            });
            observed.ask = Some(Asked {
                text: "Which approach?".into(),
                kind: "question".into(),
                handle: "child".into(),
            });
            if matches!(status, RunStatus::Pending | RunStatus::BudgetExceeded) {
                observed.limited = Some(Limited {
                    reset_at: Some(reset),
                });
            }
        }
        let observed = Rc::clone(&host.observed);
        *host.on_stop.borrow_mut() = Some(Box::new(move || {
            observed.borrow_mut().ask = None;
            observed.borrow_mut().limited = None;
        }));
        let ended = advance_ok(&state, &record.id, &host);
        assert_eq!(host.stops.get(), 1, "every agent end stops the tree");
        let prev = ended
            .prev_attempt
            .as_ref()
            .expect("agent ends populate previous attempt");
        assert_eq!(prev.outcome, expected);
        assert_eq!(prev.handle.as_deref(), Some("@builder"));
        assert!(prev.run_id.is_some());
        assert_eq!(prev.cost_usd, Some(0.25));
        assert_eq!(prev.head_start.as_deref(), Some("recorded-head"));
        assert_eq!(prev.children.as_ref().unwrap().stopped[0].handle, "child");
        assert_eq!(prev.children.as_ref().unwrap().live[0].handle, "peer");
        if expected == AttemptOutcome::Asked {
            assert_eq!(ended.state, RunState::Parked);
            assert_eq!(
                ended.park.as_ref().unwrap().reason,
                records::ParkReason::Asked
            );
            assert_eq!(
                ended.park.as_ref().unwrap().text.as_deref(),
                Some("Which approach?")
            );
            assert_eq!(prev.asked.as_ref().unwrap().kind, "question");
        } else {
            assert_eq!(ended.state, RunState::Done);
        }
        if status == RunStatus::Pending {
            assert_eq!(prev.limited.as_ref().unwrap().reset_at, Some(reset));
        }
        if status == RunStatus::BudgetExceeded {
            assert_eq!(prev.limited.as_ref().unwrap().reset_at, None);
        }
    }
    let (_dir, state, record) = setup(STEP);
    let host = FakeHost::new();
    host.observed.borrow_mut().status = RunStatus::Completed;
    let ended = advance_ok(&state, &record.id, &host);
    assert_eq!(
        ended.state,
        RunState::Done,
        "a completion whose wake preceded launch publication must not be lost"
    );
    assert_eq!(host.stops.get(), 1);
}

#[test]
fn asked_edge_routes_and_launch_failures_count_as_attempts() {
    for outcome in [
        AttemptOutcome::Died,
        AttemptOutcome::Limited,
        AttemptOutcome::Asked,
    ] {
        let (_dir, state, record) =
            setup(&STEP.replace("max-visits=2", "max-visits=2\non.asked='@failed'"));
        let host = FakeHost::new();
        if outcome != AttemptOutcome::Asked {
            *host.failure.borrow_mut() = Some(LaunchFailure {
                outcome,
                reason: "cannot launch".into(),
            });
        }
        let mut ended = advance_ok(&state, &record.id, &host);
        if outcome == AttemptOutcome::Asked {
            host.observed.borrow_mut().ask = Some(records::Asked {
                text: "Approve?".into(),
                kind: "permission".into(),
                handle: "@builder".into(),
            });
            ended = advance_ok(&state, &record.id, &host);
        }
        assert_eq!(ended.prev_attempt.as_ref().unwrap().outcome, outcome);
        assert_eq!(ended.visits["build"].total, 1);
        assert_eq!(
            ended.state,
            if outcome == AttemptOutcome::Asked {
                RunState::Failed
            } else {
                RunState::Done
            }
        );
        if outcome != AttemptOutcome::Asked {
            assert!(records::journal(&state.workflow(&record.id)).unwrap().iter().any(|row| matches!(&row.event, JournalEvent::Attempt { reason:Some(reason), .. } if reason == "cannot launch")));
        }
    }
}

#[test]
fn occupancy_waits_merge_without_consuming_input_key_or_visits() {
    for takeover in [false, true] {
        let (_dir, state, mut record) = setup(&format!("{STEP}takeover={takeover}\n"));
        record.key = Some(Key::Integer(7));
        record.input = Some(json!({"outcome":"work", "key":7}));
        update(&state, &record);
        let host = FakeHost::new();
        *host.occupancy.borrow_mut() = if takeover {
            Occupancy::Blocked(vec![(
                "sweeper".into(),
                crate::harness::schedule::takeover::OpenTurn::Working,
            )])
        } else {
            Occupancy::Occupied(vec!["sweeper".into()])
        };
        let waiting = advance_ok(&state, &record.id, &host);
        assert_eq!(
            waiting.wait.unwrap().outcome,
            if takeover { "takeover" } else { "occupied" }
        );
        let waiting = advance_ok(&state, &record.id, &host);
        assert_eq!(waiting.wait.unwrap().count, 2);
        assert_eq!(waiting.input, record.input);
        assert_eq!(waiting.key, record.key);
        assert!(waiting.visits.is_empty());
        assert!(host.launched.borrow().is_empty());
        *host.occupancy.borrow_mut() = if takeover {
            Occupancy::Occupied(vec!["sweeper".into()])
        } else {
            Occupancy::Free
        };
        let live = advance_ok(&state, &record.id, &host);
        assert!(live.wait.is_none());
        assert_eq!(live.visits["build"].total, 1);
        assert_eq!(host.evictions.get(), usize::from(takeover));
        assert!(
            records::journal(&state.workflow(&record.id))
                .unwrap()
                .iter()
                .any(|row| matches!(row.event, JournalEvent::Wait { count: 2, .. }))
        );
    }
}

#[test]
fn successful_eviction_admits_only_the_immediate_launch() {
    let (_dir, state, record) = setup(&format!("{STEP}takeover=true\n"));
    let host = FakeHost::new();
    *host.occupancy.borrow_mut() = Occupancy::Occupied(vec!["sweeper".into()]);
    host.stale_after_evict.set(true);
    let live = advance_ok(&state, &record.id, &host);
    assert_eq!(
        host.launched.borrow().len(),
        1,
        "successful eviction admits its immediate launch despite stale occupancy"
    );
    assert_eq!(live.visits["build"].total, 1);
    assert_eq!(host.evictions.get(), 1);
    host.observed.borrow_mut().status = RunStatus::Completed;
    let mut ended = advance_ok(&state, &record.id, &host);
    ended.state = RunState::Active;
    update(&state, &ended);
    host.observed.borrow_mut().status = RunStatus::Running;
    let waiting = advance_ok(&state, &record.id, &host);
    assert_eq!(waiting.wait.unwrap().outcome, "takeover");
    assert_eq!(waiting.visits["build"].total, 1);
    assert_eq!(host.launched.borrow().len(), 1);
}

#[test]
fn eviction_failure_is_a_counted_died_attempt() {
    let (_dir, state, record) = setup(&format!("{STEP}takeover=true\n"));
    let host = FakeHost::new();
    *host.occupancy.borrow_mut() = Occupancy::Occupied(vec!["sweeper".into()]);
    host.evict_error.set(true);
    let ended = advance_ok(&state, &record.id, &host);
    assert_eq!(ended.prev_attempt.unwrap().outcome, AttemptOutcome::Died);
    assert_eq!(ended.visits["build"].total, 1);
    assert!(host.launched.borrow().is_empty());
}

#[test]
fn timeout_deadline_and_cancel_stop_before_settlement() {
    for cause in ["timeout", "deadline", "cancel"] {
        let (_dir, state, record) = setup(&format!("{STEP}timeout='1s'\n"));
        let host = FakeHost::new();
        let mut live = advance_ok(&state, &record.id, &host);
        let stop_state = state.clone();
        let stop_id = record.id.clone();
        *host.on_stop.borrow_mut() = Some(Box::new(move || {
            let _guard = WorkspaceLock::acquire(&stop_state.workspace_lock).unwrap();
            let current = records::load(&stop_state.workflow(&stop_id)).unwrap();
            assert_eq!(current.state, RunState::Active);
            assert!(current.attempt.as_ref().unwrap().ending.is_some());
            assert_eq!(
                current.next_seq, 2,
                "no successor is admitted before stop returns"
            );
        }));
        if cause == "cancel" {
            assert!(cancel(&state, &record.id, &host).unwrap());
            assert!(!cancel(&state, &record.id, &host).unwrap());
        } else {
            if cause == "timeout" {
                live.attempt.as_mut().unwrap().started_at = "2000-01-01T00:00:00Z".parse().unwrap();
            } else {
                live.deadline = Some("2000-01-01T00:00:00Z".parse().unwrap());
            }
            update(&state, &live);
            advance_ok(&state, &record.id, &host);
        }
        let ended = records::load(&state.workflow(&record.id)).unwrap();
        assert_eq!(host.stops.get(), 1);
        assert!(ended.attempt.is_none());
        assert_eq!(
            ended.prev_attempt.unwrap().outcome,
            if cause == "timeout" {
                AttemptOutcome::Timeout
            } else {
                AttemptOutcome::Canceled
            }
        );
        assert_eq!(
            ended.state,
            match cause {
                "timeout" => RunState::Done,
                "deadline" => RunState::Parked,
                _ => RunState::Canceled,
            }
        );
    }
    let (_dir, state, record) = setup(STEP);
    let host = FakeHost::new();
    let launch_state = state.clone();
    let launch_id = record.id.clone();
    *host.on_launch.borrow_mut() = Some(Box::new(move || {
        assert!(cancel(&launch_state, &launch_id, &FakeHost::new()).unwrap());
        let pending = records::load(&launch_state.workflow(&launch_id)).unwrap();
        assert!(pending.attempt.is_some());
        assert!(cancel(&launch_state, &launch_id, &FakeHost::new()).unwrap());
        assert_eq!(
            records::load(&launch_state.workflow(&launch_id)).unwrap(),
            pending,
            "repeating cancel during launch must not restage ending evidence"
        );
    }));
    let ended = advance_ok(&state, &record.id, &host);
    assert_eq!(ended.state, RunState::Canceled);
    assert_eq!(host.stops.get(), 1);
    assert!(ended.prev_attempt.unwrap().run_id.is_some());
}

#[test]
fn dead_launcher_stops_the_unpublished_supervised_run() {
    for canceling in [false, true] {
        let (_dir, state, record) = setup(STEP);
        let host = FakeHost::new();
        let mut live = advance_ok(&state, &record.id, &host);
        let run = live.attempt.as_mut().unwrap().run.take().unwrap();
        live.attempt.as_mut().unwrap().host = Host {
            pid: u32::MAX,
            start: None,
        };
        let mut orphan = crate::store::run::RunRecord::new(
            state.workspace_id.clone(),
            crate::ids::AgentKind::new_unchecked("claude"),
            crate::agents::PermissionMode::Auto,
            "Build it".into(),
            live.scope.path.clone(),
        );
        orphan.run_id = run.run_id.clone();
        orphan.agent_name = Some(run.handle.trim_start_matches('@').into());
        orphan.workflow = Some(host.launched.borrow()[0].workflow.clone());
        crate::harness::run::create(&state, &orphan).unwrap();
        update(&state, &live);
        let ended = if canceling {
            cancel(&state, &record.id, &host).unwrap();
            records::load(&state.workflow(&record.id)).unwrap()
        } else {
            advance_ok(&state, &record.id, &host)
        };
        assert_eq!(
            host.stops.get(),
            1,
            "the durable orphan must stop before settlement"
        );
        let previous = ended.prev_attempt.unwrap();
        assert_eq!(previous.run_id, Some(run.run_id));
        assert_eq!(previous.handle, Some(run.handle));
        assert!(ended.attempt.is_none());
    }
}

#[test]
fn live_stopper_is_left_alone_and_dead_stopper_is_retaken() {
    let (_dir, state, record) = setup(STEP);
    let host = FakeHost::new();
    let mut live = advance_ok(&state, &record.id, &host);
    let attempt = live.attempt.as_mut().unwrap();
    attempt.ending = Some(records::Ending {
        outcome: AttemptOutcome::Timeout,
        cause: records::EndCause::Timeout,
    });
    update(&state, &live);
    assert_eq!(advance_ok(&state, &record.id, &host), live);
    assert_eq!(host.stops.get(), 0);
    live.attempt.as_mut().unwrap().host = Host {
        pid: u32::MAX,
        start: None,
    };
    let spec = toml::from_str(STEP).unwrap();
    let paths = state.workflow(&record.id);
    let mut staged = super::super::state::Transition {
        record: live,
        spec: &spec,
        paths: &paths,
        now: jiff::Timestamp::now(),
        entries: Vec::new(),
    };
    let mut observed = host.observed.borrow().clone();
    observed.children.stopped.push(records::ChildRef {
        handle: "already-stopped".into(),
        kind: crate::ids::AgentKind::new_unchecked("claude"),
        team: None,
    });
    staged.stage_agent_end(
        staged
            .record
            .attempt
            .as_ref()
            .unwrap()
            .ending
            .clone()
            .unwrap(),
        &observed,
    );
    let live = staged.record;
    update(&state, &live);
    let ended = advance_ok(&state, &record.id, &host);
    let previous = ended.prev_attempt.unwrap();
    assert_eq!(previous.outcome, AttemptOutcome::Timeout);
    assert!(
        previous
            .children
            .unwrap()
            .stopped
            .iter()
            .any(|child| child.handle == "already-stopped"),
        "a dead stopper must retain staged child evidence"
    );
    assert_eq!(host.stops.get(), 1);
    assert_eq!(host.launched.borrow().len(), 1);
}

fn judge_fixture() -> (WorkflowSpec, crate::disk::paths::WorkflowPaths, RunRecord) {
    let spec = toml::from_str("[workflow]\nscope='worktree'\nstart='judge'\n[checks.judge]\nagent='claude'\nprompt-file='judge.md'\non.ok='@done'\n").unwrap();
    let state = StatePaths::for_project_root_under(
        std::path::Path::new("/scratch"),
        std::path::Path::new("/project"),
    )
    .unwrap();
    let record: RunRecord = serde_json::from_value(json!({
        "version":1,"id":WorkflowRunId::new(),"workflow":"test","spec_dir":"/spec","spec_digest":"test",
        "scope":{"kind":"worktree","path":"/project"},"project_root":"/project",
        "created_at":"2026-01-01T00:00:00Z","state":"active","node":"judge","entered_at":"2026-01-01T00:00:00Z",
        "deadline_fired":false,"next_seq":2,"visits":{},"visit_grant":0,
        "attempt":{"kind":"judge","seq":1,"node":"judge","started_at":"2026-01-01T00:00:00Z","host":{"pid":123,"start":null}}
    })).unwrap();
    (spec, state.workflow(&record.id), record)
}

fn evaluation(
    object: Result<Value, String>,
    failure: &'static str,
) -> super::super::script::Evaluation {
    super::super::script::Evaluation {
        object,
        failure,
        digests: Default::default(),
        stderr_tail: None,
        limited: None,
        cost_usd: Some(0.1),
    }
}

#[test]
fn judge_routes_valid_objects_and_retries_died_or_invalid_once_then_parks() {
    for failure in ["ok", "died", "invalid", "canceled", "limited"] {
        let (spec, paths, record) = judge_fixture();
        let mut t = super::super::state::Transition {
            now: record.entered_at,
            record,
            spec: &spec,
            paths: &paths,
            entries: Vec::new(),
        };
        let reset = "2026-01-02T00:00:00Z".parse().unwrap();
        for turn in 0..2 {
            let mut result = evaluation(
                match failure {
                    "ok" => Ok(json!({"outcome":"ok"})),
                    "invalid" => Ok(json!({"outcome":"not-routed"})),
                    _ => Err("judge failed".into()),
                },
                failure,
            );
            if failure == "limited" {
                result.limited = Some(records::Limited {
                    reset_at: Some(reset),
                });
            }
            t.finish_judge(1, result).unwrap();
            if failure == "ok" {
                assert_eq!(
                    t.record.state,
                    RunState::Done,
                    "judge objects route through check edges"
                );
                break;
            }
            assert!(
                t.record.attempt.is_none(),
                "the hosted evaluation must settle"
            );
            if failure == "limited" {
                assert_eq!(t.record.recheck_at, Some(reset));
                assert_eq!(t.record.check_retry, 0);
            } else if failure == "canceled" {
                assert_eq!(t.record.check_retry, 0);
            } else if turn == 0 {
                assert_eq!(t.record.check_retry, 1);
                assert_eq!(t.record.state, RunState::Active);
                assert_eq!(
                    t.record.check_retry_context.as_ref().unwrap()["outcome"],
                    failure
                );
                assert!(t.record.check_retry_context.as_ref().unwrap()["reason"].is_string());
            } else {
                assert_eq!(t.record.state, RunState::Parked);
                assert_eq!(
                    t.record.park.as_ref().unwrap().reason,
                    records::ParkReason::Check
                );
                assert!(
                    t.record
                        .park
                        .as_ref()
                        .unwrap()
                        .text
                        .as_ref()
                        .unwrap()
                        .contains("judge failed")
                        || failure == "invalid"
                );
            }
            t.record.attempt = Some(serde_json::from_value(json!({"kind":"judge","seq":1,"node":"judge","started_at":t.now,"host":{"pid":123,"start":null},"retry":{"outcome":failure,"reason":"judge failed"}})).unwrap());
        }
    }
    let (mut spec, paths, mut record) = judge_fixture();
    spec.checks
        .get_mut("judge")
        .unwrap()
        .on
        .insert("again".into(), "@wait".into());
    spec.checks.get_mut("judge").unwrap().timeout = Some("1d".into());
    record.check_retry = 1;
    record.check_retry_context = Some(json!({"outcome":"died", "reason":"no object"}));
    let mut t = super::super::state::Transition {
        now: record.entered_at,
        record,
        spec: &spec,
        paths: &paths,
        entries: Vec::new(),
    };
    t.finish_judge(1, evaluation(Ok(json!({"outcome":"again"})), "invalid"))
        .unwrap();
    assert_eq!(t.record.check_retry, 1, "a wait stays at the same check");
    assert!(
        t.record.check_retry_context.is_none(),
        "a successful object ends retry context"
    );
}
