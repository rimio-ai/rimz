//! Integration coverage for `rimz loop` instance-bound delivery.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use jiff::{SignedDuration, Timestamp};
use serde_json::json;

use rimz::config::{CheckOn, LoopConfig, TaskEntry, TaskTarget, Tasks};
use rimz::harness::run::{PermissionMode, RunRecord, RunStatus};
use rimz::harness::schedule::instances;
use rimz::harness::schedule::pauses::{self, PauseEntry};
use rimz::harness::schedule::run_log::{self, LoopRunRecord, LoopRunResult};
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::message::MessageStatus;

use crate::common::{Env, ScrubSessionEnvExt};

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
fn loop_project_tasks_list_and_refuse_until_trusted() {
    let env = Env::new();
    write_project_config(
        &env,
        "[tasks.repo-check]\ncheck = \"true\"\nevery = \"15m\"\n",
    );

    let stdout = loop_ok(&env, &["loop", "list"]);
    assert!(
        stdout.contains("repo-check") && stdout.contains("project · untrusted"),
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

    let (_stdout, stderr) = loop_fail(&env, &["loop", "run", "repo-check"]);
    assert!(
        stderr.contains("project is untrusted") && stderr.contains("rimz trust grant"),
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

    loop_ok(&env, &["loop", "run", "later"]);
    assert!(
        !read_loop_instances(&env).0.contains_key("later"),
        "fired one-shot should be removed from state"
    );

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
        let stdout = loop_ok(&env, &["loop", "fire", "probe"]);
        assert!(
            stdout.contains("loop `probe`: completed (exit 0)"),
            "check-only manual fire should keep the check exit label: {stdout}"
        );
        assert!(
            read_loop_instances(&env).0.contains_key("probe"),
            "manual fire should keep the one-shot instance"
        );
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
        Some(&PauseEntry { until: None })
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
        stdout.contains("loop `probe`: task is paused; firing anyway")
            && stdout.contains("loop `probe`: completed (exit 0)"),
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
                && line.contains("paused")
        }),
        "untrusted project task should stay visible and paused: {stdout}"
    );
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
        fire_stdout.contains("loop `missing`: failed (exit 127"),
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
            && fire_stdout.contains("on=fail")
            && fire_stdout.contains("not woken"),
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
    loop_ok(&env, &["loop", "run", "broken"]);
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

    loop_fail(&env, &["loop", "run", "bad_prompt"]);
    let records = read_loop_run_records(&env);
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

#[test]
fn loop_show_displays_shadowed_error_and_run_tail() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.forensics]\n\
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
    assert!(stdout.contains("LAST FAILURE — ✗ error"), "{stdout}");
    assert!(
        stdout.contains(
            "  error:\n  │ reading system-prompt-file `/missing.md`\n  │ caused by: not found"
        ),
        "{stdout}"
    );
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
    loop_ok(&env, &["loop", "run", "green"]);
    assert_eq!(
        env.store().list_pending_messages().expect("messages").len(),
        1
    );
    assert!(
        !read_loop_instances(&env).0.contains_key("green"),
        "poll-until should be removed after firing"
    );

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
    loop_ok(&expired, &["loop", "run", "expired"]);
    assert!(read_loop_instances(&expired).0.is_empty());
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

    let stdout = loop_ok(&env, &["loop", "fire", "dead"]);
    assert!(
        stdout.contains("not alive; leaving schedule in place"),
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
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .expect("open lock");
    lock_file.try_lock().expect("hold loop run lock");

    let stdout = loop_ok(&env, &["loop", "run", "busy"]);
    assert!(
        stdout.contains("previous run still active; skipping"),
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

    let stdout = loop_ok(&env, &["loop", "show", "busy"]);
    assert!(
        stdout.contains("overlapped"),
        "show should display overlapped record: {stdout}"
    );
    lock_file.unlock().expect("unlock loop run lock");
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
    let path = instances::path(&env.state_root());
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

fn write_loop_instances(env: &Env, tasks: Tasks) {
    let path = instances::path(&env.state_root());
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
