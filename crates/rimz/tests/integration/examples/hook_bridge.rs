//! End-to-end integration tests for the reference Python hook-bridge
//! resolver under `examples/resolvers/`. The resolver speaks the public CLI
//! and the on-disk heartbeat protocol; these tests confirm the contract by
//! firing a real hook and asserting the agent-native decision JSON lands.
//!
//! Self-skips when `python3` is not on PATH or the sandbox forbids AF_UNIX.

use serde_json::Value;

use crate::common::{
    Env, permission_payload, skip_preconditions, spawn_example_resolver, wait_for_example_resolver,
};

#[test]
fn python_resolver_allow_path_renders_claude_decision() {
    let env = Env::new();
    if skip_preconditions(&env) {
        return;
    }
    env.enrol("demo", 10, "30s");

    let mut resolver = spawn_example_resolver(&env, "hook_bridge_resolver.py", "demo", 8.0, None);

    // Give the resolver a beat to lay down its first heartbeat. The hook
    // bridge engages on the first fresh sample, so without this beat the
    // hook may take the no-resolver native_ui path.
    wait_for_example_resolver(&env, "demo");

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
