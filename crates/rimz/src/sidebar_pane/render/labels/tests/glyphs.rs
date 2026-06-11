use super::*;

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
    assert_eq!(banded(1_000_000), theme.style(theme.clay(), Modifier::DIM));
    assert_eq!(banded(1_050_000), theme.style(theme.clay(), Modifier::DIM));

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

#[test]
fn attention_glyph_heats_with_the_age_clock_over_a_yellow_floor() {
    let theme = Theme::fixed(false);
    let yellow = theme.style(Color::Yellow, Modifier::BOLD).fg;
    let amber = theme.style(theme.clay(), Modifier::BOLD).fg;
    let red = theme.style(Color::Red, Modifier::BOLD).fg;

    // Both attention states floor at yellow while the age heat is still
    // resting — a row that needs a human never reads as dim chrome — then
    // step with the clock quarters. The glyph breathes, so its brightness
    // modifier varies by frame; only the color is asserted here.
    for status in [AgentStatus::Waiting, AgentStatus::Failed] {
        assert_eq!(
            agent_lead_style(&theme, status, TurnPhase::Idle, 5 * 60, 0, false).fg,
            yellow
        );
        assert_eq!(
            agent_lead_style(&theme, status, TurnPhase::Idle, 25 * 60, 0, false).fg,
            yellow
        );
        assert_eq!(
            agent_lead_style(&theme, status, TurnPhase::Idle, 31 * 60, 0, false).fg,
            amber
        );
        assert_eq!(
            agent_lead_style(&theme, status, TurnPhase::Idle, 61 * 60, 0, false).fg,
            red
        );
    }
    assert_eq!(
        agent_lead_style(
            &theme,
            AgentStatus::Idle,
            TurnPhase::Idle,
            2 * 60 * 60,
            0,
            false
        )
        .fg,
        agent_style_at(&theme, AgentStatus::Idle, 0).fg
    );
    assert_eq!(
        agent_lead_style(
            &theme,
            AgentStatus::Running,
            TurnPhase::Acting,
            2 * 60 * 60,
            0,
            false
        )
        .fg,
        agent_style_at(&theme, AgentStatus::Running, 0).fg
    );
}

#[test]
fn unread_glyph_blinks_without_losing_status_color_or_heat() {
    let theme = Theme::fixed(false);
    let read = agent_lead_style(
        &theme,
        AgentStatus::Success,
        TurnPhase::Idle,
        5 * 60,
        0,
        false,
    );
    let unread_on = agent_lead_style(
        &theme,
        AgentStatus::Success,
        TurnPhase::Idle,
        5 * 60,
        0,
        true,
    );
    let unread_off = agent_lead_style(
        &theme,
        AgentStatus::Success,
        TurnPhase::Idle,
        5 * 60,
        3,
        true,
    );
    let unread_wrap = agent_lead_style(
        &theme,
        AgentStatus::Success,
        TurnPhase::Idle,
        5 * 60,
        6,
        true,
    );

    assert_eq!(
        unread_on.fg,
        agent_style_at(&theme, AgentStatus::Success, 0).fg
    );
    assert_eq!(unread_on.add_modifier, Modifier::BOLD);
    assert_eq!(unread_off.add_modifier, Modifier::DIM);
    assert_eq!(unread_wrap.add_modifier, Modifier::BOLD);
    assert_ne!(read.add_modifier, unread_on.add_modifier);

    let unread_waiting_on = agent_lead_style(
        &theme,
        AgentStatus::Waiting,
        TurnPhase::Idle,
        5 * 60,
        0,
        true,
    );
    let unread_waiting_off = agent_lead_style(
        &theme,
        AgentStatus::Waiting,
        TurnPhase::Idle,
        5 * 60,
        3,
        true,
    );
    assert_eq!(
        unread_waiting_on.fg,
        theme.style(Color::Yellow, Modifier::empty()).fg
    );
    assert_eq!(unread_waiting_on.add_modifier, Modifier::BOLD);
    assert_eq!(unread_waiting_off.add_modifier, Modifier::DIM);

    let red_read = agent_lead_style(
        &theme,
        AgentStatus::Failed,
        TurnPhase::Idle,
        2 * 60 * 60,
        0,
        false,
    );
    assert_eq!(red_read.fg, theme.style(Color::Red, Modifier::empty()).fg);
    assert_eq!(red_read.add_modifier, hard_blink(0));
}

#[test]
fn animations_cycle_and_wrap() {
    let theme = Theme::fixed(false);
    let working = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];
    assert_eq!(working_glyph(&theme, 0), working[0]);
    assert_eq!(working_glyph(&theme, 3), working[3]);
    assert_eq!(working_glyph(&theme, working.len() as u64), working[0]);
    let thinking = [
        "⠁", "⠂", "⠄", "⡀", "⡈", "⡐", "⡠", "⣀", "⣁", "⣂", "⣄", "⣌", "⣔", "⣤", "⣥", "⣦", "⣮", "⣶",
        "⣷", "⣿", "⡿", "⠿", "⢟", "⠟", "⡛", "⠛", "⠫", "⢋", "⠋", "⠍", "⡉", "⠉", "⠑", "⠡", "⢁",
    ];
    assert_eq!(thinking_glyph(&theme, 0), thinking[0]);
    assert_eq!(thinking_glyph(&theme, 19), thinking[19]);
    assert_eq!(thinking_glyph(&theme, thinking.len() as u64), thinking[0]);
    let resolving = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    assert_eq!(resolver_glyph(&theme, resolving.len() as u64), resolving[0]);
    let compacting = ["▁", "▃", "▄", "▅", "▆", "▇", "▆", "▅", "▄", "▃"];
    assert_eq!(
        compacting_glyph(&theme, compacting.len() as u64),
        compacting[0]
    );
    let delegating = ["⢄", "⢂", "⢁", "⡁", "⡈", "⡐", "⡠"];
    assert_eq!(
        subagent_glyph(&theme, delegating.len() as u64),
        delegating[0]
    );
    assert_eq!(
        working_glyph(&theme, u64::MAX),
        working[(u64::MAX % working.len() as u64) as usize]
    );
}

