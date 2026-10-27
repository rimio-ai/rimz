use super::*;
use crate::store::workflow::{self as records, AttemptOutcome, RunState};

#[test]
fn command_steps_bypass_every_agent_host_operation_and_prompt() {
    let mut results = Vec::new();
    for runner in ["run", "shell"] {
        let text = format!(
            "[workflow]\nscope='worktree'\nstart='build'\n[steps.build]\n{runner}='true'\ntimeout='1h'\nthen='done'\n[checks.done]\nshell='true'\non={{pass='@done',fail='@failed'}}\n"
        );
        let (_dir, state, record) = super::attempts::setup(&text);
        let host = FakeHost::new();
        *host.occupancy.borrow_mut() = Occupancy::Blocked(vec![]);
        let paths = state.workflow(&record.id);
        fs::create_dir(paths.signs(Some("build"))).unwrap();
        let result = advance(&state, &record.id, &host);
        let ended = records::load(&paths).unwrap();
        let calls = (
            host.occupancies.get(),
            host.evictions.get(),
            host.launched.borrow().len(),
            host.observations.get(),
            host.stops.get(),
        );
        let valid = result.is_ok()
            && ended.state == RunState::Done
            && ended
                .prev_attempt
                .as_ref()
                .is_some_and(|p| p.outcome == AttemptOutcome::Ended)
            && calls == (0, 0, 0, 0, 0);
        results.push((runner, valid, calls, format!("{result:?}; {ended:?}")));
    }
    assert!(
        results.iter().all(|(_, valid, _, _)| *valid),
        "{results:#?}"
    );
}

#[test]
fn command_step_cycles_stop_at_total_and_keyed_caps_without_consuming_a_sequence() {
    let mut results = Vec::new();
    for keyed in [false, true] {
        let bound = if keyed {
            "max-visits-per-key"
        } else {
            "max-visits"
        };
        let text = format!(
            "[workflow]\nscope='worktree'\nstart='build'\n[steps.build]\nshell='true'\ntimeout='1h'\nthen='again'\nmax-visits={}\n{}\n[checks.again]\nshell='true'\non={{pass='build',fail='@failed'}}\n",
            if keyed { 3 } else { 1 },
            if keyed { "max-visits-per-key=1" } else { "" }
        );
        let (_dir, state, record) = super::attempts::setup(&text);
        let host = FakeHost::new();
        *host.occupancy.borrow_mut() = Occupancy::Blocked(vec![]);
        let result = advance(&state, &record.id, &host);
        let ended = records::load(&state.workflow(&record.id)).unwrap();
        let valid = result.is_ok()
            && ended.state == RunState::Parked
            && ended
                .visits
                .get("build")
                .is_some_and(|visits| visits.total == 1)
            && ended.next_seq == 3
            && ended
                .input
                .as_ref()
                .is_some_and(|input| input["bound"] == bound);
        results.push((keyed, valid, format!("{result:?}; {ended:?}")));
    }
    assert!(results.iter().all(|(_, valid, _)| *valid), "{results:#?}");
}
