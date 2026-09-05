use super::*;
use crate::config::TaskTarget;
use crate::harness::schedule::signal::SignalSource;

fn task() -> TaskEntry {
    TaskEntry {
        wake: Some(TaskTarget {
            kind: "claude".to_owned(),
            session: "session".to_owned(),
            handle: "@coder#feat-x".to_owned(),
        }),
        ..TaskEntry::default()
    }
}

fn meta(handle: &str) -> WakeMeta {
    WakeMeta {
        armed_by: WakeArmer::Agent {
            handle: handle.to_owned(),
        },
        armed_at: "2026-01-01T14:02:00Z".parse().unwrap(),
        delay: Some("30m".to_owned()),
        last_observed_at: None,
    }
}

fn signal(name: &str, payload: Value) -> Signal {
    Signal {
        name: name.parse().unwrap(),
        payload: payload.as_object().unwrap().clone(),
        source: SignalSource::Cli,
        watch: None,
    }
}

#[test]
fn wake_note_is_verbatim_and_signal_payload_is_compact() {
    let signal = signal(
        "ci.failed",
        serde_json::json!({
            "branch": "feat-x", "number": 91, "signal": "not-canonical",
            "nested": {"lines": "one\ntwo"}
        }),
    );
    let note = "  Inspect {{branch}} and {{nested}}.\nKeep this line.  \n";
    let body = compose_wake(
        "wake-test",
        &task(),
        None,
        Evidence::Signal(&signal),
        note,
        TimeZone::UTC,
    );
    let (evidence, delivered_note) = body.split_once("\n\n").unwrap();
    assert_eq!(delivered_note, note);
    assert_eq!(evidence.lines().count(), 2);
    let payload: Value = serde_json::from_str(evidence.lines().nth(1).unwrap()).unwrap();
    assert_eq!(payload["signal"], "ci.failed");
    assert_eq!(payload["nested"]["lines"], "one\ntwo");
    assert!(!evidence.contains("---"));
}

#[test]
fn wake_headline_names_trigger_and_armer() {
    let task = task();
    let own = meta("@coder#feat-x");
    assert_eq!(
        compose_wake(
            "wake-test",
            &task,
            Some(&own),
            Evidence::Scheduled,
            "",
            TimeZone::UTC
        ),
        "wake-test fired: 30m elapsed, armed by you at 14:02"
    );
    for (name, payload, trigger) in [
        (
            "ci.failed",
            serde_json::json!({"branch":"feat-x","number":91}),
            "ci.failed on feat-x (PR #91)",
        ),
        (
            "pr.merged",
            serde_json::json!({"branch":"feat-x","number":91}),
            "pr.merged on feat-x (PR #91)",
        ),
        (
            "agent.idle",
            serde_json::json!({"handle":"@coder"}),
            "agent.idle @coder",
        ),
        (
            "team.idle",
            serde_json::json!({"instance":"forge#feat-x"}),
            "team.idle forge#feat-x",
        ),
        ("deploy.finished", serde_json::json!({}), "deploy.finished"),
    ] {
        let signal = signal(name, payload);
        assert_eq!(
            compose_wake(
                "wake-test",
                &task,
                Some(&meta("@planner#feat-x")),
                Evidence::Signal(&signal),
                "",
                TimeZone::UTC
            )
            .lines()
            .next()
            .unwrap(),
            format!("wake-test fired: {trigger}, armed by @planner#feat-x at 14:02")
        );
    }
    assert!(
        compose_wake(
            "wake-test",
            &task,
            Some(&meta("@coder#other")),
            Evidence::Manual,
            "",
            TimeZone::UTC
        )
        .contains("manual fire, armed by @coder#other")
    );
    let human = WakeMeta {
        armed_by: WakeArmer::Human,
        ..own
    };
    assert_eq!(
        compose_wake(
            "wake-test",
            &task,
            Some(&human),
            Evidence::Manual,
            "",
            TimeZone::UTC
        ),
        "wake-test fired: manual fire, armed from the shell at 14:02"
    );
}

#[test]
fn watch_headline_uses_elapsed_time_and_keeps_tail() {
    let task = TaskEntry {
        watch: Some("gh run watch --exit-status".to_owned()),
        ..task()
    };
    for (watch, trigger, tail) in [
        (
            WatchOutcome::Exited {
                code: Some(1),
                output: "failed job\n".to_owned(),
                elapsed_ms: 720_000,
            },
            "`gh run watch --exit-status` exited 1 after 12m",
            "failed job\n",
        ),
        (
            WatchOutcome::TimedOut {
                code: None,
                output: "still pending".to_owned(),
                elapsed_ms: 3_540_000,
            },
            "`gh run watch --exit-status` timed out after 59m",
            "still pending",
        ),
        (
            WatchOutcome::Lost {
                detail: "lock disappeared".to_owned(),
            },
            "watcher lost",
            "lock disappeared",
        ),
    ] {
        let signal = Signal {
            watch: Some(watch),
            ..signal("wake.test", serde_json::json!({}))
        };
        assert_eq!(
            compose_wake(
                "wake-test",
                &task,
                None,
                Evidence::Signal(&signal),
                "note",
                TimeZone::UTC
            ),
            format!("wake-test fired: {trigger}\n{tail}\n\nnote")
        );
    }
    let old: WatchOutcome =
        serde_json::from_value(serde_json::json!({"result":"exited","code":0,"output":""}))
            .unwrap();
    assert!(matches!(old, WatchOutcome::Exited { elapsed_ms: 0, .. }));
}

#[test]
fn expiry_names_subscription_scope_and_window() {
    let task = TaskEntry {
        signal: Some("ci.*".to_owned()),
        matches: Some([("branch".to_owned(), "feat-x".to_owned())].into()),
        timeout: Some("59m".to_owned()),
        ..task()
    };
    assert_eq!(
        compose_wake(
            "wake-test",
            &task,
            Some(&meta("@coder#feat-x")),
            Evidence::Expired,
            "{{branch}}",
            TimeZone::UTC
        ),
        "wake-test expired: no ci.* on feat-x in 59m\n\n{{branch}}"
    );
}
