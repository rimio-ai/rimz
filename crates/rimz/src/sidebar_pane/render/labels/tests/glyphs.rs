use super::*;

/// The clock face fills a quarter per quarter hour and rings past the
/// hour, with each bucket's upper edge inclusive.
#[test]
fn elapsed_glyph_fills_by_the_quarter_hour() {
    assert_eq!(elapsed_glyph(0), "◔");
    assert_eq!(elapsed_glyph(900), "◔");
    assert_eq!(elapsed_glyph(901), "◑");
    assert_eq!(elapsed_glyph(1800), "◑");
    assert_eq!(elapsed_glyph(1801), "◕");
    assert_eq!(elapsed_glyph(2700), "◕");
    assert_eq!(elapsed_glyph(2701), "●");
    assert_eq!(elapsed_glyph(3600), "●");
    assert_eq!(elapsed_glyph(3601), "◉");
    assert_eq!(elapsed_glyph(48 * 3600), "◉");
}
/// The window token's tint steps by magnitude — the dim capability chrome
/// below 128k, sky at 128k, gold at 258k, clay amber at 1m+ — with the tinted
/// bands DIM-weighted so they never outshine the meter. `NO_COLOR`
/// collapses every band to the bare DIM weight.
#[test]
fn window_style_tints_by_size_class_but_stays_subordinate() {
    let theme = Theme::fixed(false);
    let banded = |window| window_style(&theme, window);
    assert_eq!(banded(32_000), theme.dim());
    assert_eq!(banded(127_999), theme.dim());
    assert_eq!(banded(128_000), theme.style(Color::Blue, Modifier::DIM));
    assert_eq!(banded(200_000), theme.style(Color::Blue, Modifier::DIM));
    assert_eq!(banded(258_000), theme.style(Color::Yellow, Modifier::DIM));
    assert_eq!(banded(999_999), theme.style(Color::Yellow, Modifier::DIM));
    assert_eq!(banded(1_000_000), theme.style(ORANGE, Modifier::DIM));
    assert_eq!(banded(1_050_000), theme.style(ORANGE, Modifier::DIM));

    let plain = Theme::fixed(true);
    for window in [32_000, 128_000, 258_000, 1_050_000] {
        assert!(window_style(&plain, window).fg.is_none());
        assert!(
            window_style(&plain, window)
                .add_modifier
                .contains(Modifier::DIM)
        );
    }
}
/// The attention glyph wears the shared age heat over a yellow floor — a
/// fresh ask reads yellow, amber past the half hour, red past the hour, the
/// same quarters as the age clock beside it — and only for the
/// `waiting`/`failed` states; every calm state keeps its resting tone,
/// however old.
#[test]
fn attention_glyph_heats_with_the_age_clock_over_a_yellow_floor() {
    let theme = Theme::fixed(false);
    let yellow = theme.style(Color::Yellow, Modifier::BOLD).fg;
    let amber = theme.style(ORANGE, Modifier::BOLD).fg;
    let red = theme.style(Color::Red, Modifier::BOLD).fg;

    // Both attention states floor at yellow while the age heat is still
    // resting — a row that needs a human never reads as dim chrome — then
    // step with the clock quarters. The glyph breathes, so its brightness
    // modifier varies by frame; only the color is asserted here.
    for status in [AgentStatus::Waiting, AgentStatus::Failed] {
        assert_eq!(
            attention_glyph_style(&theme, status, 5 * 60, 0, false).fg,
            yellow
        );
        assert_eq!(
            attention_glyph_style(&theme, status, 25 * 60, 0, false).fg,
            yellow
        );
        assert_eq!(
            attention_glyph_style(&theme, status, 31 * 60, 0, false).fg,
            amber
        );
        assert_eq!(
            attention_glyph_style(&theme, status, 61 * 60, 0, false).fg,
            red
        );
    }
    // Calm states never heat, however old — they take their plain style.
    assert_eq!(
        attention_glyph_style(&theme, AgentStatus::Idle, 2 * 60 * 60, 0, false).fg,
        agent_style(&theme, AgentStatus::Idle).fg
    );
    assert_eq!(
        attention_glyph_style(&theme, AgentStatus::Running, 2 * 60 * 60, 0, false).fg,
        agent_style(&theme, AgentStatus::Running).fg
    );
}

#[test]
fn unread_glyph_hard_blinks_without_heating() {
    let theme = Theme::fixed(false);
    let read = attention_glyph_style(&theme, AgentStatus::Success, 5 * 60, 0, false);
    let unread_on = attention_glyph_style(&theme, AgentStatus::Success, 5 * 60, 0, true);
    let unread_off = attention_glyph_style(&theme, AgentStatus::Success, 5 * 60, 3, true);
    let unread_wrap = attention_glyph_style(&theme, AgentStatus::Success, 5 * 60, 6, true);

    assert_eq!(unread_on.fg, agent_style(&theme, AgentStatus::Success).fg);
    assert_eq!(unread_on.add_modifier, Modifier::BOLD);
    assert_eq!(unread_off.add_modifier, Modifier::DIM);
    assert_eq!(unread_wrap.add_modifier, Modifier::BOLD);
    assert_ne!(read.add_modifier, unread_on.add_modifier);
}

