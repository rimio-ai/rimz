//! Agent hook-payload fixtures and the environment probes the example-resolver
//! tests lean on (`python3` availability, resolver heartbeat liveness).

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use rimz::schema::heartbeat::ResolverHeartbeat;
use serde_json::json;

use super::env::Env;

/// Claude-shaped `PermissionRequest` hook payload for `tool_name`.
pub fn permission_payload(tool_name: &str) -> String {
    serde_json::to_string(&json!({
        "hook_event_name": "PermissionRequest",
        "session_id": "sess-claude-permission",
        "tool_name": tool_name,
        "tool_input": { "command": "echo hi" },
    }))
    .expect("payload")
}

/// Codex-shaped `PermissionRequest` payload (shell command vector, no
/// Claude-only fields).
pub fn codex_permission_payload() -> String {
    serde_json::to_string(&json!({
        "hook_event_name": "PermissionRequest",
        "session_id": "sess-codex-permission",
        "tool_name": "shell",
        "command": ["echo", "hi"],
    }))
    .expect("payload")
}

/// Claude `PreToolUse` blocking-hook payload (`ExitPlanMode`,
/// `AskUserQuestion`).
pub fn claude_pre_tool_use_payload(tool_name: &str) -> String {
    serde_json::to_string(&json!({
        "hook_event_name": "PreToolUse",
        "tool_name": tool_name,
        "tool_input": { "plan": "ship it" },
        "session_id": "sess-claude-pretool",
    }))
    .expect("payload")
}

/// Pi-shaped blocking `tool_call` payload. Rimz authors pi's wire (the
/// extension is Rimz code), so this mirrors `extension.ts`'s envelope —
/// lowercase pi tool names (`bash`, `read`, `edit`, …).
pub fn pi_tool_call_payload(tool_name: &str) -> String {
    serde_json::to_string(&json!({
        "hook_event_name": "tool_call",
        "session_id": "sess-pi-tool",
        "tool_name": tool_name,
        "tool_input": { "command": "echo hi" },
    }))
    .expect("payload")
}

/// Absolute path to a reference resolver script under `examples/resolvers/`.
/// One place owns the `crates/<crate>` → workspace-root climb that every
/// example-resolver test would otherwise hand-roll.
pub fn example_resolver_script(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("workspace root")
        .join("examples/resolvers")
        .join(name)
}

/// Spawn a reference Python resolver pointed at the harness workspace, stdio
/// piped. When `tmux_pane` is `Some`, route the resolver's `rimz` invocations
/// at an isolated tmux server: `TMUX_PANE` selects tmux for backend detection,
/// `TMUX_TMPDIR` pins the socket, and every other mux-detection variable is
/// dropped so tmux is the only mux detected.
pub fn spawn_example_resolver(
    env: &Env,
    script_name: &str,
    resolver_id: &str,
    run_seconds: f32,
    tmux_pane: Option<&str>,
) -> Child {
    let script = example_resolver_script(script_name);
    assert!(script.exists(), "resolver script missing: {script:?}");

    let mut cmd = Command::new("python3");
    cmd.arg(&script)
        .args([
            "--workspace-id",
            env.workspace_id.as_str(),
            "--resolver-id",
            resolver_id,
            "--rimz-bin",
            &env.rimz_bin().display().to_string(),
            "--tick-seconds",
            "0.1",
            "--run-seconds",
            &run_seconds.to_string(),
        ])
        .env("XDG_STATE_HOME", env.state_root())
        .env("XDG_RUNTIME_DIR", &env.runtime_root)
        .env("XDG_CONFIG_HOME", env.config_root())
        .env("HOME", &env.project_root)
        .env_remove("RUST_LOG")
        .current_dir(&env.project_root);
    if let Some(pane) = tmux_pane {
        let tmpdir = env.project_root.join("tmux");
        std::fs::create_dir_all(&tmpdir).expect("mkdir tmux tmpdir");
        cmd.env("TMUX_TMPDIR", tmpdir)
            .env("TMUX_PANE", pane)
            .env_remove("TMUX")
            .env_remove("ZELLIJ")
            .env_remove("ZELLIJ_PANE_ID")
            .env_remove("ZELLIJ_SESSION_NAME");
    }
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn python resolver")
}

/// Whether `python3` is on PATH — example-resolver tests self-skip without it.
pub fn python3_present() -> bool {
    Command::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Shared self-skip gate for the example-resolver tests: both need `python3`
/// on PATH and an environment that permits AF_UNIX sockets. Returns `true` when
/// the test should bail early.
pub fn skip_preconditions(env: &Env) -> bool {
    if !python3_present() {
        tracing::warn!("skipping: python3 not on PATH");
        return true;
    }
    env.skip_if_sandboxed()
}

/// Block until `resolver_id` has written a fresh heartbeat, or panic at
/// `until`. Used by the example-resolver tests that wait for a spawned Python
/// resolver to come alive before firing a hook.
pub fn wait_for_heartbeat(env: &Env, resolver_id: &str, until: Instant) {
    let path = env
        .heartbeat_dir()
        .join(format!("resolver.{resolver_id}.json"));
    let ttl = Duration::from_secs(3);
    while Instant::now() < until {
        if let Ok(bytes) = std::fs::read(&path)
            && let Ok(parsed) = serde_json::from_slice::<ResolverHeartbeat>(&bytes)
        {
            let age = Timestamp::now().duration_since(parsed.last_seen);
            if !age.is_negative() && (age.as_secs() as u64) < ttl.as_secs() {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("python resolver never wrote a fresh heartbeat at {path:?}");
}
