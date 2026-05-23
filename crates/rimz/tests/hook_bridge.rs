//! Out-of-process integration tests for `rimz hooks feed` exercising the
//! `Surface::Bridge` wiring landed in M1. Each test spawns a real `rimz`
//! binary; XDG roots are scoped under a tempdir so allowlist, state, and
//! runtime files don't escape.

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use jiff::Timestamp;
use rimz::WorkspaceId;
use rimz::schema::heartbeat::ResolverHeartbeat;
use serde_json::{Value, json};
use tempfile::TempDir;

struct Env {
    home: TempDir,
    workspace_id: WorkspaceId,
    runtime_root: PathBuf,
}

impl Env {
    fn new() -> Self {
        let home = TempDir::new().expect("tempdir");
        let project_root = canonical(home.path());
        let workspace_id = WorkspaceId::from_project_root(&project_root);
        let runtime_root = project_root.join("runtime");
        let state_root = project_root.join("state");
        let config_root = project_root.join("config");
        for d in [&runtime_root, &state_root, &config_root] {
            std::fs::create_dir_all(d).expect("mkdir env root");
        }
        let env = Env {
            home,
            workspace_id,
            runtime_root,
        };
        // Ensure heartbeat dir exists ahead of time so writes don't race the
        // ledger creating it.
        std::fs::create_dir_all(env.heartbeat_dir()).expect("mkdir heartbeat");
        env
    }

    fn project_root(&self) -> PathBuf {
        canonical(self.home.path())
    }

    fn state_root(&self) -> PathBuf {
        self.project_root().join("state")
    }

    fn config_root(&self) -> PathBuf {
        self.project_root().join("config")
    }

    fn heartbeat_dir(&self) -> PathBuf {
        self.runtime_root
            .join("rimz")
            .join(self.workspace_id.as_str())
            .join("heartbeat")
    }

    fn sock_dir(&self) -> PathBuf {
        self.runtime_root
            .join("rimz")
            .join(self.workspace_id.as_str())
            .join("sock")
    }

    fn rimz(&self) -> StdCommand {
        let mut cmd = StdCommand::cargo_bin("rimz").expect("cargo-bin");
        cmd.env("XDG_STATE_HOME", self.state_root())
            .env("XDG_RUNTIME_DIR", &self.runtime_root)
            .env("XDG_CONFIG_HOME", self.config_root())
            .env("HOME", self.project_root())
            .env_remove("RUST_LOG")
            .current_dir(self.project_root())
            .args(["--root", &self.project_root().display().to_string()]);
        cmd
    }

    fn enrol(&self, id: &str, order: u32, budget: &str) {
        let status = self
            .rimz()
            .args([
                "resolver",
                "add",
                id,
                "--order",
                &order.to_string(),
                "--budget",
                budget,
            ])
            .status()
            .expect("spawn resolver add");
        assert!(status.success(), "resolver add `{id}` failed");
    }

    fn write_heartbeat(&self, id: &str, last_seen: Timestamp) {
        let resolver_id = id.parse().expect("resolver id parse");
        let mut hb = ResolverHeartbeat::new(self.workspace_id.clone(), resolver_id);
        hb.last_seen = last_seen;
        let path = self.heartbeat_dir().join(format!("resolver.{id}.json"));
        std::fs::write(&path, serde_json::to_vec(&hb).expect("serialize hb"))
            .expect("write heartbeat");
    }
}

