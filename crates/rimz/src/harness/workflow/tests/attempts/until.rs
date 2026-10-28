use super::*;
use crate::harness::assist_log::{self, Assist};
use crate::harness::workflow::LeaderTurn;
use crate::store::workflow::{HeldSession, StepLifecycle, TurnEnd, Verdict};
use jiff::Timestamp;

const UNTIL: &str = "[workflow]\nscope='worktree'\nstart='build'\n[steps.build]\nagent='claude'\nprompt-file='prompt.md'\nlifecycle='until'\nthen='pr'\nmax-visits=2\n[checks.pr]\nrun='sh check.sh'\ntimeout='1h'\non.pending='build'\non.done='@done'\n";

mod checks;

fn turn() -> LeaderTurn {
    LeaderTurn {
        end: TurnEnd {
            agent_id: "leader".into(),
            started_at: Timestamp::UNIX_EPOCH,
            ended_at: Timestamp::UNIX_EPOCH + std::time::Duration::from_secs(1),
        },
        at_rest: true,
    }
}

fn check(dir: &std::path::Path, object: Value) {
    fs::write(dir.join("check.sh"), format!(
        "cp \"$RIMZ_WORKFLOW_DIR/context.json\" \"$RIMZ_WORKFLOW_DIR/check-view.json\"\nprintf '%s' {}\n",
        shlex::try_quote(&object.to_string()).unwrap(),
    )).unwrap();
}

fn decisions(state: &StatePaths, record: &RunRecord) -> Vec<Value> {
    records::journal(&state.workflow(&record.id))
        .unwrap()
        .into_iter()
        .map(|row| serde_json::to_value(row).unwrap())
        .filter(|row| row["event"] == "turn_end")
        .collect()
}

fn held(record: &RunRecord, verdict: bool) -> RunRecord {
    let mut record = record.clone();
    let mut attempt = record.attempt.take().unwrap();
    attempt.judged = Some(turn().end);
    record.held = Some(HeldSession {
        attempt,
        entered_at: record.entered_at,
        verdict: verdict.then(|| Verdict {
            node: "pr".into(),
            entered_at: Timestamp::now(),
            target: "@done".into(),
            object: json!({"outcome":"done"}),
            reason: records::ParkReason::User,
        }),
    });
    record.node = "pr".into();
    record.entered_at = Timestamp::now();
    record
}

#[test]
fn same_step_keeps_consume_each_turn_once_without_relaunch_or_input_changes() {
    keeps("build");
}

#[test]
fn wait_keeps_consume_each_turn_once_without_relaunch_or_input_changes() {
    keeps("@wait");
}

