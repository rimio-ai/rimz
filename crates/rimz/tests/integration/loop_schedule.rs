//! Integration coverage for `rimz loop` instance-bound delivery.

use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::json;

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
    let config = std::fs::read_to_string(agents_config_path(&env)).expect("read agents config");
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
        stdout.contains("RUNS") && stdout.contains("LAST RUN") && stdout.contains("RESULT"),
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
    write_agents_config(
        &env,
        &format!(
            "[agents.loop.tasks.dead]\n\
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
    let config = std::fs::read_to_string(agents_config_path(&env)).expect("read agents config");
    assert!(
        !config.contains("[agents.loop.tasks.dead]"),
        "dead schedule should be removed: {config}"
    );
}

#[test]
fn loop_run_bind_tilde_root_queues_prompt() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-tilde", "feature-loop");
    write_agents_config(
        &env,
        "[agents.loop.tasks.tilde]\n\
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
        String::from_utf8_lossy(&missing.stderr).contains("needs --spec or --bind"),
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

fn write_agents_config(env: &Env, text: &str) {
    let path = agents_config_path(env);
    std::fs::create_dir_all(path.parent().expect("config dir")).expect("mkdir config");
    std::fs::write(path, text).expect("write agents config");
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

fn agents_config_path(env: &Env) -> std::path::PathBuf {
    env.config_root().join("rimz").join("agents.toml")
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