#[test]
fn unread_actionable_glyph_blinks_and_keeps_heat_color() {
    let theme = Theme::fixed(false);
    let unread_waiting_on = attention_glyph_style(&theme, AgentStatus::Waiting, 5 * 60, 0, true);
    let unread_waiting_off = attention_glyph_style(&theme, AgentStatus::Waiting, 5 * 60, 3, true);
    assert_eq!(
        unread_waiting_on.fg,
        theme.style(Color::Yellow, Modifier::empty()).fg
    );
    assert_eq!(unread_waiting_on.add_modifier, Modifier::BOLD);
    assert_eq!(unread_waiting_off.add_modifier, Modifier::DIM);

    let red_read = attention_glyph_style(&theme, AgentStatus::Failed, 2 * 60 * 60, 0, false);
    assert_eq!(red_read.fg, theme.style(Color::Red, Modifier::empty()).fg);
    assert_eq!(red_read.add_modifier, hard_blink(0));
}
/// Each animation cycles through its frames and wraps, so the phase can grow
/// without bound.
#[test]
fn animations_cycle_and_wrap() {
    for (phase, expected) in WORKING_FRAMES.iter().enumerate() {
        assert_eq!(working_glyph(phase as u64), *expected);
    }
    assert_eq!(
        working_glyph(WORKING_FRAMES.len() as u64),
        WORKING_FRAMES[0]
    );
    assert_eq!(THINKING_FRAMES, ["·", "✢", "✳", "✶", "✻", "✶", "✳", "✢"]);
    for (phase, expected) in THINKING_FRAMES.iter().enumerate() {
        let held_phase = phase as u64 * THINKING_FRAME_HOLD;
        assert_eq!(thinking_glyph(held_phase), *expected);
        assert_eq!(thinking_glyph(held_phase + 1), *expected);
        assert_eq!(thinking_glyph(held_phase + 2), *expected);
    }
    assert_eq!(
        thinking_glyph(THINKING_FRAMES.len() as u64 * THINKING_FRAME_HOLD),
        THINKING_FRAMES[0]
    );
    assert_eq!(
        resolver_glyph(RESOLVER_FRAMES.len() as u64),
        RESOLVER_FRAMES[0]
    );
    // The two transient heads cycle and wrap on the same shared phase.
    for (phase, expected) in COMPACTING_FRAMES.iter().enumerate() {
        assert_eq!(compacting_glyph(phase as u64), *expected);
    }
    assert_eq!(
        compacting_glyph(COMPACTING_FRAMES.len() as u64),
        COMPACTING_FRAMES[0]
    );
    for (phase, expected) in SUBAGENT_FRAMES.iter().enumerate() {
        assert_eq!(subagent_glyph(phase as u64), *expected);
    }
    assert_eq!(
        subagent_glyph(SUBAGENT_FRAMES.len() as u64),
        SUBAGENT_FRAMES[0]
    );
    // The phase can grow without bound and still indexes a frame.
    assert_eq!(
        working_glyph(u64::MAX),
        WORKING_FRAMES[(u64::MAX % WORKING_FRAMES.len() as u64) as usize]
    );
}
/// The loading dots are static while the attention glyph breathes a slow
/// brightness pulse — `DIM` at the troughs, `BOLD` at the peak — that wraps
/// with the phase, never strobing.
#[test]
fn loading_dots_and_attention_breath_cadence() {
    assert_eq!(loading_dots(0), "...");
    assert_eq!(loading_dots(7), "...");
    assert_eq!(loading_dots(8), "...");
    assert_eq!(loading_dots(16), "...");
    assert_eq!(loading_dots(24), "...");

    // DIM at the troughs, normal between, BOLD at the half-cycle peak.
    let fresh = 5 * 60;
    assert_eq!(attention_breath(0, fresh), Modifier::DIM);
    assert_eq!(attention_breath(6, fresh), Modifier::empty());
    assert_eq!(
        attention_breath(12, fresh),
        Modifier::BOLD,
        "peak at the half-cycle"
    );
    assert_eq!(attention_breath(18, fresh), Modifier::empty());
    assert_eq!(
        attention_breath(24, fresh),
        Modifier::DIM,
        "wraps to the trough"
    );
}
/// The breath paces with the age heat: yellow keeps the resting ~2.4s
/// triangle, amber runs the same wave at double-time (~1.2s), and red
/// drops the swell for a hard `BOLD`↔`DIM` blink flipping every third
/// tick — so the cadence alone carries the urgency under `NO_COLOR`.
#[test]
fn attention_breath_quickens_with_the_age_heat() {
    // Yellow (25m): the same wave as the fresh floor — slow.
    let yellow = 25 * 60;
    assert_eq!(attention_breath(0, yellow), Modifier::DIM);
    assert_eq!(attention_breath(12, yellow), Modifier::BOLD);

    // Amber (40m): double-time — the half-cycle peak lands at tick 6.
    let amber = 40 * 60;
    assert_eq!(attention_breath(0, amber), Modifier::DIM);
    assert_eq!(
        attention_breath(6, amber),
        Modifier::BOLD,
        "peak in half the time"
    );
    assert_eq!(
        attention_breath(12, amber),
        Modifier::DIM,
        "full cycle in 1.2s"
    );

    // Red (2h): a square wave — no normal mid-level, just BOLD↔DIM.
    let red = 2 * 60 * 60;
    assert_eq!(attention_breath(0, red), Modifier::BOLD);
    assert_eq!(
        attention_breath(2, red),
        Modifier::BOLD,
        "held through the half"
    );
    assert_eq!(
        attention_breath(3, red),
        Modifier::DIM,
        "hard flip, no gradient"
    );
    assert_eq!(attention_breath(5, red), Modifier::DIM);
    assert_eq!(attention_breath(6, red), Modifier::BOLD, "wraps");
}
/// The elapsed-age tone steps with the clock-fill quarters: the dim resting
/// weight through the first quarter (a resume still hits cache), yellow to
/// the half hour, amber beyond it, red past the hour — when a resume would
/// likely re-read the whole context uncached.
#[test]
fn activity_age_style_steps_with_the_clock_quarters() {
    let theme = Theme::fixed(false);
    let yellow = theme.style(Color::Yellow, Modifier::empty());
    let amber = theme.style(ORANGE, Modifier::empty());
    let red = theme.style(Color::Red, Modifier::empty());
    assert_eq!(activity_age_style(&theme, 60), theme.dim());
    assert_eq!(activity_age_style(&theme, 900), theme.dim());
    assert_eq!(
        activity_age_style(&theme, 901),
        yellow,
        "yellow from the second quarter"
    );
    assert_eq!(activity_age_style(&theme, 1800), yellow);
    assert_eq!(
        activity_age_style(&theme, 1801),
        amber,
        "amber past the half hour"
    );
    assert_eq!(activity_age_style(&theme, 3600), amber);
    assert_eq!(
        activity_age_style(&theme, 3601),
        red,
        "red once the cache is likely invalidated"
    );
}
/// The paused glyph is the media `pause` mark carrying the
/// text-presentation selector (`U+FE0E`), so it renders single-cell
/// monochrome and the cockpit columns never drift when it appears.
#[test]
fn paused_glyph_carries_the_text_presentation_selector() {
    assert_eq!(status_glyph(AgentStatus::Paused), PAUSED_GLYPH);
    let mut chars = PAUSED_GLYPH.chars();
    assert_eq!(chars.next(), Some('⏸'));
    assert_eq!(chars.next(), Some('\u{FE0E}'));
    assert_eq!(chars.next(), None);
    // Measured by ratatui's own layout width (the selector is zero-width),
    // it occupies exactly one cell like every other status glyph — so the
    // cockpit columns never drift when the `⏸` bucket appears.
    assert_eq!(Span::raw(PAUSED_GLYPH).width(), 1);
    assert_eq!(Span::raw(status_glyph(AgentStatus::Waiting)).width(), 1);
}
/// Paused rests in held amber — the attention family, but *not* the
/// bold, heating weight of `?`/`!`. It is attention-class yet parked, so
/// neglect never escalates it: even hours parked it stays amber, since
/// there is nothing to do until the provider recovers or the window resets.
#[test]
fn paused_rests_in_held_amber_and_never_reddens() {
    let theme = Theme::fixed(false);
    let style = status_style(&theme, AgentStatus::Paused);
    assert_eq!(style.fg, Some(Color::Indexed(179)));
    assert!(!style.add_modifier.contains(Modifier::BOLD));
    let long_parked = attention_glyph_style(&theme, AgentStatus::Paused, 2 * 60 * 60, 0, false);
    assert_eq!(long_parked.fg, Some(Color::Indexed(179)));
    assert!(!long_parked.add_modifier.contains(Modifier::BOLD));
}
/// A running agent animates the working fill; while its turn is still in
/// the pre-edit thinking phase it sparkles; a stalled agent (folded to
/// `Failed` upstream) and every other state takes the static glyph,
/// regardless of phase.
#[test]
fn agent_glyph_animates_only_active_states() {
    assert_eq!(
        agent_glyph(AgentStatus::Running, TurnPhase::Acting, 2),
        WORKING_FRAMES[2]
    );
    assert_eq!(
        agent_glyph(AgentStatus::Running, TurnPhase::Reasoning, 4),
        THINKING_FRAMES[1]
    );
    // The sparkle is the running-state indicator — a stale thinking bit on
    // a non-running agent never sparkles.
    assert_eq!(agent_glyph(AgentStatus::Idle, TurnPhase::Idle, 2), "○");
    assert_eq!(agent_glyph(AgentStatus::Waiting, TurnPhase::Idle, 2), "?");
    assert_eq!(agent_glyph(AgentStatus::Failed, TurnPhase::Idle, 2), "!");
    assert_eq!(agent_glyph(AgentStatus::Idle, TurnPhase::Idle, 2), "○");
    assert_eq!(agent_glyph(AgentStatus::Success, TurnPhase::Idle, 2), "✓");
}
