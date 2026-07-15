//! Integration coverage for `rimz loop` instance-bound delivery.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use jiff::{SignedDuration, Timestamp};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde_json::json;

use rimz::config::{CheckOn, LoopConfig, TaskEntry, TaskTarget, Tasks};
use rimz::harness::budget::{BudgetLedger, DayBaseline, write_ledger};
use rimz::harness::run::{PermissionMode, RunRecord, RunStatus};
use rimz::harness::schedule::pauses::{self, PauseEntry};
use rimz::harness::schedule::run_log::{self, LoopRunMode, LoopRunRecord, LoopRunResult};
use rimz::harness::schedule::runner::RunLockInfo;
use rimz::harness::schedule::strikes;
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::message::MessageStatus;

use crate::common::{Env, ScrubSessionEnvExt};

#[test]
fn loop_watch_reloads_tasks_without_reprobing_workspace() {
    let env = Env::new();
    let Some(real_git) = find_real_git() else {
        return;
    };
    if !Command::new(&real_git)
        .args(["-C", env.project_root.to_str().expect("utf-8 project root")])
        .args(["init", "-q"])
        .status()
        .is_ok_and(|status| status.success())
    {
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
    cmd.scrub_session_env();
    cmd.args(["loop", "watch", "--hold"]);
    cmd.cwd(env.project_root.as_os_str());
    cmd.env("XDG_STATE_HOME", env.state_root());
    cmd.env("XDG_RUNTIME_DIR", &env.runtime_root);
    cmd.env("XDG_CONFIG_HOME", env.config_root());
    cmd.env("HOME", &env.home_root);
    cmd.env("SHELL", "/bin/sh");
    cmd.env("TERM", "xterm-256color");
    cmd.env("RIMZ_MESSAGE_INTERVAL_MS", "0");
    cmd.env("RIMZ_TEST_GIT_LOG", &git_log);
    cmd.env("RIMZ_TEST_REAL_GIT", &real_git);
    cmd.env("PATH", crate::common::path_with_front(&bin_dir));
    cmd.env_remove("ENV");
    cmd.env_remove("BASH_ENV");
    cmd.env_remove("ZDOTDIR");
    cmd.env_remove("RUST_LOG");

    let mut child = pair.slave.spawn_command(cmd).expect("spawn loop watch");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
    let reader_thread = std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = reader.read_to_end(&mut output);
        output
    });

    std::thread::sleep(Duration::from_millis(1_200));
    let exited_after_startup = child.try_wait().expect("poll loop watch");
    write_loop_config(
        &env,
        &format!(
            "[tasks.watch-reloaded]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"15m\"\n",
            env.project_root.display()
        ),
    );
    std::thread::sleep(Duration::from_millis(1_300));
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
        exited_early.is_none() && output.contains("watch-reloaded"),
        "loop watch exited or missed config reload: {exited_early:?}\n{output}"
    );

    let git_trace = std::fs::read_to_string(&git_log).expect("read loop watch git trace");
    for probe in [
        "git\trev-parse\t--show-toplevel",
        "git\trev-parse\t--git-common-dir",
        "git\trev-parse\t--abbrev-ref\tHEAD",
    ] {
        assert_eq!(
            git_trace.lines().filter(|line| *line == probe).count(),
            1,
            "workspace probe should run once: {probe}\n{git_trace}"
        );
    }
}

#[test]
fn loop_wake_workflow_pins_and_delivers_to_live_session() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-live", "feature-loop");

    let added = loop_ok(
        &env,
        &[
            "loop",
            "add",
            "wake",
            "--wake",
            "@claude",
            "--every",
            "15m",
            "--prompt",
            "next step",
        ],
    );
    assert!(added.contains("pinned to claude session `sess-loop-live`"));
    let config: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).expect("read loop config"))
            .expect("parse loop config");
    assert_eq!(
        config.tasks.0["wake"]
            .wake
            .as_ref()
            .map(|wake| wake.session.as_str()),
        Some("sess-loop-live")
    );

    loop_ok(&env, &["loop", "run", "wake"]);
    assert_pending_message(&env, "sess-loop-live", "next step");
    assert_eq!(last_loop_record(&env).result, LoopRunResult::Delivered);
    let list = loop_ok(&env, &["loop", "list"]);
    let show = loop_ok(&env, &["loop", "show", "wake"]);
    assert!(
        list.lines()
            .any(|line| line.contains("wake") && line.contains("delivered"))
            && show.contains("source:")
            && show.contains("machine"),
        "list/show smoke failed:\n{list}\n{show}"
    );
}

