//! End-to-end integration tests for the reference Python hook-bridge
//! resolver under `examples/resolvers/`. The resolver speaks the public CLI
//! and the on-disk heartbeat protocol; these tests confirm the contract by
//! firing a real hook and asserting the agent-native decision JSON lands.
//!
//! Self-skips when `python3` is not on PATH or the sandbox forbids AF_UNIX.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::common::{Env, permission_payload, python3_present, wait_for_heartbeat};

/// Spawn the reference hook-bridge resolver, pointed at the harness workspace.
fn spawn_python_resolver(env: &Env, resolver_id: &str, run_seconds: f32) -> Child {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("workspace root")
        .join("examples/resolvers/hook_bridge_resolver.py");
    assert!(script.exists(), "resolver script missing: {script:?}");

    Command::new("python3")
        .arg(&script)
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
        .current_dir(&env.project_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn python resolver")
}

fn skip_preconditions(env: &Env) -> bool {
    if !python3_present() {
        tracing::warn!("skipping: python3 not on PATH");
        return true;
    }
    env.skip_if_sandboxed()
}

#[test]
fn python_resolver_allow_path_renders_claude_decision() {
    let env = Env::new();
    if skip_preconditions(&env) {
        return;
    }
    env.enrol("demo", 10, "30s");

    let mut resolver = spawn_python_resolver(&env, "demo", 8.0);

    // Give the resolver a beat to lay down its first heartbeat. The hook
    // bridge engages on the first fresh sample, so without this beat the
    // hook may take the no-resolver native_ui path.
    wait_for_heartbeat(&env, "demo", Instant::now() + Duration::from_secs(3));

    let output = env.run_hook("claude", &permission_payload("Read"));
    let _ = resolver.kill();
    let _ = resolver.wait();
    assert!(
        output.status.success(),
        "hook stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let decision: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("expected decision json, got: {stdout:?}"));
    assert_eq!(
        decision["hookSpecificOutput"]["decision"]["behavior"], "allow",
        "decision: {decision}"
    );
}

#[test]
fn python_resolver_abstain_path_exhausts_chain_to_neutral() {
    let env = Env::new();
    if skip_preconditions(&env) {
        return;
    }
    // Short budget so the chain-exhausted path fires before the test times
    // out (the resolver abstains on tool_name=Bash; chain has one link).
    env.enrol("demo", 10, "1s");

    let mut resolver = spawn_python_resolver(&env, "demo", 8.0);
    wait_for_heartbeat(&env, "demo", Instant::now() + Duration::from_secs(3));

    let output = env.run_hook("claude", &permission_payload("Bash"));
    let _ = resolver.kill();
    let _ = resolver.wait();
    assert!(
        output.status.success(),
        "hook stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "{}",
        "abstain on Bash should drain the chain and emit Claude's neutral payload"
    );
}
