use super::*;

fn shell_fixture(shell: &str, fields: &str) -> (Env, PathBuf) {
    fixture(
        &format!(
            "{HEADER}[test.pr]\nshell={}\non.pass='@done'\non.fail='@failed'\n{fields}",
            toml::Value::String(shell.into())
        ),
        "",
    )
}

pub(super) fn live_group(env: &Env, id: &str, node: &str) -> (Value, u32, u32) {
    let value = until(env, id, |value| {
        let attempt = &value["record"]["attempt"];
        attempt["node"] == node
            && attempt["child"]["pid"].is_number()
            && fs::read_to_string(Path::new(value["dir"].as_str().unwrap()).join("descendant.pid"))
                .ok()
                .is_some_and(|pid| pid.trim().parse::<u32>().is_ok())
    });
    let child = value["record"]["attempt"]["child"]["pid"].as_u64().unwrap() as u32;
    let descendant =
        fs::read_to_string(Path::new(value["dir"].as_str().unwrap()).join("descendant.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
    (value, child, descendant)
}

pub(super) fn assert_group_stopped(child: u32, descendant: u32) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while [child, descendant]
        .into_iter()
        .any(|pid| rimz::proc::process_is_live(pid, None))
    {
        assert!(
            Instant::now() < deadline,
            "shell group must stop, including descendants"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub(super) const LIVE: &str = "sleep 60 & echo $! > \"$RIMZ_WORKFLOW_DIR/descendant.pid\"; wait";

fn expire_check(env: &Env, value: &Value) {
    let mut record = value["record"].clone();
    record["entered_at"] =
        json!((jiff::Timestamp::now() - jiff::SignedDuration::from_secs(3601)).to_string());
    let state =
        rimz::StatePaths::for_project_root_under(&env.project_root, &env.rimz_home()).unwrap();
    let paths = state.workflow(&record["id"].as_str().unwrap().parse().unwrap());
    fs::write(paths.record, serde_json::to_vec(&record).unwrap()).unwrap();
}

#[test]
fn shell_exit_codes_log_both_streams_and_inherit_the_key() {
    for (ending, code, reason) in [
        ("exit 0", json!(0), None),
        ("exit 7", json!(7), Some("exited 7")),
    ] {
        let shell = format!(
            "pwd; printf '%s\\n' \"$RIMZ_WORKFLOW_DIR\" \"$RIMZ_WORKFLOW_RUN\"; printf stderr >&2; {ending}"
        );
        let spec = format!(
            "{}[script.seed]\nrun='sh checks/pr.sh'\non.go='pr'\n[test.pr]\nshell={}\non={{pass='@done',fail='@done'}}\n",
            HEADER.replace("start = 'pr'", "start = 'seed'"),
            toml::Value::String(shell)
        );
        let (env, _) = fixture(&spec, "printf '%s\\n' '{\"outcome\":\"go\",\"key\":7}'\n");
        let id = start(&env);
        let value = settled(&env, &id, "done");
        let log = Path::new(value["dir"].as_str().unwrap()).join("2.check.log");
        // The record keeps the path the check wrote, in the live home the run has since left.
        let recorded = env
            .state_path_for(&env.project_root)
            .workflow(&id.parse().unwrap())
            .work
            .join("2.check.log");
        let mut expected = json!({
            "outcome":if code == 0 {"pass"} else {"fail"}, "code":code, "log":recorded, "key":7
        });
        if let Some(reason) = reason {
            expected["reason"] = json!(reason);
        }
        assert_eq!(value["record"]["input"], expected);
        let evaluation = &events(&value, "evaluation")[1];
        assert_eq!(evaluation["origin"], "check");
        assert_eq!(evaluation["digests"], json!({}));
        assert!(evaluation["stderr_tail"].is_null());
        let output = fs::read_to_string(log).unwrap();
        assert!(
            output.contains(env.project_root.to_str().unwrap()),
            "{output}"
        );
        assert!(
            output.contains(recorded.parent().unwrap().to_str().unwrap()),
            "{output}"
        );
        assert!(
            output.contains(&id) && output.contains("stderr"),
            "{output}"
        );
    }
}

#[test]
fn a_test_killed_by_a_signal_dies_and_never_answers_fail() {
    let spec = format!(
        "{}[script.seed]\nrun='sh checks/pr.sh'\non.go='pr'\n[test.pr]\nshell='printf stderr >&2; kill -TERM $$'\non={{pass='@done',fail='@done'}}\n",
        HEADER.replace("start = 'pr'", "start = 'seed'"),
    );
    let (env, _) = fixture(&spec, "printf '%s\\n' '{\"outcome\":\"go\",\"key\":7}'\n");
    let id = start(&env);
    let value = settled(&env, &id, "failed");
    let log = Path::new(value["dir"].as_str().unwrap()).join("2.check.log");
    let input = &value["record"]["input"];
    assert_eq!(
        (
            &input["outcome"],
            &input["cause"],
            &input["node"],
            &input["reason"]
        ),
        (
            &json!("failed"),
            &json!("died"),
            &json!("pr"),
            &json!("killed by signal")
        )
    );
    let recorded = input["detail"]["log"].as_str().unwrap();
    assert_eq!(moved(recorded), log, "{input:#}");
    assert_eq!(input["detail"], json!({"log": recorded}));
    assert_eq!(input["key"], 7);
    assert!(fs::read_to_string(log).unwrap().contains("stderr"));
}

#[test]
fn shell_fail_waits_without_journaling_a_tail_and_a_log_error_dies() {
    let (env, dir) = shell_fixture("seq 1 5000; printf stderr >&2; exit 3", "timeout='1h'\n");
    let file = dir.join("workflow.toml");
    fs::write(
        &file,
        fs::read_to_string(&file)
            .unwrap()
            .replace("on.fail='@failed'", "on.fail='@wait'"),
    )
    .unwrap();
    let id = start(&env);
    let value = until(&env, &id, |v| {
        v["record"]["wait"]["outcome"] == "fail" && v["record"]["attempt"].is_null()
    });
    let input = &value["record"]["input"];
    assert_eq!(input["code"], 3);
    assert!(input.get("output").is_none());
    assert!(events(&value, "evaluation")[0]["stderr_tail"].is_null());
    let log = fs::read_to_string(input["log"].as_str().unwrap()).unwrap();
    assert!(log.contains("1\n2\n3\n") && log.contains("4999\n5000\n") && log.contains("stderr"));
    let text = success(command(&env, &["workflow", "show", &id]));
    assert!(text.lines().any(|line| line.contains(": fail")), "{text}");
    assert!(!text.contains("said fail"), "{text}");
    fs::create_dir(Path::new(value["dir"].as_str().unwrap()).join("2.check.log")).unwrap();
    success(command(&env, &["workflow", "advance", &id]));
    let died = settled(&env, &id, "failed");
    assert_eq!(died["record"]["input"]["cause"], "died");
    assert!(
        died["record"]["input"]["reason"]
            .as_str()
            .unwrap()
            .contains("2.check.log")
    );
}

#[test]
fn a_shell_spawn_error_dies() {
    let pending = toml::Value::String(r#"printf '{"outcome":"pending"}'"#.into());
    for (spec, waiting, spawn_error) in [
        (
            format!(
                "{HEADER}[test.pr]\nshell='exit 3'\non.pass='@done'\non.fail='@wait'\ntimeout='1h'\n"
            ),
            "fail",
            "cannot find binary path",
        ),
        (
            format!("{HEADER}[script.pr]\nshell={pending}\non.pending='@wait'\ntimeout='1h'\n"),
            "pending",
            "No such file or directory",
        ),
    ] {
        let (env, _) = fixture(&spec, "");
        let id = start(&env);
        until(&env, &id, |v| {
            v["record"]["wait"]["outcome"] == waiting && v["record"]["attempt"].is_null()
        });
        let output = env
            .rimz()
            .args(["workflow", "advance", &id])
            .env("PATH", "")
            .output()
            .unwrap();
        success(output);
        let value = settled(&env, &id, "failed");
        let input = &value["record"]["input"];
        assert_eq!(input["cause"], "died", "{waiting}: {input:#}");
        assert!(
            input["reason"].as_str().unwrap().contains(spawn_error),
            "{waiting}: {input:#}"
        );
        // The log file existed before the spawn failed, so it is named; no process exited.
        assert!(
            input["detail"]["log"]
                .as_str()
                .is_some_and(|log| moved(log).is_file()),
            "{waiting}: {input:#}"
        );
        assert_eq!(
            input["detail"].as_object().unwrap().len(),
            1,
            "{waiting}: {input:#}"
        );
    }
}

#[test]
fn shell_timeout_kills_the_group_and_routes_or_fails() {
    for edge in ["", "on.timeout='report'"] {
        let (env, _) = shell_fixture(
            LIVE,
            &format!(
                "timeout='1h'\n{edge}\n[test.report]\nshell='true'\non={{pass='@done',fail='@failed'}}\n"
            ),
        );
        let id = start(&env);
        let (value, child, descendant) = live_group(&env, &id, "pr");
        let state =
            rimz::StatePaths::for_project_root_under(&env.project_root, &env.rimz_home()).unwrap();
        let guard = rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
        expire_check(&env, &value);
        drop(guard);
        let value = settled(&env, &id, if edge.is_empty() { "failed" } else { "done" });
        let input = if edge.is_empty() {
            value["record"]["input"].clone()
        } else {
            context(&value)["input"].clone()
        };
        assert_eq!(
            input,
            json!({
                "outcome":"failed", "cause":"timeout", "node":"pr",
                "reason":"pr ran past its timeout 1h", "detail":{}
            })
        );
        assert_eq!(events(&value, "evaluation")[0]["outcome"], "timeout");
        assert_group_stopped(child, descendant);
    }
}

#[test]
fn shell_deadline_interrupts_the_group_and_uses_the_workflow_edge() {
    let (env, _) = shell_fixture(LIVE, "");
    let id = start(&env);
    let (_, child, descendant) = live_group(&env, &id, "pr");
    let deadline = jiff::Timestamp::now() - jiff::SignedDuration::from_secs(1);
    success(command(
        &env,
        &[
            "workflow",
            "resume",
            &id,
            "--extend",
            &format!("deadline={deadline}"),
        ],
    ));
    let value = settled(&env, &id, "failed");
    assert_eq!(value["record"]["input"]["cause"], "deadline");
    assert_group_stopped(child, descendant);
}

#[test]
fn shell_cancel_interrupts_the_group_and_first_terminal_wins() {
    let (env, _) = shell_fixture(LIVE, "");
    let id = start(&env);
    let (_, child, descendant) = live_group(&env, &id, "pr");
    success(command(&env, &["workflow", "cancel", &id]));
    assert_group_stopped(child, descendant);
    let value = show(&env, &id);
    assert_eq!(value["record"]["state"], "canceled");
    let evaluations = events(&value, "evaluation");
    assert_eq!(evaluations.len(), 1);
    assert_eq!(evaluations[0]["outcome"], "canceled");
    assert_eq!(evaluations[0]["seq"], 1);
    success(command(&env, &["workflow", "cancel", &id]));
    success(command(&env, &["workflow", "advance", &id]));
    assert!(!command(&env, &["workflow", "resume", &id]).status.success());
    assert_eq!(show(&env, &id), value);
}

#[test]
fn another_process_kills_a_stalled_hosts_shell_on_cancel_or_bound() {
    stalled_host_shell_group_is_killed(false);
}

#[test]
fn another_process_kills_a_stalled_hosts_exited_shell_group_on_cancel_or_bound() {
    stalled_host_shell_group_is_killed(true);
}

#[test]
fn another_process_kills_a_dead_hosts_exited_check_group_on_recovery() {
    dead_host_check_group("advance", GroupMarker::Run);
}

#[test]
fn another_process_kills_a_dead_hosts_exited_check_group_on_cancel() {
    dead_host_check_group("cancel", GroupMarker::Run);
}

#[test]
fn dead_host_check_groups_without_this_runs_marker_are_not_signaled() {
    for action in ["advance", "cancel"] {
        for marker in [GroupMarker::Missing, GroupMarker::OtherRun] {
            dead_host_check_group(action, marker);
        }
    }
}

enum GroupMarker {
    Run,
    OtherRun,
    Missing,
}

fn dead_host_check_group(action: &str, marker: GroupMarker) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::{Pid, getpgid};
    for shell in [true, false] {
        let marker_env = match marker {
            GroupMarker::Run => "",
            GroupMarker::OtherRun => "export RIMZ_WORKFLOW_RUN=other-run\n",
            GroupMarker::Missing => "unset RIMZ_WORKFLOW_RUN\n",
        };
        let script = format!(
            "{marker_env}\
             if [ -e \"$RIMZ_WORKFLOW_DIR/descendant.pid\" ]; then\n\
             printf '%s\\n' '{{\"outcome\":\"done\"}}'; exit 0\n\
             fi\n\
             sleep 600 & echo $! > \"$RIMZ_WORKFLOW_DIR/descendant.pid\"; exit 0\n"
        );
        let (env, _) = if shell {
            shell_fixture(&script, "")
        } else {
            fixture(
                &format!("{HEADER}[script.pr]\nrun='sh checks/pr.sh'\non.done='@done'\n"),
                &script,
            )
        };
        let id = start(&env);
        let (value, child, descendant) = live_group(&env, &id, "pr");
        until(&env, &id, |_| !rimz::proc::process_is_live(child, None));
        let host = value["record"]["attempt"]["host"]["pid"].as_u64().unwrap() as u32;
        let state =
            rimz::StatePaths::for_project_root_under(&env.project_root, &env.rimz_home()).unwrap();
        let guard = rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
        kill(Pid::from_raw(host as i32), Signal::SIGKILL).unwrap();
        drop(guard);
        until(&env, &id, |_| {
            !rimz::proc::process_is_live(host, None)
                && rimz::proc::process_start_token(child).is_none()
        });
        assert!(rimz::proc::process_is_live(descendant, None));
        assert_eq!(
            getpgid(Some(Pid::from_raw(descendant as i32))).unwrap(),
            Pid::from_raw(child as i32)
        );
        assert_eq!(
            rimz::proc::env_var(descendant, "RIMZ_WORKFLOW_RUN").as_deref(),
            match marker {
                GroupMarker::Run => Some(id.as_str()),
                GroupMarker::OtherRun => Some("other-run"),
                GroupMarker::Missing => None,
            }
        );
        success(command(&env, &["workflow", action, &id]));
        let value = settled(
            &env,
            &id,
            if action == "cancel" {
                "canceled"
            } else {
                "done"
            },
        );
        let evaluations = events(&value, "evaluation");
        assert_eq!(evaluations[0]["seq"], 1);
        assert_eq!(evaluations[0]["outcome"], "canceled");
        if matches!(marker, GroupMarker::Run) {
            assert_group_stopped(child, descendant);
        } else {
            let deadline = Instant::now() + Duration::from_millis(100);
            while Instant::now() < deadline {
                assert!(
                    rimz::proc::process_is_live(descendant, None),
                    "an unproven group must not receive a signal"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            kill(Pid::from_raw(descendant as i32), Signal::SIGKILL).unwrap();
        }
    }
}

fn stalled_host_shell_group_is_killed(leader_exits: bool) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    for bounded in [false, true] {
        let shell = if leader_exits {
            "sleep 600 & echo $! > \"$RIMZ_WORKFLOW_DIR/descendant.pid\"; exit 0"
        } else {
            LIVE
        };
        let (env, _) = shell_fixture(shell, "timeout='1h'\n");
        let id = start(&env);
        let (value, child, descendant) = live_group(&env, &id, "pr");
        if leader_exits {
            until(&env, &id, |_| !rimz::proc::process_is_live(child, None));
            std::thread::sleep(Duration::from_millis(100));
        }
        let host = value["record"]["attempt"]["host"]["pid"].as_u64().unwrap() as i32;
        let state =
            rimz::StatePaths::for_project_root_under(&env.project_root, &env.rimz_home()).unwrap();
        let guard = rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
        kill(Pid::from_raw(host), Signal::SIGSTOP).unwrap();
        struct ResumeHost(i32);
        impl Drop for ResumeHost {
            fn drop(&mut self) {
                let _ = kill(Pid::from_raw(self.0), Signal::SIGCONT);
            }
        }
        let resume = ResumeHost(host);
        if bounded {
            expire_check(&env, &value);
        }
        drop(guard);
        success(command(
            &env,
            &["workflow", if bounded { "advance" } else { "cancel" }, &id],
        ));
        assert_group_stopped(child, descendant);
        assert_eq!(
            events(&show(&env, &id), "evaluation")[0]["outcome"],
            if bounded { "timeout" } else { "canceled" }
        );
        drop(resume);
    }
}
