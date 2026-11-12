use super::*;
use std::time::Duration;

fn definition() -> String {
    UNTIL.replace("on.done='@done'", "on.done={to='@done',delay='30m'}")
}

fn pending() -> (tempfile::TempDir, StatePaths, RunRecord, FakeHost) {
    let (dir, state, record) = setup(&definition());
    check(dir.path(), json!({"outcome":"done"}));
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    host.observed.borrow_mut().turn = Some(turn());
    let pending = advance_ok(&state, &record.id, &host);
    assert!(
        pending.held.is_some(),
        "a delayed verdict must keep its session open"
    );
    assert_eq!(host.stops.get(), 0);
    (dir, state, pending, host)
}

fn evaluations(state: &StatePaths, record: &RunRecord) -> usize {
    records::journal(&state.workflow(&record.id))
        .unwrap()
        .iter()
        .filter(|row| matches!(row.event, JournalEvent::Evaluation { .. }))
        .count()
}

#[test]
fn a_delay_keeps_the_verdict_without_rechecking_then_closes_once() {
    let (_dir, state, mut record, host) = pending();
    assert_eq!(advance_ok(&state, &record.id, &host), record);
    assert_eq!(evaluations(&state, &record), 1);
    assert!(decisions(&state, &record).is_empty());
    let spec = load(&state.workflow(&record.id).home).unwrap();
    let journal = records::journal(&state.workflow(&record.id)).unwrap();
    assert!(hops(&spec, &journal, record.held.as_ref()).is_empty());
    record.held.as_mut().unwrap().verdict.as_mut().unwrap().at =
        Timestamp::now() - Duration::from_secs(1800);
    update(&state, &record);
    host.observed.borrow_mut().turn.as_mut().unwrap().at_rest = false;
    assert_eq!(advance_ok(&state, &record.id, &host), record);
    assert!(hops(&spec, &journal, record.held.as_ref()).is_empty());
    host.observed.borrow_mut().turn.as_mut().unwrap().at_rest = true;
    let ended = advance_ok(&state, &record.id, &host);
    assert_eq!(ended.state, RunState::Done);
    assert_eq!(host.stops.get(), 1);
    assert_eq!(evaluations(&state, &ended), 1);
    let rows = decisions(&state, &ended);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["action"], "closed");
    let journal = records::journal(&state.workflow(&record.id)).unwrap();
    assert_eq!(
        hops(&spec, &journal, ended.held.as_ref())
            .iter()
            .filter(|hop| hop.target.as_ref().map(|target| target.as_str()) == Some("@done"))
            .count(),
        1
    );
    assert_eq!(advance_ok(&state, &record.id, &host), ended);
    assert_eq!(host.stops.get(), 1);
}

#[test]
fn a_new_turn_discards_a_delay_and_the_next_verdict_gets_its_own_clock() {
    let (_dir, state, record, host) = pending();
    let first = record.held.as_ref().unwrap().verdict.as_ref().unwrap().at;
    host.observed.borrow_mut().turn = None;
    let restored = advance_ok(&state, &record.id, &host);
    assert!(restored.held.is_none());
    assert_eq!(decisions(&state, &restored)[0]["action"], "discarded");
    let mut next_turn = turn();
    next_turn.end.ended_at += Duration::from_secs(1);
    host.observed.borrow_mut().turn = Some(next_turn);
    let mut next = advance_ok(&state, &record.id, &host);
    let verdict = next.held.as_ref().unwrap().verdict.as_ref().unwrap();
    assert!(verdict.at >= first);
    assert_eq!(advance_ok(&state, &record.id, &host), next);
    assert_eq!(evaluations(&state, &next), 2);
    assert_eq!(host.stops.get(), 0);
    next.held.as_mut().unwrap().verdict.as_mut().unwrap().at =
        Timestamp::now() - Duration::from_secs(1801);
    update(&state, &next);
    let ended = advance_ok(&state, &record.id, &host);
    assert_eq!(ended.state, RunState::Done);
    assert_eq!(host.stops.get(), 1);
    assert_eq!(evaluations(&state, &ended), 2);
    let rows = decisions(&state, &ended);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1]["action"], "closed");
}

