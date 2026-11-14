//! A definition edited under a live run: each read takes the edit, a run the edit no longer
//! fits ends failed naming the node, and a definition that does not read never moves the run.

use super::rest::{SESSION, check, held, turn};
use super::*;
use crate::store::workflow::{EndCause, Ending, RunOrigin};

/// Replace the definition the run reads at its next engine read.
fn edit(state: &StatePaths, record: &RunRecord, text: &str) {
    let paths = state.workflow(&record.id);
    let current = records::load(&paths).unwrap();
    fs::write(
        crate::disk::paths::workflow_spec_file(&current.definition_dir(&paths)),
        text,
    )
    .unwrap();
}

/// The run ended failed, its reason naming `node` and what no longer fits.
fn assert_misfit(ended: &RunRecord, node: &str, what: &str) {
    assert_eq!(ended.state, RunState::Failed, "{ended:#?}");
    assert_eq!(
        ended
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.reason.as_deref()),
        Some(format!("{node}: the workflow definition changed under the run: {what}").as_str()),
        "{ended:#?}"
    );
}

const AFTER: &str =
    "[test.after]\ntimeout='365d'\nshell='true'\non={pass='@done',fail='@failed'}\n";

#[test]
fn a_step_removed_under_its_running_agent_fails_the_run_and_stops_the_agent() {
    let (_dir, state, record) = setup(STEP);
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    edit(
        &state,
        &record,
        "[workflow]\nscope='worktree'\nstart='other'\n[agent.other]\nagent='claude'\nlifecycle='turn'\nprompt-file='prompt.md'\nthen='@done'\n",
    );
    host.observed.borrow_mut().status = RunStatus::Completed;
    let ended = advance_ok(&state, &record.id, &host);
    assert_misfit(&ended, "build", "it is no longer in the definition");
    assert_eq!(host.stops.get(), 1);
}

#[test]
fn a_step_changed_to_a_command_under_its_agent_fails_the_run() {
    let (_dir, state, record) = setup(&(STEP.replace("then='@done'", "then='after'") + AFTER));
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    edit(
        &state,
        &record,
        &format!(
            "[workflow]\nscope='worktree'\nstart='build'\n[command.build]\nshell='true'\ntimeout='1m'\nthen='after'\n{AFTER}"
        ),
    );
    let ended = advance_ok(&state, &record.id, &host);
    assert_misfit(&ended, "build", "it is no longer an agent step");
    assert_eq!(host.stops.get(), 1);
}

#[test]
fn a_session_switched_to_a_turn_fails_the_run() {
    let (_dir, state, record) = setup(SESSION);
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    edit(
        &state,
        &record,
        &SESSION.replace("lifecycle='session'\nrest-check=['pr']", "lifecycle='turn'"),
    );
    let ended = advance_ok(&state, &record.id, &host);
    assert_misfit(&ended, "build", "its lifecycle is now turn, not session");
    assert_eq!(host.stops.get(), 1);
}

#[test]
fn rest_checks_removed_under_a_held_session_fail_the_run() {
    let (_dir, state, record) = setup(SESSION);
    let host = FakeHost::new();
    let live = advance_ok(&state, &record.id, &host);
    update(&state, &held(&live, false));
    edit(&state, &record, &SESSION.replace("rest-check=['pr']\n", ""));
    let ended = advance_ok(&state, &record.id, &host);
    assert_misfit(&ended, "build", "it no longer has rest checks");
    assert!(ended.held.is_none(), "{ended:#?}");
    assert_eq!(host.stops.get(), 1);
}

#[test]
fn rest_checks_removed_with_a_push_pending_fail_the_run() {
    let (dir, state, record) = setup(SESSION);
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    check(dir.path(), json!({"outcome":"pending"}));
    host.observed.borrow_mut().turn = Some(turn());
    host.push_error.set(true);
    assert!(advance(&state, &record.id, &host).is_err());
    let saved = records::load(&state.workflow(&record.id)).unwrap();
    assert!(saved.attempt.as_ref().unwrap().push.is_some(), "{saved:#?}");
    edit(&state, &record, &SESSION.replace("rest-check=['pr']\n", ""));
    let ended = advance_ok(&state, &record.id, &host);
    assert_misfit(&ended, "build", "it no longer has rest checks");
    assert_eq!(host.stops.get(), 1);
}

