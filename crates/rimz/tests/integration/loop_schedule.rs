//! Integration coverage for `rimz loop` instance-bound delivery.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use jiff::{SignedDuration, Timestamp};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde_json::json;

use rimz::agents::PermissionMode;
use rimz::agents::{AgentRateLimits, RateLimitCacheEntry, RateLimitWindow, RateLimitsCache};
use rimz::config::{CheckOn, LoopConfig, TaskEntry, TaskTarget, Tasks};
use rimz::harness::budget::{BudgetLedger, DayBaseline, write_ledger};
use rimz::harness::schedule::arming::Arming;
use rimz::harness::schedule::run_log::{LoopRunMode, LoopRunRecord, LoopRunResult};
use rimz::harness::schedule::runner::RunLockInfo;
use rimz::ids::{AgentKind, AgentSessionId, MuxName, WorkspaceId};
use rimz::store::message::{AutoCompact, MessageStatus};
use rimz::store::run::{RunRecord, RunStatus};

use crate::common::{Env, ScrubSessionEnvExt, canonical, retained_runs_refusal};
#[path = "loop_list.rs"]
mod list_tests;
#[path = "loop_overlap.rs"]
mod overlap_tests;
#[path = "agents_tune.rs"]
mod tune_tests;

#[test]
fn loop_show_condition_evidence_and_watch_command_are_not_repeated_or_cut() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.condition]\nroot={:?}\ncheck='true'\nwhen=['pr=merged && ci=passed']\n",
            env.project_root
        ),
    );
    write_loop_fire_state(
        &env,
        BTreeMap::from([("condition".into(), Timestamp::now())]),
    );
    let text = loop_ok(&env, &["loop", "show", "condition"]);
    assert!(
        text.starts_with("condition · routine · waiting · 0 of 2 met\n"),
        "{text}"
    );
    assert!(!text.contains("condition:"), "{text}");
    assert!(
        text.contains("pr=merged   ? readings paused (room closed)"),
        "{text}"
    );
    assert!(!text.contains("try `rimz loop fire"), "{text}");
    let value: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", "condition", "--json"])).unwrap();
    assert_eq!(value["kind"], "routine");
}

#[test]
fn loop_show_watch_command_is_not_cut_inside_a_token() {
    let env = Env::new();
    let command = format!("echo {} tail", "long-token-".repeat(20));
    write_loop_instances(
        &env,
        Tasks(BTreeMap::from([(
            "watch".into(),
            TaskEntry {
                root: env.project_root.clone(),
                watch: Some(rimz::config::WatchSpec::Command(command.clone())),
                ..TaskEntry::default()
            },
        )])),
    );
    let text = loop_ok(&env, &["loop", "show", "watch"]);
    assert!(
        text.lines()
            .next()
            .unwrap()
            .contains("watch · one-off · on command exit"),
        "{text}"
    );
    assert!(text.contains(&"long-token-".repeat(20)), "{text}");
    assert!(text.contains("echo") && text.contains("tail"), "{text}");
    assert!(!text.contains("try `rimz loop fire"), "{text}");
}

#[test]
fn loop_fire_warns_for_unmet_conditions_and_keeps_them_armed() {
    let env = Env::new();
    env.record(&env.project_root);
    write_loop_config(
        &env,
        &format!(
            "[tasks.condition]\nroot={:?}\ncheck='true'\nwhen=['team.stage=Done']\n",
            env.project_root
        ),
    );
    let text = loop_ok(&env, &["loop", "fire", "condition"]);
    assert!(
        text.contains("condition not met (0 of 1); running anyway, task stays armed"),
        "{text}"
    );
    assert!(text.contains("launched"), "{text}");
    assert_eq!(
        wait_for_loop_report(&env, "condition").result,
        LoopRunResult::Completed
    );
    assert!(loop_ok(&env, &["loop", "list"]).contains("condition"));
}

#[cfg(unix)]
#[test]
fn judge_refuses_below_version_floor_before_provider_spawn() {
    let (env, _) = codex_judge_version_fixture("codex-cli 0.162.0");
    assert_eq!(
        wait_for_loop_report(&env, "guard").result,
        LoopRunResult::Failed
    );
    assert_eq!(
        std::fs::read_to_string(env.home_root.join("judge-calls")).unwrap(),
        "--version\n"
    );
}

#[test]
fn tick_fires_instance_despite_same_named_foreign_machine() {
    let env = Env::new();
    env.record(&env.project_root);
    let other = env.home_root.join("other");
    std::fs::create_dir(&other).unwrap();
    write_loop_config(
        &env,
        &format!("[tasks.same]\nroot={other:?}\ncheck='true'\nevery='1m'\n"),
    );
    write_loop_instances(
        &env,
        Tasks(BTreeMap::from([(
            "same".into(),
            TaskEntry {
                root: env.project_root.clone(),
                check: Some("true".into()),
                fire_at: Some(Timestamp::now() - SignedDuration::from_mins(1)),
                ..TaskEntry::default()
            },
        )])),
    );
    let prior = Timestamp::now() - SignedDuration::from_mins(2);
    write_loop_fire_state(&env, BTreeMap::from([("same".into(), prior)]));

    loop_ok(&env, &["loop", "tick"]);

    assert!(
        rimz::harness::schedule::last_stamps(&env.runtime_paths())
            .get("same")
            .is_some_and(|stamp| *stamp > prior)
    );
}

#[test]
fn adding_machine_refuses_foreign_root_without_mutation() {
    let env = Env::new();
    env.record(&env.project_root);
    let other = env.home_root.join("other");
    std::fs::create_dir(&other).unwrap();
    env.record(&other);
    let text =
        format!("# keep this comment\n[tasks.same]\nroot={other:?}\ncheck='true'\nevery='1h'\n");
    write_loop_config(&env, &text);

    let keys = [
        machine_task_key("same"),
        project_task_key(&env.project_root, "same"),
    ];
    write_loop_arming(
        &env,
        &keys
            .iter()
            .map(|key| {
                (
                    key.clone(),
                    Arming {
                        enabled: false,
                        at: None,
                        pause_until: None,
                        strikes: Some(2),
                    },
                )
            })
            .collect(),
    );
    std::fs::write(
        loop_strikes_path(&env),
        serde_json::to_vec(&BTreeMap::from([
            (keys[0].clone(), 2),
            (keys[1].clone(), 3),
        ]))
        .unwrap(),
    )
    .unwrap();
    let mut paths = vec![
        loop_config_path(&env),
        loop_arming_path(&env),
        loop_strikes_path(&env),
    ];
    for root in [&env.project_root, &other] {
        let path = env
            .state_path_for(root)
            .root
            .join("records/loop-declines.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"same":{}}"#).unwrap();
        paths.push(path);
    }

    for with_instance in [false, true] {
        if with_instance {
            write_loop_instances(
                &env,
                Tasks(BTreeMap::from([(
                    "same".into(),
                    TaskEntry {
                        root: env.project_root.clone(),
                        check: Some("true".into()),
                        signal: Some("ci.failed".into()),
                        once: Some(true),
                        ..TaskEntry::default()
                    },
                )])),
            );
            paths.push(loop_instances_path(&env));
        }
        let before: Vec<_> = paths
            .iter()
            .map(|path| std::fs::read(path).unwrap())
            .collect();
        let (stdout, error) = loop_fail(
            &env,
            &["loop", "add", "same", "--check", "true", "--every", "1h"],
        );
        assert!(!stdout.contains("added loop task"), "{stdout}");
        assert!(
            error.contains("same") && error.contains("another project"),
            "{error}"
        );
        assert!(error.contains(other.to_str().unwrap()), "{error}");
        assert!(error.contains("loop remove same"), "{error}");
        for (path, before) in paths.iter().zip(before) {
            assert_eq!(std::fs::read(path).unwrap(), before, "{}", path.display());
        }
    }
}

#[test]
fn adding_machine_replaces_same_root() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.same]\nroot={:?}\ncheck='true'\nevery='1h'\n",
            env.project_root
        ),
    );
    loop_ok(
        &env,
        &["loop", "add", "same", "--check", "false", "--every", "2h"],
    );
    let config: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).unwrap()).unwrap();
    let entry = &config.tasks.0["same"];
    assert_eq!(entry.check, Some("false".into()));
    assert_eq!(entry.every.as_deref(), Some("2h"));
    assert_eq!(entry.resolved_root(), env.project_root);
}

#[test]
fn adding_machine_refuses_rootless_row() {
    let env = Env::new();
    env.record(&env.project_root);
    let instances = Tasks(BTreeMap::from([(
        "same".into(),
        TaskEntry {
            root: env.project_root.clone(),
            check: Some("true".into()),
            signal: Some("ci.failed".into()),
            once: Some(true),
            ..TaskEntry::default()
        },
    )]));
    write_loop_instances(&env, instances.clone());
    let text = "# no project owns this row\n[tasks.same]\ncheck='true'\nevery='1h'\n";
    write_loop_config(&env, text);
    let (stdout, error) = loop_fail(
        &env,
        &["loop", "add", "same", "--check", "false", "--every", "2h"],
    );
    assert!(!stdout.contains("added loop task"), "{stdout}");
    assert!(
        error.contains("same") && error.contains("has no `root` in"),
        "{error}"
    );
    assert!(error.contains("loop remove same"), "{error}");
    assert!(
        error.contains("from a fresh directory outside any repository or RimZ project"),
        "{error}"
    );
    assert_eq!(
        std::fs::read(loop_config_path(&env)).unwrap(),
        text.as_bytes()
    );
    assert_eq!(read_loop_instances(&env), instances);

    let outside = env.home_root.join("outside");
    std::fs::create_dir(&outside).unwrap();
    let output = env
        .rimz()
        .current_dir(&outside)
        .args(["loop", "remove", "same"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let config: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).unwrap()).unwrap();
    assert!(!config.tasks.0.contains_key("same"));
    assert_eq!(read_loop_instances(&env), instances);
}

#[test]
fn loop_list_skips_machine_tasks_without_absolute_roots() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.skip-bare]\ncheck='true'\nevery='1h'\n\
         [tasks.skip-relative]\nroot='nowhere/x'\ncheck='true'\nevery='1h'\n\
         [tasks.skip-dot]\nroot='.'\ncheck='true'\nevery='1h'\n\
         [tasks.rooted]\nroot={:?}\ncheck='true'\nevery='1h'\n",
            env.project_root,
        ),
    );
    for args in [
        vec!["loop", "list", "--all"],
        vec!["loop", "list", "--all", "--json"],
        vec!["loop", "list"],
        vec!["loop", "list", "--json"],
    ] {
        let output = env.rimz().args(&args).output().unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stdout.contains("rooted"), "{stdout}");
        for name in ["skip-bare", "skip-relative", "skip-dot"] {
            assert!(!stdout.contains(name), "{stdout}");
            let warnings = stderr
                .lines()
                .filter(|line| line.contains(&format!("loop task `{name}`")))
                .collect::<Vec<_>>();
            assert_eq!(warnings.len(), 1, "{stderr}");
            let warning = warnings[0];
            for text in [
                "warning:",
                "does not run",
                "absolute path",
                &format!("rimz loop remove {name}"),
            ] {
                assert!(warning.contains(text), "{warning}");
            }
        }
        assert!(stderr.contains("no `root`"), "{stderr}");
        assert!(stderr.contains("relative `root` `nowhere/x`"), "{stderr}");
        assert!(stderr.contains("relative `root` `.`"), "{stderr}");
        if args.contains(&"--json") {
            let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
            let rooms = value["rooms"].as_array().unwrap();
            assert_eq!(rooms.len(), 1, "{value}");
            assert_eq!(rooms[0]["root"], json!(env.project_root), "{value}");
            assert_eq!(rooms[0]["tasks"].as_array().unwrap().len(), 1, "{value}");
        }
    }
}

#[test]
fn loop_show_reports_machine_task_root_problem() {
    let env = Env::new();
    write_loop_config(&env, "[tasks.one]\ncheck='true'\nevery='1h'\n");
    let output = env.rimz().args(["loop", "show", "one"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    for text in [
        "loop task `one`",
        "no `root`",
        "does not run",
        "rimz loop remove one",
    ] {
        assert!(stderr.contains(text), "{stderr}");
    }
    assert!(!stderr.contains("no loop task named"), "{stderr}");
}

#[test]
fn loop_tick_skips_machine_tasks_without_absolute_roots() {
    let env = Env::new();
    write_loop_config(
        &env,
        "[tasks.one]\ncheck='true'\nevery='1h'\n[tasks.two]\nroot='nowhere/x'\ncheck='true'\nevery='1h'\n",
    );
    let output = env.rimz().args(["loop", "tick"]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn adding_machine_refuses_relative_row() {
    let env = Env::new();
    let text = "[tasks.same]\nroot='nowhere/x'\ncheck='true'\nevery='1h'\n";
    write_loop_config(&env, text);
    let (stdout, error) = loop_fail(
        &env,
        &["loop", "add", "same", "--check", "false", "--every", "2h"],
    );
    assert!(!stdout.contains("added loop task"), "{stdout}");
    let reason = format!(
        "loop task `same` has relative `root` `nowhere/x` in {}; choose another name, or run `rimz loop remove same` from a fresh directory outside any repository or RimZ project to remove it first",
        loop_config_path(&env).display(),
    );
    assert!(error.contains(&reason), "{error}");
    assert!(!error.contains("belongs to another project"), "{error}");
    assert_eq!(
        std::fs::read_to_string(loop_config_path(&env)).unwrap(),
        text
    );
}

#[test]
fn removing_rootless_machine_row_succeeds() {
    for resident in [false, true] {
        let env = Env::new();
        write_loop_config(
            &env,
            &format!(
                "[tasks.same]\ncheck='true'\nevery='1h'\n{}",
                if resident {
                    "agent='codex'\nstay=true\n"
                } else {
                    ""
                },
            ),
        );
        let output = loop_ok(&env, &["loop", "remove", "same"]);
        assert!(output.contains("removed loop task `same`"), "{output}");
        let config: LoopConfig =
            toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).unwrap()).unwrap();
        assert!(!config.tasks.0.contains_key("same"));
    }
}

#[test]
fn renaming_rootless_machine_row_succeeds() {
    for resident in [false, true] {
        let env = Env::new();
        write_loop_config(
            &env,
            &format!(
                "[tasks.same]\ncheck='true'\nevery='1h'\n{}",
                if resident {
                    "agent='codex'\nstay=true\n"
                } else {
                    ""
                },
            ),
        );
        let output = loop_ok(&env, &["loop", "rename", "same", "renamed"]);
        assert!(
            output.contains("renamed loop task `same` to `renamed`"),
            "{output}"
        );
        let config: LoopConfig =
            toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).unwrap()).unwrap();
        assert!(!config.tasks.0.contains_key("same"));
        let entry = &config.tasks.0["renamed"];
        assert_eq!(entry.check, Some("true".into()));
        assert_eq!(entry.stay, resident);
        assert!(entry.root.as_os_str().is_empty());
    }
}

#[test]
fn adding_instance_removes_only_same_root_machine() {
    let env = Env::new();
    env.record(&env.project_root);
    let other = env.home_root.join("other");
    std::fs::create_dir(&other).unwrap();
    for root in [&other, &env.project_root] {
        write_loop_config(
            &env,
            &format!("[tasks.same]\nroot={root:?}\ncheck='true'\nevery='1h'\n"),
        );

        loop_ok(
            &env,
            &[
                "loop",
                "add",
                "same",
                "--signal",
                "ci.failed",
                "--check",
                "true",
                "--once",
            ],
        );

        let config: LoopConfig =
            toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).unwrap()).unwrap();
        assert_eq!(config.tasks.0.contains_key("same"), root == &other);
        if root == &other {
            assert_eq!(config.tasks.0["same"].resolved_root(), *root);
        }
        assert_eq!(read_loop_instances(&env).0["same"].root, env.project_root);
    }
}

#[cfg(unix)]
#[test]
fn judge_with_unreadable_version_still_reads_verdict() {
    let (env, output) = codex_judge_version_fixture("{}");
    assert!(output.status.success(), "{output:?}");
    // A decline ends the run done without acting.
    let report = wait_for_loop_report(&env, "guard");
    assert_eq!(
        (report.result, report.acted),
        (LoopRunResult::Completed, Some(false))
    );
    let calls = std::fs::read_to_string(env.home_root.join("judge-calls")).unwrap();
    let calls: Vec<_> = calls.lines().collect();
    assert_eq!(calls.len(), 2, "one probe and one provider launch");
    assert_eq!(calls[0], "--version");
    assert!(calls[1].contains(" exec --json "), "{calls:?}");
}

#[cfg(unix)]
fn codex_judge_version_fixture(version: &str) -> (Env, std::process::Output) {
    let env = Env::new();
    env.write_config(&env.project_root, "[agents]\nisolation = 'host'\n");
    env.record(&env.project_root);
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(&env);
    trust_codex_project(&env, &env.project_root);
    let bin = env.home_root.join("check-bin");
    write_path_shim(
        &bin,
        "codex",
        &format!(
            r#"
printf '%s' "$*" | tr '\n' ' ' >> "$HOME/judge-calls"
printf '\n' >> "$HOME/judge-calls"
if [ "$1" = '--version' ]; then printf '%s\n' '{version}'; exit 0; fi
while [ "$#" -gt 0 ]; do
    if [ "$1" = '-o' ]; then shift; printf '%s' '{{"outcome":"fail","reason":"No actionable work."}}' > "$1"; fi
    shift
done
"#
        ),
    );
    std::fs::write(
        env.home_root.join(".profile"),
        format!("PATH='{}':\"$PATH\"\n", bin.display()),
    )
    .unwrap();
    write_loop_config(
        &env,
        &format!(
            "[tasks.guard]\nagent='codex'\nprompt='repair'\nroot={:?}\nevery='1h'\non='success'\n[tasks.guard.check]\nagent='codex'\nprompt='Any work?'\n",
            env.project_root
        ),
    );
    let output = env
        .rimz()
        .env("PATH", path_with_front(&bin))
        .args(["loop", "run", "guard"])
        .output()
        .unwrap();
    (env, output)
}

#[cfg(unix)]
#[test]
fn resident_agent_decline_is_counted_and_held_until_the_guard_changes() {
    resident_agent_decline_holds(None);
}

#[cfg(unix)]
#[test]
fn resident_agent_decline_with_a_recheck_holds_in_its_run() {
    resident_agent_decline_holds(Some("6h"));
}

#[cfg(unix)]
fn resident_agent_decline_holds(recheck: Option<&str>) {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    env.write_config(&env.project_root, "[agents]\nisolation = \"host\"\n");
    env.record(&env.project_root);
    loop_ok(&env, &["worktree", "new", "auth"]);
    let checkout = rimz::utils::path::normalize_path_lexical(
        &rimz::worktree::discover_owned(&env.project_root)
            .unwrap()
            .remove(0)
            .marker
            .worktree_path,
    );
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(&env);
    trust_codex_project(&env, &env.project_root);
    let agent_bin = env.home_root.join("check-bin");
    write_path_shim(
        &agent_bin,
        "codex",
        r#"
while [ "$#" -gt 0 ]; do
    if [ "$1" = '-o' ]; then shift; printf '%s' '{"outcome":"fail","reason":"No actionable work."}' > "$1"; fi
    shift
done
printf '%s\n' '{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":5}}'
"#,
    );
    // The judge runs through the fixture's login shell, and a system profile
    // that assigns PATH (Debian's does) drops the shim directory before it.
    std::fs::write(
        env.home_root.join(".profile"),
        format!("PATH='{}':\"$PATH\"\n", agent_bin.display()),
    )
    .unwrap();
    write_loop_config(
        &env,
        &format!(
            "[tasks.guard]\nagent = \"codex\"\nprompt = \"repair\"\nroot = {:?}\nstay = true\neach-worktree = true\nwhen = [\"team.stage=Done\"]\non = \"success\"\n[tasks.guard.check]\nagent = \"codex\"\nprompt = \"Any work?\"\n{}",
            env.project_root,
            recheck
                .map(|value| format!("recheck = {value:?}\n"))
                .unwrap_or_default()
        ),
    );
    std::fs::create_dir_all(checkout.join(".rimz/team")).unwrap();
    std::fs::write(checkout.join(".rimz/team/blackboard.md"), "Stage: Done\n").unwrap();
    let since = Timestamp::now();
    let evidence = json!({"when":"team.stage=Done", "hold":null, "since":since, "held_ms":0, "readings":{"team.stage":"Done"}});
    for _ in 0..2 {
        let output = env
            .rimz()
            .env("PATH", path_with_front(&agent_bin))
            .args(["loop", "run", "guard", "--cwd"])
            .arg(&checkout)
            .arg("--condition-json")
            .arg(evidence.to_string())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let records = read_loop_run_records(&env);
    assert_eq!(
        records.len(),
        1,
        "a holding decline must write no second history row: {records:?}"
    );
    assert_eq!(records[0].result, LoopRunResult::Launched);
    assert_eq!(records[0].condition.as_ref().unwrap().since, Some(since));
    // A condition task's decline holds inside the run until its guard changes or the judge's
    // recheck elapses (`a_condition_tasks_held_decline_leaves_once_the_judges_recheck_elapses`).
    let id = records[0].run_id.as_deref().unwrap().parse().unwrap();
    let paths = env.store().paths().workflow(&id);
    let hold = "held-judge-1";
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    while !rimz::store::workflow::load(&paths)
        .is_ok_and(|run| run.state == rimz::store::workflow::RunState::Active && run.node == hold)
    {
        assert!(
            Instant::now() < deadline,
            "{}",
            loop_run_journal(&env, &env.project_root, &records[0])
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let stats = loop_ok(&env, &["stats", "--json"]);
    let stats: serde_json::Value = serde_json::from_str(&stats).unwrap();
    assert_eq!(stats["assists"]["judge_declines"], 1);
    assert!(loop_ok(&env, &["stats", "--assists"]).contains("No actionable work."));
}
#[cfg(unix)]
#[test]
fn a_judge_decline_without_a_recheck_holds_until_a_manual_fire_or_a_redefinition() {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    env.write_config(&env.project_root, "[agents]\nisolation = \"host\"\n");
    env.record(&env.project_root);
    crate::common::room::seed_sidebar_heartbeat(
        &env.runtime_paths(),
        MuxName::Zellij,
        &env.resolve_workspace(&env.project_root).session_name,
        "hold",
    );
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(&env);
    trust_codex_project(&env, &env.project_root);
    let agent_bin = env.home_root.join("check-bin");
    let asked = env.home_root.join("judge-asked");
    write_path_shim(
        &agent_bin,
        "codex",
        &format!(
            r#"
while [ "$#" -gt 0 ]; do
    if [ "$1" = '-o' ]; then shift; printf 'asked\n' >> '{}'; printf '%s' '{{"outcome":"fail","reason":"No actionable work."}}' > "$1"; fi
    shift
done
printf '%s\n' '{{"type":"turn.completed","usage":{{"input_tokens":10,"output_tokens":5}}}}'
"#,
            asked.display()
        ),
    );
    // The judge runs through the fixture's login shell, and a system profile
    // that assigns PATH (Debian's does) drops the shim directory before it.
    std::fs::write(
        env.home_root.join(".profile"),
        format!("PATH='{}':\"$PATH\"\n", agent_bin.display()),
    )
    .unwrap();
    write_loop_config(
        &env,
        &format!(
            "[tasks.guard]\nagent = \"codex\"\nprompt = \"repair\"\nroot = {:?}\nstay = true\nevery = \"1h\"\non = \"success\"\n[tasks.guard.check]\nagent = \"codex\"\nprompt = \"Any work?\"\n",
            env.project_root
        ),
    );
    let loop_run = |verb: &str| {
        let output = env
            .rimz()
            .env("PATH", path_with_front(&agent_bin))
            .args(["loop", verb, "guard"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "loop {verb}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    let asks = || {
        std::fs::read_to_string(&asked)
            .unwrap_or_default()
            .lines()
            .count()
    };
    let runs = || {
        let mut runs = rimz::store::workflow::list(env.store().paths()).unwrap();
        runs.retain(|run| run.task.as_deref() == Some("guard"));
        runs.sort_by_key(|run| run.created_at);
        runs
    };
    // Waits until the task's newest run holds the decline, and returns it.
    let held = |count: usize| {
        let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
        loop {
            let runs = runs();
            if runs.len() == count
                && runs.last().is_some_and(|run| {
                    run.state == rimz::store::workflow::RunState::Active
                        && run.node == "hold-judge-1"
                })
            {
                return runs;
            }
            assert!(Instant::now() < deadline, "{runs:?}");
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    let ended = |id: &rimz::ids::WorkflowRunId| {
        let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
        while !rimz::store::workflow::load(&env.store().paths().workflow(id))
            .is_ok_and(|run| run.state == rimz::store::workflow::RunState::Canceled)
        {
            assert!(Instant::now() < deadline, "run {id} was not canceled");
            std::thread::sleep(Duration::from_millis(25));
        }
    };

    loop_run("run");
    let first = held(1).remove(0);
    loop_run("run");
    loop_run("run");
    assert_eq!(
        (asks(), runs().len()),
        (1, 1),
        "two later fires on the held checkout ask no judge"
    );
    assert_eq!(
        rimz::store::workflow::load(&env.store().paths().workflow(&first.id))
            .unwrap()
            .node,
        "hold-judge-1"
    );

    loop_run("fire");
    ended(&first.id);
    let second = held(2).pop().unwrap();
    assert_eq!(
        asks(),
        2,
        "a manual fire supersedes the hold and asks again"
    );

    loop_ok_root(
        &env,
        &env.project_root,
        &[
            "loop",
            "add",
            "guard",
            "--agent",
            "codex",
            "--stay",
            "--every",
            "1h",
            "--prompt",
            "repair again",
            "--judge",
            "codex",
            "--judge-prompt",
            "Any work?",
            "--on",
            "success",
        ],
    );
    ended(&second.id);
    loop_run("run");
    held(3);
    assert_eq!(
        asks(),
        3,
        "a redefinition releases the hold and forgets the checkout"
    );
}

#[cfg(unix)]
use crate::common::{
    path_with_front, trust_codex_preflight_hooks, trust_codex_project, write_fake_login_shell,
    write_path_shim,
};

#[cfg(unix)]
#[test]
fn scheduled_agent_opens_named_tab_despite_ambient_pane() {
    loop_channel_case(LoopLaunch::Agent, LoopFirer::None);
}

#[cfg(unix)]
#[test]
fn check_launch_uses_its_worktree_channel_not_the_firers_team() {
    loop_channel_case(LoopLaunch::Check, LoopFirer::None);
}

#[cfg(unix)]
#[test]
fn resident_launch_does_not_inherit_the_firers_channel() {
    loop_channel_case(LoopLaunch::Resident, LoopFirer::None);
}

#[cfg(unix)]
#[test]
fn scheduled_launch_ignores_a_subagent_firer() {
    loop_channel_case(LoopLaunch::Agent, LoopFirer::Subagent);
}

#[cfg(unix)]
#[test]
fn resident_launch_ignores_a_firer_at_the_chain_limit() {
    loop_channel_case(LoopLaunch::Resident, LoopFirer::ChainLimit);
}

#[cfg(unix)]
#[test]
fn check_launch_ignores_a_firer_found_by_ancestry() {
    loop_channel_case(LoopLaunch::Check, LoopFirer::Ancestry);
}

#[cfg(unix)]
#[test]
fn scheduled_launch_ignores_a_firers_room_pin() {
    loop_channel_case(LoopLaunch::Agent, LoopFirer::RoomPin);
}

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq)]
enum LoopLaunch {
    Agent,
    Check,
    Resident,
}

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq)]
enum LoopFirer {
    None,
    Subagent,
    ChainLimit,
    Ancestry,
    RoomPin,
}

#[cfg(unix)]
fn loop_channel_case(action: LoopLaunch, firer: LoopFirer) {
    use crate::common::CommandTimeoutExt;

    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    env.record(&env.project_root);
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(&env);
    trust_codex_project(&env, &env.project_root);
    let agent_bin = crate::common::write_failing_agent_shim(&env, "codex", 1);
    let shell = write_fake_login_shell(&env, "rimz-test-sh", &[]);
    let workspace = env.resolve_workspace(&env.project_root);
    let cwd = if action == LoopLaunch::Check {
        let linked = env.home_root.join("linked");
        assert!(git_ok(
            &env.project_root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "linked",
                linked.to_str().unwrap()
            ]
        ));
        env.write_config(
            &env.project_root,
            r#"
            [profiles.coder]
            agent = "codex"
            [[agents.teams.forge.roles]]
            role = "coder"
            profile = "coder"
            "#,
        );
        loop_ok(&env, &["trust", "grant"]);
        seed_agent_launch(
            &env,
            &env.project_root,
            "probe-member",
            rimz::agents::LaunchParams {
                team: Some("forge".to_owned()),
                role: Some("coder".to_owned()),
                channel: Some("probe".to_owned()),
                ..Default::default()
            },
            None,
        );
        trust_codex_project(&env, &linked);
        canonical(&linked)
    } else {
        env.project_root.clone()
    };
    let trace = env.home_root.join("scheduled-tab.log");
    let panes = r#"[{"id":1,"is_plugin":false,"tab_id":1,"title":"rimz-sidebar"},{"id":2,"is_plugin":false,"tab_id":1,"title":"sh"},{"id":3,"is_plugin":false,"tab_id":2,"tab_name":"rimzd","title":"loop","pane_command":"rimz loop watch --hold"}]"#;
    crate::common::room::seed_live_zellij_room(
        &env.runtime_paths(),
        &workspace.session_name,
        serde_json::from_str(panes).unwrap(),
    );
    let effect = match action {
        LoopLaunch::Check => format!(
            "dir = {:?}\ncheck = {:?}\n",
            cwd.to_str().unwrap(),
            shlex::try_join([
                env.rimz_bin().to_str().unwrap(),
                "agents",
                "coder",
                "repair"
            ])
            .unwrap(),
        ),
        LoopLaunch::Resident => "agent = \"codex\"\nprompt = \"repair\"\nstay = true\n".to_owned(),
        LoopLaunch::Agent => "agent = \"codex\"\nprompt = \"repair\"\n".to_owned(),
    };
    let timeout = if action == LoopLaunch::Check {
        "30s"
    } else {
        "2s"
    };
    write_loop_config(
        &env,
        &format!(
            "default-timeout = \"1s\"\n[tasks.rimzd]\nevery = \"1h\"\n{effect}root = {:?}\ntimeout = \"{timeout}\"\nthrottle = \"off\"\n",
            env.project_root.to_str().unwrap(),
        ),
    );
    if firer != LoopFirer::None {
        seed_agent_launch(
            &env,
            &env.project_root,
            "firer",
            rimz::agents::LaunchParams {
                parent_agent_id: (firer == LoopFirer::Subagent)
                    .then(|| AgentSessionId::from("launch_parent")),
                parent_agent_kind: (firer == LoopFirer::Subagent)
                    .then(|| AgentKind::new_unchecked("claude")),
                launch_depth: Some(match firer {
                    LoopFirer::Subagent => 1,
                    LoopFirer::RoomPin => 0,
                    _ => 3,
                }),
                ..Default::default()
            },
            (firer == LoopFirer::Ancestry).then(|| {
                rimz::pane::RuntimeOwner::new(
                    rimz::pane::RuntimeOwnerKind::Agent,
                    "firer",
                    std::process::id(),
                    None,
                )
            }),
        );
    }
    let mut command = env.rimz();
    if matches!(
        firer,
        LoopFirer::Subagent | LoopFirer::ChainLimit | LoopFirer::RoomPin
    ) {
        command
            .env("RIMZ_AGENT_KIND", "claude")
            .env("RIMZ_AGENT_ID", "launch_firer");
    }
    if firer == LoopFirer::RoomPin {
        let firer_root = env.home_root.join("firer-project");
        std::fs::create_dir(&firer_root).unwrap();
        assert!(init_git_repo(&firer_root));
        let firer_workspace = env.resolve_workspace(&firer_root);
        assert_ne!(workspace.workspace_id, firer_workspace.workspace_id);
        command.envs(rimz::workspace::pin_env(
            &firer_workspace.workspace_id,
            &firer_workspace.project_root,
        ));
    }
    let output = command
        .args(["--mux", "zellij", "loop", "run", "rimzd"])
        .env("SHELL", shell)
        .env("PATH", path_with_front(&agent_bin))
        .env("RIMZ_ZELLIJ_BIN", crate::common::zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace)
        .env("RIMZ_TEST_ZELLIJ_VERSION", "0.45.0")
        .env("RIMZ_TEST_ZELLIJ_LOG_LAYOUTS", "1")
        .env("RIMZ_TEST_ZELLIJ_LIST_PANES", panes)
        .env("ZELLIJ_PANE_ID", "2")
        .env("ZELLIJ_SESSION_NAME", &workspace.session_name)
        .env("RIMZ_CHANNEL", "probe")
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{} [Created 1s ago]\n", workspace.session_name),
        )
        .bounded_output()
        .unwrap();
    // The fire starts a workflow run; its detached advance opens the tab.
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    let trace = loop {
        let text = std::fs::read_to_string(&trace).unwrap_or_default();
        if text.contains("layout-json\t") || Instant::now() >= deadline {
            break text;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let tabs = trace
        .lines()
        .filter(|line| line.contains("action\tnew-tab"))
        .collect::<Vec<_>>();
    assert_eq!(
        tabs.len(),
        1,
        "{trace}\n{}\n{:?}",
        String::from_utf8_lossy(&output.stderr),
        read_loop_run_records(&env),
    );
    // A check's own launch is the loop's; an agent task's agent is its run's step.
    let automation = match action {
        LoopLaunch::Check => json!({"by": "loop"}),
        _ => json!({"by": "workflow", "step": "agent", "workflow": "rimzd"}),
    };
    assert!(tabs[0].contains("\t--name\tloop rimzd"), "{trace}");
    assert!(tabs[0].contains("\t--no-focus"), "{trace}");
    assert!(
        trace
            .lines()
            .any(|line| line.starts_with("layout\t") && line.contains("rimz-sidebar")),
        "run tab must dock a sidebar: {trace}",
    );
    assert!(!trace.contains("\tnew-pane\t"), "{trace}");

    let layout: String = serde_json::from_str(
        trace
            .lines()
            .find_map(|line| line.strip_prefix("layout-json\t"))
            .unwrap(),
    )
    .unwrap();
    let layout: kdl::KdlDocument = layout.parse().unwrap();
    let (_, argv) = loop_agent_pane(&layout).expect("agent pane args");
    let request = argv
        .windows(2)
        .find(|pair| pair[0] == "--request-file")
        .unwrap();
    let payload = std::fs::read_to_string(&request[1]).unwrap();
    let request = rimz::harness::launch::decode_exec_request("codex", None, &payload).unwrap();
    assert_eq!(
        serde_json::to_value(&request.identity.params).unwrap()["automation"],
        automation,
    );
    let mut pane_env: BTreeMap<String, String> = argv
        .iter()
        .take_while(|arg| arg.contains('='))
        .map(|arg| {
            let (key, value) = arg.split_once('=').unwrap();
            (key.to_owned(), value.to_owned())
        })
        .collect();
    let pane_channel = pane_env.get("RIMZ_CHANNEL").cloned();
    pane_env.insert(
        "RIMZ_AGENT_ID".to_owned(),
        request.identity.launch_id.clone().unwrap(),
    );
    pane_env.insert(
        "RIMZ_AGENT_NAME".to_owned(),
        request.identity.name.clone().unwrap(),
    );
    if let Some(run_id) = request.run_id {
        pane_env.insert("RIMZ_RUN_ID".to_owned(), run_id.to_string());
    }
    let pane_env = pane_env
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect::<Vec<_>>();
    let hook = env.run_installed_hook_in_pane(
        "codex",
        &json!({
            "hook_event_name": "SessionStart", "session_id": "loop-channel-session", "cwd": cwd,
        })
        .to_string(),
        &pane_env,
    );
    assert!(
        hook.status.success(),
        "{}",
        String::from_utf8_lossy(&hook.stderr)
    );
    let agents = env
        .store()
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents;
    let agent = agents
        .iter()
        .find(|agent| agent.name.as_deref() == request.identity.name.as_deref())
        .unwrap();
    assert!(agent.launched_by.is_none(), "{agent:?}");
    assert_eq!(
        serde_json::to_value(agent).unwrap()["automation"],
        automation
    );
    assert!(agent.parent_agent_id.is_none(), "{agent:?}");
    assert_eq!(agent.launch_depth, None, "{agent:?}");
    // A check launches into its own checkout's lane, which carries no name:
    // a foreign linked worktree for the check, trunk for an agent task.
    let (home, path) = if action == LoopLaunch::Check {
        (rimz::ids::LaneHome::Foreign, cwd.display().to_string())
    } else {
        (
            rimz::ids::LaneHome::Trunk,
            env.project_root.display().to_string(),
        )
    };
    let lane = agent.lane.as_ref().expect("the launch decided a lane");
    assert_eq!(
        (lane.home(), lane.path()),
        (home, path.as_str()),
        "{lane:?}"
    );
    assert_eq!(
        agent.channel.as_deref(),
        None,
        "recorded channel after the pane's registration hook"
    );
    assert_eq!(
        pane_channel.as_deref(),
        Some(""),
        "channel exported to the agent pane"
    );
    assert_eq!(
        agent.team, None,
        "the firer's team must not qualify a loop's bare profile"
    );
}

#[cfg(unix)]
fn loop_agent_pane(document: &kdl::KdlDocument) -> Option<(&kdl::KdlNode, Vec<String>)> {
    for node in document.nodes() {
        if node.name().value() == "pane"
            && let Some(args) = node.children().and_then(|children| children.get("args"))
        {
            let args = args
                .entries()
                .iter()
                .map(|entry| entry.value().as_string().unwrap().to_owned())
                .collect::<Vec<_>>();
            if args.iter().any(|arg| arg == "--request-file") {
                return Some((node, args));
            }
        }
        if let Some(pane) = node.children().and_then(loop_agent_pane) {
            return Some(pane);
        }
    }
    None
}

#[test]
fn resident_layout_refuses_account_pin() {
    let env = Env::new();
    let output = env
        .rimz()
        .args([
            "loop",
            "add",
            "resident",
            "--every",
            "1h",
            "--agent",
            "codex",
            "--prompt",
            "watch",
            "--stay",
            "--account",
            "work",
        ])
        .output()
        .unwrap();
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{error}");
    assert!(error.contains("--stay cannot use --account"), "{error}");
    assert!(error.contains("omit --account or --stay"), "{error}");
}

#[test]
fn condition_tick_observes_board_and_records_evidence() {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    env.record(&env.project_root);
    let scope = env.home_root.join("condition-scope");
    assert!(git_ok(
        &env.project_root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "condition-scope",
            scope.to_str().unwrap()
        ]
    ));
    let root_board = env.project_root.join(".rimz/team/blackboard.md");
    let board = scope.join(".rimz/team/blackboard.md");
    std::fs::create_dir_all(root_board.parent().unwrap()).unwrap();
    std::fs::create_dir_all(board.parent().unwrap()).unwrap();
    std::fs::write(&root_board, "Stage: Review\n").unwrap();
    std::fs::write(&board, "Stage: Done\n").unwrap();
    let receipt = loop_ok(
        &env,
        &[
            "loop",
            "add",
            "ready",
            "--when",
            "team.stage=Done",
            "--check",
            "true",
            "--root",
            scope.to_str().unwrap(),
            "--once",
        ],
    );
    assert!(
        receipt.contains(&format!(
            "scope: {}",
            canonical(&env.project_root).display()
        )),
        "{receipt}"
    );
    assert!(
        receipt.contains(&format!(
            "note: runs at the project root {}, not {}",
            canonical(&env.project_root).display(),
            canonical(&scope).display()
        )),
        "{receipt}"
    );
    assert!(
        receipt.contains("waiting · team.stage: Review"),
        "{receipt}"
    );
    let instances = read_loop_instances(&env);
    assert_eq!(instances.0["ready"].dir, None);
    assert_eq!(instances.0["ready"].run_dir(), canonical(&env.project_root));
    assert_eq!(instances.0["ready"].once, Some(true));
    loop_ok(&env, &["loop", "tick"]);
    let armed = rimz::harness::schedule::last_stamps(&env.runtime_paths());
    assert!(armed.contains_key("ready"));
    loop_ok(&env, &["loop", "tick"]);
    assert_eq!(
        rimz::harness::schedule::last_stamps(&env.runtime_paths()),
        armed
    );
    assert!(read_loop_run_records(&env).is_empty());
    let listing = loop_ok(&env, &["loop", "list"]);
    assert!(listing.contains("when team.stage=Done"), "{listing}");
    let shown = loop_ok(&env, &["loop", "show", "ready"]);
    assert!(
        shown.contains("waiting · 0 of 1 met") && shown.contains("team.stage=Done   ✗ Review"),
        "{shown}"
    );
    std::fs::write(&root_board, "Stage: Done\n").unwrap();
    std::fs::write(&board, "Stage: Review\n").unwrap();
    loop_ok(&env, &["loop", "tick"]);
    wait_for_loop_run_records(&env, "ready record and once-instance removal", |records| {
        !records.is_empty() && !read_loop_instances(&env).0.contains_key("ready")
    });
    assert_eq!(
        wait_for_loop_report(&env, "ready").result,
        LoopRunResult::Completed
    );
    // The fire's own row carries the evidence its run started on.
    let record = read_loop_run_records(&env).remove(0);
    assert_eq!(record.result, LoopRunResult::Launched);
    let condition = record.condition.unwrap();
    assert_eq!(condition.when, "team.stage=Done");
    assert_eq!(condition.readings["team.stage"].as_deref(), Some("Done"));
    assert!(!read_loop_instances(&env).0.contains_key("ready"));
}

/// A successful forge probe stamp that no fire stamp in a test postdates, so the room cache's
/// readings stay fresh under the loop condition floor.
fn fresh_probe_ms() -> i64 {
    Timestamp::now().as_millisecond() + 3_600_000
}

#[test]
fn queue_condition_selects_a_dequeued_open_pr_from_the_room_cache() {
    let env = Env::new();
    let runtime = env.runtime_paths();
    crate::common::room::seed_sidebar_heartbeat(
        &runtime,
        MuxName::Zellij,
        &env.resolve_workspace(&env.project_root).session_name,
        "queue",
    );
    let write_open = |facts: serde_json::Value| {
        std::fs::create_dir_all(runtime.lane_path("pr-state.json").parent().unwrap()).unwrap();
        std::fs::write(
            runtime.lane_path("pr-state.json"),
            json!({
                "branch_ci": {},
                "path_repos": {env.project_root.to_string_lossy().to_string(): "repo"},
                "repos": {"repo": {"ok": true, "refreshed_at_ms": fresh_probe_ms()}},
                "states": {env.project_root.to_string_lossy().to_string(): {
                    "state": "open", "number": 91, "open": facts,
                }},
            })
            .to_string(),
        )
        .unwrap();
    };
    write_open(json!({"head": "head-a", "base": "main", "queue": {
        "state": "dequeued", "at": "2026-10-03T12:46:03Z",
        "reason": "failed_checks", "commit": "queue-a",
    }}));
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "requeue",
            "--when",
            "pr=open && pr.queue=dequeued",
            "--check",
            "true",
        ],
    );
    let show = loop_ok(&env, &["loop", "show", "requeue"]);
    for term in ["pr=open", "pr.queue=dequeued"] {
        let line = show
            .lines()
            .find(|line| line.trim_start().starts_with(term))
            .unwrap_or_else(|| panic!("{show}"));
        assert!(line.contains('✓'), "{show}");
    }
    assert!(show.contains("dequeued"), "{show}");

    // A re-armed PR waiting to enter the queue reads armed, not none.
    for (facts, reading) in [
        (
            json!({"head": "head-a", "base": "main", "auto_merge": true,
                "queue": {"state": "queued", "at": "2026-10-03T13:00:00Z"}}),
            "queued",
        ),
        (
            json!({"head": "head-a", "base": "main", "auto_merge": true}),
            "armed",
        ),
    ] {
        write_open(facts);
        let show = loop_ok(&env, &["loop", "show", "requeue"]);
        let line = show
            .lines()
            .find(|line| line.trim_start().starts_with("pr.queue=dequeued"))
            .unwrap_or_else(|| panic!("{show}"));
        assert!(line.ends_with(reading) && !line.contains('✓'), "{show}");
    }
}

#[test]
fn condition_add_refuses_invalid_predicates_and_options() {
    let env = Env::new();
    for (flags, expected) in [
        (
            vec!["--when", "wat=yes"],
            "team.stage, ci, pr, pr.queue, pr.mergeable, pr.review, window.5h.left, window.7d.left",
        ),
        (
            vec!["--when", "window.5h.left>=40"],
            "a --check-only task has none; add --agent or --wait",
        ),
        (
            vec!["--when", "team.stage=Missing"],
            "no team defines stage Missing; stages: Done",
        ),
        (vec!["--for", "30m"], "--when"),
        (
            vec!["--project", "--when", "ci=passed"],
            "--project tasks cannot use --when yet",
        ),
    ] {
        let output = env
            .rimz()
            .args(["loop", "add", "bad", "--check", "true"])
            .args(flags)
            .output()
            .unwrap();
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success());
        assert!(error.contains(expected), "expected {expected}: {error}");
    }
    let receipt = loop_ok(
        &env,
        &[
            "loop",
            "add",
            "known",
            "--check",
            "true",
            "--when",
            "ci!=failed",
        ],
    );
    assert!(receipt.contains("trigger: when ci!=failed"), "{receipt}");
}

#[test]
fn paused_fanout_keeps_hold_and_fired_state() {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    env.record(&env.project_root);
    loop_ok(&env, &["worktree", "new", "lane"]);
    let owned = rimz::worktree::discover_owned(&env.project_root).unwrap();
    assert_eq!(owned.len(), 1);
    std::fs::create_dir_all(owned[0].path.join(".rimz/team")).unwrap();
    std::fs::write(
        owned[0].path.join(".rimz/team/blackboard.md"),
        "Stage: Done\n",
    )
    .unwrap();
    write_loop_config(
        &env,
        &format!(
            "[tasks.resident]\nroot = {:?}\nagent = \"codex\"\nprompt = \"repair\"\nstay = true\neach-worktree = true\nwhen = [\"team.stage=Done\"]\nfor = \"30m\"\n",
            env.project_root.to_str().unwrap()
        ),
    );
    loop_ok(&env, &["loop", "tick"]);
    loop_ok(&env, &["loop", "tick"]);
    let runtime = env.runtime_paths();
    let fire_path = runtime.lane_path("loop-fire.json");
    let when_path = runtime.lane_path("loop-when.json");
    let stamps = std::fs::read(&fire_path).unwrap();
    let mut states: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&when_path).unwrap()).unwrap();
    assert_eq!(states.as_object().unwrap().len(), 1);
    for fired in [false, true] {
        for state in states.as_object_mut().unwrap().values_mut() {
            state["fired"] = json!(fired);
        }
        std::fs::write(&when_path, serde_json::to_vec(&states).unwrap()).unwrap();
        loop_ok(&env, &["loop", "pause", "resident", "--for", "2h"]);
        loop_ok(&env, &["loop", "tick"]);
        assert_eq!(std::fs::read(&fire_path).unwrap(), stamps);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(&when_path).unwrap())
                .unwrap(),
            states
        );
        loop_ok(&env, &["loop", "enable", "resident"]);
        loop_ok(&env, &["loop", "tick"]);
        assert_eq!(std::fs::read(&fire_path).unwrap(), stamps);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(&when_path).unwrap())
                .unwrap(),
            states
        );
    }
}

#[test]
fn resident_add_accepts_layout_and_persists_launch_options() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let output = env
        .rimz()
        .args([
            "loop",
            "add",
            "resident",
            "--agent",
            "claude,codex",
            "--stay",
            "--each-worktree",
            "--when",
            "pr=open",
            "--takeover",
            "--subscribe",
            "ci.failed",
            "--subscribe",
            "pr.conflicted",
            "--mode",
            "ask",
            "--effort",
            "high",
            "--fast",
            "--prompt",
            "repair",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let config: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).unwrap()).unwrap();
    let entry = serde_json::to_value(&config.tasks.0["resident"]).unwrap();
    assert_eq!(entry["stay"], true);
    assert_eq!(entry["each-worktree"], true);
    assert_eq!(entry["takeover"], true);
    assert_eq!(entry["subscribe"][0]["signal"], "ci.failed");
    assert_eq!(entry["subscribe"][1]["signal"], "pr.conflicted");
    assert_eq!(entry["mode"], "ask");
    assert_eq!(entry["effort"], "high");
    assert_eq!(entry["fast"], true);
}

#[test]
fn resident_subscriptions_require_a_usable_scope_at_add() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let argv = |signal| {
        [
            "loop",
            "add",
            "resident",
            "--agent",
            "claude",
            "--stay",
            "--every",
            "1h",
            "--subscribe",
            signal,
            "--prompt",
            "watch",
        ]
    };
    for (signal, fix) in [
        ("ci.failed", "--each-worktree"),
        ("pr.conflicted", "--each-worktree"),
        ("team.stage", "team layout"),
    ] {
        let (_, error) = loop_fail(&env, &argv(signal));
        assert!(
            error.contains("--subscribe") && error.contains(fix),
            "{error}"
        );
    }
    if !init_git_repo(&env.project_root) {
        crate::common::skip("git unavailable");
        return;
    }
    let linked = env.home_root.join("linked");
    assert!(git_ok(
        &env.project_root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            linked.to_str().unwrap()
        ]
    ));
    let output = env
        .rimz()
        .current_dir(&linked)
        .args(argv("ci.failed"))
        .output()
        .unwrap();
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success()
            && error.contains(
                "--subscribe ci.failed needs a checkout scope; use --each-worktree with --when, or --worktree <name>"
            )
            && !error.contains("--root"),
        "{error}"
    );
}

#[test]
fn resident_named_worktree_saves_at_root_with_checkout_subscriptions_and_receipt() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let text = loop_ok(
        &env,
        &[
            "loop",
            "add",
            "booked",
            "--agent",
            "claude",
            "--stay",
            "--worktree",
            "landing",
            "--every",
            "1h",
            "--subscribe",
            "pr.merged",
            "--takeover",
            "--prompt",
            "review",
        ],
    );
    let config: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).unwrap()).unwrap();
    let entry = &config.tasks.0["booked"];
    assert_eq!(entry.worktree.as_deref(), Some("landing"));
    assert_eq!(entry.run_dir(), canonical(&env.project_root));
    assert!(
        text.contains("worktree `landing`") && text.contains("when the task fires"),
        "{text}"
    );
    assert!(
        text.contains("scope") && text.contains("guards") && text.contains("launch memory"),
        "{text}"
    );
    assert!(
        text.contains("before launch: stops idle agents in worktree `landing` first"),
        "{text}"
    );
    assert!(!env.project_root.join(".worktrees/landing").exists());
    let listing: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "list", "--json"])).unwrap();
    let row = listing["rooms"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|room| room["tasks"].as_array().unwrap())
        .find(|row| row["name"] == "booked")
        .unwrap();
    assert_eq!(row["worktree"], "landing", "{row}");
    let text = loop_ok(&env, &["loop", "list"]);
    assert!(
        text.contains("landing") && !text.contains("landing (here)"),
        "{text}"
    );
    let shown = loop_ok(&env, &["loop", "show", "landing"]);
    assert!(shown.contains("worktree landing → booked"), "{shown}");
    assert!(
        shown.contains("booked · one-off · worktree landing"),
        "{shown}"
    );
    assert!(!env.project_root.join(".worktrees/landing").exists());
}

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum NamedResident {
    Create,
    Solo,
    Reuse,
    Live,
    Closed,
    OtherTeam,
    OtherTeamClosed,
    Takeover,
    TakeoverMissing,
    TakeoverOtherTeam,
}

#[cfg(unix)]
#[test]
fn resident_named_worktree_creates_from_fetched_trunk_at_fire() {
    named_resident_case(NamedResident::Create);
}

#[cfg(unix)]
#[test]
fn resident_named_worktree_non_team_uses_configured_base() {
    named_resident_case(NamedResident::Solo);
}

#[cfg(unix)]
#[test]
fn resident_named_worktree_reuses_existing_checkout_without_reseeding() {
    named_resident_case(NamedResident::Reuse);
}

#[cfg(unix)]
#[test]
fn resident_named_worktree_refuses_live_cohort_without_delivering_prompt() {
    named_resident_case(NamedResident::Live);
}

#[cfg(unix)]
#[test]
fn resident_named_worktree_refuses_closed_cohort_without_takeover() {
    named_resident_case(NamedResident::Closed);
}

#[cfg(unix)]
#[test]
fn resident_named_worktree_refuses_a_different_teams_hold() {
    named_resident_case(NamedResident::OtherTeam);
}

#[cfg(unix)]
#[test]
fn resident_named_worktree_refuses_a_stopped_teams_unfinished_board() {
    named_resident_case(NamedResident::OtherTeamClosed);
}

#[cfg(unix)]
#[test]
fn resident_named_worktree_takeover_relaunches_fresh_and_leaves_scope_occupants() {
    named_resident_case(NamedResident::Takeover);
}

#[cfg(unix)]
#[test]
fn resident_named_worktree_takeover_of_missing_destination_stops_nobody() {
    named_resident_case(NamedResident::TakeoverMissing);
}

#[cfg(unix)]
#[test]
fn resident_named_worktree_takeover_stops_other_team_but_refuses_its_hold() {
    named_resident_case(NamedResident::TakeoverOtherTeam);
}

#[cfg(unix)]
fn named_resident_case(case: NamedResident) {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    env.record(&env.project_root);
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(&env);
    env.write_config(
        &env.project_root,
        r#"
        [profiles.codex]
        agent = "codex"
        [[agents.teams.forge.roles]]
        role = "coder"
        profile = "codex"
        owns = ["Build"]
        [agents.teams.forge]
        stages = ["Build"]
    "#,
    );
    loop_ok(&env, &["trust", "grant"]);
    let worktrees = env.home_root.join("worktrees");
    std::fs::write(
        env.rimz_home().join("config.toml"),
        format!("[agents.worktree]\ndir = {worktrees:?}\n"),
    )
    .unwrap();
    let destination = worktrees.join("landing");
    let absent = matches!(
        case,
        NamedResident::Create | NamedResident::Solo | NamedResident::TakeoverMissing
    );
    if !absent {
        loop_ok(&env, &["worktree", "new", "landing"]);
        std::fs::write(destination.join("keep.txt"), "existing work\n").unwrap();
    }
    // Advance the remote after booking, without moving the scope's HEAD.
    let remote = env.home_root.join("remote");
    assert!(git_ok(
        &env.project_root,
        &["clone", "-q", "--bare", ".", remote.to_str().unwrap()]
    ));
    assert!(git_ok(
        &env.project_root,
        &["remote", "add", "origin", remote.to_str().unwrap()]
    ));
    write_loop_config(
        &env,
        &format!(
            "[tasks.resident]\nagent={:?}\nprompt='review'\nroot={:?}\nevery='1h'\nstay=true\nworktree='landing'\ntakeover={}\n",
            if case == NamedResident::Solo {
                "codex"
            } else {
                "forge"
            },
            env.project_root,
            matches!(
                case,
                NamedResident::Takeover
                    | NamedResident::TakeoverMissing
                    | NamedResident::TakeoverOtherTeam
            ),
        ),
    );
    assert!(!absent || !destination.exists());
    std::fs::write(env.project_root.join("merged.txt"), "new trunk\n").unwrap();
    assert!(git_ok(&env.project_root, &["add", "merged.txt"]));
    assert!(git_ok(&env.project_root, &["commit", "-q", "-m", "merged"]));
    assert!(git_ok(&env.project_root, &["push", "-q", "origin", "main"]));
    assert!(git_ok(
        &env.project_root,
        &["reset", "--hard", "-q", "HEAD~1"]
    ));
    trust_codex_project(&env, &env.project_root);
    let provider_config = env.agent_config_path("codex");
    let mut provider_table: toml::Table = std::fs::read_to_string(&provider_config)
        .unwrap()
        .parse()
        .unwrap();
    provider_table
        .get_mut("projects")
        .unwrap()
        .as_table_mut()
        .unwrap()
        .insert(
            destination.to_str().unwrap().to_owned(),
            toml::Value::Table(toml::Table::from_iter([(
                "trust_level".into(),
                "trusted".into(),
            )])),
        );
    std::fs::write(provider_config, toml::to_string(&provider_table).unwrap()).unwrap();
    let workspace = env.resolve_workspace(&env.project_root);
    let store = env.store();
    let seed = |name: &str, path: &Path, pane: &str, team: Option<&str>| {
        seed_agent_launch(
            &env,
            path,
            name,
            rimz::agents::LaunchParams {
                team: team.map(str::to_owned),
                role: team.map(|_| "coder".to_owned()),
                channel: Some(if team.is_some() { "landing" } else { "scope" }.to_owned()),
                ..Default::default()
            },
            None,
        );
        let mut command = env.hook_command("claude");
        command
            .current_dir(path)
            .env(rimz::harness::launch::ENV_AGENT_NAME, name)
            .env("ZELLIJ", "1")
            .env("ZELLIJ_SESSION_NAME", &workspace.session_name)
            .env("ZELLIJ_PANE_ID", pane);
        let output = env
            .spawn_payload(
                command,
                &json!({"hook_event_name":"SessionStart", "session_id":name, "cwd":path})
                    .to_string(),
            )
            .wait_with_output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        env.drain_hooks();
    };
    seed("scope-agent", &env.project_root, "50", None);
    if case == NamedResident::Reuse {
        seed("destination-solo", &destination, "52", None);
    }
    let occupied = matches!(
        case,
        NamedResident::Live
            | NamedResident::Closed
            | NamedResident::OtherTeam
            | NamedResident::OtherTeamClosed
            | NamedResident::Takeover
            | NamedResident::TakeoverOtherTeam
    );
    if occupied {
        let team = if matches!(
            case,
            NamedResident::OtherTeam
                | NamedResident::OtherTeamClosed
                | NamedResident::TakeoverOtherTeam
        ) {
            "other"
        } else {
            "forge"
        };
        seed("old-leader", &destination, "51", Some(team));
        rimz::harness::team_notes::register(
            &store,
            &destination,
            team,
            &toml::from_str("stages = ['Build']").unwrap(),
            rimz::harness::team_notes::Launch::Resume,
        )
        .unwrap();
        std::fs::write(
            destination.join(".rimz/team/blackboard.md"),
            "Stage: Build\n",
        )
        .unwrap();
        if matches!(case, NamedResident::Closed | NamedResident::OtherTeamClosed) {
            stamp_session_ended(&env, "old-leader");
        }
    }
    crate::common::room::seed_live_zellij_room(
        &env.runtime_paths(),
        &workspace.session_name,
        Vec::new(),
    );
    let agent_bin = crate::common::write_failing_agent_shim(&env, "codex", 1);
    let shell = write_fake_login_shell(&env, "rimz-test-sh", &[]);
    let trace = env.home_root.join("named-resident.log");
    // The trace shim cannot end a pane's fixture owner. Model the
    // provider's SessionEnd so reconciliation sees the cohort after its stop.
    let zellij = crate::common::write_path_shim(
        &env.home_root.join("mux-bin"),
        "zellij",
        r#""$RIMZ_TEST_ZELLIJ_TRACE" "$@" || exit "$?"
for arg do
    if [ "$arg" = close-pane ]; then
        printf '%s\n' "$RIMZ_TEST_ENDED_PAYLOAD" | RIMZ_AGENT_PID="$RIMZ_TEST_AGENT_OWNER_PID" "$RIMZ_TEST_RIMZ" hooks feed --source claude || exit "$?"
        exec "$RIMZ_TEST_RIMZ" hooks drain --project-root "$RIMZ_TEST_SCOPE" --once
    fi
done
"#,
    );
    let command = || {
        let mut command = env.rimz();
        command
            .args(["--mux", "zellij"])
            .env("SHELL", &shell)
            .env("PATH", path_with_front(&agent_bin))
            .env("RIMZ_ZELLIJ_BIN", &zellij)
            .env(
                "RIMZ_TEST_ZELLIJ_TRACE",
                crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace")),
            )
            .env("RIMZ_TEST_RIMZ", env.rimz().get_program())
            .env("RIMZ_TEST_AGENT_OWNER_PID", env.agent_owner_pid().to_string())
            .env("RIMZ_TEST_SCOPE", &env.project_root)
            .env(
                "RIMZ_TEST_ENDED_PAYLOAD",
                json!({"hook_event_name":"SessionEnd", "session_id":"old-leader", "cwd":destination}).to_string(),
            )
            .env("RIMZ_TEST_ZELLIJ_LOG", &trace)
            .env("RIMZ_TEST_ZELLIJ_LIST_PANES", "[]")
            .env(
                "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
                format!("{} [Created 1s ago]\n", workspace.session_name),
            );
        command
    };
    let before_queue = store.list_messages().unwrap();
    let before_history = store.list_message_history().unwrap();
    let output = command()
        .args(["loop", "run", "resident"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let refused = matches!(
        case,
        NamedResident::Live
            | NamedResident::Closed
            | NamedResident::OtherTeam
            | NamedResident::OtherTeamClosed
            | NamedResident::TakeoverOtherTeam
    );
    let fired = last_loop_record(&env);
    assert_eq!(fired.result, LoopRunResult::Launched);
    assert_eq!(fired.checkout.as_deref(), Some(env.project_root.as_path()));
    // The fire returns once its run exists; the launch, or its refusal, lands in the run.
    let leader = |store: &rimz::Store| {
        store
            .runtime_projection(rimz::RuntimeScope::Audit)
            .unwrap()
            .agents
            .iter()
            .find(|agent| agent.loop_task.as_deref() == Some("resident"))
            .cloned()
    };
    let record = if refused {
        wait_for_loop_report(&env, "resident")
    } else {
        let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
        while leader(&store).is_none() {
            let rows = read_loop_run_records(&env);
            assert!(
                rows.len() == 1 && Instant::now() < deadline,
                "resident never launched: {rows:?}\n{}\n{:?}",
                loop_run_journal(&env, &env.project_root, &fired),
                store
                    .runtime_projection(rimz::RuntimeScope::Audit)
                    .unwrap()
                    .agents
                    .iter()
                    .map(|agent| (
                        agent.name.clone(),
                        agent.loop_task.clone(),
                        agent.worktree_path.clone(),
                        agent.ended_at
                    ))
                    .collect::<Vec<_>>()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        fired.clone()
    };
    let trace_text = std::fs::read_to_string(&trace).unwrap_or_default();
    let residents = store.paths().root.join("records/loop-launches.json");
    let agents = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents;
    assert!(
        agents
            .iter()
            .find(|agent| agent.name.as_deref() == Some("scope-agent"))
            .unwrap()
            .ended_at
            .is_none()
    );
    assert!(!trace_text.contains("--pane-id 50"), "{trace_text}");
    if matches!(
        case,
        NamedResident::Takeover | NamedResident::TakeoverOtherTeam
    ) {
        assert!(
            agents
                .iter()
                .find(|agent| agent.name.as_deref() == Some("old-leader"))
                .unwrap()
                .ended_at
                .is_some()
        );
    }
    if refused {
        assert_eq!(record.result, LoopRunResult::Failed, "{record:?}");
        let error = record.error.as_deref().unwrap();
        assert!(error.contains("landing"), "{error}");
        if case == NamedResident::Live {
            for word in [
                "forge",
                "@coder#landing",
                "prompt was not delivered",
                "--takeover",
            ] {
                assert!(error.contains(word), "{error}");
            }
        } else if case == NamedResident::Closed {
            assert!(
                error.contains("resume") && error.contains("fresh"),
                "{error}"
            );
        } else {
            assert!(
                error.contains("other") && error.contains("holds team"),
                "{error}"
            );
        }
        assert_eq!(store.list_messages().unwrap(), before_queue);
        assert_eq!(store.list_message_history().unwrap(), before_history);
        assert!(!residents.exists());
        assert!(!trace_text.contains("new-tab"), "{trace_text}");
        assert_eq!(
            trace_text.matches("close-pane").count(),
            usize::from(case == NamedResident::TakeoverOtherTeam),
            "{trace_text}"
        );
        // `run_log::record_transition` appends the report row before it notes the strike.
        let key = machine_task_key("resident");
        let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
        while !read_loop_strikes(&env).contains_key(&key) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(read_loop_strikes(&env).get(&key), Some(&1));
        if case == NamedResident::Live {
            let direct = command()
                .args(["teams", "forge", "-w", "landing", "ordinary launch prompt"])
                .output()
                .unwrap();
            assert!(
                direct.status.success(),
                "{}",
                String::from_utf8_lossy(&direct.stderr)
            );
            let messages = store.list_messages().unwrap();
            let history = store.list_message_history().unwrap();
            assert_eq!(
                messages.len() + history.len(),
                before_queue.len() + before_history.len() + 1,
                "{direct:?}"
            );
            assert!(
                messages
                    .iter()
                    .chain(&history)
                    .any(|message| message.text == "ordinary launch prompt")
            );
        }
        return;
    }
    // The run remembers the firing checkout; no fire writes the resident launch file.
    assert!(!residents.exists());
    let workflows = rimz::harness::schedule::launch_ledger::load_workflows(store.paths()).unwrap();
    assert!(
        workflows.tasks["resident"].contains_key(&env.project_root),
        "{workflows:?}"
    );
    let marker = rimz::worktree::read_marker_for_worktree(&destination)
        .unwrap()
        .unwrap();
    assert_eq!(marker.worktree_path, destination);
    assert_eq!(
        destination.join("merged.txt").exists(),
        absent && case != NamedResident::Solo
    );
    assert_eq!(destination.join("keep.txt").exists(), !absent);
    let leader = leader(&store).unwrap();
    assert_eq!(leader.worktree_path.as_deref(), destination.to_str());
    assert_eq!(leader.channel.as_deref(), Some("landing"));
    assert_eq!(
        trace_text.matches("close-pane").count(),
        usize::from(case == NamedResident::Takeover),
        "{trace_text}"
    );
    assert!(
        command()
            .args(["loop", "run", "resident"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(read_loop_run_records(&env).len(), 1);
}

#[test]
fn resident_subscribe_requires_prompt_leader_and_installed_hooks() {
    let env = Env::new();
    for (spec, expected) in [("term", "prompt leader"), ("claude", "--subscribe")] {
        let (_, error) = loop_fail(
            &env,
            &[
                "loop",
                "add",
                "resident",
                "--agent",
                spec,
                "--stay",
                "--each-worktree",
                "--when",
                "pr=open",
                "--subscribe",
                "ci.failed",
                "--prompt",
                "repair",
            ],
        );
        assert!(error.contains(expected), "{error}");
    }
    assert!(read_loop_instances(&env).0.is_empty());
}

#[cfg(unix)]
#[test]
fn resident_launch_deduplicates_and_manual_fire_relaunches() {
    resident_launch_case(false, false);
}

#[cfg(unix)]
#[test]
fn resident_show_summarizes_worktree_conditions() {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    write_loop_config(
        &env,
        &format!(
            "[tasks.resident]\nroot = {:?}\nagent = \"codex\"\nprompt = \"repair\"\nstay = true\neach-worktree = true\nwhen = [\"pr=open\"]\n",
            env.project_root
        ),
    );
    let summary = loop_ok(&env, &["loop", "show", "resident"]);
    assert!(summary.contains("WORKTREES (0 owned)"), "{summary}");
    assert!(!summary.contains("SUBSCRIPTIONS"), "{summary}");
}

#[test]
fn resident_show_survives_failed_worktree_discovery() {
    let env = Env::new();
    assert!(rimz::worktree::discover_owned(&env.project_root).is_err());
    write_loop_config(
        &env,
        &format!(
            "[tasks.resident]\nroot = {:?}\nagent = \"codex\"\nprompt = \"repair\"\nstay = true\neach-worktree = true\nwhen = [\"team.stage=Done\"]\n",
            env.project_root
        ),
    );
    let shown = loop_ok(&env, &["loop", "show", "resident"]);
    assert!(
        shown.contains("WORKTREES\n  worktrees unavailable:"),
        "{shown}"
    );
    assert!(!shown.contains("0 owned"), "{shown}");
    let json = loop_ok(&env, &["loop", "show", "resident", "--json"]);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&json).unwrap()["worktrees"],
        json!([])
    );
}

#[test]
fn resident_show_survives_unavailable_worktree_runtime() {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    write_loop_config(
        &env,
        &format!(
            "[tasks.resident]\nroot = {:?}\nagent = \"codex\"\nprompt = \"repair\"\nstay = true\neach-worktree = true\nwhen = [\"team.stage=Done\"]\n",
            env.project_root
        ),
    );
    let runtime = env.runtime_root.join("x".repeat(120));
    let output = env
        .rimz()
        .args(["loop", "show", "resident"])
        .env("XDG_RUNTIME_DIR", &runtime)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let shown = String::from_utf8_lossy(&output.stdout);
    assert!(
        shown.contains("WORKTREES\n  worktrees unavailable:"),
        "{shown}"
    );
    assert!(!shown.contains("0 owned"), "{shown}");
    let output = env
        .rimz()
        .args(["loop", "show", "resident", "--json"])
        .env("XDG_RUNTIME_DIR", runtime)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["worktrees"],
        json!([])
    );
}

#[test]
fn resident_show_keeps_the_screen_on_room_catalog_failure() {
    let env = Env::new();
    let root = env.home_root.join("other-project");
    std::fs::create_dir(&root).unwrap();
    assert!(init_git_repo(&root));
    write_loop_config(
        &env,
        &format!(
            "[tasks.resident]\nroot = {:?}\nagent = \"codex\"\nprompt = \"repair\"\nstay = true\neach-worktree = true\nwhen = [\"team.stage=Done\"]\n",
            root
        ),
    );
    let instances = env
        .state_path_for(&root)
        .root
        .join("records/loop-instances.json");
    std::fs::create_dir_all(instances.parent().unwrap()).unwrap();
    std::fs::write(instances, "not json").unwrap();
    let json = env
        .rimz()
        .args(["loop", "show", "resident", "--json"])
        .output()
        .unwrap();
    let human = env
        .rimz()
        .args(["loop", "show", "resident"])
        .output()
        .unwrap();
    let shown = String::from_utf8_lossy(&human.stdout);
    assert!(
        shown.contains("WORKTREES (0 owned)") && shown.contains("action:"),
        "{human:?}"
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&json.stdout).unwrap()["worktrees"],
        json!([])
    );
    for output in [&human, &json] {
        assert!(
            !output.status.success()
                && String::from_utf8_lossy(&output.stderr).contains("loop-instances.json"),
            "an unreadable catalog fails both forms after they print: {output:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn resident_show_drops_a_daemon_leader_reaped_by_agents_list() {
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let workspace = env.resolve_workspace(&env.project_root);
    let kind = AgentKind::new_unchecked("codex");
    store
        .append_event(&rimz::store::event::EventEnvelope::agent_launched(
            workspace.workspace_id,
            &workspace.session_name,
            &kind,
            rimz::store::event::AgentLaunchPayload {
                system_prompt_digest: None,
                agent_id: "ghost-session".into(),
                launch_id: None,
                agent_name: "ghost-leader".into(),
                agent_name_explicit: true,
                launch: Default::default(),
                state: rimz::store::event::AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner: Some(rimz::pane::RuntimeOwner::new(
                    rimz::pane::RuntimeOwnerKind::Daemon,
                    "ghost-session",
                    std::process::id(),
                    None,
                )),
                worktree_path: Some(env.project_root.display().to_string()),
                worktree_branch: None,
                prompt: None,
                description: None,
            },
        ))
        .unwrap();
    let reap = env.runtime_paths().lane_path("codex-daemon-reap.json");
    std::fs::create_dir_all(reap.parent().unwrap()).unwrap();
    std::fs::write(
        reap,
        json!({
            "produced_at_ms": rimz::utils::time::unix_now_ms(),
            "daemon_pids": [std::process::id()],
            "loaded": [],
        })
        .to_string(),
    )
    .unwrap();
    assert!(
        store
            .snapshot_cached()
            .unwrap()
            .agents
            .iter()
            .any(|agent| agent.name.as_deref() == Some("ghost-leader"))
    );
    crate::common::room::seed_live_zellij_room(
        &env.runtime_paths(),
        &workspace.session_name,
        Vec::new(),
    );
    let listed = env
        .rimz()
        .args(["--mux", "zellij", "agents", "list", "--json"])
        .env("RIMZ_ZELLIJ_BIN", crate::common::zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", env.home_root.join("reap.log"))
        .env("RIMZ_TEST_ZELLIJ_LIST_PANES", "[]")
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{} [Created 1s ago]\n", workspace.session_name),
        )
        .output()
        .unwrap();
    assert!(listed.status.success(), "{listed:?}");
    let listed = String::from_utf8_lossy(&listed.stdout);
    assert!(!listed.contains("ghost-leader"), "{listed}");
    write_loop_config(
        &env,
        &format!(
            "[tasks.resident]\nroot = {:?}\nagent = \"codex\"\nprompt = \"repair\"\nevery = \"1h\"\nstay = true\nsubscribe = [{{ signal = \"ci.failed\" }}]\n",
            env.project_root
        ),
    );
    let ledger = store.paths().root.join("records/loop-launches.json");
    std::fs::create_dir_all(ledger.parent().unwrap()).unwrap();
    std::fs::write(
        ledger,
        serde_json::to_vec(&serde_json::json!({"resident": {
            env.project_root.display().to_string():
                {"at": Timestamp::now(), "leader": "ghost-leader"}
        }}))
        .unwrap(),
    )
    .unwrap();
    let shown = loop_ok(&env, &["loop", "show", "resident"]);
    assert!(shown.contains("0 armed, no live leader"), "{shown}");
}

#[test]
fn resident_inspector_reads_the_planners_checkout_clocks_without_writes() {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    env.record(&env.project_root);
    for name in ["holding", "ready", "waiting"] {
        loop_ok(&env, &["worktree", "new", name]);
    }
    let owned = rimz::worktree::discover_owned(&env.project_root).unwrap();
    for worktree in &owned {
        std::fs::create_dir_all(worktree.path.join(".rimz/team")).unwrap();
        std::fs::write(
            worktree.path.join(".rimz/team/blackboard.md"),
            if worktree.marker.name == "waiting" {
                "Stage: Review\n"
            } else {
                "Stage: Done\n"
            },
        )
        .unwrap();
    }
    let config = format!(
        "[tasks.resident]\nroot = {:?}\nagent = \"codex\"\nprompt = \"repair\"\nstay = true\neach-worktree = true\nwhen = [\"team.stage=Done\"]\n",
        env.project_root
    );
    write_loop_config(&env, &format!("{config}for = \"3m\"\n"));
    loop_ok(&env, &["loop", "tick"]);
    loop_ok(&env, &["loop", "tick"]);
    let runtime = env.runtime_paths();
    let when_path = runtime.lane_path("loop-when.json");
    let mut clocks: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&when_path).unwrap()).unwrap();
    let now = jiff::Timestamp::now();
    for worktree in owned
        .iter()
        .filter(|worktree| worktree.marker.name != "waiting")
    {
        let checkout = rimz::utils::path::normalize_path_lexical(&worktree.marker.worktree_path);
        let key = serde_json::to_string(&("resident", checkout.to_string_lossy())).unwrap();
        assert!(clocks[&key].is_object(), "planner clock missing for {key}");
        clocks[&key]["since"] = json!(
            now.checked_sub(jiff::SignedDuration::from_secs(
                if worktree.marker.name == "holding" {
                    120
                } else {
                    180
                }
            ))
            .unwrap()
        );
    }
    let bytes = serde_json::to_vec(&clocks).unwrap();
    std::fs::write(&when_path, &bytes).unwrap();
    let shown = loop_ok(&env, &["loop", "show", "resident", "--json"]);
    let shown: serde_json::Value = serde_json::from_str(&shown).unwrap();
    assert_eq!(
        shown["worktrees"]
            .as_array()
            .expect("inspected checkout rows")
            .len(),
        3
    );
    assert_eq!(shown["worktrees"][0]["state"], "holding");
    assert_eq!(shown["worktrees"][0]["hold_ms"], 180000);
    assert!(shown["worktrees"][0]["held_ms"].as_u64().unwrap() >= 120000);
    assert_eq!(shown["worktrees"][1]["state"], "ready");
    assert_eq!(shown["worktrees"][2]["short"], json!(["team.stage"]));
    assert_eq!(shown["worktrees"][2]["state"], "waiting");
    assert_eq!(shown["worktrees"][2]["readings"]["team.stage"], "Review");
    assert_eq!(
        std::fs::read(&when_path).unwrap(),
        bytes,
        "inspection must not update clocks"
    );
    std::fs::remove_file(&when_path).unwrap();
    let shown = loop_ok(&env, &["loop", "show", "resident", "--json"]);
    let shown: serde_json::Value = serde_json::from_str(&shown).unwrap();
    assert_eq!(
        shown["worktrees"][0]["state"], "holding",
        "a true condition without a matching clock must start its hold"
    );
    assert_eq!(shown["worktrees"][0]["hold_ms"], 180000);
    assert_eq!(shown["worktrees"][0]["held_ms"], 0);
    assert!(
        shown["worktrees"][0]["since"]
            .as_str()
            .unwrap()
            .parse::<Timestamp>()
            .unwrap()
            >= now
    );
    let summary = loop_ok(&env, &["loop", "show", "resident"]);
    assert_eq!(summary.matches("holding 0s/3m").count(), 2, "{summary}");
    assert!(
        !when_path.exists(),
        "inspection must not start a stored clock"
    );
    let mut stale = clocks;
    for clock in stale.as_object_mut().unwrap().values_mut() {
        clock["fingerprint"][0] = json!("team.stage=Review");
    }
    let stale = serde_json::to_vec(&stale).unwrap();
    std::fs::write(&when_path, &stale).unwrap();
    let shown = loop_ok(&env, &["loop", "show", "resident", "--json"]);
    let shown: serde_json::Value = serde_json::from_str(&shown).unwrap();
    assert_eq!(shown["worktrees"][0]["state"], "holding");
    assert_eq!(shown["worktrees"][0]["held_ms"], 0);
    assert_eq!(std::fs::read(&when_path).unwrap(), stale);
    write_loop_config(&env, &config);
    let shown = loop_ok(&env, &["loop", "show", "resident", "--json"]);
    let shown: serde_json::Value = serde_json::from_str(&shown).unwrap();
    assert_eq!(shown["worktrees"][0]["state"], "ready");
    assert_eq!(shown["worktrees"][1]["state"], "ready");
}

#[test]
fn resident_show_json_adds_unfolded_worktrees_and_keeps_existing_keys() {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    env.record(&env.project_root);
    loop_ok(&env, &["worktree", "new", "waiting"]);
    let owned = rimz::worktree::discover_owned(&env.project_root).unwrap();
    write_loop_config(
        &env,
        &format!(
            "[tasks.resident]\nroot = {:?}\nagent = \"codex\"\nprompt = \"repair\"\nstay = true\neach-worktree = true\nwhen = [\"pr=open && ci=passed && team.stage=Done\"]\n",
            env.project_root
        ),
    );
    let raw = loop_ok(&env, &["loop", "show", "resident", "--json"]);
    let shown: serde_json::Value = serde_json::from_str(&raw).unwrap();
    for key in [
        "task", "entry", "launches", "runs", "running", "held", "waiting",
    ] {
        assert!(shown.get(key).is_some(), "existing key {key} must remain");
    }
    assert_eq!(
        shown["worktrees"]
            .as_array()
            .expect("additive worktrees array")
            .len(),
        1
    );
    assert_eq!(
        shown["worktrees"][0]["checkout"],
        rimz::utils::path::normalize_path_lexical(&owned[0].marker.worktree_path)
            .to_string_lossy()
            .as_ref()
    );
    assert_eq!(shown["worktrees"][0]["name"], "waiting");
    assert_eq!(shown["worktrees"][0]["state"], "waiting");
    assert_eq!(
        shown["worktrees"][0]["readings"],
        json!({"pr": null, "ci": null, "team.stage": null})
    );
    assert_eq!(
        shown["worktrees"][0]["short"],
        json!(["pr", "ci", "team.stage"])
    );
    let readings = raw.split("\"readings\"").last().unwrap();
    assert!(
        readings.find("\"pr\"").unwrap() < readings.find("\"ci\"").unwrap(),
        "readings must follow expression order: {raw}"
    );
    let all = loop_ok(&env, &["loop", "show", "resident", "--all"]);
    assert!(
        all.lines().any(|line| line.starts_with("  waiting ")),
        "{all}"
    );
    assert!(!all.contains("… 1 more waiting"), "{all}");
}

#[test]
fn loop_show_lists_each_derived_subscription() {
    loop_show_subscriptions_case(false);
}

#[test]
fn loop_show_loads_subscriptions_and_results_from_the_tasks_room() {
    loop_show_subscriptions_case(true);
}

fn loop_show_subscriptions_case(other_room: bool) {
    use rimz::harness::schedule::run_log::SignalRecord;

    let env = Env::new();
    let root = if other_room {
        let root = env.home_root.join("other-room");
        std::fs::create_dir(&root).unwrap();
        root
    } else {
        env.project_root.clone()
    };
    write_loop_config(
        &env,
        &format!(
            "[tasks.sweep]\nroot = {root:?}\nagent = \"codex\"\nprompt = \"repair\"\nstay = true\nwhen = [\"pr=open\"]\n"
        ),
    );
    let subscription = |checkout: &str, handle: &str, signal: &str| TaskEntry {
        root: root.clone(),
        dir: Some(env.home_root.join(checkout)),
        loop_task: Some("sweep".into()),
        wait: Some(TaskTarget {
            kind: AgentKind::new_unchecked("codex"),
            session: "session".into(),
            handle: handle.into(),
        }),
        signal: Some(signal.into()),
        ..TaskEntry::default()
    };
    let mut unrelated = subscription("feature-a", "@fixer#feature-a", "ci.failed");
    unrelated.loop_task = Some("another-loop".into());
    let mut team_only = unrelated.clone();
    team_only.loop_task = None;
    team_only.team = Some("forge#feature-a".parse().unwrap());
    let tasks = Tasks(BTreeMap::from([
        (
            "derived-a".into(),
            subscription("feature-a", "@fixer#feature-a", "ci.failed"),
        ),
        (
            "derived-b".into(),
            subscription("feature-a", "@fixer#feature-a", "pr.merged"),
        ),
        (
            "derived-c".into(),
            subscription("feature-b", "@reviewer#feature-b", "ci.failed"),
        ),
        (
            "derived-d".into(),
            subscription("feature-b", "@reviewer#feature-b", "ci.passed"),
        ),
        ("unrelated".into(), unrelated),
        ("team-only".into(), team_only),
    ]));
    let path = env
        .state_path_for(&root)
        .root
        .join("records/loop-instances.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_vec(&tasks).unwrap()).unwrap();
    let record = |name: &str, result: LoopRunResult, minutes: i64| {
        let mut record = LoopRunRecord::new(name, result, LoopRunMode::Scheduled, 0);
        record.root = Some(root.clone());
        record.at = Timestamp::now() - SignedDuration::from_mins(minutes);
        if result == LoopRunResult::SignalSkipped {
            record.signal = Some(SignalRecord {
                name: tasks.0[name].signal.as_deref().unwrap().parse().unwrap(),
                payload: serde_json::Map::new(),
            });
        }
        record
    };
    let mut foreign = record("derived-a", LoopRunResult::Failed, 1);
    foreign.root = Some(env.home_root.join("foreign-room"));
    write_loop_run_records(
        &env,
        &[
            record("derived-a", LoopRunResult::Delivered, 5),
            record("derived-b", LoopRunResult::Completed, 4),
            record("derived-b", LoopRunResult::SignalSkipped, 3),
            record("derived-b", LoopRunResult::SignalSkipped, 2),
            record("derived-b", LoopRunResult::SignalSkipped, 1),
            record("derived-d", LoopRunResult::SignalSkipped, 1),
            foreign,
        ],
    );
    let shown = loop_ok(&env, &["loop", "show", "sweep"]);
    let subscriptions = shown
        .split_once("SUBSCRIPTIONS\n")
        .unwrap_or_else(|| panic!("missing SUBSCRIPTIONS block: {shown}"))
        .1
        .split("\n\n")
        .next()
        .unwrap();
    for (name, checkout, target, signal, last) in [
        (
            "derived-a",
            "feature-a",
            "@fixer#feature-a",
            "ci.failed",
            "✓ 5m ago",
        ),
        (
            "derived-b",
            "feature-a",
            "@fixer#feature-a",
            "pr.merged",
            "✓ 4m ago",
        ),
        (
            "derived-c",
            "feature-b",
            "@reviewer#feature-b",
            "ci.failed",
            "never fired",
        ),
        (
            "derived-d",
            "feature-b",
            "@reviewer#feature-b",
            "ci.passed",
            "heard ci.passed 1m ago",
        ),
    ] {
        let row = subscriptions
            .lines()
            .find(|line| line.contains(name))
            .unwrap_or_else(|| panic!("missing {name}: {shown}"));
        for cell in [checkout, target, signal, last] {
            assert!(row.contains(cell), "missing {cell}: {row}");
        }
        assert_eq!(subscriptions.matches(name).count(), 1, "{shown}");
    }
    assert!(!subscriptions.contains("unrelated"), "{shown}");
    assert!(!subscriptions.contains("team-only"), "{shown}");
    assert!(!subscriptions.contains("✗ failed"), "{shown}");
    assert!(!subscriptions.contains("skipped"), "{shown}");
}

#[cfg(unix)]
#[test]
fn resident_worktree_launch_preserves_marker_path() {
    resident_launch_case(true, false);
}

#[cfg(unix)]
#[test]
fn resident_team_layout_uses_the_helpers_resolved_checkout() {
    resident_launch_case(true, true);
}

#[test]
fn agents_team_cwd_subdirectory_refuses_root_checkout_bindings() {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    env.record(&env.project_root);
    env.install_agent_hooks("codex");
    env.write_config(
        &env.project_root,
        r#"
        [profiles.codex]
        agent = "codex"
        [[agents.teams.forge.roles]]
        role = "coder"
        profile = "codex"
        signals = [{ signal = "ci.failed" }]
        "#,
    );
    loop_ok(&env, &["trust", "grant"]);
    let cwd = env.project_root.join("docs");
    std::fs::create_dir(&cwd).unwrap();
    let output = env
        .rimz()
        .args(["agents", "forge", "--cwd"])
        .arg(&cwd)
        .output()
        .unwrap();
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{error}");
    assert!(
        error.contains("root checkout needs an explicit scope"),
        "{error}"
    );
}

#[cfg(unix)]
fn resident_launch_case(each_worktree: bool, team: bool) {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    env.record(&env.project_root);
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(&env);
    let actual = env.home_root.join("actual-worktrees");
    let alias = env.home_root.join("linked-worktrees");
    std::fs::create_dir(&actual).unwrap();
    std::os::unix::fs::symlink(&actual, &alias).unwrap();
    std::fs::create_dir(alias.join("spare")).unwrap();
    let worktree_dir = if each_worktree {
        alias.join("spare/..")
    } else {
        alias.clone()
    };
    std::fs::write(
        env.rimz_home().join("config.toml"),
        format!(
            "[agents.worktree]\ndir = {:?}\n",
            worktree_dir.to_str().unwrap()
        ),
    )
    .unwrap();
    loop_ok(&env, &["worktree", "new", "lane"]);
    if team {
        env.write_config(
            &env.project_root,
            r#"
            [profiles.codex]
            agent = "codex"
            [[agents.teams.forge.roles]]
            role = "coder"
            profile = "codex"
            signals = [{ signal = "ci.failed" }]
        "#,
        );
        loop_ok(&env, &["trust", "grant"]);
    }
    let checkout = alias.join("lane");
    let marker = rimz::worktree::read_marker_for_worktree(&checkout)
        .unwrap()
        .unwrap();
    assert_eq!(marker.worktree_path, worktree_dir.join("lane"));
    assert_ne!(checkout, canonical(&checkout));
    trust_codex_project(&env, &checkout);
    let agent_bin = crate::common::write_failing_agent_shim(&env, "codex", 1);
    let shell = write_fake_login_shell(&env, "rimz-test-sh", &[]);
    let workspace = env.resolve_workspace(&env.project_root);
    let trace = env.home_root.join("resident-mux.log");
    let _room = if each_worktree {
        Some(crate::common::room::ShimRoom::watch(&env, &trace))
    } else {
        crate::common::room::seed_live_zellij_room(
            &env.runtime_paths(),
            &workspace.session_name,
            Vec::new(),
        );
        None
    };
    let command = || {
        let mut command = env.rimz();
        command
            .args(["--mux", "zellij"])
            .env("SHELL", &shell)
            .env("PATH", path_with_front(&agent_bin))
            .env(
                "RIMZ_ZELLIJ_BIN",
                crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace")),
            )
            .env("RIMZ_TEST_ZELLIJ_LOG", &trace)
            .env(
                "RIMZ_TEST_ZELLIJ_LIST_PANES",
                if each_worktree {
                    crate::common::room::MATERIALIZED_ROOM_PANES
                } else {
                    "[]"
                },
            )
            .env(
                "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
                if each_worktree {
                    String::new()
                } else {
                    format!("{} [Created 1s ago]\n", workspace.session_name)
                },
            );
        if team {
            command.env("RIMZ_TEST_ZELLIJ_LOG_LAYOUTS", "1");
        }
        command
    };
    write_loop_config(
        &env,
        &format!(
            "[tasks.resident]\nagent = {:?}\nprompt = \"repair\"\nroot = {:?}\ndir = {:?}\n{}\nstay = true\nsubscribe = [{{ signal = \"ci.failed\" }}]\n",
            if team { "forge" } else { "codex" },
            env.project_root.to_str().unwrap(),
            checkout.to_str().unwrap(),
            if each_worktree {
                "each-worktree = true\nwhen = [\"pr=open\"]"
            } else {
                "every = \"1h\""
            },
        ),
    );
    if each_worktree {
        let runtime = env.runtime_paths();
        std::fs::create_dir_all(runtime.lane_path("pr-state.json").parent().unwrap()).unwrap();
        std::fs::write(
            runtime.lane_path("pr-state.json"),
            json!({"states": {checkout.to_string_lossy().to_string(): {"state": "open"}}})
                .to_string(),
        )
        .unwrap();
        let source = rimz::harness::when::CiSource::read(&runtime);
        let expr = rimz::harness::when::WhenExpr::parse(&["pr=open".to_owned()]).unwrap();
        let windows = rimz::harness::when::WindowReadings::new(Some(&runtime), Timestamp::now());
        assert!(
            rimz::harness::when::evaluate(&expr, &checkout, Some(&source), None, None, &windows).ok
        );
    }
    let store = env.store();
    let leaders = |store: &rimz::Store| {
        store
            .runtime_projection(rimz::RuntimeScope::Audit)
            .unwrap()
            .agents
            .iter()
            .filter(|agent| agent.loop_task.as_deref() == Some("resident"))
            .cloned()
            .collect::<Vec<_>>()
    };
    let wait_for_leaders = |count: usize| {
        let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
        loop {
            let leaders = leaders(&store);
            if leaders.len() == count {
                return leaders;
            }
            if Instant::now() >= deadline {
                let rows = read_loop_run_records(&env);
                panic!(
                    "resident never launched: {rows:?}\n{}\n{leaders:?}",
                    loop_run_journal(&env, &env.project_root, &rows[0])
                );
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    for fire in 0..2 {
        let mut run = command();
        run.args(["loop", "run", "resident"]);
        if each_worktree {
            run.arg("--cwd").arg(&checkout).arg("--condition-json").arg(
                json!({
                    "when": "pr=open", "hold": null, "held_ms": 0,
                    "readings": {"pr": "open"}
                })
                .to_string(),
            );
        }
        let output = run.output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        assert!(
            !stderr.contains("did not materialize"),
            "the birth must pass the sidebar layout wait: {stderr}"
        );
        if fire == 0 {
            wait_for_leaders(1);
        }
    }
    let agents = wait_for_leaders(1);
    let expected_checkout = if each_worktree {
        checkout.clone()
    } else {
        canonical(&checkout)
    };
    // A run's scope is its checkout's physical path, so its leader launches there.
    assert_eq!(
        agents[0].worktree_path.as_deref(),
        canonical(&checkout).to_str()
    );
    assert_eq!(agents[0].channel.as_deref(), Some("lane"));
    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].result, LoopRunResult::Launched);
    let run = rimz::store::workflow::load(
        &store
            .paths()
            .workflow(&records[0].run_id.as_deref().unwrap().parse().unwrap()),
    )
    .unwrap();
    assert!(run.notes.is_some(), "resident fires keep memory: {run:?}");
    // The launch records the supervised run after the leader's row, so wait for it.
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    let runs = loop {
        let runs = rimz::harness::run::list(store.paths()).unwrap();
        if runs
            .iter()
            .any(|run| run.loop_task.as_deref() == Some("resident"))
        {
            break runs;
        }
        assert!(
            Instant::now() < deadline,
            "the resident's supervised run never appeared: {runs:?}\n{:?}",
            read_loop_run_records(&env)
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0].loop_task.as_deref(), Some("resident"));
    // The run remembers the checkout; no fire writes the resident launch file.
    assert!(
        !store
            .paths()
            .root
            .join("records/loop-launches.json")
            .exists()
    );
    let workflows = rimz::harness::schedule::launch_ledger::load_workflows(store.paths()).unwrap();
    assert!(
        workflows.tasks["resident"].contains_key(&expected_checkout),
        "{workflows:?}"
    );
    if team {
        assert_eq!(agents[0].team.as_deref(), Some("forge"));
        // The team's tab opens after its run is recorded, so wait for its layout.
        let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
        let (cwd, trace) = loop {
            let text = std::fs::read_to_string(&trace).unwrap_or_default();
            let cwd = text
                .split_inclusive('\n')
                .filter_map(|line| line.strip_prefix("layout-json\t")?.strip_suffix('\n'))
                .find_map(|layout| {
                    let layout: String = serde_json::from_str(layout).unwrap();
                    let layout: kdl::KdlDocument = layout.parse().unwrap();
                    let (pane, _) = loop_agent_pane(&layout)?;
                    Some(pane.get("cwd")?.value().as_string()?.to_owned())
                });
            if let Some(cwd) = cwd {
                break (cwd, text);
            }
            assert!(
                Instant::now() < deadline,
                "the team's layout never reached the mux: {text}"
            );
            std::thread::sleep(Duration::from_millis(25));
        };
        assert_eq!(cwd, canonical(&checkout).to_str().unwrap(), "{trace}");
        return;
    }
    let assists = rimz::harness::assist_log::recent(&env.rimz_home().join("logs"), None)
        .iter()
        .map(|record| serde_json::to_value(record).unwrap())
        .collect::<Vec<_>>();
    let starts = assists
        .iter()
        .filter(|assist| assist["assist"] == "workflow_start")
        .collect::<Vec<_>>();
    assert_eq!(starts.len(), 1, "{assists:?}");
    assert_eq!(starts[0]["task"], "resident");
    let owner = dummy_agent_process();
    let owner_pid = owner.id();
    reap_later(owner);
    let transcript = env.home_root.join("resident-transcript.jsonl");
    std::fs::write(&transcript, "{}\n").unwrap();
    let hook = |event: &str| {
        let mut command = env.hook_command("codex");
        command
            .env("RIMZ_AGENT_PID", owner_pid.to_string())
            .env("ZELLIJ", "1")
            .env("ZELLIJ_SESSION_NAME", &workspace.session_name)
            .env("ZELLIJ_PANE_ID", "51")
            .env("RIMZ_RUN_ID", runs[0].run_id.to_string());
        command.current_dir(&checkout).env(
            rimz::harness::launch::ENV_AGENT_NAME,
            agents[0].name.as_deref().unwrap(),
        );
        let output = env
            .spawn_payload(
                command,
                &json!({
                    "hook_event_name": event, "session_id": "resident-session",
                    "cwd": expected_checkout, "prompt": "work",
                    "transcript_path": transcript
                })
                .to_string(),
            )
            .wait_with_output()
            .unwrap();
        env.drain_hooks();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    hook("SessionStart");
    hook("UserPromptSubmit");
    // The run's step subscription reaches the leader; loop arms no row of its own. Its
    // default `path` is the run's scope, the checkout's physical path.
    assert!(read_loop_instances(&env).0.is_empty());
    for (path, matching) in [(&env.project_root, false), (&canonical(&checkout), true)] {
        loop_ok(
            &env,
            &[
                "events",
                "emit",
                "ci.failed",
                "--source",
                "forge",
                "--json",
                &json!({"path":path}).to_string(),
            ],
        );
        if !matching {
            assert!(store.list_pending_messages().unwrap().is_empty());
        }
    }
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    while store.list_pending_messages().unwrap().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    let messages = store.list_pending_messages().unwrap();
    assert_eq!(messages.len(), 1, "{:?}", read_loop_run_records(&env));
    assert_eq!(messages[0].kind.as_str(), "codex");
    assert_eq!(messages[0].agent_id.as_str(), "resident-session");
    assert!(
        messages[0].text.contains("ci.failed"),
        "{}",
        messages[0].text
    );
    // Once its run ends, a manual fire bypasses the memory and relaunches.
    let stopped = command()
        .args(["loop", "stop", "resident"])
        .output()
        .unwrap();
    assert!(stopped.status.success(), "{stopped:?}");
    wait_for_loop_report(&env, "resident");
    let output = command()
        .args(["loop", "fire", "resident"])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    wait_for_leaders(2);
    if each_worktree {
        let refused = command()
            .args(["loop", "fire", "resident"])
            .output()
            .unwrap();
        assert!(!refused.status.success());
        assert!(String::from_utf8_lossy(&refused.stderr).contains("owned worktree"));
    }
    loop_ok(&env, &["loop", "remove", "resident"]);
    assert!(
        !rimz::harness::schedule::launch_ledger::load_workflows(store.paths())
            .unwrap()
            .tasks
            .contains_key("resident")
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Takeover {
    Restricted,
    IdleOccupants,
    WorkingOccupant,
    ClosedPane,
    OmittedTab,
    SymlinkedCheckout,
    FailedStop,
    OpenRunPane,
    EmptyCheckout,
    TeamLayout,
}

#[test]
fn resident_saved_untrusted_fails_its_run_before_takeover() {
    resident_takeover_case(Takeover::Restricted);
}

#[test]
fn resident_takeover_stops_idle_occupants_of_every_origin_and_no_other_checkout() {
    resident_takeover_case(Takeover::IdleOccupants);
}

#[test]
fn resident_takeover_waits_for_a_working_occupant_then_stops_it_and_launches() {
    resident_takeover_case(Takeover::WorkingOccupant);
}

#[test]
fn resident_takeover_ignores_a_working_row_in_a_closed_pane_and_an_unrecorded_live_pane() {
    resident_takeover_case(Takeover::ClosedPane);
}

#[test]
fn resident_takeover_waits_for_a_working_occupant_in_an_omitted_live_tab() {
    resident_takeover_case(Takeover::OmittedTab);
}

#[cfg(unix)]
#[test]
fn resident_takeover_of_a_symlinked_checkout_finds_the_occupant_at_its_physical_path() {
    resident_takeover_case(Takeover::SymlinkedCheckout);
}

#[test]
fn resident_takeover_failed_stop_commits_no_launch_or_ledger() {
    resident_takeover_case(Takeover::FailedStop);
}

/// A kept run's pane that is still listed after a failed close is an occupant
/// still in the checkout.
#[test]
fn resident_takeover_of_a_run_whose_pane_stays_open_commits_no_launch_or_ledger() {
    resident_takeover_case(Takeover::OpenRunPane);
}

#[test]
fn resident_takeover_of_an_empty_checkout_launches_and_stops_nothing() {
    resident_takeover_case(Takeover::EmptyCheckout);
}

#[test]
fn resident_takeover_into_a_team_layout_relaunches_the_team_holding_the_checkout() {
    resident_takeover_case(Takeover::TeamLayout);
}

fn resident_takeover_case(case: Takeover) {
    let env = Env::new();
    let Some(cwd) = team_signal_fixture(&env) else {
        return;
    };
    env.record(&env.project_root);
    // The launch checkout as the task names it, and as a provider hook reports
    // the occupant's cwd: the same path unless a symlink leads to the checkout.
    let symlinked = case == Takeover::SymlinkedCheckout;
    let (launch_cwd, cwd) = if symlinked {
        let launch_cwd = symlinked_owned_worktree(&env);
        let physical = canonical(&launch_cwd);
        assert_ne!(launch_cwd, physical);
        (launch_cwd, physical)
    } else {
        (cwd.clone(), cwd)
    };
    let working = symlinked || case == Takeover::WorkingOccupant;
    let working = working || matches!(case, Takeover::ClosedPane | Takeover::OmittedTab);
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(&env);
    trust_codex_project(&env, &launch_cwd);
    if case == Takeover::Restricted {
        let config = env.agent_config_path("codex");
        let mut table: toml::Table = std::fs::read_to_string(&config).unwrap().parse().unwrap();
        table["projects"][launch_cwd.to_str().unwrap()]["trust_level"] = "untrusted".into();
        std::fs::write(config, toml::to_string(&table).unwrap()).unwrap();
    }
    let sibling = env.home_root.join("sibling");
    assert!(git_ok(
        &env.project_root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "sibling",
            sibling.to_str().unwrap()
        ]
    ));
    let docs = cwd.join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    let workspace = env.resolve_workspace(&env.project_root);
    let store = env.store();
    let team = |channel: &str| rimz::agents::LaunchParams {
        team: Some("forge".to_owned()),
        role: Some("coder".to_owned()),
        channel: Some(channel.to_owned()),
        ..Default::default()
    };
    // The fire runs under the test process, so a launched child owned by it
    // would make the fire itself a subagent.
    let child_owner = dummy_agent_process();
    let child_pid = child_owner.id();
    reap_later(child_owner);
    let closed_owner = (case == Takeover::ClosedPane).then(dummy_agent_process);
    let closed_pid = closed_owner.as_ref().map(std::process::Child::id);
    let owner_pid = |session: &str| {
        if session.ends_with("-child") {
            child_pid
        } else if session == "old-coder" {
            closed_pid.unwrap_or_else(std::process::id)
        } else {
            std::process::id()
        }
    };
    let hook = |session: &str, checkout: &Path, pane: Option<&str>, event: &str| {
        let mut hook = env.hook_command("claude");
        hook.current_dir(checkout)
            .env(rimz::harness::launch::ENV_AGENT_NAME, session)
            .env("RIMZ_AGENT_PID", owner_pid(session).to_string());
        if let Some(pane) = pane {
            hook.env("ZELLIJ", "1")
                .env("ZELLIJ_SESSION_NAME", &workspace.session_name)
                .env("ZELLIJ_PANE_ID", pane);
        }
        let output = env
            .spawn_payload(
                hook,
                &json!({"hook_event_name":event, "session_id":session, "cwd":checkout, "prompt":"work"})
                    .to_string(),
            )
            .wait_with_output()
            .unwrap();
        env.drain_hooks();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    let child_of_old_coder = || rimz::agents::LaunchParams {
        parent_agent_id: Some(AgentSessionId::from("launch_old-coder")),
        parent_agent_kind: Some(AgentKind::new_unchecked("claude")),
        launch_depth: Some(1),
        ..Default::default()
    };
    let mut occupants = vec![("sibling-coder", &sibling, Some("52"), team("sibling-coder"))];
    if case != Takeover::EmptyCheckout {
        occupants.push(("old-coder", &cwd, Some("51"), team("old-coder")));
    }
    if case == Takeover::IdleOccupants {
        occupants.push((
            "sweeper",
            &cwd,
            Some("53"),
            rimz::agents::LaunchParams {
                loop_task: Some("sweep".to_owned()),
                ..Default::default()
            },
        ));
        occupants.push(("solo", &docs, Some("54"), Default::default()));
        occupants.push(("old-child", &cwd, Some("55"), child_of_old_coder()));
    }
    if case == Takeover::FailedStop {
        occupants.push(("paneless-child", &cwd, None, child_of_old_coder()));
    }
    let seeded = occupants.len();
    for (session, checkout, pane, launch) in occupants {
        seed_agent_launch(&env, checkout, session, launch, None);
        hook(session, checkout, pane, "SessionStart");
    }
    if case == Takeover::ClosedPane {
        // Make the final live-owner hook publish even inside the cadence window.
        std::fs::File::open(store.paths().snapshots_dir().unwrap().join("publish.stamp"))
            .unwrap()
            .set_modified(std::time::SystemTime::UNIX_EPOCH)
            .unwrap();
    }
    if working {
        hook("old-coder", &cwd, Some("51"), "UserPromptSubmit");
    }
    let team_members = if case == Takeover::EmptyCheckout {
        1
    } else {
        2
    };
    assert_eq!(read_loop_instances(&env).0.len(), team_members);
    let open_run_pane = case == Takeover::OpenRunPane;
    if open_run_pane {
        let mut run = rimz::store::run::RunRecord::new(
            env.workspace_id.clone(),
            AgentKind::new_unchecked("claude"),
            rimz::agents::PermissionMode::Auto,
            "task".to_owned(),
            cwd.clone(),
        );
        run.agent_name = Some("old-coder".to_owned());
        run.pane_id = Some(rimz::ids::PaneId::from_parts(
            rimz::ids::MuxName::Zellij,
            "terminal_51",
        ));
        run.status = rimz::store::run::RunStatus::Completed;
        rimz::harness::run::create(store.paths(), &run).unwrap();
    }
    let runtime = env.runtime_paths();
    crate::common::room::seed_live_zellij_room(&runtime, &workspace.session_name, Vec::new());
    if symlinked {
        std::fs::write(
            runtime.lane_path("pr-state.json"),
            json!({
                "branch_ci": {},
                "path_repos": {launch_cwd.to_string_lossy().to_string(): "repo"},
                "repos": {"repo": {"ok": true, "refreshed_at_ms": fresh_probe_ms()}},
                "states": {launch_cwd.to_string_lossy().to_string(): {"state": "open"}},
            })
            .to_string(),
        )
        .unwrap();
    }
    let agent_bin = crate::common::write_failing_agent_shim(&env, "codex", 1);
    let shell = write_fake_login_shell(&env, "rimz-test-sh", &[]);
    let trace = env.home_root.join("takeover.log");
    let command = || {
        let mut command = env.rimz();
        command
            .args(["--mux", "zellij"])
            .env("SHELL", &shell)
            .env("PATH", path_with_front(&agent_bin))
            .env(
                "RIMZ_ZELLIJ_BIN",
                crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace")),
            )
            .env("RIMZ_TEST_ZELLIJ_LOG", &trace)
            .env("RIMZ_TEST_ZELLIJ_LIST_PANES", "[]")
            .env(
                "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
                format!("{} [Created 1s ago]\n", workspace.session_name),
            );
        if open_run_pane {
            command.env("RIMZ_TEST_ZELLIJ_FAIL_CLOSE_PANE", "1").env(
                "RIMZ_TEST_ZELLIJ_LIST_PANES",
                r#"[{"id":51,"is_plugin":false,"tab_id":1,"title":"sh"}]"#,
            );
        }
        if matches!(case, Takeover::ClosedPane | Takeover::OmittedTab) {
            command.env(
                "RIMZ_TEST_ZELLIJ_LIST_PANES",
                json!([
                    {"id":52,"is_plugin":false,"tab_id":1,"title":"sibling"},
                    {"id":99,"is_plugin":false,"tab_id":1,"title":"sh","pane_cwd":cwd}
                ])
                .to_string(),
            );
        }
        command
    };
    write_loop_config(
        &env,
        &format!(
            "[tasks.resident]\nagent = {:?}\nprompt = \"repair\"\nroot = {:?}\ndir = {:?}\n{}\nstay = true\ntakeover = true\n",
            if case == Takeover::TeamLayout {
                "forge"
            } else {
                "codex"
            },
            env.project_root,
            launch_cwd,
            if symlinked {
                "each-worktree = true\nwhen = [\"pr=open\"]"
            } else {
                "every = \"1h\""
            },
        ),
    );
    let strikes = BTreeMap::from([(machine_task_key("resident"), 2_u32)]);
    std::fs::create_dir_all(loop_strikes_path(&env).parent().unwrap()).unwrap();
    std::fs::write(
        loop_strikes_path(&env),
        serde_json::to_vec(&strikes).unwrap(),
    )
    .unwrap();
    let fire = || {
        let mut fire = command();
        fire.args(["loop", "run", "resident"]);
        if symlinked {
            fire.arg("--cwd")
                .arg(&launch_cwd)
                .arg("--condition-json")
                .arg(
                    json!({
                        "when": "pr=open", "hold": null, "held_ms": 0,
                        "readings": {"pr": "open"}
                    })
                    .to_string(),
                );
        }
        fire.output().unwrap()
    };
    let trace_text = || std::fs::read_to_string(&trace).unwrap_or_default();
    let checkout = rimz::utils::path::normalize_path_lexical(&launch_cwd);
    // Each-worktree memory keys the marker path; a single checkout keys its physical path.
    let expected_checkout = if symlinked {
        checkout.clone()
    } else {
        canonical(&checkout)
    };
    let remembered = || {
        rimz::harness::schedule::launch_ledger::load_workflows(store.paths())
            .unwrap()
            .tasks
            .get("resident")
            .is_some_and(|checkouts| checkouts.contains_key(&expected_checkout))
    };
    let starts = || {
        rimz::harness::assist_log::recent(&env.rimz_home().join("logs"), None)
            .iter()
            .map(|record| serde_json::to_value(record).unwrap())
            .filter(|assist| assist["assist"] == "workflow_start")
            .count()
    };
    let leaders = || {
        store
            .runtime_projection(rimz::RuntimeScope::Audit)
            .unwrap()
            .agents
            .into_iter()
            .filter(|agent| agent.loop_task.as_deref() == Some("resident"))
            .collect::<Vec<_>>()
    };

    if let Some(mut owner) = closed_owner {
        owner.kill().unwrap();
        owner.wait().unwrap();
        let cached = rimz::store::snapshot::read_fresh_latest(store.paths())
            .expect("the final live-owner hook published the roster");
        let ghost = cached
            .agents
            .iter()
            .find(|agent| {
                agent
                    .runtime_owner
                    .as_ref()
                    .is_some_and(|recorded| recorded.pid == owner.id())
            })
            .unwrap();
        assert_eq!(
            rimz::store::runtime::agent_liveness(ghost),
            rimz::store::runtime::AgentLiveness::Dead
        );
    }
    let before_agents = serde_json::to_value(store.snapshot().unwrap().agents).unwrap();
    let output = fire();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let fired = last_loop_record(&env);
    assert_eq!(fired.result, LoopRunResult::Launched, "{fired:?}");
    let id: rimz::ids::WorkflowRunId = fired.run_id.as_deref().unwrap().parse().unwrap();
    let run_paths = store.paths().workflow(&id);
    let wait_for_run = |what: &str, done: &dyn Fn(&rimz::store::workflow::RunRecord) -> bool| {
        let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
        loop {
            if rimz::store::workflow::load(&run_paths).is_ok_and(|run| done(&run)) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "resident run never {what}: {}\n{:?}",
                loop_run_journal(&env, &env.project_root, &fired),
                leaders()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    let wait_for_leader = || {
        wait_for_run("launched", &|_| !leaders().is_empty());
        leaders()
    };

    if matches!(
        case,
        Takeover::Restricted | Takeover::FailedStop | Takeover::OpenRunPane
    ) {
        if case != Takeover::Restricted {
            // The eviction started nothing, so the step waits to retry it once, counting nothing.
            wait_for_run("waited to retry", &|run| {
                run.wait
                    .as_ref()
                    .is_some_and(|wait| wait.outcome == "retry")
                    && run.attempt.is_none()
                    && run.prev_attempt.is_none()
                    && run.visits.is_empty()
            });
            assert!(!trace_text().contains("new-tab"));
            assert!(leaders().is_empty());
            let advance = command()
                .args(["workflow", "advance", id.as_str()])
                .output()
                .unwrap();
            assert!(
                advance.status.success(),
                "{}",
                String::from_utf8_lossy(&advance.stderr)
            );
        }
        let report = wait_for_loop_report(&env, "resident");
        assert_eq!(report.result, LoopRunResult::Failed, "{report:?}");
        let error = report.error.as_deref().unwrap_or_default();
        let reasons: &[&str] = match case {
            Takeover::Restricted => &["saved untrusted", "Open restricted"],
            Takeover::OpenRunPane => &["terminal_51 is still open"],
            _ => &["no bound pane"],
        };
        for reason in reasons {
            assert!(error.contains(reason), "{error}");
        }
        let trace_text = trace_text();
        assert!(!trace_text.contains("new-tab"), "{trace_text}");
        assert!(leaders().is_empty());
        if case == Takeover::Restricted {
            // The refused launch is prepared before the takeover, so no occupant stops.
            assert!(!trace_text.contains("close-pane"), "{trace_text}");
            assert_eq!(
                serde_json::to_value(store.snapshot().unwrap().agents).unwrap(),
                before_agents
            );
        }
        return;
    }
    if working && case != Takeover::ClosedPane {
        // A working occupant holds the takeover: the run waits, and the task keeps its strikes.
        wait_for_run("waited on the occupant", &|run| {
            run.wait
                .as_ref()
                .is_some_and(|wait| wait.outcome == "takeover")
        });
        let trace_text = trace_text();
        assert!(!trace_text.contains("close-pane"), "{trace_text}");
        assert!(!trace_text.contains("new-tab"), "{trace_text}");
        assert!(leaders().is_empty());
        assert_eq!(
            serde_json::from_slice::<BTreeMap<String, u32>>(
                &std::fs::read(loop_strikes_path(&env)).unwrap()
            )
            .unwrap(),
            strikes
        );
        assert_eq!(read_loop_instances(&env).0.len(), 2);
        assert_eq!(
            store
                .runtime_projection(rimz::RuntimeScope::Audit)
                .unwrap()
                .agents
                .len(),
            seeded
        );
        if case == Takeover::OmittedTab {
            return;
        }
        hook("old-coder", &cwd, Some("51"), "Stop");
        let advance = command()
            .args(["workflow", "advance", id.as_str()])
            .output()
            .unwrap();
        assert!(
            advance.status.success(),
            "{}",
            String::from_utf8_lossy(&advance.stderr)
        );
    }
    let launched = wait_for_leader();
    // The leader's card can land before the launch opens its tab.
    wait_for_run("opened its tab", &|_| trace_text().contains("new-tab"));
    assert!(remembered());
    assert_eq!(starts(), 1);
    let trace_text = trace_text();
    let stops = trace_text.matches("close-pane").count();
    if matches!(case, Takeover::EmptyCheckout | Takeover::ClosedPane) {
        assert_eq!(stops, 0, "{trace_text}");
        if case == Takeover::EmptyCheckout {
            assert_eq!(read_loop_instances(&env).0.len(), 1);
        }
        return;
    }
    let expected: &[&str] = if case == Takeover::IdleOccupants {
        &["old-coder", "sweeper", "solo", "old-child"]
    } else {
        &["old-coder"]
    };
    assert_eq!(stops, expected.len(), "{trace_text}");
    assert!(
        trace_text.rfind("close-pane").unwrap() < trace_text.find("new-tab").unwrap(),
        "{trace_text}"
    );
    if case == Takeover::TeamLayout {
        // The hold admits a relaunch of the team that holds the checkout.
        let agents = store
            .runtime_projection(rimz::RuntimeScope::Audit)
            .unwrap()
            .agents;
        let mut launched: Vec<(Option<&str>, Option<&str>)> = agents
            .iter()
            .filter(|agent| {
                !agent
                    .name
                    .as_deref()
                    .is_some_and(|name| name.ends_with("-coder"))
            })
            .map(|agent| {
                assert_eq!(agent.team.as_deref(), Some("forge"));
                assert_eq!(
                    agent.worktree_path.as_deref(),
                    canonical(&checkout).to_str()
                );
                (agent.role.as_deref(), agent.loop_task.as_deref())
            })
            .collect();
        launched.sort();
        assert_eq!(
            launched,
            [(Some("coder"), Some("resident")), (Some("reviewer"), None)]
        );
        return;
    }
    assert_eq!(launched.len(), 1);
    let subscriptions = read_loop_instances(&env);
    assert_eq!(subscriptions.0.len(), 1);
    assert_eq!(
        subscriptions
            .0
            .values()
            .next()
            .unwrap()
            .wait
            .as_ref()
            .unwrap()
            .session
            .as_str(),
        "sibling-coder"
    );
    stamp_session_ended(&env, "old-coder");
    let output = command().args(["teams", "stop", "forge"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(read_loop_instances(&env).0.is_empty());
}

/// An owned worktree whose marker path runs through a symlinked worktree
/// directory, so its lexical and physical forms differ.
#[cfg(unix)]
fn symlinked_owned_worktree(env: &Env) -> std::path::PathBuf {
    let actual = env.home_root.join("actual-worktrees");
    let alias = env.home_root.join("linked-worktrees");
    std::fs::create_dir(&actual).unwrap();
    std::os::unix::fs::symlink(&actual, &alias).unwrap();
    std::fs::write(
        env.rimz_home().join("config.toml"),
        format!("[agents.worktree]\ndir = {:?}\n", alias.to_str().unwrap()),
    )
    .unwrap();
    loop_ok(env, &["worktree", "new", "lane"]);
    alias.join("lane")
}

fn publish_claude_5h(env: &Env, used: u8) {
    publish_claude_5h_resetting(env, used, SignedDuration::from_hours(2));
}

fn publish_claude_5h_resetting(env: &Env, used: u8, resets_in: SignedDuration) {
    env.publish_rate_limits(&RateLimitsCache {
        entries: BTreeMap::from([(
            "claude@default".parse().unwrap(),
            RateLimitCacheEntry {
                limits: AgentRateLimits {
                    windows: vec![RateLimitWindow {
                        used_percentage: Some(used),
                        resets_at: Some(Timestamp::now() + resets_in),
                        duration_mins: Some(rimz::agents::WindowSpan::FiveHour.minutes()),
                        ..RateLimitWindow::default()
                    }],
                },
                ..Default::default()
            },
        )]),
        ..Default::default()
    });
}

#[test]
fn after_reset_fires_once_at_the_window_reset_and_removes_its_row() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-reset", "feature-loop");
    let add = |name: &str, extra: &[&str]| {
        calling_loop(&env, "sess-reset")
            .args(["loop", "add", name, "--after-reset", "5h"])
            .args(extra)
            .output()
            .unwrap()
    };
    for (extra, expected) in [
        (
            &["--check", "true"][..],
            "--after-reset requires --agent or --wait",
        ),
        (
            &["--project", "--agent", "claude", "--prompt", "go"][..],
            "--project tasks cannot use --after-reset",
        ),
        (&["--wait", "--in", "5m"][..], "cannot be used with"),
    ] {
        let refused = add("refused", extra);
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(
            !refused.status.success() && stderr.contains(expected),
            "{extra:?}: {stderr}"
        );
    }
    let refused = add("refused", &["--wait"]);
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("no current claude 5h window reading; open the room's sidebar"),
        "no cache: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
    publish_claude_5h_resetting(&env, 30, SignedDuration::from_mins(-1));
    let refused = add("refused", &["--wait"]);
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("no current claude 5h window reading"),
        "expired cache: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        read_loop_instances(&env).0.is_empty(),
        "a refusal writes nothing"
    );

    publish_claude_5h(&env, 30);
    let started = add("later", &["--wait"]);
    let receipt = String::from_utf8_lossy(&started.stdout);
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    for line in [
        "trigger: fires once after the next claude 5h reset\n",
        "window: claude 5h · 70% left · resets ",
        "next fire: ",
    ] {
        assert!(receipt.contains(line), "{line}: {receipt}");
    }
    assert!(read_loop_instances(&env).0["later"].fire_at.is_some());
    loop_ok(&env, &["loop", "tick"]);
    loop_ok(&env, &["loop", "tick"]);
    assert!(read_loop_run_records(&env).is_empty());
    assert!(read_loop_instances(&env).0.contains_key("later"));

    publish_claude_5h_resetting(&env, 0, SignedDuration::from_hours(5));
    let fresh = add("now", &["--wait"]);
    let receipt = String::from_utf8_lossy(&fresh.stdout);
    for line in [
        "trigger: fires once after the next claude 5h reset\n",
        "window: claude 5h · not started · 100% left\n",
        "next fire: the next scheduler tick, since the window has not started\n",
    ] {
        assert!(receipt.contains(line), "{line}: {receipt}");
    }
    loop_ok(&env, &["loop", "tick"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    while (read_loop_run_records(&env).is_empty()
        || read_loop_instances(&env).0.contains_key("now"))
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(last_loop_record(&env).task, "now");
    let rows = read_loop_instances(&env).0;
    assert!(!rows.contains_key("now"));
    assert!(rows.contains_key("later"));
}

#[test]
fn window_condition_waits_for_room_in_the_provider_window() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-window", "feature-loop");
    publish_claude_5h(&env, 92);
    let output = calling_loop(&env, "sess-window")
        .args([
            "loop",
            "add",
            "roomy",
            "--wait",
            "--when",
            "window.5h.left >= 40",
            "--once",
        ])
        .output()
        .unwrap();
    let receipt = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        receipt.contains("now: waiting · window.5h.left: 8"),
        "{receipt}"
    );
    let row = &read_loop_instances(&env).0["roomy"];
    assert_eq!(
        row.when.as_deref(),
        Some(&["window.5h.left>=40".to_owned()][..])
    );
    assert_eq!(row.provider, Some(AgentKind::new_unchecked("claude")));
    loop_ok(&env, &["loop", "tick"]);
    loop_ok(&env, &["loop", "tick"]);
    assert!(read_loop_run_records(&env).is_empty());
    publish_claude_5h(&env, 10);
    loop_ok(&env, &["loop", "tick"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    while (read_loop_run_records(&env).is_empty()
        || read_loop_instances(&env).0.contains_key("roomy"))
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(25));
    }
    let condition = last_loop_record(&env).condition.unwrap();
    assert_eq!(condition.when, "window.5h.left>=40");
    assert_eq!(condition.readings["window.5h.left"].as_deref(), Some("90"));
    assert!(!read_loop_instances(&env).0.contains_key("roomy"));
}

#[test]
fn condition_wait_pins_projects_and_deduplicates() {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    env.record(&env.project_root);
    let first = env.home_root.join("condition-first");
    let second = env.home_root.join("condition-second");
    for (branch, path) in [("first", &first), ("second", &second)] {
        assert!(git_ok(
            &env.project_root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                branch,
                path.to_str().unwrap()
            ]
        ));
    }
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-condition", "feature-loop");
    for (name, scope, expected) in [
        ("ready", &first, "added loop task `ready`"),
        ("duplicate", &first, "already subscribed as ready"),
        ("second", &second, "added loop task `second`"),
        ("second-duplicate", &second, "already subscribed as second"),
    ] {
        let output = calling_loop(&env, "sess-condition")
            .args([
                "loop",
                "add",
                name,
                "--wait",
                "--when",
                "team.stage=Done",
                "--for",
                "30m",
                "--once",
            ])
            .arg("--root")
            .arg(scope)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(expected),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    let instances = read_loop_instances(&env);
    assert_eq!(instances.0.len(), 2);
    assert_eq!(instances.0["ready"].run_dir(), canonical(&first));
    assert_eq!(instances.0["second"].run_dir(), canonical(&second));
    assert_eq!(
        instances.0["ready"].when.as_ref().unwrap(),
        &["team.stage=Done"]
    );
    assert_eq!(instances.0["ready"].hold.as_deref(), Some("30m"));
    let output = env
        .rimz()
        .args(["sidebar", "snapshot", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let snapshot: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        snapshot
            .to_string()
            .contains("\"when\":\"team.stage=Done\""),
        "{snapshot}"
    );
    loop_ok(&env, &["loop", "run", "ready"]);
    assert_eq!(
        env.store().list_pending_messages().unwrap()[0].sender,
        rimz::store::message::MessageSender::Harness {
            notice: rimz::store::message::HarnessNotice::Wait
        }
    );
}
#[test]
fn trunk_signal_fires_only_through_git_source() {
    let env = Env::new();
    env.record(&env.project_root);
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "trunk",
            "--signal",
            "trunk.moved",
            "--check",
            "true",
        ],
    );
    let refused = env
        .rimz()
        .args(["events", "emit", "trunk.moved"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("reserved"));
    crate::common::room::seed_sidebar_heartbeat(
        &env.runtime_paths(),
        MuxName::Zellij,
        &env.resolve_workspace(&env.project_root).session_name,
        "trunk",
    );
    loop_ok(&env, &["events", "emit", "trunk.moved", "--source", "git"]);
    wait_for_loop_run_records(&env, "trunk Completed", |records| {
        records
            .iter()
            .any(|r| r.task == "trunk" && r.result == LoopRunResult::Completed)
    });
    assert!(
        read_loop_run_records(&env)
            .iter()
            .any(|r| r.task == "trunk" && r.result == LoopRunResult::Completed)
    );
}

#[test]
fn worktree_created_fires_a_loop_subscriber() {
    let env = Env::new();
    env.record(&env.project_root);
    for args in [
        vec!["init", "-b", "main"],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
    ] {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&env.project_root)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "created",
            "--signal",
            "worktree.created",
            "--check",
            "true",
        ],
    );
    crate::common::room::seed_sidebar_heartbeat(
        &env.runtime_paths(),
        MuxName::Zellij,
        &env.resolve_workspace(&env.project_root).session_name,
        "created",
    );
    loop_ok(&env, &["worktree", "new", "signal-test"]);
    wait_for_loop_run_records(&env, "created Completed", |records| {
        records
            .iter()
            .any(|r| r.task == "created" && r.result == LoopRunResult::Completed)
    });
    assert!(
        read_loop_run_records(&env)
            .iter()
            .any(|r| r.task == "created" && r.result == LoopRunResult::Completed)
    );
}

#[test]
fn forge_behind_signal_fires_matching_task_and_skips_merged_sibling() {
    let env = Env::new();
    env.record(&env.project_root);
    for (name, signal) in [("behind", "pr.behind"), ("merged", "pr.merged")] {
        loop_ok(
            &env,
            &[
                "loop",
                "add",
                name,
                "--signal",
                signal,
                "--match",
                "branch=feature",
                "--check",
                "true",
            ],
        );
    }
    crate::common::room::seed_sidebar_heartbeat(
        &env.runtime_paths(),
        MuxName::Zellij,
        &env.resolve_workspace(&env.project_root).session_name,
        "behind",
    );
    for branch in ["feature", "other"] {
        loop_ok(
            &env,
            &[
                "events",
                "emit",
                "pr.behind",
                "--source",
                "forge",
                "--json",
                &json!({"branch": branch}).to_string(),
            ],
        );
    }
    wait_for_loop_run_records(
        &env,
        "behind Completed and merged SignalSkipped",
        |records| {
            records.len() == 3
                && records.iter().any(|record| {
                    record.task == "behind" && record.result == LoopRunResult::Completed
                })
                && records.iter().any(|record| {
                    record.task == "merged" && record.result == LoopRunResult::SignalSkipped
                })
        },
    );
    let records = read_loop_run_records(&env);
    // The behind fire's launch row, its run's report, and the merged sibling's skip.
    assert_eq!(records.len(), 3, "{records:?}");
    assert!(
        records
            .iter()
            .any(|record| record.task == "behind" && record.result == LoopRunResult::Completed),
        "{records:?}"
    );
    assert!(
        records
            .iter()
            .any(|record| record.task == "merged" && record.result == LoopRunResult::SignalSkipped),
        "{records:?}"
    );
}

#[test]
fn wildcard_team_binding_arms_at_root_and_delivers_across_worktrees() {
    let env = Env::new();
    env.record(&env.project_root);
    if !init_git_repo(&env.project_root) {
        crate::common::skip("git unavailable");
        return;
    }
    let cwd = env.home_root.join("feature-team");
    assert!(git_ok(
        &env.project_root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature-team",
            cwd.to_str().unwrap()
        ]
    ));
    env.install_agent_hooks("claude");
    env.write_config(&env.project_root, r#"
        [profiles.claude]
        agent = "claude"
        [[agents.teams.forge.roles]]
        role = "coder"
        profile = "claude"
        signals = [{ signal = "pr.conflicted", match = { branch = "*" } }, { signal = "team.stage", match = { team = "*", to = "Done" } }]
    "#);
    loop_ok(&env, &["trust", "grant"]);
    let launch = env.rimz().args(["teams", "forge"]).output().unwrap();
    let error = String::from_utf8_lossy(&launch.stderr);
    assert!(!error.contains("root checkout"), "{error}");
    assert!(
        !error.contains("no team, profile, or command named"),
        "{error}"
    );
    seed_team_signal_member(&env, &env.project_root, "sess-any", None);
    team_signal_hook(&env, &env.project_root, "sess-any", "SessionStart");
    let armed = read_loop_instances(&env);
    assert_eq!(armed.0.len(), 2, "launch stderr: {error}");
    for (name, matches) in [
        (
            "team-forge-feature-team-coder-pr-conflicted",
            BTreeMap::from([("branch".to_owned(), "*".to_owned())]),
        ),
        (
            "team-forge-feature-team-coder-team-stage",
            BTreeMap::from([
                ("team".to_owned(), "*".to_owned()),
                ("to".to_owned(), "Done".to_owned()),
            ]),
        ),
    ] {
        let entry = &armed.0[name];
        assert_eq!(
            entry.team.as_ref().unwrap().to_string(),
            "forge#feature-team"
        );
        assert_eq!(entry.matches.as_ref(), Some(&matches));
    }
    let listing = loop_ok(&env, &["loop", "list", "--all"]);
    assert_eq!(
        listing.matches("team forge#feature-team ·").count(),
        1,
        "{listing}"
    );
    assert!(
        listing.contains("pr.conflicted") && listing.contains("team.stage"),
        "{listing}"
    );
    team_signal_hook(&env, &env.project_root, "sess-any", "UserPromptSubmit");
    for payload in [
        json!({"path":cwd,"repo":"o/r","number":7}),
        json!({"branch":"feature-team","path":cwd,"repo":"o/r","number":7}),
    ] {
        let output = env
            .rimz()
            .current_dir(&cwd)
            .args([
                "events",
                "emit",
                "pr.conflicted",
                "--source",
                "forge",
                "--json",
                &payload.to_string(),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if payload.get("branch").is_none() {
            assert!(read_loop_run_records(&env).is_empty());
            assert!(env.store().list_pending_messages().unwrap().is_empty());
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while read_loop_run_records(&env).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(read_loop_run_records(&env).iter().any(|record| record.task
        == "team-forge-feature-team-coder-pr-conflicted"
        && record.result == LoopRunResult::Delivered));
    assert_pending_message(&env, "sess-any", "feature-team");
}

#[test]
fn team_signal_binding_registers_delivers_and_retires() {
    let env = Env::new();
    env.record(&env.project_root);
    let Some(cwd) = team_signal_fixture(&env) else {
        return;
    };
    seed_team_signal_member(&env, &cwd, "sess-team-coder", None);
    team_signal_hook(&env, &cwd, "sess-team-coder", "SessionStart");
    let audit = env
        .store()
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap();
    let member = audit
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "sess-team-coder")
        .unwrap();
    assert_eq!(
        member.launch_id.as_ref().unwrap().as_str(),
        "launch_sess-team-coder"
    );
    let armed = read_loop_instances(&env);
    assert_eq!(armed.0.len(), 1);
    let entry = &armed.0["team-forge-feature-team-coder-ci-failed"];
    assert_eq!(
        entry.team.as_ref().unwrap().to_string(),
        "forge#feature-team"
    );
    assert_eq!(entry.signal.as_deref(), Some("ci.failed"));
    assert!(entry.once.is_none() && entry.deadline.is_none());
    assert_eq!(
        entry.matches.as_ref().unwrap()["path"],
        cwd.display().to_string()
    );
    assert_eq!(
        entry.wait.as_ref().unwrap().session.as_str(),
        "sess-team-coder"
    );
    team_signal_hook(&env, &cwd, "sess-team-coder", "SessionStart");
    assert_eq!(read_loop_instances(&env), armed);
    team_signal_hook(&env, &cwd, "sess-team-coder", "UserPromptSubmit");
    loop_ok(&env, &["events", "emit", "deploy.done"]);
    assert!(env.store().list_pending_messages().unwrap().is_empty());
    assert!(read_loop_run_records(&env).is_empty());
    for (path, matching) in [(&env.project_root, false), (&cwd, true)] {
        loop_ok(
            &env,
            &[
                "events",
                "emit",
                "ci.failed",
                "--source",
                "forge",
                "--json",
                &json!({"path": path}).to_string(),
            ],
        );
        if !matching {
            assert!(env.store().list_pending_messages().unwrap().is_empty());
            assert!(read_loop_run_records(&env).is_empty());
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while read_loop_run_records(&env).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_pending_message(&env, "sess-team-coder", "ci.failed");
    assert_eq!(
        env.store().list_pending_messages().unwrap()[0].sender,
        rimz::store::message::MessageSender::Harness {
            notice: rimz::store::message::HarnessNotice::Signal,
        }
    );
    assert_eq!(last_loop_record(&env).result, LoopRunResult::Delivered);
    assert_eq!(read_loop_instances(&env), armed);
    team_signal_hook(&env, &cwd, "sess-team-coder", "SessionEnd");
    assert!(read_loop_instances(&env).0.is_empty());
}

#[test]
fn team_signal_registration_preserves_same_named_machine_configuration() {
    let env = Env::new();
    env.record(&env.project_root);
    let Some(cwd) = team_signal_fixture(&env) else {
        return;
    };
    let config = format!(
        "[tasks.team-forge-feature-team-coder-ci-failed]\ncheck = \"true\"\nevery = \"15m\"\nroot = {:?}\n",
        env.project_root.display().to_string()
    );
    write_loop_config(&env, &config);
    seed_team_signal_member(&env, &cwd, "configured-session", None);
    let mut command = env.hook_command("claude");
    command
        .current_dir(&cwd)
        .env(rimz::harness::launch::ENV_AGENT_NAME, "configured-session")
        .env(rimz::workspace::ENV_CHANNEL, "feature-team");
    let output = env.spawn_payload(command, &json!({ "hook_event_name": "SessionStart", "session_id": "configured-session", "cwd": cwd }).to_string()).wait_with_output().unwrap();
    env.drain_hooks();
    assert!(output.status.success());
    assert_eq!(
        std::fs::read_to_string(loop_config_path(&env)).unwrap(),
        config
    );
    assert!(read_loop_instances(&env).0.is_empty());
    assert!(
        std::fs::read_to_string(env.store().paths().hook_drainer_log())
            .unwrap()
            .contains("configuration-owned"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn team_signal_slug_collisions_preserve_distinct_subscriptions() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    write_team_signal_config(&env);
    crate::common::write_definition(
        &env,
        "teams",
        "forge",
        r#"leader: coder
stages: [Build]
roles:
  - role: coder
    agent: worker
    owns: [Build]
    signals:
      - deploy.foo-bar
      - deploy.foo_bar
      - {signal: deploy.done, match: {target: first}}
      - {signal: deploy.done, match: {target: second}}
      - deploy.done-2
      - deploy.done-2-2"#,
        "Complete the work.",
    );
    seed_team_signal_member(&env, &env.project_root, "slug-session", None);
    team_signal_hook(&env, &env.project_root, "slug-session", "SessionStart");
    let tasks = read_loop_instances(&env);
    assert_eq!(tasks.0.len(), 6);
    assert_eq!(
        tasks
            .0
            .values()
            .map(|entry| (
                entry.signal.as_deref().unwrap(),
                entry
                    .matches
                    .as_ref()
                    .and_then(|matches| matches.get("target"))
                    .map(String::as_str),
            ))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            ("deploy.foo-bar", None),
            ("deploy.foo_bar", None),
            ("deploy.done", Some("first")),
            ("deploy.done", Some("second")),
            ("deploy.done-2", None),
            ("deploy.done-2-2", None),
        ])
    );
    team_signal_hook(&env, &env.project_root, "slug-session", "SessionStart");
    assert_eq!(read_loop_instances(&env), tasks);
}

#[test]
fn team_signal_bindings_ignore_child_registration() {
    let env = Env::new();
    env.record(&env.project_root);
    let Some(cwd) = team_signal_fixture(&env) else {
        return;
    };
    seed_team_signal_member(&env, &cwd, "sess-team-child", Some("sess-parent"));
    team_signal_hook(&env, &cwd, "sess-team-child", "SessionStart");
    let audit = env
        .store()
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap();
    let child = audit
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "sess-team-child")
        .unwrap();
    assert!(child.parent_agent_id.is_some());
    assert!(read_loop_instances(&env).0.is_empty());
}

#[test]
fn team_signal_binding_resume_keeps_session_rows_separate() {
    let env = Env::new();
    env.record(&env.project_root);
    let Some(cwd) = team_signal_fixture(&env) else {
        return;
    };
    seed_team_signal_member(&env, &cwd, "sess-team-old", None);
    team_signal_hook(&env, &cwd, "sess-team-old", "SessionStart");
    seed_team_signal_member(&env, &cwd, "sess-team-new", None);
    team_signal_hook(&env, &cwd, "sess-team-new", "SessionStart");
    team_signal_hook(&env, &cwd, "sess-team-old", "SessionEnd");
    let armed = read_loop_instances(&env);
    assert_eq!(armed.0.len(), 1);
    assert_eq!(
        armed
            .0
            .values()
            .next()
            .unwrap()
            .wait
            .as_ref()
            .unwrap()
            .session
            .as_str(),
        "sess-team-new"
    );
    team_signal_hook(&env, &cwd, "sess-team-new", "UserPromptSubmit");
    loop_ok(
        &env,
        &[
            "events",
            "emit",
            "ci.failed",
            "--source",
            "forge",
            "--json",
            &json!({"path": cwd}).to_string(),
        ],
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while read_loop_run_records(&env).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_pending_message(&env, "sess-team-new", "ci.failed");
}

#[test]
fn team_idle_and_root_end_hooks_deliver_only_to_the_matching_instance() {
    for (signal, hook) in [("team.idle", "Stop"), ("team.ended", "SessionEnd")] {
        let env = Env::new();
        env.record(&env.project_root);
        let Some(cwd) = team_signal_fixture(&env) else {
            return;
        };
        register_running_agent(&env, "sess-team-observer", "main");
        seed_team_signal_member(&env, &cwd, "sess-team-member", None);
        team_signal_hook(&env, &cwd, "sess-team-member", "SessionStart");
        team_signal_hook(&env, &cwd, "sess-team-member", "UserPromptSubmit");
        for (name, instance) in [
            ("matching", "forge#feature-team"),
            ("sibling", "forge#sibling"),
        ] {
            loop_ok(
                &env,
                &[
                    "loop",
                    "add",
                    name,
                    "--wait",
                    "@claude#project",
                    "--signal",
                    signal,
                    "--match",
                    &format!("instance={instance}"),
                    "--once",
                ],
            );
        }
        team_signal_hook(&env, &cwd, "sess-team-member", hook);
        let deadline = Instant::now() + Duration::from_secs(5);
        while read_loop_run_records(&env).is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        let records = read_loop_run_records(&env);
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].result, LoopRunResult::Delivered);
        let evidence = records[0].signal.as_ref().unwrap();
        assert_eq!(evidence.name.as_str(), signal);
        assert_eq!(evidence.payload["instance"], "forge#feature-team");
        assert_pending_message(&env, "sess-team-observer", signal);
        let instances = read_loop_instances(&env);
        assert!(!instances.0.contains_key("matching"));
        assert!(instances.0.contains_key("sibling"));
    }
}

#[test]
fn team_signal_payload_handles_round_trip_beside_teamless_role_rivals() {
    let env = Env::new();
    env.record(&env.project_root);
    let Some(cwd) = team_signal_fixture(&env) else {
        return;
    };
    register_running_agent(&env, "sess-team-observer", "main");
    seed_team_signal_member(&env, &cwd, "sess-team-member", None);
    team_signal_hook(&env, &cwd, "sess-team-member", "SessionStart");
    team_signal_hook(&env, &cwd, "sess-team-member", "UserPromptSubmit");
    seed_agent_launch(
        &env,
        &env.project_root,
        "inline-coder",
        rimz::agents::LaunchParams {
            role: Some("coder".to_owned()),
            channel: Some("feature-team".to_owned()),
            ..Default::default()
        },
        Some(rimz::pane::RuntimeOwner::new(
            rimz::pane::RuntimeOwnerKind::Agent,
            "inline-coder",
            env.agent_owner_pid(),
            None,
        )),
    );
    let workspace = env.resolve_workspace(&env.project_root);
    let mut rival = rimz::agents::AgentLifecycleObservation::new(
        Some("sess-inline-coder".into()),
        rimz::agents::LifecycleSignal::Registered,
    );
    rival.agent_name = Some("inline-coder".to_owned());
    env.store()
        .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
            session_name: &workspace.session_name,
            agent_kind: AgentKind::new_unchecked("claude"),
            event_name: "test",
            observation: &rival,
            spawned_subagents: &[],
        })
        .unwrap();
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "matching",
            "--wait",
            "@claude#project",
            "--signal",
            "team.idle",
            "--match",
            "instance=forge#feature-team",
            "--once",
        ],
    );
    team_signal_hook(&env, &cwd, "sess-team-member", "Stop");
    let deadline = Instant::now() + Duration::from_secs(5);
    while read_loop_run_records(&env).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].result, LoopRunResult::Delivered);
    let signal = records[0].signal.as_ref().unwrap();
    assert_eq!(signal.name.as_str(), "team.idle");
    assert_eq!(signal.payload["members"].as_array().unwrap().len(), 1);
    let snapshot = env.store().snapshot_cached().unwrap();
    assert!(
        rimz::address::addressable_agents(&snapshot)
            .iter()
            .any(|agent| agent.agent_id.as_str() == "sess-inline-coder"),
        "{:?}",
        snapshot.agents
    );
    for handle in [
        &signal.payload["member"],
        &signal.payload["members"][0]["handle"],
    ] {
        let handle = handle.as_str().unwrap();
        let resolved = rimz::address::resolve_one(
            &snapshot,
            handle,
            None,
            &rimz::address::AddressContext {
                channel: Some("feature-team".to_owned()),
                origin: rimz::address::ChannelOrigin::Explicit,
            },
        );
        assert_eq!(
            resolved.map(|agent| (agent.kind.as_str(), agent.agent_id.as_str())),
            Ok(("claude", "sess-team-member")),
            "{handle}"
        );
    }
}

#[test]
fn team_signal_launch_refuses_root_before_side_effects() {
    let env = Env::new();
    env.record(&env.project_root);
    write_team_signal_config(&env);
    let before = env
        .store()
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap();
    let (_, error) = loop_fail(&env, &["teams", "forge"]);
    assert!(
        error.contains("team `forge` role `coder` signal binding 1"),
        "{error}"
    );
    assert!(
        error.contains("root checkout needs an explicit scope"),
        "{error}"
    );
    assert!(error.contains("launch with -w"), "{error}");
    assert_eq!(
        env.store()
            .runtime_projection(rimz::RuntimeScope::Audit)
            .unwrap()
            .agents,
        before.agents
    );
    assert!(read_loop_instances(&env).0.is_empty());
    assert!(!env.project_root.join(".worktrees").exists());
}

fn write_team_signal_config(env: &Env) {
    crate::common::write_definition(
        env,
        "agents",
        "claude",
        "description: Claude base",
        "Follow instructions.",
    );
    crate::common::write_definition(
        env,
        "agents",
        "worker",
        "description: Worker\nagent: claude\ntools: []",
        "",
    );
    crate::common::write_definition(
        env,
        "teams",
        "forge",
        "leader: coder\nstages: [Build]\nroles:\n  - {role: coder, agent: worker, owns: [Build], signals: [ci.failed]}\n  - {role: reviewer, agent: worker, signals: [deploy.done]}",
        "Complete the work.",
    );
}

fn team_signal_fixture(env: &Env) -> Option<std::path::PathBuf> {
    if !init_git_repo(&env.project_root) {
        crate::common::skip("git unavailable");
        return None;
    }
    let cwd = env.home_root.join("feature-team");
    assert!(git_ok(
        &env.project_root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature-team",
            cwd.to_str().unwrap()
        ]
    ));
    env.install_agent_hooks("claude");
    write_team_signal_config(env);
    Some(cwd)
}

fn seed_team_signal_member(env: &Env, cwd: &Path, session: &str, parent: Option<&str>) {
    seed_agent_launch(
        env,
        cwd,
        session,
        rimz::agents::LaunchParams {
            team: Some("forge".to_owned()),
            role: Some("coder".to_owned()),
            channel: Some("feature-team".to_owned()),
            parent_agent_id: parent.map(AgentSessionId::from),
            parent_agent_kind: parent.map(|_| AgentKind::new_unchecked("claude")),
            launch_depth: parent.map(|_| 1),
            ..Default::default()
        },
        None,
    );
}

fn seed_agent_launch(
    env: &Env,
    cwd: &Path,
    session: &str,
    mut launch: rimz::agents::LaunchParams,
    runtime_owner: Option<rimz::pane::RuntimeOwner>,
) {
    // A launch event carries the lane its stamp means at `cwd`.
    if let Some(stamp) = launch.channel.take() {
        launch.lane = Some(crate::common::wait::stamped_lane(env, cwd, &stamp));
    }
    let workspace = env.resolve_workspace(&env.project_root);
    env.store()
        .append_event(&rimz::store::event::EventEnvelope::agent_launched(
            workspace.workspace_id,
            &workspace.session_name,
            &AgentKind::new_unchecked("claude"),
            rimz::store::event::AgentLaunchPayload {
                system_prompt_digest: None,
                agent_id: AgentSessionId::from(format!("launch_{session}")),
                launch_id: Some(AgentSessionId::from(format!("launch_{session}"))),
                agent_name: session.to_owned(),
                agent_name_explicit: true,
                launch,
                state: rimz::store::event::AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner,
                worktree_path: Some(cwd.display().to_string()),
                worktree_branch: Some("feature-team".to_owned()),
                prompt: None,
                description: None,
            },
        ))
        .unwrap();
}

fn team_signal_hook(env: &Env, cwd: &Path, session: &str, event: &str) {
    let mut command = env.hook_command("claude");
    command
        .current_dir(cwd)
        .env(rimz::harness::launch::ENV_AGENT_NAME, session)
        .env(rimz::workspace::ENV_CHANNEL, "feature-team");
    let payload =
        json!({"hook_event_name": event, "session_id": session, "cwd": cwd, "prompt": "work"})
            .to_string();
    let output = env
        .spawn_payload(command, &payload)
        .wait_with_output()
        .unwrap();
    env.drain_hooks();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("failed to arm team"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn loop_deliveries_always_persist_as_instances() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-instance-storage", "feature-loop");
    for (name, trigger) in [
        ("recurring", ["--every", "15m"]),
        ("standing", ["--signal", "deploy.done"]),
        ("one-shot", ["--in", "1h"]),
    ] {
        loop_ok(
            &env,
            &[
                "loop", "add", name, "--wait", "@claude", trigger[0], trigger[1],
            ],
        );
    }
    let instances = read_loop_instances(&env);
    for name in ["recurring", "standing", "one-shot"] {
        assert!(instances.0[name].wait.is_some());
    }
    let one_shot = &instances.0["one-shot"];
    assert!(
        one_shot.at.is_none() && one_shot.fire_at.is_some(),
        "{one_shot:?}"
    );
    let config = std::fs::read_to_string(loop_config_path(&env)).unwrap_or_default();
    assert!(!config.contains("[tasks."), "{config}");
    let (_, error) = loop_fail(
        &env,
        &[
            "loop",
            "add",
            "project-wait",
            "--project",
            "--wait",
            "@claude",
            "--every",
            "15m",
        ],
    );
    assert!(error.contains("--project"), "{error}");
}

#[test]
fn loop_one_shots_store_an_absolute_instant() {
    let env = Env::new();
    env.record(&env.project_root);
    let before = jiff::Timestamp::now();
    for (name, trigger) in [
        ("later", ["--in", "3d"]),
        ("weekly", ["--at", "fri 09:30"]),
        ("dated", ["--at", "2099-01-02 03:04"]),
    ] {
        let out = loop_ok(
            &env,
            &[
                "loop", "add", name, "--check", "true", trigger[0], trigger[1],
            ],
        );
        let entry = &read_loop_instances(&env).0[name];
        assert!(entry.at.is_none(), "{name}: {entry:?}");
        let fire_at = entry.fire_at.expect("a one-shot stores fire-at");
        assert!(fire_at > before, "{name}: {fire_at}");
        assert!(out.contains("trigger: fires once at 20"), "{name}: {out}");
    }
    let later = read_loop_instances(&env).0["later"].fire_at.unwrap();
    let three_days = jiff::SignedDuration::from_secs(3 * 86_400);
    assert!(
        later >= before.checked_add(three_days).unwrap() && later.as_second() % 60 == 0,
        "{later}"
    );
    let (_, error) = loop_fail(
        &env,
        &[
            "loop",
            "add",
            "past",
            "--check",
            "true",
            "--at",
            "2001-01-02 03:04",
        ],
    );
    assert!(error.contains("in the past"), "{error}");
    let (_, error) = loop_fail(
        &env,
        &[
            "loop",
            "add",
            "weekly",
            "--check",
            "true",
            "--every",
            "day",
            "--at",
            "fri 09:30",
        ],
    );
    assert!(error.contains("--every <days> --at HH:MM"), "{error}");
}

#[test]
fn loop_wait_me_and_bare_wait_pin_the_calling_session() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-caller", "feature-loop");
    for (name, wait) in [
        ("explicit", vec!["--wait", "@me"]),
        ("bare", vec!["--wait"]),
    ] {
        let output = calling_loop(&env, "sess-loop-caller")
            .args(["loop", "add", name, "--every", "15m"])
            .args(wait)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let instances = read_loop_instances(&env);
        let target = instances.0[name].wait.as_ref().unwrap();
        assert_eq!(target.kind, AgentKind::new_unchecked("claude"));
        assert_eq!(target.session, AgentSessionId::from("sess-loop-caller"));
        assert!(instances.0[name].wait_meta.is_none());
    }
}

#[test]
fn loop_signal_dedupe_preserves_the_existing_definition_and_overlays() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-dedupe", "feature-loop");
    let output = loop_ok(
        &env,
        &[
            "loop",
            "add",
            "original",
            "--wait",
            "@claude",
            "--signal",
            "deploy.done",
            "--match",
            "zone=west",
            "--match",
            "status=green",
            "--prompt",
            "keep this prompt",
        ],
    );
    assert!(output.contains("added loop task `original`"), "{output}");
    let mut original = read_loop_instances(&env);
    original.0.get_mut("original").unwrap().team = Some("forge#feature-loop".parse().unwrap());
    write_loop_instances(&env, original.clone());
    let key = project_task_key(&env.project_root, "original");
    let arming = BTreeMap::from([(
        key.clone(),
        Arming {
            enabled: true,
            at: Some(Timestamp::from_second(1).unwrap()),
            pause_until: Some(Timestamp::from_second(2).unwrap()),
            strikes: None,
        },
    )]);
    write_loop_arming(&env, &arming);
    let strikes = BTreeMap::from([(key, 2_u32)]);
    std::fs::write(
        loop_strikes_path(&env),
        serde_json::to_vec(&strikes).unwrap(),
    )
    .unwrap();
    let output = loop_ok(
        &env,
        &[
            "loop",
            "add",
            "replacement",
            "--wait",
            "@claude",
            "--signal",
            "deploy.done",
            "--match",
            "status=green",
            "--match",
            "zone=west",
            "--once",
            "--prompt",
            "discard this prompt",
        ],
    );
    assert!(
        output.contains("already subscribed as original"),
        "{output}"
    );
    assert_eq!(read_loop_instances(&env), original);
    assert_eq!(read_loop_arming(&env), arming);
    assert_eq!(read_loop_strikes(&env), strikes);
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "different-match",
            "--wait",
            "@claude",
            "--signal",
            "deploy.done",
            "--match",
            "zone=east",
            "--match",
            "status=green",
        ],
    );
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "different-selector",
            "--wait",
            "@claude",
            "--signal",
            "deploy.failed",
            "--match",
            "zone=west",
            "--match",
            "status=green",
        ],
    );
    assert_eq!(read_loop_instances(&env).0.len(), 3);
    register_running_agent(&env, "sess-loop-other-target", "feature-other");
    let output = calling_loop(&env, "sess-loop-other-target")
        .args([
            "loop",
            "add",
            "different-target",
            "--wait",
            "@me",
            "--signal",
            "deploy.done",
            "--match",
            "status=green",
            "--match",
            "zone=west",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let instances = read_loop_instances(&env);
    assert_eq!(instances.0.len(), 4);
    assert_eq!(
        instances.0["different-target"]
            .wait
            .as_ref()
            .unwrap()
            .session,
        AgentSessionId::from("sess-loop-other-target")
    );
}

#[test]
fn concurrent_signal_adds_return_one_existing_name() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-concurrent", "feature-loop");
    let children = ["first", "second"].map(|name| {
        env.rimz()
            .args([
                "loop",
                "add",
                name,
                "--wait",
                "@claude",
                "--signal",
                "deploy.done",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    });
    let outputs = children.map(|child| {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    });
    let instances = read_loop_instances(&env);
    assert_eq!(instances.0.len(), 1);
    let name = instances.0.keys().next().unwrap();
    for output in outputs {
        assert!(output.contains(name), "{output}");
    }
}

#[test]
fn loop_signal_defaults_follow_the_caller_worktree_and_team() {
    let env = Env::new();
    env.record(&env.project_root);
    let Some(cwd) = team_signal_fixture(&env) else {
        return;
    };
    register_running_agent(&env, "sess-scope-target", "main");
    seed_team_signal_member(&env, &cwd, "sess-scope-caller", None);
    team_signal_hook(&env, &cwd, "sess-scope-caller", "SessionStart");
    for (name, signal, key, expected) in [
        ("caller-ci", "ci.passed", "path", cwd.display().to_string()),
        (
            "caller-team",
            "team.idle",
            "instance",
            "forge#feature-team".to_owned(),
        ),
    ] {
        let output = calling_loop(&env, "launch_sess-scope-caller")
            .env("RIMZ_AGENT_NAME", "sess-scope-caller")
            .args([
                "loop",
                "add",
                name,
                "--wait",
                "@claude#project",
                "--signal",
                signal,
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let instances = read_loop_instances(&env);
        assert_eq!(instances.0[name].matches.as_ref().unwrap()[key], expected);
        assert_eq!(
            instances.0[name].wait.as_ref().unwrap().session,
            AgentSessionId::from("sess-scope-target")
        );
    }
    let output = calling_loop(&env, "sess-scope-target")
        .args(["loop", "add", "root-ci", "--wait", "--signal", "ci.failed"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("root checkout needs an explicit scope"),
        "{error}"
    );
    assert!(!read_loop_instances(&env).0.contains_key("root-ci"));
    let output = calling_loop(&env, "sess-scope-target")
        .args([
            "loop",
            "add",
            "root-any",
            "--wait",
            "--signal",
            "ci.failed",
            "--match",
            "branch=*",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        read_loop_instances(&env).0["root-any"].matches,
        Some(BTreeMap::from([("branch".to_owned(), "*".to_owned())]))
    );
}

#[test]
fn signal_siblings_keep_subscriptions_and_matches_consume_only_once() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-signal-lifetimes", "feature-loop");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "standing",
            "--wait",
            "@claude",
            "--signal",
            "deploy.done",
            "--match",
            "branch=feature-loop",
        ],
    );
    let mut instances = read_loop_instances(&env);
    let mut once = instances.0["standing"].clone();
    once.once = Some(true);
    instances.0.insert("once".to_owned(), once);
    write_loop_instances(&env, instances.clone());
    loop_ok(
        &env,
        &[
            "events",
            "emit",
            "deploy.done",
            "--json",
            r#"{"branch":"sibling"}"#,
        ],
    );
    assert_eq!(read_loop_instances(&env), instances);
    assert!(read_loop_run_records(&env).is_empty());
    assert!(env.store().list_pending_messages().unwrap().is_empty());
    loop_ok(
        &env,
        &[
            "events",
            "emit",
            "deploy.done",
            "--json",
            r#"{"branch":"feature-loop"}"#,
        ],
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while read_loop_run_records(&env).len() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 2, "{records:?}");
    assert!(
        records
            .iter()
            .all(|record| record.result == LoopRunResult::Delivered)
    );
    instances.0.remove("once");
    assert_eq!(read_loop_instances(&env), instances);
    let messages = env.store().list_pending_messages().unwrap();
    assert_eq!(messages.len(), 2);
    assert!(messages.iter().all(|message| message.sender
        == rimz::store::message::MessageSender::Harness {
            notice: rimz::store::message::HarnessNotice::Signal,
        }));
}

#[test]
fn session_end_hook_retires_all_own_deliveries_and_their_overlays() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-retire", "feature-loop");
    for (name, trigger) in [
        ("clock", ["--every", "15m"]),
        ("signal", ["--signal", "deploy.done"]),
    ] {
        loop_ok(
            &env,
            &[
                "loop", "add", name, "--wait", "@claude", trigger[0], trigger[1],
            ],
        );
    }
    loop_ok(&env, &["loop", "disable", "clock"]);
    loop_ok(&env, &["loop", "pause", "signal", "--for", "2h"]);
    let mut instances = read_loop_instances(&env);
    let mut sibling = instances.0["clock"].clone();
    sibling.wait.as_mut().unwrap().session = AgentSessionId::from("sess-retire-sibling");
    instances.0.insert("sibling".to_owned(), sibling.clone());
    write_loop_instances(&env, instances);
    std::fs::write(
        loop_strikes_path(&env),
        serde_json::to_vec(&BTreeMap::from([
            (project_task_key(&env.project_root, "clock"), 2_u32),
            (project_task_key(&env.project_root, "signal"), 1),
        ]))
        .unwrap(),
    )
    .unwrap();
    run_hook(
        &env,
        json!({"hook_event_name": "SessionEnd", "session_id": "sess-retire"}),
        &env.project_root,
    );
    assert_eq!(
        read_loop_instances(&env).0,
        BTreeMap::from([("sibling".to_owned(), sibling)])
    );
    for name in ["clock", "signal"] {
        let key = project_task_key(&env.project_root, name);
        assert!(!read_loop_arming(&env).contains_key(&key));
        assert!(!read_loop_strikes(&env).contains_key(&key));
    }
}

#[test]
fn non_hook_end_retires_matching_subscription_before_delivery() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-ended", "feature-loop");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "ended",
            "--wait",
            "@claude",
            "--signal",
            "deploy.done",
            "--match",
            "branch=feature-loop",
        ],
    );
    register_running_agent(&env, "sess-live", "feature-loop");
    let mut instances = read_loop_instances(&env);
    let mut live = instances.0["ended"].clone();
    live.wait.as_mut().unwrap().session = AgentSessionId::from("sess-live");
    instances.0.insert("live".to_owned(), live.clone());
    write_loop_instances(&env, instances);
    loop_ok(&env, &["loop", "enable", "ended"]);
    let key = project_task_key(&env.project_root, "ended");
    std::fs::write(
        loop_strikes_path(&env),
        serde_json::to_vec(&BTreeMap::from([(key.clone(), 1_u32)])).unwrap(),
    )
    .unwrap();
    stamp_session_ended(&env, "sess-ended");
    loop_ok(
        &env,
        &[
            "events",
            "emit",
            "deploy.done",
            "--json",
            r#"{"branch":"feature-loop"}"#,
        ],
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while read_loop_run_records(&env).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].task, "live");
    assert_eq!(records[0].result, LoopRunResult::Delivered);
    assert_pending_message(&env, "sess-live", "deploy.done");
    assert_eq!(
        read_loop_instances(&env).0,
        BTreeMap::from([("live".to_owned(), live)])
    );
    assert!(!read_loop_arming(&env).contains_key(&key));
    assert!(!read_loop_strikes(&env).contains_key(&key));
}

/// The reported symptom: a listener whose family mostly fires siblings never
/// reaches the delivering path's liveness gate, so the sibling fire itself has
/// to refuse a durably ended target. A live sibling session and a target with no
/// agent row at all still record their skip — the latter stays gc's business.
#[test]
fn sibling_fire_retires_only_the_durably_ended_subscription() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-ended", "feature-loop");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "ended",
            "--wait",
            "@claude",
            "--signal",
            "deploy.done",
        ],
    );
    register_running_agent(&env, "sess-live", "feature-loop");
    let mut instances = read_loop_instances(&env);
    let template = instances.0["ended"].clone();
    for (name, session) in [("live", "sess-live"), ("absent", "sess-never-registered")] {
        let mut row = template.clone();
        row.wait.as_mut().unwrap().session = AgentSessionId::from(session);
        instances.0.insert(name.to_owned(), row);
    }
    write_loop_instances(&env, instances.clone());
    loop_ok(&env, &["loop", "enable", "ended"]);
    let key = project_task_key(&env.project_root, "ended");
    std::fs::write(
        loop_strikes_path(&env),
        serde_json::to_vec(&BTreeMap::from([(key.clone(), 2_u32)])).unwrap(),
    )
    .unwrap();
    stamp_session_ended(&env, "sess-ended");

    // Same family, different exact name: the skip branch, which returns before
    // the fire path's only liveness gate.
    loop_ok(&env, &["events", "emit", "deploy.started"]);

    let records = read_loop_run_records(&env);
    assert_eq!(
        records
            .iter()
            .map(|record| (record.task.as_str(), record.result))
            .collect::<BTreeMap<_, _>>(),
        BTreeMap::from([
            ("live", LoopRunResult::SignalSkipped),
            ("absent", LoopRunResult::SignalSkipped),
        ]),
        "{records:?}"
    );
    instances.0.remove("ended");
    assert_eq!(read_loop_instances(&env), instances);
    assert!(!read_loop_arming(&env).contains_key(&key));
    assert!(!read_loop_strikes(&env).contains_key(&key));
}

#[test]
fn other_session_hook_retires_non_hook_ended_subscription() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-ended", "feature-loop");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "ended",
            "--wait",
            "@claude",
            "--signal",
            "deploy.done",
        ],
    );
    register_running_agent(&env, "sess-live", "feature-loop");
    let mut instances = read_loop_instances(&env);
    let mut live = instances.0["ended"].clone();
    live.wait.as_mut().unwrap().session = AgentSessionId::from("sess-live");
    instances.0.insert("live".to_owned(), live.clone());
    write_loop_instances(&env, instances);
    stamp_session_ended(&env, "sess-ended");
    run_hook(
        &env,
        json!({"hook_event_name": "SessionStart", "session_id": "sess-live"}),
        &env.project_root,
    );
    assert_eq!(
        read_loop_instances(&env).0,
        BTreeMap::from([("live".to_owned(), live)])
    );
}

#[test]
fn side_conversation_registration_hook_retires_ended_subscription() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-ended", "feature-loop");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "ended",
            "--wait",
            "@claude",
            "--signal",
            "deploy.done",
        ],
    );
    assert!(read_loop_instances(&env).0.contains_key("ended"));
    stamp_session_ended(&env, "sess-ended");
    run_agent_hook(
        &env,
        "codex",
        json!({
            "hook_event_name": "SessionStart",
            "session_id": "codex-side",
            "source": "fork",
            "transcript_path": null,
        }),
        &env.project_root,
    );
    assert!(read_loop_instances(&env).0.is_empty());
}

/// The hook reconcile is a price every observation pays, so one whose commit
/// appends nothing must not pay it: nothing was published, the session reaper
/// never ran, and no end appeared that was not already there. A side
/// conversation's repeat hook is that case — the store returns before it stages
/// anything — and the next observation that does append still takes the row.
#[test]
fn repeat_side_conversation_hook_reconciles_nothing() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-ended", "feature-loop");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "ended",
            "--wait",
            "@claude",
            "--signal",
            "deploy.done",
        ],
    );
    let armed = read_loop_instances(&env);
    // A side conversation's first hook registers it, so that one does append.
    run_agent_hook(
        &env,
        "codex",
        json!({
            "hook_event_name": "SessionStart",
            "session_id": "codex-side",
            "source": "fork",
            "transcript_path": null,
        }),
        &env.project_root,
    );
    stamp_session_ended(&env, "sess-ended");
    run_agent_hook(
        &env,
        "codex",
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "codex-side",
            "transcript_path": null,
            "prompt": "a side question",
        }),
        &env.project_root,
    );
    assert_eq!(read_loop_instances(&env), armed);
    register_running_agent(&env, "sess-live", "feature-loop");
    assert!(read_loop_instances(&env).0.is_empty());
}

#[test]
fn revived_session_subscription_survives_matching_signal() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-revived", "feature-loop");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "revived",
            "--wait",
            "@claude",
            "--signal",
            "deploy.done",
        ],
    );
    let instances = read_loop_instances(&env);
    stamp_session_ended(&env, "sess-revived");
    run_hook(
        &env,
        json!({"hook_event_name": "SessionStart", "session_id": "sess-revived"}),
        &env.project_root,
    );
    loop_ok(&env, &["events", "emit", "deploy.done"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    while read_loop_run_records(&env).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].task, "revived");
    assert_eq!(records[0].result, LoopRunResult::Delivered);
    assert_pending_message(&env, "sess-revived", "deploy.done");
    assert_eq!(read_loop_instances(&env), instances);
}

#[test]
fn retired_delivery_runner_preserves_replacement_session_subscription() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "retired-session", "feature-cas");
    let ready = env.project_root.join("check-ready");
    let release = env.project_root.join("check-release");
    let check = format!(
        "touch {}; while ! test -e {}; do sleep 0.01; done",
        shlex::try_quote(ready.to_str().unwrap()).unwrap(),
        shlex::try_quote(release.to_str().unwrap()).unwrap(),
    );
    // `loop add` refuses a guarded `--wait`; a stored row of the shape still runs.
    write_loop_instances(
        &env,
        Tasks(BTreeMap::from([(
            "reused".to_owned(),
            TaskEntry {
                wait: Some(TaskTarget {
                    kind: AgentKind::new_unchecked("claude"),
                    session: AgentSessionId::from("retired-session"),
                    handle: "@claude".to_owned(),
                }),
                prompt: Some("old".to_owned()),
                check: Some(check.into()),
                on: Some(CheckOn::Any),
                root: env.project_root.clone(),
                every: Some("15m".to_owned()),
                ..TaskEntry::default()
            },
        )])),
    );
    loop_ok(&env, &["loop", "run", "reused"]);
    wait_for_path(&ready);
    run_hook(
        &env,
        json!({"hook_event_name": "SessionEnd", "session_id": "retired-session"}),
        &env.project_root,
    );
    register_running_agent(&env, "replacement-session", "feature-cas");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "reused",
            "--wait",
            "@replacement-session",
            "--every",
            "15m",
            "--prompt",
            "replacement",
        ],
    );
    std::fs::write(release, "").unwrap();
    // The run's message step finds its pinned session gone.
    assert_eq!(
        wait_for_loop_report(&env, "reused").result,
        LoopRunResult::Failed
    );
    assert_eq!(
        read_loop_instances(&env).0["reused"]
            .wait
            .as_ref()
            .unwrap()
            .session
            .as_str(),
        "replacement-session"
    );
    loop_ok(&env, &["loop", "run", "reused"]);
    assert_pending_message(&env, "replacement-session", "replacement");
}

#[test]
fn same_task_name_in_two_rooms_does_not_collide() {
    let env = Env::new();
    let other = env.home_root.join("other-project");
    std::fs::create_dir(&other).expect("other project");
    for root in [&env.project_root, &other] {
        env.record(root);
        let output = env
            .rimz()
            .current_dir(root)
            .args(["loop", "add", "same", "--check", "true", "--at", "07:00"])
            .output()
            .expect("add instance");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let other_path = env
        .state_path_for(&other)
        .root
        .join("records/loop-instances.json");
    let other_bytes = std::fs::read(&other_path).expect("other instances");
    let other_tasks: Tasks = serde_json::from_slice(&other_bytes).expect("tasks");
    assert_eq!(
        read_loop_instances(&env).0["same"].resolved_root(),
        env.project_root
    );
    assert_eq!(other_tasks.0["same"].resolved_root(), other);
    loop_ok(&env, &["loop", "disable", "same"]);
    let output = env
        .rimz()
        .current_dir(&other)
        .args(["loop", "enable", "same"])
        .output()
        .expect("enable other");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let arming = read_loop_arming(&env);
    assert!(!arming[&project_task_key(&env.project_root, "same")].enabled);
    assert!(
        arming
            .get(&project_task_key(&other, "same"))
            .is_none_or(|state| state.enabled)
    );
    loop_ok(&env, &["loop", "remove", "same"]);
    assert!(!read_loop_instances(&env).0.contains_key("same"));
    assert_eq!(
        std::fs::read(&other_path).expect("other unchanged"),
        other_bytes
    );
}

#[test]
fn loop_launched_logs_show_checkout_and_leader() {
    let env = Env::new();
    let mut record =
        LoopRunRecord::new("resident", LoopRunResult::Launched, LoopRunMode::Manual, 0);
    record.checkout = Some(env.project_root.join("lane"));
    record.target = Some("@spry-cargo".into());
    write_loop_run_records(&env, &[record]);
    let output = loop_ok(&env, &["loop", "logs", "resident"]);
    assert!(
        output.contains("checkout:") && output.contains("/lane"),
        "{output}"
    );
    assert!(output.contains("leader: @spry-cargo"), "{output}");
}

#[test]
fn loop_history_filters_workspace_and_keeps_legacy_records() {
    let env = Env::new();
    let records = [
        (None, "legacy-room"),
        (Some(env.project_root.clone()), "this-room"),
        (Some(env.home_root.join("other")), "foreign-room"),
    ]
    .map(|(root, message)| {
        let mut record =
            LoopRunRecord::new("history", LoopRunResult::Errored, LoopRunMode::Manual, 0);
        record.root = root;
        record.error = Some(message.to_owned());
        record
    });
    write_loop_run_records(&env, &records);
    {
        let output = loop_ok(&env, &["loop", "logs", "history"]);
        assert!(
            output.contains("legacy-room") && output.contains("this-room"),
            "{output}"
        );
        assert!(!output.contains("foreign-room"), "{output}");
    }
    for flags in [vec![], vec!["--json"]] {
        let output = env
            .rimz()
            .args(["loop", "show", "history"])
            .args(flags)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(stderr, retained_runs_refusal(&stderr, "history", 2));
    }
}

#[test]
fn external_tick_fires_a_machine_task_without_a_workspace_record() {
    if which::which("tmux").is_err() && which::which("zellij").is_err() {
        crate::common::skip("neither tmux nor zellij on PATH");
        return;
    }
    let env = Env::new();
    let marker = env.project_root.join("machine-tick-ran");
    write_loop_config(
        &env,
        &format!(
            "[tasks.machine-tick]\ncheck = \"touch {}\"\nroot = \"{}\"\nevery = \"1m\"\n",
            marker.display(),
            env.project_root.display(),
        ),
    );
    let prior = Timestamp::now() - SignedDuration::from_mins(2);
    write_loop_fire_state(&env, BTreeMap::from([("machine-tick".to_owned(), prior)]));
    assert!(
        !env.state_path_for(&env.project_root)
            .workspace_record
            .exists()
    );
    assert!(!env.runtime_paths().shared_root.exists());

    assert!(tick_without_runner_leaves_state_absent(
        &env,
        &env.project_root
    ));
    loop_ok(&env, &["loop", "tick"]);

    wait_for_path(&marker);
    assert!(env.runtime_paths().shared_root.is_dir());
    assert!(
        env.state_path_for(&env.project_root)
            .workspace_record
            .exists()
    );
}

#[test]
fn external_tick_discovers_a_trusted_project_without_a_workspace_record() {
    if which::which("tmux").is_err() && which::which("zellij").is_err() {
        crate::common::skip("neither tmux nor zellij on PATH");
        return;
    }
    let env = Env::new();
    let project = env.home_root.join("never-roomed-project");
    std::fs::create_dir(&project).expect("project root");
    let marker = project.join("project-tick-ran");
    let output = env
        .rimz()
        .current_dir(&project)
        .args([
            "loop",
            "add",
            "project-tick",
            "--project",
            "--check",
            &format!("touch {}", marker.display()),
            "--every",
            "1m",
        ])
        .output()
        .expect("add project loop");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        rimz::trust::status_with_roots(&project, &env.rimz_home())
            .expect("project trust")
            .state,
        rimz::trust::TrustState::Trusted,
    );
    assert!(!env.state_path_for(&project).workspace_record.exists());
    let prior = Timestamp::now() - SignedDuration::from_mins(2);
    let mut arming = read_loop_arming(&env);
    arming
        .get_mut(&project_task_key(&project, "project-tick"))
        .expect("project arming")
        .at = Some(prior);
    write_loop_arming(&env, &arming);
    write_loop_fire_state_for_root(
        &env,
        &project,
        BTreeMap::from([("project-tick".to_owned(), prior)]),
    );

    assert!(tick_without_runner_leaves_state_absent(&env, &project));
    loop_ok(&env, &["loop", "tick"]);

    wait_for_path(&marker);
    assert!(env.state_path_for(&project).workspace_record.exists());
}

#[test]
fn malformed_instances_fail_reads_and_survive_adds() {
    let env = Env::new();
    let path = loop_instances_path(&env);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("workspace dir");
    std::fs::write(&path, b"not json").expect("instances");
    let (_, error) = loop_fail(&env, &["loop", "list"]);
    assert!(error.contains("loop-instances.json"), "{error}");
    loop_fail(
        &env,
        &["loop", "add", "new", "--check", "true", "--at", "07:00"],
    );
    assert_eq!(std::fs::read(&path).expect("unchanged"), b"not json");
}

#[test]
fn instance_task_in_a_recorded_project_reaches_machine_wide_readers() {
    let env = Env::new();
    env.record(&env.project_root);
    loop_ok(
        &env,
        &["loop", "add", "nightly", "--at", "07:00", "--check", "true"],
    );
    assert!(
        env.state_path_for(&env.project_root)
            .workspace_record
            .exists()
    );

    // Doctor reads instance tasks through the same enumeration the external
    // loop timer fires from.
    let output = env
        .rimz()
        .current_dir("/")
        .args(["doctor", "--json"])
        .output()
        .expect("rimz doctor");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"nightly\""), "{stdout}");
}

#[cfg(unix)]
#[test]
fn external_tick_spawns_an_agent_without_a_workspace_record() {
    if which::which("tmux").is_err() && which::which("zellij").is_err() {
        crate::common::skip("neither tmux nor zellij on PATH");
        return;
    }
    let env = Env::new();
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[agents]\nshell = 'bash'\n",
    )
    .unwrap();
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(&env);
    trust_codex_project(&env, &env.project_root);
    let agent_bin = crate::common::write_failing_agent_shim(&env, "codex", 1);
    let login_shell = write_fake_login_shell(&env, "rimz-test-sh", &[]);
    std::os::unix::fs::symlink(&login_shell, agent_bin.join("bash")).unwrap();
    let shell = write_path_shim(
        &env.home_root.join("interactive-shell-bin"),
        "rimz-test-sh",
        &format!(
            "for arg do\n  if [ \"$arg\" = -c ]; then exec {} \"$@\"; fi\ndone\nexec /bin/sh \"$@\"",
            shlex::try_quote(login_shell.to_str().unwrap()).unwrap(),
        ),
    );
    write_loop_config(
        &env,
        &format!(
            "default-timeout = \"1s\"\n[tasks.cold-agent]\nagent = \"codex\"\nprompt = \"repair\"\nroot = {:?}\nevery = \"1m\"\ntimeout = \"2s\"\nthrottle = \"off\"\n",
            env.project_root,
        ),
    );
    let prior = Timestamp::now() - SignedDuration::from_mins(2);
    write_loop_fire_state(&env, BTreeMap::from([("cold-agent".to_owned(), prior)]));
    assert!(!env.state_path_for(&env.project_root).root.exists());

    let output = env
        .rimz()
        .args(["loop", "tick"])
        .env("PATH", path_with_front(&agent_bin))
        .env("SHELL", shell)
        .output()
        .expect("tick a cold agent root");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The fire's workflow run starts the agent's one supervised run.
    wait_for_loop_run_records(&env, "the cold-agent step's supervised run", |_| {
        env.state_path_for(&env.project_root).root.exists()
            && rimz::harness::run::list(env.store().paths()).is_ok_and(|runs| runs.len() == 1)
    });
    assert!(
        env.state_path_for(&env.project_root)
            .workspace_record
            .exists()
    );
    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(env.state_path_for(&env.project_root).workspace_record).unwrap(),
    )
    .unwrap();
    assert_eq!(record.get("agent_shell"), None);
}

#[cfg(unix)]
#[test]
fn loop_birth_saves_shell_lookups_only_for_manual_fires() {
    use crate::common::CommandTimeoutExt;

    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (kind, prior, verb) in [
        ("check", false, "run"),
        ("check", true, "run"),
        ("agent", true, "run"),
        ("stay", false, "run"),
        ("stay", true, "run"),
        ("workflow", false, "run"),
        ("workflow", true, "run"),
        ("agent", true, "fire"),
        ("check-agent", true, "fire"),
        ("agent", true, "print"),
    ] {
        let env = Env::new();
        std::fs::write(
            env.rimz_home().join("config.toml"),
            "[agents]\nshell = 'bash'\n",
        )
        .unwrap();
        env.install_agent_hooks("codex");
        trust_codex_preflight_hooks(&env);
        trust_codex_project(&env, &env.project_root);
        let agent_bin = crate::common::write_failing_agent_shim(&env, "codex", 1);
        let shell = write_fake_login_shell(&env, "bash", &[]);
        let hit = agent_bin.join("bash");
        std::os::unix::fs::symlink(&shell, &hit).unwrap();
        let path = env.state_path_for(&env.project_root).workspace_record;
        if prior {
            env.record(&env.project_root);
            let mut record: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            record["agent_shell"] = json!({"name": "bash", "path": "/previous/bash"});
            std::fs::write(&path, record.to_string()).unwrap();
        }
        let effect = match kind {
            "check" => "check = 'true'\n".to_owned(),
            "agent" => "agent = 'codex'\nprompt = 'repair'\ntimeout = '1s'\n".to_owned(),
            "stay" => "agent = 'codex'\nprompt = 'repair'\nstay = true\n".to_owned(),
            "check-agent" => format!(
                "check = {:?}\n",
                shlex::try_join([
                    env.rimz_bin().to_str().unwrap(),
                    "--mux",
                    "zellij",
                    "agents",
                    "-p",
                    "codex",
                    "repair",
                    "--timeout",
                    "1s",
                ])
                .unwrap(),
            ),
            "workflow" => {
                let dir = env.rimz_home().join("workflows/review");
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(
                    dir.join("workflow.toml"),
                    "[workflow]\non-break='@failed'\nscope = 'worktree'\nstart = 'ready'\n\
                     [checks.ready]\nrun = 'true'\non.ok = '@done'\n",
                )
                .unwrap();
                "workflow = 'review'\n".to_owned()
            }
            _ => unreachable!(),
        };
        let throttle = if kind == "workflow" {
            ""
        } else {
            "throttle = 'off'\n"
        };
        write_loop_config(
            &env,
            &format!(
                "[tasks.lookup]\nroot = {:?}\nevery = '1h'\n{throttle}{effect}",
                env.project_root,
            ),
        );
        let mut command = env.rimz();
        command.args(["--mux", "zellij"]);
        if verb == "print" {
            command.args(["agents", "-p", "codex", "repair", "--timeout", "1s"]);
        } else {
            command.args(["loop", verb, "lookup"]);
        }
        let output = command
            .env("PATH", path_with_front(&agent_bin))
            .env("SHELL", &shell)
            .env("RIMZ_ZELLIJ_BIN", crate::common::zellij_trace_shim())
            .env("RIMZ_TEST_ZELLIJ_VERSION", "0.45.0")
            .env("RIMZ_TEST_ZELLIJ_LOG", env.home_root.join("zellij.log"))
            .bounded_output_within(LOOP_RUNNER_DEADLINE)
            .unwrap();
        let records = read_loop_run_records(&env);
        assert_eq!(
            !records.is_empty(),
            verb != "print",
            "{kind}/{prior}/{verb}: {output:?}"
        );
        // A check runs in the fire's detached run; its own agent launch births the room there.
        if kind.starts_with("check") {
            wait_for_loop_report(&env, "lookup");
        }
        // Every task's agent launches from its workflow run, whose advance writes the notes.
        let expected_notes = match (kind, verb) {
            ("agent" | "stay", "run" | "fire") | ("workflow", _) => vec![json!("workflow")],
            _ => Vec::new(),
        };
        let archive = env.state_path_for(&env.project_root).automation_notes_dir;
        let read_notes = || {
            let mut notes = Vec::new();
            for date in std::fs::read_dir(&archive).into_iter().flatten() {
                for run in std::fs::read_dir(date.unwrap().path()).unwrap() {
                    let Ok(manifest) = std::fs::read(run.unwrap().path().join("run.json")) else {
                        continue;
                    };
                    let manifest: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
                    notes.push(manifest["kind"].clone());
                }
            }
            notes
        };
        let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
        while read_notes().len() < expected_notes.len() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(read_notes(), expected_notes, "{kind}/{prior}/{verb}");
        let record: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&path).unwrap_or_else(|error| panic!("{error}: {output:?}")),
        )
        .unwrap();
        actual.push((kind, prior, verb, record.get("agent_shell").cloned()));
        expected.push((
            kind,
            prior,
            verb,
            (verb != "run").then(|| json!({"name": "bash", "path": hit})),
        ));
    }
    assert_eq!(actual, expected);
}

#[test]
fn external_tick_yields_a_root_with_a_fresh_sidebar() {
    let env = Env::new();
    env.record(&env.project_root);
    let record_path = env.state_path_for(&env.project_root).workspace_record;
    let record = std::fs::read(&record_path).unwrap();
    let marker = env.project_root.join("open-root-tick-ran");
    write_loop_config(
        &env,
        &format!(
            "[tasks.open-root]\ncheck = \"touch {}\"\nroot = \"{}\"\nevery = \"1m\"\n",
            marker.display(),
            env.project_root.display(),
        ),
    );
    let prior = Timestamp::now() - SignedDuration::from_mins(2);
    write_loop_fire_state(&env, BTreeMap::from([("open-root".to_owned(), prior)]));
    let runtime = env.runtime_paths();
    crate::common::room::seed_sidebar_heartbeat(&runtime, MuxName::Tmux, "rimz-test", "sidebar");

    loop_ok(&env, &["loop", "tick"]);

    let stamps: BTreeMap<String, Timestamp> = serde_json::from_slice(
        &std::fs::read(runtime.lane_path("loop-fire.json")).expect("fire state"),
    )
    .expect("fire state json");
    assert_eq!(stamps.get("open-root"), Some(&prior));
    assert!(!marker.exists());

    loop_ok(&env, &["loop", "run", "open-root"]);
    wait_for_path(&marker);
    assert_eq!(std::fs::read(&record_path).unwrap(), record);
}

#[cfg(unix)]
#[test]
fn external_tick_refuses_when_the_systemd_user_manager_is_unreachable() {
    let env = Env::new();
    let shims = env.home_root.join("tick-shims");
    write_path_shim(&shims, "systemd-run", "exit 0");
    write_path_shim(
        &shims,
        "systemctl",
        "echo 'Failed to connect to bus: No medium found' >&2\nexit 1",
    );
    let marker = env.project_root.join("unreachable-bus-tick-ran");
    write_loop_config(
        &env,
        &format!(
            "[tasks.unreachable-bus]\ncheck = \"touch {}\"\nroot = \"{}\"\nevery = \"1m\"\n",
            marker.display(),
            env.project_root.display(),
        ),
    );
    let prior = Timestamp::now() - SignedDuration::from_mins(2);
    write_loop_fire_state(
        &env,
        BTreeMap::from([("unreachable-bus".to_owned(), prior)]),
    );

    let output = env
        .rimz()
        .args(["loop", "tick"])
        .env("INVOCATION_ID", "fixture-timer-unit")
        .env("PATH", path_with_front(&shims))
        .output()
        .expect("rimz loop tick");

    assert!(
        !output.status.success(),
        "an unreachable user manager must refuse the tick"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Failed to connect to bus"), "{stderr}");
    assert!(stderr.contains("rimz loop timer install"), "{stderr}");
    assert!(!marker.exists());
    let stamps: BTreeMap<String, Timestamp> = serde_json::from_slice(
        &std::fs::read(env.runtime_paths().lane_path("loop-fire.json")).expect("fire state"),
    )
    .expect("fire state json");
    assert_eq!(stamps.get("unreachable-bus"), Some(&prior));
    assert!(read_loop_run_records(&env).is_empty());
}

#[test]
fn external_tick_records_a_spawn_failure_without_retrying_or_striking() {
    let env = Env::new();
    env.record(&env.project_root);
    write_loop_config(
        &env,
        &format!(
            "[tasks.missing-runner]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"1h\"\n",
            env.project_root.display(),
        ),
    );
    let prior = Timestamp::now() - SignedDuration::from_hours(2);
    write_loop_fire_state(&env, BTreeMap::from([("missing-runner".to_owned(), prior)]));
    let mut tick = env.rimz();
    tick.args(["loop", "tick"])
        .env("RIMZ_BIN", env.home_root.join("missing-rimz"));
    let output = tick.output().expect("rimz loop tick");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 1, "{records:?}");
    let record = &records[0];
    assert_eq!(record.task, "missing-runner");
    let root = canonical(&env.project_root);
    assert_eq!(record.root.as_deref(), Some(root.as_path()));
    assert_eq!(record.result, LoopRunResult::StartFailed);
    assert_eq!(record.mode, Some(LoopRunMode::Scheduled));
    assert_eq!(record.duration_ms, None);
    assert!(record.error.as_ref().is_some_and(|error| !error.is_empty()));
    let logs = loop_ok(&env, &["loop", "logs", "missing-runner"]);
    assert!(logs.contains("start failed"), "{logs}");
    let show = loop_ok(&env, &["loop", "show", "missing-runner"]);
    assert!(show.contains("start failed"), "{show}");
    let fire_path = env.runtime_paths().lane_path("loop-fire.json");
    let stamps: BTreeMap<String, Timestamp> =
        serde_json::from_slice(&std::fs::read(&fire_path).expect("fire state"))
            .expect("fire state json");
    assert!(stamps["missing-runner"] > prior);
    assert!(!loop_strikes_path(&env).exists());
    assert!(!loop_arming_path(&env).exists());

    let output = tick.output().expect("second rimz loop tick");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(read_loop_run_records(&env), records);
    let after: BTreeMap<String, Timestamp> =
        serde_json::from_slice(&std::fs::read(&fire_path).expect("fire state"))
            .expect("fire state json");
    assert_eq!(after, stamps);
    assert!(!loop_strikes_path(&env).exists());
    assert!(!loop_arming_path(&env).exists());
}

fn write_fire_at_row(env: &Env, name: &str) {
    write_loop_instances(
        env,
        Tasks(BTreeMap::from([(
            name.into(),
            TaskEntry {
                root: env.project_root.clone(),
                check: Some("true".into()),
                fire_at: Some(Timestamp::now() - SignedDuration::from_mins(30)),
                ..TaskEntry::default()
            },
        )])),
    );
}

fn tick_with_missing_runner(env: &Env) {
    let output = env
        .rimz()
        .args(["loop", "tick"])
        .env("RIMZ_BIN", env.home_root.join("missing-rimz"))
        .output()
        .expect("rimz loop tick");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn start_failures(env: &Env, name: &str) -> usize {
    read_loop_run_records(env)
        .iter()
        .filter(|record| record.task == name && record.result == LoopRunResult::StartFailed)
        .count()
}

/// A `fire-at` row whose runner never started keeps its row, is not dispatched
/// again within the five-minute grace, and is retried once the grace passes.
#[test]
fn external_tick_retries_a_fire_at_start_failure_after_the_grace() {
    let env = Env::new();
    env.record(&env.project_root);
    write_fire_at_row(&env, "stranded");

    tick_with_missing_runner(&env);
    assert_eq!(start_failures(&env, "stranded"), 1);
    assert!(read_loop_instances(&env).0.contains_key("stranded"));

    tick_with_missing_runner(&env);
    assert_eq!(start_failures(&env, "stranded"), 1, "within the grace");

    let stale = Timestamp::now() - SignedDuration::from_mins(6);
    write_loop_fire_state(&env, BTreeMap::from([("stranded".to_owned(), stale)]));
    tick_with_missing_runner(&env);
    assert_eq!(
        start_failures(&env, "stranded"),
        2,
        "retried past the grace"
    );
    assert!(read_loop_instances(&env).0.contains_key("stranded"));
    assert!(!loop_strikes_path(&env).exists());
    assert!(!loop_arming_path(&env).exists());
}

/// A scheduled runner that finds its `fire-at` row paused exits without a
/// record; the row waits and is dispatched again once live past the grace.
#[test]
fn a_paused_fire_at_runner_records_nothing_and_is_retried_once_live() {
    let env = Env::new();
    env.record(&env.project_root);
    write_fire_at_row(&env, "raced");
    loop_ok(&env, &["loop", "pause", "raced", "--for", "2h"]);

    loop_ok(&env, &["loop", "run", "raced"]);
    assert!(read_loop_run_records(&env).is_empty());
    assert!(read_loop_instances(&env).0.contains_key("raced"));

    let stale = Timestamp::now() - SignedDuration::from_mins(6);
    write_loop_fire_state(&env, BTreeMap::from([("raced".to_owned(), stale)]));
    tick_with_missing_runner(&env);
    assert_eq!(start_failures(&env, "raced"), 0, "carried while paused");

    loop_ok(&env, &["loop", "enable", "raced"]);
    tick_with_missing_runner(&env);
    assert_eq!(start_failures(&env, "raced"), 1, "dispatched once live");
    assert!(read_loop_instances(&env).0.contains_key("raced"));
}

#[cfg(unix)]
#[test]
fn loop_spawn_home_cwd_refusal_names_the_loop_fix() {
    let env = Env::new();
    env.record(&env.home_root);
    let store = env.store_for(&env.home_root);
    env.install_agent_hooks("claude");
    env.write_config(&env.home_root, "[agents]\nisolation = \"host\"\n");
    let agent_bin = crate::common::write_failing_agent_shim(&env, "claude", 1);
    write_loop_config(
        &env,
        &format!(
            "[tasks.home-spawn]\nagent = \"claude\"\nprompt = \"repair\"\nroot = {:?}\nevery = \"1h\"\nthrottle = \"off\"\n",
            env.home_root,
        ),
    );
    let trace = env.project_root.join("home-spawn-mux.log");
    let output = env
        .rimz()
        .current_dir(&env.home_root)
        .args(["--mux", "zellij", "loop", "run", "home-spawn"])
        .env("PATH", path_with_front(&agent_bin))
        .env("RIMZ_ZELLIJ_BIN", crate::common::zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    // The run's agent step refuses the home directory, so the run fails.
    let report = wait_for_loop_report(&env, "home-spawn");
    assert_eq!(report.result, LoopRunResult::Failed);
    let journal = loop_run_journal(&env, &env.home_root, &report);
    assert!(journal.contains("home directory"), "{journal}");
    assert!(
        journal.contains(env.home_root.to_str().unwrap()),
        "{journal}"
    );
    assert!(rimz::harness::run::list(store.paths()).unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn loop_judge_home_cwd_refusal_names_the_loop_fix() {
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    env.install_agent_hooks("claude");
    env.write_config(&env.project_root, "[agents]\nisolation = \"host\"\n");
    let agent_bin = crate::common::write_failing_agent_shim(&env, "claude", 1);
    write_loop_config(
        &env,
        &format!(
            "[tasks.home-judge]\nagent = \"claude\"\nprompt = \"repair\"\nroot = {:?}\ndir = {:?}\nevery = \"1h\"\nthrottle = \"off\"\non = \"success\"\n[tasks.home-judge.check]\nagent = \"claude\"\nprompt = \"Any work?\"\n",
            env.project_root, env.home_root,
        ),
    );
    let trace = env.project_root.join("home-judge-mux.log");
    let output = env
        .rimz()
        .args(["--mux", "zellij", "loop", "run", "home-judge"])
        .env("PATH", path_with_front(&agent_bin))
        .env("RIMZ_ZELLIJ_BIN", crate::common::zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    // The run's agent step refuses the home directory, so the run fails.
    let report = wait_for_loop_report(&env, "home-judge");
    assert_eq!(report.result, LoopRunResult::Failed);
    let journal = loop_run_journal(&env, &env.project_root, &report);
    assert!(journal.contains("home directory"), "{journal}");
    assert!(
        journal.contains(env.home_root.to_str().unwrap()),
        "{journal}"
    );
    assert!(rimz::harness::run::list(store.paths()).unwrap().is_empty());
}

#[test]
fn signal_emit_records_a_spawn_failure_without_reporting_the_task_as_fired() {
    let env = Env::new();
    env.record(&env.project_root);
    write_loop_config(
        &env,
        &format!(
            "[tasks.missing-runner]\ncheck = \"true\"\nroot = \"{}\"\nsignal = \"deploy.finished\"\n",
            env.project_root.display(),
        ),
    );
    let payload = json!({"outcome": "failure", "attempt": 2});
    let output = env
        .rimz()
        .args([
            "events",
            "emit",
            "deploy.finished",
            "--json",
            &payload.to_string(),
        ])
        .env("RIMZ_BIN", env.home_root.join("missing-rimz"))
        .output()
        .expect("rimz events emit");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 1, "{records:?}");
    let record = &records[0];
    assert_eq!(record.task, "missing-runner");
    assert_eq!(record.result, LoopRunResult::StartFailed);
    assert_eq!(record.duration_ms, None);
    let signal = record.signal.as_ref().expect("signal forensics");
    assert_eq!(signal.name.as_str(), "deploy.finished");
    assert_eq!(&signal.payload, payload.as_object().unwrap());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("fired no task"), "{stdout}");
    assert!(
        stdout.contains("  missing-runner: did not start"),
        "{stdout}"
    );
}

#[cfg(unix)]
#[test]
fn external_tick_records_a_failed_scope_handoff() {
    let env = Env::new();
    env.record(&env.project_root);
    let shims = env.home_root.join("tick-shims");
    write_path_shim(&shims, "systemctl", "exit 0");
    write_path_shim(&shims, "systemd-run", "exit 1");
    write_loop_config(
        &env,
        &format!(
            "[tasks.failed-handoff]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"1h\"\n",
            env.project_root.display(),
        ),
    );
    let prior = Timestamp::now() - SignedDuration::from_hours(2);
    write_loop_fire_state(&env, BTreeMap::from([("failed-handoff".to_owned(), prior)]));

    let output = env
        .rimz()
        .args(["loop", "tick"])
        .env("INVOCATION_ID", "fixture-timer-unit")
        .env("PATH", path_with_front(&shims))
        .output()
        .expect("rimz loop tick");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 1, "{records:?}");
    let record = &records[0];
    assert_eq!(record.task, "failed-handoff");
    assert_eq!(record.result, LoopRunResult::StartFailed);
    assert_eq!(record.mode, Some(LoopRunMode::Scheduled));
    let error = record.error.as_deref().expect("handoff failure reason");
    assert!(error.contains("scope hand-off"), "{error}");
    assert!(error.contains("no history row"), "{error}");
    let logs = loop_ok(&env, &["loop", "logs", "failed-handoff"]);
    assert!(logs.contains("start failed"), "{logs}");
    let stamps: BTreeMap<String, Timestamp> = serde_json::from_slice(
        &std::fs::read(env.runtime_paths().lane_path("loop-fire.json")).expect("fire state"),
    )
    .expect("fire state json");
    assert!(stamps["failed-handoff"] > prior);
}

#[cfg(unix)]
#[test]
fn external_tick_does_not_record_a_fast_run_as_a_failed_start() {
    let env = Env::new();
    env.record(&env.project_root);
    let shims = env.home_root.join("tick-shims");
    write_path_shim(&shims, "systemctl", "exit 0");
    write_path_shim(
        &shims,
        "systemd-run",
        "while [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = -- ]; then\n    shift\n    exec \"$@\"\n  fi\n  shift\ndone\nexit 1",
    );
    write_loop_config(
        &env,
        &format!(
            "[tasks.fast-run]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"1h\"\ndeadline = \"1970-01-01T00:00:01Z\"\n",
            env.project_root.display(),
        ),
    );
    let prior = Timestamp::now() - SignedDuration::from_hours(2);
    write_loop_fire_state(&env, BTreeMap::from([("fast-run".to_owned(), prior)]));

    let output = env
        .rimz()
        .args(["loop", "tick"])
        .env("INVOCATION_ID", "fixture-timer-unit")
        .env("PATH", path_with_front(&shims))
        .output()
        .expect("rimz loop tick");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    wait_for_path(&loop_runs_path(&env));
    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].task, "fast-run");
    let root = canonical(&env.project_root);
    assert_eq!(records[0].root.as_deref(), Some(root.as_path()));
    assert_eq!(records[0].result, LoopRunResult::Expired);
    assert_eq!(records[0].mode, Some(LoopRunMode::Scheduled));
    let logs = loop_ok(&env, &["loop", "logs", "fast-run"]);
    assert!(logs.contains("expired"), "{logs}");
    assert!(!logs.contains("start failed"), "{logs}");
}

#[test]
fn loop_watch_requires_a_terminal() {
    let env = Env::new();
    for args in [vec!["loop", "watch"], vec!["loop", "watch", "--hold"]] {
        let output = env.rimz().args(args).output().unwrap();
        assert!(!output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stderr).trim(),
            "error: `rimz loop watch` needs a terminal; use `rimz loop list` for one snapshot"
        );
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn loop_watch_refuses_without_an_open_room() {
    let env = Env::new();
    let (output, status) = loop_watch_pty(&env, false, true, &[]);
    let mut plain = anstream::StripStream::new(Vec::new());
    plain.write_all(output.as_bytes()).unwrap();
    assert_eq!(
        String::from_utf8(plain.into_inner()).unwrap().trim(),
        "error: `rimz loop watch` shows an open room's loops; start one with `rimz start`, or use `rimz loop list`"
    );
    assert_eq!(status, 1);
    assert!(!output.contains("\x1b[?1049h"), "{output}");
}

#[test]
fn loop_watch_pinned_room_paints_before_its_first_heartbeat() {
    let env = Env::new();
    let (output, _) = loop_watch_pty(
        &env,
        true,
        false,
        &["no loop tasks; add one with `rimz loop add …`"],
    );
    assert!(
        output.contains("no loop tasks") && output.contains("\x1b[J"),
        "{output}"
    );
    assert!(!output.contains("error:"), "{output}");
}

#[test]
fn loop_watch_reloads_tasks_without_reprobing_workspace() {
    let env = Env::new();
    seed_loop_watch_room(&env);
    let Some(real_git) = find_real_git() else {
        crate::common::skip("git not on PATH");
        return;
    };
    if !Command::new(&real_git)
        .args(["-C", env.project_root.to_str().expect("utf-8 project root")])
        .args(["init", "-q"])
        .status()
        .is_ok_and(|status| status.success())
    {
        crate::common::skip("git unavailable");
        return;
    }

    let bin_dir = env.home_root.join("git-trace-bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir git trace bin");
    std::os::unix::fs::symlink(
        crate::common::cargo_bin("git-trace", env!("CARGO_BIN_EXE_git-trace")),
        bin_dir.join("git"),
    )
    .expect("symlink git trace shim");
    let git_log = env.home_root.join("loop-watch-git.log");

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open loop watch pty");
    let mut cmd = CommandBuilder::new(env.rimz_bin());
    env.pin_pty_command(&mut cmd);
    cmd.args(["loop", "watch", "--hold"]);
    cmd.cwd(env.project_root.as_os_str());
    cmd.env("TERM", "xterm-256color");
    cmd.env("RIMZ_TEST_GIT_LOG", &git_log);
    cmd.env("RIMZ_TEST_REAL_GIT", &real_git);
    cmd.env("PATH", crate::common::path_with_front(&bin_dir));

    let mut child = pair.slave.spawn_command(cmd).expect("spawn loop watch");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
    let (startup_tx, startup_rx) = std::sync::mpsc::sync_channel(1);
    let (reload_tx, reload_rx) = std::sync::mpsc::sync_channel(1);
    let reader_thread = std::thread::spawn(move || {
        let mut output = Vec::new();
        let mut buffer = [0; 4096];
        let mut terminal = vt100::Parser::new(24.try_into().unwrap(), 100.try_into().unwrap(), 0);
        while let Ok(count) = reader.read(&mut buffer) {
            if count == 0 {
                break;
            }
            output.extend_from_slice(&buffer[..count]);
            terminal.process(&buffer[..count]);
            let frame = terminal.screen().contents();
            if frame.contains("no loop tasks; add one with `rimz loop add …`") {
                let _ = startup_tx.try_send(());
            }
            if frame.contains("watch-reloaded") && frame.contains("never fired") {
                let _ = reload_tx.try_send(());
            }
        }
        output
    });

    let startup = startup_rx.recv_timeout(Duration::from_secs(30));
    let exited_after_startup = child.try_wait().expect("poll loop watch");
    write_loop_config(
        &env,
        &format!(
            "[tasks.watch-reloaded]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"15m\"\n",
            env.project_root.display()
        ),
    );
    let reloaded = reload_rx.recv_timeout(Duration::from_secs(30));
    let exited_after_reload = child.try_wait().expect("poll loop watch");
    let exited_early = exited_after_startup.or(exited_after_reload);
    if exited_early.is_none() {
        child.kill().expect("terminate loop watch");
        let _ = child.wait().expect("reap loop watch");
    }
    drop(pair.master);
    let output =
        String::from_utf8_lossy(&reader_thread.join().expect("join pty reader")).into_owned();
    assert!(
        startup.is_ok() && reloaded.is_ok() && exited_early.is_none(),
        "loop watch exited or missed config reload: {exited_early:?}\n{output}"
    );

    let git_trace = std::fs::read_to_string(&git_log).expect("read loop watch git trace");
    // The fixture repo is unborn, so the combined probe falls back once.
    for probe in [
        "\trev-parse\t--show-toplevel\t--git-common-dir\t--abbrev-ref\tHEAD",
        "\trev-parse\t--show-toplevel\t--git-common-dir",
    ] {
        assert_eq!(
            git_trace
                .lines()
                .filter(|line| line.starts_with("git\t-C\t") && line.ends_with(probe))
                .count(),
            1,
            "workspace probe should run once: {probe}\n{git_trace}"
        );
    }
}

#[test]
#[cfg(target_os = "linux")]
fn loop_watch_follows_room_build() {
    watch_build_move(false);
}

#[test]
#[cfg(target_os = "linux")]
fn loop_watch_follows_room_build_while_held() {
    watch_build_move(true);
}

#[cfg(target_os = "linux")]
fn watch_build_move(hold: bool) {
    use crate::common::builds::{RoomBuilds, executable};

    let env = Env::new();
    let builds = RoomBuilds::new(&env);
    seed_loop_watch_room(&env);
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = CommandBuilder::new(&builds.first);
    env.pin_pty_command(&mut command);
    command.args(["loop", "watch"]);
    if hold {
        command.arg("--hold");
    }
    command.cwd(env.project_root.as_os_str());
    command.env("TERM", "xterm-256color");
    command.env("RIMZ_TEST_SIDEBAR_RECORD_POLL_MS", "50");
    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let pid = child.process_id().unwrap();
    let mut reader = pair.master.try_clone_reader().unwrap();
    let (startup_tx, startup_rx) = std::sync::mpsc::sync_channel(1);
    let (repaint_tx, repaint_rx) = std::sync::mpsc::sync_channel(1);
    let reader_thread = std::thread::spawn(move || {
        let mut output = Vec::new();
        let mut buffer = [0; 4096];
        let mut terminal = vt100::Parser::new(24.try_into().unwrap(), 100.try_into().unwrap(), 0);
        while let Ok(count) = reader.read(&mut buffer) {
            if count == 0 {
                break;
            }
            output.extend_from_slice(&buffer[..count]);
            terminal.process(&buffer[..count]);
            let frame = terminal.screen().contents();
            if frame.contains("no loop tasks; add one with `rimz loop add …`") {
                let _ = startup_tx.try_send(());
            }
            if frame.contains("after-build-move") && frame.contains("never fired") {
                let _ = repaint_tx.try_send(());
            }
        }
        output
    });
    let startup = startup_rx.recv_timeout(Duration::from_secs(15));
    builds.record(&env, &builds.second);
    let deadline = Instant::now() + Duration::from_secs(10);
    while executable(pid) != builds.second && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    let moved = executable(pid) == builds.second;
    write_loop_config(
        &env,
        &format!(
            "[tasks.after-build-move]\nroot = {:?}\ncheck = 'true'\nevery = '15m'\n",
            env.project_root
        ),
    );
    let repainted = repaint_rx.recv_timeout(Duration::from_secs(15));
    let exited = child.try_wait().unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    drop(pair.master);
    let output = String::from_utf8_lossy(&reader_thread.join().unwrap()).into_owned();
    assert!(startup.is_ok(), "loop panel never painted: {output}");
    assert!(
        moved,
        "loop panel did not exec the recorded build: {output}"
    );
    assert!(
        repainted.is_ok() && exited.is_none(),
        "loop panel did not repaint: {output}"
    );
}

#[test]
fn loop_wait_workflow_pins_and_delivers_to_live_session() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-live", "feature-loop");
    loop_ok(&env, &["config", "set", "harness.smart_compact", "70%"]);

    let added = loop_ok(
        &env,
        &[
            "loop",
            "add",
            "wait",
            "--wait",
            "@claude",
            "--every",
            "15m",
            "--prompt",
            "next step",
        ],
    );
    assert!(added.contains("pinned to claude session `sess-loop-live`"));
    let instances = read_loop_instances(&env);
    assert_eq!(
        instances.0["wait"]
            .wait
            .as_ref()
            .map(|wait| wait.session.as_str()),
        Some("sess-loop-live")
    );

    loop_ok(&env, &["loop", "run", "wait"]);
    assert_pending_message(&env, "sess-loop-live", "next step");
    assert_eq!(
        env.store().list_pending_messages().unwrap()[0].sender,
        rimz::store::message::MessageSender::Harness {
            notice: rimz::store::message::HarnessNotice::Wait,
        }
    );
    assert_eq!(
        env.store().list_pending_messages().unwrap()[0].auto_compact,
        Some(AutoCompact::Percent(70))
    );
    assert_eq!(last_loop_record(&env).result, LoopRunResult::Delivered);
    assert_eq!(
        last_loop_record(&env).root.as_deref(),
        Some(env.project_root.as_path())
    );
    let list = loop_ok(&env, &["loop", "list", "--all"]);
    let show = loop_ok(&env, &["loop", "show", "wait"]);
    assert!(
        list.lines()
            .any(|line| line == "  loop wait · every 15m → @claude in 1 worktree"),
        "{list}"
    );
    assert!(show.contains("✓ healthy · last run "), "{show}");
}

#[test]
fn emitted_signal_reaches_the_matching_wait_consumer() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let workspace = env.resolve_workspace(&env.project_root);
    env.store()
        .append_event(&rimz::store::event::EventEnvelope::agent_launched(
            workspace.workspace_id,
            &workspace.session_name,
            &AgentKind::new_unchecked("claude"),
            rimz::store::event::AgentLaunchPayload {
                system_prompt_digest: None,
                agent_id: AgentSessionId::from("sess-signal-live"),
                launch_id: None,
                agent_name: "claude".to_owned(),
                agent_name_explicit: true,
                launch: rimz::agents::LaunchParams::default(),
                state: rimz::store::event::AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner: None,
                worktree_path: Some(env.project_root.display().to_string()),
                worktree_branch: Some("feature-signal".to_owned()),
                prompt: None,
                description: None,
            },
        ))
        .unwrap();
    for signal in [
        rimz::agents::LifecycleSignal::Registered,
        rimz::agents::LifecycleSignal::TurnStarted { turn_id: None },
    ] {
        let mut observation = rimz::agents::AgentLifecycleObservation::new(
            Some(AgentSessionId::from("sess-signal-live")),
            signal,
        );
        observation.agent_name = Some("claude".to_owned());
        env.store()
            .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
                session_name: &workspace.session_name,
                agent_kind: AgentKind::new_unchecked("claude"),
                event_name: "test",
                observation: &observation,
                spawned_subagents: &[],
            })
            .unwrap();
    }
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "ci-wait",
            "--wait",
            "@claude",
            "--signal",
            "deploy.finished",
            "--match",
            "outcome=failure",
            "--prompt",
            "Inspect deployment",
        ],
    );

    loop_ok(
        &env,
        &[
            "events",
            "emit",
            "deploy.finished",
            "--json",
            r#"{"outcome":"success"}"#,
        ],
    );
    assert!(read_loop_run_records(&env).is_empty());
    assert!(env.store().list_pending_messages().unwrap().is_empty());

    loop_ok(
        &env,
        &[
            "events",
            "emit",
            "deploy.finished",
            "--json",
            r#"{"outcome":"failure"}"#,
        ],
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while read_loop_run_records(&env).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].result, LoopRunResult::Delivered);
    let signal = records[0].signal.as_ref().expect("signal forensics");
    assert_eq!(signal.name.as_str(), "deploy.finished");
    assert_eq!(signal.payload["outcome"], "failure");
    let message = &env.store().list_pending_messages().unwrap()[0];
    assert_eq!(records[0].message_id.as_ref(), Some(&message.message_id));
    assert_eq!(message.agent_id.as_str(), "sess-signal-live");
    assert_eq!(
        message.sender,
        rimz::store::message::MessageSender::Harness {
            notice: rimz::store::message::HarnessNotice::Signal,
        }
    );
    assert!(
        message.text.contains("Inspect deployment"),
        "{}",
        message.text
    );
    assert!(
        message
            .text
            .starts_with("waited on deploy.finished\nfired [ci-wait]\n"),
        "{}",
        message.text
    );
    assert_eq!(message.text.lines().nth(2), Some("outcome: failure"));
    assert!(message.text.ends_with("\n\nInspect deployment"));

    let show = loop_ok(&env, &["loop", "show", "ci-wait"]);
    assert!(show.contains(" · signal deploy.finished\n"), "{show}");
    assert!(
        show.contains(&format!("message: {}", message.message_id)),
        "{show}"
    );

    loop_ok(&env, &["loop", "disable", "ci-wait"]);
    loop_ok(
        &env,
        &[
            "events",
            "emit",
            "deploy.finished",
            "--json",
            r#"{"outcome":"failure"}"#,
        ],
    );
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(read_loop_run_records(&env).len(), 1);
    assert_eq!(env.store().list_pending_messages().unwrap().len(), 1);

    // A merge-queue rejection reaches its waiter with the reason and the
    // queue's own checks link; the sibling `pr.queued` is recorded skipped.
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "queue-wait",
            "--wait",
            "@claude",
            "--signal",
            "pr.dequeued",
            "--match",
            "branch=feature-signal",
            "--prompt",
            "Fix the queue failure",
        ],
    );
    let queue_checks_url = "https://github.com/org/repo/commit/queue-a/checks";
    for (name, payload) in [
        (
            "pr.queued",
            json!({"branch": "feature-signal", "queued_at": "2026-10-03T12:28:46Z"}),
        ),
        (
            "pr.dequeued",
            json!({
                "branch": "feature-signal", "dequeued_at": "2026-10-03T12:46:03Z",
                "reason": "failed_checks", "queue_checks_url": queue_checks_url,
            }),
        ),
    ] {
        loop_ok(
            &env,
            &[
                "events",
                "emit",
                name,
                "--source",
                "forge",
                "--json",
                &payload.to_string(),
            ],
        );
    }
    wait_for_loop_run_records(&env, "queue-wait SignalSkipped then Delivered", |records| {
        records.len() == 3
    });
    let records = read_loop_run_records(&env);
    let queue_results: Vec<_> = records
        .iter()
        .filter(|record| record.task == "queue-wait")
        .map(|record| {
            let signal = record.signal.as_ref().expect("signal forensics");
            (signal.name.as_str(), record.result)
        })
        .collect();
    assert_eq!(
        queue_results,
        vec![
            ("pr.queued", LoopRunResult::SignalSkipped),
            ("pr.dequeued", LoopRunResult::Delivered),
        ],
        "{records:?}"
    );
    let messages = env.store().list_pending_messages().unwrap();
    assert_eq!(messages.len(), 2, "{messages:?}");
    // Substrings only: the body's layout around these two values is not this test's contract.
    let queue_message = messages
        .iter()
        .find(|message| message.text.contains("Fix the queue failure"))
        .unwrap_or_else(|| panic!("{messages:?}"));
    assert_eq!(queue_message.agent_id.as_str(), "sess-signal-live");
    for value in ["failed_checks", queue_checks_url] {
        assert!(queue_message.text.contains(value), "{}", queue_message.text);
    }
}

#[test]
fn lifecycle_signal_wakes_only_for_the_matching_agent_session() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "planner-session", "feature-planner");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "review-finished",
            "--wait",
            "@claude",
            "--signal",
            "agent.idle",
            "--match",
            "session=reviewer-session",
            "--prompt",
            "review finished",
        ],
    );

    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "planner-session",
            "last_assistant_message": "planning done",
            "worktree_branch": "feature-planner",
        }),
        &env.project_root,
    );
    assert!(read_loop_run_records(&env).is_empty());
    run_hook(
        &env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "planner-session",
            "prompt": "wait for review",
            "worktree_branch": "feature-planner",
        }),
        &env.project_root,
    );

    register_running_agent(&env, "reviewer-session", "feature-reviewer");
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "reviewer-session",
            "last_assistant_message": "review done",
            "worktree_branch": "feature-reviewer",
        }),
        &env.project_root,
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    while !read_loop_run_records(&env)
        .iter()
        .any(|record| record.result == LoopRunResult::Delivered)
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(25));
    }
    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 2, "{records:?}");
    assert_eq!(records[0].result, LoopRunResult::SignalSkipped);
    assert_eq!(
        records[1]
            .signal
            .as_ref()
            .map(|signal| signal.name.as_str()),
        Some("agent.idle")
    );
    assert_pending_message(&env, "planner-session", "review finished");
}

#[test]
fn agent_budget_edits_and_views_use_local_day() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-budget", "feature-budget");
    let agent_id = AgentSessionId::from("sess-budget");
    let kind = AgentKind::new_unchecked("claude");

    for (value, expected_cap, disabled) in [
        ("10", Some(10.0), false),
        ("+5", Some(15.0), false),
        ("off", None, true),
    ] {
        loop_ok(
            &env,
            &["agents", "budget", "@claude", value, "--no-continue"],
        );
        let ledger = rimz::harness::budget::read_ledger(&env.runtime_paths(), &kind, &agent_id)
            .expect("budget ledger");
        assert_eq!(
            (ledger.effective_cap_usd(), ledger.disabled),
            (expected_cap, disabled)
        );
    }

    let mut context = rimz::agents::AgentContext::new("claude", Timestamp::now());
    context.cost = Some(rimz::agents::AgentCost {
        total_cost_usd: Some(50.0),
        ..rimz::agents::AgentCost::default()
    });
    let record = rimz::agents::context::record::AgentContextRecord::new(
        "claude",
        agent_id.as_str(),
        context,
    );
    rimz::store::agent_context::write_record(&env.runtime_paths(), &record)
        .expect("write cost sidecar");
    let mut ledger = BudgetLedger::new("20/day".parse().expect("budget"));
    ledger.day_baseline = Some(DayBaseline {
        date: jiff::civil::date(2026, 6, 1),
        cost_usd: 40.0,
    });
    write_ledger(&env.runtime_paths(), &kind, &agent_id, &ledger).expect("write budget ledger");

    for args in [
        &["agents", "budget", "@claude"][..],
        &["agents", "show", "@claude"][..],
    ] {
        let output = loop_ok(&env, args);
        assert!(output.contains("$10.00"), "rimz {args:?}: {output}");
    }

    // The budget view names its agent the way every other surface does: by the
    // handle the reader can type back, never by the session id behind it.
    let view = loop_ok(&env, &["agents", "budget", "@claude"]);
    assert!(
        view.contains("@claude#main · $10.00 of $20.00 day"),
        "budget view must print the handle: {view}"
    );
    assert!(
        !view.contains("sess-budget"),
        "budget view must not print the session id: {view}"
    );

    let show: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["agents", "show", "@claude", "--json"]))
            .expect("show JSON");
    let budget = &show["agent"]["budget"];
    assert_eq!(budget["cap"], "$20.00/day");
    assert_eq!(budget["spent_usd"], 10.0);
    assert_eq!(budget["parked"], false);
    assert_eq!(budget["park"], serde_json::Value::Null);
}

#[test]
fn agent_budget_plain_reports_json_and_overflow() {
    use assert_cmd::assert::OutputAssertExt;
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-budget", "feature-budget");
    let text = loop_ok(&env, &["agents", "budget", "@claude"]);
    insta::assert_snapshot!(text, @"@claude#main · no cap · set one: rimz agents tune @claude#main budget 5");
    let json = loop_ok(&env, &["agents", "budget", "@claude", "--json"]);
    insta::assert_json_snapshot!(serde_json::from_str::<serde_json::Value>(&json).unwrap(), @r#"
    {
      "agent": "@claude#main",
      "cap_usd": null,
      "parked": false,
      "source": "none",
      "spend_usd": null,
      "window": null
    }
    "#);
    let text = loop_ok(&env, &["agents", "budget", "@claude", "5"]);
    insta::assert_snapshot!(text, @"@claude#main · unknown spend of $5.00 session · not parked · source override · raise: rimz agents tune @claude#main budget +10 · lift: rimz agents tune @claude#main budget off");
    let kind = AgentKind::new_unchecked("claude");
    let id = AgentSessionId::from("sess-budget");
    let mut ledger = rimz::harness::budget::read_ledger(&env.runtime_paths(), &kind, &id).unwrap();
    ledger.parked = Some(rimz::harness::budget::BudgetParkStamp {
        at_cost: 5.0,
        at: Timestamp::now(),
    });
    write_ledger(&env.runtime_paths(), &kind, &id, &ledger).unwrap();
    let json = loop_ok(&env, &["agents", "budget", "@claude", "--json"]);
    insta::assert_json_snapshot!(serde_json::from_str::<serde_json::Value>(&json).unwrap(), @r#"
    {
      "agent": "@claude#main",
      "cap_usd": 5.0,
      "parked": true,
      "source": "override",
      "spend_usd": null,
      "window": "session"
    }
    "#);
    let text = loop_ok(&env, &["agents", "budget", "@claude"]);
    assert!(text.contains(" · parked · source override"));
    loop_ok(
        &env,
        &["agents", "budget", "@claude", "off", "--no-continue"],
    );
    assert!(
        rimz::harness::budget::read_ledger(&env.runtime_paths(), &kind, &id)
            .unwrap()
            .disabled
    );
    ledger.raised_cap_usd = Some(f64::MAX);
    write_ledger(&env.runtime_paths(), &kind, &id, &ledger).unwrap();
    env.rimz()
        .args(["agents", "budget", "@claude", &format!("+{}", f64::MAX)])
        .assert()
        .failure()
        .stderr(predicates::str::contains("budget raise is too large"));
    assert_eq!(
        rimz::harness::budget::read_ledger(&env.runtime_paths(), &kind, &id).unwrap(),
        ledger
    );
}

#[test]
fn loop_spawn_controls_persist_render_and_gate_daily_budget() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    loop_ok(&env, &["config", "init"]);
    write_loop_config(&env, "default-timeout = \"3h\"\n");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "bounded",
            "--agent",
            "claude",
            "--prompt",
            "bounded work",
            "--every",
            "15m",
            "--budget",
            "$5",
            "--budget-per-day",
            "20",
            "--verify",
            "cargo xtask gate",
            "--max-attempts",
            "4",
            "--max-strikes",
            "5",
        ],
    );

    let text = std::fs::read_to_string(loop_config_path(&env)).expect("read loop config");
    assert!(text.contains("budget = \"$5.00\"") && text.contains("budget-per-day = \"$20.00\""));
    let config: LoopConfig = toml::from_str(&text).expect("parse loop config");
    let task = &config.tasks.0["bounded"];
    assert_eq!(task.budget.as_deref(), Some("$5.00"));
    assert_eq!(task.budget_per_day.as_deref(), Some("$20.00"));
    assert_eq!(task.verify.as_deref(), Some("cargo xtask gate"));
    assert_eq!((task.max_attempts, task.max_strikes), (Some(4), Some(5)));
    let show = loop_ok(&env, &["loop", "show", "bounded"]);
    assert!(
        show.contains("\n  verify:  cargo xtask gate (up to 4 attempts)\n")
            && show.contains("\n  timeout: 3h (default)\n"),
        "{show}"
    );

    let spent = (0..4)
        .map(|_| {
            let mut record =
                LoopRunRecord::new("bounded", LoopRunResult::Completed, LoopRunMode::Manual, 1);
            record.cost_usd = Some(5.0);
            record
        })
        .collect::<Vec<_>>();
    write_loop_run_records(&env, &spent);
    loop_ok(&env, &["loop", "run", "bounded"]);
    let skipped = last_loop_record(&env);
    assert_eq!(skipped.result, LoopRunResult::BudgetSkipped);
    assert!(
        skipped
            .error
            .as_deref()
            .is_some_and(|error| error.contains("daily budget"))
    );
    assert!(
        std::fs::read_to_string(loop_config_path(&env))
            .expect("read loop config")
            .contains("[tasks.bounded]")
    );

    // The fleet budget is a scope gate, which R3's ladder reads before the daily budget.
    loop_ok(&env, &["config", "set", "harness.budget", "0/day"]);
    let marker = env.project_root.join("scope-check-ran");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "scope-bounded",
            "--agent",
            "claude",
            "--prompt",
            "bounded work",
            "--check",
            &format!("touch {}", marker.display()),
            "--on",
            "success",
            "--every",
            "15m",
        ],
    );
    loop_ok(&env, &["loop", "run", "scope-bounded"]);
    let skipped = last_loop_record(&env);
    assert_eq!(skipped.result, LoopRunResult::BudgetSkipped);
    assert!(
        skipped
            .error
            .as_deref()
            .is_some_and(|error| error.contains("fleet budget exhausted"))
    );
    assert!(!marker.exists(), "scope caps must gate before the check");
}

#[test]
fn loop_project_trust_controls_visibility_execution_and_precedence() {
    let env = Env::new();
    env.record(&env.project_root);
    crate::common::room::seed_sidebar_heartbeat(
        &env.runtime_paths(),
        MuxName::Tmux,
        "rimz-test",
        "sidebar",
    );
    write_loop_config(
        &env,
        &format!(
            "[tasks.shared]\ncheck = \"printf machine\"\nroot = \"{}\"\nevery = \"15m\"\n\
             [tasks.profile-ref]\nagent = \"repo-agent\"\nprompt = \"work\"\nroot = \"{}\"\nevery = \"15m\"\n",
            env.project_root.display(),
            env.project_root.display()
        ),
    );
    write_project_config(
        &env,
        "[profiles.repo-agent]\nagent = \"claude\"\n\
         [tasks.repo-check]\ncheck = \"true\"\nevery = \"15m\"\n\
         [tasks.shared]\ncheck = \"printf project\"\nevery = \"15m\"\n",
    );

    let list = loop_ok(&env, &["loop", "list"]);
    assert!(
        list.contains("repo-check")
            && list.contains("blocked · project untrusted")
            && list.contains("NEEDS YOU")
            && list.contains("rimz trust grant"),
        "{list}"
    );
    let (_stdout, error) = loop_fail(&env, &["loop", "run", "repo-check"]);
    assert!(
        error.contains("loop task `repo-check` is blocked — project trust is untrusted")
            && error.contains("rimz trust grant"),
        "{error}"
    );
    let (_stdout, error) = loop_fail(&env, &["loop", "run", "profile-ref"]);
    assert!(
        error.contains("profiles are configured")
            && error.contains("untrusted")
            && error.contains("rimz trust grant"),
        "the loop launch resolver must refuse an untrusted project profile: {error}"
    );

    loop_ok(&env, &["loop", "run", "shared"]);
    let report = wait_for_loop_report(&env, "shared");
    let journal = loop_run_journal(&env, &env.project_root, &report);
    assert!(journal.contains("machine"), "{journal}");
    grant_project_trust(&env);
    let list = loop_ok(&env, &["loop", "list"]);
    assert!(
        list.contains("repo-check") && list.contains("off · repo task, enable here to run"),
        "{list}"
    );
    let runs_before = read_loop_run_records(&env).len();
    loop_ok(&env, &["loop", "run", "shared"]);
    assert_eq!(read_loop_run_records(&env).len(), runs_before);
    loop_ok(&env, &["loop", "enable", "shared"]);
    loop_ok(&env, &["loop", "run", "shared"]);
    let report = wait_for_loop_report_after(&env, "shared", runs_before);
    let journal = loop_run_journal(&env, &env.project_root, &report);
    assert!(journal.contains("project"), "{journal}");
}

#[test]
fn project_task_enablement_is_scoped_by_project_root() {
    let env = Env::new();
    let other = env.home_root.join("other-project");
    std::fs::create_dir_all(&other).expect("other project");
    write_project_config_at(
        &env.project_root,
        "[tasks.nightly]\ncheck = \"true\"\nevery = \"15m\"\n",
    );
    write_project_config_at(
        &other,
        "[tasks.nightly]\ncheck = \"true\"\nevery = \"15m\"\n",
    );
    loop_ok_root(&env, &env.project_root, &["trust", "grant"]);
    loop_ok_root(&env, &other, &["trust", "grant"]);

    assert!(
        loop_ok_root(&env, &env.project_root, &["loop", "list"])
            .contains("off · repo task, enable here to run")
    );
    assert!(
        loop_ok_root(&env, &other, &["loop", "list"])
            .contains("off · repo task, enable here to run")
    );

    loop_ok_root(&env, &env.project_root, &["loop", "enable", "nightly"]);
    assert!(
        !loop_ok_root(&env, &env.project_root, &["loop", "list"])
            .contains("off · repo task, enable here to run")
    );
    assert!(
        loop_ok_root(&env, &other, &["loop", "list"])
            .contains("off · repo task, enable here to run")
    );

    let arming = read_loop_arming(&env);
    assert!(arming[&project_task_key(&env.project_root, "nightly")].enabled);
    assert!(!arming.contains_key(&project_task_key(&other, "nightly")));
}

#[test]
fn trusted_project_task_edits_repin_trust() {
    let env = Env::new();
    env.record(&env.project_root);
    crate::common::room::seed_sidebar_heartbeat(
        &env.runtime_paths(),
        MuxName::Tmux,
        "rimz-test",
        "sidebar",
    );
    write_project_config(&env, "[tasks.first]\ncheck = \"true\"\nevery = \"15m\"\n");
    grant_project_trust(&env);
    let dismissal = env
        .rimz_home()
        .join("trust")
        .join(rimz::WorkspaceId::from_project_root(&env.project_root).as_str())
        .join("folder-trust-prompt.toml");
    let declined = "dismissed_kinds = ['codex']\ndismissed_at = '2026-01-01T00:00:00Z'\n";
    std::fs::write(&dismissal, declined).unwrap();

    let added = loop_ok(
        &env,
        &[
            "loop",
            "add",
            "second",
            "--project",
            "--check",
            "true",
            "--every",
            "15m",
        ],
    );
    assert!(
        added.contains("trust: granted — task enabled and ready to fire")
            && !added.contains("stay inert")
            && !added.contains("grant trust now"),
        "{added}"
    );
    assert!(loop_ok(&env, &["trust"]).contains("trust: trusted"));
    assert!(read_loop_arming(&env)[&project_task_key(&env.project_root, "second")].enabled);
    loop_ok(&env, &["loop", "run", "second"]);

    let renamed = loop_ok(&env, &["loop", "rename", "second", "renamed"]);
    assert!(renamed.contains("trust: kept"), "{renamed}");
    assert!(loop_ok(&env, &["trust"]).contains("trust: trusted"));
    assert!(read_loop_arming(&env)[&project_task_key(&env.project_root, "renamed")].enabled);
    loop_ok(&env, &["loop", "run", "renamed"]);

    let removed = loop_ok(&env, &["loop", "remove", "renamed"]);
    assert!(removed.contains("trust: kept"), "{removed}");
    assert!(loop_ok(&env, &["trust"]).contains("trust: trusted"));
    loop_ok(&env, &["loop", "run", "first"]);
    assert_eq!(
        std::fs::read_to_string(dismissal).ok().as_deref(),
        Some(declined)
    );
}

#[test]
fn first_project_task_grants_fresh_config() {
    let env = Env::new();
    assert!(!env.project_root.join(".rimz/config.toml").exists());

    let added = loop_ok(
        &env,
        &[
            "loop",
            "add",
            "fresh",
            "--project",
            "--check",
            "true",
            "--every",
            "15m",
        ],
    );
    assert!(
        added.contains("trust: granted — task enabled and ready to fire"),
        "{added}"
    );
    assert!(loop_ok(&env, &["trust"]).contains("trust: trusted"));
    let arming = read_loop_arming(&env);
    assert!(
        arming
            .get(&project_task_key(&env.project_root, "fresh"))
            .is_some_and(|entry| entry.enabled),
        "{arming:?}"
    );

    // A root whose `.rimz` is the RimZ home holds the machine config, never
    // a project layer, so a project task edit there refuses untouched.
    let config = env.project_root.join(".rimz/config.toml");
    let before = std::fs::read_to_string(&config).unwrap();
    let output = env
        .rimz()
        .env("RIMZ_HOME", env.project_root.join(".rimz"))
        .args([
            "loop",
            "add",
            "home",
            "--project",
            "--check",
            "true",
            "--every",
            "1h",
        ])
        .output()
        .expect("rimz loop add");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("holds the RimZ home"), "{stderr}");
    assert_eq!(std::fs::read_to_string(&config).unwrap(), before);
}

#[test]
fn project_task_edit_does_not_grant_foreign_change() {
    let env = Env::new();
    write_project_config(
        &env,
        "[tasks.remove-me]\ncheck = \"true\"\nevery = \"15m\"\n",
    );
    grant_project_trust(&env);
    write_project_config(
        &env,
        "[tasks.remove-me]\ncheck = \"true\"\nevery = \"15m\"\n\n\
         [[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks codex\"\n",
    );

    let removed = loop_ok(&env, &["loop", "remove", "remove-me"]);
    assert!(
        removed
            .contains("trust: stale — project tasks stay inert until you run `rimz trust grant`")
            && !removed.contains("trust: kept"),
        "{removed}"
    );
    assert!(loop_ok(&env, &["trust"]).contains("trust: stale"));
}

#[test]
fn loop_task_storage_policy_and_manual_fire_preserve_one_shots() {
    let env = Env::new();
    env.record(&env.project_root);
    write_loop_config(
        &env,
        &format!(
            "# keep unrelated task comment\n[tasks.keep]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"1h\"\n",
            env.project_root.display()
        ),
    );

    loop_ok(
        &env,
        &["loop", "add", "probe", "--check", "printf ok", "--in", "5m"],
    );
    assert!(read_loop_instances(&env).0.contains_key("probe"));
    assert!(
        !std::fs::read_to_string(loop_config_path(&env))
            .unwrap()
            .contains("[tasks.probe]")
    );
    loop_ok(&env, &["loop", "fire", "probe"]);
    assert!(read_loop_instances(&env).0.contains_key("probe"));
    loop_ok(&env, &["loop", "run", "probe"]);
    assert!(!read_loop_instances(&env).0.contains_key("probe"));

    loop_ok(
        &env,
        &[
            "loop", "add", "morning", "--check", "true", "--every", "weekday", "--at", "07:00",
        ],
    );
    assert!(!read_loop_instances(&env).0.contains_key("morning"));
    assert!(
        std::fs::read_to_string(loop_config_path(&env))
            .unwrap()
            .contains("[tasks.morning]")
    );

    loop_ok(
        &env,
        &["loop", "add", "swap", "--check", "true", "--every", "15m"],
    );
    loop_ok(
        &env,
        &["loop", "add", "swap", "--check", "true", "--in", "5m"],
    );
    assert!(read_loop_instances(&env).0.contains_key("swap"));
    assert!(
        !std::fs::read_to_string(loop_config_path(&env))
            .unwrap()
            .contains("[tasks.swap]")
    );
    loop_ok(
        &env,
        &[
            "loop", "add", "swap", "--check", "true", "--every", "weekday", "--at", "07:00",
        ],
    );
    let text = std::fs::read_to_string(loop_config_path(&env)).expect("read loop config");
    assert!(
        !read_loop_instances(&env).0.contains_key("swap")
            && text.contains("[tasks.swap]")
            && text.contains("# keep unrelated task comment"),
        "{text}"
    );

    let (_stdout, error) = loop_fail(
        &env,
        &[
            "loop",
            "add",
            "project-once",
            "--project",
            "--check",
            "true",
            "--in",
            "5m",
        ],
    );
    assert!(
        error.contains("need a trigger") && error.contains("--every, --cron, or --signal"),
        "{error}"
    );
}

#[test]
fn loop_multi_name_enable_disable_resolves_before_writing() {
    let env = Env::new();
    for name in ["first", "second"] {
        loop_ok(
            &env,
            &["loop", "add", name, "--check", "true", "--every", "15m"],
        );
    }
    for verb in ["disable", "enable"] {
        let before = read_loop_arming(&env);
        let (out, error) = loop_fail(&env, &["loop", verb, "first", "missing"]);
        assert!(
            error.contains("no loop task named `missing`; see `rimz loop list`"),
            "{error}"
        );
        assert!(out.is_empty(), "{out}");
        assert_eq!(read_loop_arming(&env), before);
        let output = loop_ok(&env, &["loop", verb, "first", "second"]);
        for name in ["first", "second"] {
            assert!(
                output.contains(&format!("loop `{name}`: {verb}d")),
                "{output}"
            );
            assert_eq!(
                read_loop_arming(&env)[&machine_task_key(name)].enabled,
                verb == "enable"
            );
        }
        loop_fail(&env, &["loop", verb, "first", "--all"]);
        loop_fail(&env, &["loop", verb]);
    }
}

#[test]
fn loop_multi_name_remove_reports_each_outcome() {
    let env = Env::new();
    for name in ["first", "second"] {
        loop_ok(
            &env,
            &["loop", "add", name, "--check", "true", "--every", "15m"],
        );
    }
    let output = env
        .rimz()
        .args(["loop", "remove", "first", "missing", "second", "first"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        text,
        "removed loop task `first`\nno loop task named `missing`\nremoved loop task `second`\nno loop task named `first`\n"
    );
    assert!(output.status.success());
    assert!(loop_ok(&env, &["loop", "list"]).contains("no loop tasks"));
    loop_fail(&env, &["loop", "remove"]);
}

#[test]
fn loop_add_new_task_follows_template_and_remove_restores_it() {
    let template = rimz::config::MachineConfig::template_loop();
    let mut rendered = Vec::new();
    let mut expected = Vec::new();
    for initialize in [false, true] {
        let env = Env::new();
        if initialize {
            loop_ok(&env, &["config", "init"]);
        }
        loop_ok(
            &env,
            &["loop", "add", "first", "--check", "true", "--every", "15m"],
        );
        rendered.push(std::fs::read_to_string(loop_config_path(&env)).unwrap());
        expected.push(format!(
            "{template}\n[tasks.first]\ncheck = \"true\"\nroot = {:?}\nevery = \"15m\"\n",
            env.project_root
        ));
        loop_ok(&env, &["loop", "remove", "first"]);
        rendered.push(std::fs::read_to_string(loop_config_path(&env)).unwrap());
        expected.push(template.to_owned());
    }
    assert_eq!(rendered, expected);
}

#[test]
fn loop_add_keeps_commented_default_timeout_at_root() {
    let env = Env::new();
    loop_ok(
        &env,
        &["loop", "add", "first", "--check", "true", "--every", "15m"],
    );
    let path = loop_config_path(&env);
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        path,
        text.replace("# default-timeout = \"2h\"", "default-timeout = \"2h\""),
    )
    .unwrap();
    assert_eq!(
        loop_ok(&env, &["config", "get", "loop.default-timeout"]),
        "2h\n"
    );
}

#[test]
fn loop_add_new_project_task_follows_comments_without_a_leading_blank_line() {
    let mut rendered = Vec::new();
    let mut expected = Vec::new();
    for seed in ["", "# Project loops\n# Keep settings above the tasks\n"] {
        let env = Env::new();
        if !seed.is_empty() {
            write_project_config(&env, seed);
        }
        loop_ok(
            &env,
            &[
                "loop",
                "add",
                "one",
                "--project",
                "--check",
                "true",
                "--every",
                "15m",
            ],
        );
        rendered.push(std::fs::read_to_string(env.project_root.join(".rimz/config.toml")).unwrap());
        let separator = if seed.is_empty() { "" } else { "\n" };
        expected.push(format!(
            "{seed}{separator}[tasks.one]\ncheck = \"true\"\nevery = \"15m\"\n"
        ));
    }
    assert_eq!(rendered, expected);
}

#[test]
fn loop_add_new_task_follows_root_settings_and_trailing_comments() {
    let env = Env::new();
    let seed = "default-timeout = \"2h\"\n\n# Scheduled loops\n# Keep the default above\n";
    write_loop_config(&env, seed);
    loop_ok(
        &env,
        &["loop", "add", "one", "--check", "true", "--every", "15m"],
    );
    let text = std::fs::read_to_string(loop_config_path(&env)).unwrap();
    assert_eq!(
        text,
        format!(
            "{seed}\n[tasks.one]\ncheck = \"true\"\nroot = {:?}\nevery = \"15m\"\n",
            env.project_root
        )
    );
    assert_eq!(
        toml::from_str::<LoopConfig>(&text)
            .unwrap()
            .default_timeout
            .as_deref(),
        Some("2h")
    );
}

#[test]
fn loop_add_new_task_keeps_preceding_task_comments_and_following_sections() {
    let comments = "# every = \"30m\"   # faster during release week\n";
    let mut rendered = Vec::new();
    let mut expected = Vec::new();
    for project in [false, true] {
        for following_section in [false, true] {
            let env = Env::new();
            let root = if project {
                String::new()
            } else {
                format!("root = {:?}\n", env.project_root)
            };
            let first = format!("[tasks.one]\n{root}check = \"true\"\nevery = \"1h\"\n");
            let next = match (following_section, project) {
                (false, _) => "",
                (true, false) => "\n# Throttle settings\n[throttle]\nstagger = \"10s\"\n",
                (true, true) => "\n# Agent settings\n[agents]\n",
            };
            let seed = format!("{first}{comments}{next}");
            let mut args = vec!["loop", "add", "two", "--check", "false", "--every", "2h"];
            let path = if project {
                write_project_config(&env, &seed);
                args.push("--project");
                env.project_root.join(".rimz/config.toml")
            } else {
                write_loop_config(&env, &seed);
                loop_config_path(&env)
            };
            loop_ok(&env, &args);
            rendered.push(std::fs::read_to_string(path).unwrap());
            expected.push(format!(
                "{first}{comments}\n[tasks.two]\ncheck = \"false\"\n{root}every = \"2h\"\n{next}"
            ));
        }
    }
    assert_eq!(rendered, expected);
}

#[test]
fn loop_add_replacing_first_machine_task_keeps_header_comments() {
    let env = Env::new();
    let heading = "# Machine loops\n\n# One note\n[tasks.one] # header note\n";
    let next = format!(
        "\n# Two note\n[tasks.two]\nroot = {:?}\ncheck = \"true\"\nevery = \"7d\"\n",
        env.project_root
    );
    write_loop_config(
        &env,
        &format!(
            "{heading}root = {:?}\ncheck = \"true\"\nevery = \"1h\"\n{next}",
            env.project_root
        ),
    );

    loop_ok(
        &env,
        &["loop", "add", "one", "--check", "false", "--every", "2h"],
    );
    assert_eq!(
        std::fs::read_to_string(loop_config_path(&env)).unwrap(),
        format!(
            "{heading}check = \"false\"\nroot = {:?}\nevery = \"2h\"\n{next}",
            env.project_root
        )
    );
}

#[test]
fn loop_add_replacing_first_project_task_keeps_header_comments() {
    let env = Env::new();
    let heading = "# Project loops\n\n# One note\n[tasks.one] # header note\n";
    let next = "\n# Two note\n[tasks.two]\ncheck = \"true\"\nevery = \"7d\"\n";
    write_project_config(
        &env,
        &format!("{heading}check = \"true\"\nevery = \"1h\"\n{next}"),
    );

    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "one",
            "--project",
            "--check",
            "false",
            "--every",
            "2h",
        ],
    );
    assert_eq!(
        std::fs::read_to_string(env.project_root.join(".rimz/config.toml")).unwrap(),
        format!("{heading}check = \"false\"\nevery = \"2h\"\n{next}")
    );
}

#[test]
fn loop_add_new_task_keeps_explicit_tasks_header_comments() {
    let env = Env::new();
    let original = format!(
        "# lead\n[tasks]\n\n# One note\n[tasks.one]\n\
         root = {:?}\ncheck = \"true\"\nevery = \"1h\"\n",
        env.project_root
    );
    write_loop_config(&env, &original);

    loop_ok(
        &env,
        &["loop", "add", "two", "--check", "false", "--every", "2h"],
    );
    assert_eq!(
        std::fs::read_to_string(loop_config_path(&env)).unwrap(),
        format!(
            "{original}\n[tasks.two]\ncheck = \"false\"\nroot = {:?}\nevery = \"2h\"\n",
            env.project_root
        )
    );
}

#[test]
fn loop_add_replacing_task_keeps_explicit_tasks_header_comments() {
    let env = Env::new();
    let heading = "# lead\n[tasks]\n\n# One note\n[tasks.one]\n";
    write_loop_config(
        &env,
        &format!(
            "{heading}root = {:?}\ncheck = \"true\"\nevery = \"1h\"\n",
            env.project_root
        ),
    );

    loop_ok(
        &env,
        &["loop", "add", "one", "--check", "false", "--every", "2h"],
    );
    assert_eq!(
        std::fs::read_to_string(loop_config_path(&env)).unwrap(),
        format!(
            "{heading}check = \"false\"\nroot = {:?}\nevery = \"2h\"\n",
            env.project_root
        )
    );
}

#[test]
fn loop_remove_first_task_keeps_template_comments() {
    let env = Env::new();
    write_loop_config(
        &env,
        "# Supervised scheduled turns without a task-specific timeout use two hours.\n\
         # default-timeout = \"2h\"\n\n\
         [tasks.one]\ncheck = \"true\"\nevery = \"1h\"\n\n\
         [tasks.two]\ncheck = \"true\"\nevery = \"1w\"\n",
    );

    let output = loop_ok(&env, &["loop", "remove", "one"]);
    assert_eq!(output, "removed loop task `one`\n");
    assert_eq!(
        std::fs::read_to_string(loop_config_path(&env)).unwrap(),
        "# Supervised scheduled turns without a task-specific timeout use two hours.\n\
         # default-timeout = \"2h\"\n\n\
         [tasks.two]\ncheck = \"true\"\nevery = \"1w\"\n"
    );
}

#[test]
fn loop_remove_last_task_keeps_preceding_task_comments() {
    let env = Env::new();
    write_loop_config(
        &env,
        "[tasks.one]\ncheck = \"true\"\nevery = \"1h\"\n\
         # every = \"30m\"   # faster cadence during release week\n\
         # Weekly cleanup\n[tasks.two]\ncheck = \"true\"\nevery = \"1w\"\n",
    );

    let output = loop_ok(&env, &["loop", "remove", "two"]);
    assert_eq!(output, "removed loop task `two`\n");
    assert_eq!(
        std::fs::read_to_string(loop_config_path(&env)).unwrap(),
        "[tasks.one]\ncheck = \"true\"\nevery = \"1h\"\n\
         # every = \"30m\"   # faster cadence during release week\n\
         # Weekly cleanup\n"
    );
}

#[test]
fn loop_enable_disable_pause_workflow() {
    let env = Env::new();
    env.record(&env.project_root);
    loop_ok(
        &env,
        &["loop", "add", "probe", "--check", "true", "--every", "15m"],
    );
    let (_stdout, error) = loop_fail(&env, &["loop", "pause", "probe"]);
    assert!(
        error.contains("--for") && error.contains("required"),
        "{error}"
    );
    assert!(loop_ok(&env, &["loop", "disable", "probe"]).contains("disabled"));
    assert!(loop_ok(&env, &["loop", "list"]).contains("off"));
    let fired = loop_ok(&env, &["loop", "fire", "probe"]);
    assert!(fired.contains("task is disabled; firing anyway") && fired.contains("launched"));
    wait_for_loop_report(&env, "probe");
    assert!(loop_ok(&env, &["loop", "enable", "probe"]).contains("enabled"));

    let timed = loop_ok(&env, &["loop", "pause", "probe", "--for", "2h"]);
    assert!(
        timed.contains("resumes in 2h")
            && read_loop_arming(&env)
                .get(&machine_task_key("probe"))
                .is_some_and(|entry| entry.pause_until.is_some()),
        "{timed}"
    );
    assert!(loop_ok(&env, &["loop", "show", "probe"]).contains("· paused"));
    let fired = loop_ok(&env, &["loop", "fire", "probe"]);
    assert!(fired.contains("task is paused; firing anyway") && fired.contains("launched"));
    wait_for_loop_report_after(&env, "probe", 2);
    assert!(loop_ok(&env, &["loop", "enable", "probe"]).contains("enabled"));
    assert!(!loop_ok(&env, &["loop", "list"]).contains("paused, resumes"));
    assert!(loop_ok(&env, &["loop", "enable", "probe"]).contains("already enabled"));

    let key = machine_task_key("probe");
    let prior_enable = Timestamp::now() - SignedDuration::from_hours(3);
    let expired_pause_end = Timestamp::now() - SignedDuration::from_mins(1);
    let expired = BTreeMap::from([(
        key.clone(),
        Arming {
            enabled: true,
            at: Some(prior_enable),
            pause_until: Some(expired_pause_end),
            strikes: None,
        },
    )]);
    std::fs::write(
        loop_arming_path(&env),
        serde_json::to_vec(&expired).expect("serialize expired pause"),
    )
    .expect("write expired pause");
    std::fs::write(
        loop_strikes_path(&env),
        serde_json::to_vec(&BTreeMap::from([(key.clone(), 2_u32)]))
            .expect("serialize loop strikes"),
    )
    .expect("write loop strikes");
    assert!(loop_ok(&env, &["loop", "enable", "probe"]).contains("already enabled"));
    let enabled = read_loop_arming(&env)[&key];
    assert_eq!(enabled.at, Some(prior_enable));
    assert_eq!(enabled.pause_until, Some(expired_pause_end));
    assert!(!read_loop_strikes(&env).contains_key(&key));

    loop_ok(
        &env,
        &["loop", "add", "second", "--check", "true", "--every", "15m"],
    );
    let disabled = loop_ok(&env, &["loop", "disable", "--all"]);
    assert!(
        disabled.contains("loop `probe`: disabled") && disabled.contains("loop `second`: disabled"),
        "{disabled}"
    );
    let enabled = loop_ok(&env, &["loop", "enable", "--all"]);
    assert!(
        enabled.contains("loop `probe`: enabled") && enabled.contains("loop `second`: enabled"),
        "{enabled}"
    );
}

#[test]
fn loop_repeated_failures_auto_disable_notify_once_and_enable() {
    let env = Env::new();
    env.record(&env.project_root);
    let notify_log = env.project_root.join("loop-disabled-notify.log");
    let config_path = env.rimz_home().join("config.toml");
    std::fs::create_dir_all(config_path.parent().expect("config parent")).expect("mkdir config");
    std::fs::write(
        config_path,
        format!(
            "[notifications]\ncommand = '''printf '%s|%s\\n' \"$RIMZ_NOTIFY_KIND\" \"$RIMZ_NOTIFY_TITLE\" >> '{}' '''\n",
            notify_log.display()
        ),
    )
    .expect("write notification config");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "watchdog",
            "--every",
            "15m",
            "--check",
            "printf broken; exit 1",
        ],
    );

    // Each run's report strikes; the third strikes the task out.
    for _ in 0..3 {
        let before = read_loop_run_records(&env).len();
        loop_ok(&env, &["loop", "run", "watchdog"]);
        let report = wait_for_loop_report_after(&env, "watchdog", before);
        assert_eq!(report.result, LoopRunResult::Failed);
    }
    assert_eq!(
        read_loop_arming(&env)
            .get(&machine_task_key("watchdog"))
            .and_then(|arming| arming.strikes),
        Some(3)
    );

    let before = read_loop_run_records(&env).len();
    let fire = loop_ok(&env, &["loop", "fire", "watchdog"]);
    assert!(fire.contains("task is disabled; firing anyway"), "{fire}");
    wait_for_loop_report_after(&env, "watchdog", before);
    assert_eq!(
        read_loop_strikes(&env).get(&machine_task_key("watchdog")),
        Some(&4)
    );
    assert_eq!(
        read_loop_arming(&env)
            .get(&machine_task_key("watchdog"))
            .and_then(|arming| arming.strikes),
        Some(3),
        "manual fire must not replace an existing disable"
    );

    let deadline = Instant::now() + Duration::from_secs(2);
    let notification = loop {
        let text = std::fs::read_to_string(&notify_log).unwrap_or_default();
        if text.contains("loop_disabled|RimZ: loop watchdog disabled") {
            break text;
        }
        assert!(Instant::now() < deadline, "notification missing: {text}");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(notification.lines().count(), 1, "{notification}");

    loop_ok(&env, &["loop", "enable", "watchdog"]);
    assert!(!read_loop_strikes(&env).contains_key(&machine_task_key("watchdog")));
    assert!(
        read_loop_arming(&env)
            .get(&machine_task_key("watchdog"))
            .is_some_and(|arming| arming.enabled
                && arming.pause_until.is_none()
                && arming.strikes.is_none())
    );
}

#[test]
fn loop_task_mutations_move_and_clear_overlays() {
    let env = Env::new();
    env.record(&env.project_root);
    write_loop_config(
        &env,
        &format!(
            "# keep unrelated task comment\n[tasks.keep]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"1h\"\n\
             [tasks.old]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"15m\"\n",
            env.project_root.display(),
            env.project_root.display()
        ),
    );
    loop_ok(&env, &["loop", "pause", "old", "--for", "2h"]);
    std::fs::write(loop_strikes_path(&env), r#"{"machine::old":2}"#).expect("write loop strikes");
    loop_ok(&env, &["loop", "rename", "old", "new"]);
    let text = std::fs::read_to_string(loop_config_path(&env)).expect("read loop config");
    assert!(
        text.contains("# keep unrelated task comment")
            && text.contains("[tasks.new]")
            && !text.contains("[tasks.old]")
    );
    assert!(read_loop_arming(&env).contains_key(&machine_task_key("new")));
    assert_eq!(
        read_loop_strikes(&env).get(&machine_task_key("new")),
        Some(&2)
    );

    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "old-state",
            "--check",
            "true",
            "--at",
            "07:00",
        ],
    );
    loop_ok(&env, &["loop", "pause", "old-state", "--for", "2h"]);
    loop_ok(&env, &["loop", "rename", "old-state", "new-state"]);
    let instances = read_loop_instances(&env);
    assert!(
        !instances.0.contains_key("old-state")
            && instances.0.contains_key("new-state")
            && read_loop_arming(&env)
                .contains_key(&project_task_key(&env.project_root, "new-state"))
    );

    loop_ok(&env, &["loop", "remove", "new"]);
    assert!(!read_loop_arming(&env).contains_key(&machine_task_key("new")));
    assert!(!read_loop_strikes(&env).contains_key(&machine_task_key("new")));
    loop_ok(
        &env,
        &["loop", "add", "swap", "--check", "true", "--every", "15m"],
    );
    loop_ok(&env, &["loop", "pause", "swap", "--for", "2h"]);
    let replaced = loop_ok(
        &env,
        &["loop", "add", "swap", "--check", "true", "--every", "30m"],
    );
    assert!(
        replaced.contains("arming: reset")
            && !read_loop_arming(&env).contains_key(&machine_task_key("swap"))
    );
    assert!(
        std::fs::read_to_string(loop_config_path(&env))
            .unwrap()
            .contains("# keep unrelated task comment")
    );
}

#[test]
fn loop_rename_moves_live_workflow_runs() {
    let env = Env::new();
    env.record(&env.project_root);
    write_loop_config(
        &env,
        &format!(
            "[tasks.old]\nworkflow = \"review\"\nroot = {:?}\nevery = \"15m\"\n",
            env.project_root
        ),
    );
    let paths = env.state_path_for(&env.project_root);
    let records: Vec<(rimz::store::workflow::RunRecord, Vec<u8>)> = {
        let guard = rimz::disk::lock::WorkspaceLock::acquire(&paths.workspace_lock).unwrap();
        [
            ("parked", Some("old")),
            ("active", Some("old")),
            ("done", Some("old")),
            ("failed", Some("old")),
            ("canceled", Some("old")),
            ("active", Some("other")),
            ("active", None),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (state, task))| {
            let record: rimz::store::workflow::RunRecord = serde_json::from_value(json!({
                "version":rimz::store::workflow::VERSION, "id":rimz::ids::WorkflowRunId::new(), "workflow":"review",
                "task":task, "spec_dir":"/definitions/review", "spec_digest":"abc",
                "prompt":null, "scope":{"kind":"worktree","path":env.project_root.join(index.to_string())},
                "project_root":env.project_root, "created_at":"2026-01-01T00:00:00Z",
                "state":state, "park":if state == "parked" {
                    json!({"reason":"user","text":null,"at":"2026-01-01T00:00:00Z"})
                } else { json!(null) },
                "node":"pr", "entered_at":"2026-01-01T00:00:00Z", "input":null,
                "key":null, "recheck_at":null, "wait":null, "visits":{}, "visit_grant":0,
                "deadline":null, "deadline_fired":false, "next_seq":1, "attempt":null,
                "prev_attempt":null, "terminal_at":if matches!(state, "done" | "failed" | "canceled") {
                    json!("2026-01-01T00:00:00Z")
                } else { json!(null) }
            })).unwrap();
            rimz::store::workflow::initialize(&paths, &record, b"spec", &guard).unwrap();
            let bytes = std::fs::read(paths.workflow(&record.id).record).unwrap();
            (record, bytes)
        })
        .collect()
    };

    let receipt = loop_ok(&env, &["loop", "rename", "old", "new"]);
    assert_eq!(receipt.trim(), "renamed loop task `old` to `new`");
    for (mut expected, bytes) in records.clone() {
        let run_paths = paths.workflow(&expected.id);
        if expected.task.as_deref() == Some("old") && !expected.state.is_terminal() {
            expected.task = Some("new".into());
        } else {
            assert_eq!(std::fs::read(&run_paths.record).unwrap(), bytes);
        }
        assert_eq!(rimz::store::workflow::load(&run_paths).unwrap(), expected);
    }
    let shown = loop_ok(&env, &["loop", "show", "new"]);
    assert!(shown.contains("RUNS"), "{shown}");
    let json: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", "new", "--json"])).unwrap();
    for (index, (record, _)) in records[..2].iter().enumerate() {
        let address = format!("review#{index}");
        assert!(shown.contains(&address), "{shown}");
        assert!(
            json["workflow_runs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| { row["id"] == json!(record.id) && row["address"] == address }),
            "{json:#}"
        );
    }
    let listed = loop_ok(&env, &["loop", "list"]);
    assert!(listed.contains("NEEDS YOU"), "{listed}");
    assert!(
        listed.split_whitespace().any(|word| word == "new"),
        "{listed}"
    );
    assert!(
        !listed.split_whitespace().any(|word| word == "old"),
        "{listed}"
    );
    let (_, error) = loop_fail(&env, &["loop", "show", "old"]);
    assert!(error.contains("no loop task named `old`"), "{error}");
}

#[test]
fn loop_rename_without_a_room_does_not_create_state() {
    for definition in [
        "check = 'true'\nevery = '15m'",
        "workflow = 'review'\nwhen = ['pr=open']",
    ] {
        let env = Env::new();
        write_loop_config(
            &env,
            &format!("[tasks.old]\nroot = {:?}\n{definition}\n", env.project_root),
        );
        let paths = env.state_path_for(&env.project_root);
        assert!(!paths.root.exists());

        let receipt = loop_ok(&env, &["loop", "rename", "old", "new"]);
        assert_eq!(receipt.trim(), "renamed loop task `old` to `new`");
        assert!(!paths.root.exists(), "rename must not create room state");
    }
}

#[test]
fn loop_remove_workflow_without_a_room_does_not_create_state() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.review]\nroot = {:?}\nworkflow = 'review'\nwhen = ['pr=open']\n",
            env.project_root
        ),
    );
    let paths = env.state_path_for(&env.project_root);
    assert!(!paths.root.exists());

    let receipt = loop_ok(&env, &["loop", "remove", "review"]);
    assert_eq!(receipt.trim(), "removed loop task `review`");
    assert!(!paths.root.exists(), "remove must not create room state");
}

#[test]
fn loop_rename_rejects_collisions_and_reports_missing() {
    let env = Env::new();
    env.record(&env.project_root);
    write_loop_config(
        &env,
        &format!(
            "[tasks.old]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"15m\"\n\
             [tasks.existing]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"15m\"\n",
            env.project_root.display(),
            env.project_root.display()
        ),
    );
    loop_ok(
        &env,
        &["loop", "add", "state", "--check", "true", "--at", "07:00"],
    );

    for (old, new, expected) in [
        ("old", "old", "must differ"),
        ("old", "existing", "already exists"),
        ("old", "state", "already exists"),
        ("state", "existing", "already exists"),
    ] {
        let (_stdout, error) = loop_fail(&env, &["loop", "rename", old, new]);
        assert!(error.contains(expected), "rename {old} -> {new}: {error}");
    }
    let output = loop_ok(&env, &["loop", "rename", "missing", "free"]);
    assert!(output.contains("no loop task named `missing`"), "{output}");
}

#[test]
fn loop_qwen_exact_quota_skip_precedes_check_command() {
    let env = Env::new();
    env.record(&env.project_root);
    loop_ok(&env, &["config", "init"]);
    loop_ok(&env, &["config", "set", "harness.budget", "0/day"]);
    let settings = env.agent_config_path("qwen");
    std::fs::create_dir_all(settings.parent().expect("Qwen config parent"))
        .expect("create Qwen config");
    std::fs::write(
        &settings,
        r#"{
            "security":{"auth":{"selectedType":"openai"}},
            "model":{"name":"qwen3-coder-plus"},
            "modelProviders":{"openai":[{
                "id":"qwen3-coder-plus",
                "baseUrl":"https://coding-intl.dashscope.aliyuncs.com/v1",
                "envKey":"BAILIAN_CODING_PLAN_API_KEY"
            }]},
            "env":{"BAILIAN_CODING_PLAN_API_KEY":"sentinel-loop-secret"}
        }"#,
    )
    .expect("write Qwen settings");
    env.install_agent_hooks("qwen");

    let launch_env = BTreeMap::from([(
        "RIMZ_QWEN_SETTINGS".to_owned(),
        settings.display().to_string(),
    )]);
    let adapter = rimz::agents::find_definition("qwen").expect("Qwen definition");
    let argv = adapter
        .launch_command(&[], Some("work"))
        .expect("Qwen launch argv");
    let binding = adapter
        .resolve_managed_launch(&env.project_root, &launch_env, None, &argv)
        .binding()
        .cloned()
        .expect("exact Qwen binding");
    let encoded = serde_json::to_value(&binding).expect("serialize binding");
    let account_key = encoded["account_key"]
        .as_str()
        .expect("binding account key")
        .to_owned();
    let runtime = env.runtime_paths();
    runtime.ensure_dirs().expect("runtime dirs");
    let rate_cache = |used_7d, used_30d| RateLimitsCache {
        entries: [(
            rimz::agents::account::AccountUsageKey::new(
                rimz::ids::LoginKey::default_for(rimz::ids::AgentKind::new_unchecked("qwen")),
                binding.scope().clone(),
            ),
            RateLimitCacheEntry {
                scope: binding.scope().clone(),
                account_key: Some(account_key.clone()),
                limits: AgentRateLimits {
                    windows: vec![
                        RateLimitWindow {
                            used_percentage: Some(used_7d),
                            resets_at: Some(
                                Timestamp::now() + jiff::SignedDuration::from_hours(84),
                            ),
                            duration_mins: Some(7 * 24 * 60),
                            ..RateLimitWindow::default()
                        },
                        RateLimitWindow {
                            used_percentage: Some(used_30d),
                            resets_at: Some(
                                Timestamp::now() + jiff::SignedDuration::from_hours(360),
                            ),
                            duration_mins: Some(30 * 24 * 60),
                            ..RateLimitWindow::default()
                        },
                    ],
                },
                bound_limits: None,
                pending: Vec::new(),
                unknown_since_ms: None,
                reset_spotted_at_ms: None,
            },
        )]
        .into_iter()
        .collect(),
        ..RateLimitsCache::default()
    };
    rimz::disk::atomic::write_temp_then_rename_cache(
        &RateLimitsCache::shared_path(&runtime),
        &rate_cache(20, 100),
    )
    .expect("write exact quota cache");
    assert!(
        rimz::agents::provider_budget_gate(
            &runtime,
            &rimz::ids::LoginKey::default_for(rimz::ids::AgentKind::new_unchecked("qwen")),
            &binding,
            Timestamp::now()
        )
        .is_some(),
        "the exact cache must close the provider gate before the loop runner starts"
    );

    let marker = env.project_root.join("check-ran");
    qwen_loop_ok(
        &env,
        &settings,
        &[
            "loop",
            "add",
            "qwen-bounded",
            "--check",
            &format!("touch {}", marker.display()),
            "--on",
            "success",
            "--agent",
            "qwen",
            "--prompt",
            "work",
            "--surplus",
            "2x",
            "--every",
            "15m",
        ],
    );
    let mut config: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).expect("read loop config"))
            .expect("parse loop config");

    qwen_loop_ok(&env, &settings, &["loop", "run", "qwen-bounded"]);
    let scope_skip = last_loop_record(&env);
    assert_eq!(scope_skip.result, LoopRunResult::BudgetSkipped);
    assert!(
        scope_skip
            .error
            .as_deref()
            .is_some_and(|reason| reason.contains("fleet budget exhausted")),
        "scope cap must gate before provider quota"
    );

    loop_ok(&env, &["budget", "off", "--no-continue"]);
    qwen_loop_ok(&env, &settings, &["loop", "run", "qwen-bounded"]);

    assert!(
        !marker.exists(),
        "provider quota must gate before the check"
    );
    let records = read_loop_run_records(&env);
    let skip = records.last().expect("Qwen budget skip record");
    assert_eq!(skip.result, LoopRunResult::BudgetSkipped);
    let reason = skip.error.as_deref().expect("skip reason");
    assert!(
        reason.contains("Qwen Alibaba International mo window exhausted"),
        "{reason}"
    );
    assert!(!reason.contains("sentinel-loop-secret"), "{reason}");
    assert!(!reason.contains(&account_key), "{reason}");

    rimz::disk::atomic::write_temp_then_rename_cache(
        &RateLimitsCache::shared_path(&runtime),
        &rate_cache(80, 80),
    )
    .expect("write surplus cache");
    qwen_loop_ok(&env, &settings, &["loop", "run", "qwen-bounded"]);
    let surplus = last_loop_record(&env);
    assert_eq!(surplus.result, LoopRunResult::SurplusSkipped);
    assert!(
        surplus
            .error
            .as_deref()
            .is_some_and(|reason| reason.contains("surplus")),
        "surplus must gate before run lock and check"
    );
    let lock_path = loop_run_lock_path(&env, "qwen-bounded");
    std::fs::create_dir_all(lock_path.parent().expect("lock parent")).expect("mkdir runtime");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("open run lock");
    lock_file.try_lock().expect("hold run lock");

    config
        .tasks
        .0
        .get_mut("qwen-bounded")
        .expect("Qwen task")
        .surplus = None;
    std::fs::write(
        loop_config_path(&env),
        toml::to_string_pretty(&config).expect("serialize loop config"),
    )
    .expect("write loop config");
    qwen_loop_ok(&env, &settings, &["loop", "run", "qwen-bounded"]);
    assert_eq!(last_loop_record(&env).result, LoopRunResult::Overlapped);

    // R3's ladder reads the deadline before the lease.
    config
        .tasks
        .0
        .get_mut("qwen-bounded")
        .expect("Qwen task")
        .deadline = Some(Timestamp::from_second(1).expect("deadline"));
    std::fs::write(
        loop_config_path(&env),
        toml::to_string_pretty(&config).expect("serialize loop config"),
    )
    .expect("write loop config");
    qwen_loop_ok(&env, &settings, &["loop", "run", "qwen-bounded"]);
    assert_eq!(last_loop_record(&env).result, LoopRunResult::Expired);
    lock_file.unlock().expect("unlock run lock");
    assert!(
        !std::fs::read_to_string(loop_config_path(&env))
            .expect("read loop config")
            .contains("qwen-bounded"),
        "an expired deadline retires the task"
    );
    assert!(
        !marker.exists(),
        "ordered gates must stop before the check command"
    );
}

#[test]
fn loop_check_added_from_a_worktree_runs_at_the_root_and_legacy_dir_rows_keep_their_dir() {
    let env = Env::new();
    env.record(&env.project_root);
    if !init_git_repo(&env.project_root) {
        crate::common::skip("git unavailable");
        return;
    }
    let linked = env.home_root.join("linked");
    let add = Command::new("git")
        .args(["worktree", "add", "-q", "-b", "linked"])
        .arg(&linked)
        .current_dir(&env.project_root)
        .status()
        .expect("run git worktree add");
    assert!(add.success(), "git worktree add failed");
    let linked = canonical(&linked);
    let root = canonical(&env.project_root);
    let probe = "pwd -P; printf '%s\\n' \"$RIMZ_WORKTREE_PATH\" \"$RIMZ_PROJECT_ROOT\" \"$RIMZ_WORKSPACE_ID\"";
    let output = env
        .rimz()
        .current_dir(&linked)
        .args(["loop", "add", "where", "--check", probe, "--every", "15m"])
        .output()
        .expect("add check from linked worktree");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt = String::from_utf8_lossy(&output.stdout);
    assert!(
        receipt.contains(&format!("action: runs check in {}", root.display())),
        "{receipt}"
    );
    assert!(
        receipt.lines().any(|line| line
            == format!(
                "note: runs at the project root {}, not {}; pass --worktree <prefix> for a fresh worktree per fire",
                root.display(),
                linked.display()
            )),
        "{receipt}"
    );
    let stored: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).unwrap()).unwrap();
    let entry = &stored.tasks.0["where"];
    assert_eq!(entry.root, root);
    assert_eq!(entry.dir, None);

    loop_ok(&env, &["loop", "fire", "where"]);
    let report = wait_for_loop_report(&env, "where");
    assert_eq!(report.result, LoopRunResult::Completed);
    assert_eq!(report.checkout.as_deref(), Some(root.as_path()));
    assert_eq!(
        read_loop_run_records(&env)[0].root.as_deref(),
        Some(root.as_path())
    );
    let workspace_id = WorkspaceId::from_project_root(&root);
    let journal = loop_run_journal(&env, &env.project_root, &report);
    let output = format!("{0}\n{0}\n{0}\n{workspace_id}\n", root.display());
    assert!(journal.contains(&output), "{output}: {journal}");

    let mut config = std::fs::read_to_string(loop_config_path(&env)).unwrap();
    config.push_str(&format!(
        "\n[tasks.legacy]\nroot = {root:?}\ndir = {linked:?}\ncheck = {probe:?}\nevery = \"15m\"\n"
    ));
    std::fs::write(loop_config_path(&env), config).unwrap();
    loop_ok(&env, &["loop", "fire", "legacy"]);
    let report = wait_for_loop_report(&env, "legacy");
    assert_eq!(report.result, LoopRunResult::Completed);
    assert_eq!(report.checkout.as_deref(), Some(linked.as_path()));
    assert_eq!(report.root.as_deref(), Some(root.as_path()));
    let journal = loop_run_journal(&env, &env.project_root, &report);
    let output = format!(
        "{0}\n{0}\n{1}\n{workspace_id}\n",
        linked.display(),
        root.display()
    );
    assert!(journal.contains(&output), "{output}: {journal}");
    let shown = loop_ok(&env, &["loop", "show", "legacy"]);
    assert!(
        shown
            .lines()
            .any(|line| line.contains("dir") && line.contains("~/linked")),
        "{shown}"
    );

    let output = env
        .rimz()
        .current_dir(&linked)
        .args([
            "loop",
            "add",
            "project-where",
            "--project",
            "--check",
            "pwd -P; printf '%s\\n' \"$RIMZ_WORKTREE_PATH\"",
            "--every",
            "15m",
        ])
        .output()
        .expect("add project check from linked worktree");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains(&format!("action: runs check in {}", root.display()))
    );
    let project: toml::Value = toml::from_str(
        &std::fs::read_to_string(env.project_root.join(".rimz/config.toml")).unwrap(),
    )
    .unwrap();
    let entry = project["tasks"]["project-where"].as_table().unwrap();
    assert!(!entry.contains_key("root"));
    assert!(!entry.contains_key("dir"));
    let output = env
        .rimz()
        .current_dir(&linked)
        .env(rimz::workspace::ENV_WORKTREE_PATH, &linked)
        .args(["loop", "fire", "project-where"])
        .output()
        .expect("fire project check with inherited worktree context");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = wait_for_loop_report(&env, "project-where");
    assert_eq!(report.root.as_deref(), Some(root.as_path()));
    let journal = loop_run_journal(&env, &env.project_root, &report);
    let output = format!("{0}\n{0}\n", root.display());
    assert!(journal.contains(&output), "{output}: {journal}");
}

#[test]
fn check_task_worktree_stores_a_fresh_prefix_listed_as_a_pattern() {
    let env = Env::new();
    env.record(&env.project_root);
    let receipt = loop_ok(
        &env,
        &[
            "loop",
            "add",
            "nightly",
            "--check",
            "true",
            "--worktree",
            "night",
            "--every",
            "1h",
        ],
    );
    assert!(
        receipt.lines().any(|line| line
            == "action: runs check in a fresh worktree `night-<time>` per fire, branched from trunk"),
        "{receipt}"
    );
    assert!(!receipt.contains("note:"), "{receipt}");
    let stored: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).unwrap()).unwrap();
    let entry = &stored.tasks.0["nightly"];
    assert_eq!(entry.fresh_worktree.as_deref(), Some("night"));
    assert_eq!(
        (entry.worktree.as_deref(), entry.dir.as_deref()),
        (None, None)
    );
    let listing: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "list", "--json"])).unwrap();
    let row = listing["rooms"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|room| room["tasks"].as_array().unwrap())
        .find(|row| row["name"] == "nightly")
        .unwrap();
    assert_eq!(row["fresh_worktree"], "night", "{row}");
    assert_eq!(row["worktree"], "night-*", "{row}");
    let shown: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", "nightly", "--json"])).unwrap();
    assert_eq!(shown["fresh_worktree"], "night", "{shown}");
}

/// A bare `origin` one commit ahead of the local `main`, so a tree branched from
/// fetched trunk is told apart from one branched from the local checkout.
fn origin_ahead_of_local_main(env: &Env) -> Option<String> {
    if !init_git_repo(&env.project_root) {
        return None;
    }
    let origin = env.home_root.join("origin.git");
    let root = &env.project_root;
    let pushed = git_ok(
        root,
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            origin.to_str().unwrap(),
        ],
    ) && git_ok(root, &["remote", "add", "origin", origin.to_str().unwrap()])
        && git_ok(root, &["commit", "-q", "--allow-empty", "-m", "ahead"])
        && git_ok(root, &["push", "-q", "-u", "origin", "main"])
        && git_ok(root, &["reset", "-q", "--hard", "HEAD~1"]);
    assert!(pushed, "origin fixture");
    let tip = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "origin/main"])
        .output()
        .unwrap();
    Some(String::from_utf8(tip.stdout).unwrap().trim().to_owned())
}

/// Every `night-*` worktree path and branch the fixture still has.
fn night_leftovers(env: &Env) -> Vec<String> {
    let mut left: Vec<String> = std::fs::read_dir(env.home_root.join("project-worktrees"))
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    let branches = Command::new("git")
        .arg("-C")
        .arg(&env.project_root)
        .args(["branch", "--list", "night-*", "--format=%(refname:short)"])
        .output()
        .unwrap();
    left.extend(
        String::from_utf8(branches.stdout)
            .unwrap()
            .lines()
            .map(|branch| format!("branch {branch}")),
    );
    left
}

#[cfg(unix)]
#[test]
fn check_task_with_worktree_runs_each_fire_in_a_fresh_trunk_tree_and_removes_it() {
    let env = Env::new();
    env.record(&env.project_root);
    let Some(tip) = origin_ahead_of_local_main(&env) else {
        crate::common::skip("git unavailable");
        return;
    };
    let seen = env.home_root.join("seen");
    let check = format!(
        "{{ pwd -P; git rev-parse HEAD; git branch --show-current; }} >> {:?}; touch scratch",
        seen.display().to_string()
    );
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "nightly",
            "--check",
            &check,
            "--worktree",
            "night",
            "--every",
            "1h",
        ],
    );
    let mut reports = Vec::new();
    for _ in 0..2 {
        let before = read_loop_run_records(&env).len();
        loop_ok(&env, &["loop", "fire", "nightly"]);
        reports.push(wait_for_loop_report_after(&env, "nightly", before));
        wait_for_fire_tree_helpers(&env);
    }

    let lines: Vec<String> = std::fs::read_to_string(&seen)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(lines.len(), 6, "{lines:?}");
    let records: Vec<_> = read_loop_run_records(&env)
        .into_iter()
        .filter(|record| record.result == LoopRunResult::Launched)
        .collect();
    assert_eq!(records.len(), 2);
    let worktrees = canonical(&env.home_root.join("project-worktrees"));
    let root = canonical(&env.project_root);
    for ((fire, record), report) in lines.chunks(3).zip(&records).zip(&reports) {
        assert_eq!(report.result, LoopRunResult::Completed);
        assert_eq!(report.run_id, record.run_id);
        // The task's history keys on the project root it fired from, not the fire's tree.
        assert_eq!(report.checkout.as_deref(), Some(root.as_path()));
        let tree = rimz::utils::path::normalize_path_lexical(
            &record.worktree.as_ref().expect("fire worktree").path,
        );
        assert_eq!(fire[0], tree.display().to_string());
        assert_eq!(tree.parent(), Some(worktrees.as_path()));
        let name = tree.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("night-"), "{name}");
        assert_eq!(fire[1], tip, "branched from fetched trunk");
        assert_eq!(fire[2], name, "the branch takes the tree's name");
    }
    assert_eq!(night_leftovers(&env), Vec::<String>::new());
    let logs = loop_ok(&env, &["loop", "logs", "nightly"]);
    let name = records[1]
        .worktree
        .as_ref()
        .unwrap()
        .path
        .file_name()
        .unwrap();
    assert!(
        logs.contains(&format!("  worktree: {}", name.to_str().unwrap())),
        "{logs}"
    );
}

#[cfg(unix)]
#[test]
fn a_declined_guard_in_a_fresh_tree_leaves_no_tree_behind() {
    let env = Env::new();
    env.record(&env.project_root);
    if origin_ahead_of_local_main(&env).is_none() {
        crate::common::skip("git unavailable");
        return;
    }
    env.install_agent_hooks("claude");
    crate::common::trust_claude_project(&env, &env.project_root);
    let pwd_file = env.home_root.join("guard-pwd");
    let guard = format!(
        "pwd -P > {:?}; touch untracked; exit 1",
        pwd_file.display().to_string()
    );
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "guarded",
            "--agent",
            "claude",
            "--prompt",
            "repair",
            "--every",
            "1h",
            "--check",
            &guard,
            "--on",
            "success",
            "--worktree",
            "night",
        ],
    );
    loop_ok(&env, &["loop", "fire", "guarded"]);

    // A decline ends the run without acting, and its report removes the tree.
    let report = wait_for_loop_report(&env, "guarded");
    assert_eq!(
        (report.result, report.acted),
        (LoopRunResult::Completed, Some(false))
    );
    let record = read_loop_run_records(&env)
        .into_iter()
        .find(|record| record.result == LoopRunResult::Launched)
        .expect("the fire's row");
    let tree =
        rimz::utils::path::normalize_path_lexical(&record.worktree.expect("fire worktree").path);
    assert_eq!(
        std::fs::read_to_string(&pwd_file).unwrap().trim_end(),
        tree.display().to_string()
    );
    wait_for_fire_tree_helpers(&env);
    assert!(!tree.exists());
    assert_eq!(night_leftovers(&env), Vec::<String>::new());
    let signals: Vec<serde_json::Value> = env
        .read_events()
        .into_iter()
        .filter(|event| event.method == "signal.emit")
        .map(|event| serde_json::from_str(event.params.get()).unwrap())
        .collect();
    let named: Vec<_> = signals
        .iter()
        .map(|signal| (signal["name"].as_str(), signal["payload"]["name"].as_str()))
        .collect();
    let name = tree.file_name().and_then(|name| name.to_str());
    assert_eq!(
        named,
        [
            (Some("worktree.created"), name),
            (Some("worktree.removed"), name)
        ],
        "{signals:?}"
    );
    assert_eq!(signals[1]["payload"]["branch_deleted"], true, "{signals:?}");
}

#[cfg(unix)]
#[test]
fn a_fresh_task_fired_while_its_run_lives_overlaps_without_cutting_a_tree() {
    let env = Env::new();
    env.record(&env.project_root);
    if origin_ahead_of_local_main(&env).is_none() {
        crate::common::skip("git unavailable");
        return;
    }
    let release = env.home_root.join("release");
    let check = format!(
        "while [ ! -e {:?} ]; do sleep 0.05; done",
        release.display().to_string()
    );
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "nightly",
            "--check",
            &check,
            "--worktree",
            "night",
            "--every",
            "1h",
        ],
    );
    loop_ok(&env, &["loop", "fire", "nightly"]);
    loop_ok(&env, &["loop", "fire", "nightly"]);
    let records = read_loop_run_records(&env);
    let launched = records
        .iter()
        .find(|record| record.result == LoopRunResult::Launched)
        .expect("the first fire's row");
    let overlapped = records.last().unwrap();
    assert_eq!(
        (
            overlapped.result,
            overlapped.blocking_run.as_ref().map(|run| run.as_str())
        ),
        (LoopRunResult::Overlapped, launched.run_id.as_deref()),
        "{records:?}"
    );
    assert_eq!(overlapped.worktree, None, "{overlapped:?}");
    assert_eq!(night_leftovers(&env).len(), 2, "one tree and its branch");

    std::fs::write(&release, "").unwrap();
    let report = wait_for_loop_report(&env, "nightly");
    assert_eq!(report.result, LoopRunResult::Completed);
    wait_for_fire_tree_helpers(&env);
    assert_eq!(night_leftovers(&env), Vec::<String>::new());
}

#[cfg(unix)]
#[test]
fn a_declined_run_the_backstop_reports_removes_its_fresh_tree() {
    let env = Env::new();
    env.record(&env.project_root);
    if origin_ahead_of_local_main(&env).is_none() {
        crate::common::skip("git unavailable");
        return;
    }
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "nightly",
            "--check",
            "exit 1",
            "--worktree",
            "night",
            "--every",
            "1h",
        ],
    );
    // A run whose advance died after its terminal persist: its report is still owed, and the
    // backstop's advance is the one that reports it.
    let root = canonical(&env.project_root);
    let created = rimz::worktree::create_for_loop_fire(
        &root,
        &rimz::config::WorktreeConfig::default(),
        "night",
        "nightly",
        Timestamp::now(),
    )
    .unwrap();
    let tree = created.marker.worktree_path.clone();
    let id = rimz::ids::WorkflowRunId::new();
    let paths = env.state_path_for(&root).workflow(&id);
    std::fs::create_dir_all(paths.record.parent().unwrap()).unwrap();
    let record = json!({
        "version":rimz::store::workflow::VERSION, "id":id, "workflow":"nightly", "origin":"inline", "task":"nightly",
        "spec_dir":"/spec", "spec_digest":"abc", "fire_tree":true,
        "scope":{"kind":"worktree","path":tree}, "project_root":root,
        "created_at":"2026-01-01T00:00:00Z", "state":"failed", "node":"check",
        "entered_at":"2026-01-01T00:00:00Z", "visits":{}, "visit_grant":0,
        "deadline_fired":false, "next_seq":1, "terminal_at":"2026-01-01T00:00:01Z",
        "loop_report":"owed", "outcome":{"ending":{"kind":"failed"}, "acted":false}
    });
    std::fs::write(&paths.record, serde_json::to_vec(&record).unwrap()).unwrap();
    let output = env
        .rimz()
        .arg("--root")
        .arg(&root)
        .args(["workflow", "advance", id.as_str()])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    let report = wait_for_loop_report(&env, "nightly");
    assert_eq!(
        (report.result, report.acted, report.checkout.as_deref()),
        (LoopRunResult::Failed, Some(false), Some(root.as_path()))
    );
    wait_for_fire_tree_helpers(&env);
    assert!(!tree.exists());
    assert_eq!(night_leftovers(&env), Vec::<String>::new());
}

/// A codex stand-in that runs a whole hook-firing turn in a tmux pane. Each
/// turn appends its `pwd -P` to `turns`, and commits a file first while
/// `commit` exists.
#[cfg(unix)]
struct LaunchedTurn {
    bin: std::path::PathBuf,
    shell: std::path::PathBuf,
    turns: std::path::PathBuf,
    commit: std::path::PathBuf,
}

#[cfg(unix)]
impl LaunchedTurn {
    fn install(env: &Env) -> Self {
        std::fs::write(
            env.rimz_home().join("config.toml"),
            "[agents]\nshell = 'bash'\n",
        )
        .unwrap();
        env.install_agent_hooks("codex");
        trust_codex_preflight_hooks(env);
        trust_codex_project(env, &env.project_root);
        let bin = crate::common::write_hook_firing_agent(env, "codex");
        let hooks = bin.join("codex-hooks");
        std::fs::rename(bin.join("codex"), &hooks).unwrap();
        let turns = env.home_root.join("turn-pwd");
        let commit = env.home_root.join("turn-commits");
        let quote = |path: &Path| {
            shlex::try_quote(path.to_str().unwrap())
                .unwrap()
                .into_owned()
        };
        write_path_shim(
            &bin,
            "codex",
            &format!(
                "case \"${{1:-}}\" in --version|login|app-server) exec {hooks} \"$@\";; esac\n\
                 pwd -P >> {turns}\n\
                 if [ -e {commit} ]; then\n\
                   echo work > work.txt && git add work.txt && \
                 git -c user.name=t -c user.email=t@example.com commit -qm work || exit 9\n\
                 fi\n\
                 exec {hooks} \"$@\"",
                hooks = quote(&hooks),
                turns = quote(&turns),
                commit = quote(&commit),
            ),
        );
        let login_shell = write_fake_login_shell(env, "rimz-test-sh", &[]);
        std::os::unix::fs::symlink(&login_shell, bin.join("bash")).unwrap();
        // The room's first pane runs `$SHELL` bare, and must outlive the birth.
        let shell = write_path_shim(
            &env.home_root.join("interactive-shell-bin"),
            "rimz-test-sh",
            &format!(
                "for arg do\n  if [ \"$arg\" = -c ]; then exec {} \"$@\"; fi\ndone\nexec /bin/sh \"$@\"",
                quote(&login_shell),
            ),
        );
        Self {
            bin,
            shell,
            turns,
            commit,
        }
    }

    /// Fire `task`, wait for its run's report, then for its pane's wrapper and cleanup
    /// helper to exit.
    fn fire(&self, env: &Env, task: &str, commit: bool) -> (std::process::Output, LoopRunRecord) {
        if commit {
            std::fs::write(&self.commit, "").unwrap();
        } else {
            let _ = std::fs::remove_file(&self.commit);
        }
        let before = read_loop_run_records(env).len();
        let output = env
            .rimz()
            .args(["--mux", "tmux", "loop", "fire", task])
            .env("PATH", path_with_front(&self.bin))
            .env("SHELL", &self.shell)
            .output()
            .expect("fire the launched turn");
        assert!(output.status.success(), "{output:?}");
        let report = wait_for_loop_report_after(env, task, before);
        let spec = rimz::testkit::sandbox::SandboxSpec {
            home_root: env.home_root.clone(),
            runtime_root: env.runtime_root.clone(),
        };
        let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
        while rimz::testkit::sandbox::sandbox_processes(&spec)
            .into_iter()
            .filter_map(rimz::proc::argv)
            .any(|argv| argv.iter().any(|arg| arg == "--supervise"))
        {
            assert!(Instant::now() < deadline, "the turn's wrapper did not exit");
            std::thread::sleep(Duration::from_millis(50));
        }
        wait_for_fire_tree_helpers(env);
        (output, report)
    }

    /// The row the fire wrote when its run started, which names the fire's tree.
    fn launched(env: &Env, report: &LoopRunRecord) -> LoopRunRecord {
        read_loop_run_records(env)
            .into_iter()
            .find(|record| {
                record.result == LoopRunResult::Launched && record.run_id == report.run_id
            })
            .expect("the fire's row")
    }

    fn turn_dirs(&self) -> Vec<String> {
        std::fs::read_to_string(&self.turns)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

#[cfg(unix)]
#[test]
fn a_launched_turn_in_a_fresh_tree_keeps_the_tree_only_with_work() {
    if which::which("tmux").is_err() {
        crate::common::skip("tmux not on PATH");
        return;
    }
    let env = Env::new();
    if origin_ahead_of_local_main(&env).is_none() {
        crate::common::skip("git unavailable");
        return;
    }
    let turn = LaunchedTurn::install(&env);
    let guards = env.home_root.join("guard-pwd");
    let guard = format!("pwd -P >> {:?}", guards.display().to_string());
    for (task, verify) in [("turn", "true"), ("unverified", "exit 1")] {
        loop_ok(
            &env,
            &[
                "loop",
                "add",
                task,
                "--agent",
                "codex",
                "--prompt",
                "repair",
                "--every",
                "1h",
                "--check",
                &guard,
                "--on",
                "success",
                "--verify",
                verify,
                "--worktree",
                "night",
            ],
        );
    }

    let mut kept = Vec::new();
    for (task, commit) in [
        ("turn", false),
        ("turn", true),
        ("unverified", false),
        ("unverified", true),
    ] {
        let (output, report) = turn.fire(&env, task, commit);
        let verified = task == "turn";
        assert_eq!(
            (report.result, report.acted),
            (
                if verified {
                    LoopRunResult::Completed
                } else {
                    LoopRunResult::Failed
                },
                Some(true)
            ),
            "{report:?}"
        );
        let record = LaunchedTurn::launched(&env, &report);
        let tree = rimz::utils::path::normalize_path_lexical(
            &record.worktree.expect("fire worktree").path,
        );
        let shown = tree.display().to_string();
        assert_eq!(turn.turn_dirs().last(), Some(&shown), "{output:?}");
        let guarded = std::fs::read_to_string(&guards).unwrap();
        assert_eq!(guarded.lines().last(), Some(shown.as_str()));
        assert_eq!(
            tree.exists(),
            commit,
            "{task} (commit: {commit}): {output:?}"
        );
        if commit {
            assert!(tree.join("work.txt").exists());
            kept.push((task, tree));
        }
    }

    for (task, tree) in &kept {
        let text = loop_ok(&env, &["loop", "show", task]);
        let section = text
            .split_once("KEPT WORKTREES")
            .unwrap_or_else(|| panic!("{text}"))
            .1;
        let name = tree.file_name().unwrap().to_str().unwrap();
        assert!(section.contains(name), "{text}");
        let json: serde_json::Value =
            serde_json::from_str(&loop_ok(&env, &["loop", "show", task, "--json"])).unwrap();
        let listed: Vec<std::path::PathBuf> = json["kept_worktrees"]
            .as_array()
            .unwrap_or_else(|| panic!("{json}"))
            .iter()
            .map(|row| {
                rimz::utils::path::normalize_path_lexical(Path::new(row["path"].as_str().unwrap()))
            })
            .collect();
        assert_eq!(listed, std::slice::from_ref(tree), "{json}");
        let marker = rimz::worktree::read_marker_for_worktree(tree)
            .unwrap()
            .unwrap();
        assert_eq!(marker.loop_task.as_deref(), Some(*task));
    }

    // A fleet budget that closes while the guard runs passes the runner's own
    // gate and refuses the launch itself, after the fire handed its tree over.
    let closed = env.home_root.join("closed-fleet-budget.json");
    let ledger = rimz::harness::budget::DailyBudgetLedger {
        override_spec: Some("0/day".parse().expect("budget")),
        ..Default::default()
    };
    std::fs::write(&closed, serde_json::to_vec(&ledger).unwrap()).unwrap();
    let record = env.state_path_for(&env.project_root).fleet_budget_record;
    let quote = |path: &Path| {
        shlex::try_quote(path.to_str().unwrap())
            .unwrap()
            .into_owned()
    };
    let close = format!(
        "{guard} && mkdir -p {} && mv {} {}",
        quote(record.parent().unwrap()),
        quote(&closed),
        quote(&record),
    );
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "refused",
            "--agent",
            "codex",
            "--prompt",
            "repair",
            "--every",
            "1h",
            "--check",
            &close,
            "--on",
            "success",
            "--verify",
            "true",
            "--worktree",
            "night",
        ],
    );
    let turns = turn.turn_dirs().len();
    let (output, refused) = turn.fire(&env, "refused", false);
    assert_eq!(refused.result, LoopRunResult::BudgetSkipped, "{refused:?}");
    assert!(
        refused
            .error
            .as_deref()
            .is_some_and(|error| error.contains("fleet budget")),
        "{refused:?}"
    );
    let refused = LaunchedTurn::launched(&env, &refused);
    let tree =
        rimz::utils::path::normalize_path_lexical(&refused.worktree.expect("fire worktree").path);
    let guarded = std::fs::read_to_string(&guards).unwrap();
    assert_eq!(
        guarded.lines().last(),
        Some(tree.display().to_string().as_str())
    );
    assert_eq!(turn.turn_dirs().len(), turns, "no turn ran: {output:?}");
    assert!(
        !tree.exists(),
        "the refused launch's clean tree stays: {output:?}"
    );
}

#[cfg(unix)]
#[test]
fn agent_task_added_from_a_worktree_stores_no_dir_and_guards_at_the_root() {
    let env = Env::new();
    env.record(&env.project_root);
    if !init_git_repo(&env.project_root) {
        crate::common::skip("git unavailable");
        return;
    }
    env.install_agent_hooks("claude");
    crate::common::trust_claude_project(&env, &env.project_root);
    let linked = env.home_root.join("linked");
    assert!(git_ok(
        &env.project_root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            linked.to_str().unwrap()
        ]
    ));
    let linked = canonical(&linked);
    let root = canonical(&env.project_root);
    let pwd_file = env.home_root.join("guard-pwd");
    let guard = format!("pwd -P > {:?}; exit 1", pwd_file.display().to_string());
    for (name, extra) in [("guarded", vec![]), ("seated", vec!["--stay"])] {
        let mut argv = vec![
            "loop", "add", name, "--agent", "claude", "--prompt", "repair", "--every", "1h",
        ];
        argv.extend(extra);
        if name == "guarded" {
            argv.extend(["--check", guard.as_str(), "--on", "success"]);
        }
        let output = env
            .rimz()
            .current_dir(&linked)
            .args(&argv)
            .output()
            .expect("add agent task from linked worktree");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stored: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).unwrap()).unwrap();
    for name in ["guarded", "seated"] {
        let entry = &stored.tasks.0[name];
        assert_eq!(entry.root, root, "{name}");
        assert_eq!(entry.dir, None, "{name}");
        assert_eq!(entry.run_dir(), root, "{name}");
    }

    loop_ok(&env, &["loop", "fire", "guarded"]);
    let report = wait_for_loop_report(&env, "guarded");
    assert_eq!(
        (report.result, report.acted),
        (LoopRunResult::Completed, Some(false))
    );
    assert_eq!(
        std::fs::read_to_string(&pwd_file).unwrap().trim_end(),
        root.display().to_string()
    );

    if which::which("tmux").is_err() {
        crate::common::skip("tmux not on PATH");
        return;
    }
    let turn = LaunchedTurn::install(&env);
    let output = env
        .rimz()
        .current_dir(&linked)
        .args([
            "loop", "add", "launched", "--agent", "codex", "--prompt", "repair", "--every", "1h",
        ])
        .output()
        .expect("add a launching task from linked worktree");
    assert!(output.status.success(), "{output:?}");
    let (output, report) = turn.fire(&env, "launched", false);
    assert_eq!(report.result, LoopRunResult::Completed, "{output:?}");
    assert_eq!(turn.turn_dirs(), [root.display().to_string()]);
}

#[test]
fn loop_check_outcomes_report_once_and_render_from_their_runs() {
    let env = Env::new();
    env.record(&env.project_root);
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "history",
            "--check",
            "printf healthy",
            "--every",
            "15m",
        ],
    );
    loop_ok(&env, &["loop", "fire", "history"]);
    let healthy = wait_for_loop_report(&env, "history");
    assert_eq!(healthy.result, LoopRunResult::Completed);
    let journal = loop_run_journal(&env, &env.project_root, &healthy);
    assert!(journal.contains("healthy"), "{journal}");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "history",
            "--check",
            "printf broken; definitely-missing-rimz-loop-command",
            "--every",
            "15m",
        ],
    );
    let before = read_loop_run_records(&env).len();
    loop_ok(&env, &["loop", "fire", "history"]);
    let broken = wait_for_loop_report_after(&env, "history", before);
    assert_eq!(broken.result, LoopRunResult::Failed);
    let journal = loop_run_journal(&env, &env.project_root, &broken);
    assert!(journal.contains("broken"), "{journal}");
    assert_eq!(read_loop_run_records(&env).len(), 4);

    // The views read each fire's check output from its run's journal.
    let show = loop_ok(&env, &["loop", "show", "history"]);
    assert!(
        show.contains("LAST RUN — ✗ failed")
            && show.contains("check: fail · exit 127")
            && show.contains("broken"),
        "{show}"
    );
    let logs = loop_ok(&env, &["loop", "logs", "history"]);
    assert!(
        logs.find("healthy").unwrap() < logs.find("broken").unwrap()
            && logs.contains(broken.run_id.as_deref().unwrap()),
        "{logs}"
    );
    let failed = loop_ok(&env, &["loop", "logs", "history", "--failed"]);
    assert!(
        failed.contains("broken") && !failed.contains("healthy"),
        "{failed}"
    );
}

#[test]
fn loop_judge_decline_renders_its_verdict_from_the_run() {
    let env = Env::new();
    env.record(&env.project_root);
    // A stored guarded wait whose judge declines ends before its message step.
    write_loop_config(
        &env,
        &format!(
            "[tasks.judged]\nwait = {{ kind = \"claude\", session = \"sess-judged\", handle = \"@claude\" }}\n\
             prompt = \"fix it\"\non = \"fail\"\n\
             every = \"15m\"\nroot = {:?}\n\
             [[tasks.judged.judge]]\nagent = \"claude\"\nprompt = \"is it broken?\"\n",
            env.project_root
        ),
    );
    let shell = crate::common::write_fake_login_shell(&env, "judge-shell", &[]);
    let bin = env.home_root.join("judge-bin");
    crate::common::write_path_shim(
        &bin,
        "claude",
        "if [ \"$1\" = --version ]; then echo 'claude 9.0.0'; exit 0; fi\n\
         printf '%s\\n' '{\"structured_output\":{\"outcome\":\"pass\",\"reason\":\"all green\"}}'\n",
    );
    let fired = env
        .rimz()
        .args(["loop", "fire", "judged"])
        .env("SHELL", &shell)
        .env("PATH", crate::common::path_with_front(&bin))
        .output()
        .unwrap();
    assert!(fired.status.success(), "{fired:?}");
    let report = wait_for_loop_report(&env, "judged");
    assert_eq!(report.result, LoopRunResult::Completed);

    let logs = loop_ok(&env, &["loop", "logs", "judged"]);
    assert!(logs.contains("judge claude: pass — all green"), "{logs}");
    let show = loop_ok(&env, &["loop", "show", "judged"]);
    assert_eq!(
        show.matches("judge claude: pass — all green").count(),
        1,
        "{show}"
    );
}

#[test]
fn loop_guard_skips_or_delivers_with_evidence() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-check", "feature-loop");
    // `loop add` refuses a guarded `--wait`; stored rows of the shape still run.
    let task = |name: &str, check: &str| {
        format!(
            "[tasks.{name}]\n\
             wait = {{ kind = \"claude\", session = \"sess-loop-check\", handle = \"@claude\" }}\n\
             prompt = \"fix it\"\ncheck = {check:?}\non = \"fail\"\nroot = {:?}\nevery = \"15m\"\n",
            env.project_root
        )
    };
    write_loop_config(
        &env,
        &format!(
            "{}{}",
            task("healthy", "printf probe-line"),
            task("broken", "printf boom; exit 1")
        ),
    );
    loop_ok(&env, &["loop", "run", "healthy"]);
    let skipped = wait_for_loop_report(&env, "healthy");
    assert_eq!(
        (skipped.result, skipped.acted),
        (LoopRunResult::Completed, Some(false))
    );
    assert!(
        env.store()
            .list_pending_messages()
            .expect("messages")
            .is_empty()
    );

    loop_ok(&env, &["loop", "run", "broken"]);
    let delivered = wait_for_loop_report(&env, "broken");
    assert_eq!(
        (delivered.result, delivered.acted),
        (LoopRunResult::Completed, Some(true))
    );
    assert_pending_message(&env, "sess-loop-check", "fix it");
}

#[test]
fn loop_preparation_error_records_and_renders() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-trip-error", "feature-loop");
    write_loop_config(
        &env,
        &format!(
            "[tasks.trip_error]\n\
             wait = {{ kind = \"claude\", session = \"sess-loop-trip-error\", handle = \"@claude\" }}\n\
             prompt-file = \"missing-prompt.txt\"\ncheck = \"false\"\nroot = \"{}\"\nevery = \"15m\"\n",
            env.project_root.display()
        ),
    );

    // The prompt file is read where the run's definition compiles, before any check runs.
    let (_stdout, error) = loop_fail(&env, &["loop", "fire", "trip_error"]);
    assert!(
        error.contains("reading prompt-file") && error.contains("missing-prompt.txt"),
        "{error}"
    );
    let records = read_loop_run_records(&env);
    assert_eq!(
        records.len(),
        1,
        "a preparation error must append exactly one terminal row"
    );
    let record = &records[0];
    assert_eq!(record.result, LoopRunResult::Errored);
    assert!(
        record
            .error
            .as_deref()
            .is_some_and(|error| error.contains("reading prompt-file"))
    );
    let show = loop_ok(&env, &["loop", "show", "trip_error"]);
    assert!(
        show.contains("error") && show.contains("reading prompt-file"),
        "{show}"
    );
    assert!(
        std::fs::read_to_string(loop_config_path(&env))
            .unwrap()
            .contains("[tasks.trip_error]")
    );
}

#[test]
fn loop_missing_spawn_prompt_names_task() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    write_loop_config(
        &env,
        &format!(
            "[tasks.named_spawn]\nagent = \"claude\"\nroot = \"{}\"\nat = \"07:00\"\n",
            env.project_root.display()
        ),
    );
    let (_stdout, error) = loop_fail(&env, &["loop", "run", "named_spawn"]);
    assert!(
        error.contains("loop task `named_spawn` has no prompt")
            && !error.contains("loop task `claude` has no prompt"),
        "{error}"
    );
}

#[cfg(unix)]
#[test]
fn loop_scheduled_one_shot_consumption_follows_preflight_boundary() {
    let dispatch = Env::new();
    dispatch.record(&dispatch.project_root);
    dispatch.install_agent_hooks("claude");
    crate::common::trust_claude_project(&dispatch, &dispatch.project_root);
    loop_ok(
        &dispatch,
        &[
            "loop",
            "add",
            "dispatch-fails",
            "--agent",
            "claude",
            "--prompt",
            "ship it",
            "--at",
            "07:00",
        ],
    );
    let empty_path = dispatch.home_root.join("empty-path");
    std::fs::create_dir_all(&empty_path).expect("empty PATH");
    let shell = write_fake_login_shell(&dispatch, "loop-preflight-test-sh", &[]);
    // A live room: the fire births none, so nothing before the run needs a multiplexer.
    crate::common::room::seed_sidebar_heartbeat(
        &dispatch.runtime_paths(),
        MuxName::Zellij,
        &dispatch
            .resolve_workspace(&dispatch.project_root)
            .session_name,
        "dispatch",
    );
    let output = dispatch
        .rimz()
        .env("SHELL", shell)
        .env("PATH", &empty_path)
        .args(["loop", "run", "dispatch-fails"])
        .output()
        .expect("run scheduled spawn");
    // The run started, so the one-shot is spent, whatever its step then meets.
    assert!(output.status.success(), "{output:?}");
    assert!(
        !read_loop_instances(&dispatch)
            .0
            .contains_key("dispatch-fails")
    );
    let launched = last_loop_record(&dispatch);
    assert_eq!(
        (launched.task.as_str(), launched.result),
        ("dispatch-fails", LoopRunResult::Launched)
    );
    assert!(launched.run_id.is_some(), "{launched:?}");

    let preflight = Env::new();
    write_loop_instances(
        &preflight,
        Tasks(BTreeMap::from([(
            "preflight-fails".to_owned(),
            TaskEntry {
                agent: Some("claude".to_owned()),
                prompt: Some("ship it".to_owned()),
                root: preflight.project_root.clone(),
                at: Some("07:00".to_owned()),
                ..TaskEntry::default()
            },
        )])),
    );
    let (_stdout, error) = loop_fail(&preflight, &["loop", "run", "preflight-fails"]);
    assert!(error.contains("hooks are not installed"), "{error}");
    assert!(
        read_loop_instances(&preflight)
            .0
            .contains_key("preflight-fails")
    );
    assert_eq!(last_loop_record(&preflight).task, "preflight-fails");
}

#[test]
fn loop_show_surfaces_spawn_failure_tail_and_prior_error() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.forensics]\nagent = \"codex\"\nprompt = \"go\"\ncheck = \"true\"\nroot = \"{}\"\nevery = \"15m\"\n",
            env.project_root.display()
        ),
    );
    let paths = env.state_path_for(&env.project_root);
    paths.ensure_dirs().unwrap();
    let mut run = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "go".to_owned(),
        env.project_root.clone(),
    );
    run.status = RunStatus::Failed;
    run.failure_tail = Some("agent startup failed\nmissing binary".to_owned());
    run.transcript_path = Some("/tmp/rimz-transcript.jsonl".to_owned());
    rimz::harness::run::create(&paths, &run).unwrap();

    let mut prior =
        LoopRunRecord::new("forensics", LoopRunResult::Errored, LoopRunMode::Manual, 42);
    prior.at = Timestamp::from_second(10).unwrap();
    prior.error = Some("reading system-prompt-file `/missing.md`\ncaused by: not found".to_owned());
    let mut failed =
        LoopRunRecord::new("forensics", LoopRunResult::Failed, LoopRunMode::Manual, 50);
    failed.at = Timestamp::from_second(20).unwrap();
    failed.run_id = Some(run.run_id.to_string());
    failed.transcript_path = Some("/tmp/rimz-transcript.jsonl".to_owned());
    write_loop_run_records(&env, &[prior, failed]);

    let show = loop_ok(&env, &["loop", "show", "forensics"]);
    assert!(
        show.contains("LAST RUN — ✗ failed (exit 1)")
            && show.contains("agent startup failed\n  │ missing binary")
            && show.contains("transcript: /tmp/rimz-transcript.jsonl")
            && !show.contains("last failure"),
        "{show}"
    );
    assert!(!show.contains("caused by: not found"), "{show}");
}

#[test]
fn loop_poll_until_delivers_once_or_expires() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-until", "feature-loop");
    // `loop add --until` refuses; a stored `--until` row still runs, one run per fire.
    write_loop_instances(
        &env,
        Tasks(BTreeMap::from([(
            "green".to_owned(),
            TaskEntry {
                wait: Some(TaskTarget {
                    kind: AgentKind::new_unchecked("claude"),
                    session: AgentSessionId::from("sess-loop-until"),
                    handle: "@claude".to_owned(),
                }),
                prompt: Some("merge now".to_owned()),
                check: Some("true".into()),
                on: Some(CheckOn::Success),
                root: env.project_root.clone(),
                every: Some("2m".to_owned()),
                deadline: Some(Timestamp::now() + SignedDuration::from_mins(30)),
                ..TaskEntry::default()
            },
        )])),
    );
    loop_ok(&env, &["loop", "run", "green"]);
    // The acted run's report consumes the row.
    let report = wait_for_loop_report(&env, "green");
    assert_eq!(report.acted, Some(true));
    assert_pending_message(&env, "sess-loop-until", "merge now");
    wait_for_loop_run_records(&env, "the acted --until row's removal", |_| {
        !read_loop_instances(&env).0.contains_key("green")
    });

    let expired = Env::new();
    expired.record(&expired.project_root);
    write_loop_instances(
        &expired,
        Tasks(BTreeMap::from([(
            "expired".to_owned(),
            TaskEntry {
                wait: Some(TaskTarget {
                    kind: AgentKind::new_unchecked("claude"),
                    session: AgentSessionId::from("sess-expired"),
                    handle: "@claude".to_owned(),
                }),
                prompt: Some("too late".to_owned()),
                check: Some("true".into()),
                on: Some(CheckOn::Success),
                root: expired.project_root.clone(),
                every: Some("2m".to_owned()),
                deadline: Some(Timestamp::from_second(1).unwrap()),
                ..TaskEntry::default()
            },
        )])),
    );
    loop_ok(&expired, &["loop", "run", "expired"]);
    assert!(read_loop_instances(&expired).0.is_empty());
    let records = read_loop_run_records(&expired);
    assert_eq!(
        records.len(),
        1,
        "a prepare-time Done result must append exactly one terminal row"
    );
    assert_eq!(records[0].result, LoopRunResult::Expired);
    assert!(
        expired
            .store()
            .list_pending_messages()
            .expect("messages")
            .is_empty()
    );
}

#[test]
fn loop_worktree_target_delivery_preserves_session() {
    let env = Env::new();
    env.record(&env.project_root);
    if !init_git_repo(&env.project_root) {
        crate::common::skip("git unavailable");
        return;
    }
    let worktree = env.home_root.join("project-worktrees/feature-loop");
    std::fs::create_dir_all(worktree.parent().expect("worktree parent")).expect("mkdir worktrees");
    assert!(git_ok(
        &env.project_root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature-loop",
            worktree.to_str().expect("utf8 worktree"),
        ]
    ));
    env.install_agent_hooks("claude");
    register_running_agent_at(&env, "sess-loop-worktree", "feature-loop", &worktree);
    let added = env
        .rimz()
        .current_dir(&worktree)
        .args([
            "loop",
            "add",
            "wait-worktree",
            "--wait",
            "@claude",
            "--every",
            "15m",
            "--prompt",
            "worktree next step",
        ])
        .output()
        .expect("loop add");
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    loop_ok(&env, &["loop", "run", "wait-worktree"]);
    assert_pending_message(&env, "sess-loop-worktree", "worktree next step");
}

#[test]
fn loop_guarded_wait_to_a_dead_target_fails_its_run() {
    let env = Env::new();
    env.record(&env.project_root);
    let config = format!(
        "[tasks.dead]\nwait = {{ kind = \"claude\", session = \"sess-dead\", handle = \"@claude\" }}\n\
         prompt = \"wake up\"\ncheck = \"false\"\nroot = \"{}\"\nat = \"07:00\"\n",
        env.project_root.display()
    );
    write_loop_config(&env, &config);
    // A guarded wait runs its guard and message step in a run; an undelivered message fails it.
    // A scheduled run spends the one-shot; a manual fire keeps it.
    for (verb, kept) in [("fire", true), ("run", false)] {
        let before = read_loop_run_records(&env).len();
        let text = loop_ok(&env, &["loop", verb, "dead"]);
        assert!(text.contains("launched"), "{text}");
        let report = wait_for_loop_report_after(&env, "dead", before);
        assert_eq!(report.result, LoopRunResult::Failed, "{verb}");
        assert_eq!(
            std::fs::read_to_string(loop_config_path(&env))
                .unwrap()
                .contains("[tasks.dead]"),
            kept,
            "{verb}"
        );
    }
}

#[test]
fn loop_list_uses_room_arm_stamp_for_next_fire() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.next]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"15m\"\n",
            env.project_root.display()
        ),
    );
    let list = loop_ok(&env, &["loop", "list"]);
    assert!(
        list.lines()
            .any(|line| { line.trim_start().starts_with("next") && line.contains("never fired") })
    );
    write_loop_fire_state(
        &env,
        BTreeMap::from([(
            "next".to_owned(),
            Timestamp::now() - SignedDuration::from_secs(16 * 60),
        )]),
    );
    crate::common::room::seed_sidebar_heartbeat(
        &env.runtime_paths(),
        MuxName::Tmux,
        "rimz-test",
        "sidebar",
    );
    let list = loop_ok(&env, &["loop", "list"]);
    assert!(
        list.lines()
            .any(|line| line.trim_start().starts_with("next") && line.contains("due"))
    );
}

#[test]
fn malformed_schedule_stays_visible_and_manual_action_remains_runnable() {
    let env = Env::new();
    env.record(&env.project_root);
    write_loop_config(
        &env,
        &format!(
            "[tasks.invalid]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"15m\"\nat = \"07:00\"\n",
            env.project_root.display()
        ),
    );

    let list = loop_ok(&env, &["loop", "list"]);
    let show = loop_ok(&env, &["loop", "show", "invalid"]);
    let fire = loop_ok(&env, &["loop", "fire", "invalid"]);

    assert!(
        list.lines()
            .any(|line| { line.trim_start().starts_with("invalid") && line.contains("invalid:") })
            && show.contains("invalid · routine · invalid:")
            && fire.contains("launched"),
        "{list}\n{show}\n{fire}"
    );
    wait_for_loop_report(&env, "invalid");

    env.install_agent_hooks("claude");
    register_running_agent(&env, "manual-invalid-session", "feature-manual");
    let mut config: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).unwrap()).unwrap();
    let entry = config.tasks.0.get_mut("invalid").unwrap();
    entry.check = None;
    entry.wait = Some(TaskTarget {
        kind: AgentKind::new_unchecked("claude"),
        session: AgentSessionId::from("manual-invalid-session"),
        handle: "@claude#feature-manual".to_owned(),
    });
    write_loop_config(&env, &toml::to_string(&config).unwrap());
    let fire = loop_ok(&env, &["loop", "fire", "invalid"]);
    assert!(fire.contains("delivered"), "{fire}");
}

#[test]
fn loop_legacy_run_record_renders_through_list_and_show() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.legacy]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"15m\"\n",
            env.project_root.display()
        ),
    );
    append_legacy_loop_record(&env, "legacy", LoopRunResult::Completed);
    let list = loop_ok(&env, &["loop", "list"]);
    let show = loop_ok(&env, &["loop", "show", "legacy"]);
    assert!(
        list.lines()
            .any(|line| { line.trim_start().starts_with("legacy") && line.contains('✓') })
            && show.contains("✓ healthy · last run")
            && show.contains(", completed")
            && !show.contains("LAST RUN")
            && !show.contains("RECENT RUNS")
            && !show.contains("MODE")
            && !show.contains("last failure"),
        "{list}\n{show}"
    );
    assert!(show.contains("  action: run check · true"), "{show}");
    assert!(
        show.lines().any(|line| line.starts_with("  source: "))
            && !show.contains("throttle:")
            && !show.contains("THROTTLE"),
        "{show}"
    );
}

#[test]
fn loop_show_nonspawning_tasks_have_no_throttle_readings() {
    let env = Env::new();
    let core = env.rimz_home().join("config.toml");
    let base = std::fs::read_to_string(&core).unwrap_or_default();
    for (limit, switch) in [
        ("", ""),
        ("", "throttle='on'\n"),
        ("", "throttle='off'\n"),
        ("[throttle]\nmax-parallel-agents = 1\n", ""),
    ] {
        std::fs::write(&core, format!("{base}\n{limit}")).unwrap();
        write_loop_config(
            &env,
            &format!(
                "[tasks.check]\n{switch}check = \"true\"\nroot = {:?}\nevery = \"1h\"\n\
                 [tasks.wake]\n{switch}root = {:?}\nevery = \"1h\"\n\
                 wait = {{ kind = \"claude\", session = \"session\", handle = \"@reader\" }}\n",
                env.project_root, env.project_root,
            ),
        );
        for name in ["check", "wake"] {
            let show = loop_ok(&env, &["loop", "show", name]);
            assert!(
                !show.contains("throttle:")
                    && !show.contains("load:")
                    && !show.contains("THROTTLE"),
                "{show}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn loop_show_wait_survives_unavailable_workspace_enrichment() {
    use std::os::unix::fs::PermissionsExt;

    let env = Env::new();
    let git_bin = env.home_root.join("git-bin");
    let git = write_path_shim(&git_bin, "git", "exit 1");
    std::fs::set_permissions(git, std::fs::Permissions::from_mode(0o644)).unwrap();
    write_loop_config(
        &env,
        &format!(
            "[tasks.wake]\nroot = {:?}\nevery = \"1h\"\n\
             wait = {{ kind = \"claude\", session = \"session\", handle = \"@reader\" }}\n",
            env.project_root,
        ),
    );
    let output = env
        .rimz()
        .env("PATH", git_bin)
        .args(["loop", "show", "wake"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "workspace enrichment must not prevent showing the task: {output:?}"
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains("@reader") && !text.contains(" (you)"),
        "{text}"
    );
}

#[test]
fn loop_show_wait_survives_unreadable_store_enrichment() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.wake]\nroot = {:?}\nevery = \"1h\"\n\
             wait = {{ kind = \"claude\", session = \"session\", handle = \"@reader\" }}\n",
            env.project_root,
        ),
    );
    let store = env.store();
    let events = rimz::StatePaths::history_paths(&store.paths().root).events_log;
    std::fs::create_dir_all(events).unwrap();
    assert!(store.snapshot_cached().is_err());
    let output = env.rimz().args(["loop", "show", "wake"]).output().unwrap();
    assert!(
        output.status.success(),
        "store enrichment must not prevent showing the task: {output:?}"
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains("@reader") && !text.contains(" (you)"),
        "{text}"
    );
}

#[test]
fn loop_overlap_records_holder_and_preserves_one_shot() {
    let env = Env::new();
    env.record(&env.project_root);
    loop_ok(
        &env,
        &["loop", "add", "busy", "--check", "true", "--at", "07:00"],
    );
    // A stored bare `at` row: `loop add --at` now writes `fire-at`, whose one scheduled
    // attempt ends the row whatever its result.
    let mut tasks = read_loop_instances(&env);
    let busy = tasks.0.get_mut("busy").unwrap();
    busy.fire_at = None;
    busy.at = Some("07:00".to_owned());
    write_loop_instances(&env, tasks);
    let holder = RunLockInfo {
        pid: 42_424,
        started_at: Timestamp::now() - SignedDuration::from_secs(25 * 60),
    };
    let lock_file = hold_loop_run_lock(&loop_run_lock_path(&env, "busy"), &holder);

    let run = loop_ok(&env, &["loop", "run", "busy"]);
    assert!(
        run.contains("previous run still active (pid 42424, started 25m ago) — skipped"),
        "{run}"
    );
    assert!(read_loop_instances(&env).0.contains_key("busy"));
    let record = last_loop_record(&env);
    assert_eq!(record.result, LoopRunResult::Overlapped);
    let show = loop_ok(&env, &["loop", "show", "busy"]);
    assert!(
        show.starts_with("busy · one-off · once at 07:00 · ▸ running 25m · pid 42424\n")
            && show.contains("  1 fire skipped while the active run holds the lock\n")
            && show.contains("  stop with `rimz loop stop busy`\n")
            && !show
                .lines()
                .any(|line| line.trim_start().starts_with("active:"))
            && !show.contains("○ overlapped")
            && !show.contains("LAST RUN"),
        "{show}"
    );
    let logs = loop_ok(&env, &["loop", "logs", "busy"]);
    assert!(
        logs.contains("○ overlapped") && logs.ends_with("\n\n▸ running 25m · pid 42424\n"),
        "{logs}"
    );
    let failed = loop_ok(&env, &["loop", "logs", "busy", "--failed"]);
    assert!(!failed.contains("▸ running"), "{failed}");
    let json = loop_ok(&env, &["loop", "show", "busy", "--json"]);
    assert!(json.contains("\"result\": \"overlapped\""), "{json}");
    let json: serde_json::Value = serde_json::from_str(&json).expect("show json");
    assert_eq!(
        (
            record.error.as_deref(),
            logs.contains("previous run still active — skipped"),
            logs.contains("holder: pid 42424 · started 25m before this fire"),
            &json["runs"][0]["holder"]["pid"],
        ),
        (
            Some("previous run still active — skipped"),
            true,
            true,
            &json!(42424),
        ),
        "{logs}\n{json}"
    );
    assert_eq!(
        json["running"],
        json!({ "pid": 42_424, "started_at": holder.started_at, "run_id": null })
    );
    lock_file.unlock().expect("unlock loop run lock");
    let json = loop_ok(&env, &["loop", "show", "busy", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&json).expect("show json");
    assert!(json["running"].is_null(), "{json}");
    assert!(
        json.as_object()
            .is_some_and(|keys| keys.contains_key("running")),
        "{json}"
    );
}

#[test]
fn loop_list_shows_a_running_task_beside_its_last_result() {
    let env = Env::new();
    env.record(&env.project_root);
    loop_ok(
        &env,
        &["loop", "add", "busy", "--check", "true", "--every", "1h"],
    );
    loop_ok(&env, &["loop", "fire", "busy"]);
    // The fire returns once its run starts; LAST reads the run's report.
    wait_for_loop_report(&env, "busy");
    let row = |list: &str, head: &str| -> Vec<String> {
        list.lines()
            .find(|line| line.trim_start().starts_with(head))
            .unwrap_or_else(|| panic!("no `{head}` line: {list}"))
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    };

    let list = loop_ok(&env, &["loop", "list"]);
    assert_eq!(
        row(&list, "NAME"),
        ["NAME", "RUNS", "NEXT", "ACTION", "LAST"]
    );
    let idle = row(&list, "busy");
    assert!(idle.contains(&"✓".to_owned()), "{list}");
    assert!(!list.contains("▸ running"), "{list}");

    let holder = RunLockInfo {
        pid: 42_424,
        started_at: Timestamp::now() - SignedDuration::from_secs(3 * 60),
    };
    let lock_file = hold_loop_run_lock(&loop_run_lock_path(&env, "busy"), &holder);
    let list = loop_ok(&env, &["loop", "list"]);
    let running = row(&list, "busy");
    assert!(
        running.ends_with(&["▸".into(), "running".into(), "3m".into()]),
        "{list}"
    );
    let value: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "list", "--json"])).unwrap();
    assert_eq!(value["rooms"][0]["tasks"][0]["last"]["result"], "completed");
    assert_eq!(
        value["rooms"][0]["tasks"][0]["running_since"],
        json!(holder.started_at)
    );
    lock_file.unlock().expect("unlock loop run lock");
}

#[test]
fn loop_list_adds_one_row_per_held_lock_no_task_claims() {
    let env = Env::new();
    env.record(&env.project_root);
    loop_ok(
        &env,
        &["loop", "add", "fan", "--check", "true", "--every", "1h"],
    );
    let holder = RunLockInfo {
        pid: 42_424,
        started_at: Timestamp::now() - SignedDuration::from_secs(3 * 60),
    };
    let _locks = ["fan", "fan-ws_0123456789abcdef01234567", "later"]
        .map(|name| hold_loop_run_lock(&loop_run_lock_path(&env, name), &holder));

    let list = loop_ok(&env, &["loop", "list"]);
    assert!(list.contains("ROUTINE") && list.contains("DONE"), "{list}");
    let fan = list
        .lines()
        .find(|line| line.trim_start().starts_with("fan "))
        .unwrap();
    let later = list
        .lines()
        .find(|line| line.trim_start().starts_with("later "))
        .unwrap();
    assert!(
        fan.contains("every 1h") && fan.contains("▸ running 3m"),
        "{list}"
    );
    assert!(later.contains("▸ running 3m"), "{list}");
}

#[test]
fn loop_list_warns_once_when_the_run_locks_cannot_be_listed() {
    let env = Env::new();
    env.record(&env.project_root);
    for name in ["first", "second"] {
        loop_ok(
            &env,
            &["loop", "add", name, "--check", "true", "--every", "1h"],
        );
    }
    // A file where the locks directory belongs makes listing it fail.
    let locks = loop_run_lock_path(&env, "first");
    let locks = locks.parent().expect("locks directory");
    let _ = std::fs::remove_dir_all(locks);
    std::fs::create_dir_all(locks.parent().expect("runtime root")).unwrap();
    std::fs::write(locks, "").unwrap();

    let list = env.rimz().args(["loop", "list"]).output().unwrap();
    let stderr = String::from_utf8(list.stderr).unwrap();
    let stdout = String::from_utf8(list.stdout).unwrap();
    assert!(list.status.success(), "loop list: {stderr}");
    assert!(
        stderr.matches("warning:").count() == 1
            && stderr.contains(locks.to_str().expect("utf-8 locks path"))
            && stderr.contains("no active run is shown"),
        "loop list: {stderr}"
    );
    assert!(
        stdout.contains("first") && stdout.contains("second") && !stdout.contains("▸ running"),
        "{stdout}"
    );
}

#[test]
fn loop_watch_keeps_acting_results_after_skipped_signals() {
    use rimz::harness::schedule::run_log::SignalRecord;

    let env = Env::new();
    seed_loop_watch_room(&env);
    write_loop_config(
        &env,
        &toml::to_string(&LoopConfig {
            tasks: Tasks(
                ["good", "bad", "heard"]
                    .map(|name| {
                        (
                            name.into(),
                            TaskEntry {
                                root: env.project_root.clone(),
                                check: Some("true".into()),
                                signal: Some("ci.failed".into()),
                                ..TaskEntry::default()
                            },
                        )
                    })
                    .into(),
            ),
            ..LoopConfig::default()
        })
        .unwrap(),
    );
    let mut records = Vec::new();
    for (name, result) in [
        ("good", LoopRunResult::Completed),
        ("bad", LoopRunResult::Failed),
    ] {
        let mut record = LoopRunRecord::new(name, result, LoopRunMode::Manual, 0);
        record.root = Some(env.project_root.clone());
        record.at = Timestamp::now() - SignedDuration::from_mins(10);
        records.push(record);
    }
    for minutes in [7, 6, 5] {
        for name in ["good", "bad", "heard"] {
            let mut record =
                LoopRunRecord::new(name, LoopRunResult::SignalSkipped, LoopRunMode::Manual, 0);
            record.root = Some(env.project_root.clone());
            record.at = Timestamp::now() - SignedDuration::from_mins(minutes);
            record.signal = Some(SignalRecord {
                name: "ci.passed".parse().unwrap(),
                payload: serde_json::Map::new(),
            });
            records.push(record);
        }
    }
    write_loop_run_records(&env, &records);

    let output = loop_watch_frames(
        &env,
        &[
            "✓ 10m ago",
            "✗ failed 10m ago",
            "✗ 1 needs you",
            "heard ci.passed 5m ago",
        ],
    );
    let good = output.lines().find(|line| line.contains("good")).unwrap();
    assert!(good.contains("✓ 10m ago"), "{output}");
    let bad = output.lines().find(|line| line.contains("bad")).unwrap();
    assert!(bad.contains("✗ failed 10m ago"), "{output}");
    assert!(output.contains("✗ 1 needs you"), "{output}");
    assert!(output.contains("heard ci.passed 5m ago"), "{output}");
    assert!(!output.contains("skipped"), "{output}");
}

#[test]
fn loop_watch_scopes_to_the_whole_callers_room() {
    let env = Env::new();
    seed_loop_watch_room(&env);
    let checkout = env.home_root.join("checkout");
    let elsewhere = env.home_root.join("elsewhere");
    std::fs::create_dir(&checkout).unwrap();
    std::fs::create_dir(&elsewhere).unwrap();
    // Opening another room's instances would block the first repaint.
    let remote_instances = env
        .state_path_for(&elsewhere)
        .root
        .join("records/loop-instances.json");
    std::fs::create_dir_all(remote_instances.parent().unwrap()).unwrap();
    nix::unistd::mkfifo(&remote_instances, nix::sys::stat::Mode::S_IRWXU).unwrap();
    write_loop_config(
        &env,
        &toml::to_string(&LoopConfig {
            tasks: Tasks(BTreeMap::from([
                (
                    "local".into(),
                    TaskEntry {
                        root: env.project_root.clone(),
                        check: Some("true".into()),
                        every: Some("1h".into()),
                        ..TaskEntry::default()
                    },
                ),
                (
                    "bound".into(),
                    TaskEntry {
                        root: env.project_root.clone(),
                        dir: Some(checkout),
                        check: Some("true".into()),
                        every: Some("1h".into()),
                        ..TaskEntry::default()
                    },
                ),
                (
                    "remote-task".into(),
                    TaskEntry {
                        root: elsewhere,
                        check: Some("true".into()),
                        every: Some("1h".into()),
                        ..TaskEntry::default()
                    },
                ),
            ])),
            ..LoopConfig::default()
        })
        .unwrap(),
    );

    let output = loop_watch_frames(&env, &["local", "bound"]);
    assert!(
        output.contains("local") && output.contains("bound"),
        "{output}"
    );
    assert!(
        !output.contains("remote-task") && !output.contains("~/elsewhere"),
        "{output}"
    );
}

#[test]
fn loop_watch_shows_a_run_whose_task_row_is_gone() {
    let env = Env::new();
    seed_loop_watch_room(&env);
    env.record(&env.project_root);
    let holder = RunLockInfo {
        pid: 42_424,
        started_at: Timestamp::now() - SignedDuration::from_secs(3 * 60),
    };
    let _lock = hold_loop_run_lock(&loop_run_lock_path(&env, "later"), &holder);

    let output = loop_watch_frames(&env, &["▸ 1 running", "later", "▸ running 3m"]);

    assert!(
        output.contains("▸ 1 running")
            && output.contains("later")
            && output.contains("▸ running 3m")
            && !output.contains("next:")
            && !output.contains("no loop tasks"),
        "{output}"
    );
}

#[test]
fn loop_watch_stays_silent_when_a_task_run_lock_cannot_be_read() {
    let env = Env::new();
    seed_loop_watch_room(&env);
    loop_ok(
        &env,
        &["loop", "add", "probe", "--check", "true", "--every", "1h"],
    );
    // A directory where the run lock file belongs makes opening the lock fail.
    std::fs::create_dir_all(loop_run_lock_path(&env, "probe")).unwrap();

    let output = loop_watch_frames(&env, &["probe", "never fired"]);

    assert!(
        output.contains("probe")
            && !output.contains("▸ running")
            && !output.contains("warning")
            && !output.contains("cannot read"),
        "{output}"
    );
}

#[test]
fn loop_watch_stays_silent_when_the_locks_directory_cannot_be_listed() {
    let env = Env::new();
    seed_loop_watch_room(&env);
    let elsewhere = env.home_root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    write_loop_config(
        &env,
        &format!(
            "[tasks.remote-task]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"15m\"\n",
            elsewhere.display()
        ),
    );
    // A file where the project's locks directory belongs makes listing it fail.
    let locks = loop_run_lock_path(&env, "any");
    let locks = locks.parent().expect("locks directory");
    let _ = std::fs::remove_dir_all(locks);
    std::fs::create_dir_all(locks.parent().expect("runtime root")).unwrap();
    std::fs::write(locks, "").unwrap();

    let output = loop_watch_frames(&env, &["no loop tasks; add one with `rimz loop add …`"]);

    assert!(
        output.starts_with("loop\n\nno loop tasks")
            && !output.contains("~/elsewhere")
            && !output.contains("remote-task")
            && !output.contains("~/project")
            && !output.contains("warning")
            && !output.contains("listing loop run locks"),
        "{output}"
    );
}

#[cfg(unix)]
#[test]
fn loop_stop_reaches_a_run_holding_only_a_per_checkout_lock() {
    let env = Env::new();
    env.record(&env.project_root);
    loop_ok(
        &env,
        &["loop", "add", "fan", "--check", "true", "--every", "1h"],
    );
    let mut runner = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn holder");
    let holder = RunLockInfo {
        pid: runner.id(),
        started_at: Timestamp::now(),
    };
    let lock_file = hold_loop_run_lock(
        &loop_run_lock_path(&env, "fan-ws_0123456789abcdef01234567"),
        &holder,
    );

    let stop = env
        .rimz()
        .args(["loop", "stop", "fan"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn loop stop");
    // `loop stop` signals the holder only for a lock it found held.
    runner.wait().expect("holder stopped by loop stop");
    lock_file.unlock().expect("unlock loop run lock");
    let stop = stop.wait_with_output().expect("loop stop");
    let stdout = String::from_utf8_lossy(&stop.stdout);
    assert!(
        stop.status.success() && stdout.contains("loop `fan`: stopped · SIGTERM"),
        "{stdout}{}",
        String::from_utf8_lossy(&stop.stderr)
    );
}

#[test]
fn loop_stop_without_active_run_reports_no_active_run() {
    let env = Env::new();
    env.record(&env.project_root);
    loop_ok(
        &env,
        &["loop", "add", "idle", "--check", "true", "--every", "15m"],
    );
    let stopped = loop_ok(&env, &["loop", "stop", "idle"]);
    assert!(stopped.contains("loop `idle`: no active run"), "{stopped}");
}

#[cfg(unix)]
#[test]
fn terminal_check_runs_interactive_shell_to_completion() {
    if which::which("bash").is_err() {
        crate::common::skip("bash not on PATH");
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "shell-probe",
            "--every",
            "15m",
            // A bound on the failure only: the check exits on its own, and a
            // shell stopped by terminal job control is what runs into it.
            "--timeout",
            "60s",
            "--check",
            "bash -i -c 'printf shell-ready'",
        ],
    );
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = CommandBuilder::new(env.rimz_bin());
    env.pin_pty_command(&mut command);
    command.args(["loop", "fire", "shell-probe"]);
    command.cwd(&env.project_root);
    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let output = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = reader.read_to_end(&mut bytes);
        bytes
    });
    child.wait().unwrap();
    drop(pair.master);
    output.join().unwrap();
    let report = wait_for_loop_report(&env, "shell-probe");
    let journal = loop_run_journal(&env, &env.project_root, &report);
    assert_eq!(report.result, LoopRunResult::Completed, "{journal}");
    assert!(journal.contains("shell-ready"), "{journal}");
}

/// The fire returns at run start while the terminal's session leader lives on, as a user's
/// shell does, so the check's terminal job control must not reach it.
#[cfg(unix)]
#[test]
fn terminal_check_outlived_by_its_session_leader_runs_to_completion() {
    if which::which("bash").is_err() {
        crate::common::skip("bash not on PATH");
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "shell-probe",
            "--every",
            "15m",
            "--timeout",
            "20s",
            "--check",
            "bash -i -c 'printf shell-ready'",
        ],
    );
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = CommandBuilder::new("sh");
    env.pin_pty_command(&mut command);
    command.args(["-c", "\"$1\" loop fire shell-probe; sleep 90", "leader"]);
    command.arg(env.rimz_bin());
    command.cwd(&env.project_root);
    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let output = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = reader.read_to_end(&mut bytes);
        bytes
    });
    let report = wait_for_loop_report(&env, "shell-probe");
    let leader_alive = child.try_wait().unwrap().is_none();
    let _ = child.kill();
    child.wait().unwrap();
    drop(pair.master);
    output.join().unwrap();
    let journal = loop_run_journal(&env, &env.project_root, &report);
    assert!(leader_alive, "the session leader outlives the run");
    assert_eq!(report.result, LoopRunResult::Completed, "{journal}");
    assert!(journal.contains("shell-ready"), "{journal}");
}

#[cfg(unix)]
#[test]
fn manual_check_clears_the_agent_identity_overlay() {
    let env = Env::new();
    env.record(&env.project_root);
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "identity",
            "--every",
            "15m",
            "--check",
            "test -z \"${RIMZ_AGENT_ID+x}${RIMZ_AGENT_KIND+x}${RIMZ_AGENT_NAME+x}${RIMZ_AGENT_PROFILE+x}${RIMZ_AGENT_ROLE+x}${RIMZ_AGENT_MODEL+x}${RIMZ_AGENT_EFFORT+x}${RIMZ_AGENT_BUDGET+x}${RIMZ_AGENT_PID+x}\" && printf 'clean %s' \"${RIMZ_CHANNEL-unset}\"",
        ],
    );
    let output = env
        .rimz()
        .args(["loop", "fire", "identity"])
        .envs([
            ("RIMZ_CHANNEL", "probe"),
            ("RIMZ_AGENT_ID", "stale-launch"),
            ("RIMZ_AGENT_KIND", "claude"),
            ("RIMZ_AGENT_NAME", "old-name"),
            ("RIMZ_AGENT_PROFILE", "old-profile"),
            ("RIMZ_AGENT_ROLE", "old-role"),
            ("RIMZ_AGENT_MODEL", "old-model"),
            ("RIMZ_AGENT_EFFORT", "old-effort"),
            ("RIMZ_AGENT_BUDGET", "99"),
            ("RIMZ_AGENT_PID", "1"),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = wait_for_loop_report(&env, "identity");
    let journal = loop_run_journal(&env, &env.project_root, &report);
    assert_eq!(report.result, LoopRunResult::Completed, "{journal}");
    assert!(journal.contains("clean unset"), "{journal}");
}

#[cfg(unix)]
#[test]
fn loop_stop_cancels_a_spawn_run_in_an_unroomed_project() {
    loop_stop_of_a_spawn_run(false);
}

/// The run stops and its lock is released, then the command fails naming
/// the pane it could not close and the stop that does close it.
#[cfg(unix)]
#[test]
fn loop_stop_fails_after_stopping_a_run_whose_pane_stays_open() {
    loop_stop_of_a_spawn_run(true);
}

#[cfg(unix)]
fn loop_stop_of_a_spawn_run(pane_open: bool) {
    let env = Env::new();
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(&env);
    trust_codex_project(&env, &env.project_root);
    let agent_bin = crate::common::write_failing_agent_shim(&env, "codex", 1);
    let shell = write_fake_login_shell(&env, "rimz-test-sh", &[]);
    let workspace = env.resolve_workspace(&env.project_root);
    crate::common::room::seed_live_zellij_room(
        &env.runtime_paths(),
        &workspace.session_name,
        Vec::new(),
    );
    let pane_fixture = env.project_root.join("panes.json");
    std::fs::write(&pane_fixture, "[]").unwrap();
    let command = || {
        let mut command = env.rimz();
        command
            .args(["--mux", "zellij"])
            .env("SHELL", &shell)
            .env("PATH", path_with_front(&agent_bin))
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env(
                "RIMZ_ZELLIJ_BIN",
                crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace")),
            )
            .env("RIMZ_TEST_ZELLIJ_LOG", env.project_root.join("spawn.log"))
            .env("RIMZ_TEST_ZELLIJ_LOG_LAYOUTS", "1")
            .env("RIMZ_TEST_ZELLIJ_LIST_PANES", "[]")
            .env("ZELLIJ_PANE_ID", "1")
            .env(
                "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
                format!("{} [Created 1s ago]\n", workspace.session_name),
            );
        command
    };
    loop_ok(
        &env,
        &[
            "loop", "add", "spawn", "--agent", "codex", "--prompt", "fix it", "--every", "15m",
        ],
    );
    let paths = env.state_path_for(&env.project_root);
    assert!(!paths.workspace_record.exists());
    let fired = command().args(["loop", "run", "spawn"]).output().unwrap();
    assert!(fired.status.success(), "{fired:?}");
    // The fire's workflow run launches the agent step's supervised run.
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    let run = loop {
        let records = rimz::harness::run::list(&paths).unwrap_or_default();
        if let Some(record) = records.first() {
            if record.status == RunStatus::Running {
                break record.clone();
            }
            // The trace mux opens no provider process, so supply its startup hook.
            if record.status == RunStatus::Pending {
                let mut hook = env.hook_command("codex");
                hook.env(rimz::harness::launch::ENV_RUN_ID, record.run_id.as_str())
                    .env(
                        rimz::harness::launch::ENV_AGENT_NAME,
                        record.agent_name.as_ref().unwrap().as_str(),
                    );
                let output = env
                    .spawn_payload(
                        hook,
                        &json!({
                            "hook_event_name": "SessionStart",
                            "session_id": "loop-spawn-session",
                            "cwd": env.project_root,
                        })
                        .to_string(),
                    )
                    .wait_with_output()
                    .unwrap();
                env.drain_hooks();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for Running run"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert!(run.workflow.is_some(), "{run:?}");
    assert!(paths.workspace_record.is_file());
    assert!(
        rimz::StatePaths::history_paths(&paths.root)
            .events_log
            .is_file()
    );

    let mut stop = command();
    if pane_open {
        rimz::harness::run::record_pane(
            &paths,
            &run.run_id,
            rimz::ids::PaneId::from_parts(rimz::ids::MuxName::Zellij, "terminal_51"),
        )
        .unwrap();
        stop.env("RIMZ_TEST_ZELLIJ_FAIL_CLOSE_PANE", "1").env(
            "RIMZ_TEST_ZELLIJ_LIST_PANES",
            r#"[{"id":51,"is_plugin":false,"tab_id":1,"title":"sh"}]"#,
        );
    }
    let stopped = stop.args(["loop", "stop", "spawn"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&stopped.stderr);
    if pane_open {
        // The workflow's cancel cannot stop an attempt whose pane stays open.
        assert!(!stopped.status.success(), "{stopped:?}");
        assert!(
            stderr.contains(&format!(
                "loop `spawn`: workflow run {}",
                run.workflow.as_ref().unwrap().run
            )) && stderr.contains("not canceled: error: stopping workflow attempt"),
            "{stderr}"
        );
        return;
    }
    assert!(
        String::from_utf8_lossy(&stopped.stdout).contains("loop `spawn`: stopped · run"),
        "{stopped:?}"
    );
    assert!(stopped.status.success(), "{stderr}");
    assert_eq!(
        rimz::harness::run::load(&paths, &run.run_id)
            .unwrap()
            .status,
        RunStatus::Canceled
    );
    assert_eq!(
        wait_for_loop_report(&env, "spawn").result,
        LoopRunResult::Canceled
    );
    for entry in std::fs::read_dir(env.rimz_home().join("ws")).unwrap() {
        let name = entry.unwrap().file_name();
        let name = name.to_string_lossy();
        assert!(
            !name
                .strip_prefix("ws-")
                .is_some_and(|suffix| suffix.len() == 24
                    && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())),
            "fallback workspace directory: {name}"
        );
    }

    loop_ok(&env, &["gc", "--all"]);
    assert!(paths.root.is_dir());
    assert!(
        paths
            .runs_dir
            .join(format!("{}.json", run.run_id))
            .is_file()
    );
    assert_eq!(
        rimz::harness::run::load(&paths, &run.run_id)
            .unwrap()
            .status,
        RunStatus::Canceled
    );
}

#[cfg(unix)]
#[test]
fn loop_stop_terminates_holder_and_records_cancellation() {
    let env = Env::new();
    env.record(&env.project_root);
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "stuck",
            "--check",
            "touch check-ready; parent=$PPID; while kill -0 \"$parent\" 2>/dev/null; do sleep 1; done",
            "--at",
            "23:59",
        ],
    );
    loop_ok(&env, &["loop", "run", "stuck"]);
    wait_for_path(&env.project_root.join("check-ready"));
    loop_ok(&env, &["loop", "remove", "stuck"]);
    let stopped = loop_ok(&env, &["loop", "stop", "stuck"]);
    assert!(stopped.contains("stopped · run"), "{stopped}");
    let record = wait_for_loop_report(&env, "stuck");
    assert_eq!(record.result, LoopRunResult::Canceled);
    let list = loop_ok(&env, &["loop", "list"]);
    assert!(
        list.contains("DONE") && list.contains("stuck") && list.contains("canceled"),
        "{list}"
    );
}

/// A scheduled `--in` spawn one-shot whose runner holds its lock with a Running
/// run record, after the fire consumed its instance row.
#[cfg(unix)]
struct ConsumedSpawn {
    shell: std::path::PathBuf,
    agent_bin: std::path::PathBuf,
    pane_fixture: std::path::PathBuf,
    session_name: String,
}

#[cfg(unix)]
impl ConsumedSpawn {
    fn command(&self, env: &Env) -> Command {
        let mut command = env.rimz();
        command
            .args(["--mux", "zellij"])
            .env("SHELL", &self.shell)
            .env("PATH", path_with_front(&self.agent_bin))
            .env("RIMZ_TEST_PANE_LIST", &self.pane_fixture)
            .env(
                "RIMZ_ZELLIJ_BIN",
                crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace")),
            )
            .env("RIMZ_TEST_ZELLIJ_LOG", env.project_root.join("spawn.log"))
            .env("RIMZ_TEST_ZELLIJ_LIST_PANES", "[]")
            .env("ZELLIJ_PANE_ID", "1")
            .env(
                "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
                format!("{} [Created 1s ago]\n", self.session_name),
            );
        command
    }
}

#[cfg(unix)]
fn start_consumed_spawn(env: &Env, name: &str) -> (ConsumedSpawn, RunRecord) {
    env.record(&env.project_root);
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(env);
    trust_codex_project(env, &env.project_root);
    let pane_fixture = env.project_root.join("panes.json");
    std::fs::write(&pane_fixture, "[]").unwrap();
    let workspace = env.resolve_workspace(&env.project_root);
    crate::common::room::seed_live_zellij_room(
        &env.runtime_paths(),
        &workspace.session_name,
        Vec::new(),
    );
    let fixture = ConsumedSpawn {
        shell: write_fake_login_shell(env, "rimz-test-sh", &[]),
        agent_bin: crate::common::write_failing_agent_shim(env, "codex", 1),
        pane_fixture,
        session_name: workspace.session_name.to_string(),
    };
    loop_ok(
        env,
        &[
            "loop", "add", name, "--agent", "codex", "--prompt", "fix it", "--in", "30m",
        ],
    );
    assert!(read_loop_instances(env).0.contains_key(name));
    let paths = env.state_path_for(&env.project_root);
    let fired = fixture
        .command(env)
        .args(["loop", "run", name])
        .output()
        .unwrap();
    assert!(fired.status.success(), "{fired:?}");
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    let run = loop {
        let records = rimz::harness::run::list(&paths).unwrap_or_default();
        if let Some(record) = records.first() {
            if record.status == RunStatus::Running {
                break record.clone();
            }
            // The trace mux opens no provider process, so supply its startup hook.
            if record.status == RunStatus::Pending {
                let mut hook = env.hook_command("codex");
                hook.env(rimz::harness::launch::ENV_RUN_ID, record.run_id.as_str())
                    .env(
                        rimz::harness::launch::ENV_AGENT_NAME,
                        record.agent_name.as_ref().unwrap().as_str(),
                    );
                let output = env
                    .spawn_payload(
                        hook,
                        &json!({
                            "hook_event_name": "SessionStart",
                            "session_id": "loop-spawn-session",
                            "cwd": env.project_root,
                        })
                        .to_string(),
                    )
                    .wait_with_output()
                    .unwrap();
                env.drain_hooks();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for Running run"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert!(run.workflow.is_some(), "{run:?}");
    assert!(!read_loop_instances(env).0.contains_key(name));
    (fixture, run)
}

#[cfg(unix)]
#[test]
fn loop_stop_reaches_a_consumed_one_shot_run() {
    let env = Env::new();
    let (fixture, run) = start_consumed_spawn(&env, "later");
    let paths = env.state_path_for(&env.project_root);

    let stopped = fixture
        .command(&env)
        .args(["loop", "stop", "later"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&stopped.stdout);
    assert!(
        stopped.status.success(),
        "{}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    assert!(stdout.contains("loop `later`: stopped · run"), "{stdout}");
    assert_eq!(
        rimz::harness::run::load(&paths, &run.run_id)
            .unwrap()
            .status,
        RunStatus::Canceled
    );
    let record = wait_for_loop_report(&env, "later");
    assert_eq!(record.result, LoopRunResult::Canceled);
    assert_eq!(record.kind, Some(rimz::harness::schedule::TaskKind::OneOff));
    assert!(!read_loop_instances(&env).0.contains_key("later"));
    let list = loop_ok(&env, &["loop", "list"]);
    assert!(
        list.contains("DONE") && list.contains("later") && list.contains("canceled"),
        "{list}"
    );
    let show = loop_ok(&env, &["loop", "show", "later"]);
    assert!(
        show.contains("canceled") && !show.contains("▸ running"),
        "{show}"
    );
}

#[test]
fn loop_commands_reject_an_unknown_name_with_a_free_lock() {
    let env = Env::new();
    env.record(&env.project_root);
    for command in ["show", "logs", "stop"] {
        let (_stdout, error) = loop_fail(&env, &["loop", command, "ghost"]);
        assert!(error.contains("no loop task named `ghost`"), "{error}");
    }
    let paths = env.state_path_for(&env.project_root);
    paths.ensure_dirs().unwrap();
    let mut run = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "go".to_owned(),
        env.project_root.clone(),
    );
    run.status = RunStatus::Running;
    run.loop_task = Some("ghost".to_owned());
    rimz::harness::run::create(&paths, &run).unwrap();
    for command in ["show", "logs", "stop"] {
        let (_stdout, error) = loop_fail(&env, &["loop", command, "ghost"]);
        assert!(error.contains("no loop task named `ghost`"), "{error}");
    }
}

#[test]
fn loop_show_and_logs_drop_only_the_active_run_when_the_lock_lookup_fails() {
    let env = Env::new();
    env.record(&env.project_root);
    loop_ok(
        &env,
        &["loop", "add", "probe", "--check", "true", "--every", "1h"],
    );
    let before = read_loop_run_records(&env).len();
    loop_ok(&env, &["loop", "fire", "probe"]);
    // The fire returns once its run starts; the views below read the run's report.
    wait_for_loop_report_after(&env, "probe", before);
    // A directory where the run lock file belongs makes opening the lock fail.
    for name in ["probe", "ghost"] {
        let lock = loop_run_lock_path(&env, name);
        let _ = std::fs::remove_file(&lock);
        std::fs::create_dir_all(&lock).unwrap();
    }
    let display = |command: &str| {
        let output = env
            .rimz()
            .args(["loop", command, "probe"])
            .output()
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(output.status.success(), "loop {command}: {stderr}");
        assert_eq!(
            stderr.matches("cannot read the loop run lock").count(),
            1,
            "loop {command}: {stderr}"
        );
        assert!(!stdout.contains("▸ running"), "{stdout}");
        stdout
    };
    let list = env.rimz().args(["loop", "list"]).output().unwrap();
    let stderr = String::from_utf8(list.stderr).unwrap();
    let stdout = String::from_utf8(list.stdout).unwrap();
    assert!(list.status.success(), "loop list: {stderr}");
    assert_eq!(
        stderr
            .matches("cannot read the loop run lock of `probe`")
            .count(),
        1,
        "loop list: {stderr}"
    );
    assert!(
        stdout.contains("probe") && !stdout.contains("▸ running"),
        "{stdout}"
    );

    let show = display("show");
    assert!(
        show.contains("every 1h") && show.contains("manual"),
        "{show}"
    );
    loop_ok(&env, &["loop", "remove", "probe"]);
    let history = display("logs");
    assert!(history.contains("manual"), "{history}");
    let (stdout, stderr) = loop_fail(&env, &["loop", "show", "probe"]);
    assert!(stdout.is_empty(), "{stdout}");
    assert_eq!(
        stderr
            .matches("cannot read the loop run lock of `probe`")
            .count(),
        1,
        "{stderr}"
    );
    assert_eq!(
        &stderr[stderr.find("error: ").unwrap()..],
        retained_runs_refusal(&stderr, "probe", 1)
    );
    let (_stdout, error) = loop_fail(&env, &["loop", "logs", "ghost"]);
    assert!(
        error.contains("cannot read the loop run lock")
            && error.contains("no loop task named `ghost`"),
        "{error}"
    );
}

#[test]
fn loop_add_persists_machine_and_project_signal_triggers() {
    let env = Env::new();
    let added = loop_ok(
        &env,
        &[
            "loop",
            "add",
            "machine-signal",
            "--check",
            "true",
            "--on",
            "any",
            "--signal",
            "ci.failed",
            "--match",
            "branch=feature",
        ],
    );
    assert!(
        added.contains("trigger: fires on ci.failed [branch=feature]"),
        "{added}"
    );
    let machine: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).expect("read loop config"))
            .expect("parse loop config");
    let machine_task = &machine.tasks.0["machine-signal"];
    assert_eq!(machine_task.signal.as_deref(), Some("ci.failed"));
    assert_eq!(machine_task.on, Some(CheckOn::Any));
    assert_eq!(
        machine_task
            .matches
            .as_ref()
            .and_then(|matches| matches.get("branch"))
            .map(String::as_str),
        Some("feature")
    );

    let project_added = loop_ok(
        &env,
        &[
            "loop",
            "add",
            "project-signal",
            "--project",
            "--check",
            "true",
            "--signal",
            "ci.failed",
            "--match",
            "branch=feature",
        ],
    );
    assert!(
        project_added.contains("fires on ci.failed"),
        "{project_added}"
    );
    let config_path = env.project_root.join(".rimz/config.toml");
    let project_text = std::fs::read_to_string(&config_path).expect("project config");
    assert!(
        project_text.contains("[tasks.project-signal]"),
        "{project_text}"
    );
    assert!(
        project_text.contains("signal = \"ci.failed\"")
            && project_text.contains("[tasks.project-signal.match]")
            && project_text.contains("branch = \"feature\""),
        "{project_text}"
    );
    let trusted = rimz::trust::status_with_roots(&env.project_root, &env.rimz_home())
        .expect("project trust after add");
    assert_eq!(trusted.state, rimz::trust::TrustState::Trusted);
    let trusted_hash = trusted.current_hash.expect("trusted surface hash");

    std::fs::write(
        &config_path,
        project_text.replace("branch = \"feature\"", "branch = \"success\""),
    )
    .expect("change project signal match");
    let stale = rimz::trust::status_with_roots(&env.project_root, &env.rimz_home())
        .expect("project trust after signal change");
    assert_eq!(stale.state, rimz::trust::TrustState::Stale);
    assert_ne!(stale.current_hash.as_deref(), Some(trusted_hash.as_str()));
}

#[cfg(unix)]
#[test]
fn loop_add_from_an_empty_dir_caller_anchors_at_its_checkout() {
    let env = Env::new();
    let workspace = env.resolve_workspace(&env.project_root);
    env.store().record_workspace(&workspace).unwrap();
    let caller_cwd = env.store().paths().empty_agent_dir("caller");
    std::fs::create_dir_all(&caller_cwd).unwrap();
    let mut observation = rimz::agents::AgentLifecycleObservation::new(
        Some("caller".into()),
        rimz::agents::LifecycleSignal::Registered,
    );
    observation.worktree_path = Some(caller_cwd.display().to_string());
    observation.launch.checkout = Some(env.project_root.display().to_string());
    env.store()
        .append_event(&rimz::store::event::EventEnvelope::agent_lifecycle(
            workspace.workspace_id.clone(),
            &workspace.session_name,
            "claude",
            "SessionStart",
            &observation,
        ))
        .unwrap();
    let output = calling_loop(&env, "caller")
        .current_dir(&caller_cwd)
        .envs(rimz::workspace::pin_env(
            &workspace.workspace_id,
            &env.project_root,
        ))
        .args([
            "loop",
            "add",
            "from-empty",
            "--check",
            "true",
            "--every",
            "1h",
        ])
        .output()
        .expect("rimz loop add");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let machine: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).expect("read loop config"))
            .expect("parse loop config");
    let task = &machine.tasks.0["from-empty"];
    assert_eq!(
        canonical(&task.resolved_root()),
        canonical(&env.project_root)
    );
    assert_eq!(
        task.dir, None,
        "the task must not run in the caller's empty dir"
    );
}

#[test]
fn loop_add_rejects_agent_signal_self_waits() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-self-wait", "feature-loop");

    for extra in [Vec::new(), vec!["--match", "handle=@claude"]] {
        let mut args = vec![
            "loop",
            "add",
            "self-wait",
            "--wait",
            "@claude",
            "--signal",
            "agent.idle",
            "--prompt",
            "continue",
        ];
        args.extend(extra);
        let (_stdout, error) = loop_fail(&env, &args);
        assert!(
            error.contains("requires --match handle=<other> or --match session=<other> to avoid waking the target from its own lifecycle signal"),
            "{error}"
        );
        assert!(read_loop_instances(&env).0.is_empty());
    }

    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "peer-wait",
            "--wait",
            "@claude",
            "--signal",
            "agent.idle",
            "--match",
            "handle=@reviewer",
            "--prompt",
            "continue",
        ],
    );
    let instances = read_loop_instances(&env);
    assert_eq!(
        instances.0["peer-wait"]
            .matches
            .as_ref()
            .and_then(|matches| matches.get("handle"))
            .map(String::as_str),
        Some("@reviewer")
    );
}

fn add_claude_account(env: &Env, name: &str) -> std::path::PathBuf {
    let home = env.home_root.join(format!("claude-{name}"));
    let output = env
        .rimz()
        .args(["accounts", "add", "claude", name, "--home"])
        .arg(&home)
        .output()
        .expect("rimz accounts add");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    home
}

#[test]
fn loop_add_resolves_a_markdown_profiles_provider_before_selecting_its_account() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    add_claude_account(&env, "hooloovoo");
    crate::common::write_kind_base(&env, "codex");
    crate::common::write_definition(
        &env,
        "agents",
        "sonnet",
        "description: Sonnet\nagent: claude\nmodel: sonnet\ntools: []",
        "",
    );
    env.publish_accounts(&rimz::agents::account::AccountsCache {
        logins: BTreeMap::from([(
            "claude@hooloovoo".parse().unwrap(),
            rimz::agents::account::ProviderRecord {
                consecutive_failures: 0,
                version_probed_at_ms: None,
                inputs_stamp: None,
                profile: None,
                probe_error: None,
                login: None,
                probed_at_ms: 1,
                ok: true,
                account: None,
            },
        )]),
    });

    let added = loop_ok(
        &env,
        &[
            "loop",
            "add",
            "sonnet-ping-hooloovoo",
            "--agent",
            "sonnet",
            "--account",
            "hooloovoo",
            "--prompt",
            "ping",
            "--in",
            "1m",
            "--timeout",
            "2m",
        ],
    );
    assert!(
        added.contains("action: launches a fresh sonnet pane on account `hooloovoo` in "),
        "{added}"
    );
    let instances = read_loop_instances(&env);
    let task = &instances.0["sonnet-ping-hooloovoo"];
    assert_eq!(task.agent.as_deref(), Some("sonnet"));
    assert_eq!(task.account, Some("hooloovoo".parse().unwrap()));
}

#[test]
fn loop_add_pins_a_declared_account_and_refuses_the_rest() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    add_claude_account(&env, "work");
    let pinned = |name: &'static str, agent: &'static str, pin: &'static str| {
        vec![
            "loop",
            "add",
            name,
            "--agent",
            agent,
            "--account",
            pin,
            "--prompt",
            "x",
            "--every",
            "15m",
        ]
    };

    let (_, undeclared) = loop_fail(&env, &pinned("ghost", "claude", "ghost"));
    assert!(
        undeclared.contains(
            "unknown claude account `ghost`; configured: default, work; run `rimz accounts add claude ghost`"
        ),
        "{undeclared}"
    );
    let (_, unsupported) = loop_fail(&env, &pinned("unsupported", "grok", "work"));
    assert!(
        unsupported.contains("grok has no named accounts; only `default` is available"),
        "{unsupported}"
    );
    assert!(
        !loop_config_path(&env).exists(),
        "a refused add writes no row"
    );

    let added = loop_ok(&env, &pinned("held", "claude", "work"));
    assert!(
        added.contains("action: launches a fresh claude pane on account `work` in "),
        "{added}"
    );
    let config: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).expect("loop config"))
            .expect("parse loop config");
    assert_eq!(
        config.tasks.0["held"].account,
        Some("work".parse().expect("login name"))
    );
    for view in [&["loop", "show", "held"][..], &["loop", "list"]] {
        let shown = loop_ok(&env, view);
        assert!(shown.contains("claude · account work"), "{shown}");
    }
}

#[test]
fn loop_fire_keeps_a_pinned_markdown_profiles_provider_when_unavailable() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    add_claude_account(&env, "rimio");
    loop_ok(
        &env,
        &[
            "accounts",
            "add",
            "codex",
            "rimio",
            "--home",
            env.home_root.join("codex-rimio").to_str().unwrap(),
        ],
    );
    crate::common::write_kind_base(&env, "codex");
    crate::common::write_definition(
        &env,
        "agents",
        "sonnet",
        "description: Sonnet\nagent: claude\nmodel: sonnet\ntools: []",
        "",
    );
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "ping",
            "--agent",
            "sonnet",
            "--account",
            "rimio",
            "--prompt",
            "ping",
            "--every",
            "15m",
            "--throttle",
            "off",
        ],
    );
    env.publish_accounts(&rimz::agents::account::AccountsCache {
        logins: BTreeMap::from([(
            "claude@rimio".parse().unwrap(),
            rimz::agents::account::ProviderRecord {
                consecutive_failures: 0,
                version_probed_at_ms: None,
                inputs_stamp: None,
                profile: None,
                probe_error: None,
                login: Some(rimz::agents::account::RecordedLogin::LoggedOut),
                probed_at_ms: 1,
                ok: true,
                account: None,
            },
        )]),
    });

    let output = env.rimz().args(["loop", "run", "ping"]).output().unwrap();
    let records = read_loop_run_records(&env);
    assert_eq!(
        records.last().map(|record| record.result),
        Some(LoopRunResult::AccountSkipped),
        "must skip the declared Claude account, not try Codex: {records:?}; {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success());
    let record = records.last().unwrap();
    assert!(
        record
            .error
            .as_deref()
            .unwrap()
            .starts_with("claude account `rimio` is logged out")
    );
    assert!(record.run_id.is_none());
    assert!(read_loop_strikes(&env).is_empty());
    assert!(
        rimz::harness::run::list(&env.state_path_for(&env.project_root))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn loop_fire_skips_when_its_pinned_account_cannot_run() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let home = add_claude_account(&env, "work");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "pinned",
            "--agent",
            "claude",
            "--account",
            "work",
            "--prompt",
            "work",
            "--every",
            "15m",
        ],
    );
    let skipped = || {
        loop_ok(&env, &["loop", "run", "pinned"]);
        let record = last_loop_record(&env);
        assert_eq!(record.result, LoopRunResult::AccountSkipped);
        record.error.expect("skip reason")
    };

    env.publish_accounts(&rimz::agents::account::AccountsCache {
        logins: BTreeMap::from([(
            "claude@work".parse().unwrap(),
            rimz::agents::account::ProviderRecord {
                consecutive_failures: 0,
                version_probed_at_ms: None,
                inputs_stamp: None,
                profile: None,
                probe_error: None,
                login: None,
                probed_at_ms: 1,
                ok: true,
                account: None,
            },
        )]),
    });
    assert_eq!(
        skipped(),
        format!(
            "claude account `work` is logged out; log in with `CLAUDE_CONFIG_DIR={} claude`",
            home.display()
        )
    );

    std::fs::remove_file(env.runtime_paths().shared_accounts_path()).unwrap();
    std::fs::remove_dir_all(&home).unwrap();
    let reason = skipped();
    assert!(reason.contains("is not a directory; run `"), "{reason}");

    let config = std::fs::read_to_string(loop_config_path(&env)).unwrap();
    std::fs::write(
        loop_config_path(&env),
        config.replace("account = \"work\"", "account = \"ghost\""),
    )
    .unwrap();
    assert_eq!(
        skipped(),
        "unknown claude account `ghost`; configured: default, work; run `rimz accounts add claude ghost`"
    );

    assert_eq!(read_loop_run_records(&env).len(), 3);
    assert!(read_loop_strikes(&env).is_empty(), "a skip adds no strike");
    let paths = env.state_path_for(&env.project_root);
    assert!(
        rimz::harness::run::list(&paths)
            .unwrap_or_default()
            .is_empty(),
        "a skipped fire launches nothing"
    );
}

/// A codex task pinned to `work` or `default` fires in a room whose codex
/// account is `spare`: the launch is stamped with the pin, never the room's.
/// A `default` pin stores no stamp, which every launch reads as the provider's
/// own home.
#[cfg(unix)]
#[test]
fn loop_fire_launches_on_the_pinned_account_not_the_rooms() {
    for (pin, stamp) in [("work", Some("work")), ("default", None)] {
        let env = Env::new();
        env.install_agent_hooks("codex");
        trust_codex_preflight_hooks(&env);
        trust_codex_project(&env, &env.project_root);
        for name in ["work", "spare"] {
            let home = env.home_root.join(name);
            let output = env
                .rimz()
                .args(["accounts", "add", "codex", name, "--home"])
                .arg(&home)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            // Trust the named home's hooks the way the default home's are.
            let config = env.agent_config_path("codex");
            let mut table: toml::Table = std::fs::read_to_string(&config).unwrap().parse().unwrap();
            let state = table["hooks"]["state"].as_table_mut().unwrap();
            let prefix = format!("{}:", config.display());
            let named: Vec<_> = state
                .iter()
                .filter_map(|(key, value)| {
                    key.strip_prefix(&prefix).map(|event| {
                        (
                            format!("{}:{event}", home.join("config.toml").display()),
                            value.clone(),
                        )
                    })
                })
                .collect();
            state.extend(named);
            std::fs::write(config, toml::to_string(&table).unwrap()).unwrap();
        }
        let agent_bin = crate::common::write_failing_agent_shim(&env, "codex", 1);
        let shell = write_fake_login_shell(&env, "rimz-test-sh", &[]);
        let workspace = env.resolve_workspace(&env.project_root);
        let store = env.store();
        store.record_workspace(&workspace).expect("record room");
        store
            .switch_room_login(
                &workspace,
                &AgentKind::new_unchecked("codex"),
                &"spare".parse().unwrap(),
            )
            .unwrap();
        crate::common::room::seed_live_zellij_room(
            &env.runtime_paths(),
            &workspace.session_name,
            Vec::new(),
        );
        let pane_fixture = env.project_root.join("panes.json");
        std::fs::write(&pane_fixture, "[]").unwrap();
        let command = || {
            let mut command = env.rimz();
            command
                .args(["--mux", "zellij"])
                .env("SHELL", &shell)
                .env("PATH", path_with_front(&agent_bin))
                .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
                .env(
                    "RIMZ_ZELLIJ_BIN",
                    crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace")),
                )
                .env("RIMZ_TEST_ZELLIJ_LOG", env.project_root.join("spawn.log"))
                .env("RIMZ_TEST_ZELLIJ_LIST_PANES", "[]")
                .env("ZELLIJ_PANE_ID", "1")
                .env(
                    "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
                    format!("{} [Created 1s ago]\n", workspace.session_name),
                );
            command
        };
        loop_ok(
            &env,
            &[
                "loop",
                "add",
                "spawn",
                "--agent",
                "codex",
                "--account",
                pin,
                "--prompt",
                "fix it",
                "--every",
                "15m",
            ],
        );
        let fired = command().args(["loop", "run", "spawn"]).output().unwrap();
        assert!(fired.status.success(), "{fired:?}");
        // The fire's run launches the agent step from its detached advance.
        let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
        let launched = loop {
            let agents = store
                .runtime_projection(rimz::RuntimeScope::Audit)
                .expect("launch history")
                .agents;
            if let Some(agent) = agents.into_iter().next() {
                break agent;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the launch"
            );
            std::thread::sleep(Duration::from_millis(25));
        };
        assert_eq!(
            launched.login.as_ref().map(|name| name.as_str()),
            stamp,
            "task pinned to `{pin}`"
        );
        let stopped = command().args(["loop", "stop", "spawn"]).output().unwrap();
        assert!(
            stopped.status.success(),
            "{}",
            String::from_utf8_lossy(&stopped.stderr)
        );
    }
}

#[test]
fn loop_add_judge_names_the_profile_and_rejects_unsupported_kinds() {
    let env = Env::new();
    let flags = [
        "loop",
        "add",
        "guarded",
        "--agent",
        "claude",
        "--stay",
        "--every",
        "1h",
        "--prompt",
        "work",
        "--judge",
        "opencode",
        "--judge-prompt",
        "q",
        "--on",
        "success",
    ];
    let output = env.rimz().args(flags).output().unwrap();
    assert!(
        !output.status.success(),
        "unsupported judge must be rejected"
    );
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("opencode") && error.contains("only claude and codex run headless judges"),
        "{error}"
    );

    let mut flags = flags;
    flags[11] = "claude";
    let receipt = loop_ok(&env, &flags);
    assert!(
        receipt.contains("guard: asks claude (cap 5m); acts when it passes"),
        "{receipt}"
    );
    let show = loop_ok(&env, &["loop", "show", "guarded"]);
    assert!(
        show.contains("judge `claude`") && !show.contains("judge `claude` ("),
        "{show}"
    );
    let list = loop_ok(&env, &["loop", "list"]);
    assert!(list.contains("judge `claude`"), "{list}");
    let config: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).unwrap()).unwrap();
    let check = serde_json::to_value(&config.tasks.0["guarded"]).unwrap();
    assert_eq!(check["judge"][0]["agent"], "claude");
    assert_eq!(check["judge"][0]["prompt"], "q");
}

#[test]
fn loop_add_saves_and_warns_for_unsupported_fast() {
    let env = Env::new();
    env.install_agent_hooks("cursor");
    write_loop_config(&env, "");
    let (_, error) = loop_ok_with_stderr(
        &env,
        &[
            "loop", "add", "bad-fast", "--agent", "cursor", "--fast", "--prompt", "p", "--every",
            "1h",
        ],
    );
    assert!(
        error.contains("fast")
            && error.contains("cursor")
            && error.contains("launching without")
            && error.contains("fast model id"),
        "{error}"
    );
    let config: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).unwrap()).unwrap();
    assert!(config.tasks.0.contains_key("bad-fast"));
}

#[test]
fn loop_add_saves_and_warns_for_unsupported_effort() {
    let env = Env::new();
    env.install_agent_hooks("cursor");
    write_loop_config(&env, "");
    let (_, error) = loop_ok_with_stderr(
        &env,
        &[
            "loop",
            "add",
            "bad-effort",
            "--agent",
            "cursor",
            "--effort",
            "high",
            "--prompt",
            "p",
            "--every",
            "1h",
        ],
    );
    assert!(
        error.contains(
            "warning: `cursor` effort high is unsupported on cursor; launching without it"
        ),
        "{error}"
    );
    let config: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).unwrap()).unwrap();
    assert_eq!(config.tasks.0["bad-effort"].effort.as_deref(), Some("high"));
    assert_eq!(
        error.matches("effort high is unsupported").count(),
        1,
        "{error}"
    );
}

#[test]
fn loop_add_saves_and_warns_for_unsupported_effort_on_team_member() {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    env.record(&env.project_root);
    env.write_config(
        &env.project_root,
        r#"
        [profiles.lead]
        agent = "claude"
        [profiles.helper]
        agent = "cursor"
        [[agents.teams.forge.roles]]
        role = "lead"
        profile = "lead"
        [[agents.teams.forge.roles]]
        role = "helper"
        profile = "helper"
        "#,
    );
    loop_ok(&env, &["trust", "grant"]);
    write_loop_config(&env, "");
    let (_, error) = loop_ok_with_stderr(
        &env,
        &[
            "loop",
            "add",
            "effort-team",
            "--agent",
            "forge",
            "--stay",
            "--effort",
            "high",
            "--when",
            "pr=open",
            "--prompt",
            "p",
        ],
    );
    assert!(
        error.contains(
            "warning: `cursor` effort high is unsupported on cursor; launching without it"
        ),
        "{error}"
    );
    assert_eq!(
        error.matches("effort high is unsupported").count(),
        1,
        "{error}"
    );
    let config: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).unwrap()).unwrap();
    assert_eq!(
        config.tasks.0["effort-team"].effort.as_deref(),
        Some("high")
    );
}

#[test]
fn loop_add_saves_and_warns_for_unsupported_fast_on_nonleader() {
    let env = Env::new();
    write_loop_config(&env, "");
    let (_, error) = loop_ok_with_stderr(
        &env,
        &[
            "loop",
            "add",
            "bad-resident",
            "--agent",
            "claude,cursor",
            "--stay",
            "--fast",
            "--when",
            "pr=open",
            "--prompt",
            "p",
        ],
    );
    assert!(
        error.contains("fast")
            && error.contains("cursor")
            && error.contains("launching without")
            && error.contains("fast model id"),
        "{error}"
    );
    let config: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).unwrap()).unwrap();
    assert!(config.tasks.0.contains_key("bad-resident"));
}

#[test]
fn loop_add_saves_and_warns_for_unsupported_fast_on_team_member() {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    env.record(&env.project_root);
    env.write_config(
        &env.project_root,
        r#"
        [profiles.lead]
        agent = "claude"
        [profiles.helper]
        agent = "cursor"
        [[agents.teams.forge.roles]]
        role = "lead"
        profile = "lead"
        [[agents.teams.forge.roles]]
        role = "helper"
        profile = "helper"
        "#,
    );
    loop_ok(&env, &["trust", "grant"]);
    write_loop_config(&env, "");
    let (_, error) = loop_ok_with_stderr(
        &env,
        &[
            "loop", "add", "bad-team", "--agent", "forge", "--stay", "--fast", "--when", "pr=open",
            "--prompt", "p",
        ],
    );
    assert!(
        error.contains("fast")
            && error.contains("cursor")
            && error.contains("launching without")
            && error.contains("fast model id"),
        "{error}"
    );
    let config: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).unwrap()).unwrap();
    assert!(config.tasks.0.contains_key("bad-team"));
}

#[test]
fn loop_add_accepts_no_fast_without_provider_switch() {
    let env = Env::new();
    env.install_agent_hooks("cursor");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "no-fast",
            "--agent",
            "cursor",
            "--no-fast",
            "--prompt",
            "p",
            "--every",
            "1h",
        ],
    );
    let config: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).unwrap()).unwrap();
    assert_eq!(config.tasks.0["no-fast"].fast, Some(false));
}

#[test]
fn loop_add_rejects_invalid_action_shapes() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-validate", "feature-loop");
    let cases = [
        (
            vec!["loop", "add", "missing", "--every", "15m", "--prompt", "x"],
            "needs --agent, --wait, --check, or --workflow",
        ),
        (
            vec![
                "loop", "add", "conflict", "--agent", "claude", "--wait", "@claude", "--every",
                "15m", "--prompt", "x",
            ],
            "cannot be used with",
        ),
        (
            vec![
                "loop",
                "add",
                "wait-mode",
                "--wait",
                "@claude",
                "--mode",
                "auto",
                "--every",
                "15m",
                "--prompt",
                "x",
            ],
            "`wait-mode` uses --wait, so --mode only apply to --agent tasks",
        ),
        (
            vec![
                "loop",
                "add",
                "wait-flags",
                "--wait",
                "@claude",
                "--mode",
                "auto",
                "--effort",
                "high",
                "--fast",
                "--budget",
                "5",
                "--budget-per-day",
                "20",
                "--system-prompt-file",
                "missing-system-prompt",
                "--timeout",
                "1m",
                "--every",
                "15m",
                "--prompt",
                "x",
            ],
            "`wait-flags` uses --wait, so --mode, --effort, --fast, --budget, --budget-per-day, --system-prompt-file, --timeout only apply to --agent tasks",
        ),
        (
            vec![
                "loop",
                "add",
                "check-flags",
                "--check",
                "true",
                "--worktree",
                "lane",
                "--mode",
                "auto",
                "--effort",
                "high",
                "--no-fast",
                "--budget",
                "5",
                "--budget-per-day",
                "20",
                "--system-prompt-file",
                "missing-system-prompt",
                "--timeout",
                "1m",
                "--every",
                "15m",
            ],
            "`check-flags` uses --check without an agent action, so --mode, --effort, --no-fast, --budget, --budget-per-day, --system-prompt-file only apply to --agent tasks",
        ),
        (
            vec![
                "loop",
                "add",
                "wait-account",
                "--wait",
                "@claude",
                "--account",
                "work",
                "--every",
                "15m",
                "--prompt",
                "x",
            ],
            "`wait-account` uses --wait, so --account only apply to --agent tasks",
        ),
        (
            vec![
                "loop",
                "add",
                "check-account",
                "--check",
                "true",
                "--account",
                "work",
                "--every",
                "15m",
            ],
            "`check-account` uses --check without an agent action, so --account only apply to --agent tasks",
        ),
        (
            vec![
                "loop", "add", "on", "--agent", "claude", "--on", "fail", "--every", "15m",
                "--prompt", "x",
            ],
            "--on requires --check",
        ),
        (
            vec![
                "loop",
                "add",
                "match",
                "--check",
                "true",
                "--match",
                "status=failed",
                "--every",
                "15m",
            ],
            "--match requires --signal",
        ),
        (
            vec![
                "loop",
                "add",
                "bad-match",
                "--check",
                "true",
                "--signal",
                "ci.failed",
                "--match",
                "status",
            ],
            "invalid --match `status`; expected KEY=VALUE",
        ),
        (
            vec![
                "loop",
                "add",
                "signal-schedule",
                "--check",
                "true",
                "--signal",
                "ci.failed",
                "--every",
                "15m",
            ],
            "cannot be used with",
        ),
        (
            vec![
                "loop",
                "add",
                "surplus",
                "--check",
                "true",
                "--surplus",
                "1.5x",
                "--every",
                "15m",
            ],
            "--surplus and --surplus-after require --agent or --wait",
        ),
        (
            vec![
                "loop",
                "add",
                "attempts",
                "--agent",
                "claude",
                "--verify",
                "true",
                "--max-attempts",
                "0",
                "--every",
                "15m",
                "--prompt",
                "x",
            ],
            "--max-attempts must be at least 1",
        ),
        (
            vec![
                "loop",
                "add",
                "until-check",
                "--agent",
                "claude",
                "--until",
                "1h",
                "--check",
                "true",
                "--every",
                "15m",
                "--prompt",
                "x",
            ],
            "--until is no longer accepted; fire on a condition with --when, or `rimz loop eject` the task and wait in a workflow check",
        ),
        (
            vec![
                "loop",
                "add",
                "project-wait",
                "--project",
                "--wait",
                "@claude",
                "--every",
                "15m",
                "--prompt",
                "x",
            ],
            "--project tasks cannot use --wait; project config cannot pin a machine-local session",
        ),
        (
            vec![
                "loop",
                "add",
                "project-once",
                "--project",
                "--check",
                "true",
                "--at",
                "12:00",
            ],
            "--project tasks need a trigger; set --every, --cron, or --signal",
        ),
        (
            vec![
                "loop",
                "add",
                "project-signal-once",
                "--project",
                "--check",
                "true",
                "--signal",
                "ci.failed",
                "--once",
            ],
            "--project tasks cannot use --once; one-shot subscriptions are machine state",
        ),
    ];
    for (args, expected) in cases {
        let (_stdout, error) = loop_fail(&env, &args);
        assert!(error.contains(expected), "rimz {args:?}: {error}");
    }
}

fn loop_ok(env: &Env, args: &[&str]) -> String {
    loop_ok_with_stderr(env, args).0
}

fn loop_ok_with_stderr(env: &Env, args: &[&str]) -> (String, String) {
    let output = env.rimz().args(args).output().expect("rimz loop");
    assert!(
        output.status.success(),
        "rimz {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        String::from_utf8(output.stdout).expect("stdout"),
        String::from_utf8(output.stderr).expect("stderr"),
    )
}

fn calling_loop(env: &Env, session: &str) -> Command {
    let mut command = env.rimz();
    command
        .env("RIMZ_AGENT_KIND", "claude")
        .env("RIMZ_AGENT_ID", session);
    command
}

fn loop_ok_root(env: &Env, root: &Path, args: &[&str]) -> String {
    let output = env
        .rimz()
        .arg("--root")
        .arg(root)
        .args(args)
        .output()
        .expect("rimz loop at root");
    assert!(
        output.status.success(),
        "rimz --root {} {args:?} failed: {}",
        root.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout")
}

fn qwen_loop_ok(env: &Env, settings: &Path, args: &[&str]) -> String {
    let output = env
        .rimz()
        .env("RIMZ_QWEN_SETTINGS", settings)
        .args(args)
        .output()
        .expect("rimz Qwen loop");
    assert!(
        output.status.success(),
        "rimz {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout")
}

fn loop_fail(env: &Env, args: &[&str]) -> (String, String) {
    let output = env.rimz().args(args).output().expect("rimz loop");
    assert!(
        !output.status.success(),
        "rimz {args:?} should fail: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    (
        String::from_utf8(output.stdout).expect("stdout"),
        String::from_utf8(output.stderr).expect("stderr"),
    )
}

/// Wait for the terminal report of `task`'s latest started run, and return it.
fn wait_for_loop_report(env: &Env, task: &str) -> LoopRunRecord {
    wait_for_loop_report_after(env, task, 0)
}

/// Wait for a terminal run report of `task` among the history rows past the first `before`.
fn wait_for_loop_report_after(env: &Env, task: &str, before: usize) -> LoopRunRecord {
    let report = |record: &LoopRunRecord| {
        record.task == task && record.run_id.is_some() && record.result != LoopRunResult::Launched
    };
    wait_for_loop_run_records(env, &format!("{task}'s run report"), |records| {
        records.iter().skip(before).any(report)
    });
    read_loop_run_records(env)
        .into_iter()
        .skip(before)
        .rfind(report)
        .expect("run report")
}

/// Wait for the detached `rimz worktree` helpers a fire tree's report spawned to exit.
fn wait_for_fire_tree_helpers(env: &Env) {
    let spec = rimz::testkit::sandbox::SandboxSpec {
        home_root: env.home_root.clone(),
        runtime_root: env.runtime_root.clone(),
    };
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    while rimz::testkit::sandbox::sandbox_processes(&spec)
        .into_iter()
        .filter_map(rimz::proc::argv)
        .any(|argv| {
            argv.iter().any(|arg| arg == "worktree")
                && argv.iter().any(|arg| arg == "remove" || arg == "cleanup")
        })
    {
        assert!(
            Instant::now() < deadline,
            "the fire tree's helper did not exit"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The journal of the run a loop row names, rendered for substring checks.
fn loop_run_journal(env: &Env, root: &Path, record: &LoopRunRecord) -> String {
    let id = record.run_id.as_deref().expect("run id").parse().unwrap();
    let paths = env.state_path_for(root).workflow(&id);
    let journal = rimz::store::workflow::journal(&paths).unwrap();
    // A check's output lives in the log its evaluation names.
    let logs = journal
        .iter()
        .filter_map(|entry| match &entry.event {
            rimz::store::workflow::JournalEvent::Evaluation { object, .. } => object
                .get("log")
                .and_then(|log| log.as_str())
                .map(std::path::PathBuf::from),
            _ => None,
        })
        .map(|log| std::fs::read_to_string(log).unwrap_or_default())
        .collect::<String>();
    let run = rimz::store::workflow::load(&paths)
        .map(|run| {
            format!(
                "{:?} node={} input={:?} wait={:?} attempt={:?}",
                run.state, run.node, run.input, run.wait, run.attempt
            )
        })
        .unwrap_or_else(|error| format!("{error:#}"));
    format!("{run}\n{journal:?}\n{logs}")
}

fn last_loop_record(env: &Env) -> LoopRunRecord {
    read_loop_run_records(env)
        .pop()
        .expect("last loop run record")
}

fn grant_project_trust(env: &Env) {
    loop_ok(env, &["trust", "grant"]);
}

fn assert_pending_message(env: &Env, session: &str, text_fragment: &str) {
    let messages = env
        .store()
        .list_pending_messages()
        .expect("pending messages");
    assert_eq!(messages.len(), 1, "{messages:?}");
    let message = &messages[0];
    assert_eq!(message.kind.as_str(), "claude");
    assert_eq!(message.agent_id.as_str(), session);
    assert_eq!(message.status, MessageStatus::Queued);
    assert!(message.text.contains(text_fragment), "{}", message.text);
}

fn stamp_session_ended(env: &Env, session_id: &str) {
    let workspace = env.resolve_workspace(&env.project_root);
    let observation = rimz::agents::AgentLifecycleObservation::new(
        Some(AgentSessionId::from(session_id)),
        rimz::agents::LifecycleSignal::Ended,
    );
    env.store()
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            &workspace.session_name,
            "claude",
            "rimz.agent-ended",
            &observation,
        ))
        .expect("stamp session ended without a hook");
}

fn register_running_agent(env: &Env, session_id: &str, branch: &str) {
    register_running_agent_at(env, session_id, branch, &env.project_root);
}

fn register_running_agent_at(env: &Env, session_id: &str, branch: &str, cwd: &Path) {
    run_hook(
        env,
        json!({
            "hook_event_name": "SessionStart",
            "session_id": session_id,
            "worktree_branch": branch,
        }),
        cwd,
    );
    run_hook(
        env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": session_id,
            "prompt": "work",
            "worktree_branch": branch,
        }),
        cwd,
    );
}

fn run_hook(env: &Env, payload: serde_json::Value, cwd: &Path) {
    run_agent_hook(env, "claude", payload, cwd);
}

fn run_agent_hook(env: &Env, source: &str, payload: serde_json::Value, cwd: &Path) {
    let payload = serde_json::to_string(&payload).expect("payload");
    let owner = dummy_agent_process();
    let owner_pid = owner.id();
    reap_later(owner);
    let mut cmd = env.hook_command(source);
    cmd.current_dir(cwd)
        .env("RIMZ_AGENT_PID", owner_pid.to_string());
    let output = env
        .spawn_payload(cmd, &payload)
        .wait_with_output()
        .expect("wait hook");
    env.drain_hooks();
    assert!(
        output.status.success(),
        "hook failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn dummy_agent_process() -> std::process::Child {
    let mut cmd = Command::new("sleep");
    cmd.scrub_session_env();
    // ponytail: bounded sleeper keeps hook-owned agents live for test snapshots;
    // add a per-test owner guard if tests start lasting longer than this window.
    cmd.arg("30").spawn().expect("spawn dummy agent process")
}

fn reap_later(mut child: std::process::Child) {
    let _ = std::thread::spawn(move || {
        let _ = child.wait();
    });
}

fn write_loop_config(env: &Env, text: &str) {
    let path = loop_config_path(env);
    std::fs::create_dir_all(path.parent().expect("config dir")).expect("mkdir config");
    std::fs::write(path, text).expect("write loop config");
}

fn write_project_config(env: &Env, text: &str) {
    write_project_config_at(&env.project_root, text);
}

fn write_project_config_at(project_root: &Path, text: &str) {
    let path = project_root.join(".rimz/config.toml");
    std::fs::create_dir_all(path.parent().expect("project config dir"))
        .expect("mkdir project config");
    std::fs::write(path, text).expect("write project config");
}

fn loop_arming_path(env: &Env) -> std::path::PathBuf {
    env.rimz_home().join("loops/loop-arming.json")
}

fn loop_strikes_path(env: &Env) -> std::path::PathBuf {
    env.rimz_home().join("loops/loop-strikes.json")
}

fn loop_instances_path(env: &Env) -> std::path::PathBuf {
    env.state_path_for(&env.project_root)
        .root
        .join("records/loop-instances.json")
}

fn loop_runs_path(env: &Env) -> std::path::PathBuf {
    env.rimz_home().join("logs/loop-runs.log.jsonl")
}

fn read_loop_run_records(env: &Env) -> Vec<LoopRunRecord> {
    let path = loop_runs_path(env);
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .map(|line| serde_json::from_str(line).expect("loop run record"))
        .collect()
}

/// A detached loop runner can birth a cold room before the effect a test waits for, which takes
/// past 15 s under gate load, so this bound guards a hung runner rather than timing a slow one.
const LOOP_RUNNER_DEADLINE: Duration = Duration::from_secs(60);

fn wait_for_loop_run_records(
    env: &Env,
    description: &str,
    ready: impl Fn(&[LoopRunRecord]) -> bool,
) {
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    loop {
        let records = read_loop_run_records(env);
        if ready(&records) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {description}: {records:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn write_loop_run_records(env: &Env, records: &[LoopRunRecord]) {
    let path = loop_runs_path(env);
    std::fs::create_dir_all(path.parent().expect("log parent")).expect("mkdir log parent");
    let mut text = String::new();
    for record in records {
        text.push_str(&serde_json::to_string(record).expect("loop record json"));
        text.push('\n');
    }
    std::fs::write(path, text).expect("write loop run records");
}

fn read_loop_instances(env: &Env) -> Tasks {
    let path = loop_instances_path(env);
    let Ok(text) = std::fs::read_to_string(path) else {
        return Tasks::default();
    };
    serde_json::from_str(&text).expect("loop instances")
}

fn read_loop_arming(env: &Env) -> BTreeMap<String, Arming> {
    let path = loop_arming_path(env);
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    serde_json::from_str(&text).expect("loop arming")
}

fn write_loop_arming(env: &Env, entries: &BTreeMap<String, Arming>) {
    let path = loop_arming_path(env);
    std::fs::create_dir_all(path.parent().expect("arming parent")).expect("mkdir arming parent");
    std::fs::write(
        path,
        serde_json::to_vec_pretty(entries).expect("arming json"),
    )
    .expect("write loop arming");
}

fn machine_task_key(name: &str) -> String {
    format!("machine::{name}")
}

fn project_task_key(project_root: &Path, name: &str) -> String {
    format!(
        "{}::{name}",
        rimz::ids::WorkspaceId::from_project_root(project_root)
    )
}

fn read_loop_strikes(env: &Env) -> BTreeMap<String, u32> {
    let path = loop_strikes_path(env);
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    serde_json::from_str(&text).expect("loop strikes")
}

fn write_loop_instances(env: &Env, tasks: Tasks) {
    let path = loop_instances_path(env);
    std::fs::create_dir_all(path.parent().expect("instances parent")).expect("mkdir state");
    std::fs::write(path, serde_json::to_vec_pretty(&tasks).expect("json"))
        .expect("write loop instances");
}

fn write_loop_fire_state(env: &Env, stamps: BTreeMap<String, Timestamp>) {
    write_loop_fire_state_for_root(env, &env.project_root, stamps);
}

fn write_loop_fire_state_for_root(env: &Env, root: &Path, stamps: BTreeMap<String, Timestamp>) {
    let state = env.state_path_for(root);
    let path =
        rimz::RuntimePaths::for_state_under(&state, &env.runtime_root).lane_path("loop-fire.json");
    std::fs::create_dir_all(path.parent().expect("loop fire parent")).expect("mkdir runtime");
    std::fs::write(path, serde_json::to_vec_pretty(&stamps).expect("json"))
        .expect("write loop fire state");
}

fn tick_without_runner_leaves_state_absent(env: &Env, root: &Path) -> bool {
    let state = env.state_path_for(root);
    let fire_path =
        rimz::RuntimePaths::for_state_under(&state, &env.runtime_root).lane_path("loop-fire.json");
    let before = std::fs::read(&fire_path).expect("fire state before probe");
    let output = env
        .rimz()
        .args(["loop", "tick"])
        .env("RIMZ_BIN", env.home_root.join("missing-rimz"))
        .output()
        .expect("tick without a runner");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let root = canonical(root);
    let fired = read_loop_run_records(env).iter().any(|record| {
        record.root.as_deref() == Some(root.as_path())
            && record.result == LoopRunResult::StartFailed
    });
    let absent = !env.rimz_home().join("ws").exists();
    // No child can birth a room; restore the due stamp for the real-run assertion.
    std::fs::write(fire_path, before).expect("restore fire state after probe");
    fired && absent
}

fn wait_for_path(path: &Path) {
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    while !path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(path.exists(), "timed out waiting for {}", path.display());
}

fn seed_loop_watch_room(env: &Env) {
    crate::common::room::seed_sidebar_heartbeat(
        &env.runtime_paths(),
        MuxName::Tmux,
        "watch",
        "watch",
    );
}

/// Run `loop watch --hold` in a pty until the expected content paints, and return the visible frame.
fn loop_watch_frames(env: &Env, expected: &'static [&'static str]) -> String {
    let (output, _) = loop_watch_pty(env, false, false, expected);
    assert!(
        output.contains("\x1b[J"),
        "loop watch did not repaint:\n{output}"
    );
    let mut terminal = vt100::Parser::new(24.try_into().unwrap(), 100.try_into().unwrap(), 0);
    terminal.process(output.as_bytes());
    terminal.screen().contents()
}

fn loop_watch_pty(
    env: &Env,
    pinned: bool,
    refusal: bool,
    expected: &'static [&'static str],
) -> (String, u32) {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open loop watch pty");
    let mut cmd = CommandBuilder::new(env.rimz_bin());
    env.pin_pty_command(&mut cmd);
    cmd.env_remove("RIMZ_WORKSPACE_ID");
    cmd.env_remove("RIMZ_PROJECT_ROOT");
    if pinned {
        cmd.env(
            "RIMZ_WORKSPACE_ID",
            WorkspaceId::from_project_root(&env.project_root).as_str(),
        );
        cmd.env("RIMZ_PROJECT_ROOT", &env.project_root);
    }
    cmd.args(["loop", "watch", "--hold"]);
    cmd.cwd(env.project_root.as_os_str());
    cmd.env("TERM", "xterm-256color");
    let mut child = pair.slave.spawn_command(cmd).expect("spawn loop watch");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let reader_thread = std::thread::spawn(move || {
        let mut output = Vec::new();
        let mut buffer = [0; 4096];
        let mut terminal = vt100::Parser::new(24.try_into().unwrap(), 100.try_into().unwrap(), 0);
        while let Ok(count) = reader.read(&mut buffer) {
            if count == 0 {
                break;
            }
            output.extend_from_slice(&buffer[..count]);
            terminal.process(&buffer[..count]);
            let ready = if refusal {
                let marker = b"or use `rimz loop list`";
                output.windows(marker.len()).any(|bytes| bytes == marker)
            } else {
                let frame = terminal.screen().contents();
                expected.iter().all(|text| frame.contains(*text))
            };
            if ready {
                let _ = ready_tx.send(());
                break;
            }
        }
        output
    });
    let ready = ready_rx.recv_timeout(Duration::from_secs(15));
    if !refusal || ready.is_err() {
        child.kill().expect("terminate loop watch");
    }
    let status = child.wait().expect("reap loop watch").exit_code();
    drop(pair.master);
    let output = reader_thread.join().expect("join pty reader");
    let output = String::from_utf8_lossy(&output).into_owned();
    (output, status)
}

fn loop_run_lock_path(env: &Env, name: &str) -> std::path::PathBuf {
    env.runtime_paths()
        .lock_path(format!("loop-run-{name}.lock"))
}

/// Hold a run lock as a runner would, with `holder` as its payload.
fn hold_loop_run_lock(path: &Path, holder: &RunLockInfo) -> std::fs::File {
    std::fs::create_dir_all(path.parent().expect("lock parent")).expect("mkdir runtime");
    std::fs::write(path, serde_json::to_vec(holder).unwrap()).expect("write lock holder");
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open lock");
    lock_file.try_lock().expect("hold loop run lock");
    lock_file
}

fn append_legacy_loop_record(env: &Env, task: &str, result: LoopRunResult) {
    let path = loop_runs_path(env);
    std::fs::create_dir_all(path.parent().expect("log parent")).expect("mkdir log parent");
    let result = serde_json::to_string(&result).expect("result json");
    let line =
        format!("{{\"task\":\"{task}\",\"at\":\"1970-01-01T00:00:10Z\",\"result\":{result}}}\n");
    std::fs::write(path, line).expect("write legacy loop run record");
}

fn loop_config_path(env: &Env) -> std::path::PathBuf {
    env.rimz_home().join("loop.toml")
}

fn init_git_repo(root: &Path) -> bool {
    if !git_ok(root, &["init", "-q", "-b", "main"]) {
        return false;
    }
    let _ = git_ok(root, &["config", "user.email", "test@example.com"]);
    let _ = git_ok(root, &["config", "user.name", "Test User"]);
    std::fs::write(root.join("README.md"), "base\n").expect("write README");
    git_ok(root, &["add", "README.md"]) && git_ok(root, &["commit", "-q", "-m", "base"])
}

fn git_ok(cwd: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .status()
        .is_ok_and(|status| status.success())
}

fn find_real_git() -> Option<std::path::PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())
}

#[test]
fn loop_add_spawn_still_requires_prompt() {
    let env = Env::new();
    let output = env
        .rimz()
        .args([
            "loop", "add", "no-note", "--agent", "claude", "--every", "15m",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("needs a prompt; pass --prompt or --prompt-file"),
        "{error}"
    );
}

#[test]
fn branch_conditions_show_distinct_live_readings() {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    env.record(&env.project_root);
    let runtime = env.runtime_paths();
    crate::common::room::seed_sidebar_heartbeat(
        &runtime,
        MuxName::Zellij,
        &env.resolve_workspace(&env.project_root).session_name,
        "branches",
    );
    std::fs::create_dir_all(runtime.lane_path("pr-state.json").parent().unwrap()).unwrap();
    std::fs::write(
        runtime.lane_path("pr-state.json"),
        json!({
            "branch_ci": {},
            "path_repos": {env.project_root.to_string_lossy().to_string(): "repo"},
            "repos": {"repo": {"ok": true, "refreshed_at_ms": fresh_probe_ms()}},
            "branches": {"repo": {"a": {"pr": {"state": "merged"}}, "b": {"pr": {"state": "open"}}}}
        })
        .to_string(),
    )
    .unwrap();
    let receipt = loop_ok(
        &env,
        &[
            "loop",
            "add",
            "branches",
            "--check",
            "true",
            "--when",
            "pr[a]=merged && pr[b]=merged",
        ],
    );
    assert!(receipt.contains("pr[a]: merged, pr[b]: open"), "{receipt}");
    let show = loop_ok(&env, &["loop", "show", "branches"]);
    for (term, glyph, reading) in [
        ("pr[a]=merged", '✓', "merged"),
        ("pr[b]=merged", '✗', "open"),
    ] {
        let line = show
            .lines()
            .find(|line| line.trim_start().starts_with(term))
            .unwrap_or_else(|| panic!("{show}"));
        assert!(line.contains(glyph) && line.ends_with(reading), "{line}");
    }
}

#[test]
fn explain_reports_live_terms_truth_and_leaves_inputs_unchanged() {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    env.record(&env.project_root);
    let runtime = env.runtime_paths();
    crate::common::room::seed_sidebar_heartbeat(
        &runtime,
        MuxName::Zellij,
        &env.resolve_workspace(&env.project_root).session_name,
        "explain",
    );
    let cache = runtime.lane_path("pr-state.json");
    std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
    let bytes = json!({"branch_ci": {env.project_root.to_string_lossy().to_string(): "passing"}, "path_repos": {env.project_root.to_string_lossy().to_string(): "repo"}, "repos": {"repo": {"ok": true, "refreshed_at_ms": fresh_probe_ms()}}, "branches": {"repo": {"a": {"pr": {"state": "merged"}}}}}).to_string();
    std::fs::write(&cache, &bytes).unwrap();
    for (condition, code, truth) in [
        ("pr[a]=merged && ci=passed", 0, "true"),
        ("pr[a]=open && ci=passed", 1, "false"),
    ] {
        let output = env
            .rimz()
            .args(["loop", "explain", condition])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(code),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(
            text.starts_with(&format!(
                "when: {condition}\nscope: {}\n",
                env.project_root.display()
            )),
            "{text}"
        );
        assert!(text.contains("  ci=passed   ✓ passed"), "{text}");
        assert!(text.contains(&format!("now: {truth}\n")), "{text}");
        let term = text
            .lines()
            .find(|line| line.starts_with("  pr[a]"))
            .unwrap();
        assert!(term.ends_with("merged"), "{text}");
    }
    assert_eq!(std::fs::read_to_string(cache).unwrap(), bytes);
    assert!(!runtime.lane_path("loop-when.json").exists());
    assert!(loop_ok(&env, &["loop", "list", "--json"]).contains("[]"));
}

#[test]
fn explain_outside_a_room_reports_unknown_and_the_missing_inputs() {
    let env = Env::new();
    let output = env
        .rimz()
        .args(["loop", "explain", "ci=unknown", "window.5h.left>=40"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains("when: ci=unknown && window.5h.left>=40"),
        "{text}"
    );
    assert!(
        text.contains("  ci=unknown   ? readings paused (room closed)"),
        "{text}"
    );
    assert!(text.contains("  window.5h.left>=40   ? not read"), "{text}");
    assert!(
        text.contains(
            "ci and pr readings come from the room's sidebar; the loop timer alone never sees them"
        ),
        "{text}"
    );
    assert!(text.contains("--provider"), "{text}");
    assert!(text.contains("now: false"), "{text}");
    let output = env
        .rimz()
        .args(["loop", "explain", "ci=pased"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("clause 1, position") && error.contains("did you mean passed?"),
        "{error}"
    );
}

#[test]
fn explain_json_reports_live_terms_truth_and_leaves_inputs_unchanged() {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    env.record(&env.project_root);
    let runtime = env.runtime_paths();
    crate::common::room::seed_sidebar_heartbeat(
        &runtime,
        MuxName::Zellij,
        &env.resolve_workspace(&env.project_root).session_name,
        "explain-json",
    );
    let cache = runtime.lane_path("pr-state.json");
    std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
    let bytes = json!({
        "branch_ci": {env.project_root.to_string_lossy().to_string(): "passing"},
        "path_repos": {env.project_root.to_string_lossy().to_string(): "repo"},
        "repos": {"repo": {"refreshed_at_ms": 1_700_000_000_123_u64, "ok": true}},
        "branches": {"repo": {"a": {"pr": {"state": "merged"}}}}
    })
    .to_string();
    std::fs::write(&cache, &bytes).unwrap();
    for (ci, truth, code) in [("passed", true, 0), ("pending", false, 1)] {
        let condition = format!("!(pr[a]=open || ci=failed) && ci={ci} && pr[missing]=unknown");
        let output = env
            .rimz()
            .args(["loop", "explain", "--json", &condition])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(code));
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value = serde_json::from_slice::<serde_json::Value>(&output.stdout);
        assert!(value.is_ok(), "{}", String::from_utf8_lossy(&output.stdout));
        assert_eq!(
            value.unwrap(),
            json!({
                "when": condition,
                "scope": env.project_root,
                "room_open": true,
                "now": truth,
                "checked_at": "2023-11-14T22:13:20.123Z",
                "terms": [
                    {"term": "pr[a]=open", "key": "pr[a]", "negated": true,
                     "reading": "merged", "met": true, "unread": null},
                    {"term": "ci=failed", "key": "ci", "negated": true,
                     "reading": "passed", "met": true, "unread": null},
                    {"term": format!("ci={ci}"), "key": "ci", "negated": false,
                     "reading": "passed", "met": truth, "unread": null},
                    {"term": "pr[missing]=unknown", "key": "pr[missing]", "negated": false,
                     "reading": null, "met": true, "unread": "no PR for branch missing"}
                ]
            })
        );
    }
    let output = loop_ok(&env, &["loop", "explain", "--json", "team.stage=unknown"]);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&output).unwrap(),
        json!({
            "when": "team.stage=unknown",
            "scope": env.project_root,
            "room_open": true,
            "now": true,
            "checked_at": null,
            "terms": [{"term": "team.stage=unknown", "key": "team.stage", "negated": false,
                       "reading": null, "met": true, "unread": "not read"}]
        })
    );
    assert_eq!(std::fs::read_to_string(cache).unwrap(), bytes);
    assert!(!runtime.lane_path("loop-when.json").exists());
    assert!(loop_ok(&env, &["loop", "list", "--json"]).contains("[]"));
}

#[test]
fn explain_json_outside_a_room_reports_unknown_and_usage_errors() {
    let env = Env::new();
    let output = env
        .rimz()
        .args([
            "loop",
            "explain",
            "--json",
            "ci=unknown",
            "window.5h.left>=40",
            "!pr=open",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = serde_json::from_slice::<serde_json::Value>(&output.stdout);
    assert!(value.is_ok(), "{}", String::from_utf8_lossy(&output.stdout));
    assert_eq!(
        value.unwrap(),
        json!({
            "when": "ci=unknown && window.5h.left>=40 && !pr=open",
            "scope": env.project_root,
            "room_open": false,
            "now": false,
            "checked_at": null,
            "terms": [
                {"term": "ci=unknown", "key": "ci", "negated": false,
                 "reading": null, "met": false, "unread": "readings paused (room closed)"},
                {"term": "window.5h.left>=40", "key": "window.5h.left", "negated": false,
                 "reading": null, "met": false, "unread": "not read"},
                {"term": "pr=open", "key": "pr", "negated": true,
                 "reading": null, "met": false, "unread": "readings paused (room closed)"}
            ]
        })
    );
    let output = env
        .rimz()
        .args(["loop", "explain", "--json", "ci=pased"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("clause 1, position") && error.contains("did you mean passed?"),
        "{error}"
    );
}

#[test]
fn explain_from_a_linked_worktree_reads_the_project_root() {
    let env = Env::new();
    if !init_git_repo(&env.project_root) {
        crate::common::skip("git unavailable");
        return;
    }
    let linked = env.home_root.join("linked");
    assert!(git_ok(
        &env.project_root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            linked.to_str().unwrap()
        ]
    ));
    for (checkout, stage) in [(&env.project_root, "Plan"), (&linked, "Done")] {
        std::fs::create_dir_all(checkout.join(".rimz/team")).unwrap();
        std::fs::write(
            checkout.join(".rimz/team/blackboard.md"),
            format!("Stage: {stage}\n"),
        )
        .unwrap();
    }
    let output = env
        .rimz()
        .current_dir(&linked)
        .args(["loop", "explain", "team.stage=Done"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains(&format!(
            "scope: {}\n",
            canonical(&env.project_root).display()
        )),
        "{text}"
    );
    assert!(text.contains("now: false"), "{text}");
}

#[test]
fn explain_root_and_account_select_the_same_readings_as_tasks() {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    let other = env.project_root.join("checkout");
    std::fs::create_dir(&other).unwrap();
    assert!(init_git_repo(&other));
    std::fs::create_dir_all(other.join(".rimz/team")).unwrap();
    std::fs::write(other.join(".rimz/team/blackboard.md"), "Stage: Done\n").unwrap();
    let text = loop_ok(
        &env,
        &[
            "loop",
            "explain",
            "team.stage=Done",
            "--root",
            other.to_str().unwrap(),
        ],
    );
    assert!(
        text.contains(&format!("scope: {}", other.display())),
        "{text}"
    );
    assert!(text.contains("now: true"), "{text}");
    let output = env
        .rimz()
        .args([
            "loop",
            "explain",
            "window.5h.left>=40",
            "--provider",
            "claude",
            "--account",
            "work",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains("now: false") && !text.contains("need --provider"),
        "{text}"
    );
}
