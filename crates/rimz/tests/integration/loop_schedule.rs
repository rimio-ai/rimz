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
use rimz::harness::schedule::run_log::{self, LoopRunRecord, LoopRunResult};
use rimz::harness::schedule::runner::RunLockInfo;
use rimz::harness::schedule::strikes;
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::message::MessageStatus;

use crate::common::{Env, ScrubSessionEnvExt};

#[test]
fn loop_watch_resolves_workspace_git_once_and_reloads_tasks() {
    let env = Env::new();
    let Some(real_git) = find_real_git() else {
        return;
    };
    let initialized = Command::new(&real_git)
        .args(["-C", env.project_root.to_str().expect("utf-8 project root")])
        .args(["init", "-q"])
        .status()
        .is_ok_and(|status| status.success());
    if !initialized {
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

    let first_deadline = Instant::now() + Duration::from_millis(1_200);
    let mut exited_early = None;
    while Instant::now() < first_deadline {
        if let Some(status) = child.try_wait().expect("poll loop watch") {
            exited_early = Some(status);
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    write_loop_config(
        &env,
        &format!(
            "[tasks.watch-reloaded]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"15m\"\n",
            env.project_root.display()
        ),
    );
    let final_deadline = Instant::now() + Duration::from_millis(1_300);
    while exited_early.is_none() && Instant::now() < final_deadline {
        if let Some(status) = child.try_wait().expect("poll loop watch") {
            exited_early = Some(status);
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    if exited_early.is_none() {
        child.kill().expect("terminate loop watch");
        let _ = child.wait().expect("reap loop watch");
    }
    drop(pair.master);
    let output =
        String::from_utf8_lossy(&reader_thread.join().expect("join pty reader")).into_owned();
    assert!(
        exited_early.is_none(),
        "loop watch exited before multiple repaints: {exited_early:?}\n{output}"
    );
    assert!(
        output.contains("watch-reloaded"),
        "loop watch should reload task files after startup:\n{output}"
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
            "workspace probe should run once at startup: {probe}\n{git_trace}",
        );
    }
}

#[test]
fn loop_add_bind_pins_live_session_and_run_queues_prompt() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-live", "feature-loop");

    let add_stdout = loop_ok(
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
    assert!(
        add_stdout.contains("pinned to claude session `sess-loop-live`")
            && add_stdout.contains("next fire:"),
        "loop add should explain wake pin and next fire: {add_stdout}"
    );
    let config = std::fs::read_to_string(loop_config_path(&env)).expect("read loop config");
    assert!(
        config.contains("session = \"sess-loop-live\""),
        "task should pin the live session id: {config}"
    );

    loop_ok(&env, &["loop", "run", "wake"]);

    let messages = env.store().list_pending_messages().expect("messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text, "next step");
    assert_eq!(messages[0].kind.as_str(), "claude");
    assert_eq!(messages[0].agent_id.as_str(), "sess-loop-live");
    assert_eq!(messages[0].status, MessageStatus::Queued);

    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].task, "wake");
    assert_eq!(records[0].result, LoopRunResult::Delivered);

    let stdout = loop_ok(&env, &["loop", "list"]);
    assert!(
        stdout.contains("NAME")
            && stdout.contains("TASK")
            && stdout.contains("SCHEDULE")
            && stdout.contains("NEXT")
            && stdout.contains("LAST")
            && stdout.contains("STATUS")
            && stdout.contains("COST")
            && stdout.contains("SOURCE")
            && !stdout.contains("RUNS")
            && !stdout.contains("RESULT"),
        "loop list should show grouped compact columns: {stdout}"
    );
    assert!(
        stdout
            .lines()
            .any(|line| line.contains("~/project") && line.contains("room")),
        "loop list should group rows under the project root and room state: {stdout}"
    );
    assert!(
        stdout.lines().any(|line| {
            line.trim_start().starts_with("wake")
                && line.contains("@claude")
                && line.contains("machine")
                && line.contains("✓ delivered")
        }),
        "loop list should fold run history for wake: {stdout}"
    );

    let stdout = loop_ok(&env, &["loop", "show", "wake"]);
    assert!(
        stdout.lines().any(|line| {
            line.contains("source:") && line.contains("machine") && line.contains("loop.toml")
        }),
        "loop show should name the machine task source file: {stdout}"
    );
}

#[test]
fn loop_add_round_trips_run_and_daily_budgets() {
    let env = Env::new();
    env.install_agent_hooks("claude");
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
        ],
    );

    let config = std::fs::read_to_string(loop_config_path(&env)).expect("read loop config");
    assert!(config.contains("budget = \"$5.00\""), "{config}");
    assert!(config.contains("budget-per-day = \"$20.00\""), "{config}");
    let loop_config: LoopConfig = toml::from_str(&config).expect("parse loop config");
    let task = loop_config.tasks.0.get("bounded").expect("bounded task");
    assert_eq!(task.budget.as_deref(), Some("$5.00"));
    assert_eq!(task.budget_per_day.as_deref(), Some("$20.00"));

    write_loop_run_records(
        &env,
        &[LoopRunRecord {
            task: "bounded".to_owned(),
            at: Timestamp::now(),
            result: LoopRunResult::Completed,
            mode: Some(rimz::harness::schedule::run_log::LoopRunMode::Manual),
            duration_ms: Some(1_000),
            error: None,
            check: None,
            run_id: None,
            transcript_path: None,
            last_message: None,
            target: None,
            cost_usd: Some(0.42),
            input_tokens: Some(12_000),
            output_tokens: Some(3_400),
        }],
    );
    let list = loop_ok(&env, &["loop", "list"]);
    assert!(list.contains("$0.42/$20"), "{list}");
    let show = loop_ok(&env, &["loop", "show", "bounded"]);
    assert!(show.contains("budget: $5 per run · $20 per day"), "{show}");
    assert!(show.contains("$0.42 today of $20 · $0.42 last"), "{show}");
    assert!(show.contains("cost: $0.42 · ↘ 12k ↗ 3k"), "{show}");
}

#[test]
fn loop_add_round_trips_verify_completion_rule() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "verified",
            "--agent",
            "claude",
            "--prompt",
            "fix it",
            "--every",
            "15m",
            "--verify",
            "cargo xtask gate",
            "--max-attempts",
            "4",
            "--max-strikes",
            "5",
        ],
    );

    let config = std::fs::read_to_string(loop_config_path(&env)).expect("read loop config");
    assert!(config.contains("verify = \"cargo xtask gate\""), "{config}");
    assert!(config.contains("max-attempts = 4"), "{config}");
    assert!(config.contains("max-strikes = 5"), "{config}");
    let loop_config: LoopConfig = toml::from_str(&config).expect("parse loop config");
    let task = loop_config.tasks.0.get("verified").expect("verified task");
    assert_eq!(task.verify.as_deref(), Some("cargo xtask gate"));
    assert_eq!(task.max_attempts, Some(4));
    assert_eq!(task.max_strikes, Some(5));

    let shown = loop_ok(&env, &["loop", "show", "verified"]);
    assert!(
        shown.contains("verify: cargo xtask gate (up to 4 attempts)"),
        "{shown}"
    );
}