#[test]
fn cases_shrunk_under_a_held_wait_verdict_fail_the_run() {
    let two = "[condition.pr]\ntimeout='1h'\nelse='pending'\non={done='@done',pending='build',unknown='build'}\n[[condition.pr.cases]]\nwhen=['pr=open']\noutcome='done'\nfor='1h'\n[[condition.pr.cases]]\nwhen=['pr=merged']\noutcome='done'\nfor='1h'\n";
    let head = SESSION.split("[script.pr]").next().unwrap();
    let (_dir, state, record) = setup(&format!("{head}{two}"));
    let host = FakeHost::new();
    let live = advance_ok(&state, &record.id, &host);
    let mut holding = held(&live, true);
    let verdict = holding.held.as_mut().unwrap().verdict.as_mut().unwrap();
    verdict.target = "@wait".into();
    verdict.object = json!({"outcome":"hold", "case":1});
    update(&state, &holding);
    let one = two
        .split("[[condition.pr.cases]]\nwhen=['pr=merged']")
        .next()
        .unwrap();
    edit(&state, &record, &format!("{head}{one}"));
    let ended = advance_ok(&state, &record.id, &host);
    assert_misfit(&ended, "pr", "it no longer has case 1");
    assert_eq!(host.stops.get(), 1);
}

#[test]
fn an_asked_route_removed_under_a_staged_asked_ending_fails_the_run() {
    let asked = STEP.replace("then='@done'", "then='@done'\non.asked='help'")
        + "[agent.help]\nagent='claude'\nlifecycle='turn'\nprompt-file='prompt.md'\nthen='@done'\n";
    let (_dir, state, record) = setup(&asked);
    let host = FakeHost::new();
    let mut live = advance_ok(&state, &record.id, &host);
    let attempt = live.attempt.as_mut().unwrap();
    attempt.ending = Some(Ending {
        outcome: AttemptOutcome::Asked,
        cause: EndCause::Lifecycle,
        route: None,
        reason: None,
    });
    // The host that staged the ending is gone, so this advance finishes the attempt.
    attempt.host = Host {
        pid: u32::MAX,
        start: None,
    };
    update(&state, &live);
    edit(&state, &record, STEP);
    let ended = advance_ok(&state, &record.id, &host);
    assert_misfit(&ended, "build", "it no longer routes asked");
    assert_eq!(host.stops.get(), 1);
}

#[test]
fn the_current_check_removed_fails_the_run() {
    let (_dir, state, record) = setup(OUTCOME_STEP);
    let host = FakeHost::new();
    let waiting = answer(&state, &record, &host, json!({"outcome":"queued"}));
    assert_eq!(
        (waiting.node.as_str(), waiting.state),
        ("watch", RunState::Active)
    );
    let without = OUTCOME_STEP
        .replace("start='watch'", "start='judge'")
        .replace("[script.watch]\nrun='cat answer.json'\ntimeout='1h'\non={red='build',blue='build',queued='@wait'}\n", "")
        .replace("then='watch'", "then='judge'");
    edit(&state, &record, &without);
    let mut current = records::load(&state.workflow(&record.id)).unwrap();
    current.recheck_at = None;
    update(&state, &current);
    let ended = advance_ok(&state, &record.id, &host);
    assert_misfit(&ended, "watch", "it is no longer in the definition");
}

/// A deployed definition that no longer parses, or that admission refuses, leaves the run as
/// it was; the next advance once it is fixed proceeds.
#[test]
fn a_broken_deployed_definition_never_moves_the_run_and_a_fix_resumes_it() {
    for broken in ["[workflow", &STEP.replace("then='@done'", "then='build'")] {
        let (_dir, state, record) = setup(STEP);
        let host = FakeHost::new();
        advance_ok(&state, &record.id, &host);
        edit(&state, &record, broken);
        host.observed.borrow_mut().status = RunStatus::Completed;
        let paths = state.workflow(&record.id);
        let before = (
            fs::read(&paths.record).unwrap(),
            fs::read(&paths.journal).ok(),
        );
        assert!(advance(&state, &record.id, &host).is_err(), "{broken}");
        assert_eq!(
            (
                fs::read(&paths.record).unwrap(),
                fs::read(&paths.journal).ok()
            ),
            before,
            "{broken}"
        );
        edit(&state, &record, STEP);
        assert_eq!(
            advance_ok(&state, &record.id, &host).state,
            RunState::Done,
            "{broken}"
        );
    }
}