#[test]
fn an_overlong_delay_warns_and_closes_the_session_at_once() {
    let text = definition().replace("delay='30m'", "delay='366d'");
    let (dir, state, record) = setup(&text);
    check(dir.path(), json!({"outcome":"done"}));
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    host.observed.borrow_mut().turn = Some(turn());
    let ended = advance_ok(&state, &record.id, &host);
    assert_eq!(
        ended.state,
        RunState::Done,
        "an overlong delay must not hold the session"
    );
    assert!(ended.held.is_none());
    assert_eq!(host.stops.get(), 1);
    let spec = super::super::super::super::spec::load(&state.workflow(&record.id).home).unwrap();
    assert!(
        spec.ignored
            .iter()
            .any(|issue| issue.path == "checks.pr.on.done.delay"
                && issue.message.contains("365d maximum")),
        "{:?}",
        spec.ignored
    );
    assert_eq!(spec.checks["pr"].on["done"].target(), "@done");
    assert!(spec.checks["pr"].on["done"].delay().is_none());
}

#[test]
fn cancel_deadline_and_provider_exit_interrupt_a_delayed_close() {
    for ending in ["cancel", "deadline", "exit"] {
        let (_dir, state, mut record, host) = pending();
        match ending {
            "cancel" => {
                cancel(&state, &record.id, &host, None).unwrap();
            }
            "deadline" => {
                record.deadline = Some(Timestamp::UNIX_EPOCH);
                update(&state, &record);
                advance_ok(&state, &record.id, &host);
            }
            _ => {
                host.observed.borrow_mut().status = RunStatus::Completed;
                advance_ok(&state, &record.id, &host);
            }
        }
        let ended = records::load(&state.workflow(&record.id)).unwrap();
        assert!(ended.held.is_none(), "{ending}");
        assert_eq!(
            decisions(&state, &ended)[0]["action"],
            "interrupted",
            "{ending}"
        );
        assert_eq!(host.stops.get(), 1, "{ending}");
        assert_eq!(
            evaluations(&state, &ended),
            if ending == "exit" { 2 } else { 1 },
            "{ending}"
        );
        if ending == "deadline" {
            assert_eq!(ended.state, RunState::Failed);
            assert!(ended.deadline_fired);
        }
        if ending == "cancel" {
            let spec = load(&state.workflow(&record.id).home).unwrap();
            let journal = records::journal(&state.workflow(&record.id)).unwrap();
            assert!(
                hops(&spec, &journal, ended.held.as_ref())
                    .iter()
                    .all(|hop| hop.target.as_ref().map(|target| target.as_str()) != Some("@done"))
            );
        }
    }
}

#[test]
fn extending_a_run_preserves_its_delayed_verdict() {
    let (_dir, state, record, host) = pending();
    resume(&state, &record.id, None, &["visits=+1".into()]).unwrap();
    let extended = advance_ok(&state, &record.id, &host);
    assert_eq!(extended.held, record.held);
    assert_eq!(host.stops.get(), 0);
}

#[test]
fn resuming_at_the_check_without_a_held_session_routes_at_once() {
    let (dir, state, mut record) = setup(&definition());
    record.state = RunState::Parked;
    update(&state, &record);
    check(dir.path(), json!({"outcome":"done"}));
    resume(&state, &record.id, Some("pr"), &[]).unwrap();
    let host = FakeHost::new();
    let ended = advance_ok(&state, &record.id, &host);
    assert_eq!(ended.state, RunState::Done);
    assert_eq!(host.stops.get(), 0);
}

#[test]
fn a_held_timeout_can_delay_its_close_without_retriggering_the_timeout() {
    let text = format!("{}on.timeout={{to='@failed',delay='30m'}}\n", definition());
    let (_dir, state, record) = setup(&text);
    let host = FakeHost::new();
    let live = advance_ok(&state, &record.id, &host);
    let mut checking = held(&live, false);
    checking.entered_at = Timestamp::UNIX_EPOCH;
    update(&state, &checking);
    host.observed.borrow_mut().turn = Some(turn());
    let mut pending = advance_ok(&state, &record.id, &host);
    assert!(
        pending.held.is_some(),
        "a timed-out verdict must honor its route delay"
    );
    assert_eq!(
        pending
            .held
            .as_ref()
            .unwrap()
            .verdict
            .as_ref()
            .unwrap()
            .object["outcome"],
        "timeout"
    );
    assert_eq!(advance_ok(&state, &record.id, &host), pending);
    assert_eq!(host.stops.get(), 0);
    pending.held.as_mut().unwrap().verdict.as_mut().unwrap().at = Timestamp::UNIX_EPOCH;
    update(&state, &pending);
    let ended = advance_ok(&state, &record.id, &host);
    assert_eq!(ended.state, RunState::Failed);
    assert_eq!(decisions(&state, &ended)[0]["action"], "closed");
}