#[test]
fn agent_budget_command_persists_absolute_relative_and_clear_edits() {
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
        let output = env
            .rimz()
            .args(["agents", "budget", "@claude", value, "--no-continue"])
            .output()
            .expect("rimz agents budget");
        assert!(
            output.status.success(),
            "budget {value} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let ledger = rimz::harness::budget::read_ledger(&env.runtime_paths(), &kind, &agent_id)
            .expect("budget ledger");
        assert_eq!(ledger.effective_cap_usd(), expected_cap);
        assert_eq!(ledger.disabled, disabled);
    }
}

#[test]
fn agent_budget_views_render_local_day_spend() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-budget-view", "feature-budget");
    let agent_id = AgentSessionId::from("sess-budget-view");
    let kind = AgentKind::new_unchecked("claude");

    let mut context = rimz::store::agent_context::empty_context("claude", jiff::Timestamp::now());
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
        vec!["agents", "budget", "@claude"],
        vec!["agents", "show", "@claude"],
    ] {
        let output = env.rimz().args(&args).output().expect("rimz budget view");
        assert!(
            output.status.success(),
            "rimz {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).expect("stdout");
        assert!(
            stdout.contains("$10.00"),
            "rimz {args:?} should render day-relative spend: {stdout}"
        );
    }
}

#[test]
fn loop_project_tasks_list_and_refuse_until_trusted() {
    let env = Env::new();
    write_project_config(
        &env,
        "[tasks.repo-check]\ncheck = \"true\"\nevery = \"15m\"\n",
    );

    let stdout = loop_ok(&env, &["loop", "list"]);
    assert!(
        stdout.contains("repo-check")
            && stdout.contains("project · untrusted")
            && stdout.contains("blocked · trust")
            && stdout.contains("1 task(s) blocked by project trust")
            && stdout.contains("review with `rimz trust`")
            && stdout.contains("approve with `rimz trust grant`"),
        "project task should stay visible with trust state: {stdout}"
    );
    let stdout = loop_ok(&env, &["loop", "show", "repo-check"]);
    assert!(
        stdout.lines().any(|line| {
            line.contains("source:")
                && line.contains("project · untrusted")
                && line.contains(".rimz/config.toml")
        }),
        "project task show should name the defining file: {stdout}"
    );
    assert!(
        stdout.contains("next blocked · trust")
            && stdout.contains("will not fire:")
            && stdout.contains("project trust is untrusted")
            && stdout.contains("review with `rimz trust`")
            && stdout.contains("approve with `rimz trust grant`"),
        "project task show should explain the trust block: {stdout}"
    );

    let (_stdout, stderr) = loop_fail(&env, &["loop", "run", "repo-check"]);
    assert!(
        stderr.contains("loop task `repo-check` is blocked — project trust is untrusted")
            && stderr.contains("configured in")
            && stderr.contains(".rimz/config.toml")
            && stderr.contains("review the project config with `rimz trust`")
            && stderr.contains("approve with `rimz trust grant`"),
        "project task should refuse before trust grant: {stderr}"
    );

    let grant = env
        .rimz()
        .args(["trust", "grant"])
        .output()
        .expect("trust grant");
    assert!(
        grant.status.success(),
        "trust grant failed: {}",
        String::from_utf8_lossy(&grant.stderr)
    );

    let stdout = loop_ok(&env, &["loop", "run", "repo-check"]);
    assert!(
        stdout.contains("loop `repo-check`: completed"),
        "trusted project task should run: {stdout}"
    );
}

#[test]
fn loop_list_rejects_old_task_keys() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.old]\n\
             spec = \"claude\"\n\
             prompt = \"wake\"\n\
             root = \"{}\"\n\
             at = \"07:00\"\n",
            env.project_root.display()
        ),
    );

    let (_stdout, stderr) = loop_fail(&env, &["loop", "list"]);

    assert!(
        stderr.contains("unknown field `spec`"),
        "old key should fail loudly: {stderr}"
    );
}

#[test]
fn loop_untrusted_project_shadow_does_not_block_machine_task() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.shared]\n\
             check = \"printf machine\"\n\
             root = \"{}\"\n\
             every = \"15m\"\n",
            env.project_root.display()
        ),
    );
    write_project_config(
        &env,
        "[tasks.shared]\ncheck = \"printf project\"\nevery = \"15m\"\n",
    );

    let stdout = loop_ok(&env, &["loop", "list"]);
    assert!(
        stdout.contains("shared") && stdout.contains("project · untrusted"),
        "untrusted project task should stay visible: {stdout}"
    );

    loop_ok(&env, &["loop", "run", "shared"]);
    let records = read_loop_run_records(&env);
    assert!(
        records
            .last()
            .and_then(|record| record.check.as_ref())
            .is_some_and(|check| check.output.contains("machine")),
        "untrusted project shadow should leave machine task runnable: {records:?}"
    );

    let grant = env
        .rimz()
        .args(["trust", "grant"])
        .output()
        .expect("trust grant");
    assert!(
        grant.status.success(),
        "trust grant failed: {}",
        String::from_utf8_lossy(&grant.stderr)
    );

    loop_ok(&env, &["loop", "run", "shared"]);
    let records = read_loop_run_records(&env);
    assert!(
        records
            .last()
            .and_then(|record| record.check.as_ref())
            .is_some_and(|check| check.output.contains("project")),
        "trusted project task should shadow machine task: {records:?}"
    );
}

#[test]
fn loop_add_ephemeral_tasks_use_instance_state() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-state", "feature-loop");

    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "later",
            "--wake",
            "@claude",
            "--in",
            "5m",
            "--prompt",
            "later wake",
        ],
    );
    assert!(
        read_loop_instances(&env).0.contains_key("later"),
        "--in should persist as state"
    );
    assert!(
        !loop_config_path(&env).exists(),
        "--in should not create loop.toml"
    );

    let history_before = read_loop_run_records(&env).len();
    loop_ok(&env, &["loop", "run", "later"]);
    assert!(
        !read_loop_instances(&env).0.contains_key("later"),
        "fired one-shot should be removed from state"
    );
    assert_eq!(read_loop_run_records(&env).len(), history_before + 1);

    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "morning",
            "--wake",
            "@claude",
            "--every",
            "weekday",
            "--at",
            "07:00",
            "--prompt",
            "weekday wake",
        ],
    );
    let loop_text = std::fs::read_to_string(loop_config_path(&env)).expect("read loop config");
    assert!(
        loop_text.contains("[tasks.morning]"),
        "recurring task should persist in loop.toml: {loop_text}"
    );
    assert!(
        !read_loop_instances(&env).0.contains_key("morning"),
        "recurring task should not persist as state"
    );
}

