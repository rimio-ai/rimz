//! `rimz doctor` agent rollup integration tests. Inject an `agent.lifecycle`
//! event directly into the ledger, then run the binary and assert the rendered
//! per-agent rows.

use rimz::agents::AgentLifecycleObservation;
use rimz::agents::lifecycle::LifecycleSignal;
use rimz::ids::{MuxName, ResolverId, SidebarInstanceId};
use rimz::schema::event::EventEnvelope;
use rimz::schema::heartbeat::{ResolverHeartbeat, SidebarHeartbeat};

use crate::common::Env;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn inject_lifecycle(
    env: &Env,
    agent_kind: &str,
    agent_id: &str,
    signal: LifecycleSignal,
    branch: Option<&str>,
) {
    let obs = AgentLifecycleObservation {
        agent_id: Some(agent_id.into()),
        agent_name: None,
        kind_ordinal: None,
        signal,
        agent_pid: None,
        agent_process_start: None,
        runtime_owner: None,
        worktree_path: Some(env.project_root.display().to_string()),
        worktree_branch: branch.map(ToOwned::to_owned),
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

#[test]
fn doctor_renders_status_row_per_agent() {
    let env = Env::new();
    inject_lifecycle(
        &env,
        "claude",
        "claude-session-abc",
        LifecycleSignal::Registered,
        Some("main"),
    );
    inject_lifecycle(
        &env,
        "claude",
        "claude-session-abc",
        LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
        },
        Some("main"),
    );
    inject_lifecycle(
        &env,
        "codex",
        "codex-session-xyz",
        LifecycleSignal::TurnStarted,
        Some("feature-migration"),
    );

    let output = env
        .rimz()
        .args(["doctor", "--audit"])
        .output()
        .expect("spawn doctor");
    assert!(
        output.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");

    // Group headers — one per kind, sorted lexically.
    assert!(
        stdout.contains("agent (claude)"),
        "missing claude group header in:\n{stdout}"
    );
    assert!(
        stdout.contains("agent (codex)"),
        "missing codex group header in:\n{stdout}"
    );

    // Per-agent row: agent id + worktree + status.
    assert_eq!(
        stdout.matches("claude-session-abc").count(),
        1,
        "the rollup folds both claude events into one row, got:\n{stdout}"
    );
    assert!(stdout.contains("claude-session-abc"));
    assert!(stdout.contains("main"));
    assert!(stdout.contains("failed"));

    assert!(stdout.contains("codex-session-xyz"));
    assert!(stdout.contains("feature-migration"));
    assert!(stdout.contains("running"));
}

#[test]
fn doctor_reports_agent_hook_install_and_trust_states() {
    let env = Env::new();
    let output = env.rimz().arg("doctor").output().expect("spawn doctor");
    assert!(
        output.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");

    assert!(
        stdout.contains("agent hooks"),
        "doctor must report agent hook install status:\n{stdout}"
    );
    assert!(
        stdout.contains("not installed"),
        "an un-onboarded machine must read 'not installed':\n{stdout}"
    );
    assert!(
        stdout.contains("rimz hooks install claude") && stdout.contains("rimz hooks install codex"),
        "doctor must name the wiring command for each missing agent:\n{stdout}"
    );

    let env = Env::new();
    env.install_agent_hooks("codex");
    let output = env.rimz().arg("doctor").output().expect("spawn doctor");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8");

    assert!(
        stdout.contains("rimz hooks install claude"),
        "claude is still unwired, so its install hint stays:\n{stdout}"
    );
    assert!(
        stdout.contains("codex installed, untrusted"),
        "freshly installed hooks are untrusted until /hooks:\n{stdout}"
    );
    assert!(
        stdout.contains("run /hooks inside codex and trust the Rimz hooks"),
        "doctor names the fix:\n{stdout}"
    );

    let env = Env::new();
    env.install_agent_hooks("codex");
    trust_codex_hooks(&env);

    let output = env.rimz().arg("doctor").output().expect("spawn doctor");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8");

    assert!(
        stdout.contains("codex installed") && !stdout.contains("untrusted"),
        "trusted hooks read plain installed:\n{stdout}"
    );
    assert!(
        !stdout.contains("hooks trust"),
        "no trust-fix line once trusted:\n{stdout}"
    );
}

/// Append `[hooks.state]` trust entries for every Rimz-installed codex event,
/// key-shaped exactly as Codex writes them after the user trusts via /hooks.
fn trust_codex_hooks(env: &Env) {
    let config = env.agent_config_path("codex");
    let mut text = std::fs::read_to_string(&config).expect("read codex config");
    for token in [
        "session_start",
        "user_prompt_submit",
        "subagent_start",
        "subagent_stop",
        "stop",
        "permission_request",
        "pre_tool_use",
        "post_tool_use",
        "pre_compact",
        "post_compact",
    ] {
        text.push_str(&format!(
            "\n[hooks.state.\"{}:{token}:0:0\"]\ntrusted_hash = \"sha256:deadbeef\"\n",
            config.display(),
        ));
    }
    std::fs::write(&config, text).expect("write trust state");
}

fn write_machine_config(env: &Env, text: &str) -> PathBuf {
    let dir = env.config_root().join("rimz");
    std::fs::create_dir_all(&dir).expect("mkdir config");
    let path = dir.join("config.toml");
    std::fs::write(&path, text).expect("write machine config");
    path
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

#[test]
fn doctor_reports_remote_control_preflight_refusals() {
    let env = Env::new();
    write_machine_config(&env, "[remote_control]\nclaude = true\ncodex = true\n");
    let settings = write_claude_settings(&env, r#"{ "disableRemoteControl": true }"#);
    let stub_dir = stub_claude_version(&env, "2.1.173 (Claude Code)");
    let codex_home = env.home_root.join("codex-home");

    let output = env
        .rimz()
        .arg("doctor")
        .env("PATH", path_with_stub_first(&stub_dir))
        .env("CODEX_HOME", &codex_home)
        .env("RIMZ_CLAUDE_SETTINGS", &settings)
        .output()
        .expect("spawn doctor");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8");

    assert!(stdout.contains("remote control: claude enabled, blocked"));
    assert!(stdout.contains("codex enabled, standalone install missing"));
    assert!(stdout.contains("`disableRemoteControl: true`"));
    assert!(stdout.contains("managed standalone Codex install is missing"));
    assert!(stdout.contains("[remote_control] claude = false"));
    assert!(stdout.contains("[remote_control] codex = false"));
}

#[test]
fn doctor_reports_protocol_version_mismatches() {
    let env = Env::new();

    let mut event = EventEnvelope::new(
        env.workspace_id.clone(),
        "test-session",
        "rimz",
        "cli",
        "event.emit",
        serde_json::json!({ "kind": "build.started" }),
    );
    event.schema_version = "rimz.event.v0".to_owned();
    env.ledger().append_event(&event).expect("append old event");

    let rt = env.runtime_paths();
    rt.ensure_dirs().expect("runtime dirs");

    let mut sidebar = SidebarHeartbeat::new(
        env.workspace_id.clone(),
        SidebarInstanceId::new(),
        MuxName::Tmux,
        "rimz-test",
        rt.sock_dir.join("sidebar.old.sock"),
        None,
    );
    sidebar.protocol_version = "rimz.plugin.v0".to_owned();
    rimz::ledger::atomic::write_temp_then_rename(
        &rt.heartbeat_dir.join("sidebar.old.json"),
        &sidebar,
    )
    .expect("write old sidebar heartbeat");

    let resolver_id: ResolverId = "opus-policy".parse().expect("resolver id");
    let mut resolver = ResolverHeartbeat::new(env.workspace_id.clone(), resolver_id);
    resolver.protocol_version = "rimz.resolver.v0".to_owned();
    rimz::ledger::atomic::write_temp_then_rename(
        &rt.heartbeat_dir.join("resolver.opus-policy.json"),
        &resolver,
    )
    .expect("write old resolver heartbeat");

    let output = env.rimz().arg("doctor").output().expect("spawn doctor");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8");

    assert!(stdout.contains(
        "protocols     : event rimz.event.v2; sidebar rimz.plugin.v4; resolver rimz.resolver.v1",
    ));
    assert!(stdout.contains(
        "protocol warn : event log schema rimz.event.v0 seen 1 record (expected rimz.event.v2)",
    ));
    assert!(stdout.contains(
        "protocol warn : sidebar heartbeat sidebar.old.json uses rimz.plugin.v0 (expected rimz.plugin.v4)",
    ));
    assert!(stdout.contains(
        "protocol warn : resolver heartbeat resolver.opus-policy.json uses rimz.resolver.v0 (expected rimz.resolver.v1)",
    ));
}
