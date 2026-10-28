use super::shells::{LIVE, assert_group_stopped, live_group};
use super::*;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use rimz::store::workflow::{self as records, RunRecord, RunState};
use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};

fn command_fixture(runner: &str, command: &str) -> (Env, PathBuf) {
    fixture(
        &format!(
            "{HEADER}[checks.pr]\nrun='sh checks/pr.sh'\non.go='build'\n[steps.build]\n{runner}={}\ntimeout='1h'\nthen='report'\nmax-visits=2\n[checks.report]\nshell='true'\non={{pass='@done',fail='@failed'}}\n",
            toml::Value::String(command.into())
        ),
        "printf '%s\\n' '{\"outcome\":\"go\",\"key\":7,\"payload\":\"keep me\"}'",
    )
}

fn edit_record(env: &Env, id: &str, edit: impl FnOnce(&mut RunRecord)) {
    let state = env.state_path_for(&env.project_root);
    let paths = state.workflow(&id.parse().unwrap());
    let guard = rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    let mut record = records::load(&paths).unwrap();
    edit(&mut record);
    records::write(&paths, &record, &guard).unwrap();
}

fn ready_step(env: &Env, dir: &Path) -> String {
    let path = dir.join("workflow.toml");
    fs::write(
        &path,
        fs::read_to_string(&path)
            .unwrap()
            .replace("on.go='build'", "on.go='@user'"),
    )
    .unwrap();
    let id = start(env);
    settled(env, &id, "parked");
    edit_record(env, &id, |record| {
        record.state = RunState::Active;
        record.park = None;
        record.node = "build".into();
        record.entered_at = jiff::Timestamp::now();
    });
    id
}

