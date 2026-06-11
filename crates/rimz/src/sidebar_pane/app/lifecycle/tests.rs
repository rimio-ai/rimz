use super::*;

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
