use super::*;
use rimz::agents::{AgentRateLimits, RateLimitCacheEntry, RateLimitWindow, RateLimitsCache};
use std::collections::BTreeMap;

pub(super) fn capacity(env: &Env, used: u8) -> jiff::Timestamp {
    let reset = jiff::Timestamp::now() + jiff::SignedDuration::from_hours(2);
    env.publish_rate_limits(&RateLimitsCache {
        entries: BTreeMap::from([(
            "claude@default".parse().unwrap(),
            RateLimitCacheEntry {
                limits: AgentRateLimits {
                    windows: vec![RateLimitWindow {
                        used_percentage: Some(used),
                        resets_at: Some(reset),
                        duration_mins: Some(300),
                        ..Default::default()
                    }],
                },
                ..Default::default()
            },
        )]),
        ..Default::default()
    });
    reset
}

/// `submit` refuses a caller that is no judge, and an answer it cannot parse, before it
/// touches any run.
#[test]
fn submit_refuses_a_non_judge_caller_and_unparsable_flags_leaving_the_run_alone() {
    let (env, _) = fixture(
        &format!("{HEADER}{WAIT}"),
        "printf '{\"outcome\":\"pending\"}'",
    );
    let id = start(&env);
    let before = until(&env, &id, |value| value["record"]["wait"].is_object())["record"].clone();
    for (args, reason) in [
        (
            &["workflow", "submit", "done"][..],
            "error: caller is not a workflow judge; rimz workflow submit answers a check from \
             its judge's own pane\n",
        ),
        (
            &["workflow", "submit", "done", "--json", "[1]"][..],
            "invalid value '[1]' for '--json <OBJECT>': expected one JSON object",
        ),
        (
            &["workflow", "submit", "done", "--recheck-at", "soonish"][..],
            "invalid value 'soonish' for '--recheck-at <WHEN>': \"soonish\" is not a workflow \
             time; give a duration (8h), HH:MM, <weekday> HH:MM, a date (2026-11-14), a \
             date-time (2026-11-14 09:00), or a timestamp (2026-11-13T21:00:00Z)",
        ),
    ] {
        let output = command(&env, args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success() && stderr.contains(reason),
            "{args:?}: {stderr}"
        );
        assert!(!stderr.contains("accepted"), "{args:?}: {stderr}");
    }
    assert_eq!(show(&env, &id)["record"], before);
}

pub(super) fn workflow_diagnostics(env: &Env, kind: &str) -> Vec<Value> {
    let state = env.state_path_for(&env.project_root);
    let sink = rimz::diag::DiagSink::under(state.root, state.workspace_id, "test", None);
    fs::read_to_string(sink.log_path().unwrap())
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|record| record["event"]["kind"] == kind)
        .collect()
}