#[test]
fn agent_budget_edits_and_views_use_local_day() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-budget", "feature-budget");
    let agent_id = AgentSessionId::from("sess-budget");
    let kind = AgentKind::new_unchecked("claude");

    for (value, expected_cap, disabled) in [
        ("10", Some(10.0), false),
        ("+5", Some(15.0), false),
        ("clear", None, true),
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

    let mut context = rimz::store::agent_context::empty_context("claude", Timestamp::now());
    context.cost = Some(rimz::agents::AgentCost {
        total_cost_usd: Some(50.0),
        ..rimz::agents::AgentCost::default()
    });
    let record = rimz::store::agent_context::new_record("claude", agent_id.as_str(), context);
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
}

#[test]
fn loop_spawn_controls_persist_render_and_gate_daily_budget() {
    let env = Env::new();
    env.install_agent_hooks("claude");
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
        show.contains("verify: cargo xtask gate (up to 4 attempts)")
            && show.contains("timeout: 3h (default)"),
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
}

#[test]
fn loop_project_trust_controls_visibility_execution_and_precedence() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.shared]\ncheck = \"printf machine\"\nroot = \"{}\"\nevery = \"15m\"\n",
            env.project_root.display()
        ),
    );
    write_project_config(
        &env,
        "[tasks.repo-check]\ncheck = \"true\"\nevery = \"15m\"\n\
         [tasks.shared]\ncheck = \"printf project\"\nevery = \"15m\"\n",
    );

    let list = loop_ok(&env, &["loop", "list"]);
    assert!(
        list.contains("repo-check")
            && list.contains("project · untrusted")
            && list.contains("blocked · trust")
            && list.contains("rimz trust grant"),
        "{list}"
    );
    loop_ok(&env, &["loop", "pause", "repo-check"]);
    assert!(read_loop_pauses(&env).contains_key("repo-check"));
    let (_stdout, error) = loop_fail(&env, &["loop", "run", "repo-check"]);
    assert!(
        error.contains("loop task `repo-check` is blocked — project trust is untrusted")
            && error.contains("rimz trust grant"),
        "{error}"
    );

    loop_ok(&env, &["loop", "run", "shared"]);
    assert!(
        last_loop_record(&env)
            .check
            .is_some_and(|check| check.output.contains("machine"))
    );
    grant_project_trust(&env);
    loop_ok(&env, &["loop", "run", "shared"]);
    assert!(
        last_loop_record(&env)
            .check
            .is_some_and(|check| check.output.contains("project"))
    );
}

#[test]
fn loop_task_storage_policy_and_manual_fire_preserve_one_shots() {
    let env = Env::new();
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
        error.contains("must repeat") && error.contains("--every or --cron"),
        "{error}"
    );
}

#[test]
fn loop_pause_fire_resume_workflow() {
    let env = Env::new();
    loop_ok(
        &env,
        &["loop", "add", "probe", "--check", "true", "--every", "15m"],
    );
    let paused = loop_ok(&env, &["loop", "pause", "probe"]);
    assert!(paused.contains("resume with `rimz loop resume probe`"));
    assert_eq!(
        read_loop_pauses(&env).get("probe"),
        Some(&PauseEntry {
            until: None,
            strikes: None,
        })
    );
    assert!(loop_ok(&env, &["loop", "show", "probe"]).contains("· paused"));

    let timed = loop_ok(&env, &["loop", "pause", "probe", "--for", "2h"]);
    assert!(
        timed.contains("resumes in 2h")
            && read_loop_pauses(&env)
                .get("probe")
                .is_some_and(|entry| entry.until.is_some()),
        "{timed}"
    );
    let fired = loop_ok(&env, &["loop", "fire", "probe"]);
    assert!(fired.contains("task is paused; firing anyway") && fired.contains("check passed"));
    assert!(loop_ok(&env, &["loop", "resume", "probe"]).contains("resumed"));
    assert!(!loop_ok(&env, &["loop", "list"]).contains("paused ·"));
    assert!(loop_ok(&env, &["loop", "resume", "probe"]).contains("not paused"));
}