/// An inline definition that no longer parses ends the run failed; one that parses but that
/// admission refuses waits like a deployed one.
#[test]
fn an_unparsable_inline_definition_fails_the_run_and_a_refused_one_waits() {
    let inline = |state: &StatePaths, record: &RunRecord| {
        let paths = state.workflow(&record.id);
        let mut record = records::load(&paths).unwrap();
        record.origin = RunOrigin::Inline;
        record.spec_dir = None;
        let prompt =
            std::collections::BTreeMap::from([("prompt.md".to_owned(), b"Build it".to_vec())]);
        records::seal_definition(&paths.definition, STEP, &prompt).unwrap();
        update(state, &record);
    };
    let (_dir, state, record) = setup(STEP);
    inline(&state, &record);
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    edit(
        &state,
        &record,
        &STEP.replace("then='@done'", "then='build'"),
    );
    assert!(advance(&state, &record.id, &host).is_err());
    let waiting = records::load(&state.workflow(&record.id)).unwrap();
    assert_eq!(waiting.state, RunState::Active);

    edit(&state, &record, "[workflow");
    let ended = advance_ok(&state, &record.id, &host);
    assert_eq!(ended.state, RunState::Failed, "{ended:#?}");
    let reason = ended.outcome.unwrap().reason.unwrap();
    assert!(
        reason.starts_with("this RimZ cannot read the definition generated at the fire: "),
        "{reason}"
    );
    assert_eq!(host.stops.get(), 1);
}

/// `workflow cancel` reads only the record: it ends a run whose definition is missing, does
/// not parse, or is refused, stops its agent, and counts the visit as acting.
#[test]
fn cancel_needs_no_readable_definition() {
    let refused = STEP.replace("then='@done'", "then='build'");
    for broken in [None, Some("[workflow"), Some(refused.as_str())] {
        let (_dir, state, record) = setup(STEP);
        let host = FakeHost::new();
        advance_ok(&state, &record.id, &host);
        let spec = definition_file(&record);
        match broken {
            Some(text) => fs::write(&spec, text).unwrap(),
            None => fs::remove_file(&spec).unwrap(),
        }
        cancel(&state, &record.id, &host, Some("stop")).unwrap();
        let ended = records::load(&state.workflow(&record.id)).unwrap();
        assert_eq!(ended.state, RunState::Canceled, "{broken:?}");
        assert!(ended.outcome.unwrap().acted, "{broken:?}");
        assert_eq!(host.stops.get(), 1, "{broken:?}");
    }
}

/// The engine fault evaluations the run's journal holds.
fn faults(state: &StatePaths, record: &RunRecord) -> usize {
    records::journal(&state.workflow(&record.id))
        .unwrap()
        .iter()
        .filter(|entry| {
            matches!(&entry.event, JournalEvent::Evaluation { outcome, .. } if outcome == "fault")
        })
        .count()
}

/// A close whose off-lock stop failed stays staged; the next advance or cancel finishes it as
/// the first close meant, without a second fault, whatever the definition reads by then.
#[test]
fn a_close_whose_stop_failed_is_finished_by_the_next_advance_or_cancel() {
    let other = "[workflow]\nscope='worktree'\nstart='other'\n[agent.other]\nagent='claude'\nlifecycle='turn'\nprompt-file='prompt.md'\nthen='@done'\n";
    for (first_cancel, retry_cancel) in [(false, false), (false, true), (true, false), (true, true)]
    {
        let case = (first_cancel, retry_cancel);
        let (_dir, state, record) = setup(STEP);
        let host = FakeHost::new();
        advance_ok(&state, &record.id, &host);
        if !first_cancel {
            edit(&state, &record, other);
        }
        host.stop_error.set(true);
        if first_cancel {
            assert!(
                cancel(&state, &record.id, &host, Some("stop")).is_err(),
                "{case:?}"
            );
            edit(&state, &record, other);
        } else {
            assert!(advance(&state, &record.id, &host).is_err(), "{case:?}");
        }
        let mut staged = records::load(&state.workflow(&record.id)).unwrap();
        assert_eq!(staged.state, RunState::Active, "{case:?}");
        // The process that staged the close and failed its stop has exited.
        staged.attempt.as_mut().unwrap().host = Host {
            pid: u32::MAX,
            start: None,
        };
        update(&state, &staged);
        if retry_cancel {
            cancel(&state, &record.id, &host, Some("again")).unwrap();
        } else {
            advance_ok(&state, &record.id, &host);
        }
        let ended = records::load(&state.workflow(&record.id)).unwrap();
        if first_cancel {
            assert_eq!(ended.state, RunState::Canceled, "{case:?} {ended:#?}");
            assert_eq!(faults(&state, &record), 0, "{case:?}");
        } else {
            assert_misfit(&ended, "build", "it is no longer in the definition");
            assert_eq!(faults(&state, &record), 1, "{case:?}");
        }
        assert_eq!(host.stops.get(), 1, "{case:?}");
    }
}