#[test]
fn loop_fire_keeps_ephemeral_task() {
    let env = Env::new();

    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "probe",
            "--check",
            "printf ok",
            "--at",
            "07:00",
        ],
    );
    assert!(
        read_loop_instances(&env).0.contains_key("probe"),
        "one-shot should persist as state"
    );

    for _ in 0..2 {
        let history_before = read_loop_run_records(&env).len();
        let stdout = loop_ok(&env, &["loop", "fire", "probe"]);
        assert!(
            stdout.contains("probe — check") && stdout.contains("✓ check passed (exit 0)"),
            "check-only manual fire should keep the check exit label: {stdout}"
        );
        assert!(
            read_loop_instances(&env).0.contains_key("probe"),
            "manual fire should keep the one-shot instance"
        );
        assert_eq!(read_loop_run_records(&env).len(), history_before + 1);
    }

    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 2);
    assert!(
        records
            .iter()
            .all(|record| record.task == "probe" && record.result == LoopRunResult::Completed)
    );
}

#[test]
fn loop_pause_renders_and_persists_indefinite_and_timed_state() {
    let env = Env::new();
    loop_ok(
        &env,
        &[
            "loop", "add", "nightly", "--check", "true", "--every", "15m",
        ],
    );

    let stdout = loop_ok(&env, &["loop", "pause", "nightly"]);
    assert!(
        stdout.contains("loop `nightly`: paused; resume with `rimz loop resume nightly`"),
        "pause should explain manual resume: {stdout}"
    );
    assert_eq!(
        read_loop_pauses(&env).get("nightly"),
        Some(&PauseEntry {
            until: None,
            strikes: None,
        })
    );
    let stdout = loop_ok(&env, &["loop", "list"]);
    assert!(
        stdout
            .lines()
            .any(|line| line.trim_start().starts_with("nightly") && line.contains("paused")),
        "list should replace NEXT with paused: {stdout}"
    );
    let stdout = loop_ok(&env, &["loop", "show", "nightly"]);
    assert!(
        stdout.contains("· paused") && stdout.contains("resume with `rimz loop resume nightly`"),
        "show should render pause state and hint: {stdout}"
    );

    let stdout = loop_ok(&env, &["loop", "pause", "nightly", "--for", "2h"]);
    assert!(
        stdout.contains("loop `nightly`: paused; resumes in 2h (")
            && read_loop_pauses(&env)
                .get("nightly")
                .is_some_and(|entry| entry.until.is_some()),
        "timed pause should overwrite the entry and render its end: {stdout}"
    );
    let stdout = loop_ok(&env, &["loop", "list"]);
    assert!(
        stdout.lines().any(|line| {
            line.trim_start().starts_with("nightly") && line.contains("paused · in")
        }),
        "list should render the timed resume: {stdout}"
    );
}

#[test]
fn loop_fire_runs_paused_task_and_resume_is_idempotent() {
    let env = Env::new();
    loop_ok(
        &env,
        &["loop", "add", "probe", "--check", "true", "--every", "15m"],
    );
    loop_ok(&env, &["loop", "pause", "probe"]);

    let stdout = loop_ok(&env, &["loop", "fire", "probe"]);
    assert!(
        stdout.contains("probe — check")
            && stdout.contains("task is paused; firing anyway")
            && stdout.contains("✓ check passed (exit 0)"),
        "manual fire should explain and bypass the pause: {stdout}"
    );

    let stdout = loop_ok(&env, &["loop", "resume", "probe"]);
    assert!(
        stdout.contains("loop `probe`: resumed"),
        "resume should lift the pause: {stdout}"
    );
    assert!(
        !loop_ok(&env, &["loop", "list"])
            .lines()
            .any(|line| line.trim_start().starts_with("probe") && line.contains("paused")),
        "resumed task should leave paused rendering"
    );
    let stdout = loop_ok(&env, &["loop", "resume", "probe"]);
    assert!(
        stdout.contains("loop `probe`: not paused"),
        "repeated resume should be a successful no-op: {stdout}"
    );
}

