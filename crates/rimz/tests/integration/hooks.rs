//! Out-of-process integration tests for `rimz hooks feed` exercising the
//! `Surface::Bridge` wiring landed in M1. Each test spawns a real `rimz`
//! binary; XDG roots are scoped under a tempdir so allowlist, state, and
//! runtime files don't escape.

use std::process::Command;
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use jiff::Timestamp;
use serde_json::{Value, json};

use crate::common::{
    Env, claude_pre_tool_use_payload, codex_permission_payload, permission_payload,
};

#[test]
fn hooks_install_is_discoverable_but_feed_entrypoint_is_hidden() {
    let top = Command::cargo_bin("rimz")
        .expect("cargo-bin")
        .arg("--help")
        .output()
        .expect("top help");
    assert!(top.status.success());
    let top_stdout = String::from_utf8(top.stdout).expect("utf8 top help");
    assert!(
        top_stdout.contains("hooks"),
        "top-level help should expose hook install/uninstall entrypoint:\n{top_stdout}"
    );

    let hooks = Command::cargo_bin("rimz")
        .expect("cargo-bin")
        .args(["hooks", "--help"])
        .output()
        .expect("hooks help");
    assert!(hooks.status.success());
    let hooks_stdout = String::from_utf8(hooks.stdout).expect("utf8 hooks help");
    assert!(hooks_stdout.contains("install"));
    assert!(hooks_stdout.contains("uninstall"));
    assert!(
        !hooks_stdout.contains("\n  feed"),
        "internal hook feed entrypoint should stay hidden:\n{hooks_stdout}"
    );
}

#[test]
fn hook_with_no_allowlisted_resolver_stays_native_ui() {
    let env = Env::new();
    let output = env.run_hook("claude", &permission_payload("Bash"));
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "{}",
        "neutral payload expected"
    );

    let items = env.feed_list_json();
    let items = items.as_array().expect("array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["surface"], "native_ui");
    assert_eq!(items[0]["status"], "pending");
}

#[test]
fn hook_with_stale_heartbeat_stays_native_ui() {
    let env = Env::new();
    env.enrol("opus-policy", 10, "30s");
    env.write_heartbeat("opus-policy", Timestamp::now() - Duration::from_secs(60));

    let output = env.run_hook("claude", &permission_payload("Bash"));
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "{}",
        "fresh heartbeat is required to engage bridge"
    );
    assert_eq!(env.feed_list_json()[0]["surface"], "native_ui");
}

#[test]
fn hook_with_resolver_chain_rejects_out_of_turn_and_advances_on_abstain() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    env.enrol("opus-policy", 10, "30s");
    env.enrol("slack-on-call", 20, "5m");
    env.write_heartbeat("opus-policy", Timestamp::now());
    env.write_heartbeat("slack-on-call", Timestamp::now());

    let child = env.spawn_hook("claude", &permission_payload("Bash"));

    let request_id = env
        .poll_pending_request_id(Instant::now() + Duration::from_secs(5))
        .expect("bridge item should appear in feed");

    let initial = env.feed_show_json(&request_id);
    assert_eq!(initial["chain"][0]["resolver_id"], "opus-policy");
    assert_eq!(initial["chain"][0]["state"], "active");
    assert_eq!(initial["chain"][1]["resolver_id"], "slack-on-call");
    assert_eq!(initial["chain"][1]["state"], "queued");
    assert_eq!(initial["chain_active_resolver"], "opus-policy");
    assert!(initial["chain_active_until"].is_string());

    let out_of_turn = env.resolve(
        &request_id,
        r#"{"choice":"allow"}"#,
        "slack-on-call",
        "hook-bridge",
    );
    assert!(
        !out_of_turn.status.success(),
        "queued resolver must not answer before it is active"
    );

    let abstain = env.abstain(&request_id, "opus-policy", "outside policy");
    assert!(
        abstain.status.success(),
        "abstain failed: {}",
        String::from_utf8_lossy(&abstain.stderr)
    );

    let advanced = env.feed_show_json(&request_id);
    assert_eq!(advanced["chain"][0]["state"], "abstained");
    assert_eq!(advanced["chain"][1]["state"], "active");
    assert_eq!(advanced["chain_active_resolver"], "slack-on-call");
    assert!(advanced["chain_active_until"].is_string());

    let resolve = env.resolve(
        &request_id,
        r#"{"choice":"allow"}"#,
        "slack-on-call",
        "hook-bridge",
    );
    assert!(
        resolve.status.success(),
        "resolve failed: {}",
        String::from_utf8_lossy(&resolve.stderr)
    );

    let output = child.wait_with_output().expect("wait child");
    assert!(
        output.status.success(),
        "hook stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let decision: Value = serde_json::from_str(stdout.trim()).expect("agent json");
    assert_eq!(
        decision["hookSpecificOutput"]["decision"]["behavior"],
        "allow"
    );

    let resolved = env.feed_show_json(&request_id);
    assert_eq!(resolved["status"], "resolved");
    assert_eq!(resolved["chain"][0]["state"], "abstained");
    assert_eq!(resolved["chain"][1]["state"], "answered");
    assert!(resolved["chain_active_resolver"].is_null());
    assert!(resolved["chain_active_until"].is_null());
}

