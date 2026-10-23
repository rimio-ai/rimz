use super::*;
use crate::common::{path_with_front, write_fake_login_shell, write_path_shim};
use rimz::agents::{AgentRateLimits, RateLimitCacheEntry, RateLimitWindow, RateLimitsCache};
use sha2::Digest;

pub(super) fn capacity(env: &Env, used: u8) -> jiff::Timestamp {
    let reset = jiff::Timestamp::now() + jiff::SignedDuration::from_hours(2);
    env.publish_rate_limits(&RateLimitsCache {
        entries: BTreeMap::from([(
            "claude@default".parse().unwrap(),
            RateLimitCacheEntry {
                limits: AgentRateLimits {
                    windows: vec![RateLimitWindow {
                        used_percentage: Some(used),
                        resets_at: Some(reset),
                        duration_mins: Some(300),
                        ..Default::default()
                    }],
                },
                ..Default::default()
            },
        )]),
        ..Default::default()
    });
    reset
}

use std::collections::BTreeMap;

#[test]
fn judge_refuses_below_version_floor_before_provider_spawn() {
    let (env, _) = fixture(
        &format!("{HEADER}[checks.pr]\nagent='codex'\nprompt-file='prompt.md'\non.done='@done'\n"),
        "",
    );
    env.write_config(&env.project_root, "[agents]\nisolation = 'host'\n");
    env.install_agent_hooks("codex");
    crate::common::trust_codex_preflight_hooks(&env);
    crate::common::trust_codex_project(&env, &env.project_root);
    let shell = write_fake_login_shell(&env, "judge-shell", &[]);
    let bin = env.home_root.join("judge-bin");
    write_path_shim(
        &bin,
        "codex",
        r#"
printf '%s\n' "$*" >> "$HOME/judge-calls"
if [ "$1" = '--version' ]; then printf '%s\n' 'codex-cli 0.162.0'; exit 0; fi
while [ "$#" -gt 0 ]; do
    if [ "$1" = '-o' ]; then shift; printf '%s' '{"outcome":"done"}' > "$1"; fi
    shift
done
"#,
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
    let value = until(&env, id, |value| !events(value, "evaluation").is_empty());
    let evaluation = &events(&value, "evaluation")[0];
    assert_eq!(evaluation["object"]["outcome"], "died");
    let reason = evaluation["object"]["reason"].as_str().unwrap();
    assert!(
        reason.contains("0.162.0") && reason.contains("0.163.0"),
        "{reason}"
    );
    assert!(reason.ends_with("upgrade Codex"), "{reason}");
    let value = settled(&env, id, "parked");
    assert_eq!(value["record"]["park"]["reason"], "check");
    let evaluations = events(&value, "evaluation");
    assert!(
        evaluations.iter().all(|row| row["digests"] == json!({})),
        "a refused judge never spawned: {evaluations:#?}"
    );
    let calls = fs::read_to_string(env.home_root.join("judge-calls")).unwrap();
    assert!(!calls.is_empty());
    assert!(calls.lines().all(|call| call == "--version"), "{calls}");
}

#[test]
fn judge_process_objects_retry_limit_and_dead_host_policy() {
    struct NameModeOnPanic(&'static str);
    impl Drop for NameModeOnPanic {
        #[expect(
            clippy::print_stderr,
            reason = "names the failing mode beside the panic"
        )]
        fn drop(&mut self) {
            if std::thread::panicking() {
                eprintln!("judge mode {} failed", self.0);
            }
        }
    }
    let digest = hex::encode(sha2::Sha256::digest(b"Build it"));
    for mode in ["object", "file", "missing", "limited", "dead-host"] {
        let _named = NameModeOnPanic(mode);
        let (env, _) = fixture(
            &format!(
                "{HEADER}[checks.pr]\nagent='claude'\nprompt-file='prompt.md'\non.done='@done'\ntimeout='1h'\n"
            ),
            "",
        );
        let shell = write_fake_login_shell(&env, "judge-shell", &[]);
        let bin = env.home_root.join("judge-bin");
        let body = match mode {
            "file" => {
                "printf '%s\\n' '{\"outcome\":\"done\",\"reason\":\"judge passed\"}' > \"$(sed -n 's/^  \"output_path\": \"\\(.*\\)\",*$/\\1/p' \"$RIMZ_WORKFLOW_DIR/context.json\")\"; printf '%s\\n' '{\"is_error\":false}'"
            }
            "missing" => "printf '%s\\n' '{\"is_error\":false}'",
            "limited" => "printf '%s\\n' '{\"is_error\":true,\"result\":\"quota exhausted\"}'",
            "dead-host" => "sleep 60",
            _ => {
                "printf '%s\\n' \"$@\" > \"$RIMZ_WORKFLOW_DIR/judge-argv\"; printf '%s\\n' '{\"structured_output\":{\"outcome\":\"done\",\"reason\":\"judge passed\"},\"total_cost_usd\":0.25}'"
            }
        };
        write_path_shim(&bin, "claude", body);
        let invoke = |args: &[&str]| {
            env.rimz()
                .args(args)
                .env("SHELL", &shell)
                .env("PATH", path_with_front(&bin))
                .output()
                .unwrap()
        };
        let receipt = success(invoke(&["workflow", "run", "review"]));
        let id = receipt.lines().next().unwrap();
        match mode {
            "object" | "file" => {
                let value = settled(&env, id, "done");
                assert_eq!(
                    events(&value, "evaluation")[0]["object"]["reason"],
                    "judge passed"
                );
                if mode == "object" {
                    assert_eq!(events(&value, "evaluation")[0]["cost_usd"], 0.25);
                    let prompt = fs::read_to_string(
                        Path::new(value["dir"].as_str().unwrap()).join("judge-argv"),
                    )
                    .unwrap();
                    assert!(
                        prompt.contains("required string outcome")
                            && prompt.contains("string-or-integer key")
                            && prompt.contains("no prose")
                            && prompt.contains("other fields"),
                        "the prompt must carry the open object contract: {prompt}"
                    );
                }
                assert!(Path::new(context(&value)["output_path"].as_str().unwrap()).is_file());
            }
            "missing" => {
                let value = settled(&env, id, "parked");
                assert_eq!(value["record"]["park"]["reason"], "check");
                assert_eq!(value["record"]["check_retry"], 1);
                assert_eq!(events(&value, "evaluation").len(), 2);
                assert_eq!(context(&value)["retry"]["outcome"], "died");
                assert_eq!(workflow_diagnostics(&env, "workflow_retry").len(), 1);
            }
            "limited" => {
                // Capacity is consulted after the provider result, not as a launch gate.
                capacity(&env, 100);
                success(invoke(&["workflow", "advance", id]));
                let value = until(&env, id, |value| value["record"]["recheck_at"].is_string());
                let entered: jiff::Timestamp = value["record"]["entered_at"]
                    .as_str()
                    .unwrap()
                    .parse()
                    .unwrap();
                assert_eq!(
                    value["record"]["recheck_at"],
                    (entered + jiff::SignedDuration::from_hours(1)).to_string(),
                    "provider backoff must not hide the check timeout from the ticker"
                );
                assert_eq!(value["record"]["check_retry"], 0);
                let count = events(&value, "evaluation").len();
                success(invoke(&["workflow", "advance", id]));
                assert_eq!(events(&show(&env, id), "evaluation").len(), count);
                success(command(&env, &["workflow", "cancel", id]));
            }
            _ => {
                let value = until(&env, id, |value| {
                    value["record"]["attempt"]["child"].is_object()
                });
                let host = value["record"]["attempt"]["host"]["pid"].as_u64().unwrap() as i32;
                nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(host),
                    nix::sys::signal::Signal::SIGKILL,
                )
                .unwrap();
                write_path_shim(
                    &bin,
                    "claude",
                    "printf '%s\\n' '{\"structured_output\":{\"outcome\":\"done\"}}'",
                );
                success(
                    env.rimz()
                        .args(["workflow", "advance", id])
                        .env("SHELL", &shell)
                        .env("PATH", path_with_front(&bin))
                        .output()
                        .unwrap(),
                );
                let value = settled(&env, id, "done");
                assert_eq!(value["record"]["check_retry"], 0);
                assert!(
                    events(&value, "evaluation")
                        .iter()
                        .any(|entry| entry["outcome"] == "canceled" && entry["origin"] == "engine")
                );
                assert_eq!(workflow_diagnostics(&env, "workflow_rerun").len(), 1);
            }
        }
        let evaluations = events(&show(&env, id), "evaluation");
        for evaluation in &evaluations {
            assert_eq!(
                evaluation["digests"],
                json!({"prompt.md": digest}),
                "{mode}: {evaluations:#?}"
            );
        }
    }
}

pub(super) fn workflow_diagnostics(env: &Env, kind: &str) -> Vec<Value> {
    let state = env.state_path_for(&env.project_root);
    let sink = rimz::diag::DiagSink::under(state.root, state.workspace_id, "test", None);
    fs::read_to_string(sink.log_path().unwrap())
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|record| record["event"]["kind"] == kind)
        .collect()
}
