//! End-to-end coverage for the reference Python hook-bridge resolver
//! answering a pi `tool_call`. Pi's decision wire differs from Claude's —
//! allow is the empty object `{}` and deny is `{"block": true, …}` — so the
//! same resolver protocol must land pi's own shapes on the hook stdout.
//!
//! Self-skips when `python3` is not on PATH or the sandbox forbids AF_UNIX.

use serde_json::{Value, json};

use crate::common::{
    Env, pi_tool_call_payload, skip_preconditions, spawn_example_resolver,
    wait_for_example_resolver,
};

#[test]
fn python_resolver_allow_path_renders_pi_decision() {
    let env = Env::new();
    if skip_preconditions(&env) {
        return;
    }
    env.enrol("demo", 10, "30s");

    let mut resolver = spawn_example_resolver(&env, "hook_bridge_resolver.py", "demo", 8.0, None);
    wait_for_example_resolver(&env, "demo");

    // `read` is on the resolver's allowlist (pi's lowercase tool vocabulary).
    let output = env.run_hook("pi", &pi_tool_call_payload("read"));
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
        decision,
        json!({}),
        "pi's allow is the empty object — the extension blocks only on block === true"
    );
}