#[test]
fn hook_with_fresh_resolver_engages_bridge_and_resolves() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    env.enrol("opus-policy", 10, "30s");
    env.write_heartbeat("opus-policy", Timestamp::now());

    let child = env.spawn_hook("claude", &permission_payload("Bash"));

    let request_id = env
        .poll_pending_request_id(Instant::now() + Duration::from_secs(5))
        .expect("bridge item should appear in feed");

    let resolve = env.resolve(
        &request_id,
        r#"{"choice":"allow"}"#,
        "opus-policy",
        "hook-bridge",
    );
    assert!(
        resolve.status.success(),
        "resolve failed: {}",
        String::from_utf8_lossy(&resolve.stderr)
    );

    let output = child.wait_with_output().expect("wait child");
    assert!(
        output.status.success(),
        "hook stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let decision: Value = serde_json::from_str(stdout.trim()).expect("agent json");
    assert_eq!(
        decision["hookSpecificOutput"]["decision"]["behavior"],
        "allow"
    );
    assert_eq!(
        decision["hookSpecificOutput"]["hookEventName"],
        "PermissionRequest"
    );
}

// --- Codex parity ---
//
// The hook bridge wiring is agent-agnostic; the only differences between
// adapters are the stdout payload shapes and the neutral payload. Codex
// expects `{"decision":"allow"|"deny"}` and an empty stdout on neutral.

#[test]
fn codex_hook_with_no_allowlisted_resolver_stays_native_ui() {
    let env = Env::new();
    let output = env.run_hook("codex", &codex_permission_payload());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "Codex neutral must be empty stdout, got: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );

    let items = env.feed_list_json();
    let items = items.as_array().expect("array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["surface"], "native_ui");
    assert_eq!(items[0]["status"], "pending");
    assert_eq!(items[0]["source"], "codex");
}

#[test]
fn codex_hook_with_stale_heartbeat_stays_native_ui() {
    let env = Env::new();
    env.enrol("opus-policy", 10, "30s");
    env.write_heartbeat("opus-policy", Timestamp::now() - Duration::from_secs(60));

    let output = env.run_hook("codex", &codex_permission_payload());
    assert!(output.status.success());
    assert!(
        output.stdout.is_empty(),
        "stale heartbeat must still emit Codex neutral (empty)"
    );
    assert_eq!(env.feed_list_json()[0]["surface"], "native_ui");
}

#[test]
fn codex_hook_with_fresh_resolver_engages_bridge_and_resolves() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    env.enrol("opus-policy", 10, "30s");
    env.write_heartbeat("opus-policy", Timestamp::now());

    let child = env.spawn_hook("codex", &codex_permission_payload());

    let request_id = env
        .poll_pending_request_id(Instant::now() + Duration::from_secs(5))
        .expect("bridge item should appear in feed");

    let resolve = env.resolve(
        &request_id,
        r#"{"choice":"allow"}"#,
        "opus-policy",
        "hook-bridge",
    );
    assert!(
        resolve.status.success(),
        "resolve failed: {}",
        String::from_utf8_lossy(&resolve.stderr)
    );

    let output = child.wait_with_output().expect("wait child");
    assert!(
        output.status.success(),
        "hook stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let decision: Value = serde_json::from_str(stdout.trim()).expect("agent json");
    assert_eq!(
        decision["hookSpecificOutput"]["hookEventName"],
        "PermissionRequest"
    );
    assert_eq!(
        decision["hookSpecificOutput"]["decision"]["behavior"],
        "allow"
    );
    // Reserved-key invariant — Codex PermissionRequest must never see fields
    // reserved for future behavior.
    assert!(decision.get("updatedInput").is_none());
    assert!(decision.get("updatedPermissions").is_none());
    assert!(decision.get("interrupt").is_none());
}

#[test]
fn codex_hook_bridge_cap_timeout_emits_neutral() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    env.enrol("opus-policy", 10, "30s");
    env.write_heartbeat("opus-policy", Timestamp::now());

    let mut cmd = env.hook_command("codex");
    cmd.env("RIMZ_HOOK_CAP_MILLIS", "200");
    let output = env
        .spawn_payload(cmd, &codex_permission_payload())
        .wait_with_output()
        .expect("wait child");
    assert!(
        output.status.success(),
        "hook stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "Codex cap-elapsed should emit empty stdout (neutral)"
    );

    let parsed = env.feed_list_json();
    assert_eq!(parsed[0]["status"], "timed_out");
    assert_eq!(parsed[0]["surface"], "bridge");
    assert_eq!(parsed[0]["source"], "codex");
}

