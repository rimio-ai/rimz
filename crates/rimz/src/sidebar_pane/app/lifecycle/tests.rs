use super::*;
use std::time::Duration;

#[test]
fn self_close_covers_startup_latch_and_unknown_counts() {
    let mut state = SelfCloseState::default();
    assert!(!self_close_decision(&mut state, Some(0)));
    assert!(!state.seen_sibling);
    assert!(self_close_decision(&mut state, Some(0)));

    let mut state = SelfCloseState::default();
    assert!(!self_close_decision(&mut state, Some(1)));
    assert!(state.seen_sibling, "seeing a sibling must latch");
    assert!(self_close_decision(&mut state, Some(0)));

    let mut state = SelfCloseState {
        seen_sibling: true,
        empty_startup_observations: 0,
    };
    assert!(!self_close_decision(&mut state, Some(2)));
    assert!(!self_close_decision(&mut state, None));
    assert!(
        state.seen_sibling,
        "an unknown count must not clear the latch"
    );
}

#[test]
fn resize_grew_treats_strictly_larger_width_as_grow() {
    assert!(resize_grew(Some(30), 120), "wider pane is a grow");
    assert!(!resize_grew(Some(120), 30), "narrower pane is not a grow");
    assert!(!resize_grew(Some(80), 80), "same width is not a grow");
    assert!(
        resize_grew(None, 1),
        "an unknown previous width counts as a grow"
    );
}

#[test]
fn paint_hold_blocks_until_ceiling_then_expires() {
    let now = Instant::now();
    let mut hold = PaintHold::default();
    assert!(!hold.blocks_paint(now), "released holds never block");

    hold.engage(now, 100);
    assert!(hold.blocks_paint(now + RESIZE_PAINT_HOLD_CEILING / 2));
    assert!(!hold.blocks_paint(now + RESIZE_PAINT_HOLD_CEILING));
    assert!(!hold.is_engaged(), "expiry clears the hold");
}

#[test]
fn paint_hold_reengage_restamps_deadline_and_stamp() {
    let now = Instant::now();
    let mut hold = PaintHold::default();
    hold.engage(now, 100);
    hold.engage(now + Duration::from_millis(500), 200);

    assert!(
        hold.blocks_paint(now + RESIZE_PAINT_HOLD_CEILING),
        "the second engage owns the deadline"
    );
    assert!(!hold.releases_on_stamp(Some(199)));
    assert!(hold.releases_on_stamp(Some(200)));
}

#[test]
fn paint_hold_releases_on_post_engage_stamp_only() {
    let now = Instant::now();
    let mut hold = PaintHold::default();
    assert!(
        !hold.releases_on_stamp(Some(100)),
        "released holds do not release on arbitrary stamps"
    );

    hold.engage(now, 100);
    assert!(!hold.releases_on_stamp(None));
    assert!(!hold.releases_on_stamp(Some(99)));
    assert!(hold.releases_on_stamp(Some(100)));
    assert!(hold.releases_on_stamp(Some(101)));
}
