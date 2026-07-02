//! Integration coverage for `rimz loop` instance-bound delivery.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};

use jiff::{SignedDuration, Timestamp};
use serde_json::json;

use rimz::config::{CheckOn, TaskEntry, TaskTarget, Tasks};
use rimz::harness::schedule::instances;
use rimz::harness::schedule::run_log::{self, LoopRunRecord, LoopRunResult};
use rimz::message::MessageStatus;

use crate::common::Env;

#[test]
fn loop_add_bind_pins_live_session_and_run_queues_prompt() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-live", "feature-loop");

    let add = env
        .rimz()
        .args([
            "loop",
            "add",
            "wake",
            "--bind",
            "@claude",
            "--every",
            "15m",
            "--prompt",
            "next step",
        ])
        .output()
        .expect("loop add");
    assert!(
        add.status.success(),
        "loop add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    let config = std::fs::read_to_string(loop_config_path(&env)).expect("read loop config");
    assert!(
        config.contains("session = \"sess-loop-live\""),
        "task should pin the live session id: {config}"
    );

    let run = env
        .rimz()
        .args(["loop", "run", "wake"])
        .output()
        .expect("loop run");
    assert!(
        run.status.success(),
        "loop run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let messages = env.ledger().list_pending_messages().expect("messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text, "next step");
    assert_eq!(messages[0].kind.as_str(), "claude");
    assert_eq!(messages[0].agent_id.as_str(), "sess-loop-live");
    assert_eq!(messages[0].status, MessageStatus::Queued);

    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].task, "wake");
    assert_eq!(records[0].result, LoopRunResult::Delivered);

    let list = env
        .rimz()
        .args(["loop", "list"])
        .output()
        .expect("loop list");
    assert!(
        list.status.success(),
        "loop list failed: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        stdout.contains("SOURCE")
            && stdout.contains("RUNS")
            && stdout.contains("LAST RUN")
            && stdout.contains("RESULT"),
        "loop list should show run-history columns: {stdout}"
    );
    assert!(
        stdout.lines().any(|line| {
            line.starts_with("wake") && line.contains("  1  ") && line.contains("delivered")
        }),
        "loop list should fold run history for wake: {stdout}"
    );
}

#[test]
fn loop_add_ephemeral_tasks_use_instance_state() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-state", "feature-loop");

    let add_later = env
        .rimz()
        .args([
            "loop",
            "add",
            "later",
            "--bind",
            "@claude",
            "--in",
            "5m",
            "--prompt",
            "later wake",
        ])
        .output()
        .expect("loop add --in");
    assert!(
        add_later.status.success(),
        "loop add --in failed: {}",
        String::from_utf8_lossy(&add_later.stderr)
    );
    assert!(
        read_loop_instances(&env).0.contains_key("later"),
        "--in should persist as state"
    );
    assert!(
        !loop_config_path(&env).exists(),
        "--in should not create loop.toml"
    );

    let run_later = env
        .rimz()
        .args(["loop", "run", "later"])
        .output()
        .expect("loop run later");
    assert!(
        run_later.status.success(),
        "loop run failed: {}",
        String::from_utf8_lossy(&run_later.stderr)
    );
    assert!(
        !read_loop_instances(&env).0.contains_key("later"),
        "fired one-shot should be removed from state"
    );

    let add_daily = env
        .rimz()
        .args([
            "loop",
            "add",
            "daily",
            "--bind",
            "@claude",
            "--at",
            "07:00",
            "--prompt",
            "daily wake",
        ])
        .output()
        .expect("loop add daily");
    assert!(
        add_daily.status.success(),
        "loop add daily failed: {}",
        String::from_utf8_lossy(&add_daily.stderr)
    );
    let loop_text = std::fs::read_to_string(loop_config_path(&env)).expect("read loop config");
    assert!(
        loop_text.contains("[tasks.daily]"),
        "recurring task should persist in loop.toml: {loop_text}"
    );
    assert!(
        !read_loop_instances(&env).0.contains_key("daily"),
        "recurring task should not persist as state"
    );
}