#[test]
fn codex_session_start_writes_agent_lifecycle_event() {
    let env = Env::new();
    let payload = serde_json::to_string(&json!({
        "hook_event_name": "SessionStart",
        "session_id": "sess-codex-01",
        "approval_policy": "ask",
        "worktree_branch": "feature-x",
    }))
    .expect("payload");

    let output = env.run_hook("codex", &payload);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty(), "lifecycle hook is silent");

    // The lifecycle event must land in the snapshot's agents rollup.
    let parsed = env.snapshot_json();
    let agents = parsed["agents"].as_array().expect("agents array");
    assert_eq!(agents.len(), 1, "exactly one agent rolled up: {agents:?}");
    assert_eq!(agents[0]["kind"], "codex");
    assert_eq!(agents[0]["agent_id"], "sess-codex-01");
    // SessionStart registers the agent idle (wired in, nothing asked yet).
    assert_eq!(agents[0]["status"], "idle");
    assert_eq!(agents[0]["permission_posture"], "default");
    assert_eq!(agents[0]["worktree_branch"], "feature-x");
}

#[test]
fn codex_install_uninstall_cli_round_trips_into_codex_config() {
    let env = Env::new();
    let codex_config = env.project_root.join(".codex").join("config.toml");

    let install = env
        .rimz()
        .env("RIMZ_CODEX_CONFIG", &codex_config)
        .args(["hooks", "install", "codex"])
        .output()
        .expect("spawn install");
    assert!(
        install.status.success(),
        "install stderr: {}",
        String::from_utf8_lossy(&install.stderr)
    );
    let report: Value = serde_json::from_slice(&install.stdout).expect("install report json");
    assert_eq!(report["agent"], "codex");
    assert_eq!(report["merged"], false);
    let events = report["installed_events"].as_array().expect("events");
    let names: Vec<&str> = events.iter().filter_map(Value::as_str).collect();
    assert!(names.contains(&"SessionStart"));
    assert!(names.contains(&"SubagentStart"));
    assert!(names.contains(&"SubagentStop"));
    assert!(names.contains(&"PermissionRequest"));

    let written = std::fs::read_to_string(&codex_config).expect("read codex config");
    assert!(
        written.contains("[[hooks.SessionStart]]")
            && written.contains("rimz hooks feed --source codex"),
        "config must use Codex's documented inline hook shape:\n{written}"
    );

    let uninstall = env
        .rimz()
        .env("RIMZ_CODEX_CONFIG", &codex_config)
        .args(["hooks", "uninstall", "codex"])
        .output()
        .expect("spawn uninstall");
    assert!(
        uninstall.status.success(),
        "uninstall stderr: {}",
        String::from_utf8_lossy(&uninstall.stderr)
    );
    let report: Value = serde_json::from_slice(&uninstall.stdout).expect("uninstall report json");
    assert_eq!(report["existed"], true);
    let removed = report["removed_events"].as_array().expect("removed events");
    assert!(!removed.is_empty(), "must report removed events");
    let written = std::fs::read_to_string(&codex_config).expect("read codex config");
    assert!(!written.contains("rimz hooks feed --source codex"));
}