fn accept_gate(listener: &UnixListener) -> UnixStream {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok((stream, _)) = listener.accept() {
            return stream;
        }
        assert!(
            Instant::now() < deadline,
            "command host did not reach its result boundary"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn successful_command_records_evidence_environment_and_accounting() {
    command_exit("exit 0", "ended", json!(0));
}

#[test]
fn nonzero_command_records_evidence_environment_and_accounting() {
    command_exit("exit 7", "died", json!(7));
}

#[test]
fn signaled_command_records_evidence_environment_and_accounting() {
    command_exit("kill -TERM $$", "died", Value::Null);
}

fn command_exit(ending: &str, outcome: &str, code: Value) {
    let shell = format!(
        "git commit --quiet --allow-empty -m 'command step'; pwd; printf '%s\\n' \"$RIMZ_WORKFLOW_DIR\" \"$RIMZ_WORKFLOW_RUN\" \"$RIMZ_PROJECT_ROOT\"; printf stderr >&2; if read line; then exit 99; fi; {ending}"
    );
    let (env, _) = command_fixture("shell", &shell);
    crate::common::git::init_repo(&env.project_root);
    let head = crate::common::git::git_stdout(&env.project_root, &["rev-parse", "HEAD"]);
    let id = start(&env);
    let value = settled(&env, &id, "done");
    let head_end = crate::common::git::git_stdout(&env.project_root, &["rev-parse", "HEAD"]);
    let attempts = events(&value, "attempt");
    assert_eq!(attempts.len(), 1, "{value:#}");
    let attempt = &attempts[0];
    assert_eq!(attempt["outcome"], outcome, "{value:#}");
    assert_eq!(attempt["code"], code);
    assert_eq!(attempt["seq"], 2);
    assert_eq!(attempt["visit"], 1);
    assert_eq!(attempt["key"], 7);
    assert_eq!(attempt["head_start"], head);
    assert_ne!(head_end, head);
    assert_eq!(attempt["head_end"], head_end);
    for field in [
        "handle", "run_id", "children", "limited", "asked", "cost_usd",
    ] {
        assert!(attempt[field].is_null(), "{field}: {attempt}");
    }
    let previous = &value["record"]["prev_attempt"];
    assert_eq!(previous["outcome"], outcome);
    assert_eq!(previous["code"], code);
    assert_eq!(previous["log"], attempt["log"]);
    assert_eq!(value["record"]["visits"]["build"]["total"], 1);
    assert_eq!(value["record"]["next_seq"], 4);
    let ctx = context(&value);
    assert_eq!(
        ctx["input"],
        json!({"outcome":"go", "key":7, "payload":"keep me"})
    );
    assert_eq!(ctx["prev_attempt"], *previous);
    let log_path = Path::new(attempt["log"].as_str().unwrap());
    assert_eq!(log_path.file_name().unwrap(), "2.step.log");
    let log = fs::read_to_string(log_path).unwrap();
    for expected in [
        env.project_root.to_str().unwrap(),
        value["dir"].as_str().unwrap(),
        &id,
        "stderr",
    ] {
        assert!(log.contains(expected), "{expected}: {log}");
    }
    let text = success(command(&env, &["workflow", "show", &id]));
    let code_text = code
        .as_i64()
        .map_or_else(|| "—".into(), |code| code.to_string());
    assert!(
        text.contains(&format!(
            "exit {code_text} HEAD {} → {}",
            &head[..8],
            &head_end[..8]
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!("log: {}", log_path.display())),
        "{text}"
    );
    let stats = assists_once(&env, "workflow_steps", 1);
    let steps: Vec<_> = stats["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["assist"] == "workflow_step")
        .collect();
    assert_eq!(steps.len(), 1, "{stats}");
    assert!(
        steps[0]["handle"].is_null() && steps[0]["run_id"].is_null(),
        "{stats}"
    );
}

#[test]
fn run_steps_pass_literal_argv_and_publish_file_digests() {
    let (env, dir) = command_fixture("run", "sh checks/action.sh 'two words' '$HOME' '*'");
    fs::write(
        dir.join("checks/action.sh"),
        "printf '%s\\n' \"$@\" > \"$RIMZ_WORKFLOW_DIR/notes/argv\"; printf out; printf err >&2",
    )
    .unwrap();
    let id = start(&env);
    let value = settled(&env, &id, "done");
    let work = Path::new(value["dir"].as_str().unwrap());
    let argv = fs::read_to_string(work.join("notes/argv"));
    assert_eq!(
        argv.as_deref().ok(),
        Some("two words\n$HOME\n*\n"),
        "{argv:?}"
    );
    let attempts = events(&value, "attempt");
    assert_eq!(attempts.len(), 1, "{value:#}");
    assert_eq!(attempts[0]["outcome"], "ended");
    assert_eq!(attempts[0]["digests"].as_object().unwrap().len(), 1);
    assert!(
        attempts[0]["digests"]["checks/action.sh"].is_string(),
        "{attempts:?}"
    );
    let log = fs::read_to_string(work.join("2.step.log")).unwrap();
    assert!(log.contains("out") && log.contains("err"), "{log}");
}

struct ResumeHost(u32);

impl Drop for ResumeHost {
    fn drop(&mut self) {
        let _ = kill(Pid::from_raw(self.0 as i32), Signal::SIGCONT);
    }
}

#[test]
fn command_host_timeout_settles_once_and_stops_the_group() {
    command_bound("timeout");
}

#[test]
fn another_advance_times_out_a_commands_stalled_host() {
    command_bound("external-timeout");
}

#[test]
fn command_host_deadline_settles_once_and_stops_the_group() {
    command_bound("deadline");
}

#[test]
fn another_advance_applies_a_deadline_to_a_commands_stalled_host() {
    command_bound("external-deadline");
}

#[test]
fn command_cancel_settles_once_and_stops_the_group() {
    command_bound("cancel");
}

#[test]
fn a_dead_command_host_is_canceled_without_repeating_the_action() {
    command_bound("dead-host");
}

fn command_bound(mode: &str) {
    let (env, _) = command_fixture("shell", LIVE);
    crate::common::git::init_repo(&env.project_root);
    let head = crate::common::git::git_stdout(&env.project_root, &["rev-parse", "HEAD"]);
    let id = start(&env);
    let (live, child, descendant) = live_group(&env, &id, "build");
    let attempt = &live["record"]["attempt"];
    assert_eq!(attempt["kind"], "script");
    assert_eq!(attempt["head_start"], head, "{attempt:#}");
    assert_eq!(context(&live)["visit"], 1);
    assert_eq!(context(&live)["visits"], 1);
    let host = live["record"]["attempt"]["host"]["pid"].as_u64().unwrap() as u32;
    let external = mode.starts_with("external");
    let resume = external.then_some(ResumeHost(host));
    edit_record(&env, &id, |record| {
        if external {
            kill(Pid::from_raw(host as i32), Signal::SIGSTOP).unwrap();
        }
        if mode.contains("timeout") {
            record.attempt.as_mut().unwrap().started_at = jiff::Timestamp::UNIX_EPOCH;
        } else if mode.contains("deadline") {
            record.deadline = Some(jiff::Timestamp::UNIX_EPOCH);
        } else if mode == "dead-host" {
            kill(Pid::from_raw(host as i32), Signal::SIGKILL).unwrap();
        }
    });
    if mode == "dead-host" {
        until(&env, &id, |_| !rimz::proc::process_is_live(host, None));
    }
    if external || mode == "dead-host" || mode == "cancel" {
        success(command(
            &env,
            &[
                "workflow",
                if mode == "cancel" {
                    "cancel"
                } else {
                    "advance"
                },
                &id,
            ],
        ));
    }
    let state = if mode == "cancel" {
        "canceled"
    } else if mode.contains("deadline") {
        "parked"
    } else {
        "done"
    };
    let value = settled(&env, &id, state);
    assert_group_stopped(child, descendant);
    drop(resume);
    let attempts = events(&value, "attempt");
    assert_eq!(attempts.len(), 1, "{mode}: {value:#}");
    let outcome = if mode.contains("timeout") {
        "timeout"
    } else {
        "canceled"
    };
    assert_eq!(attempts[0]["outcome"], outcome, "{mode}: {value:#}");
    assert_eq!(value["record"]["prev_attempt"]["outcome"], outcome);
    assert_eq!(value["record"]["visits"]["build"]["total"], 1);
    assert!(attempts[0]["code"].is_null());
    assert_eq!(
        attempts[0]["head_end"],
        if matches!(mode, "timeout" | "deadline") {
            json!(head)
        } else {
            Value::Null
        }
    );
    assert_eq!(
        attempts[0]["log"],
        Path::new(value["dir"].as_str().unwrap())
            .join("2.step.log")
            .to_str()
            .unwrap()
    );
    if mode.contains("deadline") {
        assert_eq!(value["record"]["input"]["outcome"], "deadline");
        assert_eq!(value["record"]["deadline_fired"], true);
    }
    if mode == "dead-host" {
        assert!(
            attempts[0]["reason"].as_str().unwrap().contains("host"),
            "{attempts:?}"
        );
    }
    if mode == "cancel" {
        assert_eq!(events(&value, "cancel").len(), 1);
        success(command(&env, &["workflow", "advance", &id]));
        assert_eq!(show(&env, &id), value);
    }
}

#[test]
fn command_spawn_failure_records_a_reason_without_a_step_assist() {
    command_failure(false);
}

#[test]
fn command_log_failure_records_a_reason_without_a_step_assist() {
    command_failure(true);
}

fn command_failure(log_failure: bool) {
    let (env, dir) = if log_failure {
        command_fixture("shell", "true")
    } else {
        command_fixture("run", "rimz-workflow-command-does-not-exist")
    };
    let id = ready_step(&env, &dir);
    if log_failure {
        let paths = env
            .state_path_for(&env.project_root)
            .workflow(&id.parse().unwrap());
        fs::create_dir(paths.work.join("2.step.log")).unwrap();
    }
    success(command(&env, &["workflow", "advance", &id]));
    let value = show(&env, &id);
    let attempts = events(&value, "attempt");
    assert_eq!(attempts.len(), 1, "{value:#}");
    assert_eq!(attempts[0]["outcome"], "died");
    assert!(attempts[0]["code"].is_null());
    assert_eq!(attempts[0]["digests"], json!({}));
    let reason = attempts[0]["reason"].as_str().unwrap();
    assert!(!reason.is_empty(), "{attempts:?}");
    assert!(
        reason.contains(if log_failure { "2.step.log" } else { "running" }),
        "{reason}"
    );
    let text = success(command(&env, &["workflow", "show", &id]));
    assert!(text.contains(&format!("reason: {reason}")), "{text}");
    assert!(text.contains("    exit — HEAD — → —\n"), "{text}");
    let stats = assists(&env);
    assert!(
        stats["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|event| event["assist"] != "workflow_step"),
        "{stats}"
    );
}

#[test]
fn exited_command_keeps_its_status_when_its_timeout_falls_due_before_settlement() {
    exited_command(false, false);
}

#[test]
fn exited_command_keeps_its_status_when_the_deadline_falls_due_before_settlement() {
    exited_command(true, false);
}

#[test]
fn another_advance_leaves_an_exited_commands_due_timeout_to_its_host() {
    exited_command(false, true);
}

#[test]
fn another_advance_leaves_an_exited_commands_due_deadline_to_its_host() {
    exited_command(true, true);
}

fn exited_command(deadline: bool, contender: bool) {
    let (env, dir) = command_fixture("shell", "exit 0");
    let id = ready_step(&env, &dir);
    let socket = env.runtime_root.join("command-result.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut advance = env
        .rimz()
        .args(["workflow", "advance", &id])
        .env("RIMZ_TEST_WORKFLOW_RESULT_GATE", &socket)
        .spawn()
        .unwrap();
    let mut gate = accept_gate(&listener);
    edit_record(&env, &id, |record| {
        if deadline {
            record.deadline = Some(jiff::Timestamp::UNIX_EPOCH);
        } else {
            record.attempt.as_mut().unwrap().started_at = jiff::Timestamp::UNIX_EPOCH;
        }
    });
    if contender {
        success(command(&env, &["workflow", "advance", &id]));
        let paths = env
            .state_path_for(&env.project_root)
            .workflow(&id.parse().unwrap());
        let record = records::load(&paths).unwrap();
        assert!(
            record.attempt.is_some() && record.prev_attempt.is_none(),
            "{record:#?}"
        );
    }
    gate.write_all(&[1]).unwrap();
    if !deadline {
        accept_gate(&listener).write_all(&[1]).unwrap();
    }
    assert!(advance.wait().unwrap().success());
    let value = settled(&env, &id, if deadline { "parked" } else { "done" });
    assert_eq!(
        value["record"]["prev_attempt"]["outcome"], "ended",
        "{value:#}"
    );
    assert_eq!(value["record"]["prev_attempt"]["code"], 0);
    assert_eq!(value["record"]["deadline_fired"], deadline);
}

#[test]
fn an_interrupted_command_whose_deadline_was_extended_is_canceled_then_checked() {
    let (env, dir) = command_fixture("shell", LIVE);
    let id = ready_step(&env, &dir);
    let socket = env.runtime_root.join("command-interrupted.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut advance = env
        .rimz()
        .args(["workflow", "advance", &id])
        .env("RIMZ_TEST_WORKFLOW_RESULT_GATE", &socket)
        .spawn()
        .unwrap();
    let (_, child, descendant) = live_group(&env, &id, "build");
    edit_record(&env, &id, |record| {
        record.deadline = Some(jiff::Timestamp::UNIX_EPOCH)
    });
    let mut gate = accept_gate(&listener);
    edit_record(&env, &id, |record| {
        record.deadline = Some("2099-01-01T00:00:00Z".parse().unwrap())
    });
    gate.write_all(&[1]).unwrap();
    accept_gate(&listener).write_all(&[1]).unwrap();
    assert!(advance.wait().unwrap().success());
    let value = settled(&env, &id, "done");
    assert_group_stopped(child, descendant);
    assert_eq!(
        value["record"]["prev_attempt"]["outcome"], "canceled",
        "{value:#}"
    );
    assert_eq!(value["record"]["deadline_fired"], false);
    assert_eq!(events(&value, "attempt").len(), 1, "{value:#}");
}