#[test]
fn loop_repeated_broken_deliveries_auto_pause_notify_and_resume() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-strikes", "feature-loop");
    let notify_log = env.project_root.join("loop-paused-notify.log");
    let config_path = env.config_root().join("rimz").join("config.toml");
    std::fs::create_dir_all(config_path.parent().expect("config parent"))
        .expect("mkdir config parent");
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

    loop_ok(&env, &["loop", "run", "watchdog"]);
    let show = loop_ok(&env, &["loop", "show", "watchdog"]);
    assert!(show.contains("strikes: 1/3"), "{show}");

    loop_ok(&env, &["loop", "run", "watchdog"]);
    let third = loop_ok(&env, &["loop", "run", "watchdog"]);
    assert!(
        third.contains("loop `watchdog`: paused after 3 consecutive failed fires"),
        "{third}"
    );
    assert_eq!(
        read_loop_pauses(&env).get("watchdog"),
        Some(&PauseEntry {
            until: None,
            strikes: Some(3),
        })
    );
    let list = loop_ok(&env, &["loop", "list"]);
    assert!(list.contains("paused · 3 strikes"), "{list}");
    let show = loop_ok(&env, &["loop", "show", "watchdog"]);
    assert!(
        show.contains("paused after 3 strikes — resume with `rimz loop resume watchdog`"),
        "{show}"
    );

    let fire = loop_ok(&env, &["loop", "fire", "watchdog"]);
    assert!(
        fire.contains("task is paused; firing anyway") && fire.contains("delivered"),
        "{fire}"
    );
    assert_eq!(read_loop_strikes(&env).get("watchdog"), Some(&4));
    assert_eq!(
        read_loop_pauses(&env)
            .get("watchdog")
            .and_then(|pause| pause.strikes),
        Some(3),
        "an active pause should not be replaced or notified twice"
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let notification = loop {
        let text = std::fs::read_to_string(&notify_log).unwrap_or_default();
        if text.contains("loop_paused|Rimz: loop watchdog paused") {
            break text;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "loop-paused notification was not delivered: {text}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert_eq!(notification.lines().count(), 1, "{notification}");

    let resumed = loop_ok(&env, &["loop", "resume", "watchdog"]);
    assert!(resumed.contains("loop `watchdog`: resumed"), "{resumed}");
    assert!(!read_loop_strikes(&env).contains_key("watchdog"));
    assert!(
        read_loop_pauses(&env)
            .get("watchdog")
            .is_some_and(|pause| pause.until.is_some() && pause.strikes.is_none())
    );
    assert!(!loop_ok(&env, &["loop", "list"]).contains("paused ·"));
}

#[test]
fn loop_task_edits_keep_pause_overlay_consistent() {
    let env = Env::new();
    loop_ok(
        &env,
        &["loop", "add", "old", "--check", "true", "--every", "15m"],
    );
    loop_ok(&env, &["loop", "pause", "old"]);
    loop_ok(&env, &["loop", "rename", "old", "new"]);
    let pauses = read_loop_pauses(&env);
    assert!(!pauses.contains_key("old") && pauses.contains_key("new"));

    loop_ok(&env, &["loop", "remove", "new"]);
    assert!(read_loop_pauses(&env).is_empty());

    loop_ok(
        &env,
        &["loop", "add", "swap", "--check", "true", "--every", "15m"],
    );
    loop_ok(&env, &["loop", "pause", "swap"]);
    let stdout = loop_ok(
        &env,
        &["loop", "add", "swap", "--check", "true", "--every", "30m"],
    );
    assert!(
        stdout.contains("pause: cleared") && !read_loop_pauses(&env).contains_key("swap"),
        "replacing a task should clear and report its old pause: {stdout}"
    );
}

#[test]
fn loop_pause_accepts_untrusted_project_task_as_local_state() {
    let env = Env::new();
    write_project_config(
        &env,
        "[tasks.repo-check]\ncheck = \"true\"\nevery = \"15m\"\n",
    );

    let stdout = loop_ok(&env, &["loop", "pause", "repo-check"]);
    assert!(
        stdout.contains("loop `repo-check`: paused")
            && !stdout.contains("trust grant")
            && read_loop_pauses(&env).contains_key("repo-check"),
        "pause should be a local overlay without changing project trust: {stdout}"
    );
    let stdout = loop_ok(&env, &["loop", "list"]);
    assert!(
        stdout.lines().any(|line| {
            line.trim_start().starts_with("repo-check")
                && line.contains("project · untrusted")
                && line.contains("blocked · trust")
        }),
        "project trust should be the stronger visible block: {stdout}"
    );
    assert!(read_loop_pauses(&env).contains_key("repo-check"));
}

#[test]
fn loop_add_replaces_same_name_across_config_and_state() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-replace", "feature-loop");

    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "swap",
            "--wake",
            "@claude",
            "--every",
            "15m",
            "--prompt",
            "durable wake",
        ],
    );
    assert!(
        std::fs::read_to_string(loop_config_path(&env))
            .expect("read loop config")
            .contains("[tasks.swap]"),
        "durable task should persist in loop.toml"
    );

    loop_ok(
        &env,
        &[
            "loop", "add", "swap", "--wake", "@claude", "--in", "5m", "--prompt", "one shot",
        ],
    );
    assert!(
        read_loop_instances(&env).0.contains_key("swap"),
        "ephemeral replacement should persist in state"
    );
    let loop_text = std::fs::read_to_string(loop_config_path(&env)).expect("read loop config");
    assert!(
        !loop_text.contains("[tasks.swap]"),
        "ephemeral replacement should remove config task: {loop_text}"
    );

    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "swap",
            "--wake",
            "@claude",
            "--every",
            "30m",
            "--prompt",
            "durable again",
        ],
    );
    assert!(
        !read_loop_instances(&env).0.contains_key("swap"),
        "durable replacement should remove state task"
    );
    let loop_text = std::fs::read_to_string(loop_config_path(&env)).expect("read loop config");
    assert!(
        loop_text.contains("[tasks.swap]"),
        "durable replacement should persist in loop.toml: {loop_text}"
    );
}

#[test]
fn loop_run_check_only_logs_command_result() {
    let env = Env::new();

    for (name, command, expected) in [
        ("check-ok", "printf ok", LoopRunResult::Completed),
        ("check-fail", "printf fail; exit 1", LoopRunResult::Failed),
    ] {
        loop_ok(
            &env,
            &["loop", "add", name, "--check", command, "--every", "15m"],
        );
        loop_ok(&env, &["loop", "run", name]);
        let records = read_loop_run_records(&env);
        assert_eq!(
            records.last().map(|record| record.result),
            Some(expected),
            "{name} should log {expected:?}"
        );
        let record = records.last().expect("last record");
        assert_eq!(
            record.check.as_ref().and_then(|check| check.code),
            Some(if expected == LoopRunResult::Completed {
                0
            } else {
                1
            })
        );
        assert!(
            record
                .check
                .as_ref()
                .is_some_and(|check| check.output.contains(name.strip_prefix("check-").unwrap())),
            "{name} should keep check output"
        );
    }
}

#[test]
fn loop_run_records_daily_budget_skip() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "daily-bounded",
            "--agent",
            "claude",
            "--prompt",
            "bounded work",
            "--every",
            "15m",
            "--budget",
            "$5.00",
            "--budget-per-day",
            "$5.00",
        ],
    );
    let mut spent = LoopRunRecord::new(
        "daily-bounded",
        LoopRunResult::Completed,
        rimz::harness::schedule::run_log::LoopRunMode::Manual,
        1,
    );
    spent.cost_usd = Some(5.0);
    write_loop_run_records(&env, &[spent]);

    loop_ok(&env, &["loop", "run", "daily-bounded"]);

    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 2);
    let skip = records.last().expect("budget skip record");
    assert_eq!(skip.result, LoopRunResult::BudgetSkipped);
    assert!(
        skip.error
            .as_deref()
            .is_some_and(|error| error.contains("daily budget")),
        "runner should record the daily-budget reason: {skip:?}"
    );
    assert!(
        std::fs::read_to_string(loop_config_path(&env))
            .expect("read loop config")
            .contains("[tasks.daily-bounded]"),
        "budget skips should keep the task definition"
    );
}

#[test]
fn loop_check_failure_show_prints_exit_and_output() {
    let env = Env::new();
    let command = "definitely-missing-rimz-loop-command";
    loop_ok(
        &env,
        &[
            "loop", "add", "missing", "--check", command, "--every", "15m",
        ],
    );

    let fire_stdout = loop_ok(&env, &["loop", "fire", "missing"]);
    assert!(
        fire_stdout.contains("✗ check failed (exit 127"),
        "fire should print outcome summary: {fire_stdout}"
    );
    assert!(
        fire_stdout.contains(command),
        "fire should print check output tail: {fire_stdout}"
    );
    loop_ok(&env, &["loop", "fire", "missing"]);

    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 2);
    let record = records.last().expect("check record");
    assert_eq!(record.result, LoopRunResult::Failed);
    let check = record.check.as_ref().expect("check detail");
    assert_eq!(check.code, Some(127));
    assert!(check.output.contains(command));

    let stdout = loop_ok(&env, &["loop", "show", "missing"]);
    assert!(
        stdout.contains("STATUS") && stdout.contains("failed"),
        "show should print runs table: {stdout}"
    );
    assert!(
        stdout
            .lines()
            .any(|line| { line.contains("✗ failed (exit 127) ×2") && line.contains(command) }),
        "show should collapse repeated failures and print note: {stdout}"
    );
    assert!(
        stdout.contains("LAST RUN — ✗ failed (exit 127)")
            && stdout.contains("│ sh:")
            && stdout.contains(command),
        "show should print output detail: {stdout}"
    );

    let stdout = loop_ok(&env, &["loop", "list"]);
    assert!(
        !stdout.contains("NOTE"),
        "list should merge failure notes into STATUS: {stdout}"
    );
    assert!(
        stdout.lines().any(|line| {
            line.trim_start().starts_with("missing")
                && line.contains("✗ failed (exit 127) ×2")
                && line.contains(command)
        }),
        "list should print failure streak and note: {stdout}"
    );
}

