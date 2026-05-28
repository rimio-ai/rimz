//! Agent hook-payload fixtures and the environment probes the example-resolver
//! tests lean on (`python3` availability, resolver heartbeat liveness).

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use rimz::schema::heartbeat::ResolverHeartbeat;
use serde_json::json;

use super::env::Env;

/// Claude-shaped `PermissionRequest` hook payload for `tool_name`.
pub fn permission_payload(tool_name: &str) -> String {
    serde_json::to_string(&json!({
        "hook_event_name": "PermissionRequest",
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
