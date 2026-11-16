use super::*;
use crate::store::workflow::{self as records, AttemptOutcome, RunState};

#[test]
fn workflow_definition_and_loop_task_reach_command_run_and_shell_children() {
    let text = r#"
[workflow]
scope='worktree'
start='command'
[command.command]
shell='printf "%s|%s|%s" "$RIMZ_WORKFLOW_DEFINITION" "$RIMZ_LOOP_TASK" "$RIMZ_LOOP_TRIGGER"'
timeout='1m'
then='run'
[script.run]
timeout = '365d'
run='sh checks/env.sh'
on.ok='shell'
[test.shell]
timeout = '365d'
shell='printf "%s|%s" "$RIMZ_WORKFLOW_DEFINITION" "$RIMZ_LOOP_TASK" > "$RIMZ_WORKFLOW_DIR/shell-definition"'
on={pass='@done',fail='@failed'}
"#;
    let (dir, state, mut record) = super::attempts::setup(text);
    let deployed = dir.path().join("workflow definitions");
    fs::create_dir_all(deployed.join("checks")).unwrap();
    fs::write(crate::disk::paths::workflow_spec_file(&deployed), text).unwrap();
    record.spec_dir = Some(deployed.clone());
    record.task = Some("nightly".into());
    record.prompt = Some("the task's prompt".into());
    let booker = crate::agents::LaunchedBy {
        kind: crate::ids::AgentKind::new_unchecked("claude"),
        agent_id: "booker".into(),
    };
    record.fire = Some(records::RunFire::Scheduled {
        task: "nightly".into(),
        trigger: None,
        evidence: Some("Rule `nightly` fired here. deploy.done".into()),
        booked_by: vec![booker],
    });
    fs::write(
        deployed.join("checks/env.sh"),
        "printf '{\"outcome\":\"ok\",\"definition\":\"%s\",\"task\":\"%s\",\"booked_by\":%s}' \"$RIMZ_WORKFLOW_DEFINITION\" \"$RIMZ_LOOP_TASK\" \"${RIMZ_LOOP_BOOKED_BY:-null}\"\n",
    )
    .unwrap();
    let paths = state.workflow(&record.id);
    let guard = crate::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    records::write(&paths, &record, &guard).unwrap();
    drop(guard);
    advance(&state, &record.id, &FakeHost::new()).unwrap();
    let definition = deployed.display().to_string();
    let seen = format!("{definition}|nightly");
    assert_eq!(
        fs::read_to_string(paths.step_log(1)).unwrap(),
        format!("{seen}|Rule `nightly` fired here. deploy.done"),
        "a command reads the fire's evidence as its trigger, not the run's prompt"
    );
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
fn a_run_that_does_not_split_is_refused_and_the_run_waits() {
    let (_dir, state, record) = super::attempts::setup(
        "[workflow]\nscope='worktree'\nstart='run'\n[script.run]\ntimeout='365d'\nrun=\"echo 'open\"\non.ok='@done'\n",
    );
    let paths = state.workflow(&record.id);
    let before = (
        fs::read(&paths.record).unwrap(),
        fs::read(&paths.journal).ok(),
    );
    let refused = advance(&state, &record.id, &FakeHost::new()).unwrap_err();
    assert_eq!(
        refused.to_string(),
        "test: script.run.run: invalid run command: use a nonempty, correctly quoted argv"
    );
    assert_eq!(
        (
            fs::read(&paths.record).unwrap(),
            fs::read(&paths.journal).ok()
        ),
        before
    );
}

#[test]
fn command_steps_bypass_every_agent_host_operation_and_prompt() {
    let mut results = Vec::new();
    for runner in ["run", "shell"] {
        let text = format!(
            "[workflow]\nscope='worktree'\nstart='build'\n[command.build]\n{runner}='true'\ntimeout='1h'\nthen='done'\n[test.done]\ntimeout='365d'\nshell='true'\non={{pass='@done',fail='@failed'}}\n"
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
fn command_step_cycles_stop_at_the_cap_without_consuming_a_sequence() {
    let mut results = Vec::new();
    for max in [1, 2] {
        let text = format!(
            "[workflow]\nscope='worktree'\nstart='build'\n[command.build]\nshell='true'\ntimeout='1h'\nthen='again'\nmax-visits={max}\n[test.again]\ntimeout='365d'\nshell='true'\non={{pass='build',fail='@failed'}}\n"
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
                .is_some_and(|visits| *visits == max)
            && ended.next_seq == 2 * max + 1
            && ended
                .input
                .as_ref()
                .is_some_and(|input| input["cause"] == "visits");
        results.push((max, valid, format!("{result:?}; {ended:?}")));
    }
    assert!(results.iter().all(|(_, valid, _)| *valid), "{results:#?}");
}

/// A hosted child's timeout is the definition's as it is now: one extended while the child
/// runs does not kill it at the old value, and one shortened ends it timed out.
#[test]
fn a_running_childs_timeout_follows_the_definition_as_it_is_now() {
    let command = |timeout: &str, sleep: u32| {
        format!(
            "[workflow]\nscope='worktree'\nstart='work'\n[command.work]\ntimeout='{timeout}'\nshell='cp edited.toml workflow.toml; sleep {sleep}'\nthen='after'\n[test.after]\ntimeout='365d'\nshell='true'\non={{pass='@done',fail='@failed'}}\n"
        )
    };
    let check = |timeout: &str, sleep: u32| {
        format!(
            "[workflow]\nscope='worktree'\nstart='work'\n[script.work]\ntimeout='{timeout}'\nshell='cp edited.toml workflow.toml; sleep {sleep}; printf %s {{\\\"outcome\\\":\\\"ok\\\"}}'\non.ok='@done'\n"
        )
    };
    for (kind, text) in [
        ("command", &command as &dyn Fn(&str, u32) -> String),
        ("check", &check),
    ] {
        for (from, to, sleep, ended) in [("1s", "1h", 3, true), ("1h", "1s", 30, false)] {
            let case = (kind, from, to);
            let (dir, state, record) = super::attempts::setup(&text(from, sleep));
            fs::write(dir.path().join("edited.toml"), text(to, sleep)).unwrap();
            let started = std::time::Instant::now();
            advance(&state, &record.id, &FakeHost::new()).unwrap();
            let settled = records::load(&state.workflow(&record.id)).unwrap();
            let journal = records::journal(&state.workflow(&record.id)).unwrap();
            if ended {
                assert_eq!(settled.state, RunState::Done, "{case:?} {journal:#?}");
            } else {
                assert_eq!(settled.state, RunState::Failed, "{case:?} {journal:#?}");
                assert!(
                    started.elapsed() < std::time::Duration::from_secs(20),
                    "{case:?}"
                );
                let timed_out = journal.iter().any(|row| match &row.event {
                    records::JournalEvent::Attempt { outcome, .. } => {
                        *outcome == AttemptOutcome::Timeout
                    }
                    records::JournalEvent::Evaluation { outcome, .. } => outcome == "timeout",
                    _ => false,
                });
                assert!(timed_out, "{case:?} {journal:#?}");
            }
        }
    }
}
