use super::super::super::age_heat_amount_for_test;
use super::*;
use crate::sidebar_pane::render::animation::{BREATH_DEEP_AMPLITUDE, BreathSample};
use crate::sidebar_pane::render::theme::Component;

fn truecolor_theme() -> Theme {
    let mut sidebar = crate::config::SidebarConfig::default();
    sidebar.theme.mode = crate::config::ThemeMode::Truecolor;
    Theme::fixed_for_sidebar(false, &sidebar)
}

fn truecolor_theme_with(sidebar: &crate::config::SidebarConfig) -> Theme {
    let mut sidebar = sidebar.clone();
    sidebar.theme.mode = crate::config::ThemeMode::Truecolor;
    Theme::fixed_for_sidebar(false, &sidebar)
}

/// A sidebar config that pins the unread attention effect, so a test can drive
/// one specific mode rather than the shipped default (`shimmer`).
fn unread_sidebar(effect: crate::config::UnreadEffect) -> crate::config::SidebarConfig {
    let mut sidebar = crate::config::SidebarConfig::default();
    sidebar.animations.unread = Some(effect);
    sidebar
}

#[test]
fn card_emphasis_maps_attention_tiers() {
    for status in [
        AgentStatus::Waiting,
        AgentStatus::Failed,
        AgentStatus::Paused,
        AgentStatus::Success,
    ] {
        assert_eq!(card_emphasis(status, true, false), CardEmphasis::Blink);
        assert_eq!(card_emphasis(status, false, false), CardEmphasis::Normal);
    }

    assert_eq!(
        card_emphasis(AgentStatus::Running, false, true),
        CardEmphasis::Normal,
        "selection lifts non-attention rows to the normal tier"
    );
    for status in [AgentStatus::Running, AgentStatus::Idle] {
        assert_eq!(card_emphasis(status, true, false), CardEmphasis::Blink);
        assert_eq!(card_emphasis(status, false, false), CardEmphasis::Soft);
    }
}

#[test]
fn elapsed_glyph_fills_by_the_quarter_hour() {
    for (secs, glyph) in [
        (0, "◔"),
        (900, "◔"),
        (901, "◑"),
        (1800, "◑"),
        (1801, "◕"),
        (2700, "◕"),
        (2701, "●"),
        (3600, "●"),
        (3601, "◉"),
        (48 * 3600, "◉"),
    ] {
        assert_eq!(elapsed_glyph(secs), glyph, "elapsed_glyph({secs})");
    }
}

