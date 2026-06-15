//! Out-of-process integration tests for `rimz hooks feed` exercising the
//! `Surface::Bridge` wiring landed in M1. Each test spawns a real `rimz`
//! binary; XDG roots are scoped under a tempdir so allowlist, state, and
//! runtime files don't escape.

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use jiff::Timestamp;
use serde_json::{Value, json};

use crate::common::{
    Env, claude_pre_tool_use_payload, codex_permission_payload, codex_pre_tool_use_payload,
    permission_payload, pi_tool_call_payload, tmux_pane,
};

const BRIDGE_ITEM_WAIT: Duration = Duration::from_secs(5);
const TEST_HOOK_CAP_MILLIS: &str = "50";

fn permission_cases() -> [(&'static str, String); 2] {
    [
        ("claude", permission_payload("Bash")),
        ("codex", codex_permission_payload()),
    ]
}

fn assert_hook_succeeded_neutral(source: &str, output: Output) {
    assert!(
        output.status.success(),
        "{source} hook stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "{source} neutral stdout must stay empty, got: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
}

fn bridge_permission_to_allow(env: &Env, source: &str, payload: &str) -> Value {
    env.enrol("opus-policy", 10, "30s");
    env.write_heartbeat("opus-policy", Timestamp::now());

    let child = env.spawn_hook(source, payload);
    let request_id = env
        .poll_pending_request_id(Instant::now() + BRIDGE_ITEM_WAIT)
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
        "{source} hook stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    serde_json::from_str(stdout.trim()).expect("agent json")
}

fn assert_permission_allow_decision(source: &str, decision: &Value) {
    assert_eq!(
        decision["hookSpecificOutput"]["hookEventName"],
        "PermissionRequest"
    );
    assert_eq!(
        decision["hookSpecificOutput"]["decision"]["behavior"],
        "allow"
    );
    if source == "codex" {
        // Reserved-key invariant — Codex PermissionRequest must never see
        // fields reserved for future behavior.
        assert!(decision.get("updatedInput").is_none());
        assert!(decision.get("updatedPermissions").is_none());
        assert!(decision.get("interrupt").is_none());
    }
}

fn lifecycle_event_count(env: &Env) -> usize {
    env.read_events()
        .iter()
        .filter(|event| event.method == "agent.lifecycle")
        .count()
}

fn run_claude_lifecycle(env: &Env, payload: Value) {
    let payload = serde_json::to_string(&payload).expect("payload");
    let output = env.run_hook("claude", &payload);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty(), "lifecycle hook is silent");
}

fn run_cap_timeout(env: &Env, source: &str, payload: &str) -> Output {
    env.enrol("opus-policy", 10, "30s");
    env.write_heartbeat("opus-policy", Timestamp::now());

    let mut cmd = env.hook_command(source);
    cmd.env("RIMZ_HOOK_CAP_MILLIS", TEST_HOOK_CAP_MILLIS);
    env.spawn_payload(cmd, payload)
        .wait_with_output()
        .expect("wait child")
}

#[test]
fn session_start_hooks_write_lifecycle_rows() {
    for (source, payload, expected_id, expected_fields) in [
        (
            "codex",
            json!({
                "hook_event_name": "SessionStart",
                "session_id": "sess-codex-01",
                "approval_policy": "ask",
                "worktree_branch": "feature-x",
            }),
            "sess-codex-01",
            vec![("worktree_branch", json!("feature-x"))],
        ),
        (
            "pi",
            json!({
                "hook_event_name": "session_start",
                "session_id": "019e9161-a5d0-791d-879e-39679acd4ded",
                "reason": "startup",
                "model": "gpt-5.5",
                "context_pct": 3,
                "context_window": 272000,
                "total_tokens": 8160,
            }),
            "019e9161-a5d0-791d-879e-39679acd4ded",
            vec![
                ("model", json!("gpt-5.5")),
                ("context_window", json!(272000)),
            ],
        ),
        (
            "claude",
            json!({
                "hook_event_name": "SessionStart",
                "session_id": "sess-claude-01",
                "permission_mode": "default",
                "worktree_branch": "feature-x",
            }),
            "sess-claude-01",
            vec![("worktree_branch", json!("feature-x"))],
        ),
    ] {
        let env = Env::new();
        let payload = serde_json::to_string(&payload).expect("payload");
        let output = env.run_hook(source, &payload);
        assert!(
            output.status.success(),
            "{source} stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stdout.is_empty(),
            "{source} lifecycle hook is silent"
        );

        let parsed = env.snapshot_json();
        let agents = parsed["agents"].as_array().expect("agents array");
        assert_eq!(agents.len(), 1, "{source} rolled up one agent: {agents:?}");
        assert_eq!(agents[0]["kind"], source);
        assert_eq!(agents[0]["agent_id"], expected_id);
        assert_eq!(agents[0]["status"], "idle");
        for (field, value) in expected_fields {
            assert_eq!(agents[0][field], value, "{source} {field}");
        }
    }
}

#[test]
fn permission_hook_with_no_allowlisted_resolver_stays_native_ui() {
    for (source, payload) in permission_cases() {
        let env = Env::new();
        let output = env.run_hook(source, &payload);
        assert_hook_succeeded_neutral(source, output);

        let items = env.feed_list_json();
        let items = items.as_array().expect("array");
        assert_eq!(items.len(), 1, "{source} should create one feed item");
        assert_eq!(items[0]["surface"], "native_ui");
        assert_eq!(items[0]["status"], "pending");
        assert_eq!(items[0]["source"], source);
    }
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
        .poll_pending_request_id(Instant::now() + BRIDGE_ITEM_WAIT)
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
fn permission_hook_with_fresh_resolver_engages_bridge_and_resolves() {
    for (source, payload) in permission_cases() {
        let env = Env::new();
        if env.skip_if_sandboxed() {
            continue;
        }

        let decision = bridge_permission_to_allow(&env, source, &payload);
        assert_permission_allow_decision(source, &decision);
    }
}

#[test]
fn permission_hook_bridge_cap_timeout_emits_neutral() {
    for (source, payload) in permission_cases() {
        let env = Env::new();
        if env.skip_if_sandboxed() {
            continue;
        }

        let output = run_cap_timeout(&env, source, &payload);
        assert_hook_succeeded_neutral(source, output);

        let parsed = env.feed_list_json();
        assert_eq!(parsed[0]["status"], "timed_out");
        assert_eq!(parsed[0]["surface"], "bridge");
        assert_eq!(parsed[0]["source"], source);
    }
}

#[test]
fn codex_daemon_routed_lifecycle_hooks_recover_distinct_pane_stamps() {
    let env = Env::new();
    let mut left = tmux_pane("%10", "codex", &env.project_root);
    left.pane_process_start = Some(Timestamp::UNIX_EPOCH);
    let mut right = tmux_pane("%11", "codex", &env.project_root);
    right.pane_process_start = Some(Timestamp::UNIX_EPOCH);

    left.is_focused = true;
    right.is_focused = false;
    let out = run_codex_daemon_lifecycle_with_panes(
        &env,
        "sess-codex-left",
        &[left.clone(), right.clone()],
    );
    assert!(
        out.status.success(),
        "first daemon hook stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    left.is_focused = false;
    right.is_focused = true;
    let out = run_codex_daemon_lifecycle_with_panes(
        &env,
        "sess-codex-right",
        &[left.clone(), right.clone()],
    );
    assert!(
        out.status.success(),
        "second daemon hook stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let first_snapshot = env.snapshot_json_with_panes(&[right.clone(), left.clone()]);
    assert_agent_pane(&first_snapshot, "sess-codex-left", "tmux:%10");
    assert_agent_pane(&first_snapshot, "sess-codex-right", "tmux:%11");

    left.is_focused = true;
    right.is_focused = false;
    let second_snapshot = env.snapshot_json_with_panes(&[left.clone(), right.clone()]);
    assert_agent_pane(&second_snapshot, "sess-codex-left", "tmux:%10");
    assert_agent_pane(&second_snapshot, "sess-codex-right", "tmux:%11");

    let log_path = rimz::binding_log::path(&env.runtime_paths());
    let log = std::fs::read_to_string(&log_path).expect("binding log");
    let records = log
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("binding log record"))
        .collect::<Vec<_>>();
    assert!(
        records.iter().any(|record| {
            record["event"] == "hook_focused_pane_recovery"
                && record["agent_id"] == "sess-codex-left"
                && record["outcome"]["outcome"] == "selected"
                && record["outcome"]["pane_id"] == "tmux:%10"
        }),
        "left hook recovery record missing:\n{log}"
    );
    assert!(
        records.iter().any(|record| {
            record["event"] == "hook_focused_pane_recovery"
                && record["agent_id"] == "sess-codex-right"
                && record["outcome"]["outcome"] == "selected"
                && record["outcome"]["pane_id"] == "tmux:%11"
        }),
        "right hook recovery record missing:\n{log}"
    );
}

fn run_codex_daemon_lifecycle_with_panes(
    env: &Env,
    session_id: &str,
    panes: &[rimz::feed::PaneRef],
) -> Output {
    let pane_fixture = env.write_pane_fixture(panes);
    let payload = serde_json::to_string(&json!({
        "hook_event_name": "SessionStart",
        "session_id": session_id,
        "approval_policy": "ask",
    }))
    .expect("payload");
    let mut cmd = env.rimz();
    cmd.args(["--mux", "tmux", "hooks", "feed", "--source", "codex"])
        .env("RIMZ_AGENT_PID", std::process::id().to_string())
        .env("RIMZ_TEST_PANE_LIST", pane_fixture)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    env.spawn_payload(cmd, &payload)
        .wait_with_output()
        .expect("wait daemon hook")
}

fn assert_agent_pane(snapshot: &Value, agent_id: &str, pane_id: &str) {
    let agent = snapshot["agents"]
        .as_array()
        .expect("agents array")
        .iter()
        .find(|agent| agent["agent_id"] == agent_id)
        .unwrap_or_else(|| panic!("agent {agent_id} missing from snapshot: {snapshot:#}"));
    assert_eq!(agent["pane"]["pane_id"], pane_id);
}

#[test]
fn pi_tool_call_with_no_resolver_emits_neutral_and_no_feed_item() {
    // Pi has no native permission prompt (`native_ask_ui` = false): with no
    // fresh resolver the hook must answer neutral (empty stdout = the tool
    // runs) and push NO feed item — nothing could ever answer one.
    let env = Env::new();
    let output = env.run_hook("pi", &pi_tool_call_payload("bash"));
    assert_hook_succeeded_neutral("pi", output);

    let items = env.feed_list_json();
    assert_eq!(
        items.as_array().expect("array").len(),
        0,
        "pi must not orphan an unanswerable native_ui item: {items}"
    );
}

#[test]
fn pi_tool_call_bridge_allow_renders_empty_object() {
    let env = Env::new();
    env.enrol("opus-policy", 10, "30s");
    env.write_heartbeat("opus-policy", Timestamp::now());

    let child = env.spawn_hook("pi", &pi_tool_call_payload("bash"));
    let request_id = env
        .poll_pending_request_id(Instant::now() + BRIDGE_ITEM_WAIT)
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
        "pi hook stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let decision: Value = serde_json::from_str(stdout.trim()).expect("pi decision json");
    // Pi's allow is the empty object — the extension blocks only on
    // `block === true`.
    assert_eq!(decision, json!({}), "decision: {decision}");
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
    assert_eq!(agents[0]["task"], "review");
    // The child keys off `agent_id`; the payload's `session_id` is captured as
    // the parent root so the sidebar can nest the child under it.
    assert_eq!(agents[0]["parent_agent_id"], "sess-codex-parent");

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
    // Codex reports no subagent error signal, so a stop resolves success and
    // the finished child reads `✓` in the parent's expanded list.
    assert_eq!(parsed["agents"][0]["status"], "success");
    // The type label and the parent link both persist past stop so a finished
    // child stays labeled and nested while it lingers in the parent's list.
    assert_eq!(parsed["agents"][0]["task"], "review");
    assert_eq!(parsed["agents"][0]["parent_agent_id"], "sess-codex-parent");
}

#[test]
fn codex_subagent_permission_without_parent_frame_stays_metadata_only() {
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
    assert_eq!(
        groups.len(),
        0,
        "a child-only ask has no frame-backed parent card: {groups:?}"
    );
    let needs_attention = parsed["needs_attention"].as_array().expect("needs");
    assert_eq!(
        needs_attention.len(),
        1,
        "the pending ask remains ledger metadata"
    );
    assert_eq!(needs_attention[0]["payload"]["agent_id"], "child-thread-1");
}

#[test]
fn claude_in_subagent_tool_event_does_not_disturb_parent() {
    // Claude stamps `agent_id` on every payload fired inside a subagent, so a
    // backgrounded child's mutating tool arrives on the parent's session with a
    // foreign id. It must fold to nothing: no lifecycle event appended, no
    // phantom child row, and — the load-bearing part — the parent's
    // `last_activity` stays its own (the child-keyed heartbeat carries the
    // child's progress instead).
    let env = Env::new();
    let output = env.run_hook(
        "claude",
        &serde_json::to_string(&json!({
            "hook_event_name": "SessionStart",
            "session_id": "sess-claude-parent",
        }))
        .expect("payload"),
    );
    assert!(output.status.success());
    let lifecycle_events_before = env
        .read_events()
        .iter()
        .filter(|event| event.method == "agent.lifecycle")
        .count();

    let output = env.run_hook(
        "claude",
        &serde_json::to_string(&json!({
            "hook_event_name": "PostToolUse",
            "session_id": "sess-claude-parent",
            "agent_id": "child-1",
            "tool_name": "Edit",
        }))
        .expect("payload"),
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty(), "lifecycle hook is silent");

    let lifecycle_events_after = env
        .read_events()
        .iter()
        .filter(|event| event.method == "agent.lifecycle")
        .count();
    assert_eq!(
        lifecycle_events_after, lifecycle_events_before,
        "a foreign-child tool event appends no lifecycle event"
    );
    let parsed = env.snapshot_json();
    let agents = parsed["agents"].as_array().expect("agents array");
    assert_eq!(agents.len(), 1, "no phantom child row: {agents:?}");
    assert_eq!(agents[0]["agent_id"], "sess-claude-parent");
}

#[test]
fn pending_native_ui_ask_survives_backgrounded_child_tool() {
    // The asking-while-running regression lock: a parent blocked on a native_ui
    // ask must stay `waiting` while a backgrounded subagent works. Before the
    // foreign-id drop, the child's mutating PostToolUse advanced the parent's
    // `last_activity` past the ask and the `waiting` fold dropped.
    let env = Env::new();
    let run = |payload: &Value| {
        let payload = serde_json::to_string(payload).expect("payload");
        let output = env.run_installed_hook_in_pane("claude", &payload, &[("TMUX_PANE", "%0")]);
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };

    run(&json!({
        "hook_event_name": "SessionStart",
        "session_id": "sess-claude-parent",
    }));
    run(&json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "sess-claude-parent",
        "prompt": "fix the sidebar reload bug",
    }));

    // No resolver enrolled, so the blocking ask lands native_ui and pending.
    run(&json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "AskUserQuestion",
        "tool_input": { "questions": [{ "question": "which fix shape?" }] },
        "session_id": "sess-claude-parent",
    }));
    let items = env.feed_list_json();
    assert_eq!(items[0]["surface"], "native_ui");
    assert_eq!(items[0]["status"], "pending");
    let request_id = items[0]["request_id"].as_str().expect("id").to_owned();

    // The backgrounded child keeps working while the parent blocks.
    run(&json!({
        "hook_event_name": "PostToolUse",
        "session_id": "sess-claude-parent",
        "agent_id": "child-1",
        "tool_name": "Bash",
    }));

    let parsed = env.snapshot_json_with_panes(&[tmux_pane("%0", "claude", &env.project_root)]);
    let groups = parsed["worktree_groups"].as_array().expect("groups");
    let rows: Vec<&Value> = groups
        .iter()
        .flat_map(|group| group["rows"].as_array().expect("rows"))
        .collect();
    assert_eq!(rows.len(), 1, "one waiting parent row expected: {rows:?}");
    assert_eq!(rows[0]["status"], "waiting");
    assert_eq!(rows[0]["request_id"], request_id.as_str());
}