fn canonical(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

fn permission_payload() -> String {
    serde_json::to_string(&json!({
        "hook_event_name": "PermissionRequest",
        "tool_name": "Bash",
        "tool_input": { "command": "echo hi" }
    }))
    .expect("payload")
}

fn poll_pending_request_id(env: &Env, until: Instant) -> Option<String> {
    while Instant::now() < until {
        let output = env
            .rimz()
            .args(["feed", "list", "--json"])
            .output()
            .expect("feed list");
        if output.status.success() {
            let parsed: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
            if let Some(arr) = parsed.as_array() {
                for item in arr {
                    if item["status"] == "pending" {
                        return item["request_id"].as_str().map(ToOwned::to_owned);
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

fn show_feed_item(env: &Env, request_id: &str) -> Value {
    let output = env
        .rimz()
        .args(["feed", "show", request_id, "--json"])
        .output()
        .expect("feed show");
    assert!(
        output.status.success(),
        "feed show failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("feed item json")
}

#[test]
fn hooks_install_is_discoverable_but_feed_entrypoint_is_hidden() {
    let top = StdCommand::cargo_bin("rimz")
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

    let hooks = StdCommand::cargo_bin("rimz")
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
    let mut child = env
        .rimz()
        .args(["hooks", "feed", "--source", "claude"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hooks");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(permission_payload().as_bytes())
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait child");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    assert_eq!(stdout.trim(), "{}", "neutral payload expected");

    let list = env
        .rimz()
        .args(["feed", "list", "--json"])
        .output()
        .expect("feed list");
    let parsed: Value = serde_json::from_slice(&list.stdout).expect("json");
    let items = parsed.as_array().expect("array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["surface"], "native_ui");
    assert_eq!(items[0]["status"], "pending");
}

#[test]
fn hook_with_stale_heartbeat_stays_native_ui() {
    let env = Env::new();
    env.enrol("opus-policy", 10, "30s");
    env.write_heartbeat("opus-policy", Timestamp::now() - Duration::from_secs(60));
    let mut child = env
        .rimz()
        .args(["hooks", "feed", "--source", "claude"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hooks");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(permission_payload().as_bytes())
        .unwrap();
    let output = child.wait_with_output().expect("wait");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "{}",
        "fresh heartbeat is required to engage bridge"
    );
    let list = env
        .rimz()
        .args(["feed", "list", "--json"])
        .output()
        .expect("list");
    let parsed: Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(parsed[0]["surface"], "native_ui");
}

#[test]
fn hook_with_resolver_chain_rejects_out_of_turn_and_advances_on_abstain() {
    let env = Env::new();
    if common::af_unix_bind_sandboxed(&{
        std::fs::create_dir_all(env.sock_dir()).unwrap();
        env.sock_dir()
    }) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return;
    }
    env.enrol("opus-policy", 10, "30s");
    env.enrol("slack-on-call", 20, "5m");
    env.write_heartbeat("opus-policy", Timestamp::now());
    env.write_heartbeat("slack-on-call", Timestamp::now());

    let mut child = env
        .rimz()
        .args(["hooks", "feed", "--source", "claude"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hooks");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(permission_payload().as_bytes())
        .unwrap();

    let request_id = poll_pending_request_id(&env, Instant::now() + Duration::from_secs(5))
        .expect("bridge item should appear in feed");

    let initial = show_feed_item(&env, &request_id);
    assert_eq!(initial["chain"][0]["resolver_id"], "opus-policy");
    assert_eq!(initial["chain"][0]["state"], "active");
    assert_eq!(initial["chain"][1]["resolver_id"], "slack-on-call");
    assert_eq!(initial["chain"][1]["state"], "queued");
    assert_eq!(initial["chain_active_resolver"], "opus-policy");
    assert!(initial["chain_active_until"].is_string());

    let out_of_turn = env
        .rimz()
        .args([
            "feed",
            "resolve",
            &request_id,
            "--decision",
            r#"{"choice":"allow"}"#,
            "--resolver-id",
            "slack-on-call",
            "--method",
            "hook-bridge",
        ])
        .output()
        .expect("spawn out-of-turn resolve");
    assert!(
        !out_of_turn.status.success(),
        "queued resolver must not answer before it is active"
    );

    let abstain = env
        .rimz()
        .args([
            "feed",
            "abstain",
            &request_id,
            "--resolver-id",
            "opus-policy",
            "--reason",
            "outside policy",
        ])
        .output()
        .expect("spawn abstain");
    assert!(
        abstain.status.success(),
        "abstain failed: {}",
        String::from_utf8_lossy(&abstain.stderr)
    );

    let advanced = show_feed_item(&env, &request_id);
    assert_eq!(advanced["chain"][0]["state"], "abstained");
    assert_eq!(advanced["chain"][1]["state"], "active");
    assert_eq!(advanced["chain_active_resolver"], "slack-on-call");
    assert!(advanced["chain_active_until"].is_string());

    let resolve = env
        .rimz()
        .args([
            "feed",
            "resolve",
            &request_id,
            "--decision",
            r#"{"choice":"allow"}"#,
            "--resolver-id",
            "slack-on-call",
            "--method",
            "hook-bridge",
        ])
        .output()
        .expect("spawn resolve");
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

    let resolved = show_feed_item(&env, &request_id);
    assert_eq!(resolved["status"], "resolved");
    assert_eq!(resolved["chain"][0]["state"], "abstained");
    assert_eq!(resolved["chain"][1]["state"], "answered");
    assert!(resolved["chain_active_resolver"].is_null());
    assert!(resolved["chain_active_until"].is_null());
}

#[test]
fn hook_with_fresh_resolver_engages_bridge_and_resolves() {
    let env = Env::new();
    if common::af_unix_bind_sandboxed(&{
        std::fs::create_dir_all(env.sock_dir()).unwrap();
        env.sock_dir()
    }) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return;
    }
    env.enrol("opus-policy", 10, "30s");
    env.write_heartbeat("opus-policy", Timestamp::now());

    let mut child = env
        .rimz()
        .args(["hooks", "feed", "--source", "claude"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hooks");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(permission_payload().as_bytes())
        .unwrap();

    let request_id = poll_pending_request_id(&env, Instant::now() + Duration::from_secs(5))
        .expect("bridge item should appear in feed");

    let resolve = env
        .rimz()
        .args([
            "feed",
            "resolve",
            &request_id,
            "--decision",
            r#"{"choice":"allow"}"#,
            "--resolver-id",
            "opus-policy",
            "--method",
            "hook-bridge",
        ])
        .output()
        .expect("spawn resolve");
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

fn codex_permission_payload() -> String {
    serde_json::to_string(&json!({
        "hook_event_name": "PermissionRequest",
        "tool_name": "shell",
        "command": ["echo", "hi"],
    }))
    .expect("payload")
}

#[test]
fn codex_hook_with_no_allowlisted_resolver_stays_native_ui() {
    let env = Env::new();
    let mut child = env
        .rimz()
        .args(["hooks", "feed", "--source", "codex"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hooks");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(codex_permission_payload().as_bytes())
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait child");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    assert!(
        stdout.is_empty(),
        "Codex neutral must be empty stdout, got: {stdout:?}"
    );

    let list = env
        .rimz()
        .args(["feed", "list", "--json"])
        .output()
        .expect("feed list");
    let parsed: Value = serde_json::from_slice(&list.stdout).expect("json");
    let items = parsed.as_array().expect("array");
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
    let mut child = env
        .rimz()
        .args(["hooks", "feed", "--source", "codex"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hooks");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(codex_permission_payload().as_bytes())
        .unwrap();
    let output = child.wait_with_output().expect("wait");
    assert!(output.status.success());
    assert!(
        output.stdout.is_empty(),
        "stale heartbeat must still emit Codex neutral (empty)"
    );
    let list = env
        .rimz()
        .args(["feed", "list", "--json"])
        .output()
        .expect("list");
    let parsed: Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(parsed[0]["surface"], "native_ui");
}

#[test]
fn codex_hook_with_fresh_resolver_engages_bridge_and_resolves() {
    let env = Env::new();
    if common::af_unix_bind_sandboxed(&{
        std::fs::create_dir_all(env.sock_dir()).unwrap();
        env.sock_dir()
    }) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return;
    }
    env.enrol("opus-policy", 10, "30s");
    env.write_heartbeat("opus-policy", Timestamp::now());

    let mut child = env
        .rimz()
        .args(["hooks", "feed", "--source", "codex"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hooks");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(codex_permission_payload().as_bytes())
        .unwrap();

    let request_id = poll_pending_request_id(&env, Instant::now() + Duration::from_secs(5))
        .expect("bridge item should appear in feed");

    let resolve = env
        .rimz()
        .args([
            "feed",
            "resolve",
            &request_id,
            "--decision",
            r#"{"choice":"allow"}"#,
            "--resolver-id",
            "opus-policy",
            "--method",
            "hook-bridge",
        ])
        .output()
        .expect("spawn resolve");
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
    assert_eq!(decision, json!({ "decision": "allow" }));
    // Reserved-key invariant — Codex must never see Claude-shaped fields.
    assert!(decision.get("hookSpecificOutput").is_none());
    assert!(decision.get("updatedInput").is_none());
    assert!(decision.get("interrupt").is_none());
}

#[test]
fn codex_hook_bridge_cap_timeout_emits_neutral() {
    let env = Env::new();
    if common::af_unix_bind_sandboxed(&{
        std::fs::create_dir_all(env.sock_dir()).unwrap();
        env.sock_dir()
    }) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return;
    }
    env.enrol("opus-policy", 10, "30s");
    env.write_heartbeat("opus-policy", Timestamp::now());

    let mut child = env
        .rimz()
        .env("RIMZ_HOOK_CAP_MILLIS", "200")
        .args(["hooks", "feed", "--source", "codex"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hooks");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(codex_permission_payload().as_bytes())
        .unwrap();

    let output = child.wait_with_output().expect("wait child");
    assert!(
        output.status.success(),
        "hook stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "Codex cap-elapsed should emit empty stdout (neutral)"
    );

    let list = env
        .rimz()
        .args(["feed", "list", "--json"])
        .output()
        .expect("list");
    let parsed: Value = serde_json::from_slice(&list.stdout).unwrap();
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
    let mut child = env
        .rimz()
        .args(["hooks", "feed", "--source", "codex"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hooks");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let output = child.wait_with_output().expect("wait");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty(), "lifecycle hook is silent");

    // The lifecycle event must land in the snapshot's agents rollup.
    let snap = env
        .rimz()
        .args([
            "sidebar",
            "snapshot",
            "--workspace-id",
            env.workspace_id.as_str(),
            "--json",
        ])
        .output()
        .expect("snapshot");
    assert!(
        snap.status.success(),
        "snapshot failed: {}",
        String::from_utf8_lossy(&snap.stderr)
    );
    let parsed: Value = serde_json::from_slice(&snap.stdout).expect("snapshot json");
    let agents = parsed["agents"].as_array().expect("agents array");
    assert_eq!(agents.len(), 1, "exactly one agent rolled up: {agents:?}");
    assert_eq!(agents[0]["kind"], "codex");
    assert_eq!(agents[0]["agent_id"], "sess-codex-01");
    assert_eq!(agents[0]["status"], "running");
    assert_eq!(agents[0]["mode"], "interactive");
    assert_eq!(agents[0]["worktree_branch"], "feature-x");
}

#[test]
fn codex_install_uninstall_cli_round_trips_into_codex_config() {
    let env = Env::new();
    let codex_config = env.project_root().join(".codex").join("config.toml");

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
    assert_eq!(report["telemetry"], false);
    let events = report["installed_events"].as_array().expect("events");
    let names: Vec<&str> = events.iter().filter_map(Value::as_str).collect();
    assert!(names.contains(&"SessionStart"));
    assert!(names.contains(&"PermissionRequest"));

    assert!(
        codex_config.exists(),
        "config file should exist after install"
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
}

#[test]
fn codex_session_start_with_never_policy_observes_bypass_mode() {
    let env = Env::new();
    let payload = serde_json::to_string(&json!({
        "hook_event_name": "SessionStart",
        "session_id": "sess-codex-bypass",
        "approval_policy": "never",
    }))
    .expect("payload");
    let mut child = env
        .rimz()
        .args(["hooks", "feed", "--source", "codex"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hooks");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let output = child.wait_with_output().expect("wait");
    assert!(output.status.success());

    let snap = env
        .rimz()
        .args([
            "sidebar",
            "snapshot",
            "--workspace-id",
            env.workspace_id.as_str(),
            "--json",
        ])
        .output()
        .expect("snapshot");
    assert!(snap.status.success());
    let parsed: Value = serde_json::from_slice(&snap.stdout).expect("snapshot json");
    let agents = parsed["agents"].as_array().expect("agents array");
    assert_eq!(agents[0]["mode"], "bypass");
}

#[test]
fn hook_bridge_cap_timeout_emits_neutral() {
    let env = Env::new();
    if common::af_unix_bind_sandboxed(&{
        std::fs::create_dir_all(env.sock_dir()).unwrap();
        env.sock_dir()
    }) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return;
    }
    env.enrol("opus-policy", 10, "30s");
    env.write_heartbeat("opus-policy", Timestamp::now());

    let mut child = env
        .rimz()
        .env("RIMZ_HOOK_CAP_MILLIS", "200")
        .args(["hooks", "feed", "--source", "claude"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hooks");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(permission_payload().as_bytes())
        .unwrap();

    let output = child.wait_with_output().expect("wait child");
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

    let list = env
        .rimz()
        .args(["feed", "list", "--json"])
        .output()
        .expect("list");
    let parsed: Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(parsed[0]["status"], "timed_out");
    assert_eq!(parsed[0]["surface"], "bridge");
}

// --- Claude PreToolUse blocking events ---
//
// `ExitPlanMode` and `AskUserQuestion` are PreToolUse blocking hooks. The
// agent expects the decision to carry `updatedInput`; the neutral payload
// stays `{}` and the agent's own UI is the answer surface.

fn claude_pre_tool_use_payload(tool_name: &str) -> String {
    serde_json::to_string(&json!({
        "hook_event_name": "PreToolUse",
        "tool_name": tool_name,
        "tool_input": { "plan": "ship it" },
        "session_id": "sess-claude-pretool",
    }))
    .expect("payload")
}

#[test]
fn claude_exit_plan_mode_default_path_pushes_plan_approval() {
    let env = Env::new();
    let mut child = env
        .rimz()
        .args(["hooks", "feed", "--source", "claude"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hooks");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(claude_pre_tool_use_payload("ExitPlanMode").as_bytes())
        .unwrap();
    let output = child.wait_with_output().expect("wait");
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

    let list = env
        .rimz()
        .args(["feed", "list", "--json"])
        .output()
        .expect("list");
    let parsed: Value = serde_json::from_slice(&list.stdout).unwrap();
    let items = parsed.as_array().expect("array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["surface"], "native_ui");
    assert_eq!(items[0]["status"], "pending");
    assert_eq!(items[0]["kind"], "plan_approval");
}

#[test]
fn claude_ask_user_question_default_path_pushes_question() {
    let env = Env::new();
    let mut child = env
        .rimz()
        .args(["hooks", "feed", "--source", "claude"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hooks");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(claude_pre_tool_use_payload("AskUserQuestion").as_bytes())
        .unwrap();
    let output = child.wait_with_output().expect("wait");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "{}");

    let list = env
        .rimz()
        .args(["feed", "list", "--json"])
        .output()
        .expect("list");
    let parsed: Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(parsed[0]["kind"], "question");
    assert_eq!(parsed[0]["surface"], "native_ui");
}

#[test]
fn claude_exit_plan_mode_bridge_path_renders_updated_input() {
    let env = Env::new();
    if common::af_unix_bind_sandboxed(&{
        std::fs::create_dir_all(env.sock_dir()).unwrap();
        env.sock_dir()
    }) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return;
    }
    env.enrol("opus-policy", 10, "30s");
    env.write_heartbeat("opus-policy", Timestamp::now());

    let mut child = env
        .rimz()
        .args(["hooks", "feed", "--source", "claude"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hooks");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(claude_pre_tool_use_payload("ExitPlanMode").as_bytes())
        .unwrap();

    let request_id = poll_pending_request_id(&env, Instant::now() + Duration::from_secs(5))
        .expect("bridge item should appear in feed");

    let resolve = env
        .rimz()
        .args([
            "feed",
            "resolve",
            &request_id,
            "--decision",
            r#"{"choice":"allow","updatedInput":{"plan":"approved"}}"#,
            "--resolver-id",
            "opus-policy",
            "--method",
            "hook-bridge",
        ])
        .output()
        .expect("spawn resolve");
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
    if common::af_unix_bind_sandboxed(&{
        std::fs::create_dir_all(env.sock_dir()).unwrap();
        env.sock_dir()
    }) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return;
    }
    env.enrol("opus-policy", 10, "30s");
    env.write_heartbeat("opus-policy", Timestamp::now());

    let mut child = env
        .rimz()
        .args(["hooks", "feed", "--source", "claude"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hooks");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(claude_pre_tool_use_payload("AskUserQuestion").as_bytes())
        .unwrap();

    let request_id = poll_pending_request_id(&env, Instant::now() + Duration::from_secs(5))
        .expect("bridge item should appear in feed");

    let resolve = env
        .rimz()
        .args([
            "feed",
            "resolve",
            &request_id,
            "--decision",
            r#"{"choice":"allow","updatedInput":{"question":"clarified"}}"#,
            "--resolver-id",
            "opus-policy",
            "--method",
            "hook-bridge",
        ])
        .output()
        .expect("spawn resolve");
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
    let mut child = env
        .rimz()
        .args(["hooks", "feed", "--source", "claude"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hooks");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let output = child.wait_with_output().expect("wait");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Claude's neutral payload is `{}` — emitted even for lifecycle hooks so
    // the agent always sees a well-formed JSON response.
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "{}");

    let snap = env
        .rimz()
        .args([
            "sidebar",
            "snapshot",
            "--workspace-id",
            env.workspace_id.as_str(),
            "--json",
        ])
        .output()
        .expect("snapshot");
    assert!(snap.status.success());
    let parsed: Value = serde_json::from_slice(&snap.stdout).expect("snapshot json");
    let agents = parsed["agents"].as_array().expect("agents array");
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["kind"], "claude");
    assert_eq!(agents[0]["agent_id"], "sess-claude-01");
    assert_eq!(agents[0]["status"], "running");
    assert_eq!(agents[0]["mode"], "interactive");
    assert_eq!(agents[0]["worktree_branch"], "feature-x");
}

#[test]
fn claude_session_start_with_bypass_permissions_observes_bypass_mode() {
    let env = Env::new();
    let payload = serde_json::to_string(&json!({
        "hook_event_name": "SessionStart",
        "session_id": "sess-claude-bypass",
        "permission_mode": "bypassPermissions",
    }))
    .expect("payload");
    let mut child = env
        .rimz()
        .args(["hooks", "feed", "--source", "claude"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hooks");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let output = child.wait_with_output().expect("wait");
    assert!(output.status.success());

    let snap = env
        .rimz()
        .args([
            "sidebar",
            "snapshot",
            "--workspace-id",
            env.workspace_id.as_str(),
            "--json",
        ])
        .output()
        .expect("snapshot");
    let parsed: Value = serde_json::from_slice(&snap.stdout).expect("snapshot json");
    assert_eq!(parsed["agents"][0]["mode"], "bypass");
}

#[test]
fn claude_install_uninstall_cli_round_trips_into_settings_json() {
    let env = Env::new();
    let claude_settings = env.project_root().join(".claude").join("settings.json");

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
    assert_eq!(report["telemetry"], false);
    let events = report["installed_events"].as_array().expect("events");
    let names: Vec<&str> = events.iter().filter_map(Value::as_str).collect();
    assert!(names.contains(&"SessionStart"));
    assert!(names.contains(&"PermissionRequest"));
    assert!(names.contains(&"PreToolUse:ExitPlanMode"));
    assert!(names.contains(&"PreToolUse:AskUserQuestion"));

    assert!(
        claude_settings.exists(),
        "settings file should exist after install"
    );
    let on_disk: Value =
        serde_json::from_slice(&std::fs::read(&claude_settings).unwrap()).expect("settings json");
    // PreToolUse block has both managed matchers.
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