#[test]
fn loop_logs_prints_full_forensics_and_filters_failures() {
    let env = Env::new();
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "history",
            "--check",
            "printf broken; exit 1",
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
            "printf healthy",
            "--every",
            "15m",
        ],
    );
    loop_ok(&env, &["loop", "fire", "history"]);

    let logs = loop_ok(&env, &["loop", "logs", "history"]);
    assert!(
        logs.contains("✗ failed (exit 1)")
            && logs.contains("  │ broken")
            && logs.contains("✓ completed")
            && logs.contains("  │ healthy"),
        "{logs}"
    );
    assert!(logs.find("broken").unwrap() < logs.find("healthy").unwrap());

    let failed = loop_ok(&env, &["loop", "logs", "history", "--failed"]);
    assert!(
        failed.contains("✗ failed (exit 1)") && failed.contains("  │ broken"),
        "{failed}"
    );
    assert!(!failed.contains("healthy"), "{failed}");

    let (_stdout, stderr) = loop_fail(&env, &["loop", "logs", "missing"]);
    assert!(stderr.contains("no loop task named `missing`"), "{stderr}");
}

#[test]
fn loop_run_check_guard_skips_or_delivers_with_output() {
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
            .is_empty(),
        "healthy check should not queue a message"
    );
    assert_eq!(
        read_loop_run_records(&env)
            .last()
            .map(|record| record.result),
        Some(LoopRunResult::CheckSkipped)
    );
    let fire_stdout = loop_ok(&env, &["loop", "fire", "healthy"]);
    assert!(
        fire_stdout.contains("probe-line")
            && fire_stdout.contains("check passed (exit 0)")
            && fire_stdout.contains("@claude#project not woken")
            && fire_stdout.contains("fires when the check fails"),
        "manual fire should stream check output and explain no wake: {fire_stdout}"
    );

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
            "--prompt",
            "fix it",
        ],
    );
    let fire_stdout = loop_ok(&env, &["loop", "fire", "broken"]);
    assert!(
        fire_stdout.contains("  │ boom")
            && fire_stdout.contains("✗ check failed (exit 1)")
            && fire_stdout.contains("→ waking @claude#project")
            && fire_stdout.contains("✓ delivered to @claude#project"),
        "manual fire should connect the tripped check to delivery: {fire_stdout}"
    );
    let messages = env.store().list_pending_messages().expect("messages");
    assert_eq!(messages.len(), 1);
    assert!(messages[0].text.contains("fix it"));
    assert!(
        messages[0]
            .text
            .contains("check `printf boom; exit 1` exited 1")
    );
    assert!(messages[0].text.contains("boom"));
    let records = read_loop_run_records(&env);
    let broken = records.last().expect("broken run record");
    assert_eq!(broken.result, LoopRunResult::Delivered);
    let check = broken.check.as_ref().expect("guard check detail");
    assert_eq!(check.code, Some(1));
    assert!(check.output.contains("boom"));
    let skipped = records
        .iter()
        .find(|record| record.task == "healthy")
        .expect("healthy run record");
    assert_eq!(skipped.result, LoopRunResult::CheckSkipped);
    assert_eq!(skipped.check.as_ref().and_then(|check| check.code), Some(0));
}

#[test]
fn loop_run_error_records_and_show_displays_message() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-error", "feature-loop");
    write_loop_config(
        &env,
        &format!(
            "[tasks.bad_prompt]\n\
             wake = {{ kind = \"claude\", session = \"sess-loop-error\", handle = \"@claude\" }}\n\
             prompt-file = \"missing-prompt.txt\"\n\
             root = \"{}\"\n\
             every = \"15m\"\n",
            env.project_root.display()
        ),
    );

    let history_before = read_loop_run_records(&env).len();
    loop_fail(&env, &["loop", "run", "bad_prompt"]);
    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), history_before + 1);
    let record = records.last().expect("errored record");
    assert_eq!(record.task, "bad_prompt");
    assert_eq!(record.result, LoopRunResult::Errored);
    assert!(
        record
            .error
            .as_deref()
            .is_some_and(|error| error.contains("reading prompt-file")),
        "record should store error chain: {record:?}"
    );
    assert!(
        std::fs::read_to_string(loop_config_path(&env))
            .expect("read loop config")
            .contains("[tasks.bad_prompt]"),
        "pre-dispatch errors should keep the task definition"
    );

    let stdout = loop_ok(&env, &["loop", "show", "bad_prompt"]);
    assert!(
        stdout.contains("error") && stdout.contains("reading prompt-file"),
        "show should display stored error: {stdout}"
    );
}

#[test]
fn loop_run_missing_machine_prompt_error_names_task() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    write_loop_config(
        &env,
        &format!(
            "[tasks.named_spawn]\n\
             agent = \"claude\"\n\
             root = \"{}\"\n\
             at = \"07:00\"\n",
            env.project_root.display()
        ),
    );

    let (_stdout, stderr) = loop_fail(&env, &["loop", "run", "named_spawn"]);

    assert!(
        stderr.contains("loop task `named_spawn` has no prompt")
            && !stderr.contains("loop task `claude` has no prompt"),
        "missing prompt error should name the task: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn loop_scheduled_spawn_consumes_one_shot_only_after_runner_preflight() {
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
    let history_before = read_loop_run_records(&dispatch).len();
    let output = dispatch
        .rimz()
        .env("PATH", &empty_path)
        .args(["loop", "run", "dispatch-fails"])
        .output()
        .expect("run scheduled spawn");
    assert!(
        !output.status.success(),
        "missing agent binary should fail\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("finding `claude` on PATH"),
        "failure should come from supervised dispatch preflight: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !read_loop_instances(&dispatch)
            .0
            .contains_key("dispatch-fails"),
        "terminal dispatch failures should not retry a consumed one-shot"
    );
    assert_eq!(read_loop_run_records(&dispatch).len(), history_before + 1);

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
    let history_before = read_loop_run_records(&preflight).len();
    let (_stdout, stderr) = loop_fail(&preflight, &["loop", "run", "preflight-fails"]);
    assert!(stderr.contains("hooks are not installed"), "{stderr}");
    assert!(
        read_loop_instances(&preflight)
            .0
            .contains_key("preflight-fails"),
        "runner preflight failures should keep a one-shot definition"
    );
    assert_eq!(read_loop_run_records(&preflight).len(), history_before + 1);
}