#[test]
fn manual_compact_then_pre_tool_use_resumes_running() {
    let env = Env::new();
    let run = |payload: &serde_json::Value| {
        let payload = serde_json::to_string(payload).expect("payload");
        let output = env.run_hook("codex", &payload);
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty(), "lifecycle hook is silent");
    };

    run(&json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "sess-codex-compact",
        "prompt": "continue after compact",
    }));
    run(&json!({
        "hook_event_name": "PostCompact",
        "session_id": "sess-codex-compact",
        "trigger": "manual",
    }));
    let after_manual = env.snapshot_json();
    assert_eq!(after_manual["agents"][0]["status"], "idle");
    let before = lifecycle_event_count(&env);

    run(&json!({
        "hook_event_name": "PreToolUse",
        "session_id": "sess-codex-compact",
        "tool_name": "shell",
    }));

    assert_eq!(
        lifecycle_event_count(&env),
        before + 1,
        "a resting-row PreToolUse reconciliation is persisted"
    );
    let resumed = env.snapshot_json();
    assert_eq!(resumed["agents"][0]["status"], "running");
}

// --- Claude PreToolUse blocking events ---
//
// `ExitPlanMode` and `AskUserQuestion` are PreToolUse blocking hooks. The
// agent expects the decision to carry `updatedInput`; neutral keeps stdout
// empty and the agent's own UI is the answer surface.