#[test]
fn loading_dots_stay_static_while_attention_breath_paces_with_age() {
    assert_eq!(loading_dots(0), "...");
    assert_eq!(loading_dots(7), "...");
    assert_eq!(loading_dots(8), "...");
    assert_eq!(loading_dots(16), "...");
    assert_eq!(loading_dots(24), "...");

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

    let yellow = 25 * 60;
    assert_eq!(attention_breath(0, yellow), Modifier::DIM);
    assert_eq!(attention_breath(12, yellow), Modifier::BOLD);

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

#[test]
fn activity_age_style_steps_with_the_clock_quarters() {
    let theme = Theme::fixed(false);
    let yellow = theme.style(Color::Yellow, Modifier::empty());
    let amber = theme.style(theme.clay(), Modifier::empty());
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

#[test]
fn paused_glyph_is_single_cell_amber_and_never_heats() {
    let theme = Theme::fixed(false);
    assert_eq!(status_glyph(&theme, AgentStatus::Paused), PAUSED_GLYPH);
    let mut chars = PAUSED_GLYPH.chars();
    assert_eq!(chars.next(), Some('⏸'));
    assert_eq!(chars.next(), Some('\u{FE0E}'));
    assert_eq!(chars.next(), None);
    assert_eq!(Span::raw(PAUSED_GLYPH).width(), 1);
    assert_eq!(
        Span::raw(status_glyph(&theme, AgentStatus::Waiting)).width(),
        1
    );

    let style = status_style(&theme, AgentStatus::Paused);
    assert_eq!(style.fg, Some(Color::Indexed(179)));
    assert!(!style.add_modifier.contains(Modifier::BOLD));
    let long_parked = agent_lead_style(
        &theme,
        AgentStatus::Paused,
        TurnPhase::Idle,
        2 * 60 * 60,
        0,
        false,
    );
    assert_eq!(long_parked.fg, Some(Color::Indexed(179)));
    assert!(!long_parked.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn agent_glyph_animates_live_and_calm_status_frames() {
    let theme = Theme::fixed(false);
    assert_eq!(
        agent_glyph(&theme, AgentStatus::Running, TurnPhase::Acting, 2),
        "⣻"
    );
    assert_eq!(
        agent_glyph(&theme, AgentStatus::Running, TurnPhase::Reasoning, 1),
        "⠂"
    );
    // The thinking head is the running-state indicator — a stale thinking bit
    // on a non-running agent never changes the static status glyph.
    assert_eq!(
        agent_glyph(&theme, AgentStatus::Idle, TurnPhase::Idle, 2),
        "○"
    );
    assert_eq!(
        agent_glyph(&theme, AgentStatus::Waiting, TurnPhase::Idle, 2),
        "?"
    );
    assert_eq!(
        agent_glyph(&theme, AgentStatus::Failed, TurnPhase::Idle, 2),
        "!"
    );
    assert_eq!(
        agent_glyph(&theme, AgentStatus::Success, TurnPhase::Idle, 2),
        "✓"
    );

    let mut sidebar = crate::config::SidebarConfig::default();
    sidebar.animations.idle = Some(
        toml::from_str::<crate::config::AnimationSpec>("frames = \"AB\"\nspeed = \"fast\"\n")
            .expect("idle animation spec"),
    );
    sidebar.animations.success = Some(
        toml::from_str::<crate::config::AnimationSpec>("frames = \"XY\"\nspeed = \"fast\"\n")
            .expect("success animation spec"),
    );
    let custom = Theme::fixed_for_sidebar(false, &sidebar);
    assert_eq!(
        agent_glyph(&custom, AgentStatus::Idle, TurnPhase::Idle, 1),
        "B"
    );
    assert_eq!(
        agent_glyph(&custom, AgentStatus::Success, TurnPhase::Idle, 1),
        "Y"
    );
    assert_eq!(
        status_glyph(&custom, AgentStatus::Idle),
        "A",
        "legend/status summaries keep the representative still frame"
    );
}

#[test]
fn default_idle_glyph_has_no_foreground_color_but_keeps_modifiers() {
    let theme = Theme::fixed(false);
    assert_eq!(status_style(&theme, AgentStatus::Idle).fg, None);
    assert_eq!(agent_style_at(&theme, AgentStatus::Idle, 0).fg, None);
    assert_eq!(
        agent_lead_style(&theme, AgentStatus::Idle, TurnPhase::Idle, 5 * 60, 0, true),
        Style::default().add_modifier(Modifier::BOLD),
        "unread idle keeps the hard-blink weight without adding a color"
    );

    let mut sidebar = crate::config::SidebarConfig::default();
    sidebar.animations.idle = Some(
        toml::from_str::<crate::config::AnimationSpec>("color = \"good\"\n")
            .expect("idle color spec"),
    );
    let custom = Theme::fixed_for_sidebar(false, &sidebar);
    assert_eq!(
        status_style(&custom, AgentStatus::Idle),
        custom.style(Color::Green, Modifier::empty()),
        "an explicit idle color still paints the glyph"
    );
    assert_eq!(
        agent_lead_style(&custom, AgentStatus::Idle, TurnPhase::Idle, 5 * 60, 0, true),
        custom.style(Color::Green, Modifier::BOLD),
        "configured idle color survives unread hard-blink"
    );
}