fn keeps(target: &str) -> (tempfile::TempDir, StatePaths, RunRecord, FakeHost) {
    let (dir, state, mut record) =
        setup(&UNTIL.replace("on.pending='build'", &format!("on.pending='{target}'")));
    record.key = Some(Key::Integer(7));
    record.input = Some(json!({"launch":true}));
    update(&state, &record);
    check(
        dir.path(),
        json!({"outcome":"pending", "key":"ignored", "recheck_at":"2099-01-01T00:00:00Z"}),
    );
    let host = FakeHost::new();
    let live = advance_ok(&state, &record.id, &host);
    host.observed.borrow_mut().turn = Some(turn());
    let mut kept = advance_ok(&state, &record.id, &host);
    assert_eq!(kept.node, "build");
    assert!(kept.held.is_none());
    assert_eq!(kept.input, record.input);
    assert_eq!(kept.key, record.key);
    assert_eq!(kept.entered_at, live.entered_at);
    assert_eq!(kept.visits, live.visits);
    assert_eq!(
        kept.attempt.as_ref().unwrap().run,
        live.attempt.as_ref().unwrap().run
    );
    assert_eq!(
        kept.attempt.as_ref().unwrap().seq,
        live.attempt.as_ref().unwrap().seq
    );
    assert_eq!(kept.attempt.as_ref().unwrap().judged, Some(turn().end));
    assert_eq!((host.stops.get(), host.launched.borrow().len()), (0, 1));
    let paths = state.workflow(&record.id);
    let checking: Value =
        serde_json::from_slice(&fs::read(paths.work.join("check-view.json")).unwrap()).unwrap();
    assert_eq!(checking["node"], "pr");
    assert!(checking.get("visit").is_none());
    assert_eq!(checking["live_attempt"]["step"], "build");
    assert_eq!(checking["live_attempt"]["visit"], 1);
    assert_eq!(checking["live_attempt"]["visits"], 1);
    assert_eq!(checking["live_attempt"]["key"], 7);
    assert_eq!(checking["live_attempt"]["turn_end"]["agent_id"], "leader");
    assert!(checking["prev_attempt"].is_null());
    for field in ["outcome", "ended_at", "head_end"] {
        assert!(checking["live_attempt"].get(field).is_none());
    }
    let restored: Value = serde_json::from_slice(&fs::read(&paths.context).unwrap()).unwrap();
    assert_eq!(restored["node"], "build");
    assert_eq!(decisions(&state, &kept)[0]["action"], "kept");
    assert_eq!(advance_ok(&state, &record.id, &host), kept);
    if target == "@wait" {
        assert!(kept.recheck_at.is_none());
        assert!(kept.wait.as_ref().unwrap().recheck_at.is_none());
        host.observed
            .borrow_mut()
            .turn
            .as_mut()
            .unwrap()
            .end
            .ended_at += std::time::Duration::from_secs(1);
        let again = advance_ok(&state, &record.id, &host);
        assert_eq!(again.wait.as_ref().unwrap().count, 2);
        assert_eq!(
            again.wait.as_ref().unwrap().first,
            kept.wait.as_ref().unwrap().first
        );
        kept = again;
    }
    (dir, state, kept, host)
}

#[test]
fn cancel_books_a_kept_wait_at_the_check() {
    end_kept_wait(true);
}

#[test]
fn provider_exit_books_a_kept_wait_at_the_check() {
    end_kept_wait(false);
}

fn end_kept_wait(canceled: bool) {
    let (dir, state, kept, host) = keeps("@wait");
    if canceled {
        assert!(cancel(&state, &kept.id, &host).unwrap());
    } else {
        host.observed.borrow_mut().status = RunStatus::Completed;
        check(dir.path(), json!({"outcome":"done"}));
        assert_eq!(advance_ok(&state, &kept.id, &host).state, RunState::Done);
    }
    let waits: Vec<_> = records::journal(&state.workflow(&kept.id))
        .unwrap()
        .into_iter()
        .filter_map(|row| match row.event {
            JournalEvent::Wait { count, .. } => Some((row.node, count)),
            _ => None,
        })
        .collect();
    let routes: Vec<_> = assist_log::recent(&crate::disk::paths::logs_dir(), None)
        .into_iter()
        .filter_map(|record| match record.assist {
            Assist::WorkflowRoute {
                run,
                node,
                outcome,
                to,
                count: Some(count),
                ..
            } if run == kept.id => Some((node, outcome, to, count)),
            _ => None,
        })
        .collect();
    assert_eq!(
        (waits, routes),
        (
            vec![("pr".into(), 2)],
            vec![("pr".into(), "pending".into(), "@wait".into(), 2)]
        ),
        "a kept wait belongs to its check in both the journal and route assist"
    );
}

