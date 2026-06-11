use super::*;

fn ts(second: i64) -> Timestamp {
    Timestamp::from_second(second).unwrap()
}

fn degraded_since(since: Timestamp, recovered: bool) -> Health {
    Health {
        failure_streak: ALERT_AFTER_FAILURES,
        alert: Some(Alert {
            reason: "snapshot failed: boom".to_owned(),
            since,
            recovered_at: recovered.then_some(since),
        }),
    }
}

#[test]
fn degraded_give_up_requires_an_active_alert_past_the_ceiling() {
    let base = 1_700_000_000;
    let since = ts(base);
    let ceiling = GIVE_UP_AFTER_DEGRADED.as_secs() as i64;
    let cases = [
        (
            "sustained active alert",
            degraded_since(since, false),
            ts(base + ceiling),
            true,
        ),
        (
            "brief active alert",
            degraded_since(since, false),
            ts(base + 5),
            false,
        ),
        (
            "recovered sticky alert",
            degraded_since(since, true),
            ts(base + 1_000),
            false,
        ),
        ("no alert", Health::default(), ts(base + ceiling), false),
    ];

    for (name, health, now, expected) in cases {
        assert_eq!(degraded_too_long(&health, now), expected, "{name}");
    }
}