#[test]
fn claude_pre_tool_blocking_events_use_native_ui_without_resolver() {
    for (tool, expected_kind) in [
        ("ExitPlanMode", "plan_approval"),
        ("AskUserQuestion", "question"),
    ] {
        let env = Env::new();
        let output = env.run_hook("claude", &claude_pre_tool_use_payload(tool));
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stdout.is_empty(),
            "neutral Claude blocking hook must keep stdout empty"
        );

        let items = env.feed_list_json();
        let items = items.as_array().expect("array");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["surface"], "native_ui", "{tool}");
        assert_eq!(items[0]["status"], "pending", "{tool}");
        assert_eq!(items[0]["kind"], expected_kind, "{tool}");
    }
}

#[test]
fn claude_pre_tool_bridge_path_renders_updated_input() {
    for (tool, field, value) in [
        ("ExitPlanMode", "plan", "approved"),
        ("AskUserQuestion", "question", "clarified"),
    ] {
        let env = Env::new();
        if env.skip_if_sandboxed() {
            continue;
        }
        env.enrol("opus-policy", 10, "30s");
        env.write_heartbeat("opus-policy", Timestamp::now());

        let child = env.spawn_hook("claude", &claude_pre_tool_use_payload(tool));
        let request_id = env
            .poll_pending_request_id(Instant::now() + BRIDGE_ITEM_WAIT)
            .expect("bridge item should appear in feed");
        let answer = format!(r#"{{"choice":"allow","updatedInput":{{"{field}":"{value}"}}}}"#);
        let resolve = env.resolve(&request_id, &answer, "opus-policy", "hook-bridge");
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
        assert_eq!(decision["hookSpecificOutput"]["updatedInput"][field], value);
    }
}

// --- Codex PreToolUse blocking events ---

#[test]
fn codex_request_user_input_uses_native_ui_without_resolver() {
    let env = Env::new();
    let output = env.run_hook("codex", &codex_pre_tool_use_payload());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "neutral Codex blocking hook must keep stdout empty"
    );

    let items = env.feed_list_json();
    let items = items.as_array().expect("array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["surface"], "native_ui");
    assert_eq!(items[0]["status"], "pending");
    assert_eq!(items[0]["kind"], "question");
}

#[test]
fn codex_request_user_input_bridge_path_renders_pre_tool_decision() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    env.enrol("opus-policy", 10, "30s");
    env.write_heartbeat("opus-policy", Timestamp::now());

    let child = env.spawn_hook("codex", &codex_pre_tool_use_payload());
    let request_id = env
        .poll_pending_request_id(Instant::now() + BRIDGE_ITEM_WAIT)
        .expect("bridge item should appear in feed");
    let resolve = env.resolve(
        &request_id,
        r#"{"choice":"allow","updatedInput":{"answer":"clarified"}}"#,
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
        decision["hookSpecificOutput"]["updatedInput"]["answer"],
        "clarified"
    );
}

