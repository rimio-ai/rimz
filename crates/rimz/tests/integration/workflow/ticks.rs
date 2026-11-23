use super::*;

fn tick_until(env: &Env, id: &str, state: &str) -> Value {
    until(env, id, |value| {
        if value["record"]["state"] == state {
            return true;
        }
        success(command(env, &["loop", "tick"]));
        false
    })
}

#[test]
fn tick_discovers_a_manual_run_and_drives_its_recheck() {
    let recheck = jiff::Timestamp::now() + jiff::SignedDuration::from_secs(2);
    let (env, _) = fixture(
        &format!("{HEADER}{WAIT}"),
        &format!("printf '%s\\n' '{{\"outcome\":\"pending\",\"recheck_at\":\"{recheck}\"}}'"),
    );
    let id = start(&env);
    until(&env, &id, |v| v["record"]["wait"].is_object());
    fs::write(
        super::definition(&env, &id).join("checks/pr.sh"),
        evaluation_script("done"),
    )
    .unwrap();
    tick_until(&env, &id, "done");
}

/// A waiting check whose program is only on the room's saved PATH runs again when a tick from a
/// process without that directory re-runs it.
#[test]
fn tick_reruns_a_waiting_check_on_the_rooms_saved_path() {
    use crate::common::{path_with_front, write_path_shim};
    let recheck = jiff::Timestamp::now() + jiff::SignedDuration::from_secs(2);
    let (env, _) = fixture(
        &format!(
            "{HEADER}[script.pr]\nrun = 'room-path-check'\non = {{ pending = '@wait', done = '@done' }}\ntimeout = '1h'\n"
        ),
        "",
    );
    let bin = env.home_root.join("room-bin");
    let ran = env.home_root.join("room-path-check.ran");
    let ran = shlex::try_quote(ran.to_str().unwrap())
        .unwrap()
        .into_owned();
    write_path_shim(
        &bin,
        "room-path-check",
        &format!(
            "if [ -e {ran} ]; then printf '{{\"outcome\":\"done\"}}'; else : > {ran}; printf '{{\"outcome\":\"pending\",\"recheck_at\":\"{recheck}\"}}'; fi"
        ),
    );
    let room_path = path_with_front(&bin);
    let record_path = env.state_path_for(&env.project_root).workspace_record;
    let mut record: Value = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    record["terminal_path"] = json!(room_path.to_str().unwrap());
    fs::write(&record_path, record.to_string()).unwrap();
    let id = start_with(|| {
        let mut command = env.rimz();
        command.env("PATH", &room_path);
        command
    });
    until(&env, &id, |v| v["record"]["wait"]["outcome"] == "pending");
    let value = until(&env, &id, |value| {
        if value["record"]["state"] != "active" {
            return true;
        }
        success(command(&env, &["loop", "tick"]));
        false
    });
    assert_eq!(
        value["record"]["state"],
        "done",
        "{}{value:#}",
        died_attempts(&value)
    );
}

#[test]
fn tick_routes_check_timeout_and_workflow_deadline_before_far_backoff() {
    for deadline in [false, true] {
        let header = if deadline {
            format!(
                "{HEADER}deadline='{}'\n",
                jiff::Timestamp::now() + jiff::SignedDuration::from_secs(2)
            )
        } else {
            HEADER.into()
        };
        let timeout = if deadline { "1h" } else { "2s" };
        let spec = format!(
            "{header}[script.pr]\nsummary=\"Review the work\"\nrun='sh checks/pr.sh'\non.pending='@wait'\ntimeout='{timeout}'\n"
        );
        let (env, _) = fixture(&spec, PENDING);
        let id = start(&env);
        let value = tick_until(&env, &id, "failed");
        assert_eq!(
            value["record"]["input"]["cause"],
            if deadline { "deadline" } else { "timeout" }
        );
    }
}

#[test]
fn tick_fires_a_deadline_granted_after_the_recheck_was_published() {
    let (env, _) = fixture(&format!("{HEADER}{WAIT}"), PENDING);
    let id = start(&env);
    until(&env, &id, |v| v["record"]["recheck_at"].is_string());
    let deadline = jiff::Timestamp::now() + jiff::SignedDuration::from_secs(2);
    success(command(
        &env,
        &[
            "workflow",
            "extend",
            &id,
            "--deadline",
            &deadline.to_string(),
        ],
    ));
    let value = tick_until(&env, &id, "failed");
    assert_eq!(value["record"]["input"]["cause"], "deadline");
}

#[test]
fn tick_retries_a_reopened_judge_window() {
    use crate::common::{path_with_front, write_fake_login_shell, write_path_shim};
    let (env, _) = fixture(
        &format!(
            "{HEADER}[judge.pr]\nsummary=\"Review the work\"\nagent='claude'\nprompt-file='prompt.md'\nadmission.window='5h.left>=42'\non.done='@done'\ntimeout='1h'\n"
        ),
        "",
    );
    ready_claude(&env);
    judges::capacity(&env, 100);
    let shell = write_fake_login_shell(&env, "window-shell", &[]);
    let bin = env.home_root.join("window-bin");
    write_path_shim(&bin, "claude", "exit 0");
    let receipt = success(
        env.rimz()
            .args(["workflow", "run", "review"])
            .env("SHELL", &shell)
            .env("PATH", path_with_front(&bin))
            .output()
            .unwrap(),
    );
    let id = receipt.lines().next().unwrap();
    until(&env, id, |v| v["record"]["wait"]["outcome"] == "window");
    judges::capacity(&env, 0);
    // The reopened window lets the judge's launch go ahead; this fixture has no room for its
    // pane, so the test stops at the wait the tick closes.
    until(&env, id, |v| {
        if v["record"]["wait"]["outcome"] != "window" {
            return true;
        }
        success(
            env.rimz()
                .args(["loop", "tick"])
                .env("SHELL", &shell)
                .env("PATH", path_with_front(&bin))
                .output()
                .unwrap(),
        );
        false
    });
}