#[test]
fn closing_verdicts_stop_once_and_follow_the_saved_route() {
    let mut actual = Vec::new();
    for target in ["@done", "@user", "next", "other"] {
        let text = format!(
            "{}\n[steps.next]\nagent='claude'\nprompt-file='prompt.md'\nthen='other'\n[checks.other]\nshell='true'\non.pass='@done'\n",
            UNTIL.replace("on.done='@done'", &format!("on.done='{target}'"))
        );
        let (dir, state, record) = setup(&text);
        check(dir.path(), json!({"outcome":"done"}));
        let host = FakeHost::new();
        advance_ok(&state, &record.id, &host);
        host.observed.borrow_mut().turn = Some(turn());
        let ended = advance_ok(&state, &record.id, &host);
        let rows = records::journal(&state.workflow(&record.id)).unwrap();
        let values: Vec<_> = rows
            .into_iter()
            .map(|row| serde_json::to_value(row).unwrap())
            .collect();
        let attempt = values
            .iter()
            .position(|row| row["event"] == "attempt")
            .unwrap_or(usize::MAX);
        let decision = values
            .iter()
            .position(|row| row["event"] == "turn_end")
            .unwrap_or(usize::MAX);
        actual.push((
            target,
            host.stops.get(),
            ended.state,
            ended.node.clone(),
            values
                .iter()
                .filter(|row| row["event"] == "evaluation" && row["node"] == "pr")
                .count(),
            attempt < decision,
        ));
        if let Some(row) = decisions(&state, &ended).first() {
            assert_eq!(row["action"], "closed");
            assert_eq!(row["target"], target);
            assert_eq!(row["node"], "build");
            assert_eq!(
                ended.prev_attempt.as_ref().unwrap().outcome,
                AttemptOutcome::Ended
            );
        }
    }
    assert_eq!(
        actual,
        vec![
            ("@done", 1, RunState::Done, "pr".into(), 1, true),
            ("@user", 1, RunState::Parked, "pr".into(), 1, true),
            ("next", 1, RunState::Active, "next".into(), 1, true),
            ("other", 1, RunState::Done, "other".into(), 1, true),
        ]
    );
}

#[test]
fn a_moved_turn_discards_the_close() {
    reread(true);
}

#[test]
fn same_turn_debt_defers_the_close_without_rechecking() {
    reread(false);
}

fn reread(moved: bool) {
    let (dir, state, record) = setup(UNTIL);
    check(dir.path(), json!({"outcome":"done"}));
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    host.observed.borrow_mut().turn = Some(turn());
    let observed = host.observed.clone();
    let context = state.workflow(&record.id).context;
    *host.on_observe.borrow_mut() = Some(Box::new(move || {
        let value: Value = serde_json::from_slice(&fs::read(&context).unwrap()).unwrap();
        if value["node"] == "pr" {
            if moved {
                observed.borrow_mut().turn = None;
            } else {
                observed.borrow_mut().turn.as_mut().unwrap().at_rest = false;
            }
        }
    }));
    let pending = advance_ok(&state, &record.id, &host);
    assert_eq!(
        (
            host.stops.get(),
            pending.held.is_some(),
            decisions(&state, &pending)
                .first()
                .map(|row| row["action"].clone()),
        ),
        (0, !moved, moved.then(|| json!("discarded")))
    );
    if !moved {
        *host.on_observe.borrow_mut() = None;
        host.observed.borrow_mut().turn.as_mut().unwrap().at_rest = true;
        let ended = advance_ok(&state, &record.id, &host);
        assert_eq!(ended.state, RunState::Done);
        assert_eq!(host.stops.get(), 1);
        assert_eq!(
            records::journal(&state.workflow(&record.id))
                .unwrap()
                .iter()
                .filter(|row| matches!(row.event, JournalEvent::Evaluation { .. }))
                .count(),
            1
        );
    }
}

#[test]
fn a_discard_can_check_a_newer_eligible_end_in_the_same_advance() {
    let (dir, state, record) = setup(UNTIL);
    check(dir.path(), json!({"outcome":"done"}));
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    host.observed.borrow_mut().turn = Some(turn());
    let observed = host.observed.clone();
    let context = state.workflow(&record.id).context;
    let moved = std::cell::Cell::new(false);
    *host.on_observe.borrow_mut() = Some(Box::new(move || {
        let value: Value = serde_json::from_slice(&fs::read(&context).unwrap()).unwrap();
        if value["node"] == "pr" && !moved.replace(true) {
            observed.borrow_mut().turn.as_mut().unwrap().end.ended_at +=
                std::time::Duration::from_secs(1);
        }
    }));
    let ended = advance_ok(&state, &record.id, &host);
    assert_eq!(ended.state, RunState::Done);
    assert_eq!(host.stops.get(), 1);
    assert_eq!(
        decisions(&state, &ended)
            .iter()
            .map(|row| row["action"].clone())
            .collect::<Vec<_>>(),
        vec![json!("discarded"), json!("closed")]
    );
}

