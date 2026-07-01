//! Integration coverage for `rimz loop` instance-bound delivery.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};

use jiff::Timestamp;
use serde_json::json;

use rimz::config::{CheckOn, TaskEntry, TaskTarget, Tasks};
use rimz::loop_instances;
use rimz::loop_run_log::{self, LoopRunRecord, LoopRunResult};
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
    }
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
    let path = loop_run_log::log_path(&env.state_root());
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .map(|line| serde_json::from_str(line).expect("loop run record"))
        .collect()
}

fn read_loop_instances(env: &Env) -> Tasks {
    let path = loop_instances::path(&env.state_root());
    let Ok(text) = std::fs::read_to_string(path) else {
        return Tasks::default();
    };
    serde_json::from_str(&text).expect("loop instances")
}

fn write_loop_instances(env: &Env, tasks: Tasks) {
    let path = loop_instances::path(&env.state_root());
    std::fs::create_dir_all(path.parent().expect("instances parent")).expect("mkdir state");
    std::fs::write(path, serde_json::to_vec_pretty(&tasks).expect("json"))
        .expect("write loop instances");
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