#[test]
fn loop_show_displays_shadowed_error_and_run_tail() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.forensics]\n\
             agent = \"codex\"\n\
             prompt = \"go\"\n\
             check = \"true\"\n\
             root = \"{}\"\n\
             every = \"15m\"\n",
            env.project_root.display()
        ),
    );
    let paths = rimz::StatePaths::under(env.workspace_id.clone(), &env.state_root()).unwrap();
    paths.ensure_dirs().unwrap();
    let mut run_record = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "go".to_owned(),
        env.project_root.clone(),
    );
    run_record.status = RunStatus::Failed;
    run_record.failure_tail = Some("agent startup failed\nmissing binary".to_owned());
    run_record.transcript_path = Some("/tmp/rimz-transcript.jsonl".to_owned());
    rimz::harness::run::create(&paths, &run_record).unwrap();
    write_loop_run_records(
        &env,
        &[
            LoopRunRecord {
                task: "forensics".to_owned(),
                at: Timestamp::from_second(10).expect("timestamp"),
                result: LoopRunResult::Errored,
                mode: Some(rimz::harness::schedule::run_log::LoopRunMode::Manual),
                duration_ms: Some(42),
                error: Some(
                    "reading system-prompt-file `/missing.md`\ncaused by: not found".to_owned(),
                ),
                check: None,
                run_id: None,
                transcript_path: None,
                last_message: None,
                target: None,
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
            },
            LoopRunRecord {
                task: "forensics".to_owned(),
                at: Timestamp::from_second(20).expect("timestamp"),
                result: LoopRunResult::Failed,
                mode: Some(rimz::harness::schedule::run_log::LoopRunMode::Manual),
                duration_ms: Some(50),
                error: None,
                check: None,
                run_id: Some(run_record.run_id.to_string()),
                transcript_path: Some("/tmp/rimz-transcript.jsonl".to_owned()),
                last_message: None,
                target: None,
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
            },
        ],
    );

    let stdout = loop_ok(&env, &["loop", "show", "forensics"]);

    assert!(stdout.contains("LAST RUN — ✗ failed (exit 1)"), "{stdout}");
    assert!(stdout.contains("  output tail:"), "{stdout}");
    assert!(
        stdout.contains("  │ agent startup failed\n  │ missing binary"),
        "{stdout}"
    );
    assert!(
        stdout.contains("  transcript: /tmp/rimz-transcript.jsonl"),
        "{stdout}"
    );
    assert!(stdout.contains("AGENT RUNS — 1 of 2 runs"), "{stdout}");
    assert!(
        stdout.contains("last failure — ✗ error")
            && stdout.contains("dig in: rimz loop logs forensics --failed"),
        "{stdout}"
    );
    assert!(!stdout.contains("  error:"), "{stdout}");
    assert!(!stdout.contains("caused by: not found"), "{stdout}");
}

#[test]
fn loop_run_poll_until_fires_once_and_expires() {
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
    assert!(
        read_loop_instances(&env).0.contains_key("green"),
        "poll-until should persist as state"
    );
    let history_before = read_loop_run_records(&env).len();
    loop_ok(&env, &["loop", "run", "green"]);
    assert_eq!(
        env.store().list_pending_messages().expect("messages").len(),
        1
    );
    assert!(
        !read_loop_instances(&env).0.contains_key("green"),
        "poll-until should be removed after firing"
    );
    assert_eq!(read_loop_run_records(&env).len(), history_before + 1);

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
                deadline: Some(Timestamp::from_second(1).expect("timestamp")),
                ..TaskEntry::default()
            },
        )])),
    );
    let history_before = read_loop_run_records(&expired).len();
    loop_ok(&expired, &["loop", "run", "expired"]);
    assert!(read_loop_instances(&expired).0.is_empty());
    assert_eq!(read_loop_run_records(&expired).len(), history_before + 1);
    assert_eq!(
        read_loop_run_records(&expired)
            .last()
            .map(|record| record.result),
        Some(LoopRunResult::Expired)
    );
    assert!(
        expired
            .store()
            .list_pending_messages()
            .expect("messages")
            .is_empty(),
        "expired poll-until should not queue"
    );
}

#[test]
fn loop_run_bind_git_worktree_session_queues_prompt() {
    let env = Env::new();
    if !init_git_repo(&env.project_root) {
        return;
    }
    let worktree = env.home_root.join("project-worktrees").join("feature-loop");
    std::fs::create_dir_all(worktree.parent().expect("worktree parent")).expect("mkdir worktrees");
    assert!(
        git_ok(
            &env.project_root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feature-loop",
                worktree.to_str().expect("utf8 worktree"),
            ],
        ),
        "git worktree add failed"
    );
    env.install_agent_hooks("claude");
    register_running_agent_at(&env, "sess-loop-worktree", "feature-loop", &worktree);

    let add = env
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
        add.status.success(),
        "loop add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );

    loop_ok(&env, &["loop", "run", "wake-worktree"]);

    let messages = env.store().list_pending_messages().expect("messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text, "worktree next step");
    assert_eq!(messages[0].agent_id.as_str(), "sess-loop-worktree");
}

#[test]
fn loop_run_bind_dead_session_reaps_schedule() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.dead]\n\
             wake = {{ kind = \"claude\", session = \"sess-dead\", handle = \"@claude\" }}\n\
             prompt = \"wake up\"\n\
             root = \"{}\"\n\
             at = \"07:00\"\n",
            env.project_root.display()
        ),
    );

    let history_before = read_loop_run_records(&env).len();
    let stdout = loop_ok(&env, &["loop", "run", "dead"]);
    assert!(
        stdout.contains("not alive; removing schedule"),
        "dead target should be reported: {stdout}"
    );
    let config = std::fs::read_to_string(loop_config_path(&env)).expect("read loop config");
    assert!(
        !config.contains("[tasks.dead]"),
        "dead schedule should be removed: {config}"
    );
    assert_eq!(read_loop_run_records(&env).len(), history_before + 1);
}

#[test]
fn loop_fire_bind_dead_session_keeps_schedule() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.dead]\n\
             wake = {{ kind = \"claude\", session = \"sess-dead\", handle = \"@claude\" }}\n\
             prompt = \"wake up\"\n\
             root = \"{}\"\n\
             at = \"07:00\"\n",
            env.project_root.display()
        ),
    );

    let history_before = read_loop_run_records(&env).len();
    let stdout = loop_ok(&env, &["loop", "fire", "dead"]);
    assert!(
        stdout.contains("○ @claude not alive — schedule left in place"),
        "dead target should be reported: {stdout}"
    );
    let config = std::fs::read_to_string(loop_config_path(&env)).expect("read loop config");
    assert!(
        config.contains("[tasks.dead]"),
        "manual fire should keep dead schedule: {config}"
    );
    assert_eq!(
        read_loop_run_records(&env)
            .last()
            .map(|record| record.result),
        Some(LoopRunResult::TargetGone)
    );
    assert_eq!(read_loop_run_records(&env).len(), history_before + 1);
}

