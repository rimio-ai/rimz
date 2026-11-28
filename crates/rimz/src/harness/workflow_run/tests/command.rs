//! The command host over real processes, by the means `check_run`'s tests use: `sh -c` in its
//! own process group.

use serde_json::{Value, json};

use super::*;
use crate::store::workflow_run::Process;

/// Start `body`, run its hosts, and give the run's result.
fn hosted(h: &Harness, body: &str) -> Value {
    h.start(script(body));
    h.run_hosts();
    h.done()
}

#[test]
fn an_exit_0_answers_the_code_its_stdout_apart_from_stderr_and_the_log_of_both() {
    let h = Harness::new();
    let result = hosted(
        &h,
        "return await shell({ command: 'echo out; echo err >&2' })",
    );
    let log = h.state.workflow_run(&h.id).step(1).join("output.log");
    assert_eq!(
        result,
        json!({ "code": 0, "stdout": "out\n", "output": log.display().to_string() })
    );
    let both = std::fs::read_to_string(&log).unwrap();
    assert!(both.contains("out\n") && both.contains("err\n"), "{both:?}");
}

#[test]
fn a_nonzero_exit_fails_exit_with_its_code_stdout_and_log() {
    let h = Harness::new();
    let result = hosted(
        &h,
        "try { await shell({ command: 'echo red; exit 3' }) } catch (e) { return [e.cause, e.code, e.stdout, e.reason] }",
    );
    assert_eq!(result, json!(["exit", 3, "red\n", "the command exited 3"]));
}

#[test]
fn a_command_a_signal_killed_fails_exit_naming_the_signal() {
    let h = Harness::new();
    let result = hosted(
        &h,
        "try { await shell({ command: 'kill -9 $$' }) } catch (e) { return [e.cause, e.code === undefined, e.reason] }",
    );
    assert_eq!(
        result,
        json!(["exit", true, "the command was killed by signal 9"])
    );
}

#[test]
fn the_command_runs_in_its_cwd_with_its_context_and_the_runs_variables() {
    let h = Harness::new();
    std::fs::create_dir_all(h.checkout.join("sub")).unwrap();
    let mut input = h.input(script(
        "return (await shell({ command: 'cat \"$RIMZ_WORKFLOW_DIR/context.json\"; echo; echo ---; echo \"$RIMZ_WORKFLOW_FILES\"; echo \"$RIMZ_WORKFLOW_RUN\"; pwd -P; cat <&0 2>/dev/null | wc -c', cwd: 'sub', input: { n: 1 } })).stdout",
    ));
    input.args = Some(json!({ "goal": "g" }));
    h.start_with(input);
    h.run_hosts();
    let stdout = h.done();
    let (context, rest) = stdout.as_str().unwrap().split_once("\n---\n").unwrap();
    let context: Value = serde_json::from_str(context).unwrap();
    let lines: Vec<&str> = rest.lines().collect();
    assert_eq!(
        context,
        json!({ "run": h.id.as_str(), "step": 1, "args": { "goal": "g" }, "input": { "n": 1 } })
    );
    let files = h.state.workflow_run(&h.id).files(1).join("root/workflow");
    assert_eq!(lines[0], files.display().to_string());
    assert_eq!(lines[1], h.id.as_str());
    assert_eq!(
        std::path::Path::new(lines[2]),
        h.checkout.join("sub").canonicalize().unwrap()
    );
    assert_eq!(lines[3].trim(), "0", "stdin is closed");
}

#[test]
fn a_command_past_its_timeout_is_killed_with_its_group() {
    let h = Harness::new();
    h.start(script(
        "try { await shell({ command: 'sleep 30 & echo $! > \"$RIMZ_WORKFLOW_DIR/child\"; wait', timeout: '1m' }) } catch (e) { return [e.cause, e.reason] }",
    ));
    let pid_file = h.state.workflow_run(&h.id).step(1).join("child");
    let fake = std::rc::Rc::clone(&h.fake);
    let marker = pid_file.clone();
    h.fake.hook("now", move || {
        let started = std::fs::read_to_string(&marker).is_ok_and(|pid| !pid.trim().is_empty());
        if started {
            fake.later(jiff::SignedDuration::from_mins(2));
        }
        started
    });
    h.run_hosts();
    assert_eq!(
        h.done(),
        json!(["timeout", "the call ran past its timeout of 1m"])
    );
    let pid: u32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while crate::proc::process_is_live(pid, None) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        !crate::proc::process_is_live(pid, None),
        "the group's background child survived"
    );
}

