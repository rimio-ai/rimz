use super::*;

#[test]
fn self_close_waits_for_a_sibling_before_ever_closing() {
    let mut state = SelfCloseState::default();
    // Startup: no sibling yet (terminal pane not materialized). Give Zellij
    // one observation to finish materializing the sibling.
    assert!(!self_close_decision(&mut state, Some(0)));
    assert!(!state.seen_sibling);
}

#[test]
fn self_close_fires_when_a_sibling_never_appears() {
    let mut state = SelfCloseState::default();
    assert!(!self_close_decision(&mut state, Some(0)));
    assert!(self_close_decision(&mut state, Some(0)));
}

#[test]
fn self_close_latches_then_fires_when_alone() {
    let mut state = SelfCloseState::default();
    assert!(!self_close_decision(&mut state, Some(1)));
    assert!(state.seen_sibling, "seeing a sibling must latch");
    // Sibling went away: now alone, so close.
    assert!(self_close_decision(&mut state, Some(0)));
}

#[test]
fn self_close_holds_while_siblings_remain() {
    let mut state = SelfCloseState {
        seen_sibling: true,
        empty_startup_observations: 0,
    };
    assert!(!self_close_decision(&mut state, Some(2)));
}

#[test]
fn self_close_never_fires_on_unknown_count() {
    let mut state = SelfCloseState {
        seen_sibling: true,
        empty_startup_observations: 0,
    };
    assert!(!self_close_decision(&mut state, None));
    assert!(
        state.seen_sibling,
        "an unknown count must not clear the latch"
    );
}

#[test]
fn resize_grew_treats_strictly_larger_width_as_grow() {
    // A grow is the flash precondition (the mux handed us a sibling's space),
    // so it takes the held path; a shrink or same width keeps the instant
    // repaint, and the first resize (no prior width) is held cautiously.
    assert!(resize_grew(Some(30), 120), "wider pane is a grow");
    assert!(!resize_grew(Some(120), 30), "narrower pane is not a grow");
    assert!(!resize_grew(Some(80), 80), "same width is not a grow");
    assert!(
        resize_grew(None, 1),
        "an unknown previous width counts as a grow"
    );
}