#[test]
fn loop_fire_bind_delivers_prompt() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-fire", "feature-loop");

    loop_ok(
        &env,
        &[
            "loop", "add", "manual", "--wake", "@claude", "--every", "15m", "--prompt", "fire now",
        ],
    );

    loop_ok(&env, &["loop", "fire", "manual"]);

    let messages = env.store().list_pending_messages().expect("messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text, "fire now");
    assert_eq!(messages[0].agent_id.as_str(), "sess-loop-fire");
    assert_eq!(
        read_loop_run_records(&env)
            .last()
            .map(|record| record.result),
        Some(LoopRunResult::Delivered)
    );
}

#[test]
fn loop_list_next_uses_room_arm_stamp() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.next]\n\
             check = \"true\"\n\
             root = \"{}\"\n\
             every = \"15m\"\n",
            env.project_root.display()
        ),
    );

    let stdout = loop_ok(&env, &["loop", "list"]);
    assert!(
        stdout.contains("NEXT"),
        "list should include NEXT: {stdout}"
    );
    assert!(
        stdout.lines().any(|line| {
            line.trim_start().starts_with("next")
                && line.contains("every 15m")
                && line.split_whitespace().last() == Some("-")
        }),
        "no arm stamp should render dash: {stdout}"
    );

    write_loop_fire_state(
        &env,
        BTreeMap::from([(
            "next".to_owned(),
            Timestamp::now() - SignedDuration::from_secs(16 * 60),
        )]),
    );
    let stdout = loop_ok(&env, &["loop", "list"]);
    assert!(
        stdout
            .lines()
            .any(|line| line.trim_start().starts_with("next") && line.contains("due")),
        "due arm stamp should render due: {stdout}"
    );
}

#[test]
fn loop_add_reset_is_ping_only_and_renders_without_cold_cache() {
    let env = Env::new();

    let (_stdout, stderr) = loop_fail(
        &env,
        &[
            "loop", "add", "bad", "--agent", "claude", "--every", "reset",
        ],
    );
    assert!(
        stderr.contains("<kind>-ping"),
        "reset cadence should name the ping requirement: {stderr}"
    );

    env.install_agent_hooks("claude");
    let (_stdout, stderr) = loop_fail(
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
        stderr.contains("loop task `pingless` needs a prompt"),
        "promptless ping should fail: {stderr}"
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
    let config = std::fs::read_to_string(loop_config_path(&env)).expect("read loop config");
    assert!(
        config.contains("every = \"reset\""),
        "reset cadence should persist in loop.toml: {config}"
    );

    write_loop_fire_state(
        &env,
        BTreeMap::from([(
            "w7".to_owned(),
            Timestamp::now() - SignedDuration::from_secs(60),
        )]),
    );
    let stdout = loop_ok(&env, &["loop", "list"]);
    assert!(
        stdout
            .lines()
            .any(|line| line.trim_start().starts_with("w7")
                && line.contains("every window reset")
                && line.split_whitespace().any(|cell| cell == "-")),
        "cold cache should render reset with dash next: {stdout}"
    );
    let stdout = loop_ok(&env, &["loop", "show", "w7"]);
    assert!(
        stdout.contains("w7 — every window reset") && !stdout.contains(" · next "),
        "show should render reset with dash next: {stdout}"
    );
}

#[test]
fn loop_show_and_list_fold_legacy_records() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.legacy]\n\
             check = \"true\"\n\
             root = \"{}\"\n\
             every = \"15m\"\n",
            env.project_root.display()
        ),
    );
    append_legacy_loop_record(&env, "legacy", LoopRunResult::Completed);

    let stdout = loop_ok(&env, &["loop", "list"]);
    assert!(
        stdout
            .lines()
            .any(|line| line.trim_start().starts_with("legacy") && line.contains("✓ completed")),
        "list should fold legacy record: {stdout}"
    );

    let stdout = loop_ok(&env, &["loop", "show", "legacy"]);
    assert!(
        stdout.contains("✓ completed") && stdout.contains("MODE"),
        "show should render legacy record with defaulted fields: {stdout}"
    );
}

#[test]
fn loop_run_overlapped_records_skip_and_keeps_task_state() {
    let env = Env::new();
    loop_ok(
        &env,
        &["loop", "add", "busy", "--check", "true", "--at", "07:00"],
    );
    assert!(
        read_loop_instances(&env).0.contains_key("busy"),
        "one-shot should persist as instance state"
    );
    let lock_path = loop_run_lock_path(&env, "busy");
    std::fs::create_dir_all(lock_path.parent().expect("lock parent")).expect("mkdir runtime");
    let holder = RunLockInfo {
        pid: 42_424,
        started_at: Timestamp::now() - SignedDuration::from_secs(25 * 60),
    };
    std::fs::write(
        &lock_path,
        serde_json::to_vec(&holder).expect("serialize lock holder"),
    )
    .expect("write lock holder");
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("open lock");
    lock_file.try_lock().expect("hold loop run lock");

    let history_before = read_loop_run_records(&env).len();
    let stdout = loop_ok(&env, &["loop", "run", "busy"]);
    assert!(
        stdout.contains("previous run still active (pid 42424, started 25m ago) — skipped")
            && stdout.contains("stop it with `rimz loop stop busy`"),
        "overlap should print skip message: {stdout}"
    );
    assert!(
        read_loop_instances(&env).0.contains_key("busy"),
        "overlapped run should not remove task state"
    );
    let records = read_loop_run_records(&env);
    assert_eq!(
        records.last().map(|record| record.result),
        Some(LoopRunResult::Overlapped)
    );
    assert_eq!(
        records.last().and_then(|record| record.error.as_deref()),
        Some("previous run still active (pid 42424, started 25m ago) — skipped")
    );
    assert_eq!(records.len(), history_before + 1);

    let history_before = records.len();
    let stdout = loop_ok(&env, &["loop", "fire", "busy"]);
    assert!(
        stdout.contains("previous run still active (pid 42424, started 25m ago) — skipped")
            && stdout.contains("stop it with `rimz loop stop busy`"),
        "manual overlap should print holder details: {stdout}"
    );
    assert_eq!(read_loop_run_records(&env).len(), history_before + 1);
    assert!(read_loop_instances(&env).0.contains_key("busy"));

    let mut linked_run = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "busy loop task".to_owned(),
        env.project_root.clone(),
    );
    linked_run.status = RunStatus::Running;
    linked_run.loop_task = Some("busy".to_owned());
    rimz::harness::run::create(env.store().paths(), &linked_run).expect("create linked run");

    let stdout = loop_ok(&env, &["loop", "show", "busy"]);
    assert!(
        stdout.contains("overlapped")
            && stdout.contains(linked_run.run_id.as_str())
            && stdout.contains("stop with `rimz loop stop busy`")
            && stdout.lines().any(|line| {
                line.contains("active:")
                    && line.contains("run in progress")
                    && line.contains("pid 42424")
                    && line.contains("started 25m ago")
            }),
        "show should display overlapped record and active holder: {stdout}"
    );
    lock_file.unlock().expect("unlock loop run lock");
}