#[test]
fn a_host_whose_claim_is_refused_runs_nothing() {
    let h = Harness::new();
    h.start(script(
        "return (await shell({ command: 'echo ran > \"$RIMZ_WORKFLOW_DIR/ran\"' })).stdout",
    ));
    caught(|| cancel(&h.state, &h.room, &h.id)).unwrap();
    h.run_hosts();
    assert_eq!(h.ending(), Some(Ending::Canceled));
    assert!(!h.state.workflow_run(&h.id).step(1).join("ran").exists());
}

#[test]
fn a_host_of_another_attempt_or_beside_another_live_host_runs_nothing() {
    let h = Harness::new();
    h.start(script(
        "return (await shell({ command: 'echo ran > \"$RIMZ_WORKFLOW_DIR/ran\"' })).stdout",
    ));
    let (pin, host) = h.fake.take_hosts().remove(0);
    let run_as = |process: &Process, attempt: u32| {
        h.fake.me_is(process);
        caught(|| super::super::command::host(&h.state, &h.room, &h.id, pin.step, attempt))
            .unwrap();
    };
    run_as(&host, pin.attempt + 1);
    run_as(&h.fake.spawn(), pin.attempt);
    assert!(!h.state.workflow_run(&h.id).step(1).join("ran").exists());
    assert!(
        matches!(h.work(1), Work::Command { host: claimer, child: None, .. } if claimer == host)
    );
    run_as(&host, pin.attempt);
    assert_eq!(h.done(), json!(""));
}

#[test]
fn a_child_alive_without_its_host_is_killed_before_the_step_starts_again() {
    let h = Harness::new();
    h.start(script(
        "return (await shell({ command: 'echo $$ >> \"$RIMZ_WORKFLOW_DIR/pids\"; sleep 30', timeout: '1h' })).stdout",
    ));
    let pids = h.state.workflow_run(&h.id).step(1).join("pids");
    // The host dies at its first look at the clock, after the child is published.
    h.fake.crash_at("now", h.fake.calls("now") + 1, false);
    h.run_hosts();
    let Work::Command {
        child: Some(child), ..
    } = h.work(1)
    else {
        panic!("the child was not published: {:?}", h.work(1));
    };
    assert!(h.fake.alive(&child));
    // A kill lands a moment after it is sent; until then the step waits on the child.
    for _ in 0..100 {
        h.advance();
        if h.record().open[&1].attempt == 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(!h.fake.alive(&child), "the orphaned child was killed");
    assert_eq!(h.record().open[&1].attempt, 2);
    assert_eq!(std::fs::read_to_string(&pids).unwrap().lines().count(), 1);
    caught(|| cancel(&h.state, &h.room, &h.id)).unwrap();
}

#[test]
fn a_step_directory_that_cannot_be_made_leaves_the_step_due_again() {
    let h = Harness::new();
    h.start(script(
        "await watch({ when: 'pr=merged' })\nreturn (await shell({ command: 'echo out' })).stdout",
    ));
    let steps = h.state.workflow_run(&h.id).home.join("steps");
    std::fs::write(&steps, "").unwrap();
    h.fake.set_look(
        "pr=merged",
        super::super::room::Look {
            truth: Some(true),
            readings: Default::default(),
        },
    );
    assert!(advance(&h.state, &h.room, &h.id).is_err());
    assert!(matches!(h.work(2), Work::Due), "{:?}", h.work(2));
    std::fs::remove_file(&steps).unwrap();
    h.advance();
    h.run_hosts();
    assert_eq!(h.done(), json!("out\n"));
}

#[test]
fn a_record_that_does_not_read_while_the_command_runs_does_not_stop_it() {
    let h = Harness::new();
    h.start(script(
        "return (await shell({ command: 'record=\"$RIMZ_WORKFLOW_DIR/../../record.json\"; sleep 0.3; cp \"$record\" \"$RIMZ_WORKFLOW_DIR/saved\"; echo torn > \"$record\"; sleep 0.3; cp \"$RIMZ_WORKFLOW_DIR/saved\" \"$record\"; echo out' })).stdout",
    ));
    h.run_hosts();
    assert_eq!(h.done(), json!("out\n"));
}