#[test]
fn codex_subagent_lifecycle_uses_child_agent_identity() {
    let env = Env::new();
    let start_payload = serde_json::to_string(&json!({
        "hook_event_name": "SubagentStart",
        "session_id": "sess-codex-parent",
        "agent_id": "child-thread-1",
        "agent_type": "review",
        "permission_mode": "acceptEdits",
        "worktree_branch": "feature-x",
    }))
    .expect("payload");

    let output = env.run_hook("codex", &start_payload);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "subagent lifecycle hook is silent"
    );

    let parsed = env.snapshot_json();
    let agents = parsed["agents"].as_array().expect("agents array");
    assert_eq!(
        agents.len(),
        1,
        "exactly one subagent rolled up: {agents:?}"
    );
    assert_eq!(agents[0]["agent_id"], "child-thread-1");
    assert_eq!(agents[0]["status"], "running");
    assert_eq!(agents[0]["permission_posture"], "auto");
    assert_eq!(agents[0]["task"], "review");

    let stop_payload = serde_json::to_string(&json!({
        "hook_event_name": "SubagentStop",
        "session_id": "sess-codex-parent",
        "agent_id": "child-thread-1",
        "agent_type": "review",
    }))
    .expect("payload");
    let output = env.run_hook("codex", &stop_payload);
    assert!(output.status.success());
    assert!(output.stdout.is_empty(), "subagent stop hook is silent");

    let parsed = env.snapshot_json();
    assert_eq!(parsed["agents"][0]["agent_id"], "child-thread-1");
    assert_eq!(parsed["agents"][0]["status"], "idle");
    assert!(parsed["agents"][0]["task"].is_null());
}

#[test]
fn codex_subagent_permission_request_replaces_child_agent_row() {
    let env = Env::new();
    let start_payload = serde_json::to_string(&json!({
        "hook_event_name": "SubagentStart",
        "session_id": "sess-codex-parent",
        "agent_id": "child-thread-1",
        "agent_type": "review",
        "permission_mode": "default",
    }))
    .expect("payload");
    let output = env.run_hook("codex", &start_payload);
    assert!(output.status.success());

    let permission_payload = serde_json::to_string(&json!({
        "hook_event_name": "PermissionRequest",
        "session_id": "sess-codex-parent",
        "agent_id": "child-thread-1",
        "agent_type": "review",
        "tool_name": "Bash",
        "tool_input": { "command": "cargo test" },
    }))
    .expect("payload");
    let output = env.run_hook("codex", &permission_payload);
    assert!(output.status.success());
    assert!(output.stdout.is_empty());

    let parsed = env.snapshot_json();
    let groups = parsed["worktree_groups"].as_array().expect("groups");
    assert_eq!(groups.len(), 1, "one worktree group expected: {groups:?}");
    let rows = groups[0]["rows"].as_array().expect("rows");
    assert_eq!(
        rows.len(),
        1,
        "pending subagent request should replace the running child row: {rows:?}"
    );
    assert_eq!(rows[0]["id"], "child-thread-1");
    assert_eq!(rows[0]["status"], "waiting");
    assert_eq!(rows[0]["task"], "review");
}

#[test]
fn codex_uninstall_cli_removes_legacy_config_block() {
    let env = Env::new();
    let codex_config = env.project_root.join(".codex").join("config.toml");
    std::fs::create_dir_all(codex_config.parent().unwrap()).expect("mkdir codex config dir");
    std::fs::write(
        &codex_config,
        "model = \"gpt-5.5\"\n[hooks.rimz]\nmanaged_by = \"rimz\"\nevents = [\"SessionStart\"]\n",
    )
    .expect("write legacy codex config");

    let uninstall = env
        .rimz()
        .env("RIMZ_CODEX_CONFIG", &codex_config)
        .args(["hooks", "uninstall", "codex"])
        .output()
        .expect("spawn uninstall");
    assert!(
        uninstall.status.success(),
        "uninstall stderr: {}",
        String::from_utf8_lossy(&uninstall.stderr)
    );
    let report: Value = serde_json::from_slice(&uninstall.stdout).expect("uninstall report json");
    assert_eq!(report["existed"], true);
    let removed = report["removed_events"].as_array().expect("removed events");
    assert!(!removed.is_empty(), "must report removed events");
}