#[test]
fn provider_endings_and_deadlines_interrupt_the_held_check() {
    let mut actual = Vec::new();
    for (status, deadline, expected) in [
        (RunStatus::Completed, false, AttemptOutcome::Ended),
        (RunStatus::Failed, false, AttemptOutcome::Died),
        (RunStatus::Canceled, false, AttemptOutcome::Canceled),
        (RunStatus::BudgetExceeded, false, AttemptOutcome::Limited),
        (RunStatus::Running, true, AttemptOutcome::Canceled),
    ] {
        let (dir, state, record) = setup(UNTIL);
        check(dir.path(), json!({"outcome":"done"}));
        let host = FakeHost::new();
        let live = advance_ok(&state, &record.id, &host);
        let mut pending = held(&live, deadline);
        if deadline {
            pending.deadline = Some(Timestamp::UNIX_EPOCH);
        } else {
            let mut attempt = live.attempt.unwrap();
            attempt.kind = records::AttemptKind::Script;
            attempt.lifecycle = StepLifecycle::Turn;
            attempt.node = "pr".into();
            attempt.seq = pending.next_seq;
            pending.next_seq += 1;
            pending.attempt = Some(attempt);
        }
        update(&state, &pending);
        host.observed.borrow_mut().status = status;
        let ended = advance_ok(&state, &record.id, &host);
        let rows = decisions(&state, &ended);
        actual.push((
            status,
            deadline,
            (ended
                .prev_attempt
                .as_ref()
                .is_some_and(|previous| previous.outcome == expected)
                && ended.held.is_none()
                && (!deadline || (ended.state == RunState::Parked && ended.deadline_fired)))
                .then_some(expected),
            rows.iter()
                .map(|row| row["action"].clone())
                .collect::<Vec<_>>(),
            host.stops.get(),
        ));
    }
    assert_eq!(
        actual,
        vec![
            (
                RunStatus::Completed,
                false,
                Some(AttemptOutcome::Ended),
                vec![json!("interrupted")],
                1
            ),
            (
                RunStatus::Failed,
                false,
                Some(AttemptOutcome::Died),
                vec![json!("interrupted")],
                1
            ),
            (
                RunStatus::Canceled,
                false,
                Some(AttemptOutcome::Canceled),
                vec![json!("interrupted")],
                1
            ),
            (
                RunStatus::BudgetExceeded,
                false,
                Some(AttemptOutcome::Limited),
                vec![json!("interrupted")],
                1
            ),
            (
                RunStatus::Running,
                true,
                Some(AttemptOutcome::Canceled),
                vec![json!("interrupted")],
                1
            ),
        ]
    );
}

#[test]
fn cancel_interrupts_a_held_session_or_replaces_a_staged_close() {
    let mut actual = Vec::new();
    for staged in [false, true] {
        let (dir, state, record) = setup(UNTIL);
        check(dir.path(), json!({"outcome":"done"}));
        let host = FakeHost::new();
        let live = advance_ok(&state, &record.id, &host);
        if staged {
            host.observed.borrow_mut().turn = Some(turn());
            let cancel_state = state.clone();
            let id = record.id.clone();
            *host.on_stop.borrow_mut() = Some(Box::new(move || {
                cancel(&cancel_state, &id, &FakeHost::new()).unwrap();
            }));
            advance_ok(&state, &record.id, &host);
        } else {
            update(&state, &held(&live, true));
            cancel(&state, &record.id, &host).unwrap();
        }
        let ended = records::load(&state.workflow(&record.id)).unwrap();
        actual.push((
            staged,
            ended.state,
            host.stops.get(),
            decisions(&state, &ended)
                .iter()
                .map(|row| row["action"].clone())
                .collect::<Vec<_>>(),
        ));
    }
    assert_eq!(
        actual,
        vec![
            (false, RunState::Canceled, 1, vec![json!("interrupted")]),
            (true, RunState::Canceled, 1, vec![json!("interrupted")])
        ]
    );
}

#[test]
fn an_already_closed_first_turn_checks_in_the_launching_advance() {
    let (dir, state, record) = setup(UNTIL);
    check(dir.path(), json!({"outcome":"done"}));
    let host = FakeHost::new();
    host.observed.borrow_mut().turn = Some(turn());
    assert_eq!(advance_ok(&state, &record.id, &host).state, RunState::Done);
    assert_eq!((host.stops.get(), host.launched.borrow().len()), (1, 1));
}
