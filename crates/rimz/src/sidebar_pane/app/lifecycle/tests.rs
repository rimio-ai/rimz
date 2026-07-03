use super::*;
use std::time::Duration;

#[test]
fn brief_empty_read_does_not_self_close() {
    let now = Instant::now();
    let mut state = SelfCloseState::default();

    assert!(!self_close_decision(&mut state, Some(1), now));
    assert!(!self_close_decision(
        &mut state,
        Some(0),
        now + SELF_CLOSE_EMPTY_CONFIRM / 2
    ));
    assert!(
        state.seen_sibling,
        "seeing a sibling must stay latched for resize holds"
    );
}

#[test]
fn sustained_empty_read_self_closes() {
    let now = Instant::now();
    let mut state = SelfCloseState::default();

    assert!(!self_close_decision(&mut state, Some(1), now));
    assert!(!state.confirming_empty());
    assert!(!self_close_decision(&mut state, Some(0), now));
    assert!(state.confirming_empty());
    assert!(self_close_decision(
        &mut state,
        Some(0),
        now + SELF_CLOSE_EMPTY_CONFIRM
    ));
}

#[test]
fn non_zero_read_resets_self_close_window() {
    let now = Instant::now();
    let mut state = SelfCloseState::default();

    assert!(!self_close_decision(&mut state, Some(1), now));
    assert!(!self_close_decision(&mut state, Some(0), now));
    assert!(!self_close_decision(
        &mut state,
        Some(2),
        now + SELF_CLOSE_EMPTY_CONFIRM / 2
    ));
    assert!(!state.confirming_empty());
    assert!(!self_close_decision(
        &mut state,
        Some(0),
        now + SELF_CLOSE_EMPTY_CONFIRM
    ));
    assert!(self_close_decision(
        &mut state,
        Some(0),
        now + SELF_CLOSE_EMPTY_CONFIRM + SELF_CLOSE_EMPTY_CONFIRM
    ));
}

#[test]
fn from_birth_empty_tab_self_closes_after_confirm_window() {
    let now = Instant::now();
    let mut state = SelfCloseState::default();

    assert!(!self_close_decision(&mut state, Some(0), now));
    assert!(!state.seen_sibling);
    assert!(self_close_decision(
        &mut state,
        Some(0),
        now + SELF_CLOSE_EMPTY_CONFIRM
    ));
}

#[test]
fn unknown_counts_do_not_advance_or_reset_self_close() {
    let now = Instant::now();
    let mut state = SelfCloseState::default();

    assert!(!self_close_decision(&mut state, Some(1), now));
    assert!(state.seen_sibling, "seeing a sibling must latch");
    assert!(!self_close_decision(&mut state, Some(0), now));
    assert!(!self_close_decision(
        &mut state,
        None,
        now + SELF_CLOSE_EMPTY_CONFIRM
    ));
    assert!(
        state.seen_sibling,
        "an unknown count must not clear the latch"
    );
    assert!(self_close_decision(
        &mut state,
        Some(0),
        now + SELF_CLOSE_EMPTY_CONFIRM
    ));
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