#[test]
fn loop_repeated_failures_auto_pause_notify_once_and_resume() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-strikes", "feature-loop");
    let notify_log = env.project_root.join("loop-paused-notify.log");
    let config_path = env.config_root().join("rimz/config.toml");
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
            "--wake",
            "@claude",
            "--every",
            "15m",
            "--check",
            "printf broken; exit 1",
            "--prompt",
            "fix it",
        ],
    );

    for _ in 0..2 {
        loop_ok(&env, &["loop", "run", "watchdog"]);
    }
    let third = loop_ok(&env, &["loop", "run", "watchdog"]);
    assert!(
        third.contains("paused after 3 consecutive failed fires"),
        "{third}"
    );
    assert_eq!(
        read_loop_pauses(&env)
            .get("watchdog")
            .and_then(|pause| pause.strikes),
        Some(3)
    );

    let fire = loop_ok(&env, &["loop", "fire", "watchdog"]);
    assert!(fire.contains("task is paused; firing anyway") && fire.contains("delivered"));
    assert_eq!(read_loop_strikes(&env).get("watchdog"), Some(&4));
    assert_eq!(
        read_loop_pauses(&env)
            .get("watchdog")
            .and_then(|pause| pause.strikes),
        Some(3),
        "manual fire must not replace active pause"
    );

    let deadline = Instant::now() + Duration::from_secs(2);
    let notification = loop {
        let text = std::fs::read_to_string(&notify_log).unwrap_or_default();
        if text.contains("loop_paused|RimZ: loop watchdog paused") {
            break text;
        }
        assert!(Instant::now() < deadline, "notification missing: {text}");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(notification.lines().count(), 1, "{notification}");

    loop_ok(&env, &["loop", "resume", "watchdog"]);
    assert!(!read_loop_strikes(&env).contains_key("watchdog"));
    assert!(
        read_loop_pauses(&env)
            .get("watchdog")
            .is_some_and(|pause| pause.until.is_some() && pause.strikes.is_none())
    );
}

#[test]
fn loop_task_mutations_move_and_clear_overlays() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "# keep unrelated task comment\n[tasks.keep]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"1h\"\n\
             [tasks.old]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"15m\"\n",
            env.project_root.display(),
            env.project_root.display()
        ),
    );
    loop_ok(&env, &["loop", "pause", "old"]);
    std::fs::write(strikes::path(&env.state_root()), r#"{"old":2}"#).expect("write loop strikes");
    loop_ok(&env, &["loop", "rename", "old", "new"]);
    let text = std::fs::read_to_string(loop_config_path(&env)).expect("read loop config");
    assert!(
        text.contains("# keep unrelated task comment")
            && text.contains("[tasks.new]")
            && !text.contains("[tasks.old]")
    );
    assert!(read_loop_pauses(&env).contains_key("new"));
    assert_eq!(read_loop_strikes(&env).get("new"), Some(&2));

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
    loop_ok(&env, &["loop", "pause", "old-state"]);
    loop_ok(&env, &["loop", "rename", "old-state", "new-state"]);
    let instances = read_loop_instances(&env);
    assert!(
        !instances.0.contains_key("old-state")
            && instances.0.contains_key("new-state")
            && read_loop_pauses(&env).contains_key("new-state")
    );

    loop_ok(&env, &["loop", "remove", "new"]);
    assert!(!read_loop_pauses(&env).contains_key("new"));
    assert!(!read_loop_strikes(&env).contains_key("new"));
    loop_ok(
        &env,
        &["loop", "add", "swap", "--check", "true", "--every", "15m"],
    );
    loop_ok(&env, &["loop", "pause", "swap"]);
    let replaced = loop_ok(
        &env,
        &["loop", "add", "swap", "--check", "true", "--every", "30m"],
    );
    assert!(replaced.contains("pause: cleared") && !read_loop_pauses(&env).contains_key("swap"));
    assert!(
        std::fs::read_to_string(loop_config_path(&env))
            .unwrap()
            .contains("# keep unrelated task comment")
    );
}