/// Decision 21: a session held under its second rest check, with that check's name deleted
/// from the step's list and its table kept.
const TWO_CHECKS: &str = "[workflow]\nscope='worktree'\nstart='build'\n[agent.build]\nagent='claude'\nprompt-file='prompt.md'\nlifecycle='session'\nrest-check=['pr','ci']\nthen='pr'\nmax-visits=2\n[script.pr]\nrun='sh check.sh'\ntimeout='1h'\non.pending='build'\non.done='ci'\n[script.ci]\nrun='sh check.sh'\ntimeout='1h'\non.pending='build'\non.queued='@wait'\non.done='@done'\n";

fn held_under_ci(state: &StatePaths, record: &RunRecord, host: &FakeHost) {
    let live = advance_ok(state, &record.id, host);
    let mut holding = held(&live, false);
    holding.node = "ci".into();
    update(state, &holding);
    host.observed.borrow_mut().turn = Some(turn());
}

#[test]
fn the_judging_check_delisted_from_its_step_fails_the_run() {
    let (_dir, state, record) = setup(TWO_CHECKS);
    let host = FakeHost::new();
    held_under_ci(&state, &record, &host);
    edit(
        &state,
        &record,
        &TWO_CHECKS.replace("rest-check=['pr','ci']", "rest-check=['pr']"),
    );
    let ended = advance_ok(&state, &record.id, &host);
    assert_misfit(&ended, "ci", "it is no longer a rest check of build");
    assert!(ended.held.is_none(), "{ended:#?}");
    assert_eq!(host.stops.get(), 1);
}

#[test]
fn another_check_delisted_leaves_the_judging_check_holding_the_session() {
    let (dir, state, record) = setup(TWO_CHECKS);
    let host = FakeHost::new();
    held_under_ci(&state, &record, &host);
    check(dir.path(), json!({"outcome":"queued"}));
    edit(
        &state,
        &record,
        &TWO_CHECKS.replace("rest-check=['pr','ci']", "rest-check=['ci']"),
    );
    let waiting = advance_ok(&state, &record.id, &host);
    assert_eq!(
        (waiting.state, waiting.node.as_str()),
        (RunState::Active, "ci"),
        "{waiting:#?}"
    );
    assert!(waiting.held.is_some(), "{waiting:#?}");
    assert_eq!(host.stops.get(), 0);
}

/// A misfit after a capacity route fails the run for the misfit, not as a capacity give-up.
#[test]
fn a_misfit_after_a_capacity_route_fails_the_run_with_its_own_reason() {
    let text = "[workflow]\nscope='worktree'\nstart='build'\n[agent.build]\nagent='claude'\nlifecycle='turn'\nprompt-file='prompt.md'\nthen='@done'\non.capacity='watch'\n[script.watch]\nrun='cat answer.json'\ntimeout='1h'\non={queued='@wait',red='build'}\n";
    let (_dir, state, record) = setup(text);
    let spec: Definition = toml::from_str(text).unwrap();
    let paths = state.workflow(&record.id);
    let mut t = state::Transition {
        now: record.entered_at,
        record: record.clone(),
        spec: &spec,
        paths: &paths,
        entries: Vec::new(),
    };
    t.fail(
        None,
        "build",
        spec::Cause::Capacity,
        "old capacity wait".into(),
        json!({"waited_ms":5, "first":record.entered_at}),
    )
    .unwrap();
    assert_eq!(
        (t.record.node.as_str(), t.record.state),
        ("watch", RunState::Active)
    );
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    for entry in &t.entries {
        records::append(&paths, entry, &guard).unwrap();
    }
    records::write(&paths, &t.record, &guard).unwrap();
    drop(guard);
    edit(&state, &record, STEP);
    let ended = advance_ok(&state, &record.id, &FakeHost::new());
    assert_misfit(&ended, "watch", "it is no longer in the definition");
    assert_eq!(
        ended.outcome.as_ref().unwrap().ending,
        records::RunEnding::Failed
    );
}

/// A run whose definition is edited down to checks before it ends classifies `acted` by that
/// definition, where one that cannot be read counts any visit
/// (`cancel_needs_no_readable_definition`).
#[test]
fn acted_reads_a_checks_only_definition_by_its_steps() {
    let (_dir, state, record) = setup(STEP);
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    edit(
        &state,
        &record,
        &format!("[workflow]\nscope='worktree'\nstart='after'\n{AFTER}"),
    );
    cancel(&state, &record.id, &host, Some("stop")).unwrap();
    let ended = records::load(&state.workflow(&record.id)).unwrap();
    assert_eq!(ended.state, RunState::Canceled, "{ended:#?}");
    assert!(!ended.outcome.as_ref().unwrap().acted, "{ended:#?}");
}
