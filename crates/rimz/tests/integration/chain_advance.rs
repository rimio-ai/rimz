//! Chain advancement integration tests. These exercise the M3.5 wiring
//! that hands off the active resolver mid-hook when its per-step budget
//! elapses or its heartbeat goes stale — the contract documented in
//! `docs/internals/resolvers.md`.
//!
//! Each test spawns a real `rimz hooks feed` subprocess, then drives the
//! ledger from the outside: emulate resolver heartbeats, watch the chain
//! advance, resolve from the second link, and assert the audit trail.

use std::time::{Duration, Instant};

use jiff::Timestamp;
use rimz::schema::heartbeat::ResolverHeartbeat;
use serde_json::Value;

use crate::common::{Env, permission_payload};

/// Reasons recorded on `feed.chain_elapse` events for the harness workspace.
fn chain_elapse_reasons(env: &Env) -> Vec<String> {
    env.read_events()
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

/// Keep `resolver_id` heartbeating fresh on a background thread until the
/// deadline, so the hook's restat after a chain advance still sees it alive.
fn keep_heartbeat_fresh(env: &Env, resolver_id: &'static str) -> std::thread::JoinHandle<()> {
    let workspace_id = env.workspace_id.clone();
    let runtime_root = env.runtime_root.clone();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let path = runtime_root
            .join("rimz")
            .join(workspace_id.as_str())
            .join("heartbeat")
            .join(format!("resolver.{resolver_id}.json"));
        while Instant::now() < deadline {
            let parsed = resolver_id.parse().expect("resolver id parse");
            let mut hb = ResolverHeartbeat::new(workspace_id.clone(), parsed);
            hb.last_seen = Timestamp::now();
            let _ = std::fs::write(&path, serde_json::to_vec(&hb).expect("hb"));
            std::thread::sleep(Duration::from_millis(250));
        }
    })
}

#[test]
fn chain_advances_on_budget_elapse() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    // First resolver has a 1-second per-step budget; second is generous.
    env.enrol("opus-policy", 10, "1s");
    env.enrol("slack-on-call", 20, "30s");
    env.write_heartbeat("opus-policy", Timestamp::now());
    env.write_heartbeat("slack-on-call", Timestamp::now());

    let child = env.spawn_hook("claude", &permission_payload("Bash"));

    let request_id = env
        .poll_pending_request_id(Instant::now() + Duration::from_secs(5))
        .expect("bridge item should appear in feed");

    // Keep slack-on-call heartbeating fresh while we wait for the budget
    // elapse, so the loop's restat after the chain advance succeeds.
    let heartbeat_keepalive = keep_heartbeat_fresh(&env, "slack-on-call");

    assert!(
        env.poll_active_resolver(
            &request_id,
            "slack-on-call",
            Instant::now() + Duration::from_secs(5),
        ),
        "chain should advance to slack-on-call after the 1s budget elapses"
    );

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
    if env.skip_if_sandboxed() {
        return;
    }
    // Generous per-step budgets — the trigger we want is heartbeat staleness.
    env.enrol("opus-policy", 10, "30s");
    env.enrol("slack-on-call", 20, "30s");
    env.write_heartbeat("opus-policy", Timestamp::now());
    env.write_heartbeat("slack-on-call", Timestamp::now());

    let child = env.spawn_hook("claude", &permission_payload("Bash"));

    let request_id = env
        .poll_pending_request_id(Instant::now() + Duration::from_secs(5))
        .expect("bridge item should appear in feed");

    // Age out opus-policy's heartbeat; keep slack-on-call alive.
    env.write_heartbeat("opus-policy", Timestamp::now() - Duration::from_secs(60));
    let heartbeat_keepalive = keep_heartbeat_fresh(&env, "slack-on-call");

    assert!(
        env.poll_active_resolver(
            &request_id,
            "slack-on-call",
            Instant::now() + Duration::from_secs(5),
        ),
        "chain should advance once opus-policy heartbeat is stale"
    );

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
    if env.skip_if_sandboxed() {
        return;
    }
    env.enrol("opus-policy", 10, "1s");
    env.write_heartbeat("opus-policy", Timestamp::now());

    let output = env.run_hook("claude", &permission_payload("Bash"));
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
    let parsed = env.feed_list_json();
    assert_eq!(parsed[0]["status"], "timed_out");

    // The timeout event records the chain_exhausted reason — distinct from
    // bridge_cap_elapsed so the audit story stays unambiguous.
    let timeout_reasons: Vec<String> = env
        .read_events()
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
}
