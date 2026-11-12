use super::*;
use crate::store::workflow::{self as records, AttemptOutcome, RunState};

#[test]
fn workflow_definition_and_loop_task_reach_command_run_and_shell_children() {
    let text = r#"
[workflow]
scope='worktree'
start='command'
[steps.command]
shell='printf "%s|%s" "$RIMZ_WORKFLOW_DEFINITION" "$RIMZ_LOOP_TASK"'
timeout='1m'
then='run'
[checks.run]
run='sh checks/env.sh'
on.ok='shell'
[checks.shell]
shell='printf "%s|%s" "$RIMZ_WORKFLOW_DEFINITION" "$RIMZ_LOOP_TASK" > "$RIMZ_WORKFLOW_DIR/shell-definition"'
on={pass='@done',fail='@failed'}
"#;
    let (dir, state, mut record) = super::attempts::setup(text);
    record.spec_dir = dir.path().join("workflow definitions");
    record.task = Some("nightly".into());
    let booker = crate::agents::LaunchedBy {
        kind: crate::ids::AgentKind::new_unchecked("claude"),
        agent_id: "booker".into(),
    };
    record.fire = Some(records::RunFire::Scheduled {
        task: "nightly".into(),
        trigger: None,
        booked_by: vec![booker],
    });
    fs::create_dir_all(record.spec_dir.join("checks")).unwrap();
    fs::write(
        record.spec_dir.join("checks/env.sh"),
        "printf '{\"outcome\":\"ok\",\"definition\":\"%s\",\"task\":\"%s\",\"booked_by\":%s}' \"$RIMZ_WORKFLOW_DEFINITION\" \"$RIMZ_LOOP_TASK\" \"${RIMZ_LOOP_BOOKED_BY:-null}\"\n",
    )
    .unwrap();
    let paths = state.workflow(&record.id);
    let guard = crate::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    records::write(&paths, &record, &guard).unwrap();
    drop(guard);
    advance(&state, &record.id, &FakeHost::new()).unwrap();
    let definition = record.spec_dir.display().to_string();
    let seen = format!("{definition}|nightly");
    assert_eq!(fs::read_to_string(paths.step_log(1)).unwrap(), seen);
    assert_eq!(
        fs::read_to_string(paths.work.join("shell-definition")).unwrap(),
        seen
    );
    let journal = records::journal(&paths).unwrap();
    assert!(
        journal.iter().any(|row| matches!(&row.event,
        records::JournalEvent::Evaluation { object, .. }
        if row.node == "run" && object["definition"] == definition && object["task"] == "nightly"))
    );
    let booked_by = journal.iter().find_map(|row| match &row.event {
        records::JournalEvent::Evaluation { object, .. } if row.node == "run" => {
            Some(object["booked_by"].clone())
        }
        _ => None,
    });
    assert_eq!(
        booked_by,
        Some(serde_json::json!([{"kind": "claude", "agent_id": "booker"}])),
        "a check names the chain of the booking that fired it"
    );
}

#[test]
fn a_run_that_does_not_split_ends_invalid_with_the_command_error() {
    let (_dir, state, record) = super::attempts::setup(
        "[workflow]\nscope='worktree'\nstart='run'\n[checks.run]\nrun=\"echo 'open\"\non.ok='@done'\n",
    );
    advance(&state, &record.id, &FakeHost::new()).unwrap();
    let journal = records::journal(&state.workflow(&record.id)).unwrap();
    assert!(
        journal.iter().any(|row| matches!(&row.event,
        records::JournalEvent::Evaluation { outcome, object, .. }
        if outcome == "invalid"
            && object["reason"] == "invalid run command: use a nonempty, correctly quoted argv")),
        "{journal:#?}"
    );
}

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
fn command_step_cycles_stop_at_total_and_outcome_caps_without_consuming_a_sequence() {
    let mut results = Vec::new();
    for keyed in [false, true] {
        let bound = if keyed {
            "max-visits-per-outcome"
        } else {
            "max-visits"
        };
        let text = format!(
            "[workflow]\nscope='worktree'\nstart='build'\n[steps.build]\nshell='true'\ntimeout='1h'\nthen='again'\nmax-visits={}\n{}\n[checks.again]\nshell='true'\non={{pass='build',fail='@failed'}}\n",
            if keyed { 3 } else { 1 },
            if keyed {
                "max-visits-per-outcome=1"
            } else {
                ""
            }
        );
        let (_dir, state, record) = super::attempts::setup(&text);
        let host = FakeHost::new();
        *host.occupancy.borrow_mut() = Occupancy::Blocked(vec![]);
        let result = advance(&state, &record.id, &host);
        let ended = records::load(&state.workflow(&record.id)).unwrap();
        let valid = result.is_ok()
            && ended.state == RunState::Failed
            && ended
                .visits
                .get("build")
                .is_some_and(|visits| visits.total == if keyed { 2 } else { 1 })
            && ended.next_seq == if keyed { 5 } else { 3 }
            && ended
                .input
                .as_ref()
                .is_some_and(|input| input["bound"] == bound);
        results.push((keyed, valid, format!("{result:?}; {ended:?}")));
    }
    assert!(results.iter().all(|(_, valid, _)| *valid), "{results:#?}");
}