#[test]
fn loop_rename_rejects_collisions_and_reports_missing() {
    let env = Env::new();
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
fn loop_check_failure_records_and_renders_history() {
    let env = Env::new();
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
    loop_ok(&env, &["loop", "fire", "history"]);

    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].result, LoopRunResult::Completed);
    assert_eq!(
        records[0].check.as_ref().and_then(|check| check.code),
        Some(0)
    );
    assert!(
        records[0]
            .check
            .as_ref()
            .unwrap()
            .output
            .contains("healthy")
    );
    assert_eq!(records[1].result, LoopRunResult::Failed);
    assert_eq!(
        records[1].check.as_ref().and_then(|check| check.code),
        Some(127)
    );
    assert!(records[1].check.as_ref().unwrap().output.contains("broken"));

    let show = loop_ok(&env, &["loop", "show", "history"]);
    assert!(
        show.contains("LAST RUN — ✗ failed (exit 127)") && show.contains("broken"),
        "{show}"
    );
    let logs = loop_ok(&env, &["loop", "logs", "history"]);
    assert!(
        logs.find("healthy").unwrap() < logs.find("broken").unwrap(),
        "{logs}"
    );
    let failed = loop_ok(&env, &["loop", "logs", "history", "--failed"]);
    assert!(
        failed.contains("broken") && !failed.contains("healthy"),
        "{failed}"
    );
}

#[test]
fn loop_guard_skips_or_delivers_with_evidence() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-check", "feature-loop");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "healthy",
            "--wake",
            "@claude",
            "--every",
            "15m",
            "--check",
            "printf probe-line",
            "--on",
            "fail",
            "--prompt",
            "fix it",
        ],
    );
    loop_ok(&env, &["loop", "run", "healthy"]);
    assert!(
        env.store()
            .list_pending_messages()
            .expect("messages")
            .is_empty()
    );
    let skipped = last_loop_record(&env);
    assert_eq!(skipped.result, LoopRunResult::CheckSkipped);
    assert_eq!(skipped.check.and_then(|check| check.code), Some(0));

    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "broken",
            "--wake",
            "@claude",
            "--every",
            "15m",
            "--check",
            "printf boom; exit 1",
            "--on",
            "fail",
            "--prompt",
            "fix it",
        ],
    );
    loop_ok(&env, &["loop", "run", "broken"]);
    assert_pending_message(
        &env,
        "sess-loop-check",
        "check `printf boom; exit 1` exited 1",
    );
    let delivered = last_loop_record(&env);
    assert_eq!(delivered.result, LoopRunResult::Delivered);
    let check = delivered.check.expect("guard check detail");
    assert_eq!(check.code, Some(1));
    assert!(check.output.contains("boom"));
}

#[test]
fn loop_trip_then_preparation_error_records_and_renders() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-trip-error", "feature-loop");
    write_loop_config(
        &env,
        &format!(
            "[tasks.trip_error]\n\
             wake = {{ kind = \"claude\", session = \"sess-loop-trip-error\", handle = \"@claude\" }}\n\
             prompt-file = \"missing-prompt.txt\"\ncheck = \"false\"\nroot = \"{}\"\nevery = \"15m\"\n",
            env.project_root.display()
        ),
    );

    let (stdout, error) = loop_fail(&env, &["loop", "fire", "trip_error"]);
    assert!(stdout.contains("✗ check failed (exit 1)") && stdout.contains("→ waking @claude"));
    assert!(
        error.contains("reading prompt-file") && error.contains("missing-prompt.txt"),
        "{error}"
    );
    let record = last_loop_record(&env);
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
    dispatch.install_agent_hooks("claude");
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
    let output = dispatch
        .rimz()
        .env("PATH", &empty_path)
        .args(["loop", "run", "dispatch-fails"])
        .output()
        .expect("run scheduled spawn");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success() && error.contains("finding `claude` on PATH"),
        "{error}"
    );
    assert!(
        !read_loop_instances(&dispatch)
            .0
            .contains_key("dispatch-fails")
    );
    assert_eq!(last_loop_record(&dispatch).task, "dispatch-fails");

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
    let paths = rimz::StatePaths::under(env.workspace_id.clone(), &env.state_root()).unwrap();
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
            && show.contains("last failure — ✗ error")
            && show.contains("rimz loop logs forensics --failed"),
        "{show}"
    );
    assert!(!show.contains("caused by: not found"), "{show}");
}

