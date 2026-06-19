//! `rimz sidebar snapshot` integration tests for provider dashboard facts.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use rimz::agents::AgentLifecycleObservation;
use rimz::agents::lifecycle::LifecycleSignal;
use rimz::schema::event::EventEnvelope;

use crate::common::Env;

fn inject_lifecycle(env: &Env, agent_kind: &str, agent_id: &str) {
    let obs = AgentLifecycleObservation {
        agent_id: Some(agent_id.into()),
        agent_name: None,
        agent_profile: None,
        kind_ordinal: None,
        signal: LifecycleSignal::Registered,
        agent_pid: None,
        agent_process_start: None,
        runtime_owner: None,
        worktree_path: Some(env.project_root.display().to_string()),
        worktree_branch: Some("main".to_owned()),
        task: None,
        prompt: None,
        transcript_path: None,
        model: None,
        effort: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        turn_error: None,
        cache_read_input_tokens: None,
        cache_write_input_tokens: None,
        fresh_input_tokens: None,
        output_tokens: None,
        todo_done: None,
        todo_total: None,
        pane_id: None,
        parent_agent_id: None,
    };
    let envelope = EventEnvelope::agent_lifecycle(
        env.workspace_id.clone(),
        "test-session",
        agent_kind,
        "SessionStart",
        &obs,
    );
    env.ledger().append_event(&envelope).expect("append");
}

fn write_claude_settings(env: &Env, text: &str) -> PathBuf {
    let path = env.home_root.join("claude-settings.json");
    std::fs::write(&path, text).expect("write claude settings");
    path
}

fn stub_claude_version(env: &Env, version: &str) -> PathBuf {
    let dir = env.home_root.join("bin");
    std::fs::create_dir_all(&dir).expect("mkdir bin");
    let path = dir.join("claude");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo '{version}'; exit 0; fi\nexit 1\n"
        ),
    )
    .expect("write claude stub");
    let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod claude stub");
    dir
}

fn path_with_stub_first(stub_dir: &Path) -> String {
    let current = std::env::var_os("PATH").unwrap_or_default();
    format!("{}:{}", stub_dir.display(), current.to_string_lossy())
}

fn claude_remote_control_value(
    env: &Env,
    settings: &Path,
    path: Option<String>,
) -> serde_json::Value {
    inject_lifecycle(env, "claude", "claude-session-rc");

    let mut command = env.rimz();
    command
        .args([
            "sidebar",
            "snapshot",
            "--workspace-id",
            env.workspace_id.as_str(),
            "--json",
        ])
        .env("RIMZ_CLAUDE_SETTINGS", settings)
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN");
    if let Some(path) = path {
        command.env("PATH", path);
    }
    let output = command.output().expect("spawn sidebar snapshot");
    assert!(
        output.status.success(),
        "snapshot failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("snapshot json");
    json["providers"]
        .as_array()
        .expect("providers")
        .iter()
        .find(|provider| provider["kind"] == "claude")
        .expect("claude provider panel")["remote_control"]
        .clone()
}

#[test]
fn sidebar_lights_claude_rc_badge_from_claude_startup_setting() {
    let env = Env::new();
    let settings = write_claude_settings(&env, r#"{ "remoteControlAtStartup": true }"#);

    assert_eq!(claude_remote_control_value(&env, &settings, None), true);
}

#[test]
fn sidebar_suppresses_claude_rc_badge_when_auth_blocks_remote_control() {
    let env = Env::new();
    let settings = write_claude_settings(
        &env,
        r#"{ "remoteControlAtStartup": true, "apiKeyHelper": "op read key" }"#,
    );
    let stub_dir = stub_claude_version(&env, "2.1.173 (Claude Code)");

    assert_eq!(
        claude_remote_control_value(&env, &settings, Some(path_with_stub_first(&stub_dir))),
        false
    );
}
