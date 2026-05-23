//! Chain advancement integration tests. These exercise the M3.5 wiring
//! that hands off the active resolver mid-hook when its per-step budget
//! elapses or its heartbeat goes stale — the contract documented in
//! `docs/internals/resolvers.md`.
//!
//! Each test spawns a real `rimz hooks feed` subprocess, then drives the
//! ledger from the outside: emulate resolver heartbeats, watch the chain
//! advance, resolve from the second link, and assert the audit trail.

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

    fn events_log_path(&self) -> PathBuf {
        self.state_root()
            .join("rimz")
            .join(self.workspace_id.as_str())
            .join("events.log.jsonl")
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

fn poll_active_resolver(env: &Env, request_id: &str, expected: &str, until: Instant) -> bool {
    while Instant::now() < until {
        let output = env
            .rimz()
            .args(["feed", "show", request_id, "--json"])
            .output()
            .expect("feed show");
        if output.status.success() {
            let parsed: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
            if parsed["chain_active_resolver"] == expected {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Read every `events.log.jsonl` record via the public `Ledger::read_events`
/// API. The events log is length-framed; the harness goes through the
/// library to avoid hand-rolling the framing in test code.
fn read_events(env: &Env) -> Vec<rimz::EventEnvelope> {
    let state =
        rimz::StatePaths::under(env.workspace_id.clone(), &env.state_root()).expect("state paths");
    let runtime =
        rimz::RuntimePaths::under(env.workspace_id.clone(), &env.runtime_root).expect("runtime");
    let ledger = rimz::Ledger::open(state, runtime).expect("open ledger");
    ledger.read_events().expect("read events")
}

fn chain_elapse_reasons(env: &Env) -> Vec<String> {
    read_events(env)
        .into_iter()
        .filter(|e| e.method == "feed.chain_elapse")
        .filter_map(|e| {
            e.params
                .get("reason")
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned)
        })
        .collect()
}

fn skip_if_sandboxed(env: &Env) -> bool {
    std::fs::create_dir_all(env.sock_dir()).unwrap();
    if common::af_unix_bind_sandboxed(&env.sock_dir()) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return true;
    }
    false
}

#[test]
fn chain_advances_on_budget_elapse() {
    let env = Env::new();
    if skip_if_sandboxed(&env) {
        return;
    }
    // First resolver has a 1-second per-step budget; second is generous.
    env.enrol("opus-policy", 10, "1s");
    env.enrol("slack-on-call", 20, "30s");
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

    // Keep slack-on-call heartbeating fresh while we wait for the budget
    // elapse, so the loop's restat after the chain advance succeeds.
    let env_for_thread = env.workspace_id.clone();
    let runtime_root_for_thread = env.runtime_root.clone();
    let heartbeat_keepalive = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let path = runtime_root_for_thread
            .join("rimz")
            .join(env_for_thread.as_str())
            .join("heartbeat")
            .join("resolver.slack-on-call.json");
        while Instant::now() < deadline {
            let resolver_id = "slack-on-call".parse().expect("resolver id parse");
            let mut hb = ResolverHeartbeat::new(env_for_thread.clone(), resolver_id);
            hb.last_seen = Timestamp::now();
            let _ = std::fs::write(&path, serde_json::to_vec(&hb).expect("hb"));
            std::thread::sleep(Duration::from_millis(250));
        }
    });

    assert!(
        poll_active_resolver(
            &env,
            &request_id,
            "slack-on-call",
            Instant::now() + Duration::from_secs(5),
        ),
        "chain should advance to slack-on-call after the 1s budget elapses"
    );

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
    let _ = heartbeat_keepalive.join();
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

    let reasons = chain_elapse_reasons(&env);
    assert!(
        reasons.iter().any(|r| r == "budget_elapsed"),
        "expected feed.chain_elapse with reason=budget_elapsed, got {reasons:?}"
    );
}

#[test]
fn chain_advances_on_heartbeat_stale() {
    let env = Env::new();
    if skip_if_sandboxed(&env) {
        return;
    }
    // Generous per-step budgets — the trigger we want is heartbeat staleness.
    env.enrol("opus-policy", 10, "30s");
    env.enrol("slack-on-call", 20, "30s");
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

    // Age out opus-policy's heartbeat; keep slack-on-call alive.
    env.write_heartbeat("opus-policy", Timestamp::now() - Duration::from_secs(60));
    let env_ws = env.workspace_id.clone();
    let runtime_root = env.runtime_root.clone();
    let heartbeat_keepalive = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let path = runtime_root
            .join("rimz")
            .join(env_ws.as_str())
            .join("heartbeat")
            .join("resolver.slack-on-call.json");
        while Instant::now() < deadline {
            let resolver_id = "slack-on-call".parse().expect("resolver id parse");
            let mut hb = ResolverHeartbeat::new(env_ws.clone(), resolver_id);
            hb.last_seen = Timestamp::now();
            let _ = std::fs::write(&path, serde_json::to_vec(&hb).expect("hb"));
            std::thread::sleep(Duration::from_millis(250));
        }
    });

    assert!(
        poll_active_resolver(
            &env,
            &request_id,
            "slack-on-call",
            Instant::now() + Duration::from_secs(5),
        ),
        "chain should advance once opus-policy heartbeat is stale"
    );

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
    let _ = heartbeat_keepalive.join();
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

    let reasons = chain_elapse_reasons(&env);
    assert!(
        reasons.iter().any(|r| r == "heartbeat_stale"),
        "expected feed.chain_elapse with reason=heartbeat_stale, got {reasons:?}"
    );
}

#[test]
fn chain_exhausted_falls_back_to_neutral() {
    let env = Env::new();
    if skip_if_sandboxed(&env) {
        return;
    }
    env.enrol("opus-policy", 10, "1s");
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

    let output = child.wait_with_output().expect("wait child");
    assert!(
        output.status.success(),
        "hook stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "{}",
        "single-link chain exhaustion should emit Claude's neutral payload"
    );

    let reasons = chain_elapse_reasons(&env);
    assert!(
        reasons.iter().any(|r| r == "budget_elapsed"),
        "expected feed.chain_elapse(reason=budget_elapsed) before chain exhaustion, got {reasons:?}"
    );

    // The feed item itself must land in timed_out for the audit story.
    let list = env
        .rimz()
        .args(["feed", "list", "--json"])
        .output()
        .expect("feed list");
    let parsed: Value = serde_json::from_slice(&list.stdout).expect("feed list json");
    assert_eq!(parsed[0]["status"], "timed_out");

    // The timeout event records the chain_exhausted reason — distinct from
    // bridge_cap_elapsed so the audit story stays unambiguous.
    let events = read_events(&env);
    let timeout_reasons: Vec<String> = events
        .into_iter()
        .filter(|e| e.method == "feed.timeout")
        .filter_map(|e| {
            e.params
                .get("reason")
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned)
        })
        .collect();
    assert!(
        timeout_reasons.iter().any(|r| r == "chain_exhausted"),
        "expected feed.timeout with reason=chain_exhausted, got {timeout_reasons:?}"
    );

    // Suppress unused-field-on-events_log_path warning — the field is here
    // for future tests that want raw on-disk framing.
    let _ = env.events_log_path();
}
