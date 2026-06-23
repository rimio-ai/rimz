//! Integration coverage for `rimz loop` instance-bound delivery.

use serde_json::json;

use rimz::message::MessageStatus;

use crate::common::Env;

#[test]
fn loop_add_to_pins_live_session_and_run_queues_prompt() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-live", "feature-loop");

    let add = env
        .rimz()
        .args([
            "loop",
            "add",
            "wake",
            "--to",
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
    assert_eq!(messages[0].status, MessageStatus::Pending);
}

#[test]
fn loop_run_to_dead_session_reaps_schedule() {
    let env = Env::new();
    write_agents_config(
        &env,
        &format!(
            "[agents.loop.tasks.dead]\n\
             to = {{ kind = \"claude\", session = \"sess-dead\", handle = \"@claude\" }}\n\
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
fn loop_add_to_validates_mode_selection() {
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
        String::from_utf8_lossy(&missing.stderr).contains("needs --spec or --to"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&missing.stderr)
    );

    let both = env
        .rimz()
        .args([
            "loop", "add", "bad", "--spec", "claude", "--to", "@claude", "--every", "15m",
            "--prompt", "x",
        ])
        .output()
        .expect("loop add both modes");
    assert!(!both.status.success(), "clap should reject both modes");

    let spawn_only = env
        .rimz()
        .args([
            "loop", "add", "bad", "--to", "@claude", "--mode", "auto", "--every", "15m",
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
    run_hook(
        env,
        json!({
            "hook_event_name": "SessionStart",
            "session_id": session_id,
            "worktree_branch": branch,
        }),
    );
    run_hook(
        env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": session_id,
            "prompt": "work",
            "worktree_branch": branch,
        }),
    );
}

fn run_hook(env: &Env, payload: serde_json::Value) {
    let payload = serde_json::to_string(&payload).expect("payload");
    let output = env.run_installed_hook_in_pane("claude", &payload, &[]);
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

fn agents_config_path(env: &Env) -> std::path::PathBuf {
    env.config_root().join("rimz").join("agents.toml")
}
