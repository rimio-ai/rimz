use super::*;
use serde_json::json;

#[test]
fn provider_limit_uses_the_latest_spent_reset_across_only_the_requested_login() {
    use crate::agents::{AgentRateLimits, RateLimitCacheEntry, RateLimitWindow, RateLimitsCache};
    let now = Timestamp::from_second(2_000_000_000).unwrap();
    let entries = [
        ("pi@default/anthropic/oauth", 100, 1),
        ("pi@default/openai/oauth", 100, 3),
        ("pi@work/openai/oauth", 100, 4),
        ("pi@default", 0, 5),
    ]
    .into_iter()
    .map(|(key, used, hours)| {
        let key: crate::agents::account::AccountUsageKey = key.parse().unwrap();
        let entry = RateLimitCacheEntry {
            scope: key.scope.clone(),
            limits: AgentRateLimits {
                windows: vec![RateLimitWindow {
                    used_percentage: Some(used),
                    duration_mins: Some(300),
                    resets_at: Some(now + jiff::SignedDuration::from_hours(hours)),
                    ..Default::default()
                }],
            },
            ..Default::default()
        };
        (key, entry)
    })
    .collect();
    let cache = RateLimitsCache {
        entries,
        ..Default::default()
    };
    assert_eq!(
        latest_limit_reset(&cache, &"pi@default".parse().unwrap(), now, None),
        Some(now + jiff::SignedDuration::from_hours(3))
    );
}

#[test]
fn structured_object_is_saved_before_settlement() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("check.json");
    let value = json!({"outcome":"ok", "detail":"structured"});
    assert_eq!(object(Some(value.clone()), &path), Ok(value.clone()));
    let saved = std::fs::read(&path);
    assert!(
        saved.is_ok(),
        "structured output must be saved at output_path"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&saved.unwrap()).unwrap(),
        value
    );
}

#[test]
fn file_object_is_used_without_structured_output() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("check.json");
    let value = json!({"outcome":"ok", "detail":"file"});
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(
        object(None, &path),
        Ok(value),
        "output_path is the verdict fallback"
    );
}

#[test]
fn only_a_first_died_or_invalid_attempt_is_owed_a_retry() {
    for kind in [
        FailureKind::Died,
        FailureKind::Invalid,
        FailureKind::Timeout,
        FailureKind::Limited,
        FailureKind::Canceled,
    ] {
        assert_eq!(FailureKind::from_label(kind.label()), Some(kind));
        assert_eq!(
            kind.retry_owed(false),
            matches!(kind, FailureKind::Died | FailureKind::Invalid),
            "{kind:?}"
        );
        assert!(!kind.retry_owed(true), "{kind:?} retried twice");
    }
}