// --- Claude lifecycle and install/uninstall ---

#[test]
fn claude_compaction_bracket_closers_clear_head() {
    for (session_id, closer, expect_running, expect_event_count) in [
        (
            "sess-claude-compact",
            json!({
                "hook_event_name": "SessionStart",
                "session_id": "sess-claude-compact",
                "source": "compact",
            }),
            true,
            None,
        ),
        (
            "sess-claude-pretool-close",
            json!({
                "hook_event_name": "PreToolUse",
                "session_id": "sess-claude-pretool-close",
                "tool_name": "Read",
            }),
            false,
            Some(3),
        ),
    ] {
        let env = Env::new();
        run_claude_lifecycle(
            &env,
            json!({
                "hook_event_name": "UserPromptSubmit",
                "session_id": session_id,
                "prompt": "continue the turn",
            }),
        );
        run_claude_lifecycle(
            &env,
            json!({
                "hook_event_name": "PreCompact",
                "session_id": session_id,
            }),
        );
        run_claude_lifecycle(&env, closer);

        if let Some(count) = expect_event_count {
            assert_eq!(
                lifecycle_event_count(&env),
                count,
                "the non-mutating PreToolUse must be durable when it closes a compaction bracket"
            );
        }
        let parsed = env.snapshot_json();
        let agent = &parsed["agents"][0];
        assert_eq!(agent["compaction_count"], 1);
        assert!(
            agent.get("compacting_since").is_none_or(Value::is_null),
            "compacting head should be cleared: {agent:?}"
        );
        if expect_running {
            assert_eq!(agent["status"], "running");
            assert_eq!(agent["phase"], "reasoning");
        }
    }
}