#[test]
fn window_style_tints_by_size_class_but_stays_subordinate() {
    let theme = Theme::fixed(false);
    let banded = |window| window_style(&theme, window);
    // A neutral→cool→accent salience ramp by size class — no provider brand.
    assert_eq!(
        banded(32_000),
        theme.styled(Component::WindowSmall, Modifier::DIM)
    );
    assert_eq!(
        banded(127_999),
        theme.styled(Component::WindowSmall, Modifier::DIM)
    );
    assert_eq!(
        banded(128_000),
        theme.styled(Component::WindowMedium, Modifier::DIM)
    );
    assert_eq!(
        banded(200_000),
        theme.styled(Component::WindowMedium, Modifier::DIM)
    );
    assert_eq!(
        banded(258_000),
        theme.styled(Component::WindowLarge, Modifier::DIM)
    );
    assert_eq!(
        banded(999_999),
        theme.styled(Component::WindowLarge, Modifier::DIM)
    );
    assert_eq!(
        banded(1_000_000),
        theme.styled(Component::WindowHuge, Modifier::DIM)
    );
    assert_eq!(
        banded(1_050_000),
        theme.styled(Component::WindowHuge, Modifier::DIM)
    );

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
fn attention_glyph_holds_a_fixed_tone_and_never_heats() {
    let theme = truecolor_theme();

    // The attention heads hold their fixed semantic tone at any age — waiting
    // yellow, failed red — never sliding with the clock. A read row (unread
    // false) carries that flat tone directly.
    for age_secs in [5 * 60, 25 * 60, 50 * 60, 61 * 60] {
        assert_eq!(
            agent_lead_style(
                &theme,
                AgentStatus::Waiting,
                TurnPhase::Idle,
                age_secs,
                0,
                false,
                false
            )
            .fg,
            theme.warn(Modifier::empty()).fg,
            "waiting holds the yellow tone at {age_secs}s",
        );
        assert_eq!(
            agent_lead_style(
                &theme,
                AgentStatus::Failed,
                TurnPhase::Idle,
                age_secs,
                0,
                false,
                false
            )
            .fg,
            theme.alarm(Modifier::empty()).fg,
            "failed holds alarm red at {age_secs}s",
        );
    }
    // A calm, unselected lead softens to the body tier with the name and
    // description. An idle lead carries no hue, so it rests at plain soft gray;
    // a running lead keeps its working color, muted toward the same tier.
    assert_eq!(
        agent_lead_style(
            &theme,
            AgentStatus::Idle,
            TurnPhase::Idle,
            2 * 60 * 60,
            0,
            false,
            false,
        )
        .fg,
        theme.body().fg
    );
    let working = agent_role_style_at(&theme, AgentStatus::Running, TurnPhase::Acting, 0)
        .fg
        .expect("a running lead carries a working color");
    let calm_running = agent_lead_style(
        &theme,
        AgentStatus::Running,
        TurnPhase::Acting,
        2 * 60 * 60,
        0,
        false,
        false,
    )
    .fg;
    assert_eq!(calm_running, theme.body_brand(working).fg);
    assert_ne!(
        calm_running,
        theme.body().fg,
        "a calm running lead keeps a muted working hue, not flat gray"
    );
}

#[test]
fn attention_effect_and_speed_reach_the_rendered_pulse() {
    let theme = truecolor_theme_with(&unread_sidebar(crate::config::UnreadEffect::Blink));
    assert_ne!(
        agent_lead_style(
            &theme,
            AgentStatus::Waiting,
            TurnPhase::Idle,
            5 * 60,
            0,
            true,
            false,
        )
        .fg,
        agent_lead_style(
            &theme,
            AgentStatus::Waiting,
            TurnPhase::Idle,
            5 * 60,
            18,
            true,
            false,
        )
        .fg,
        "the default unread actionable head blinks between its bright and normal poles"
    );

    let mut quiet = unread_sidebar(crate::config::UnreadEffect::Blink);
    quiet.animations.waiting = Some(
        toml::from_str::<crate::config::AnimationSpec>("effect = \"static\"\n")
            .expect("waiting animation spec"),
    );
    let quiet = truecolor_theme_with(&quiet);
    let quiet_start = agent_lead_style(
        &quiet,
        AgentStatus::Waiting,
        TurnPhase::Idle,
        5 * 60,
        0,
        true,
        false,
    );
    let quiet_later = agent_lead_style(
        &quiet,
        AgentStatus::Waiting,
        TurnPhase::Idle,
        5 * 60,
        12,
        true,
        false,
    );
    assert_eq!(
        quiet_start, quiet_later,
        "an explicit static waiting effect quiets the unread blink"
    );

    // A per-role static effect quiets the unread pulse for a result status too:
    // an explicit static success effect freezes the unread-result pulsing.
    let mut quiet_success = crate::config::SidebarConfig::default();
    quiet_success.animations.success = Some(
        toml::from_str::<crate::config::AnimationSpec>("effect = \"static\"\n")
            .expect("success animation spec"),
    );
    let quiet_success = truecolor_theme_with(&quiet_success);
    assert_eq!(
        agent_lead_style(
            &quiet_success,
            AgentStatus::Success,
            TurnPhase::Idle,
            5 * 60,
            0,
            true,
            false,
        ),
        agent_lead_style(
            &quiet_success,
            AgentStatus::Success,
            TurnPhase::Idle,
            5 * 60,
            12,
            true,
            false,
        ),
        "an explicit static success effect quiets unread-result pulsing"
    );

    let mut fast = unread_sidebar(crate::config::UnreadEffect::Blink);
    fast.animations.waiting = Some(
        toml::from_str::<crate::config::AnimationSpec>("speed = \"fast\"\n")
            .expect("waiting animation spec"),
    );
    let fast = truecolor_theme_with(&fast);
    assert_eq!(
        agent_lead_style(
            &fast,
            AgentStatus::Waiting,
            TurnPhase::Idle,
            5 * 60,
            1,
            true,
            false,
        )
        .fg,
        agent_lead_style(
            &theme,
            AgentStatus::Waiting,
            TurnPhase::Idle,
            5 * 60,
            2,
            true,
            false,
        )
        .fg,
        "configured speed modulates the unread blink phase"
    );
}

#[test]
fn unread_glyph_pulses_without_losing_status_color_or_heat() {
    let theme = truecolor_theme_with(&unread_sidebar(crate::config::UnreadEffect::Blink));
    let read = agent_lead_style(
        &theme,
        AgentStatus::Success,
        TurnPhase::Idle,
        5 * 60,
        0,
        false,
        false,
    );
    let unread_on = agent_lead_style(
        &theme,
        AgentStatus::Success,
        TurnPhase::Idle,
        5 * 60,
        0,
        true,
        false,
    );
    let unread_off = agent_lead_style(
        &theme,
        AgentStatus::Success,
        TurnPhase::Idle,
        5 * 60,
        18,
        true,
        false,
    );

    // Two-pole: the blink holds bold the whole colored cycle and hard-flips the
    // lightness between the bright on-pole and the resting off-pole, never below
    // rest, even in truecolor.
    assert_eq!(unread_on.add_modifier, Modifier::BOLD);
    assert_eq!(unread_off.add_modifier, Modifier::BOLD);
    assert_ne!(unread_on.fg, unread_off.fg);
    assert_ne!(read.fg, unread_on.fg);

    // A paused unread row carries the same shared attention effect: it blinks
    // between poles, holds BOLD across the cycle, and rests on its status color.
    let paused_on = agent_lead_style(
        &theme,
        AgentStatus::Paused,
        TurnPhase::Idle,
        5 * 60,
        0,
        true,
        false,
    );
    let paused_off = agent_lead_style(
        &theme,
        AgentStatus::Paused,
        TurnPhase::Idle,
        5 * 60,
        18,
        true,
        false,
    );
    assert_ne!(
        paused_on, paused_off,
        "paused unread rows carry the shared unread attention effect"
    );
    assert_eq!(paused_on.add_modifier, Modifier::BOLD);
    assert_eq!(paused_off.add_modifier, Modifier::BOLD);
    assert_eq!(
        paused_off,
        theme.style(
            theme.animations.status(AgentStatus::Paused).color(),
            Modifier::BOLD
        )
    );

    let read_waiting_peak = agent_lead_style(
        &theme,
        AgentStatus::Waiting,
        TurnPhase::Idle,
        5 * 60,
        12,
        false,
        false,
    );
    let unread_waiting_peak = agent_lead_style(
        &theme,
        AgentStatus::Waiting,
        TurnPhase::Idle,
        5 * 60,
        12,
        true,
        false,
    );
    assert_ne!(
        read_waiting_peak.fg, unread_waiting_peak.fg,
        "unread actionable rows use the deeper pulse amplitude"
    );

    let red_read = agent_lead_style(
        &theme,
        AgentStatus::Failed,
        TurnPhase::Idle,
        2 * 60 * 60,
        0,
        false,
        false,
    );
    assert_eq!(
        red_read.fg,
        theme.style(theme.heat_tone(1.0), Modifier::empty()).fg
    );
    assert_eq!(red_read.add_modifier, Modifier::empty());

    let plain = Theme::fixed_for_sidebar(true, &unread_sidebar(crate::config::UnreadEffect::Blink));
    let read_waiting_plain = agent_lead_style(
        &plain,
        AgentStatus::Waiting,
        TurnPhase::Idle,
        5 * 60,
        12,
        false,
        false,
    );
    let unread_waiting_plain = agent_lead_style(
        &plain,
        AgentStatus::Waiting,
        TurnPhase::Idle,
        5 * 60,
        12,
        true,
        false,
    );
    assert_eq!(read_waiting_plain.add_modifier, Modifier::empty());
    assert_eq!(unread_waiting_plain.add_modifier, Modifier::BOLD);
    // On the off-pole the blink rests at plain weight under NO_COLOR — never
    // DIM — and the `?`/`✓` shape carries the meaning.
    assert_eq!(
        agent_lead_style(
            &plain,
            AgentStatus::Waiting,
            TurnPhase::Idle,
            5 * 60,
            18,
            true,
            false,
        )
        .add_modifier,
        Modifier::empty()
    );
    assert_eq!(
        agent_lead_style(
            &plain,
            AgentStatus::Success,
            TurnPhase::Idle,
            5 * 60,
            18,
            true,
            false,
        )
        .add_modifier,
        Modifier::empty()
    );
    assert_eq!(
        agent_lead_style(
            &plain,
            AgentStatus::Success,
            TurnPhase::Idle,
            5 * 60,
            12,
            true,
            false,
        )
        .add_modifier,
        Modifier::BOLD
    );
}

#[test]
fn only_the_lead_unread_row_keeps_the_configured_effect() {
    let theme = truecolor_theme_with(&unread_sidebar(crate::config::UnreadEffect::Shimmer));
    // The lead unread row keeps the configured shimmer; every other unread row
    // settles to the steady bright crest — one pane in motion, the rest still.
    assert!(matches!(
        unread_anim(&theme, AgentStatus::Failed, 5 * 60, 6, true),
        Some(UnreadAnim::Shimmer(_)),
    ));
    assert!(matches!(
        unread_anim(&theme, AgentStatus::Failed, 5 * 60, 6, false),
        Some(UnreadAnim::Bright),
    ));

    // The name run follows the decision: the lead shimmers across one span per
    // cell, while a non-lead unread name holds a single bright span.
    let color = Some(theme.component(Component::UnknownBrand));
    let lead = CardAttention::new(&theme, AgentStatus::Failed, 5 * 60, 6, true, false, true);
    let lead_spans = unread_run_spans(&theme, color, lead.anim, "claude");
    assert!(lead_spans.len() > 1, "the lead name shimmers per cell");
    // Every character survives the split, and the beam lifts cells unevenly, so
    // the light reads as flowing across the run.
    let kept: usize = lead_spans
        .iter()
        .map(|span| span.content.chars().count())
        .sum();
    assert_eq!(kept, "claude".chars().count(), "every character is kept");
    let tones: std::collections::HashSet<_> = lead_spans.iter().map(|span| span.style.fg).collect();
    assert!(
        tones.len() > 1,
        "the beam lifts cells unevenly across the run, so the light reads as flowing"
    );

    // A non-lead unread name settles to the steady bright crest: one span, held
    // bold, and constant across phases — no motion.
    let calm_run = |phase| {
        let calm = CardAttention::new(
            &theme,
            AgentStatus::Failed,
            5 * 60,
            phase,
            true,
            false,
            false,
        );
        unread_run_spans(&theme, color, calm.anim, "claude")
    };
    let calm_early = calm_run(0);
    let calm_late = calm_run(99);
    assert_eq!(
        calm_early.len(),
        1,
        "a non-lead unread name holds one bright span",
    );
    assert_eq!(
        calm_early[0].style.add_modifier,
        Modifier::BOLD,
        "bright holds bold"
    );
    assert_eq!(
        calm_early[0].style, calm_late[0].style,
        "bright holds a constant crest across phases — no motion"
    );
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
fn loading_dots_stay_static_while_attention_blink_paces_with_age() {
    // The loading ellipsis never animates — one constant frame at any phase.
    assert_eq!(loading_dots(24), "...");

    let tempo = crate::sidebar_pane::render::animation::breath_tempo;
    assert!(tempo(5 * 60) > tempo(25 * 60));
    assert!(tempo(25 * 60) > tempo(50 * 60));
    assert_eq!(tempo(2 * 60 * 60), tempo(60 * 60));

    // The unread blink is a hard 2-pole flip, not a smooth pulse: the on-pole
    // sits at the bright crest, the off-pole rests at the normal tone (delta 0),
    // with nothing between them.
    let on = BreathSample::blink_for_age(0, 5 * 60, BREATH_DEEP_AMPLITUDE);
    assert!(
        on.grow_delta() > 0.0,
        "the on-pole lifts toward the bright crest"
    );
    let off_phase = (0..64)
        .find(|&phase| {
            BreathSample::blink_for_age(phase, 5 * 60, BREATH_DEEP_AMPLITUDE).grow_delta() == 0.0
        })
        .expect("the blink reaches its off-pole within a cycle");
    assert_eq!(
        BreathSample::blink_for_age(off_phase, 5 * 60, BREATH_DEEP_AMPLITUDE).grow_delta(),
        0.0,
        "the off-pole rests at the normal tone, never an eased value below it"
    );
}

#[test]
fn activity_age_style_slides_with_the_clock_age() {
    let theme = Theme::fixed(false);
    let red = theme.alarm(Modifier::empty());
    let heat = |age_secs: i64| {
        theme.style(
            theme.warm_heat_tone(age_heat_amount_for_test(age_secs)),
            Modifier::empty(),
        )
    };
    assert_eq!(activity_age_style(&theme, 60), theme.muted());
    assert_eq!(activity_age_style(&theme, 900), theme.muted());
    assert_eq!(
        activity_age_style(&theme, 901),
        heat(901),
        "the ramp starts after the first quarter"
    );
    assert_eq!(
        activity_age_style(&theme, 2250),
        theme.style(theme.warm_heat_tone(0.5), Modifier::empty()),
        "caution anchors the warm ramp's midpoint"
    );
    assert_eq!(activity_age_style(&theme, 3600), red);
    assert_eq!(
        activity_age_style(&theme, 3601),
        red,
        "alarm clamps once the cache is likely invalidated"
    );
}

#[test]
fn paused_glyph_is_single_cell_blue_and_never_heats() {
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

    // Paused wears the cool blue slot (which `TokenTotal` also aliases), held
    // flat — it never heats while parked.
    let blue = theme.component(Component::TokenTotal);
    let style = status_style(&theme, AgentStatus::Paused);
    assert_eq!(style.fg, Some(blue));
    assert!(!style.add_modifier.contains(Modifier::BOLD));
    let long_parked = agent_lead_style(
        &theme,
        AgentStatus::Paused,
        TurnPhase::Idle,
        2 * 60 * 60,
        0,
        false,
        false,
    );
    assert_eq!(long_parked.fg, Some(blue));
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
        agent_lead_style(
            &theme,
            AgentStatus::Idle,
            TurnPhase::Idle,
            5 * 60,
            0,
            true,
            false
        ),
        Style::default().add_modifier(Modifier::BOLD),
        "unread idle keeps terminal foreground color but adds the durable-unread emphasis"
    );
    assert_eq!(
        agent_lead_style(
            &theme,
            AgentStatus::Idle,
            TurnPhase::Idle,
            5 * 60,
            0,
            false,
            true
        ),
        Style::default(),
        "selected default idle is normal-weight terminal fg, not the Good tone"
    );

    let mut sidebar = crate::config::SidebarConfig::default();
    sidebar.animations.idle = Some(
        toml::from_str::<crate::config::AnimationSpec>("color = \"good\"\n")
            .expect("idle color spec"),
    );
    let custom = Theme::fixed_for_sidebar(false, &sidebar);
    assert_eq!(
        status_style(&custom, AgentStatus::Idle),
        custom.good(Modifier::empty()),
        "an explicit idle color still paints the glyph"
    );
    assert_eq!(
        agent_lead_style(
            &custom,
            AgentStatus::Idle,
            TurnPhase::Idle,
            5 * 60,
            0,
            true,
            true
        ),
        custom.good(Modifier::BOLD),
        "selected unread idle uses the configured idle color with durable-unread weight"
    );
}
