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
    let (env, dir) = fixture(
        &format!("{HEADER}{WAIT}"),
        &format!("printf '%s\\n' '{{\"outcome\":\"pending\",\"recheck_at\":\"{recheck}\"}}'"),
    );
    let id = start(&env);
    until(&env, &id, |v| v["record"]["wait"].is_object());
    fs::write(dir.join("checks/pr.sh"), evaluation_script("done")).unwrap();
    tick_until(&env, &id, "done");
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
            "{header}[checks.pr]\nrun='sh checks/pr.sh'\non.pending='@wait'\ntimeout='{timeout}'\n"
        );
        let (env, _) = fixture(&spec, PENDING);
        let id = start(&env);
        let value = tick_until(&env, &id, "parked");
        assert_eq!(
            value["record"]["park"]["reason"],
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
            "resume",
            &id,
            "--extend",
            &format!("deadline={deadline}"),
        ],
    ));
    let value = tick_until(&env, &id, "parked");
    assert_eq!(value["record"]["park"]["reason"], "deadline");
}

#[test]
fn tick_retries_a_reopened_judge_window() {
    use crate::common::{path_with_front, write_fake_login_shell, write_path_shim};
    let (env, _) = fixture(
        &format!(
            "{HEADER}[checks.pr]\nagent='claude'\nprompt-file='prompt.md'\nwindow='5h.left>=42'\non.done='@done'\ntimeout='1h'\n"
        ),
        "",
    );
    judges::capacity(&env, 100);
    let shell = write_fake_login_shell(&env, "window-shell", &[]);
    let bin = env.home_root.join("window-bin");
    write_path_shim(
        &bin,
        "claude",
        "printf '%s\\n' '{\"structured_output\":{\"outcome\":\"done\"}}'",
    );
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
    until(&env, id, |v| {
        if v["record"]["state"] == "done" {
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
