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
use rimz::config::{LoopConfig, TaskEntry, TaskTarget, Tasks};
use rimz::harness::budget::{BudgetLedger, DayBaseline, write_ledger};
use rimz::harness::schedule::arming::Arming;
use rimz::harness::schedule::run_log::{LoopRunMode, LoopRunRecord, LoopRunResult};
use rimz::harness::schedule::runner::RunLockInfo;
use rimz::ids::{AgentKind, AgentSessionId, MuxName, WorkspaceId};
use rimz::store::message::MessageStatus;
use rimz::store::run::{RunRecord, RunStatus};

use crate::common::wait::{publish_claude_5h, publish_claude_5h_resetting};
use crate::common::{Env, ScrubSessionEnvExt, canonical, retained_runs_refusal};
#[path = "loop_list.rs"]
mod list_tests;
#[path = "loop_overlap.rs"]
mod overlap_tests;
#[path = "loop_rows.rs"]
mod row_tests;
#[path = "agents_tune.rs"]
mod tune_tests;

#[test]
fn loop_views_keep_finished_headers_without_a_current_record() {
    let env = Env::new();
    env.record(&env.project_root);
    write_loop_config(
        &env,
        &format!(
            "[tasks.old]\ncommand=['true']\ntimeout='1m'\nevery='1h'\nonce-per-checkout=true\nroot={:?}\n",
            env.project_root
        ),
    );
    let state = env.state_path_for(&env.project_root);
    let id = rimz::ids::WorkflowRunId::new();
    let home = state.finished_workflow(&id);
    std::fs::create_dir_all(&home.home).unwrap();
    let ended = Timestamp::now() - SignedDuration::from_mins(2);
    let header = json!({
        "version": rimz::store::workflow::VERSION - 1,
        "workflow": "review", "task": "old", "state": "failed",
        "project_root": env.project_root, "scope": {"path": env.project_root},
        "node": "hold", "terminal_at": ended,
        "outcome": {"reason": "cannot continue this run"}
    });
    std::fs::write(&home.record, serde_json::to_vec(&header).unwrap()).unwrap();
    let mut report = LoopRunRecord::new("old", LoopRunResult::Failed, LoopRunMode::Scheduled, 0);
    report.at = ended;
    report.root = Some(env.project_root.clone());
    report.checkout = Some(env.project_root.clone());
    report.run_id = Some(id.to_string());
    write_loop_run_records(&env, &[report]);
    let show_command = format!("rimz workflow show {id}");
    for text in [
        loop_ok(&env, &["loop", "show", "old"]),
        loop_ok(&env, &["loop", "logs", "old"]),
    ] {
        assert!(
            text.contains(&format!("run: {id} · failed at hold")),
            "{text}"
        );
        assert!(
            text.contains(&show_command) && !text.contains("· gone"),
            "{text}"
        );
    }
    let shown = loop_ok(&env, &["loop", "show", "old"]);
    assert!(shown.contains("RUNS") && shown.contains("2m"), "{shown}");
    let json: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", "old", "--json"])).unwrap();
    let row = json["workflow_runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id.as_str())
        .unwrap_or_else(|| panic!("{json}"));
    assert_eq!(row["state"], "failed");
    assert_eq!(row["node"], "hold");
    assert_eq!(row["terminal_at"], json!(ended));
    // Remembering the same run must join its header, not add a second or gone row.
    let ledger = state.root.join("records/loop-workflow-launches.json");
    std::fs::create_dir_all(ledger.parent().unwrap()).unwrap();
    std::fs::write(
        &ledger,
        serde_json::to_vec(&json!({
            "version": 2, "tasks": {"old": {env.project_root.to_str().unwrap(): {
                "at": ended, "run": id, "scope": "project"
            }}}
        }))
        .unwrap(),
    )
    .unwrap();
    let json: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", "old", "--json"])).unwrap();
    assert_eq!(json["workflow_runs"].as_array().unwrap().len(), 1, "{json}");
    assert_eq!(json["workflow_runs"][0]["state"], "failed", "{json}");
    // A ledger join is by id even when the header has lost its task or node.
    let mut incomplete = header.clone();
    incomplete.as_object_mut().unwrap().remove("task");
    incomplete.as_object_mut().unwrap().remove("node");
    std::fs::write(&home.record, serde_json::to_vec(&incomplete).unwrap()).unwrap();
    for text in [
        loop_ok(&env, &["loop", "show", "old"]),
        loop_ok(&env, &["loop", "logs", "old"]),
    ] {
        assert!(text.contains(&format!("run: {id} · failed\n")), "{text}");
        assert!(text.contains(&show_command), "{text}");
    }
    let output = env
        .rimz()
        .args(["workflow", "show", id.as_str()])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    std::fs::remove_dir_all(&home.home).unwrap();
    for text in [
        loop_ok(&env, &["loop", "show", "old"]),
        loop_ok(&env, &["loop", "logs", "old"]),
    ] {
        assert!(text.contains(&format!("run: {id} · gone")), "{text}");
        assert!(!text.contains(&show_command), "{text}");
    }
}

#[test]
fn loop_show_condition_evidence_and_watch_command_are_not_repeated_or_cut() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.condition]\nroot={:?}\ncommand=['true']\ntimeout='1m'\nwhen=['pr=merged && ci=passed']\nrepeat=true\n",
            env.project_root
        ),
    );
    write_loop_fire_state(
        &env,
        BTreeMap::from([("condition".into(), Timestamp::now())]),
    );
    let text = loop_ok(&env, &["loop", "show", "condition"]);
    assert!(
        text.starts_with("condition · routine · unknown · 0 of 2 met\n"),
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
            "[tasks.condition]\nroot={:?}\ncommand=['true']\ntimeout='1m'\nwhen=['team.stage=Done']\n",
            env.project_root
        ),
    );
    let text = loop_ok(&env, &["loop", "fire", "condition"]);
    assert!(
        text.contains("condition unknown (0 of 1); running anyway, task stays armed"),
        "{text}"
    );
    assert!(text.contains("launched"), "{text}");
    assert_eq!(
        wait_for_loop_report(&env, "condition").result,
        LoopRunResult::Completed
    );
    assert!(loop_ok(&env, &["loop", "list"]).contains("condition"));
}