#[cfg(unix)]
fn fake_agent_bin_dir(names: &[&str]) -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::TempDir::new().expect("fake agent bin dir");
    for name in names {
        let path = dir.path().join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write fake agent");
        let mut perms = std::fs::metadata(&path)
            .expect("fake agent metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod fake agent");
    }
    dir
}

#[cfg(unix)]
#[test]
fn hooks_install_and_uninstall_no_arg_round_trips_detected_agents() {
    let env = Env::new();
    let bin_dir = fake_agent_bin_dir(&["claude", "codex"]);

    let install = env
        .rimz()
        .env("PATH", bin_dir.path())
        .args(["hooks", "install"])
        .output()
        .expect("spawn install");
    assert!(
        install.status.success(),
        "install stderr: {}",
        String::from_utf8_lossy(&install.stderr)
    );
    let reports: Value = serde_json::from_slice(&install.stdout).expect("install reports json");
    let reports = reports.as_array().expect("array report");
    let agents = reports
        .iter()
        .filter_map(|report| report["agent"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(agents, vec!["claude", "codex"]);
    assert!(env.agent_hooks_installed("claude"));
    assert!(env.agent_hooks_installed("codex"));

    let empty_path = fake_agent_bin_dir(&[]);
    let uninstall = env
        .rimz()
        .env("PATH", empty_path.path())
        .args(["hooks", "uninstall"])
        .output()
        .expect("spawn uninstall");
    assert!(
        uninstall.status.success(),
        "uninstall stderr: {}",
        String::from_utf8_lossy(&uninstall.stderr)
    );
    let reports: Value = serde_json::from_slice(&uninstall.stdout).expect("uninstall reports json");
    let agents = reports
        .as_array()
        .expect("array report")
        .iter()
        .filter_map(|report| report["agent"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(agents, vec!["claude", "codex"]);
    assert!(!env.agent_hooks_installed("claude"));
    assert!(!env.agent_hooks_installed("codex"));
}

/// The statusline feed passes the JSON through to the wrapped command verbatim
/// and forwards its stdout, so the user's rendering is unaffected.
#[test]
fn statusline_feed_passes_json_through_to_wrapped_command() {
    let env = Env::new();
    let claude_settings = env.agent_config_path("claude");
    std::fs::create_dir_all(claude_settings.parent().unwrap()).unwrap();
    // Wrap `cat`, which echoes the JSON it receives on stdin straight back.
    std::fs::write(
        &claude_settings,
        r#"{ "statusLine": { "type": "command", "command": "cat" } }"#,
    )
    .unwrap();
    env.install_agent_hooks("claude");

    let payload = r#"{"session_id":"sess-1","model":{"id":"claude-opus-4-8"}}"#;
    let out = env.run_statusline_feed("claude", payload);
    assert!(
        out.status.success(),
        "feed stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        payload,
        "wrapped command's stdout must be forwarded verbatim"
    );
}

/// With no wrapped command, the feed prints nothing (Claude falls back to its
/// built-in statusline), captures the per-session context sidecar, and folds it
/// onto the session row once lifecycle creates that row.
#[test]
fn statusline_feed_with_no_wrap_captures_context_and_folds_snapshot() {
    let env = Env::new();
    env.install_agent_hooks("claude");

    let payload = r#"{
        "session_id": "sess-ctx",
        "model": { "id": "claude-opus-4-8", "display_name": "Opus" },
        "context_window": { "used_percentage": 42 },
        "cost": { "total_cost_usd": 0.5 }
    }"#;
    let out = env.run_statusline_feed("claude", payload);
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty(),
        "no wrap means empty stdout, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    let contexts = env.agent_contexts();
    assert_eq!(contexts.len(), 1, "the session's context was captured");
    let record = &contexts[0];
    assert_eq!(record.kind, "claude");
    assert_eq!(record.agent_id, "sess-ctx");
    assert_eq!(record.context.model_display_name.as_deref(), Some("Opus"));
    assert_eq!(
        record.context.tokens.as_ref().unwrap().used_percentage,
        Some(42)
    );

    let start = env.run_installed_hook(
        "claude",
        r#"{ "hook_event_name": "SessionStart", "session_id": "sess-ctx", "permission_mode": "default" }"#,
    );
    assert!(start.status.success());

    let snapshot = env.snapshot_json();
    let agents = snapshot["agents"].as_array().expect("agents array");
    let agent = agents
        .iter()
        .find(|a| a["agent_id"] == "sess-ctx")
        .expect("session agent present");
    assert_eq!(agent["context"]["model_display_name"], "Opus");
    assert_eq!(agent["context"]["tokens"]["used_percentage"], 42);
}

/// The `--subagent` feed harvests every task in a `subagentStatusLine` payload
/// into one per-child sidecar, keyed by the task id, and emits nothing when no
/// wrap is configured (Claude renders its own child rows).
#[test]
fn subagent_statusline_feed_writes_one_sidecar_per_task() {
    let env = Env::new();
    env.install_agent_hooks("claude");

    let payload = r#"{
        "columns": 80,
        "tasks": [
            {
                "id": "child-1",
                "type": "Explore",
                "status": "running",
                "description": "locate the render seam",
                "startTime": 1700000000,
                "tokenCount": 12400
            },
            {
                "id": "child-2",
                "type": "review",
                "description": "audit the trust hash",
                "startTime": 1700000055,
                "tokenCount": 3100
            }
        ]
    }"#;
    let out = env.run_subagent_statusline_feed("claude", payload);
    assert!(
        out.status.success(),
        "feed stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "no wrap means empty stdout, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    let mut records = env.subagent_contexts();
    records.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
    assert_eq!(records.len(), 2, "one sidecar per task");
    assert_eq!(records[0].agent_id, "child-1");
    assert_eq!(
        records[0].context.description.as_deref(),
        Some("locate the render seam")
    );
    assert_eq!(records[0].context.token_count, Some(12_400));
    assert!(records[0].context.started_at.is_some());
    assert_eq!(records[1].agent_id, "child-2");
    assert_eq!(records[1].context.token_count, Some(3_100));
}

/// Build the `rimz hooks feed --source codex` command with `RIMZ_CODEX_BIN`
/// pointed at `codex_bin`, mirroring an installed hook. The detached
/// `rimz codex refresh-context` child inherits this env, so it spawns
/// `codex_bin app-server` for its read-only enrichment.
fn codex_hook_with_app_server(env: &Env, codex_bin: &std::path::Path) -> Command {
    let mut cmd = env.rimz();
    cmd.args(["hooks", "feed", "--source", "codex"])
        .env("RIMZ_AGENT_PID", std::process::id().to_string())
        .env("RIMZ_CODEX_BIN", codex_bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Absolute path to the built `codex app-server` stub fixture.
fn codex_appserver_stub() -> std::path::PathBuf {
    Command::cargo_bin("codex-appserver-stub")
        .expect("cargo-bin stub")
        .get_program()
        .to_owned()
        .into()
}

/// A Codex turn boundary spawns a detached refresh that reads the app-server
/// (here, a stub) and writes the session's context sidecar with the rich
/// details Claude gets from its statusline: rate-limit windows, model display
/// name, and version. The context gauge (`tokens`) stays `None` — the
/// app-server exposes no read-only token usage, so that stays rollout-sourced.
#[test]
fn codex_turn_boundary_refreshes_context_sidecar_from_app_server() {
    let env = Env::new();
    let payload = serde_json::to_string(&json!({
        "hook_event_name": "SessionStart",
        "session_id": "sess-codex-rt",
        "approval_policy": "ask",
        "model": "gpt-5.5-codex",
    }))
    .expect("payload");

    let cmd = codex_hook_with_app_server(&env, &codex_appserver_stub());
    let out = env
        .spawn_payload(cmd, &payload)
        .wait_with_output()
        .expect("wait hook");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty(), "lifecycle hook is silent");

    // The refresh is detached, so the sidecar lands after the hook returns.
    let deadline = Instant::now() + Duration::from_secs(10);
    let record = loop {
        if let Some(record) = env
            .agent_contexts()
            .into_iter()
            .find(|record| record.agent_id == "sess-codex-rt")
        {
            break record;
        }
        assert!(
            Instant::now() < deadline,
            "codex context sidecar was never written"
        );
        std::thread::sleep(Duration::from_millis(50));
    };

    assert_eq!(record.kind, "codex");
    assert_eq!(record.context.source, "codex");
    let limits = record.context.rate_limits.expect("rate limits present");
    // Wire order preserved: primary (300 min) then secondary (10080 min).
    assert_eq!(limits.windows[0].duration_mins, Some(300));
    assert_eq!(limits.windows[0].used_percentage, Some(42));
    assert_eq!(limits.windows[1].duration_mins, Some(10080));
    assert_eq!(limits.windows[1].used_percentage, Some(7));
    assert_eq!(
        record.context.model_display_name.as_deref(),
        Some("GPT-5.5 Codex")
    );
    assert_eq!(
        record.context.effort, None,
        "model/list defaultReasoningEffort is not the session's actual effort"
    );
    assert_eq!(record.context.agent_version.as_deref(), Some("9.9.9"));
    assert!(
        record.context.tokens.is_none(),
        "no read-only token source for Codex — the gauge stays rollout-sourced"
    );
}

#[test]
fn codex_stop_over_error_rollout_writes_turn_error_sidecar() {
    let env = Env::new();
    let session_id = "sess-codex-error";
    let sessions = env.home_root.join("codex-sessions");
    let day = sessions.join("2026").join("06").join("11");
    std::fs::create_dir_all(&day).expect("mkdir codex sessions");
    std::fs::write(
        day.join(format!("rollout-2026-06-11T07-18-00-{session_id}.jsonl")),
        json!({
            "timestamp": "2026-06-11T07:18:00.000Z",
            "type": "event_msg",
            "payload": {
                "type": "turn_error",
                "message": "You've hit your usage limit",
                "codexErrorInfo": "usageLimitExceeded"
            }
        })
        .to_string()
            + "\n",
    )
    .expect("write rollout");

    let payload = serde_json::to_string(&json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "model": "gpt-5.5-codex",
    }))
    .expect("payload");
    let mut cmd = env.hook_command("codex");
    cmd.env("RIMZ_CODEX_SESSIONS", &sessions)
        .env("RIMZ_CODEX_BIN", "/nonexistent/codex-binary-xyz");
    let out = env
        .spawn_payload(cmd, &payload)
        .wait_with_output()
        .expect("wait hook");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty(), "lifecycle hook is silent");

    let record = env
        .agent_contexts()
        .into_iter()
        .find(|record| record.agent_id == session_id)
        .expect("turn-error sidecar");
    let marker = record.context.turn_error.expect("turn-error marker");
    assert_eq!(marker.class, rimz::agents::TurnErrorClass::PausedRateLimit);
    assert_eq!(
        marker.at,
        "2026-06-11T07:18:00.000Z".parse::<Timestamp>().unwrap()
    );
    assert_eq!(marker.label.as_deref(), Some("You've hit your usage limit"));

    let snapshot = env.snapshot_json();
    let agent = snapshot["agents"]
        .as_array()
        .expect("agents array")
        .iter()
        .find(|agent| agent["agent_id"].as_str() == Some(session_id))
        .expect("codex agent in snapshot");
    assert_eq!(
        agent["status"].as_str(),
        Some("failed"),
        "Stop over rollout turn_error must not reduce as success"
    );
}

/// The sidebar's idle/account refresh path calls the uniform hidden
/// `agents refresh-usage --kind codex` helper, which reads codex's app-server
/// (its realtime channel, pollable while idle) and merges the windows into the
/// shared provider cache.
#[test]
fn codex_rate_limit_refresh_merges_account_cache_from_app_server() {
    let env = Env::new();
    let out = env
        .rimz()
        .env("RIMZ_CODEX_BIN", codex_appserver_stub())
        .args([
            "agents",
            "refresh-usage",
            "--kind",
            "codex",
            "--workspace-id",
            env.workspace_id.as_str(),
        ])
        .output()
        .expect("spawn agents refresh-usage codex");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let cache_path = env.runtime_paths().shared_rate_limits_path();
    let cache: Value = serde_json::from_slice(&std::fs::read(cache_path).expect("rate cache"))
        .expect("rate cache json");
    assert_eq!(
        cache["windows"]["codex"]["windows"][0]["used_percentage"], 42,
        "the short window comes from the app-server primary window"
    );
    assert_eq!(
        cache["windows"]["codex"]["windows"][1]["used_percentage"], 7,
        "the long window comes from the app-server secondary window"
    );
    let credits_path = env.runtime_paths().shared_credits_path();
    let credits: Value =
        serde_json::from_slice(&std::fs::read(credits_path).expect("credits cache"))
            .expect("credits cache json");
    assert_eq!(
        credits["entries"]["codex"]["extra_credits"]["known"]["remaining_usd"], 18.5,
        "the app-server credits balance lands in the shared credits cache"
    );
}