#[test]
fn codex_session_start_with_never_policy_observes_yolo_posture() {
    let env = Env::new();
    let payload = serde_json::to_string(&json!({
        "hook_event_name": "SessionStart",
        "session_id": "sess-codex-bypass",
        "approval_policy": "never",
    }))
    .expect("payload");

    let output = env.run_hook("codex", &payload);
    assert!(output.status.success());

    let parsed = env.snapshot_json();
    assert_eq!(parsed["agents"][0]["permission_posture"], "yolo");
}

#[test]
fn hook_bridge_cap_timeout_emits_neutral() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    env.enrol("opus-policy", 10, "30s");
    env.write_heartbeat("opus-policy", Timestamp::now());

    let mut cmd = env.hook_command("claude");
    cmd.env("RIMZ_HOOK_CAP_MILLIS", "200");
    let output = env
        .spawn_payload(cmd, &permission_payload("Bash"))
        .wait_with_output()
        .expect("wait child");
    assert!(
        output.status.success(),
        "hook stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "{}",
        "cap elapsed should emit Claude's neutral payload"
    );

    let parsed = env.feed_list_json();
    assert_eq!(parsed[0]["status"], "timed_out");
    assert_eq!(parsed[0]["surface"], "bridge");
}

// --- Claude PreToolUse blocking events ---
//
// `ExitPlanMode` and `AskUserQuestion` are PreToolUse blocking hooks. The
// agent expects the decision to carry `updatedInput`; the neutral payload
// stays `{}` and the agent's own UI is the answer surface.

#[test]
fn claude_exit_plan_mode_default_path_pushes_plan_approval() {
    let env = Env::new();
    let output = env.run_hook("claude", &claude_pre_tool_use_payload("ExitPlanMode"));
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "{}",
        "neutral payload for Claude blocking hook"
    );

    let items = env.feed_list_json();
    let items = items.as_array().expect("array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["surface"], "native_ui");
    assert_eq!(items[0]["status"], "pending");
    assert_eq!(items[0]["kind"], "plan_approval");
}

#[test]
fn claude_ask_user_question_default_path_pushes_question() {
    let env = Env::new();
    let output = env.run_hook("claude", &claude_pre_tool_use_payload("AskUserQuestion"));
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "{}");

    let parsed = env.feed_list_json();
    assert_eq!(parsed[0]["kind"], "question");
    assert_eq!(parsed[0]["surface"], "native_ui");
}

#[test]
fn claude_exit_plan_mode_bridge_path_renders_updated_input() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    env.enrol("opus-policy", 10, "30s");
    env.write_heartbeat("opus-policy", Timestamp::now());

    let child = env.spawn_hook("claude", &claude_pre_tool_use_payload("ExitPlanMode"));

    let request_id = env
        .poll_pending_request_id(Instant::now() + Duration::from_secs(5))
        .expect("bridge item should appear in feed");

    let resolve = env.resolve(
        &request_id,
        r#"{"choice":"allow","updatedInput":{"plan":"approved"}}"#,
        "opus-policy",
        "hook-bridge",
    );
    assert!(
        resolve.status.success(),
        "resolve failed: {}",
        String::from_utf8_lossy(&resolve.stderr)
    );

    let output = child.wait_with_output().expect("wait child");
    assert!(
        output.status.success(),
        "hook stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let decision: Value = serde_json::from_str(stdout.trim()).expect("agent json");
    assert_eq!(
        decision["hookSpecificOutput"]["hookEventName"],
        "PreToolUse"
    );
    assert_eq!(
        decision["hookSpecificOutput"]["permissionDecision"],
        "allow"
    );
    assert_eq!(
        decision["hookSpecificOutput"]["updatedInput"]["plan"],
        "approved"
    );
}