#[test]
fn tick_fires_instance_despite_same_named_foreign_machine() {
    let env = Env::new();
    env.record(&env.project_root);
    let other = env.home_root.join("other");
    std::fs::create_dir(&other).unwrap();
    write_loop_config(
        &env,
        &format!("[tasks.same]\nroot={other:?}\ncommand=['true']\ntimeout='1m'\nevery='1m'\n"),
    );
    write_loop_instances(
        &env,
        Tasks(BTreeMap::from([(
            "same".into(),
            TaskEntry {
                root: env.project_root.clone(),
                command: vec!["true".to_owned()],
                timeout: Some("1m".to_owned()),
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
    let text = format!(
        "# keep this comment\n[tasks.same]\nroot={other:?}\ncommand=['true']\ntimeout='1m'\nevery='1h'\n"
    );
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
                        command: vec!["true".to_owned()],
                        timeout: Some("1m".to_owned()),
                        signal: Some("ci.failed".into()),
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
            &["loop", "add", "same", "--every", "1h", "--", "true"],
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
            "[tasks.same]\nroot={:?}\ncommand=['true']\ntimeout='1m'\nevery='1h'\n",
            env.project_root
        ),
    );
    loop_ok(
        &env,
        &["loop", "add", "same", "--every", "2h", "--", "false"],
    );
    let config: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).unwrap()).unwrap();
    let entry = &config.tasks.0["same"];
    assert_eq!(entry.command, ["false"]);
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
            command: vec!["true".to_owned()],
            timeout: Some("1m".to_owned()),
            signal: Some("ci.failed".into()),
            ..TaskEntry::default()
        },
    )]));
    write_loop_instances(&env, instances.clone());
    let text =
        "# no project owns this row\n[tasks.same]\ncommand=['true']\ntimeout='1m'\nevery='1h'\n";
    write_loop_config(&env, text);
    let (stdout, error) = loop_fail(
        &env,
        &["loop", "add", "same", "--every", "2h", "--", "false"],
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
            "[tasks.skip-bare]\ncommand=['true']\ntimeout='1m'\nevery='1h'\n\
         [tasks.skip-relative]\nroot='nowhere/x'\ncommand=['true']\ntimeout='1m'\nevery='1h'\n\
         [tasks.skip-dot]\nroot='.'\ncommand=['true']\ntimeout='1m'\nevery='1h'\n\
         [tasks.rooted]\nroot={:?}\ncommand=['true']\ntimeout='1m'\nevery='1h'\n",
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
    write_loop_config(
        &env,
        "[tasks.one]\ncommand=['true']\ntimeout='1m'\nevery='1h'\n",
    );
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
        "[tasks.one]\ncommand=['true']\ntimeout='1m'\nevery='1h'\n[tasks.two]\nroot='nowhere/x'\ncommand=['true']\ntimeout='1m'\nevery='1h'\n",
    );
    let output = env.rimz().args(["loop", "tick"]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn adding_machine_refuses_relative_row() {
    let env = Env::new();
    let text = "[tasks.same]\nroot='nowhere/x'\ncommand=['true']\ntimeout='1m'\nevery='1h'\n";
    write_loop_config(&env, text);
    let (stdout, error) = loop_fail(
        &env,
        &["loop", "add", "same", "--every", "2h", "--", "false"],
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
    for remembers in [false, true] {
        let env = Env::new();
        write_loop_config(
            &env,
            &format!(
                "[tasks.same]\ncommand=['true']\ntimeout='1m'\nevery='1h'\n{}",
                if remembers {
                    "once-per-checkout=true\n"
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
    for remembers in [false, true] {
        let env = Env::new();
        write_loop_config(
            &env,
            &format!(
                "[tasks.same]\ncommand=['true']\ntimeout='1m'\nevery='1h'\n{}",
                if remembers {
                    "once-per-checkout=true\n"
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
        assert_eq!(entry.command, ["true"]);
        assert_eq!(entry.once_per_checkout, remembers.then_some(true));
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
            &format!("[tasks.same]\nroot={root:?}\ncommand=['true']\ntimeout='1m'\nevery='1h'\n"),
        );

        loop_ok(
            &env,
            &["loop", "add", "same", "--signal", "ci.failed", "--", "true"],
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
use crate::common::{
    path_with_front, trust_codex_preflight_hooks, trust_codex_project, write_fake_login_shell,
    write_path_shim,
};

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
            "--root",
            scope.to_str().unwrap(),
            "--",
            "true",
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
    assert!(!instances.0["ready"].repeat);
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
    // The spent one-shot's one row is its run's report, which carries the evidence the fire
    // sealed into the run.
    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 1, "{records:?}");
    let condition = records[0].condition.clone().unwrap();
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
            "--repeat",
            "--",
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
            "name it with --provider <kind>",
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
            .args(["loop", "add", "bad"])
            .args(flags)
            .args(["--", "true"])
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
            "--when",
            "ci!=failed",
            "--repeat",
            "--",
            "true",
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
            "[tasks.resident]\nroot = {:?}\ncommand = [\"true\"]\ntimeout = \"1m\"\nonce-per-checkout = true\neach-worktree = true\nwhen = [\"team.stage=Done\"]\nfor = \"30m\"\n",
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

#[cfg(unix)]
#[test]
fn resident_show_summarizes_worktree_conditions() {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    write_loop_config(
        &env,
        &format!(
            "[tasks.resident]\nroot = {:?}\ncommand = [\"true\"]\ntimeout = \"1m\"\nonce-per-checkout = true\neach-worktree = true\nwhen = [\"pr=open\"]\n",
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
            "[tasks.resident]\nroot = {:?}\ncommand = [\"true\"]\ntimeout = \"1m\"\nonce-per-checkout = true\neach-worktree = true\nwhen = [\"team.stage=Done\"]\n",
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
            "[tasks.resident]\nroot = {:?}\ncommand = [\"true\"]\ntimeout = \"1m\"\nonce-per-checkout = true\neach-worktree = true\nwhen = [\"team.stage=Done\"]\n",
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
            "[tasks.resident]\nroot = {:?}\ncommand = [\"true\"]\ntimeout = \"1m\"\nonce-per-checkout = true\neach-worktree = true\nwhen = [\"team.stage=Done\"]\n",
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
        "[tasks.resident]\nroot = {:?}\ncommand = [\"true\"]\ntimeout = \"1m\"\nonce-per-checkout = true\neach-worktree = true\nwhen = [\"team.stage=Done\"]\n",
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
            "[tasks.resident]\nroot = {:?}\ncommand = [\"true\"]\ntimeout = \"1m\"\nonce-per-checkout = true\neach-worktree = true\nwhen = [\"pr=open && ci=passed && team.stage=Done\"]\n",
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
    assert_eq!(shown["worktrees"][0]["state"], "unknown");
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
            .args(["--", "true"])
            .output()
            .unwrap()
    };
    for (extra, expected) in [
        (&[][..], "--provider"),
        (
            &["--provider", "claude", "--project"][..],
            "--project tasks cannot use --after-reset",
        ),
        (
            &["--provider", "claude", "--in", "5m"][..],
            "cannot be used with",
        ),
    ] {
        let refused = add("refused", extra);
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(
            !refused.status.success() && stderr.contains(expected),
            "{extra:?}: {stderr}"
        );
    }
    let refused = add("refused", &["--provider", "claude"]);
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("no current claude 5h window reading; open the room's sidebar"),
        "no cache: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
    publish_claude_5h_resetting(&env, 30, SignedDuration::from_mins(-1));
    let refused = add("refused", &["--provider", "claude"]);
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
    let started = add("later", &["--provider", "claude"]);
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
    let fresh = add("now", &["--provider", "claude"]);
    let receipt = String::from_utf8_lossy(&fresh.stdout);
    for line in [
        "trigger: fires once after the next claude 5h reset\n",
        "window: claude 5h · not started · 100% left\n",
        "next fire: the next scheduler tick, since the window has not started\n",
    ] {
        assert!(receipt.contains(line), "{line}: {receipt}");
    }
    loop_ok(&env, &["loop", "tick"]);
    wait_for_loop_run_records(&env, "the fire and its spent row", |records| {
        !records.is_empty() && !read_loop_instances(&env).0.contains_key("now")
    });
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
            "--provider",
            "claude",
            "--when",
            "window.5h.left >= 40",
            "--",
            "true",
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
        Some(&["window.5h.left >= 40".to_owned()][..]),
        "a command row stores its clauses as typed"
    );
    assert_eq!(row.provider, Some(AgentKind::new_unchecked("claude")));
    loop_ok(&env, &["loop", "tick"]);
    loop_ok(&env, &["loop", "tick"]);
    assert!(read_loop_run_records(&env).is_empty());
    publish_claude_5h(&env, 10);
    loop_ok(&env, &["loop", "tick"]);
    wait_for_loop_run_records(&env, "the fire and its spent row", |records| {
        !records.is_empty() && !read_loop_instances(&env).0.contains_key("roomy")
    });
    let condition = last_loop_record(&env).condition.unwrap();
    assert_eq!(condition.when, "window.5h.left>=40");
    assert_eq!(condition.readings["window.5h.left"].as_deref(), Some("90"));
    assert!(!read_loop_instances(&env).0.contains_key("roomy"));
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
            "--repeat",
            "--",
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
    assert_eq!(
        wait_for_loop_report(&env, "trunk").result,
        LoopRunResult::Completed
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
            "--repeat",
            "--",
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
    assert_eq!(
        wait_for_loop_report(&env, "created").result,
        LoopRunResult::Completed
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
                "--repeat",
                "--",
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
    // The behind fire's passing check is a quiet fire; the merged sibling's skip is the one row.
    assert_eq!(
        wait_for_loop_report(&env, "behind").result,
        LoopRunResult::Completed
    );
    wait_for_loop_run_records(&env, "merged SignalSkipped", |records| {
        records
            .iter()
            .any(|record| record.task == "merged" && record.result == LoopRunResult::SignalSkipped)
    });
    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 1, "{records:?}");
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
    // A binding's `team` makes it standing; no `repeat` is written.
    assert!(!entry.repeat && entry.deadline.is_none());
    assert_eq!(
        entry.matches.as_ref().unwrap()["path"],
        cwd.display().to_string()
    );
    assert_eq!(
        entry.wait.as_ref().unwrap().session.as_str(),
        "launch_sess-team-coder"
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
fn a_team_binding_stored_without_repeat_delivers_on_every_match_and_stays() {
    let env = Env::new();
    env.record(&env.project_root);
    let Some(cwd) = team_signal_fixture(&env) else {
        return;
    };
    seed_team_signal_member(&env, &cwd, "sess-old-binding", None);
    team_signal_hook(&env, &cwd, "sess-old-binding", "SessionStart");
    // The row as an older RimZ stored a standing binding: no `repeat`, no `once`.
    let mut stored = read_loop_instances(&env);
    let name = "team-forge-feature-team-coder-ci-failed";
    stored.0.get_mut(name).unwrap().repeat = false;
    write_loop_instances(&env, stored.clone());
    let raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(loop_instances_path(&env)).unwrap()).unwrap();
    assert_eq!(
        raw[name]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["dir", "match", "root", "signal", "team", "wait"])
    );
    team_signal_hook(&env, &cwd, "sess-old-binding", "SessionStart");
    assert_eq!(read_loop_instances(&env), stored);
    team_signal_hook(&env, &cwd, "sess-old-binding", "UserPromptSubmit");
    for fire in 1..=2 {
        loop_ok(
            &env,
            &[
                "events",
                "emit",
                "ci.failed",
                "--source",
                "forge",
                "--json",
                &json!({"path": &cwd}).to_string(),
            ],
        );
        let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
        while read_loop_run_records(&env).len() < fire && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        let records = read_loop_run_records(&env);
        assert_eq!(
            records
                .iter()
                .map(|record| record.result)
                .collect::<Vec<_>>(),
            vec![LoopRunResult::Delivered; fire],
            "{records:?}"
        );
        assert_eq!(read_loop_instances(&env), stored, "after fire {fire}");
    }
}

#[test]
fn team_signal_registration_preserves_same_named_machine_configuration() {
    let env = Env::new();
    env.record(&env.project_root);
    let Some(cwd) = team_signal_fixture(&env) else {
        return;
    };
    let config = format!(
        "[tasks.team-forge-feature-team-coder-ci-failed]\ncommand = [\"true\"]\ntimeout = \"1m\"\nevery = \"15m\"\nroot = {:?}\n",
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
        "launch_sess-team-new"
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
            seed_wait(
                &env,
                name,
                "sess-team-observer",
                "@claude#main",
                TaskEntry {
                    signal: Some(signal.to_owned()),
                    matches: matching("instance", instance),
                    ..TaskEntry::default()
                },
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
    seed_wait(
        &env,
        "matching",
        "sess-team-observer",
        "@claude#main",
        TaskEntry {
            signal: Some("team.idle".to_owned()),
            matches: matching("instance", "forge#feature-team"),
            ..TaskEntry::default()
        },
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
                channel: Some("feature-team".parse().unwrap()),
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
            &["loop", "add", name, trigger[0], trigger[1], "--", "true"],
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
            "--at",
            "2001-01-02 03:04",
            "--",
            "true",
        ],
    );
    assert!(error.contains("in the past"), "{error}");
    let (_, error) = loop_fail(
        &env,
        &[
            "loop",
            "add",
            "weekly",
            "--every",
            "day",
            "--at",
            "fri 09:30",
            "--",
            "true",
        ],
    );
    assert!(error.contains("--every <days> --at HH:MM"), "{error}");
}

#[test]
fn signal_siblings_keep_subscriptions_and_matches_consume_only_once() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-signal-lifetimes", "feature-loop");
    seed_wait(
        &env,
        "standing",
        "sess-signal-lifetimes",
        "@claude",
        TaskEntry {
            signal: Some("deploy.done".to_owned()),
            matches: matching("branch", "feature-loop"),
            repeat: true,
            ..TaskEntry::default()
        },
    );
    let mut instances = read_loop_instances(&env);
    let mut once = instances.0["standing"].clone();
    once.repeat = false;
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
        (
            "clock",
            TaskEntry {
                every: Some("15m".to_owned()),
                ..TaskEntry::default()
            },
        ),
        (
            "signal",
            TaskEntry {
                signal: Some("deploy.done".to_owned()),
                repeat: true,
                ..TaskEntry::default()
            },
        ),
    ] {
        seed_wait(&env, name, "sess-retire", "@claude", trigger);
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
    seed_wait(
        &env,
        "ended",
        "sess-ended",
        "@claude",
        TaskEntry {
            signal: Some("deploy.done".to_owned()),
            matches: matching("branch", "feature-loop"),
            repeat: true,
            ..TaskEntry::default()
        },
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
    seed_wait(
        &env,
        "ended",
        "sess-ended",
        "@claude",
        TaskEntry {
            signal: Some("deploy.done".to_owned()),
            repeat: true,
            ..TaskEntry::default()
        },
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
    seed_wait(
        &env,
        "ended",
        "sess-ended",
        "@claude",
        TaskEntry {
            signal: Some("deploy.done".to_owned()),
            repeat: true,
            ..TaskEntry::default()
        },
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
    seed_wait(
        &env,
        "ended",
        "sess-ended",
        "@claude",
        TaskEntry {
            signal: Some("deploy.done".to_owned()),
            repeat: true,
            ..TaskEntry::default()
        },
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
    seed_wait(
        &env,
        "ended",
        "sess-ended",
        "@claude",
        TaskEntry {
            signal: Some("deploy.done".to_owned()),
            repeat: true,
            ..TaskEntry::default()
        },
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
    seed_wait(
        &env,
        "revived",
        "sess-revived",
        "@claude",
        TaskEntry {
            signal: Some("deploy.done".to_owned()),
            repeat: true,
            ..TaskEntry::default()
        },
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
fn same_task_name_in_two_rooms_does_not_collide() {
    let env = Env::new();
    let other = env.home_root.join("other-project");
    std::fs::create_dir(&other).expect("other project");
    for root in [&env.project_root, &other] {
        env.record(root);
        let output = env
            .rimz()
            .current_dir(root)
            .args(["loop", "add", "same", "--at", "07:00", "--", "true"])
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
            "[tasks.machine-tick]\ncommand = [\"sh\", \"-c\", \"touch {}\"]\ntimeout = \"1m\"\nroot = \"{}\"\nevery = \"1m\"\n",
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
            "--every",
            "1m",
            "--",
            "sh",
            "-c",
            &format!("touch {}", marker.display()),
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
    loop_fail(&env, &["loop", "add", "new", "--at", "07:00", "--", "true"]);
    assert_eq!(std::fs::read(&path).expect("unchanged"), b"not json");
}

#[test]
fn instance_task_in_a_recorded_project_reaches_machine_wide_readers() {
    let env = Env::new();
    env.record(&env.project_root);
    loop_ok(
        &env,
        &["loop", "add", "nightly", "--at", "07:00", "--", "true"],
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
fn loop_birth_saves_shell_lookups_only_for_manual_fires() {
    use crate::common::CommandTimeoutExt;

    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (kind, prior, verb) in [
        ("check", false, "run"),
        ("check", true, "run"),
        ("check-agent", true, "fire"),
        ("check-agent", true, "print"),
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
            "check" => "command = ['true']\n".to_owned(),
            "check-agent" => format!(
                "command = {:?}\n",
                [
                    env.rimz_bin().to_str().unwrap(),
                    "--mux",
                    "zellij",
                    "agents",
                    "-p",
                    "codex",
                    "repair",
                    "--timeout",
                    "1s",
                ]
            ),
            _ => unreachable!(),
        };
        write_loop_config(
            &env,
            &format!(
                "[tasks.lookup]\nroot = {:?}\nevery = '1h'\ntimeout = '1m'\n{effect}",
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
        // A fire shows as its run in flight, its report, or a quiet fire's summary.
        let fired = !shown_runs(&env, "lookup").is_empty()
            || quiet_fires(&env, "lookup").is_some()
            || !read_loop_run_records(&env).is_empty();
        assert_eq!(fired, verb != "print", "{kind}/{prior}/{verb}: {output:?}");
        // The command runs in the fire's detached run; its own agent launch births the room there.
        if verb != "print" {
            wait_for_loop_report(&env, "lookup");
        }
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
            "[tasks.open-root]\ncommand = [\"sh\", \"-c\", \"touch {}\"]\ntimeout = \"1m\"\nroot = \"{}\"\nevery = \"1m\"\n",
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
            "[tasks.unreachable-bus]\ncommand = [\"sh\", \"-c\", \"touch {}\"]\ntimeout = \"1m\"\nroot = \"{}\"\nevery = \"1m\"\n",
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
            "[tasks.missing-runner]\ncommand = [\"true\"]\ntimeout = \"1m\"\nroot = \"{}\"\nevery = \"1h\"\n",
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
                command: vec!["true".to_owned()],
                timeout: Some("1m".to_owned()),
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

#[test]
fn signal_emit_records_a_spawn_failure_without_reporting_the_task_as_fired() {
    let env = Env::new();
    env.record(&env.project_root);
    write_loop_config(
        &env,
        &format!(
            "[tasks.missing-runner]\ncommand = [\"true\"]\ntimeout = \"1m\"\nroot = \"{}\"\nsignal = \"deploy.finished\"\n",
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
            "[tasks.failed-handoff]\ncommand = [\"true\"]\ntimeout = \"1m\"\nroot = \"{}\"\nevery = \"1h\"\n",
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
            "[tasks.fast-run]\ncommand = [\"true\"]\ntimeout = \"1m\"\nroot = \"{}\"\nevery = \"1h\"\ndeadline = \"1970-01-01T00:00:01Z\"\n",
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
            "[tasks.watch-reloaded]\ncommand = [\"true\"]\ntimeout = \"1m\"\nroot = \"{}\"\nevery = \"15m\"\n",
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
            "[tasks.after-build-move]\nroot = {:?}\ncommand = ['true']\ntimeout = '1m'\nevery = '15m'\n",
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
    seed_wait(
        &env,
        "ci-wait",
        "sess-signal-live",
        "@claude",
        TaskEntry {
            signal: Some("deploy.finished".to_owned()),
            matches: matching("outcome", "failure"),
            prompt: Some("Inspect deployment".to_owned()),
            repeat: true,
            ..TaskEntry::default()
        },
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
    seed_wait(
        &env,
        "queue-wait",
        "sess-signal-live",
        "@claude",
        TaskEntry {
            signal: Some("pr.dequeued".to_owned()),
            matches: matching("branch", "feature-signal"),
            prompt: Some("Fix the queue failure".to_owned()),
            repeat: true,
            ..TaskEntry::default()
        },
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
    seed_wait(
        &env,
        "review-finished",
        "planner-session",
        "@claude",
        TaskEntry {
            signal: Some("agent.idle".to_owned()),
            matches: matching("session", "reviewer-session"),
            prompt: Some("review finished".to_owned()),
            repeat: true,
            ..TaskEntry::default()
        },
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
            "[tasks.shared]\ncommand = [\"sh\", \"-c\", \"printf machine\"]\ntimeout = \"1m\"\nroot = \"{}\"\nevery = \"15m\"\n",
            env.project_root.display()
        ),
    );
    write_project_config(
        &env,
        "[tasks.repo-check]\ncommand = [\"true\"]\ntimeout = \"1m\"\nevery = \"15m\"\n\
         [tasks.shared]\ncommand = [\"sh\", \"-c\", \"printf project\"]\ntimeout = \"1m\"\nevery = \"15m\"\n",
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
    let mark = run_mark(&env, "shared");
    loop_ok(&env, &["loop", "run", "shared"]);
    assert_eq!(read_loop_run_records(&env).len(), mark.rows);
    loop_ok(&env, &["loop", "enable", "shared"]);
    loop_ok(&env, &["loop", "run", "shared"]);
    let report = wait_for_loop_report_after(&env, "shared", mark);
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
        "[tasks.nightly]\ncommand = [\"true\"]\ntimeout = \"1m\"\nevery = \"15m\"\n",
    );
    write_project_config_at(
        &other,
        "[tasks.nightly]\ncommand = [\"true\"]\ntimeout = \"1m\"\nevery = \"15m\"\n",
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
    write_project_config(
        &env,
        "[tasks.first]\ncommand = [\"true\"]\ntimeout = \"1m\"\nevery = \"15m\"\n",
    );
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
            "--every",
            "15m",
            "--",
            "true",
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
            "--every",
            "15m",
            "--",
            "true",
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
            "--every",
            "1h",
            "--",
            "true",
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
        "[tasks.remove-me]\ncommand = [\"true\"]\ntimeout = \"1m\"\nevery = \"15m\"\n",
    );
    grant_project_trust(&env);
    write_project_config(
        &env,
        "[tasks.remove-me]\ncommand = [\"true\"]\ntimeout = \"1m\"\nevery = \"15m\"\n\n\
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
            "# keep unrelated task comment\n[tasks.keep]\ncommand = [\"true\"]\ntimeout = \"1m\"\nroot = \"{}\"\nevery = \"1h\"\n",
            env.project_root.display()
        ),
    );

    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "probe",
            "--in",
            "5m",
            "--",
            "sh",
            "-c",
            "printf ok",
        ],
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
            "loop", "add", "morning", "--every", "weekday", "--at", "07:00", "--", "true",
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
        &["loop", "add", "swap", "--every", "15m", "--", "true"],
    );
    loop_ok(&env, &["loop", "add", "swap", "--in", "5m", "--", "true"]);
    assert!(read_loop_instances(&env).0.contains_key("swap"));
    assert!(
        !std::fs::read_to_string(loop_config_path(&env))
            .unwrap()
            .contains("[tasks.swap]")
    );
    loop_ok(
        &env,
        &[
            "loop", "add", "swap", "--every", "weekday", "--at", "07:00", "--", "true",
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
            "--in",
            "5m",
            "--",
            "true",
        ],
    );
    assert!(
        error.contains("--project tasks must repeat")
            && error.contains("--every, --cron, or --signal with --repeat"),
        "{error}"
    );
}

#[test]
fn loop_multi_name_enable_disable_resolves_before_writing() {
    let env = Env::new();
    for name in ["first", "second"] {
        loop_ok(&env, &["loop", "add", name, "--every", "15m", "--", "true"]);
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
        loop_ok(&env, &["loop", "add", name, "--every", "15m", "--", "true"]);
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
            &["loop", "add", "first", "--every", "15m", "--", "true"],
        );
        rendered.push(std::fs::read_to_string(loop_config_path(&env)).unwrap());
        expected.push(format!(
            "{template}\n[tasks.first]\ncommand = [\"true\"]\nroot = {:?}\ntimeout = \"2h\"\nevery = \"15m\"\n",
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
        &["loop", "add", "first", "--every", "15m", "--", "true"],
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
                "--every",
                "15m",
                "--",
                "true",
            ],
        );
        rendered.push(std::fs::read_to_string(env.project_root.join(".rimz/config.toml")).unwrap());
        let separator = if seed.is_empty() { "" } else { "\n" };
        expected.push(format!(
            "{seed}{separator}[tasks.one]\ncommand = [\"true\"]\ntimeout = \"2h\"\nevery = \"15m\"\n"
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
        &["loop", "add", "one", "--every", "15m", "--", "true"],
    );
    let text = std::fs::read_to_string(loop_config_path(&env)).unwrap();
    assert_eq!(
        text,
        format!(
            "{seed}\n[tasks.one]\ncommand = [\"true\"]\nroot = {:?}\ntimeout = \"2h\"\nevery = \"15m\"\n",
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
            let first = format!(
                "[tasks.one]\n{root}command = [\"true\"]\ntimeout = \"1m\"\nevery = \"1h\"\n"
            );
            let next = match (following_section, project) {
                (false, _) => "",
                (true, false) => "\n# Throttle settings\n[throttle]\nstagger = \"10s\"\n",
                (true, true) => "\n# Agent settings\n[agents]\n",
            };
            let seed = format!("{first}{comments}{next}");
            let mut args = vec!["loop", "add", "two", "--every", "2h"];
            let path = if project {
                write_project_config(&env, &seed);
                args.push("--project");
                env.project_root.join(".rimz/config.toml")
            } else {
                write_loop_config(&env, &seed);
                loop_config_path(&env)
            };
            args.extend(["--", "false"]);
            loop_ok(&env, &args);
            rendered.push(std::fs::read_to_string(path).unwrap());
            expected.push(format!(
                "{first}{comments}\n[tasks.two]\ncommand = [\"false\"]\n{root}timeout = \"2h\"\nevery = \"2h\"\n{next}"
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
        "\n# Two note\n[tasks.two]\nroot = {:?}\ncommand = [\"true\"]\ntimeout = \"1m\"\nevery = \"7d\"\n",
        env.project_root
    );
    write_loop_config(
        &env,
        &format!(
            "{heading}root = {:?}\ncommand = [\"true\"]\ntimeout = \"1m\"\nevery = \"1h\"\n{next}",
            env.project_root
        ),
    );

    loop_ok(
        &env,
        &["loop", "add", "one", "--every", "2h", "--", "false"],
    );
    assert_eq!(
        std::fs::read_to_string(loop_config_path(&env)).unwrap(),
        format!(
            "{heading}command = [\"false\"]\nroot = {:?}\ntimeout = \"2h\"\nevery = \"2h\"\n{next}",
            env.project_root
        )
    );
}

#[test]
fn loop_add_replacing_first_project_task_keeps_header_comments() {
    let env = Env::new();
    let heading = "# Project loops\n\n# One note\n[tasks.one] # header note\n";
    let next =
        "\n# Two note\n[tasks.two]\ncommand = [\"true\"]\ntimeout = \"1m\"\nevery = \"7d\"\n";
    write_project_config(
        &env,
        &format!("{heading}command = [\"true\"]\ntimeout = \"1m\"\nevery = \"1h\"\n{next}"),
    );

    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "one",
            "--project",
            "--every",
            "2h",
            "--",
            "false",
        ],
    );
    assert_eq!(
        std::fs::read_to_string(env.project_root.join(".rimz/config.toml")).unwrap(),
        format!("{heading}command = [\"false\"]\ntimeout = \"2h\"\nevery = \"2h\"\n{next}")
    );
}

#[test]
fn loop_add_new_task_keeps_explicit_tasks_header_comments() {
    let env = Env::new();
    let original = format!(
        "# lead\n[tasks]\n\n# One note\n[tasks.one]\n\
         root = {:?}\ncommand = [\"true\"]\ntimeout = \"1m\"\nevery = \"1h\"\n",
        env.project_root
    );
    write_loop_config(&env, &original);

    loop_ok(
        &env,
        &["loop", "add", "two", "--every", "2h", "--", "false"],
    );
    assert_eq!(
        std::fs::read_to_string(loop_config_path(&env)).unwrap(),
        format!(
            "{original}\n[tasks.two]\ncommand = [\"false\"]\nroot = {:?}\ntimeout = \"2h\"\nevery = \"2h\"\n",
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
            "{heading}root = {:?}\ncommand = [\"true\"]\ntimeout = \"1m\"\nevery = \"1h\"\n",
            env.project_root
        ),
    );

    loop_ok(
        &env,
        &["loop", "add", "one", "--every", "2h", "--", "false"],
    );
    assert_eq!(
        std::fs::read_to_string(loop_config_path(&env)).unwrap(),
        format!(
            "{heading}command = [\"false\"]\nroot = {:?}\ntimeout = \"2h\"\nevery = \"2h\"\n",
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
         [tasks.one]\ncommand = [\"true\"]\ntimeout = \"1m\"\nevery = \"1h\"\n\n\
         [tasks.two]\ncommand = [\"true\"]\ntimeout = \"1m\"\nevery = \"1w\"\n",
    );

    let output = loop_ok(&env, &["loop", "remove", "one"]);
    assert_eq!(output, "removed loop task `one`\n");
    assert_eq!(
        std::fs::read_to_string(loop_config_path(&env)).unwrap(),
        "# Supervised scheduled turns without a task-specific timeout use two hours.\n\
         # default-timeout = \"2h\"\n\n\
         [tasks.two]\ncommand = [\"true\"]\ntimeout = \"1m\"\nevery = \"1w\"\n"
    );
}

#[test]
fn loop_remove_last_task_keeps_preceding_task_comments() {
    let env = Env::new();
    write_loop_config(
        &env,
        "[tasks.one]\ncommand = [\"true\"]\ntimeout = \"1m\"\nevery = \"1h\"\n\
         # every = \"30m\"   # faster cadence during release week\n\
         # Weekly cleanup\n[tasks.two]\ncommand = [\"true\"]\ntimeout = \"1m\"\nevery = \"1w\"\n",
    );

    let output = loop_ok(&env, &["loop", "remove", "two"]);
    assert_eq!(output, "removed loop task `two`\n");
    assert_eq!(
        std::fs::read_to_string(loop_config_path(&env)).unwrap(),
        "[tasks.one]\ncommand = [\"true\"]\ntimeout = \"1m\"\nevery = \"1h\"\n\
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
        &["loop", "add", "probe", "--every", "15m", "--", "true"],
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
    wait_for_loop_report_after(
        &env,
        "probe",
        RunMark {
            rows: 2,
            quiet: None,
        },
    );
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
        &["loop", "add", "second", "--every", "15m", "--", "true"],
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
            "--",
            "sh",
            "-c",
            "printf broken; exit 1",
        ],
    );

    // Each run's report strikes; the third strikes the task out.
    for _ in 0..3 {
        let before = run_mark(&env, "watchdog");
        loop_ok(&env, &["loop", "run", "watchdog"]);
        let report = wait_for_loop_report_after(&env, "watchdog", before);
        assert_eq!(report.result, LoopRunResult::Failed);
    }
    wait_for_loop_reports_sent(&env, "watchdog", 3);
    assert_eq!(
        read_loop_arming(&env)
            .get(&machine_task_key("watchdog"))
            .and_then(|arming| arming.strikes),
        Some(3)
    );

    let before = run_mark(&env, "watchdog");
    let fire = loop_ok(&env, &["loop", "fire", "watchdog"]);
    assert!(fire.contains("task is disabled; firing anyway"), "{fire}");
    wait_for_loop_report_after(&env, "watchdog", before);
    wait_for_loop_reports_sent(&env, "watchdog", 4);
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

    // The strike-out report spawned the handler detached, so its write trails the report.
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
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
            "# keep unrelated task comment\n[tasks.keep]\ncommand = [\"true\"]\ntimeout = \"1m\"\nroot = \"{}\"\nevery = \"1h\"\n\
             [tasks.old]\ncommand = [\"true\"]\ntimeout = \"1m\"\nroot = \"{}\"\nevery = \"15m\"\n",
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
        &["loop", "add", "old-state", "--at", "07:00", "--", "true"],
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
        &["loop", "add", "swap", "--every", "15m", "--", "true"],
    );
    loop_ok(&env, &["loop", "pause", "swap", "--for", "2h"]);
    let replaced = loop_ok(
        &env,
        &["loop", "add", "swap", "--every", "30m", "--", "true"],
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
            "[tasks.old]\ncommand = [\"true\"]\ntimeout = \"1m\"\nroot = {:?}\nevery = \"15m\"\n",
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
                "task":task, "spec_dir":"/definitions/review",
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
            rimz::store::workflow::initialize(&paths, &record, &guard).unwrap();
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
        "command = ['true']\ntimeout = '1m'\nevery = '15m'",
        "command = ['true']\ntimeout = '1m'\nwhen = ['pr=open']",
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
            "[tasks.review]\nroot = {:?}\ncommand = ['true']\ntimeout = '1m'\nwhen = ['pr=open']\n",
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
            "[tasks.old]\ncommand = [\"true\"]\ntimeout = \"1m\"\nroot = \"{}\"\nevery = \"15m\"\n\
             [tasks.existing]\ncommand = [\"true\"]\ntimeout = \"1m\"\nroot = \"{}\"\nevery = \"15m\"\n",
            env.project_root.display(),
            env.project_root.display()
        ),
    );
    loop_ok(
        &env,
        &["loop", "add", "state", "--at", "07:00", "--", "true"],
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
        .args([
            "loop", "add", "where", "--every", "15m", "--", "sh", "-c", probe,
        ])
        .output()
        .expect("add check from linked worktree");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt = String::from_utf8_lossy(&output.stdout);
    assert!(
        receipt.contains(&format!(" in {} · timeout 2h", root.display())),
        "{receipt}"
    );
    assert!(
        receipt.lines().any(|line| line
            == format!(
                "note: runs at the project root {}, not {}; pass --fresh-worktree <prefix> for a fresh worktree per fire",
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
    assert!(
        read_loop_run_records(&env).is_empty(),
        "a quiet fire logs no row"
    );
    assert_eq!(report.root.as_deref(), Some(root.as_path()));
    let workspace_id = WorkspaceId::from_project_root(&root);
    let journal = loop_run_journal(&env, &env.project_root, &report);
    // A quiet fire stores its output tail as the check printed it, final newline included.
    let output = format!("{0}\n{0}\n{0}\n{workspace_id}\n", root.display());
    assert!(journal.contains(&output), "{output}: {journal}");

    let mut config = std::fs::read_to_string(loop_config_path(&env)).unwrap();
    config.push_str(&format!(
        "\n[tasks.legacy]\nroot = {root:?}\ndir = {linked:?}\ncommand = [\"sh\", \"-c\", {probe:?}]\ntimeout = \"1m\"\nevery = \"15m\"\n"
    ));
    std::fs::write(loop_config_path(&env), config).unwrap();
    loop_ok(&env, &["loop", "fire", "legacy"]);
    let report = wait_for_loop_report(&env, "legacy");
    assert_eq!(report.result, LoopRunResult::Completed);
    assert_eq!(report.checkout.as_deref(), Some(linked.as_path()));
    assert_eq!(report.root.as_deref(), Some(root.as_path()));
    let journal = loop_run_journal(&env, &env.project_root, &report);
    let output = format!(
        "{0}\n{0}\n{1}\n{workspace_id}",
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
            "--every",
            "15m",
            "--",
            "sh",
            "-c",
            "pwd -P; printf '%s\\n' \"$RIMZ_WORKTREE_PATH\"",
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
            .contains(&format!(" in {} · timeout 2h", root.display()))
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
    let output = format!("{0}\n{0}", root.display());
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
            "--fresh-worktree",
            "night",
            "--every",
            "1h",
            "--",
            "true",
        ],
    );
    assert!(
        receipt.lines().any(|line| line
            == "action: runs `true` in a fresh worktree `night-<time>` per fire, branched from trunk · timeout 2h"),
        "{receipt}"
    );
    assert!(!receipt.contains("note:"), "{receipt}");
    let stored: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).unwrap()).unwrap();
    let entry = &stored.tasks.0["nightly"];
    assert_eq!(entry.fresh_worktree.as_deref(), Some("night"));
    assert_eq!(entry.dir, None);
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
        "{{ pwd -P; git rev-parse HEAD; git branch --show-current; }} >> {:?}",
        seen.display().to_string()
    );
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "nightly",
            "--fresh-worktree",
            "night",
            "--every",
            "1h",
            "--",
            "sh",
            "-c",
            &check,
        ],
    );
    let mut reports = Vec::new();
    for _ in 0..2 {
        let before = run_mark(&env, "nightly");
        loop_ok(&env, &["loop", "fire", "nightly"]);
        reports.push(wait_for_loop_report_after(&env, "nightly", before));
        #[cfg(target_os = "linux")]
        wait_for_fire_tree_helpers(&env);
    }

    let lines: Vec<String> = std::fs::read_to_string(&seen)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(lines.len(), 6, "{lines:?}");
    // Both passing checks are quiet fires: each report is the summary's row, carrying its tree.
    assert!(read_loop_run_records(&env).is_empty());
    assert_eq!(
        quiet_fires(&env, "nightly").map(|(count, _)| count),
        Some(2)
    );
    let worktrees = canonical(&env.home_root.join("project-worktrees"));
    let root = canonical(&env.project_root);
    for (fire, report) in lines.chunks(3).zip(&reports) {
        assert_eq!(report.result, LoopRunResult::Completed);
        // The task's history keys on the project root it fired from, not the fire's tree.
        assert_eq!(report.checkout.as_deref(), Some(root.as_path()));
        let tree = rimz::utils::path::normalize_path_lexical(
            &report.worktree.as_ref().expect("fire worktree").path,
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
    let name = reports[1]
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
            "--fresh-worktree",
            "night",
            "--every",
            "1h",
            "--",
            "sh",
            "-c",
            &check,
        ],
    );
    loop_ok(&env, &["loop", "fire", "nightly"]);
    loop_ok(&env, &["loop", "fire", "nightly"]);
    let records = read_loop_run_records(&env);
    let launched = shown_runs(&env, "nightly")
        .into_iter()
        .find(|record| record.result == LoopRunResult::Launched)
        .expect("the first fire's run in flight");
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
    #[cfg(target_os = "linux")]
    wait_for_fire_tree_helpers(&env);
    assert_eq!(night_leftovers(&env), Vec::<String>::new());
}

/// The stored quiet streak of `task`, read from the machine fire record.
fn quiet_streak(env: &Env, task: &str) -> u64 {
    let text = std::fs::read_to_string(env.rimz_home().join("loops/loop-fires.json")).unwrap();
    let fires: BTreeMap<String, serde_json::Value> = serde_json::from_str(&text).unwrap();
    fires
        .into_values()
        .filter_map(|fires| fires.get("quiet").cloned())
        .find(|quiet| quiet["last"]["task"] == task)
        .and_then(|quiet| quiet["streak"].as_u64())
        .unwrap_or_else(|| panic!("{task} has no quiet summary: {text}"))
}

fn workflow_assists(env: &Env) -> Vec<serde_json::Value> {
    rimz::harness::assist_log::recent(&env.rimz_home().join("logs"), None)
        .iter()
        .map(|record| serde_json::to_value(record).unwrap())
        .filter(|assist| assist["assist"].as_str().unwrap().starts_with("workflow_"))
        .collect()
}

/// Whether the run a row names still has a home, live or finished.
fn has_home(env: &Env, record: &LoopRunRecord) -> bool {
    let state = env.state_path_for(&env.project_root);
    let id = record.run_id.as_deref().expect("run id").parse().unwrap();
    state.workflow(&id).home.exists() || state.finished_workflow(&id).home.exists()
}

/// Wait until the quiet run a row names has no home: its collection writes the task's quiet
/// summary, which the report wait sees, before it deletes the home.
fn wait_until_forgotten(env: &Env, record: &LoopRunRecord) {
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    while has_home(env, record) {
        assert!(
            Instant::now() < deadline,
            "the quiet run kept its home: {record:?} {}",
            String::from_utf8_lossy(
                &env.rimz()
                    .args([
                        "workflow",
                        "show",
                        record.run_id.as_deref().unwrap(),
                        "--history"
                    ])
                    .output()
                    .unwrap()
                    .stdout
            )
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn quiet_fires_leave_only_their_summary_until_a_failure_or_a_one_shot_keeps_them() {
    let env = Env::new();
    env.record(&env.project_root);
    let counter = env.home_root.join("quiet-counter");
    let fail = env.home_root.join("quiet-fail");
    let command = format!(
        "n=$(cat {c} 2>/dev/null || echo 0); n=$((n+1)); echo $n > {c}; printf pass-$n; \
         test ! -e {f}",
        c = counter.display(),
        f = fail.display()
    );
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "steady",
            "--every",
            "15m",
            "--timeout",
            "1m",
            "--",
            "sh",
            "-c",
            &command,
        ],
    );
    let fire = |env: &Env, task: &str| {
        let before = run_mark(env, task);
        loop_ok(env, &["loop", "fire", task]);
        wait_for_loop_report_after(env, task, before)
    };
    let first = fire(&env, "steady");
    let second = fire(&env, "steady");
    assert_ne!(first.run_id, second.run_id);
    // Both commands that exit 0 are forgotten: no row, no home, no assist; the summary keeps the
    // newest output and counts both.
    assert!(read_loop_run_records(&env).is_empty());
    wait_until_forgotten(&env, &first);
    wait_until_forgotten(&env, &second);
    assert_eq!(workflow_assists(&env), Vec::<serde_json::Value>::new());
    let (count, last) = quiet_fires(&env, "steady").unwrap();
    assert_eq!((count, quiet_streak(&env, "steady")), (2, 2));
    assert_eq!(last.run_id, second.run_id);
    let output = last.check.as_ref().expect("quiet check");
    assert_eq!((output.output.as_str(), output.code), ("pass-2", Some(0)));
    let list: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "list", "--json"])).unwrap();
    let row = list["rooms"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|room| room["tasks"].as_array().unwrap())
        .find(|row| row["name"] == "steady")
        .unwrap();
    assert_eq!(row["quiet"]["count"], 2, "{row}");
    let shown = |env: &Env| -> serde_json::Value {
        serde_json::from_str(&loop_ok(env, &["loop", "show", "steady", "--json"])).unwrap()
    };
    let quiet_only = shown(&env);
    assert_eq!(
        (
            &quiet_only["quiet"]["count"],
            &quiet_only["quiet"]["streak"]
        ),
        (&json!(2), &json!(2)),
        "{quiet_only}"
    );
    assert_eq!(quiet_only["runs"], json!([]), "{quiet_only}");
    let logs = loop_ok(&env, &["loop", "logs", "steady"]);
    assert_eq!(
        logs.matches("2 quiet fires · 2 in a row").count(),
        1,
        "{logs}"
    );

    // A failure resets the streak and keeps the count and the last quiet output.
    std::fs::write(&fail, "").unwrap();
    let failed = fire(&env, "steady");
    assert_eq!(failed.result, LoopRunResult::Failed);
    assert!(has_home(&env, &failed));
    // The report appends its row first and resets the stored streak after it.
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    while quiet_streak(&env, "steady") != 0 {
        assert!(
            Instant::now() < deadline,
            "the failure never reset the streak"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let (count, last) = quiet_fires(&env, "steady").unwrap();
    assert_eq!(count, 2);
    assert_eq!(last.run_id, second.run_id);
    let show = loop_ok(&env, &["loop", "show", "steady"]);
    assert!(
        show.contains("LAST RUN — ✗ failed") && show.contains("LAST QUIET FIRE"),
        "{show}"
    );
    let quiet_then_failure = shown(&env);
    assert_eq!(
        (
            &quiet_then_failure["quiet"]["count"],
            &quiet_then_failure["quiet"]["streak"],
            &quiet_then_failure["runs"][0]["result"],
        ),
        (&json!(2), &json!(0), &json!("failed")),
        "{quiet_then_failure}"
    );
    // The failed row carries the command's exit and output.
    let check = quiet_then_failure["runs"][0]["check"].clone();
    assert_eq!(
        (&check["code"], &check["output"]),
        (&json!(1), &json!("pass-3")),
        "{quiet_then_failure}"
    );
    // Its log path names the finished home the report releases the run to.
    let log = std::path::PathBuf::from(check["output_path"].as_str().unwrap());
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    while !log.is_file() {
        assert!(Instant::now() < deadline, "no log at {}", log.display());
        std::thread::sleep(Duration::from_millis(25));
    }
    // The next quiet fire reads healthy again.
    std::fs::remove_file(&fail).unwrap();
    fire(&env, "steady");
    let show = loop_ok(&env, &["loop", "show", "steady"]);
    assert!(
        show.contains("3 quiet fires · 1 in a row") && show.contains("pass-4"),
        "{show}"
    );
    let failure_then_quiet = shown(&env);
    assert_eq!(
        (
            &failure_then_quiet["quiet"]["count"],
            &failure_then_quiet["quiet"]["streak"],
            failure_then_quiet["runs"].as_array().map(Vec::len),
        ),
        (&json!(3), &json!(1), Some(1)),
        "{failure_then_quiet}"
    );
    assert_eq!(read_loop_run_records(&env).len(), 1);

    // A one-shot's quiet success has no task row left to summarize it: it keeps its row and home.
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "solo",
            "--at",
            "07:00",
            "--timeout",
            "1m",
            "--",
            "printf",
            "once",
        ],
    );
    crate::common::room::seed_sidebar_heartbeat(
        &env.runtime_paths(),
        MuxName::Zellij,
        &env.resolve_workspace(&env.project_root).session_name,
        "solo",
    );
    loop_ok(&env, &["loop", "run", "solo"]);
    let solo = wait_for_loop_report(&env, "solo");
    assert_eq!(solo.result, LoopRunResult::Completed);
    assert!(quiet_fires(&env, "solo").is_none());
    let rows: Vec<_> = read_loop_run_records(&env)
        .into_iter()
        .filter(|row| row.task == "solo")
        .collect();
    assert_eq!(rows.len(), 1, "{rows:?}");
    // The run moves to the finished directory after its report.
    let state = env.state_path_for(&env.project_root);
    let solo_id = solo.run_id.as_deref().unwrap().parse().unwrap();
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    while !state.finished_workflow(&solo_id).home.exists() {
        assert!(
            Instant::now() < deadline,
            "the one-shot's run never finished"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(unix)]
#[test]
fn a_run_collected_before_its_launch_memory_is_remembered_only_when_it_succeeded() {
    for succeeded in [true, false] {
        let env = Env::new();
        env.record(&env.project_root);
        crate::common::room::seed_sidebar_heartbeat(
            &env.runtime_paths(),
            MuxName::Zellij,
            &env.resolve_workspace(&env.project_root).session_name,
            "quick",
        );
        let command = if succeeded { "true" } else { "false" };
        write_loop_config(
            &env,
            &format!(
                "[tasks.quick]\ncommand = ['{command}']\ntimeout = '1m'\nroot = {:?}\n\
                 when = [\"pr=open\"]\nrepeat = true\nonce-per-checkout = true\n",
                env.project_root
            ),
        );
        let evidence =
            json!({"when":"pr=open", "hold":null, "held_ms":0, "readings":{"pr":"open"}})
                .to_string();
        let fire = || {
            let mut fire = env.rimz();
            fire.args(["loop", "run", "quick", "--condition-json", &evidence]);
            fire
        };
        // A quiet success leaves its summary; a failure keeps its own row.
        let collected = || {
            if succeeded {
                quiet_fires(&env, "quick").map_or(0, |(count, _)| count)
            } else {
                read_loop_run_records(&env).len() as u64
            }
        };
        let remembered = || {
            rimz::harness::schedule::launch_ledger::load_workflows(env.store().paths())
                .unwrap()
                .tasks
                .get("quick")
                .is_some_and(|checkouts| !checkouts.is_empty())
        };

        // Hold the launcher before it writes launch memory until the run's collection is done.
        let hold = crate::common::DeliveryRendezvous::new(&env, "launch-memory");
        let launcher = fire()
            .env("RIMZ_TEST_LOOP_BEFORE_LAUNCH_MEMORY", &hold.path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut release = hold.arrive();
        let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
        while collected() != 1 {
            assert!(
                Instant::now() < deadline,
                "the held run was never collected"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        release.write_all(&[1]).unwrap();
        let output = launcher.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(remembered(), succeeded, "{command}: launch memory");

        // The same condition stretch fires again: only a forgotten run lets it start another.
        let output = fire().output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let expected = if succeeded { 1 } else { 2 };
        let deadline = Instant::now() + Duration::from_secs(if succeeded { 2 } else { 60 });
        while collected() != expected && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(
            collected(),
            expected,
            "{command}: runs after the second fire"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_stranded_task_run_reports_one_failed_row_without_a_strike_and_retries_a_lost_report_and_release()
 {
    let env = Env::new();
    env.record(&env.project_root);
    crate::common::room::seed_sidebar_heartbeat(
        &env.runtime_paths(),
        MuxName::Zellij,
        &env.resolve_workspace(&env.project_root).session_name,
        "strand",
    );
    let state = env.state_path_for(&env.project_root);
    let notices = env.project_root.join("notices.log");
    std::fs::write(
        env.rimz_home().join("config.toml"),
        format!(
            "[notifications]\ncommand = '''printf '%s|%s\\n' \"$RIMZ_NOTIFY_KIND\" \
             \"$RIMZ_NOTIFY_BODY\" >> '{}' '''\n",
            notices.display()
        ),
    )
    .unwrap();
    let gate = env.home_root.join("strand-gate");
    let hold = format!("while [ ! -e {} ]; do sleep 0.05; done", gate.display());
    write_loop_config(
        &env,
        &format!(
            "[tasks.strand]\ncommand = ['sh', '-c', {hold:?}]\ntimeout = '1h'\nroot = {:?}\n\
             when = [\"pr=open\"]\nrepeat = true\nonce-per-checkout = true\n",
            env.project_root
        ),
    );
    let evidence =
        json!({"when":"pr=open", "hold":null, "held_ms":0, "readings":{"pr":"open"}}).to_string();
    let output = env
        .rimz()
        .args(["loop", "run", "strand", "--condition-json", &evidence])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let remembered = || {
        rimz::harness::schedule::launch_ledger::load_workflows(env.store().paths())
            .unwrap()
            .tasks
            .get("strand")
            .is_some_and(|checkouts| !checkouts.is_empty())
    };
    let id: rimz::ids::WorkflowRunId = fired_record(&env, "strand")
        .run_id
        .expect("in-flight run id")
        .parse()
        .unwrap();
    let paths = state.workflow(&id);
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    let mut record = loop {
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&paths.record).unwrap()).unwrap();
        // The host's last write before the check runs publishes its child.
        if record["node"] == "command" && record["attempt"]["child"]["pid"].is_u64() && remembered()
        {
            break record;
        }
        assert!(Instant::now() < deadline, "the run never held: {record}");
        std::thread::sleep(Duration::from_millis(25));
    };
    // An older RimZ rewrote the run while its check ran.
    let older = rimz::store::workflow::VERSION - 1;
    record["version"] = json!(older);
    let staged = paths.home.join("record.json.test");
    std::fs::write(&staged, serde_json::to_vec(&record).unwrap()).unwrap();
    std::fs::rename(&staged, &paths.record).unwrap();
    let rows_before = read_loop_run_records(&env).len();
    // A record holding no JSON object, which owes no report, is set aside by the same pass.
    let aside = rimz::ids::WorkflowRunId::new();
    std::fs::create_dir_all(state.workflow(&aside).home).unwrap();
    std::fs::write(state.workflow(&aside).record, "corrupt").unwrap();
    let diag_log = rimz::diag::DiagSink::under(
        state.root.clone(),
        state.workspace_id.clone(),
        "rimz-test",
        None,
    )
    .log_path()
    .unwrap();

    // The loop history cannot be appended: the report stays owed and the run stays live.
    let log = loop_runs_path(&env);
    let kept = log.with_extension("kept");
    let had_log = std::fs::rename(&log, &kept).is_ok();
    std::fs::create_dir_all(&log).unwrap();
    loop_ok(&env, &["workflow", "reconcile"]);
    let live: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&paths.record).unwrap()).unwrap();
    assert_eq!(
        (&live["state"], &live["stranded"], &live["loop_report"]),
        (&json!("failed"), &json!(true), &json!("owed")),
        "{live}"
    );
    // The run stays live, so the pass that moves it tells the user; the notice names only the
    // run the pass moved, which owed no report.
    let read_notices = |count: usize| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let text = std::fs::read_to_string(&notices).unwrap_or_default();
            if text.lines().count() >= count {
                break text;
            }
            assert!(
                Instant::now() < deadline,
                "no workflow_ended notice: {text}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let notice = read_notices(1);
    assert_eq!(
        notice,
        format!(
            "workflow_ended|1 workflow run this RimZ cannot continue was ended as failed: 1 \
             whose record holds no JSON object. Inspect with `rimz --root ~/project workflow show {aside}`.\n"
        )
    );
    let diag = std::fs::read_to_string(&diag_log).unwrap();
    let ended: Vec<&str> = diag
        .lines()
        .filter(|line| line.contains("\"workflow_runs_ended\""))
        .collect();
    assert_eq!(ended.len(), 1, "{diag}");
    assert!(
        ended[0].contains(&format!("\"runs\":[\"{aside}\"]")) && !ended[0].contains("unreported"),
        "{diag}"
    );
    // An unrelated supervised-run record that does not decode fails every release.
    let corrupt = state.runs_dir.join("corrupt.json");
    std::fs::create_dir_all(&state.runs_dir).unwrap();
    std::fs::write(&corrupt, "{").unwrap();

    // Once the history can be appended, the next pass lands one row and frees the task's launch
    // memory, but the failed release keeps the run live and unannounced.
    std::fs::remove_dir(&log).unwrap();
    if had_log {
        std::fs::rename(&kept, &log).unwrap();
    }
    let (_, stderr) = loop_ok_with_stderr(&env, &["workflow", "reconcile"]);
    assert!(
        stderr.contains(&format!(
            "warning: could not release the agents of workflow run `{id}`, so it stays live for \
             the next pass: "
        )),
        "{stderr}"
    );
    let rows = read_loop_run_records(&env);
    let added = &rows[rows_before..];
    assert_eq!(added.len(), 1, "{added:?}");
    assert_eq!(added[0].result, LoopRunResult::Failed, "{:?}", added[0]);
    assert_eq!(added[0].run_id.as_deref(), Some(id.as_str()));
    assert!(
        format!("{:?}", added[0]).contains("this RimZ cannot continue the run"),
        "{:?}",
        added[0]
    );
    assert!(
        !read_loop_strikes(&env).contains_key(&machine_task_key("strand")),
        "a stranded run strikes no task"
    );
    assert!(!remembered(), "a stranded run keeps no launch memory");
    assert!(paths.home.exists(), "a run whose release failed stays live");
    // The run still live reads as the failed run it is, not as gone.
    let show = loop_ok(&env, &["loop", "show", "strand"]);
    assert!(
        show.contains(&format!("{id} · failed")) && !show.contains("· gone"),
        "{show}"
    );
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        std::fs::read_to_string(&notices).unwrap(),
        notice,
        "a run still live is announced by no pass"
    );

    // Once its agents can be released, the next pass moves the run and tells the user once.
    std::fs::remove_file(&corrupt).unwrap();
    let (_, stderr) = loop_ok_with_stderr(&env, &["workflow", "reconcile"]);
    assert!(!stderr.contains("warning:"), "{stderr}");
    let notice = read_notices(2).lines().nth(1).unwrap().to_owned();
    assert!(
        notice.starts_with(&format!(
            "workflow_ended|1 workflow run this RimZ cannot continue was ended as failed: 1 \
             written by an older RimZ (record version {older}). Inspect with `"
        )) && notice.trim_end().ends_with(" loop list`."),
        "{notice}"
    );
    // A command run holds no agent, and its report landed.
    assert!(
        !notice.contains("agents") && !notice.contains("loop report"),
        "{notice}"
    );
    assert!(!paths.home.exists(), "the released run moved");
    let finished: serde_json::Value =
        serde_json::from_slice(&std::fs::read(state.finished_workflow(&id).record).unwrap())
            .unwrap();
    assert_eq!(finished["loop_report"], "sent", "{finished}");
    loop_ok(&env, &["workflow", "reconcile"]);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        std::fs::read_to_string(&notices).unwrap().lines().count(),
        2,
        "one notice for each pass that moved a run"
    );
    assert_eq!(read_loop_run_records(&env).len(), rows_before + 1);
    std::fs::write(&gate, "").unwrap();
}

#[test]
fn a_run_whose_record_lost_engaged_continues_and_is_kept() {
    let env = Env::new();
    env.record(&env.project_root);
    let state = env.state_path_for(&env.project_root);
    let mut config = String::new();
    // `old` stands for a record an older RimZ created: no `engaged`, no sealed fire facts.
    // `rewritten` for one an older RimZ rewrote mid-run, dropping the `engaged` it did not know.
    for (task, stripped) in [
        ("old", &["engaged", "fire_facts"][..]),
        ("rewritten", &["engaged"][..]),
    ] {
        let gate = env.home_root.join(format!("{task}-gate"));
        let hold = format!(
            "while [ ! -e {} ]; do sleep 0.05; done; printf released",
            gate.display()
        );
        config.push_str(&format!(
            "[tasks.{task}]\ncommand = ['sh', '-c', {hold:?}]\ntimeout = '1h'\nroot = {:?}\n\
             every = '1h'\n",
            env.project_root
        ));
        write_loop_config(&env, &config);
        loop_ok(&env, &["loop", "fire", task]);
        let id: rimz::ids::WorkflowRunId = fired_record(&env, task)
            .run_id
            .expect("in-flight run id")
            .parse()
            .unwrap();
        let paths = state.workflow(&id);
        // Strip the fields while the hold check runs, as the older binary's write would.
        let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
        let mut record = loop {
            let record: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&paths.record).unwrap()).unwrap();
            if record["node"] == "command" && record["attempt"].is_object() {
                break record;
            }
            assert!(Instant::now() < deadline, "{task} never held: {record}");
            std::thread::sleep(Duration::from_millis(25));
        };
        assert_eq!(record["engaged"], false, "{record}");
        // A run in flight is its launched row, in `loop show --json` as in the text.
        let in_flight: serde_json::Value =
            serde_json::from_str(&loop_ok(&env, &["loop", "show", task, "--json"])).unwrap();
        assert_eq!(
            (
                &in_flight["runs"][0]["result"],
                &in_flight["runs"][0]["run_id"]
            ),
            (&json!("launched"), &json!(id.as_str())),
            "{in_flight}"
        );
        assert!(
            loop_ok(&env, &["loop", "show", task]).contains("launched"),
            "{task}"
        );
        for field in stripped {
            record.as_object_mut().unwrap().remove(*field);
        }
        let staged = paths.home.join("record.json.test");
        std::fs::write(&staged, serde_json::to_vec(&record).unwrap()).unwrap();
        std::fs::rename(&staged, &paths.record).unwrap();
        std::fs::write(&gate, "").unwrap();

        let report = wait_for_loop_report(&env, task);
        assert_eq!(
            report.result,
            LoopRunResult::Completed,
            "{task}: {report:?}"
        );
        wait_until_finished(&env, &report);
        assert_definition_in_home(&env, &report, true);
        assert!(quiet_fires(&env, task).is_none(), "{task}");
        let rows = read_loop_run_records(&env)
            .into_iter()
            .filter(|row| row.task == task)
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1, "{task}: {rows:?}");
        let finished: serde_json::Value =
            serde_json::from_slice(&std::fs::read(state.finished_workflow(&id).record).unwrap())
                .unwrap();
        assert_eq!(finished["state"], "done", "{task}: {finished}");
        assert!(finished.get("engaged").is_none(), "{task}: {finished}");
    }
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
            "--every",
            "15m",
            "--",
            "sh",
            "-c",
            "printf healthy",
        ],
    );
    loop_ok(&env, &["loop", "fire", "history"]);
    let healthy = wait_for_loop_report(&env, "history");
    assert_eq!(healthy.result, LoopRunResult::Completed);
    let journal = loop_run_journal(&env, &env.project_root, &healthy);
    assert!(journal.contains("healthy"), "{journal}");
    // The passing check is a quiet fire: no row, and one block from its task's quiet summary.
    assert!(read_loop_run_records(&env).is_empty());
    let logs = loop_ok(&env, &["loop", "logs", "history"]);
    assert!(
        logs.contains("1 quiet fire · 1 in a row") && logs.contains("healthy"),
        "{logs}"
    );
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "history",
            "--every",
            "15m",
            "--",
            "sh",
            "-c",
            "printf broken; definitely-missing-rimz-loop-command",
        ],
    );
    let before = run_mark(&env, "history");
    loop_ok(&env, &["loop", "fire", "history"]);
    let broken = wait_for_loop_report_after(&env, "history", before);
    assert_eq!(broken.result, LoopRunResult::Failed);
    let journal = loop_run_journal(&env, &env.project_root, &broken);
    assert!(journal.contains("broken"), "{journal}");
    assert_eq!(read_loop_run_records(&env).len(), 1);

    // The views read each fire's command output from its run's journal.
    let show = loop_ok(&env, &["loop", "show", "history"]);
    assert!(
        show.contains("LAST RUN — ✗ failed")
            && show.contains("command: fail · exit 127")
            && show.contains("broken"),
        "{show}"
    );
    let logs = loop_ok(&env, &["loop", "logs", "history"]);
    assert!(
        logs.contains("broken") && logs.contains(broken.run_id.as_deref().unwrap()),
        "{logs}"
    );
    let failed = loop_ok(&env, &["loop", "logs", "history", "--failed"]);
    assert!(
        failed.contains("broken") && !failed.contains("healthy"),
        "{failed}"
    );
}

/// A fired run whose generated definition stops parsing while its command runs ends failed,
/// and its saved history keeps the command's row with one line naming the unreadable definition.
#[test]
fn a_fired_run_ended_by_its_unparsable_definition_saves_its_rows() {
    let env = Env::new();
    env.record(&env.project_root);
    let go = env.home_root.join("go");
    let script = format!(
        "while [ ! -e {} ]; do sleep 0.05; done; printf boom; exit 1",
        go.display()
    );
    loop_ok(
        &env,
        &[
            "loop", "add", "unread", "--every", "1h", "--", "sh", "-c", &script,
        ],
    );
    let mut fire = env.rimz().args(["loop", "run", "unread"]).spawn().unwrap();
    let state = env.state_path_for(&env.project_root);
    let deadline = Instant::now() + Duration::from_secs(30);
    let run = loop {
        let live = rimz::store::workflow::live_runs(&state).unwrap_or_default();
        // The command has started under the definition as it read then.
        if let Some(run) = live.into_iter().find(|run| {
            run.task.as_deref() == Some("unread")
                && run
                    .attempt
                    .as_ref()
                    .is_some_and(|attempt| attempt.child.is_some())
        }) {
            break run;
        }
        assert!(Instant::now() < deadline, "the fire started no command");
        std::thread::sleep(Duration::from_millis(25));
    };
    let paths = state.workflow(&run.id);
    std::fs::write(
        run.definition_dir(&paths).join("workflow.toml"),
        "[workflow",
    )
    .unwrap();
    std::fs::write(&go, "").unwrap();
    assert!(fire.wait().unwrap().success());
    let report = wait_for_loop_report(&env, "unread");
    assert_eq!(report.result, LoopRunResult::Failed, "{report:?}");
    wait_until_finished(&env, &report);
    let saved = std::fs::read_to_string(state.finished_workflow(&run.id).history()).unwrap();
    assert!(saved.contains(" command said fail"), "{saved}");
    assert!(
        saved.contains("its definition could not be read"),
        "{saved}"
    );
    assert_eq!(saved.matches("TOML parse error").count(), 1, "{saved}");
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
            "--at",
            "07:00",
            "--",
            "true",
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
    // The spent one-shot's row is gone; the run it started holds the fire until it reports.
    let state = dispatch.state_path_for(&dispatch.project_root);
    let runs = rimz::store::workflow::live(&state).unwrap();
    assert!(
        runs.iter().any(|run| matches!(
            rimz::store::workflow::load(&run.paths),
            Ok(record) if record.started_by("dispatch-fails")
        )) || read_loop_run_records(&dispatch)
            .iter()
            .any(|row| row.task == "dispatch-fails" && row.run_id.is_some()),
        "no run for the spent one-shot: {runs:?}"
    );
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
                root: env.project_root.clone(),
                every: Some("2m".to_owned()),
                deadline: Some(Timestamp::now() + SignedDuration::from_mins(30)),
                ..TaskEntry::default()
            },
        )])),
    );
    loop_ok(&env, &["loop", "run", "green"]);
    // The delivery consumes the row.
    wait_for_loop_run_records(&env, "green's delivery", |records| {
        records
            .iter()
            .any(|record| record.task == "green" && record.result == LoopRunResult::Delivered)
    });
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
fn loop_list_uses_room_arm_stamp_for_next_fire() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.next]\ncommand = [\"true\"]\ntimeout = \"1m\"\nroot = \"{}\"\nevery = \"15m\"\n",
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
            "[tasks.invalid]\ncommand = [\"true\"]\ntimeout = \"1m\"\nroot = \"{}\"\nevery = \"15m\"\nat = \"07:00\"\n",
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
    entry.command.clear();
    entry.timeout = None;
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
            "[tasks.legacy]\ncommand = [\"true\"]\ntimeout = \"1m\"\nroot = \"{}\"\nevery = \"15m\"\n",
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
    assert!(show.contains("  action:  run true"), "{show}");
    assert!(
        show.lines().any(|line| line.starts_with("  source: "))
            && !show.contains("throttle:")
            && !show.contains("THROTTLE"),
        "{show}"
    );
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
        &["loop", "add", "busy", "--at", "07:00", "--", "true"],
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
        &["loop", "add", "busy", "--every", "1h", "--", "true"],
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
    loop_ok(&env, &["loop", "add", "fan", "--every", "1h", "--", "true"]);
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
        loop_ok(&env, &["loop", "add", name, "--every", "1h", "--", "true"]);
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
                                command: vec!["true".to_owned()],
                                timeout: Some("1m".to_owned()),
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
fn loop_watch_picks_up_a_quiet_fire_that_appends_nothing_to_the_log() {
    let env = Env::new();
    env.record(&env.project_root);
    seed_loop_watch_room(&env);
    loop_ok(
        &env,
        &["loop", "add", "hushed", "--every", "1h", "--", "true"],
    );
    let frame = loop_watch_across(
        &env,
        &["hushed", "never fired"],
        || {
            let before = run_mark(&env, "hushed");
            loop_ok(&env, &["loop", "fire", "hushed"]);
            wait_for_loop_report_after(&env, "hushed", before);
        },
        // Any age: under load the watch may first repaint seconds after the fire.
        &["hushed", "✓ ", " ago"],
    );
    assert!(!frame.contains("never fired"), "{frame}");
    assert!(read_loop_run_records(&env).is_empty());
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
                        command: vec!["true".to_owned()],
                        timeout: Some("1m".to_owned()),
                        every: Some("1h".into()),
                        ..TaskEntry::default()
                    },
                ),
                (
                    "bound".into(),
                    TaskEntry {
                        root: env.project_root.clone(),
                        dir: Some(checkout),
                        command: vec!["true".to_owned()],
                        timeout: Some("1m".to_owned()),
                        every: Some("1h".into()),
                        ..TaskEntry::default()
                    },
                ),
                (
                    "remote-task".into(),
                    TaskEntry {
                        root: elsewhere,
                        command: vec!["true".to_owned()],
                        timeout: Some("1m".to_owned()),
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
        &["loop", "add", "probe", "--every", "1h", "--", "true"],
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
            "[tasks.remote-task]\ncommand = [\"true\"]\ntimeout = \"1m\"\nroot = \"{}\"\nevery = \"15m\"\n",
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
    loop_ok(&env, &["loop", "add", "fan", "--every", "1h", "--", "true"]);
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
        &["loop", "add", "idle", "--every", "15m", "--", "true"],
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
            "15m", // A bound on the failure only: the check exits on its own, and a
            // shell stopped by terminal job control is what runs into it.
            "--timeout",
            "60s",
            "--",
            "sh",
            "-c",
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
            "--",
            "sh",
            "-c",
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
            "--",
            "sh",
            "-c",
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
fn loop_stop_terminates_holder_and_records_cancellation() {
    let env = Env::new();
    env.record(&env.project_root);
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "stuck",
            "--at",
            "23:59",
            "--",
            "sh",
            "-c",
            "touch check-ready; parent=$PPID; while kill -0 \"$parent\" 2>/dev/null; do sleep 1; done",
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
    // A failing check keeps its run's row, which `loop logs` still reads once the task is gone.
    loop_ok(
        &env,
        &["loop", "add", "probe", "--every", "1h", "--", "false"],
    );
    let before = run_mark(&env, "probe");
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
            "--signal",
            "ci.failed",
            "--match",
            "branch=feature",
            "--repeat",
            "--",
            "true",
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
            "--signal",
            "ci.failed",
            "--match",
            "branch=feature",
            "--repeat",
            "--",
            "true",
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
        .args(["loop", "add", "from-empty", "--every", "1h", "--", "true"])
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
fn loop_add_rejects_invalid_action_shapes() {
    let env = Env::new();
    env.record(&env.project_root);
    let cases = [
        (
            vec!["loop", "add", "missing", "--every", "15m"],
            "the following required arguments were not provided",
        ),
        (
            vec![
                "loop", "add", "wait", "--wait", "--every", "15m", "--", "true",
            ],
            "unexpected argument '--wait'",
        ),
        (
            vec![
                "loop", "add", "agent", "--agent", "claude", "--every", "15m", "--", "true",
            ],
            "unexpected argument '--agent'",
        ),
        (
            vec![
                "loop",
                "add",
                "match",
                "--match",
                "status=failed",
                "--every",
                "15m",
                "--",
                "true",
            ],
            "--match requires --signal",
        ),
        (
            vec![
                "loop",
                "add",
                "bad-match",
                "--signal",
                "ci.failed",
                "--match",
                "status",
                "--repeat",
                "--",
                "true",
            ],
            "invalid --match `status`; expected KEY=VALUE",
        ),
        (
            vec![
                "loop",
                "add",
                "signal-schedule",
                "--signal",
                "ci.failed",
                "--every",
                "15m",
                "--",
                "true",
            ],
            "cannot be used with",
        ),
        (
            vec![
                "loop",
                "add",
                "repeat-clock",
                "--every",
                "15m",
                "--repeat",
                "--",
                "true",
            ],
            "--repeat cannot be used with --every or --cron; they repeat already",
        ),
        (
            vec![
                "loop", "add", "until", "--until", "1h", "--every", "15m", "--", "true",
            ],
            "--until is no longer accepted; fire on a condition with --when",
        ),
        (
            vec![
                "loop",
                "add",
                "project-once",
                "--project",
                "--at",
                "12:00",
                "--",
                "true",
            ],
            "--project tasks must repeat; set --every, --cron, or --signal with --repeat",
        ),
        (
            vec![
                "loop",
                "add",
                "project-signal-once",
                "--project",
                "--signal",
                "ci.failed",
                "--",
                "true",
            ],
            "--project tasks must repeat; set --every, --cron, or --signal with --repeat",
        ),
    ];
    for (args, expected) in cases {
        let (_stdout, error) = loop_fail(&env, &args);
        assert!(error.contains(expected), "rimz {args:?}: {error}");
    }
    assert!(read_loop_instances(&env).0.is_empty());
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
    wait_for_loop_report_after(env, task, RunMark::default())
}

/// How much of a task's history a test has seen: its log rows and its last quiet fire's run.
#[derive(Clone, Debug, Default)]
struct RunMark {
    rows: usize,
    quiet: Option<String>,
}

fn run_mark(env: &Env, task: &str) -> RunMark {
    RunMark {
        rows: read_loop_run_records(env).len(),
        quiet: quiet_fires(env, task).and_then(|(_, last)| last.run_id),
    }
}

/// The quiet summary of `task` in the fixture's machine fire record: how many of its fires'
/// commands exited 0, and the newest one's row.
fn quiet_fires(env: &Env, task: &str) -> Option<(u64, LoopRunRecord)> {
    let text = std::fs::read_to_string(env.rimz_home().join("loops/loop-fires.json")).ok()?;
    let fires: BTreeMap<String, serde_json::Value> = serde_json::from_str(&text).unwrap();
    fires
        .into_values()
        .filter_map(|fires| {
            let quiet = fires.get("quiet")?;
            let last: LoopRunRecord = serde_json::from_value(quiet["last"].clone()).unwrap();
            (last.task == task).then(|| (quiet["count"].as_u64().unwrap(), last))
        })
        .max_by_key(|(_, last)| last.at)
}

/// Wait for a terminal run report of `task` past `mark`: a history row, or a quiet fire, whose
/// summary row stands in for the report it no longer logs.
fn wait_for_loop_report_after(env: &Env, task: &str, mark: RunMark) -> LoopRunRecord {
    let report = |record: &LoopRunRecord| {
        record.task == task && record.run_id.is_some() && record.result != LoopRunResult::Launched
    };
    let found = || {
        let rows = read_loop_run_records(env);
        rows.iter()
            .skip(mark.rows)
            .rfind(|row| report(row))
            .cloned()
            .or_else(|| {
                quiet_fires(env, task)
                    .map(|(_, last)| last)
                    .filter(|last| last.run_id != mark.quiet)
            })
    };
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    loop {
        if let Some(found) = found() {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {task}'s run report: {:?} quiet {:?}",
            read_loop_run_records(env),
            quiet_fires(env, task)
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The fires of `task` as `loop show --json` lists them: its history rows, and a `launched` row
/// for each of its runs still in flight, which the log no longer holds.
fn shown_runs(env: &Env, task: &str) -> Vec<LoopRunRecord> {
    let shown: serde_json::Value =
        serde_json::from_str(&loop_ok(env, &["loop", "show", task, "--json"])).unwrap();
    serde_json::from_value(shown["runs"].clone()).unwrap()
}

/// The newest fire of `task` that names a run, as `loop show --json` lists it (the run's report,
/// or the `launched` row it shows for a run still in flight), or the task's last quiet fire.
fn fired_record(env: &Env, task: &str) -> LoopRunRecord {
    let shown: serde_json::Value =
        serde_json::from_str(&loop_ok(env, &["loop", "show", task, "--json"])).unwrap();
    serde_json::from_value::<Vec<LoopRunRecord>>(shown["runs"].clone())
        .unwrap()
        .into_iter()
        .filter(|row| row.run_id.is_some())
        .chain(quiet_fires(env, task).map(|(_, last)| last))
        .max_by_key(|row| row.at)
        .unwrap_or_else(|| panic!("{task} fired no run: {shown}"))
}

/// Wait until `task`'s workflow runs number `runs` and every report has landed: a reporter marks
/// its run `Sent` only after the history row, the strike, any auto-disable, and the strike-out
/// notice, so the row alone can be read before the state that follows it.
fn wait_for_loop_reports_sent(env: &Env, task: &str, runs: usize) {
    use rimz::store::workflow::LoopReport;
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    loop {
        let store = env.store();
        let state = store.paths();
        let mut records = rimz::store::workflow::live_runs(state).unwrap_or_default();
        // A run whose report was sent has left the live directory but still counts.
        for paths in state.finished_workflow_runs().unwrap_or_default() {
            records.extend(rimz::store::workflow::load(&paths));
        }
        let reports: Vec<_> = records
            .into_iter()
            .filter(|run| run.ran_for(task))
            .map(|run| (run.state, run.loop_report))
            .collect();
        if reports.len() == runs
            && reports
                .iter()
                .all(|(_, report)| *report == LoopReport::Sent)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {runs} sent reports of {task}: {reports:?}; strikes: {:?}; \
             arming: {:?}",
            read_loop_strikes(env),
            read_loop_arming(env)
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Wait for the detached `rimz worktree` helpers a fire tree's report spawned to exit.
#[cfg(target_os = "linux")]
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
/// A workflow run's home wherever it lives now: an ended run leaves the live directory.
fn run_paths(
    env: &Env,
    root: &Path,
    id: &rimz::ids::WorkflowRunId,
) -> rimz::disk::paths::WorkflowPaths {
    let state = env.state_path_for(root);
    rimz::store::workflow::find(&state, id)
        .unwrap()
        .map_or_else(|| state.workflow(id), |found| found.paths)
}

/// A finished run's home holds no copy of a deployed definition; an inline run keeps the one
/// generated at its fire under `definition/`.
fn assert_definition_in_home(env: &Env, record: &LoopRunRecord, inline: bool) {
    let state = env.state_path_for(&env.project_root);
    let id = record.run_id.as_deref().expect("run id").parse().unwrap();
    let home = state.finished_workflow(&id);
    assert!(
        !home.home.join("workflow.toml").exists(),
        "{}",
        home.home.display()
    );
    assert_eq!(
        home.definition.join("workflow.toml").is_file(),
        inline,
        "{}",
        home.home.display()
    );
}

/// Wait until the run a report names has moved to the finished directory: a view that reads its
/// home mid-move finds neither copy.
fn wait_until_finished(env: &Env, record: &LoopRunRecord) {
    let state = env.state_path_for(&env.project_root);
    let id = record.run_id.as_deref().expect("run id").parse().unwrap();
    let deadline = Instant::now() + LOOP_RUNNER_DEADLINE;
    while !state.finished_workflow(&id).record.exists() {
        assert!(Instant::now() < deadline, "run {id} never finished");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// A run's state, journal and check logs; for a quiet run, which left no home, the output its
/// task's quiet summary stored. A home caught between the live and the finished directory is
/// read again.
fn loop_run_journal(env: &Env, root: &Path, record: &LoopRunRecord) -> String {
    let id = record.run_id.as_deref().expect("run id").parse().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let quiet =
            quiet_fires(env, &record.task).is_some_and(|(_, last)| last.run_id == record.run_id);
        if quiet {
            let output = record
                .check
                .as_ref()
                .map_or("", |check| check.output.as_str());
            return format!("quiet {:?}\n{output}", record.result);
        }
        match read_run_home(&run_paths(env, root, &id)) {
            Ok(read) => return read,
            Err(error) => assert!(Instant::now() < deadline, "{error:#}"),
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A run home's state, journal and check logs, read whole or not at all.
fn read_run_home(paths: &rimz::disk::paths::WorkflowPaths) -> anyhow::Result<String> {
    let journal = rimz::store::workflow::journal(paths)?;
    // A check's output lives in the log its evaluation names.
    let logs = journal
        .iter()
        .filter_map(|entry| match &entry.event {
            rimz::store::workflow::JournalEvent::Evaluation { object, .. } => object
                .get("log")
                .and_then(|log| log.as_str())
                .map(Path::new),
            _ => None,
        })
        .map(std::fs::read_to_string)
        .collect::<std::io::Result<String>>()?;
    let run = rimz::store::workflow::load(paths)?;
    Ok(format!(
        "{:?} node={} input={:?} wait={:?} attempt={:?}\n{journal:?}\n{logs}",
        run.state, run.node, run.input, run.wait, run.attempt
    ))
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

/// Stores the delivery row a standing wait for `session` keeps on `entry`'s trigger, at the
/// project root: `loop add` no longer writes one, and `rimz wait` and team bindings arm
/// their rows the same way.
fn seed_wait(env: &Env, name: &str, session: &str, handle: &str, entry: TaskEntry) {
    let mut instances = read_loop_instances(env);
    instances.0.insert(
        name.to_owned(),
        TaskEntry {
            root: env.project_root.clone(),
            wait: Some(TaskTarget {
                kind: AgentKind::new_unchecked("claude"),
                session: AgentSessionId::from(session),
                handle: handle.to_owned(),
            }),
            ..entry
        },
    );
    write_loop_instances(env, instances);
}

fn matching(key: &str, value: &str) -> Option<BTreeMap<String, String>> {
    Some(BTreeMap::from([(key.to_owned(), value.to_owned())]))
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

/// Run `loop watch --hold` in a pty until the expected content and a frame's closing clear
/// paint, and return the visible frame.
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
                // The screen writes its clear just before a frame's last line, so text in
                // the middle of a frame can paint before it: wait for both.
                let frame = terminal.screen().contents();
                output.windows(3).any(|bytes| bytes == b"\x1b[J")
                    && expected.iter().all(|text| frame.contains(*text))
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

/// Run `loop watch --hold` in a pty until `before` paints, run `act` while it keeps refreshing,
/// and return the visible frame once `after` paints.
fn loop_watch_across(
    env: &Env,
    before: &'static [&'static str],
    act: impl FnOnce(),
    after: &'static [&'static str],
) -> String {
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
    cmd.args(["loop", "watch", "--hold"]);
    cmd.cwd(env.project_root.as_os_str());
    cmd.env("TERM", "xterm-256color");
    let mut child = pair.slave.spawn_command(cmd).expect("spawn loop watch");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
    let (stage_tx, stage_rx) = std::sync::mpsc::channel();
    let reader_thread = std::thread::spawn(move || {
        let mut buffer = [0; 4096];
        let mut terminal = vt100::Parser::new(24.try_into().unwrap(), 100.try_into().unwrap(), 0);
        let mut stages = [before, after].into_iter().peekable();
        while let Ok(count) = reader.read(&mut buffer) {
            if count == 0 {
                break;
            }
            terminal.process(&buffer[..count]);
            let frame = terminal.screen().contents();
            if stages
                .next_if(|stage| stage.iter().all(|text| frame.contains(*text)))
                .is_some()
            {
                let _ = stage_tx.send(());
                if stages.peek().is_none() {
                    break;
                }
            }
        }
        terminal.screen().contents()
    });
    let before_painted = stage_rx.recv_timeout(Duration::from_secs(15));
    if before_painted.is_ok() {
        act();
    }
    let after_painted =
        before_painted.and_then(|()| stage_rx.recv_timeout(Duration::from_secs(15)));
    child.kill().expect("terminate loop watch");
    child.wait().expect("reap loop watch");
    drop(pair.master);
    let frame = reader_thread.join().expect("join pty reader");
    assert!(
        after_painted.is_ok(),
        "loop watch never painted its stages:\n{frame}"
    );
    frame
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
            "--when",
            "pr[a]=merged && pr[b]=merged",
            "--",
            "true",
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
        .args(["loop", "explain", "ci=passed", "window.5h.left>=40"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains("when: ci=passed && window.5h.left>=40"),
        "{text}"
    );
    assert!(
        text.contains("  ci=passed   ? readings paused (room closed)"),
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
    assert!(text.contains("now: unknown"), "{text}");
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
    let output = env
        .rimz()
        .args(["loop", "explain", "ci=unknown"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains(
            "clause 1, position 11: unknown is not a value: a term over an unread key is \
             unknown by itself; match a value the key reads"
        ),
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
        "branches": {"repo": {"a": {"pr": {"state": "merged"}}, "missing": {}}}
    })
    .to_string();
    std::fs::write(&cache, &bytes).unwrap();
    for (ci, truth, code) in [("passed", true, 0), ("pending", false, 1)] {
        let condition = format!("!(pr[a]=open || ci=failed) && ci={ci} && pr[missing]=none");
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
                    {"term": "pr[missing]=none", "key": "pr[missing]", "negated": false,
                     "reading": "none", "met": true, "unread": null}
                ]
            })
        );
    }
    let output = env
        .rimz()
        .args(["loop", "explain", "--json", "team.stage=Done"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        json!({
            "when": "team.stage=Done",
            "scope": env.project_root,
            "room_open": true,
            "now": null,
            "checked_at": null,
            "terms": [{"term": "team.stage=Done", "key": "team.stage", "negated": false,
                       "reading": null, "met": null, "unread": "not read"}]
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
            "ci=passed",
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
            "when": "ci=passed && window.5h.left>=40 && !pr=open",
            "scope": env.project_root,
            "room_open": false,
            "now": null,
            "checked_at": null,
            "terms": [
                {"term": "ci=passed", "key": "ci", "negated": false,
                 "reading": null, "met": null, "unread": "readings paused (room closed)"},
                {"term": "window.5h.left>=40", "key": "window.5h.left", "negated": false,
                 "reading": null, "met": null, "unread": "not read"},
                {"term": "pr=open", "key": "pr", "negated": true,
                 "reading": null, "met": null, "unread": "readings paused (room closed)"}
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
        text.contains("now: unknown") && !text.contains("need --provider"),
        "{text}"
    );
}