#[test]
fn loop_fire_keeps_ephemeral_task() {
    let env = Env::new();

    let add = env
        .rimz()
        .args([
            "loop",
            "add",
            "probe",
            "--check",
            "printf ok",
            "--at",
            "07:00",
            "--once",
        ])
        .output()
        .expect("loop add check-only one-shot");
    assert!(
        add.status.success(),
        "loop add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    assert!(
        read_loop_instances(&env).0.contains_key("probe"),
        "one-shot should persist as state"
    );

    for _ in 0..2 {
        let fire = env
            .rimz()
            .args(["loop", "fire", "probe"])
            .output()
            .expect("loop fire");
        assert!(
            fire.status.success(),
            "loop fire failed: {}",
            String::from_utf8_lossy(&fire.stderr)
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
fn loop_add_replaces_same_name_across_config_and_state() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-replace", "feature-loop");

    let add_durable = env
        .rimz()
        .args([
            "loop",
            "add",
            "swap",
            "--bind",
            "@claude",
            "--every",
            "15m",
            "--prompt",
            "durable wake",
        ])
        .output()
        .expect("loop add durable");
    assert!(
        add_durable.status.success(),
        "loop add durable failed: {}",
        String::from_utf8_lossy(&add_durable.stderr)
    );
    assert!(
        std::fs::read_to_string(loop_config_path(&env))
            .expect("read loop config")
            .contains("[tasks.swap]"),
        "durable task should persist in loop.toml"
    );

    let add_ephemeral = env
        .rimz()
        .args([
            "loop", "add", "swap", "--bind", "@claude", "--in", "5m", "--prompt", "one shot",
        ])
        .output()
        .expect("loop add ephemeral");
    assert!(
        add_ephemeral.status.success(),
        "loop add ephemeral failed: {}",
        String::from_utf8_lossy(&add_ephemeral.stderr)
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

    let add_durable_again = env
        .rimz()
        .args([
            "loop",
            "add",
            "swap",
            "--bind",
            "@claude",
            "--every",
            "30m",
            "--prompt",
            "durable again",
        ])
        .output()
        .expect("loop add durable again");
    assert!(
        add_durable_again.status.success(),
        "loop add durable again failed: {}",
        String::from_utf8_lossy(&add_durable_again.stderr)
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
        let add = env
            .rimz()
            .args(["loop", "add", name, "--check", command, "--every", "15m"])
            .output()
            .expect("loop add check-only");
        assert!(
            add.status.success(),
            "loop add failed: {}",
            String::from_utf8_lossy(&add.stderr)
        );
        let run = env
            .rimz()
            .args(["loop", "run", name])
            .output()
            .expect("loop run check-only");
        assert!(
            run.status.success(),
            "loop run failed: {}",
            String::from_utf8_lossy(&run.stderr)
        );
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
    let add = env
        .rimz()
        .args([
            "loop", "add", "missing", "--check", command, "--every", "15m",
        ])
        .output()
        .expect("loop add check-only");
    assert!(
        add.status.success(),
        "loop add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );

    let fire = env
        .rimz()
        .args(["loop", "fire", "missing"])
        .output()
        .expect("loop fire check-only");
    assert!(
        fire.status.success(),
        "loop fire failed: {}",
        String::from_utf8_lossy(&fire.stderr)
    );
    let fire_stdout = String::from_utf8_lossy(&fire.stdout);
    assert!(
        fire_stdout.contains("loop `missing`: failed (exit 127"),
        "fire should print outcome summary: {fire_stdout}"
    );
    assert!(
        fire_stdout.contains(command),
        "fire should print check output tail: {fire_stdout}"
    );

    let records = read_loop_run_records(&env);
    let record = records.last().expect("check record");
    assert_eq!(record.result, LoopRunResult::Failed);
    let check = record.check.as_ref().expect("check detail");
    assert_eq!(check.code, Some(127));
    assert!(check.output.contains(command));

    let show = env
        .rimz()
        .args(["loop", "show", "missing"])
        .output()
        .expect("loop show");
    assert!(
        show.status.success(),
        "loop show failed: {}",
        String::from_utf8_lossy(&show.stderr)
    );
    let stdout = String::from_utf8_lossy(&show.stdout);
    assert!(
        stdout.contains("RESULT") && stdout.contains("failed"),
        "show should print runs table: {stdout}"
    );
    assert!(
        stdout.contains("last run output (exit 127)") && stdout.contains(command),
        "show should print output detail: {stdout}"
    );
}

#[test]
fn loop_run_check_guard_skips_or_delivers_with_output() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-check", "feature-loop");

    let add_healthy = env
        .rimz()
        .args([
            "loop", "add", "healthy", "--bind", "@claude", "--every", "15m", "--check", "true",
            "--on", "fail", "--prompt", "fix it",
        ])
        .output()
        .expect("loop add healthy");
    assert!(
        add_healthy.status.success(),
        "loop add healthy failed: {}",
        String::from_utf8_lossy(&add_healthy.stderr)
    );
    let run_healthy = env
        .rimz()
        .args(["loop", "run", "healthy"])
        .output()
        .expect("loop run healthy");
    assert!(
        run_healthy.status.success(),
        "loop run healthy failed: {}",
        String::from_utf8_lossy(&run_healthy.stderr)
    );
    assert!(
        env.ledger()
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

    let add_broken = env
        .rimz()
        .args([
            "loop",
            "add",
            "broken",
            "--bind",
            "@claude",
            "--every",
            "15m",
            "--check",
            "printf boom; exit 1",
            "--prompt",
            "fix it",
        ])
        .output()
        .expect("loop add broken");
    assert!(
        add_broken.status.success(),
        "loop add broken failed: {}",
        String::from_utf8_lossy(&add_broken.stderr)
    );
    let run_broken = env
        .rimz()
        .args(["loop", "run", "broken"])
        .output()
        .expect("loop run broken");
    assert!(
        run_broken.status.success(),
        "loop run broken failed: {}",
        String::from_utf8_lossy(&run_broken.stderr)
    );
    let messages = env.ledger().list_pending_messages().expect("messages");
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
             bind = {{ kind = \"claude\", session = \"sess-loop-error\", handle = \"@claude\" }}\n\
             prompt-file = \"missing-prompt.txt\"\n\
             root = \"{}\"\n\
             every = \"15m\"\n",
            env.project_root.display()
        ),
    );

    let run = env
        .rimz()
        .args(["loop", "run", "bad_prompt"])
        .output()
        .expect("loop run bad prompt");
    assert!(
        !run.status.success(),
        "loop run should fail for missing prompt-file"
    );
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

    let show = env
        .rimz()
        .args(["loop", "show", "bad_prompt"])
        .output()
        .expect("loop show bad prompt");
    assert!(
        show.status.success(),
        "loop show failed: {}",
        String::from_utf8_lossy(&show.stderr)
    );
    let stdout = String::from_utf8_lossy(&show.stdout);
    assert!(
        stdout.contains("error") && stdout.contains("reading prompt-file"),
        "show should display stored error: {stdout}"
    );
}

#[test]
fn loop_run_poll_until_fires_once_and_expires() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-until", "feature-loop");

    let add = env
        .rimz()
        .args([
            "loop",
            "add",
            "green",
            "--bind",
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
        ])
        .output()
        .expect("loop add poll-until");
    assert!(
        add.status.success(),
        "loop add poll-until failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    assert!(
        read_loop_instances(&env).0.contains_key("green"),
        "poll-until should persist as state"
    );
    let run = env
        .rimz()
        .args(["loop", "run", "green"])
        .output()
        .expect("loop run green");
    assert!(
        run.status.success(),
        "loop run green failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        env.ledger()
            .list_pending_messages()
            .expect("messages")
            .len(),
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
                bind: Some(TaskTarget {
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
    let run = expired
        .rimz()
        .args(["loop", "run", "expired"])
        .output()
        .expect("loop run expired");
    assert!(
        run.status.success(),
        "loop run expired failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(read_loop_instances(&expired).0.is_empty());
    assert_eq!(
        read_loop_run_records(&expired)
            .last()
            .map(|record| record.result),
        Some(LoopRunResult::Expired)
    );
    assert!(
        expired
            .ledger()
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
            "--bind",
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

    let run = env
        .rimz()
        .args(["loop", "run", "wake-worktree"])
        .output()
        .expect("loop run");
    assert!(
        run.status.success(),
        "loop run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let messages = env.ledger().list_pending_messages().expect("messages");
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
             bind = {{ kind = \"claude\", session = \"sess-dead\", handle = \"@claude\" }}\n\
             prompt = \"wake up\"\n\
             root = \"{}\"\n\
             at = \"07:00\"\n",
            env.project_root.display()
        ),
    );

    let run = env
        .rimz()
        .args(["loop", "run", "dead"])
        .output()
        .expect("loop run");
    assert!(
        run.status.success(),
        "loop run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("not alive; removing schedule"),
        "dead target should be reported: {}",
        String::from_utf8_lossy(&run.stdout)
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
             bind = {{ kind = \"claude\", session = \"sess-dead\", handle = \"@claude\" }}\n\
             prompt = \"wake up\"\n\
             root = \"{}\"\n\
             at = \"07:00\"\n",
            env.project_root.display()
        ),
    );

    let fire = env
        .rimz()
        .args(["loop", "fire", "dead"])
        .output()
        .expect("loop fire");
    assert!(
        fire.status.success(),
        "loop fire failed: {}",
        String::from_utf8_lossy(&fire.stderr)
    );
    assert!(
        String::from_utf8_lossy(&fire.stdout).contains("not alive; leaving schedule in place"),
        "dead target should be reported: {}",
        String::from_utf8_lossy(&fire.stdout)
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

    let add = env
        .rimz()
        .args([
            "loop", "add", "manual", "--bind", "@claude", "--every", "15m", "--prompt", "fire now",
        ])
        .output()
        .expect("loop add");
    assert!(
        add.status.success(),
        "loop add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );

    let fire = env
        .rimz()
        .args(["loop", "fire", "manual"])
        .output()
        .expect("loop fire");
    assert!(
        fire.status.success(),
        "loop fire failed: {}",
        String::from_utf8_lossy(&fire.stderr)
    );

    let messages = env.ledger().list_pending_messages().expect("messages");
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

    let list = env
        .rimz()
        .args(["loop", "list"])
        .output()
        .expect("loop list no stamp");
    assert!(
        list.status.success(),
        "loop list failed: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        stdout.contains("NEXT"),
        "list should include NEXT: {stdout}"
    );
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("next") && line.contains("every 15m  -")),
        "no arm stamp should render dash: {stdout}"
    );

    write_loop_fire_state(
        &env,
        BTreeMap::from([(
            "next".to_owned(),
            Timestamp::now() - SignedDuration::from_secs(16 * 60),
        )]),
    );
    let list = env
        .rimz()
        .args(["loop", "list"])
        .output()
        .expect("loop list stamped");
    assert!(
        list.status.success(),
        "loop list failed: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("next") && line.contains("due")),
        "due arm stamp should render due: {stdout}"
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

    let list = env
        .rimz()
        .args(["loop", "list"])
        .output()
        .expect("loop list legacy");
    assert!(
        list.status.success(),
        "loop list failed: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("legacy") && line.contains("completed")),
        "list should fold legacy record: {stdout}"
    );

    let show = env
        .rimz()
        .args(["loop", "show", "legacy"])
        .output()
        .expect("loop show legacy");
    assert!(
        show.status.success(),
        "loop show failed: {}",
        String::from_utf8_lossy(&show.stderr)
    );
    let stdout = String::from_utf8_lossy(&show.stdout);
    assert!(
        stdout.contains("completed") && stdout.contains("MODE"),
        "show should render legacy record with defaulted fields: {stdout}"
    );
}

#[test]
fn loop_run_overlapped_records_skip_and_keeps_task_state() {
    let env = Env::new();
    let add = env
        .rimz()
        .args([
            "loop", "add", "busy", "--check", "true", "--at", "07:00", "--once",
        ])
        .output()
        .expect("loop add busy");
    assert!(
        add.status.success(),
        "loop add failed: {}",
        String::from_utf8_lossy(&add.stderr)
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

    let run = env
        .rimz()
        .args(["loop", "run", "busy"])
        .output()
        .expect("loop run busy");
    assert!(
        run.status.success(),
        "loop run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
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

    let show = env
        .rimz()
        .args(["loop", "show", "busy"])
        .output()
        .expect("loop show busy");
    assert!(
        show.status.success(),
        "loop show failed: {}",
        String::from_utf8_lossy(&show.stderr)
    );
    let stdout = String::from_utf8_lossy(&show.stdout);
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
         bind = { kind = \"claude\", session = \"sess-loop-tilde\", handle = \"@claude\" }\n\
         prompt = \"tilde wake\"\n\
         root = \"~/project\"\n\
         every = \"15m\"\n",
    );

    let run = env
        .rimz()
        .args(["loop", "run", "tilde"])
        .output()
        .expect("loop run");
    assert!(
        run.status.success(),
        "loop run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let messages = env.ledger().list_pending_messages().expect("messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text, "tilde wake");
    assert_eq!(messages[0].agent_id.as_str(), "sess-loop-tilde");
}

#[test]
fn loop_add_bind_validates_mode_selection() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-validate", "feature-loop");

    let missing = env
        .rimz()
        .args(["loop", "add", "bad", "--every", "15m", "--prompt", "x"])
        .output()
        .expect("loop add missing mode");
    assert!(!missing.status.success(), "missing mode should fail");
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("needs --spec, --bind, or --check"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&missing.stderr)
    );

    let both = env
        .rimz()
        .args([
            "loop", "add", "bad", "--spec", "claude", "--bind", "@claude", "--every", "15m",
            "--prompt", "x",
        ])
        .output()
        .expect("loop add both modes");
    assert!(!both.status.success(), "clap should reject both modes");

    let spawn_only = env
        .rimz()
        .args([
            "loop", "add", "bad", "--bind", "@claude", "--mode", "auto", "--every", "15m",
            "--prompt", "x",
        ])
        .output()
        .expect("loop add spawn-only flag");
    assert!(!spawn_only.status.success(), "spawn-only flag should fail");
    assert!(
        String::from_utf8_lossy(&spawn_only.stderr).contains("only apply to --spec tasks"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&spawn_only.stderr)
    );
}

#[test]
fn loop_rename_moves_config_entry() {
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

    let rename = env
        .rimz()
        .args(["loop", "rename", "old", "new"])
        .output()
        .expect("loop rename");
    assert!(
        rename.status.success(),
        "loop rename failed: {}",
        String::from_utf8_lossy(&rename.stderr)
    );
    assert!(
        String::from_utf8_lossy(&rename.stdout).contains("renamed loop task `old` to `new`"),
        "unexpected stdout: {}",
        String::from_utf8_lossy(&rename.stdout)
    );

    let config = std::fs::read_to_string(loop_config_path(&env)).expect("read loop config");
    assert!(config.contains("# keep this task comment"));
    assert!(config.contains("[tasks.new]"), "new task missing: {config}");
    assert!(
        !config.contains("[tasks.old]"),
        "old task should be gone: {config}"
    );
}

#[test]
fn loop_rename_moves_instance_entry() {
    let env = Env::new();

    let add = env
        .rimz()
        .args([
            "loop",
            "add",
            "old-state",
            "--check",
            "true",
            "--at",
            "07:00",
            "--once",
        ])
        .output()
        .expect("loop add instance");
    assert!(
        add.status.success(),
        "loop add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );

    let rename = env
        .rimz()
        .args(["loop", "rename", "old-state", "new-state"])
        .output()
        .expect("loop rename");
    assert!(
        rename.status.success(),
        "loop rename failed: {}",
        String::from_utf8_lossy(&rename.stderr)
    );

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

    let collision = env
        .rimz()
        .args(["loop", "rename", "old", "existing"])
        .output()
        .expect("loop rename collision");
    assert!(!collision.status.success(), "collision should fail");
    assert!(
        String::from_utf8_lossy(&collision.stderr).contains("already exists"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&collision.stderr)
    );

    let add_instance = env
        .rimz()
        .args([
            "loop", "add", "state", "--check", "true", "--at", "07:00", "--once",
        ])
        .output()
        .expect("loop add instance");
    assert!(
        add_instance.status.success(),
        "loop add failed: {}",
        String::from_utf8_lossy(&add_instance.stderr)
    );

    let config_to_instance = env
        .rimz()
        .args(["loop", "rename", "old", "state"])
        .output()
        .expect("loop rename config to instance");
    assert!(
        !config_to_instance.status.success(),
        "config-to-instance collision should fail"
    );
    assert!(
        String::from_utf8_lossy(&config_to_instance.stderr).contains("already exists"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&config_to_instance.stderr)
    );

    let instance_to_config = env
        .rimz()
        .args(["loop", "rename", "state", "existing"])
        .output()
        .expect("loop rename instance to config");
    assert!(
        !instance_to_config.status.success(),
        "instance-to-config collision should fail"
    );
    assert!(
        String::from_utf8_lossy(&instance_to_config.stderr).contains("already exists"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&instance_to_config.stderr)
    );

    let same = env
        .rimz()
        .args(["loop", "rename", "old", "old"])
        .output()
        .expect("loop rename same name");
    assert!(!same.status.success(), "same-name rename should fail");
    assert!(
        String::from_utf8_lossy(&same.stderr).contains("must differ"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&same.stderr)
    );

    let missing = env
        .rimz()
        .args(["loop", "rename", "missing", "free"])
        .output()
        .expect("loop rename missing");
    assert!(
        missing.status.success(),
        "missing rename failed: {}",
        String::from_utf8_lossy(&missing.stderr)
    );
    assert!(
        String::from_utf8_lossy(&missing.stdout).contains("no loop task named `missing`"),
        "unexpected stdout: {}",
        String::from_utf8_lossy(&missing.stdout)
    );
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
    let mut cmd = env.rimz();
    cmd.current_dir(cwd)
        .args(["hooks", "feed", "--source", "claude"])
        .env("RIMZ_AGENT_PID", std::process::id().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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

fn write_loop_config(env: &Env, text: &str) {
    let path = loop_config_path(env);
    std::fs::create_dir_all(path.parent().expect("config dir")).expect("mkdir config");
    std::fs::write(path, text).expect("write loop config");
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

fn read_loop_instances(env: &Env) -> Tasks {
    let path = instances::path(&env.state_root());
    let Ok(text) = std::fs::read_to_string(path) else {
        return Tasks::default();
    };
    serde_json::from_str(&text).expect("loop instances")
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
