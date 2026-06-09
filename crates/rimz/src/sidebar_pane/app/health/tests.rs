use super::*;

/// Health seeded with an alert whose episode started at `since`. `recovered`
/// flips it to the sticky-but-inactive (last fetch succeeded) state.
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
fn gives_up_after_sustained_degradation() {
    let base = 1_700_000_000;
    let since = Timestamp::from_second(base).unwrap();
    let now = Timestamp::from_second(base + GIVE_UP_AFTER_DEGRADED.as_secs() as i64).unwrap();
    assert!(degraded_too_long(&degraded_since(since, false), now));
}

#[test]
fn holds_while_degradation_is_still_brief() {
    // A few seconds of failure must not close the sidebar — that is a hiccup
    // or the sub-second gap while `cargo install` swaps the binary.
    let base = 1_700_000_000;
    let since = Timestamp::from_second(base).unwrap();
    let now = Timestamp::from_second(base + 5).unwrap();
    assert!(!degraded_too_long(&degraded_since(since, false), now));
}

#[test]
fn never_gives_up_once_recovered() {
    // A recovered (sticky but inactive) alert means the latest fetch
    // succeeded: the renderer is healthy and must not exit, however old the
    // past episode is.
    let base = 1_700_000_000;
    let since = Timestamp::from_second(base).unwrap();
    let now = Timestamp::from_second(base + 1_000).unwrap();
    assert!(!degraded_too_long(&degraded_since(since, true), now));
}

#[test]
fn never_gives_up_without_an_alert() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    assert!(!degraded_too_long(&Health::default(), now));
}