#[test]
fn loop_show_displays_the_effective_scheduled_timeout() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "default-timeout = \"3h\"\n\
             [tasks.bounded]\n\
             agent = \"codex\"\n\
             prompt = \"bounded work\"\n\
             root = \"{}\"\n\
             every = \"15m\"\n",
            env.project_root.display()
        ),
    );

    let stdout = loop_ok(&env, &["loop", "show", "bounded"]);
    assert!(stdout.contains("timeout: 3h (default)"), "{stdout}");
}

#[test]
fn loop_stop_reports_when_no_run_is_active() {
    let env = Env::new();
    loop_ok(
        &env,
        &["loop", "add", "idle", "--check", "true", "--every", "15m"],
    );

    let stdout = loop_ok(&env, &["loop", "stop", "idle"]);
    assert!(stdout.contains("loop `idle`: no active run"), "{stdout}");
}

#[cfg(unix)]
#[test]
fn loop_stop_sigterms_an_unlinked_holder_and_records_cancellation() {
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

    let stdout = loop_ok(&env, &["loop", "stop", "stuck"]);
    assert!(
        stdout.contains("loop `stuck`: stopped") && stdout.contains("SIGTERM"),
        "{stdout}"
    );
    let released = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(loop_run_lock_path(&env, "stuck"))
        .expect("open released loop lock");
    released.try_lock().expect("loop lock released");
    let status = runner.wait().expect("wait for stopped runner");
    assert!(!status.success(), "SIGTERMed runner should not succeed");

    let records = read_loop_run_records(&env);
    let stopped = records.last().expect("stopped history record");
    assert_eq!(stopped.task, "stuck");
    assert_eq!(stopped.result, LoopRunResult::Canceled);
    assert_eq!(stopped.error.as_deref(), Some("stopped by rimz loop stop"));
}

#[test]
fn loop_run_bind_tilde_root_queues_prompt() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-tilde", "feature-loop");
    write_loop_config(
        &env,
        "[tasks.tilde]\n\
         wake = { kind = \"claude\", session = \"sess-loop-tilde\", handle = \"@claude\" }\n\
         prompt = \"tilde wake\"\n\
         root = \"~/project\"\n\
         every = \"15m\"\n",
    );

    loop_ok(&env, &["loop", "run", "tilde"]);

    let messages = env.store().list_pending_messages().expect("messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text, "tilde wake");
    assert_eq!(messages[0].agent_id.as_str(), "sess-loop-tilde");
}

#[test]
fn loop_add_bind_validates_mode_selection() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-validate", "feature-loop");

    let (_stdout, stderr) = loop_fail(
        &env,
        &["loop", "add", "bad", "--every", "15m", "--prompt", "x"],
    );
    assert!(
        stderr.contains("needs --agent, --wake, or --check"),
        "unexpected stderr: {stderr}"
    );

    loop_fail(
        &env,
        &[
            "loop", "add", "bad", "--agent", "claude", "--wake", "@claude", "--every", "15m",
            "--prompt", "x",
        ],
    );

    let (_stdout, stderr) = loop_fail(
        &env,
        &[
            "loop", "add", "bad", "--wake", "@claude", "--mode", "auto", "--every", "15m",
            "--prompt", "x",
        ],
    );
    assert!(
        stderr.contains("only apply to --agent tasks"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn loop_add_project_rejects_one_shots() {
    let env = Env::new();

    for args in [
        &[
            "loop",
            "add",
            "bad-at",
            "--project",
            "--check",
            "true",
            "--at",
            "07:00",
        ][..],
        &[
            "loop",
            "add",
            "bad-in",
            "--project",
            "--check",
            "true",
            "--in",
            "30m",
        ][..],
    ] {
        let (_stdout, stderr) = loop_fail(&env, args);
        assert!(
            stderr.contains("must repeat") && stderr.contains("--every or --cron"),
            "unexpected stderr: {stderr}"
        );
    }
}

#[test]
fn loop_rename_moves_config_and_instance_entries() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "# keep this task comment\n\
             [tasks.old]\n\
             check = \"true\"\n\
             root = \"{}\"\n\
             every = \"15m\"\n",
            env.project_root.display()
        ),
    );

    let stdout = loop_ok(&env, &["loop", "rename", "old", "new"]);
    assert!(
        stdout.contains("renamed loop task `old` to `new`"),
        "unexpected stdout: {stdout}"
    );

    let config = std::fs::read_to_string(loop_config_path(&env)).expect("read loop config");
    assert!(config.contains("# keep this task comment"));
    assert!(config.contains("[tasks.new]"), "new task missing: {config}");
    assert!(
        !config.contains("[tasks.old]"),
        "old task should be gone: {config}"
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

    loop_ok(&env, &["loop", "rename", "old-state", "new-state"]);

    let instances = read_loop_instances(&env);
    assert!(!instances.0.contains_key("old-state"));
    assert!(instances.0.contains_key("new-state"));
}

#[test]
fn loop_rename_rejects_collision_and_missing() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.old]\n\
             check = \"true\"\n\
             root = \"{}\"\n\
             every = \"15m\"\n\
             [tasks.existing]\n\
             check = \"true\"\n\
             root = \"{}\"\n\
             every = \"15m\"\n",
            env.project_root.display(),
            env.project_root.display()
        ),
    );

    let (_stdout, stderr) = loop_fail(&env, &["loop", "rename", "old", "existing"]);
    assert!(
        stderr.contains("already exists"),
        "unexpected stderr: {stderr}"
    );

    loop_ok(
        &env,
        &["loop", "add", "state", "--check", "true", "--at", "07:00"],
    );

    let (_stdout, stderr) = loop_fail(&env, &["loop", "rename", "old", "state"]);
    assert!(
        stderr.contains("already exists"),
        "unexpected stderr: {stderr}"
    );

    let (_stdout, stderr) = loop_fail(&env, &["loop", "rename", "state", "existing"]);
    assert!(
        stderr.contains("already exists"),
        "unexpected stderr: {stderr}"
    );

    let (_stdout, stderr) = loop_fail(&env, &["loop", "rename", "old", "old"]);
    assert!(
        stderr.contains("must differ"),
        "unexpected stderr: {stderr}"
    );

    let stdout = loop_ok(&env, &["loop", "rename", "missing", "free"]);
    assert!(
        stdout.contains("no loop task named `missing`"),
        "unexpected stdout: {stdout}"
    );
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
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
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
            std::time::Instant::now() < deadline,
            "timed out waiting for loop run lock {}",
            path.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
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
