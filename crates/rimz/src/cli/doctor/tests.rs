use super::{progress_label, unscheduled_clock};
use rimz::config::TaskEntry;
use rimz::harness::schedule::{arming::Arming, catalog::TaskSource, parse_trigger};

#[test]
fn progress_label_tracks_oldest_in_flight_check() {
    assert_eq!(
        progress_label(&["temp units", "hooks", "lsp"]),
        "checking temp units (+2 more)"
    );
    assert_eq!(
        progress_label(&["hooks", "lsp"]),
        "checking hooks (+1 more)"
    );
    assert_eq!(progress_label(&["lsp"]), "checking lsp");
    assert_eq!(progress_label(&[]), "checking doctor");
}

#[test]
fn refusals_clear_only_for_matching_success_or_later_start() {
    use rimz::lsp::broker::probe::{Check, Outcome};
    let record = rimz::diag::lsp::Record {
        at: jiff::Timestamp::from_millisecond(1000).unwrap(),
        root: "/checkout".into(),
        server: "rust".into(),
        event: "refused".into(),
        details: serde_json::json!({}),
    };
    for (root, server, outcome, expected) in [
        (
            "/checkout",
            "rust",
            Outcome::Started { version: None },
            true,
        ),
        (
            "/checkout",
            "rust",
            Outcome::Running {
                state: rimz::lsp::registry::State::Ready,
            },
            true,
        ),
        ("/other", "rust", Outcome::Started { version: None }, false),
        (
            "/checkout",
            "other",
            Outcome::Started { version: None },
            false,
        ),
        (
            "/checkout",
            "rust",
            Outcome::Invalid {
                error: "bad config".into(),
                fix: "repair".into(),
            },
            false,
        ),
    ] {
        assert_eq!(
            super::refusal_recovered(
                &record,
                &[Check {
                    root: root.into(),
                    server: server.into(),
                    outcome
                }],
                &[]
            ),
            expected,
            "{root} {server}"
        );
    }
    for (root, server, started, expected) in [
        ("/checkout", "rust", 1001, true),
        ("/checkout", "rust", 1000, false),
        ("/checkout", "rust", 999, false),
        ("/other", "rust", 1001, false),
        ("/checkout", "other", 1001, false),
    ] {
        let entry = serde_json::from_value(serde_json::json!({
            "root": root, "server": server, "nonce": "nonce", "broker_pid": 1, "broker_start_token": "token",
            "state": "ready", "started_at_ms": started, "estimate_bytes": 0, "settings_hash": "hash", "request_count": 0, "peak_rss_kb": 0, "leases": []
        })).unwrap();
        assert_eq!(
            super::refusal_recovered(&record, &[], &[entry]),
            expected,
            "{root} {server} {started}"
        );
    }
    for (state, started, ready, expected) in [
        (
            serde_json::json!({"dormant": {"reason": "memory pressure", "since_ms": 1001}}),
            1001,
            None,
            false,
        ),
        (serde_json::json!("starting"), 1001, None, true),
        (serde_json::json!("ready"), 999, Some(1001), true),
        (serde_json::json!("ready"), 1001, Some(1000), false),
    ] {
        let entry = serde_json::from_value(serde_json::json!({
            "root": "/checkout", "server": "rust", "nonce": "nonce", "broker_pid": 1, "broker_start_token": "token",
            "state": state, "started_at_ms": started, "ready_at_ms": ready, "estimate_bytes": 0, "settings_hash": "hash", "request_count": 0, "peak_rss_kb": 0, "leases": []
        })).unwrap();
        assert_eq!(super::refusal_recovered(&record, &[], &[entry]), expected);
    }
}

#[test]
fn only_live_clock_tasks_without_a_scheduler_are_unscheduled() {
    let now = "2026-01-01T00:00:00Z".parse().unwrap();
    for (trigger, enabled, pause_until, room, timer, expected) in [
        ("every = '1m'", true, None, false, false, true),
        ("every = '1m'", true, None, true, false, false),
        ("every = '1m'", true, None, false, true, false),
        ("every = '1m'", false, None, false, false, false),
        (
            "every = '1m'",
            true,
            Some("2026-01-02T00:00:00Z"),
            false,
            false,
            false,
        ),
        (
            "every = '1m'",
            true,
            Some("2025-12-31T00:00:00Z"),
            false,
            false,
            true,
        ),
        ("signal = 'ci.failed'", true, None, false, false, false),
        ("watch = 'true'", true, None, false, false, false),
        ("every = 'invalid'", true, None, false, false, false),
    ] {
        let entry: TaskEntry = toml::from_str(&format!(
            "agent = 'claude'\nroot = '/tmp/project'\n{trigger}"
        ))
        .unwrap();
        let parsed = parse_trigger("morning", &entry);
        let record = Arming {
            enabled,
            at: None,
            pause_until: pause_until.map(|s| s.parse().unwrap()),
            strikes: None,
        };
        assert_eq!(
            unscheduled_clock(&parsed, Some(&record), TaskSource::Config, room, timer, now),
            expected,
            "{trigger}, {record:?}, room={room}, timer={timer}"
        );
    }
}
