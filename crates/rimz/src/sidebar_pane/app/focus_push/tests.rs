use super::*;
use std::cell::Cell;

#[test]
fn paint_wait_returns_on_first_painted_without_waiting_for_other_peers() {
    let now = Instant::now();
    let receives = Cell::new(0);
    let timed_out = wait_for_paint(
        now + PAINT_WAIT,
        || now,
        |timeout| {
            assert_eq!(timeout, PAINT_WAIT);
            receives.set(receives.get() + 1);
            Ok(())
        },
    );
    assert_eq!(receives.get(), 1, "must receive the destination's paint");
    assert!(!timed_out);
}

#[test]
fn paint_wait_returns_when_all_peers_release() {
    let now = Instant::now();
    let receives = Cell::new(0);
    let timed_out = wait_for_paint(
        now + PAINT_WAIT,
        || now,
        |_| {
            receives.set(receives.get() + 1);
            Err(RecvTimeoutError::Disconnected)
        },
    );
    assert_eq!(receives.get(), 1, "must observe all peers releasing");
    assert!(!timed_out);
}

#[test]
fn paint_wait_is_capped_even_when_a_peer_retains_its_ack() {
    let start = Instant::now();
    let clock = Cell::new(start);
    let timed_out = wait_for_paint(
        start + PAINT_WAIT,
        || clock.get(),
        |timeout| {
            assert_eq!(timeout, PAINT_WAIT);
            clock.set(clock.get() + timeout);
            Err(RecvTimeoutError::Timeout)
        },
    );
    assert!(timed_out, "an unresponsive destination must hit the cap");
    assert_eq!(clock.get(), start + PAINT_WAIT);
}