#[test]
fn claude_ask_user_question_bridge_path_renders_updated_input() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    env.enrol("opus-policy", 10, "30s");
    env.write_heartbeat("opus-policy", Timestamp::now());

    let child = env.spawn_hook("claude", &claude_pre_tool_use_payload("AskUserQuestion"));

    let request_id = env
        .poll_pending_request_id(Instant::now() + Duration::from_secs(5))
        .expect("bridge item should appear in feed");

    let resolve = env.resolve(
        &request_id,
        r#"{"choice":"allow","updatedInput":{"question":"clarified"}}"#,
        "opus-policy",
        "hook-bridge",
    );
    assert!(
        resolve.status.success(),
        "resolve failed: {}",
        String::from_utf8_lossy(&resolve.stderr)
    );

    let output = child.wait_with_output().expect("wait child");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let decision: Value = serde_json::from_str(stdout.trim()).expect("agent json");
    assert_eq!(
        decision["hookSpecificOutput"]["updatedInput"]["question"],
        "clarified"
    );
    assert_eq!(
        decision["hookSpecificOutput"]["permissionDecision"],
        "allow"
    );
}

// --- Claude lifecycle and install/uninstall ---

#[test]
fn claude_session_start_writes_agent_lifecycle_event() {
    let env = Env::new();
    let payload = serde_json::to_string(&json!({
        "hook_event_name": "SessionStart",
        "session_id": "sess-claude-01",
        "permission_mode": "default",
        "worktree_branch": "feature-x",
    }))
    .expect("payload");

    let output = env.run_hook("claude", &payload);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Claude's neutral payload is `{}` — emitted even for lifecycle hooks so
    // the agent always sees a well-formed JSON response.
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "{}");

    let parsed = env.snapshot_json();
    let agents = parsed["agents"].as_array().expect("agents array");
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["kind"], "claude");
    assert_eq!(agents[0]["agent_id"], "sess-claude-01");
    // SessionStart registers the agent idle (wired in, nothing asked yet).
    assert_eq!(agents[0]["status"], "idle");
    assert_eq!(agents[0]["permission_posture"], "default");
    assert_eq!(agents[0]["worktree_branch"], "feature-x");
}

#[test]
fn claude_session_start_with_bypass_permissions_observes_yolo_posture() {
    let env = Env::new();
    let payload = serde_json::to_string(&json!({
        "hook_event_name": "SessionStart",
        "session_id": "sess-claude-bypass",
        "permission_mode": "bypassPermissions",
    }))
    .expect("payload");

    let output = env.run_hook("claude", &payload);
    assert!(output.status.success());

    let parsed = env.snapshot_json();
    assert_eq!(parsed["agents"][0]["permission_posture"], "yolo");
}

#[test]
fn claude_install_uninstall_cli_round_trips_into_settings_json() {
    let env = Env::new();
    let claude_settings = env.project_root.join(".claude").join("settings.json");

    let install = env
        .rimz()
        .env("RIMZ_CLAUDE_SETTINGS", &claude_settings)
        .args(["hooks", "install", "claude"])
        .output()
        .expect("spawn install");
    assert!(
        install.status.success(),
        "install stderr: {}",
        String::from_utf8_lossy(&install.stderr)
    );
    let report: Value = serde_json::from_slice(&install.stdout).expect("install report json");
    assert_eq!(report["agent"], "claude");
    assert_eq!(report["merged"], false);
    let events = report["installed_events"].as_array().expect("events");
    let names: Vec<&str> = events.iter().filter_map(Value::as_str).collect();
    assert!(names.contains(&"SessionStart"));
    assert!(names.contains(&"PermissionRequest"));
    assert!(names.contains(&"PreToolUse:ExitPlanMode|AskUserQuestion"));

    assert!(
        claude_settings.exists(),
        "settings file should exist after install"
    );
    let on_disk: Value =
        serde_json::from_slice(&std::fs::read(&claude_settings).unwrap()).expect("settings json");
    // PreToolUse block has the combined blocking matcher plus the broad
    // per-tool hook.
    let pre_tool = on_disk["hooks"]["PreToolUse"].as_array().expect("array");
    assert_eq!(pre_tool.len(), 2);

    let uninstall = env
        .rimz()
        .env("RIMZ_CLAUDE_SETTINGS", &claude_settings)
        .args(["hooks", "uninstall", "claude"])
        .output()
        .expect("spawn uninstall");
    assert!(
        uninstall.status.success(),
        "uninstall stderr: {}",
        String::from_utf8_lossy(&uninstall.stderr)
    );
    let report: Value = serde_json::from_slice(&uninstall.stdout).expect("uninstall report json");
    assert_eq!(report["existed"], true);
    let removed = report["removed_events"].as_array().expect("removed events");
    assert!(
        !removed.is_empty(),
        "uninstall must report removed event labels"
    );
}