#[test]
fn loop_poll_until_delivers_once_or_expires() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-until", "feature-loop");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "green",
            "--wake",
            "@claude",
            "--every",
            "2m",
            "--check",
            "true",
            "--on",
            "success",
            "--until",
            "30m",
            "--prompt",
            "merge now",
        ],
    );
    assert!(read_loop_instances(&env).0.contains_key("green"));
    loop_ok(&env, &["loop", "run", "green"]);
    assert_pending_message(&env, "sess-loop-until", "merge now");
    assert!(!read_loop_instances(&env).0.contains_key("green"));

    let expired = Env::new();
    write_loop_instances(
        &expired,
        Tasks(BTreeMap::from([(
            "expired".to_owned(),
            TaskEntry {
                wake: Some(TaskTarget {
                    kind: "claude".to_owned(),
                    session: "sess-expired".to_owned(),
                    handle: "@claude".to_owned(),
                }),
                prompt: Some("too late".to_owned()),
                check: Some("true".to_owned()),
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
    assert_eq!(last_loop_record(&expired).result, LoopRunResult::Expired);
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
    if !init_git_repo(&env.project_root) {
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
            "wake-worktree",
            "--wake",
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
    loop_ok(&env, &["loop", "run", "wake-worktree"]);
    assert_pending_message(&env, "sess-loop-worktree", "worktree next step");
}

#[test]
fn loop_dead_target_run_removes_but_fire_keeps_task() {
    let env = Env::new();
    let config = format!(
        "[tasks.dead]\nwake = {{ kind = \"claude\", session = \"sess-dead\", handle = \"@claude\" }}\n\
         prompt = \"wake up\"\ncheck = \"false\"\nroot = \"{}\"\nat = \"07:00\"\n",
        env.project_root.display()
    );
    write_loop_config(&env, &config);
    let run = loop_ok(&env, &["loop", "run", "dead"]);
    assert!(run.contains("not alive; removing schedule"), "{run}");
    assert!(
        !std::fs::read_to_string(loop_config_path(&env))
            .unwrap()
            .contains("[tasks.dead]")
    );

    write_loop_config(&env, &config);
    let fire = loop_ok(&env, &["loop", "fire", "dead"]);
    assert!(
        fire.contains("@claude not alive — schedule left in place")
            && std::fs::read_to_string(loop_config_path(&env))
                .unwrap()
                .contains("[tasks.dead]"),
        "{fire}"
    );
    assert_eq!(last_loop_record(&env).result, LoopRunResult::TargetGone);
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
    assert!(list.lines().any(|line| {
        line.trim_start().starts_with("next") && line.split_whitespace().last() == Some("-")
    }));
    write_loop_fire_state(
        &env,
        BTreeMap::from([(
            "next".to_owned(),
            Timestamp::now() - SignedDuration::from_secs(16 * 60),
        )]),
    );
    let list = loop_ok(&env, &["loop", "list"]);
    assert!(
        list.lines()
            .any(|line| line.trim_start().starts_with("next") && line.contains("due"))
    );
}

#[test]
fn loop_reset_cadence_requires_ping_and_handles_cold_cache() {
    let env = Env::new();
    let (_stdout, error) = loop_fail(
        &env,
        &[
            "loop", "add", "bad", "--agent", "claude", "--every", "reset",
        ],
    );
    assert!(error.contains("<kind>-ping"), "{error}");
    env.install_agent_hooks("claude");
    let (_stdout, error) = loop_fail(
        &env,
        &[
            "loop",
            "add",
            "pingless",
            "--agent",
            "claude-ping",
            "--at",
            "07:00",
        ],
    );
    assert!(
        error.contains("loop task `pingless` needs a prompt"),
        "{error}"
    );
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "w7",
            "--agent",
            "claude-ping",
            "--prompt",
            "ping",
            "--every",
            "reset",
        ],
    );
    assert!(
        std::fs::read_to_string(loop_config_path(&env))
            .unwrap()
            .contains("every = \"reset\"")
    );
    write_loop_fire_state(
        &env,
        BTreeMap::from([(
            "w7".to_owned(),
            Timestamp::now() - SignedDuration::from_secs(60),
        )]),
    );
    let list = loop_ok(&env, &["loop", "list"]);
    let show = loop_ok(&env, &["loop", "show", "w7"]);
    assert!(
        list.lines().any(|line| {
            line.trim_start().starts_with("w7")
                && line.contains("every window reset")
                && line.split_whitespace().any(|cell| cell == "-")
        }) && show.contains("w7 — every window reset")
            && !show.contains(" · next "),
        "{list}\n{show}"
    );
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
            .any(|line| { line.trim_start().starts_with("legacy") && line.contains("completed") })
            && show.contains("✓ completed")
            && show.contains("MODE"),
        "{list}\n{show}"
    );
}

#[test]
fn loop_overlap_records_holder_and_preserves_one_shot() {
    let env = Env::new();
    loop_ok(
        &env,
        &["loop", "add", "busy", "--check", "true", "--at", "07:00"],
    );
    let lock_path = loop_run_lock_path(&env, "busy");
    std::fs::create_dir_all(lock_path.parent().expect("lock parent")).expect("mkdir runtime");
    let holder = RunLockInfo {
        pid: 42_424,
        started_at: Timestamp::now() - SignedDuration::from_secs(25 * 60),
    };
    std::fs::write(&lock_path, serde_json::to_vec(&holder).unwrap()).expect("write lock holder");
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("open lock");
    lock_file.try_lock().expect("hold loop run lock");

    let run = loop_ok(&env, &["loop", "run", "busy"]);
    assert!(
        run.contains("previous run still active (pid 42424, started 25m ago) — skipped"),
        "{run}"
    );
    assert!(read_loop_instances(&env).0.contains_key("busy"));
    let record = last_loop_record(&env);
    assert_eq!(record.result, LoopRunResult::Overlapped);
    assert!(
        record
            .error
            .as_deref()
            .is_some_and(|error| error.contains("pid 42424"))
    );
    let show = loop_ok(&env, &["loop", "show", "busy"]);
    assert!(
        show.contains("overlapped")
            && show.contains("run in progress")
            && show.contains("pid 42424")
            && show.contains("started 25m ago"),
        "{show}"
    );
    lock_file.unlock().expect("unlock loop run lock");
}

#[cfg(unix)]
#[test]
fn loop_stop_terminates_holder_and_records_cancellation() {
    let env = Env::new();
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "stuck",
            "--check",
            "parent=$PPID; while kill -0 \"$parent\" 2>/dev/null; do sleep 1; done",
            "--every",
            "15m",
        ],
    );
    let mut runner = env
        .rimz()
        .args(["loop", "run", "stuck"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn stuck loop runner");
    let info = wait_for_held_loop_lock(&mut runner, &loop_run_lock_path(&env, "stuck"));
    assert_eq!(info.pid, runner.id());
    let stopped = loop_ok(&env, &["loop", "stop", "stuck"]);
    assert!(
        stopped.contains("stopped") && stopped.contains("SIGTERM"),
        "{stopped}"
    );
    let released = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(loop_run_lock_path(&env, "stuck"))
        .expect("open released loop lock");
    released.try_lock().expect("loop lock released");
    assert!(!runner.wait().expect("wait for stopped runner").success());
    let record = last_loop_record(&env);
    assert_eq!(record.result, LoopRunResult::Canceled);
    assert_eq!(record.error.as_deref(), Some("stopped by rimz loop stop"));
}

#[test]
fn loop_add_rejects_invalid_action_shapes() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-validate", "feature-loop");
    let cases = [
        (
            vec!["loop", "add", "missing", "--every", "15m", "--prompt", "x"],
            "needs --agent, --wake, or --check",
        ),
        (
            vec![
                "loop", "add", "conflict", "--agent", "claude", "--wake", "@claude", "--every",
                "15m", "--prompt", "x",
            ],
            "cannot be used with",
        ),
        (
            vec![
                "loop",
                "add",
                "wake-mode",
                "--wake",
                "@claude",
                "--mode",
                "auto",
                "--every",
                "15m",
                "--prompt",
                "x",
            ],
            "only apply to --agent tasks",
        ),
    ];
    for (args, expected) in cases {
        let (_stdout, error) = loop_fail(&env, &args);
        assert!(error.contains(expected), "rimz {args:?}: {error}");
    }
}

fn loop_ok(env: &Env, args: &[&str]) -> String {
    let output = env.rimz().args(args).output().expect("rimz loop");
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
    assert_eq!(message.agent_id.as_str(), session);
    assert_eq!(message.status, MessageStatus::Queued);
    assert!(message.text.contains(text_fragment), "{}", message.text);
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
    let payload = serde_json::to_string(&payload).expect("payload");
    let owner = dummy_agent_process();
    let owner_pid = owner.id();
    reap_later(owner);
    let mut cmd = env.hook_command("claude");
    cmd.current_dir(cwd)
        .env("RIMZ_AGENT_PID", owner_pid.to_string());
    if let Some(channel) =
        rimz::harness::target::resolve_room_channel(&env.project_root, cwd, None, None)
    {
        cmd.env(rimz::harness::run::ENV_CHANNEL, channel);
    }
    let output = env
        .spawn_payload(cmd, &payload)
        .wait_with_output()
        .expect("wait hook");
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
    let path = env.project_root.join(".rimz/config.toml");
    std::fs::create_dir_all(path.parent().expect("project config dir"))
        .expect("mkdir project config");
    std::fs::write(path, text).expect("write project config");
}

fn read_loop_run_records(env: &Env) -> Vec<LoopRunRecord> {
    let path = run_log::log_path(&env.state_root());
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .map(|line| serde_json::from_str(line).expect("loop run record"))
        .collect()
}

fn write_loop_run_records(env: &Env, records: &[LoopRunRecord]) {
    let path = run_log::log_path(&env.state_root());
    std::fs::create_dir_all(path.parent().expect("log parent")).expect("mkdir log parent");
    let mut text = String::new();
    for record in records {
        text.push_str(&serde_json::to_string(record).expect("loop record json"));
        text.push('\n');
    }
    std::fs::write(path, text).expect("write loop run records");
}

fn read_loop_instances(env: &Env) -> Tasks {
    let path = rimz::harness::schedule::catalog::instances_path(&env.state_root());
    let Ok(text) = std::fs::read_to_string(path) else {
        return Tasks::default();
    };
    serde_json::from_str(&text).expect("loop instances")
}

fn read_loop_pauses(env: &Env) -> BTreeMap<String, PauseEntry> {
    let path = pauses::path(&env.state_root());
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    serde_json::from_str(&text).expect("loop pauses")
}

fn read_loop_strikes(env: &Env) -> BTreeMap<String, u32> {
    let path = strikes::path(&env.state_root());
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    serde_json::from_str(&text).expect("loop strikes")
}

fn write_loop_instances(env: &Env, tasks: Tasks) {
    let path = rimz::harness::schedule::catalog::instances_path(&env.state_root());
    std::fs::create_dir_all(path.parent().expect("instances parent")).expect("mkdir state");
    std::fs::write(path, serde_json::to_vec_pretty(&tasks).expect("json"))
        .expect("write loop instances");
}

fn write_loop_fire_state(env: &Env, stamps: BTreeMap<String, Timestamp>) {
    let path = env.runtime_paths().root.join("loop-fire.json");
    std::fs::create_dir_all(path.parent().expect("loop fire parent")).expect("mkdir runtime");
    std::fs::write(path, serde_json::to_vec_pretty(&stamps).expect("json"))
        .expect("write loop fire state");
}

fn loop_run_lock_path(env: &Env, name: &str) -> std::path::PathBuf {
    env.runtime_paths()
        .root
        .join(format!("loop-run-{name}.lock"))
}

#[cfg(unix)]
fn wait_for_held_loop_lock(child: &mut std::process::Child, path: &Path) -> RunLockInfo {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(file) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            && matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock))
            && let Ok(bytes) = std::fs::read(path)
            && let Ok(info) = serde_json::from_slice(&bytes)
        {
            return info;
        }
        assert!(
            child.try_wait().expect("poll loop runner").is_none(),
            "loop runner exited before holding its lock"
        );
        assert!(
            Instant::now() < deadline,
            "timed out waiting for loop run lock {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn append_legacy_loop_record(env: &Env, task: &str, result: LoopRunResult) {
    let path = run_log::log_path(&env.state_root());
    std::fs::create_dir_all(path.parent().expect("log parent")).expect("mkdir log parent");
    let result = serde_json::to_string(&result).expect("result json");
    let line =
        format!("{{\"task\":\"{task}\",\"at\":\"1970-01-01T00:00:10Z\",\"result\":{result}}}\n");
    std::fs::write(path, line).expect("write legacy loop run record");
}

fn loop_config_path(env: &Env) -> std::path::PathBuf {
    env.config_root().join("rimz").join("loop.toml")
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
