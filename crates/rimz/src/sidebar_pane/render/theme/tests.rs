//! Unit tests for [`super`]: palette resolution, the gray/brand tone
//! ladder, depth-aware brand emission, and capability gating.

use super::*;
use crate::config::{Semantic, SidebarAnimationsConfig, ThemeMode};

fn indices(palette: Palette) -> [Color; 13] {
    [
        palette.good,
        palette.warn,
        palette.caution,
        palette.alarm,
        palette.accent,
        palette.cool,
        palette.meta,
        palette.body,
        palette.muted,
        palette.faint,
        palette.rule,
        palette.selection,
        palette.selection_bg,
    ]
}

#[test]
fn default_const_matches_bundled_default() {
    assert_eq!(
        Semantic::DEFAULT,
        scheme::explicit_palette_tones(DEFAULT_SCHEME).expect("bundled default scheme resolves"),
        "Semantic::DEFAULT must mirror the bundled `{DEFAULT_SCHEME}` tones"
    );
}

#[test]
fn default_indexed_palette_matches_expected_indices() {
    let palette = Palette::resolve_fixed(&SidebarThemeConfig::default(), ColorDepth::Indexed);
    assert_eq!(
        indices(palette),
        [
            Color::Indexed(149),
            Color::Indexed(179),
            Color::Indexed(215),
            Color::Indexed(210),
            Color::Indexed(117),
            Color::Indexed(111),
            Color::Indexed(141),
            Color::Indexed(146),
            Color::Indexed(102),
            Color::Indexed(59),
            Color::Indexed(239),
            Color::Indexed(153),
            Color::Indexed(235),
        ]
    );
}

#[test]
fn palette_overrides_map_semantic_colors_without_remapping_brand_indices() {
    let theme = Theme {
        palette: Palette::resolve_fixed(
            &SidebarThemeConfig {
                good: Some(ThemeColor::Indexed(34)),
                ..SidebarThemeConfig::default()
            },
            ColorDepth::Indexed,
        ),
        ..Theme::default()
    };
    assert_eq!(theme.good(Modifier::empty()).fg, Some(Color::Indexed(34)));
    assert_eq!(theme.alarm(Modifier::empty()).fg, Some(Color::Indexed(210)));
    assert_eq!(
        theme.heat_tone(2.0 / 3.0),
        Color::Indexed(215),
        "the untouched caution slot still anchors the ramp's third stop"
    );
    assert_eq!(
        theme.style(Color::Indexed(173), Modifier::empty()).fg,
        Some(Color::Indexed(173))
    );

    let theme = Theme {
        palette: Palette::resolve_fixed(
            &SidebarThemeConfig {
                caution: Some(ThemeColor::Indexed(214)),
                ..SidebarThemeConfig::default()
            },
            ColorDepth::Indexed,
        ),
        ..Theme::default()
    };

    assert_eq!(
        theme.heat_tone(2.0 / 3.0),
        Color::Indexed(214),
        "the caution override flows through the ramp's third stop"
    );
    assert_eq!(
        theme.warn(Modifier::empty()).fg,
        Some(Color::Indexed(179)),
        "warning stays separate from elevated caution"
    );
}

#[test]
fn rgb_overrides_follow_depth() {
    let sidebar = SidebarConfig {
        theme: SidebarThemeConfig {
            mode: ThemeMode::Truecolor,
            good: Some(ThemeColor::Rgb(0xa3, 0xbe, 0x8c)),
            ..SidebarThemeConfig::default()
        },
        ..SidebarConfig::default()
    };
    let truecolor = Theme::fixed_for_sidebar(false, &sidebar);
    assert_eq!(
        truecolor.good(Modifier::empty()).fg,
        Some(Color::Rgb(0xa3, 0xbe, 0x8c))
    );

    let sidebar = SidebarConfig {
        theme: SidebarThemeConfig {
            good: Some(ThemeColor::Rgb(0xa3, 0xbe, 0x8c)),
            ..SidebarThemeConfig::default()
        },
        ..SidebarConfig::default()
    };
    let indexed = Theme::fixed_for_sidebar(false, &sidebar);
    assert_eq!(
        indexed.good(Modifier::empty()).fg,
        Some(Color::Indexed(nearest_xterm_index(0xa3, 0xbe, 0x8c)))
    );
}

#[test]
fn heat_tone_walks_good_to_alarm_across_stops() {
    let theme = Theme::fixed(false);
    // Four stops — good → warn → caution → alarm — at 0, ⅓, ⅔, 1; the
    // default scheme quantizes each tone to its xterm index. Endpoints clamp.
    let (good_r, good_g, good_b) = Semantic::DEFAULT.good;
    let good = Color::Indexed(nearest_xterm_index(good_r, good_g, good_b));
    assert_eq!(theme.heat_tone(-0.1), good);
    assert_eq!(theme.heat_tone(0.0), good);
    assert_eq!(theme.heat_tone(1.0 / 3.0), Color::Indexed(179));
    assert_eq!(theme.heat_tone(2.0 / 3.0), Color::Indexed(215));
    assert_eq!(theme.heat_tone(1.0), Color::Indexed(210));
    assert_eq!(theme.heat_tone(1.1), Color::Indexed(210));
}

#[test]
fn breathe_emits_color_depth_fallbacks() {
    let trough = BreathSample::new(
        0,
        24.0,
        crate::sidebar_pane::render::animation::BREATH_DEEP_AMPLITUDE,
    );
    let shallow_peak = BreathSample::new(
        12,
        24.0,
        crate::sidebar_pane::render::animation::BREATH_SHALLOW_AMPLITUDE,
    );
    let peak = BreathSample::new(
        12,
        24.0,
        crate::sidebar_pane::render::animation::BREATH_DEEP_AMPLITUDE,
    );

    let truecolor = Theme::fixed_for_sidebar(
        false,
        &SidebarConfig {
            theme: SidebarThemeConfig {
                mode: ThemeMode::Truecolor,
                ..SidebarThemeConfig::default()
            },
            ..SidebarConfig::default()
        },
    );
    let truecolor_trough = truecolor.breathe(Color::Indexed(179), trough);
    let truecolor_peak = truecolor.breathe(Color::Indexed(179), peak);
    assert!(matches!(truecolor_trough.fg, Some(Color::Rgb(..))));
    assert!(matches!(truecolor_peak.fg, Some(Color::Rgb(..))));
    assert_ne!(truecolor_trough.fg, truecolor_peak.fg);
    assert!(truecolor_peak.add_modifier.is_empty());

    let indexed = Theme::fixed(false);
    let indexed_trough = indexed.breathe(Color::Indexed(179), trough);
    let indexed_peak = indexed.breathe(Color::Indexed(179), peak);
    // The cube can't render the sub-cell lift, so indexed breathe holds the base
    // tone and pulses on weight instead: the trough dims, the deep peak bolds.
    assert_eq!(indexed_trough.fg, Some(Color::Indexed(179)));
    assert_eq!(indexed_peak.fg, Some(Color::Indexed(179)));
    assert_eq!(indexed_trough.add_modifier, Modifier::DIM);
    assert_eq!(indexed_peak.add_modifier, Modifier::BOLD);

    let plain = Theme::fixed(true);
    assert_eq!(plain.breathe(Color::Indexed(179), trough).fg, None);
    assert_eq!(
        plain.breathe(Color::Indexed(179), trough).add_modifier,
        Modifier::DIM
    );
    assert_eq!(
        plain
            .breathe(Color::Indexed(179), shallow_peak)
            .add_modifier,
        Modifier::empty()
    );
    assert_eq!(
        plain.breathe(Color::Indexed(179), peak).add_modifier,
        Modifier::BOLD
    );
}

#[test]
fn indexed_breathe_carries_the_pulse_as_weight_over_the_base_tone() {
    let indexed = Theme::fixed(false);
    let trough = BreathSample::new(
        0,
        24.0,
        crate::sidebar_pane::render::animation::BREATH_DEEP_AMPLITUDE,
    );
    let style = indexed.breathe(Color::Indexed(16), trough);
    assert_eq!(style.fg, Some(Color::Indexed(16)));
    assert!(
        style.add_modifier.contains(Modifier::DIM),
        "the 256-color cube can't render the sub-cell lift, so the pulse rides a weight modifier over the base tone"
    );
}

#[test]
fn heat_tone_honors_interpolatable_overrides() {
    let truecolor = Theme::fixed_for_sidebar(
        false,
        &SidebarConfig {
            theme: SidebarThemeConfig {
                mode: ThemeMode::Truecolor,
                alarm: Some(ThemeColor::Rgb(0xff, 0x00, 0x00)),
                ..SidebarThemeConfig::default()
            },
            ..SidebarConfig::default()
        },
    );
    assert_eq!(truecolor.heat_tone(1.0), Color::Rgb(0xff, 0x00, 0x00));

    let indexed_rgb = Theme::fixed_for_sidebar(
        false,
        &SidebarConfig {
            theme: SidebarThemeConfig {
                alarm: Some(ThemeColor::Rgb(0xff, 0x00, 0x00)),
                ..SidebarThemeConfig::default()
            },
            ..SidebarConfig::default()
        },
    );
    assert_eq!(
        indexed_rgb.heat_tone(1.0),
        Color::Indexed(nearest_xterm_index(0xff, 0x00, 0x00))
    );

    let indexed_xterm = Theme::fixed_for_sidebar(
        false,
        &SidebarConfig {
            theme: SidebarThemeConfig {
                alarm: Some(ThemeColor::Indexed(196)),
                ..SidebarThemeConfig::default()
            },
            ..SidebarConfig::default()
        },
    );
    assert_eq!(indexed_xterm.heat_tone(1.0), Color::Indexed(196));
}

#[test]
fn heat_tone_keeps_scheme_rgb_for_ansi_overrides() {
    let theme = Theme::fixed_for_sidebar(
        false,
        &SidebarConfig {
            theme: SidebarThemeConfig {
                alarm: Some(ThemeColor::Indexed(1)),
                ..SidebarThemeConfig::default()
            },
            ..SidebarConfig::default()
        },
    );
    assert_eq!(
        theme.alarm(Modifier::empty()).fg,
        Some(Color::Indexed(1)),
        "flat alarm uses the ANSI override"
    );
    assert_eq!(
        theme.heat_tone(1.0),
        Color::Indexed(210),
        "the ramp uses the scheme alarm because ANSI RGB is terminal-defined"
    );
}

#[test]
fn truecolor_heat_gradient_sweeps_green_to_alarm() {
    let theme = Theme::fixed_for_sidebar(
        false,
        &SidebarConfig {
            theme: SidebarThemeConfig {
                mode: ThemeMode::Truecolor,
                ..SidebarThemeConfig::default()
            },
            ..SidebarConfig::default()
        },
    );
    let sample = |amount: f32| match theme.heat_tone(amount) {
        Color::Rgb(red, green, blue) => (red, green, blue),
        other => panic!("truecolor heat tone should be RGB, got {other:?}"),
    };
    // Walk the full health ramp: every step moves, and green falls
    // monotonically from the healthy green start toward the alarm red end —
    // the perceptually-even sweep a filling context rides.
    let samples: Vec<_> = (0..=10).map(|step| sample(step as f32 / 10.0)).collect();
    assert!(
        samples.windows(2).all(|pair| pair[0] != pair[1]),
        "each tenth of the sweep should move: {samples:?}"
    );
    assert!(
        samples.windows(2).all(|pair| pair[0].1 > pair[1].1),
        "green falls from healthy green toward alarm: {samples:?}"
    );
}

#[test]
fn bundled_scheme_resolves_by_name() {
    let sidebar = SidebarConfig {
        theme: SidebarThemeConfig {
            scheme: Some("TokyoNight Night".to_owned()),
            ..SidebarThemeConfig::default()
        },
        ..SidebarConfig::default()
    };
    let theme = Theme::fixed_for_sidebar(false, &sidebar);
    let (good_r, good_g, good_b) = Semantic::DEFAULT.good;
    assert_eq!(
        theme.good(Modifier::empty()).fg,
        Some(Color::Indexed(nearest_xterm_index(good_r, good_g, good_b)))
    );
}

#[test]
fn provider_brand_tone_uses_rgb_only_at_truecolor_depth() {
    let panel = crate::SidebarProviderPanel {
        kind: "claude".to_owned(),
        product_name: "Claude".to_owned(),
        art: Vec::new(),
        color: 173,
        color_rgb: Some((0xd9, 0x77, 0x57)),
        version: None,
        plan: None,
        metered: false,
        remote_control: false,
        spending: None,
        extra_credits: None,
        windows: Vec::new(),
    };

    let indexed = Theme::fixed(false);
    assert_eq!(indexed.brand_tone(&panel), Color::Indexed(173));

    let truecolor = Theme::fixed_for_sidebar(
        false,
        &SidebarConfig {
            theme: SidebarThemeConfig {
                mode: ThemeMode::Truecolor,
                ..SidebarThemeConfig::default()
            },
            ..SidebarConfig::default()
        },
    );
    assert_eq!(truecolor.brand_tone(&panel), Color::Rgb(0xd9, 0x77, 0x57));
}

#[test]
fn money_tone_uses_fixed_dollar_green_at_active_depth() {
    let truecolor = Theme {
        depth: ColorDepth::Truecolor,
        palette: Palette::resolve_fixed(&SidebarThemeConfig::default(), ColorDepth::Truecolor),
        ..Theme::default()
    };
    let indexed = Theme::fixed(false);
    assert_eq!(truecolor.money_tone(), Color::Rgb(0x85, 0xbb, 0x65));
    assert_eq!(
        indexed.money_tone(),
        Color::Indexed(nearest_xterm_index(0x85, 0xbb, 0x65))
    );
    // The fixed dollar green is distinct from the semantic good/green slot.
    assert_ne!(truecolor.money_tone(), truecolor.heat_tone(0.0));
}

#[test]
fn gray_ladder_is_plain_when_lit_and_a_dim_weight_under_no_color() {
    let lit = Theme::fixed(false);
    for (style, index) in [(lit.body(), 146), (lit.muted(), 102), (lit.faint(), 59)] {
        assert_eq!(style.fg, Some(Color::Indexed(index)));
        assert!(style.add_modifier.is_empty(), "no DIM attenuation when lit");
    }
    assert_eq!(lit.rule().fg, Some(Color::Indexed(239)));
    assert!(
        lit.rule().add_modifier.contains(Modifier::DIM),
        "rule keeps a standing DIM over its own darkest gray (239), not faint's (59) — the one ladder tone still attenuated when lit"
    );

    let dark = Theme::fixed(true);
    for style in [dark.body(), dark.muted(), dark.faint(), dark.rule()] {
        assert_eq!(style.fg, None);
        assert!(style.add_modifier.contains(Modifier::DIM));
    }

    let themed = Theme {
        palette: Palette::resolve_fixed(
            &SidebarThemeConfig {
                body: Some(ThemeColor::Indexed(252)),
                ..SidebarThemeConfig::default()
            },
            ColorDepth::Indexed,
        ),
        ..Theme::default()
    };
    assert_eq!(themed.body().fg, Some(Color::Indexed(252)));
}

#[test]
fn soft_brand_dims_every_built_in_brand_keeping_its_hue() {
    let truecolor = Theme {
        depth: ColorDepth::Truecolor,
        palette: Palette::resolve_fixed(&SidebarThemeConfig::default(), ColorDepth::Truecolor),
        ..Theme::default()
    };
    // The three shipped provider brands: clay, Codex blue, Pi green. Pi sits at
    // the body weight, so a recession toward the body tone would vanish — the
    // fixed lightness step must still dim it visibly.
    let brands = [
        Identity::Claude.base_rgb(),
        (0x2f, 0xb1, 0xd1),
        (0x27, 0xa0, 0x77),
    ];
    for (red, green, blue) in brands {
        let brand = Color::Rgb(red, green, blue);
        let dimmed = truecolor.body_brand(brand);
        assert!(matches!(dimmed.fg, Some(Color::Rgb(..))), "keeps a hue");
        assert!(
            dimmed.add_modifier.is_empty(),
            "no DIM attenuation when lit"
        );
        assert_ne!(
            dimmed.fg,
            truecolor.body().fg,
            "the dimmed brand keeps its hue, not the flat soft gray"
        );
        assert_ne!(
            dimmed.fg,
            Some(brand),
            "the dimmed brand is distinguishable from full brand at truecolor"
        );
    }

    let dark = Theme::fixed(true);
    assert_eq!(
        dark.body_brand(Color::Rgb(brands[0].0, brands[0].1, brands[0].2)),
        dark.body(),
        "NO_COLOR keeps the soft DIM fallback — no hue survives"
    );
}

#[test]
fn no_color_strips_colors_from_styles_and_chips_but_keeps_modifiers() {
    let theme = Theme {
        no_color: true,
        palette: Palette::resolve_fixed(
            &SidebarThemeConfig {
                alarm: Some(ThemeColor::Indexed(196)),
                ..SidebarThemeConfig::default()
            },
            ColorDepth::Indexed,
        ),
        ..Theme::default()
    };
    let style = theme.alarm(Modifier::BOLD);
    assert_eq!(style.fg, None, "NO_COLOR suppresses even a themed tone");
    assert!(style.add_modifier.contains(Modifier::BOLD));

    let lit = Theme::fixed(false).chip(Color::Indexed(173), Modifier::BOLD);
    assert_eq!(
        lit.fg,
        Some(Color::Indexed(16)),
        "the chip lays the fixed near-black ink over the fill"
    );
    assert_eq!(
        lit.bg,
        Some(Color::Indexed(173)),
        "brand fill passes through unmapped"
    );
    assert!(lit.add_modifier.contains(Modifier::BOLD));

    let dark = Theme::fixed(true).chip(Color::Indexed(173), Modifier::BOLD);
    assert_eq!(dark.fg, None);
    assert_eq!(dark.bg, None, "NO_COLOR suppresses the chip fill too");
    assert!(dark.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn effects_follow_glow_mode_from_snapshot_and_no_color_beats_it() {
    let theme = |no_color, truecolor, glow| {
        let palette = Palette::resolve_fixed(&SidebarThemeConfig::default(), ColorDepth::Indexed);
        Theme {
            no_color,
            truecolor,
            depth: ColorDepth::Indexed,
            glow,
            animations: ResolvedAnimations::resolve(&SidebarAnimationsConfig::default(), &palette),
            palette,
        }
    };
    assert!(theme(false, true, GlowMode::Auto).effects_enabled());
    assert!(
        !theme(false, false, GlowMode::Auto).effects_enabled(),
        "auto on a terminal that advertises no truecolor stays plain"
    );
    assert!(
        theme(false, false, GlowMode::Always).effects_enabled(),
        "always forces the pass past a missing COLORTERM"
    );
    assert!(
        !theme(false, true, GlowMode::Never).effects_enabled(),
        "never pins the plain render on a truecolor terminal"
    );
    assert!(
        !theme(true, true, GlowMode::Always).effects_enabled(),
        "NO_COLOR beats every mode, the forced one included"
    );

    assert_eq!(
        Theme::for_sidebar(&SidebarConfig::default()).glow,
        GlowMode::Auto
    );
    let pinned_off = SidebarConfig {
        glow: GlowMode::Never,
        ..SidebarConfig::default()
    };
    let theme = Theme::for_sidebar(&pinned_off);
    assert_eq!(theme.glow, GlowMode::Never);
    assert!(!theme.effects_enabled());
}

fn truecolor_default() -> Theme {
    Theme::fixed_for_sidebar(
        false,
        &SidebarConfig {
            theme: SidebarThemeConfig {
                mode: ThemeMode::Truecolor,
                ..SidebarThemeConfig::default()
            },
            ..SidebarConfig::default()
        },
    )
}

/// The component-token oracle: every UI role resolves to the semantic slot it
/// names, at both palette depths, and always to a concrete tone (never `Reset`
/// or a raw carrier). The `match` below mirrors [`Component::resolve`]; the two
/// independent tables must agree, so a moved arm fails here, and a new variant
/// fails to compile until both are updated.
#[test]
fn component_golden_table_pins_every_role_to_its_slot_at_both_depths() {
    use Component::*;
    for theme in [Theme::fixed(false), truecolor_default()] {
        let p = theme.palette;
        for &component in Component::ALL {
            let expected = match component {
                Sessions | CacheRead | WindowHuge => p.accent,
                LaneSpine | FlashSelectionLanded => p.selection,
                WorktreeHeader | BranchDelta => p.body,
                LedgerLabel | TokenTotal | ProcCpu | WindowLarge => p.cool,
                SubagentHeader | RemoteControl | ProcIo | CacheWrite => p.meta,
                ProcMem | Output | FlashResolved | FlashLifted | FlashCompleted => p.good,
                Compaction | AttentionFloor | FlashWaiting => p.warn,
                Input => p.expense,
                FlashFailed => p.alarm,
                WindowMedium | UnknownBrand => p.muted,
                WindowSmall | CardRecede => p.faint,
            };
            let got = theme.component(component);
            assert_eq!(got, expected, "{component:?} resolves to its named slot");
            assert!(
                matches!(got, Color::Indexed(_) | Color::Rgb(..)),
                "{component:?} resolves to a concrete tone, got {got:?}"
            );
        }
    }
}

/// The fresh-input `expense` tone is a real step past the amber `caution` toward
/// the rose `alarm`, becoming neither: redder than caution (warming toward red
/// drops the green channel) yet warmer than the danger rose (an orange-red, so
/// its blue channel sits below the alarm's pinker blue). Locks the vermilion so a
/// future retune can't collapse it back to caution or overshoot the danger slot.
#[test]
fn expense_sits_between_caution_and_alarm() {
    let p = truecolor_default().palette;
    let expense = color_to_rgb(p.expense).expect("expense is a concrete tone");
    let caution = color_to_rgb(p.caution).expect("caution is a concrete tone");
    let alarm = color_to_rgb(p.alarm).expect("alarm is a concrete tone");

    assert_ne!(expense, caution, "expense is not the amber caution");
    assert_ne!(expense, alarm, "expense is not the danger alarm");
    assert!(
        expense.1 < caution.1,
        "expense reads redder than caution: {expense:?} vs {caution:?}"
    );
    assert!(
        expense.2 < alarm.2,
        "expense stays warmer (more orange) than the rose alarm: {expense:?} vs {alarm:?}"
    );

    // And it genuinely sits on the shortest-path hue arc from caution to alarm —
    // a partial rotation toward the rose, not a darker off-hue tone that merely
    // passes the channel checks above.
    let arc = |from: (u8, u8, u8), to: (u8, u8, u8)| {
        use crate::sidebar_pane::render::oklab::hue_angle;
        use std::f32::consts::{PI, TAU};
        let mut delta = hue_angle(to) - hue_angle(from);
        while delta > PI {
            delta -= TAU;
        }
        while delta < -PI {
            delta += TAU;
        }
        delta
    };
    let to_alarm = arc(caution, alarm);
    let to_expense = arc(caution, expense);
    assert!(
        to_expense.signum() == to_alarm.signum() && to_expense.abs() < to_alarm.abs(),
        "expense hue lies between caution and alarm: caution→expense {to_expense:.3}rad, \
         caution→alarm {to_alarm:.3}rad"
    );
}

/// Under `NO_COLOR` every component drops its hue but keeps the requested
/// modifier — the gauge/flash/marker still reads by shape and weight.
#[test]
fn components_collapse_to_modifier_only_under_no_color() {
    let plain = Theme::fixed(true);
    for &component in Component::ALL {
        let style = plain.styled(component, Modifier::BOLD);
        assert_eq!(style.fg, None, "{component:?} drops its hue under NO_COLOR");
        assert!(
            style.add_modifier.contains(Modifier::BOLD),
            "{component:?} keeps its modifier under NO_COLOR"
        );
    }
}

/// A perceptual-luminance proxy so the lit-band assertions can read "darker"
/// without reaching into the private OKLab type.
fn luminance(color: Color) -> f32 {
    let (red, green, blue) = color_to_rgb(color).expect("a concrete band tone");
    0.2126 * f32::from(red) + 0.7152 * f32::from(green) + 0.0722 * f32::from(blue)
}

#[test]
fn selection_band_recesses_flat_below_selection_bg_at_truecolor() {
    let theme = truecolor_default();
    let band = theme.selection_band().expect("a band at truecolor");
    // One flat tone, no gradient: the band recesses below the raw `selection_bg`, so
    // the selected card sinks into a well rather than rising as a bright panel.
    assert!(
        luminance(band) < luminance(theme.palette.selection_bg),
        "the band recesses below the raw selection_bg: {} !< {}",
        luminance(band),
        luminance(theme.palette.selection_bg),
    );
}

#[test]
fn indexed_band_and_wash_step_one_cell_either_side_of_the_panel() {
    let theme = Theme::fixed(false);
    let panel = theme.palette.selection_bg;
    let band = theme.selection_band().expect("a band at indexed depth");
    let wash = theme.unread_wash().expect("a wash at indexed depth");

    // The cube is too coarse for the truecolor sub-cell steps, so indexed depth
    // steps a whole xterm cell instead of collapsing onto the panel: the band one
    // cell darker, the wash one cell lighter, the panel's own cell between them.
    // Three distinct, ordered cells carry the truecolor ordering at the cube's
    // resolution. Pinned to the default scheme's gray-ramp neighbours so a retune of
    // `INDEXED_SELECTION_STEP` that re-collapses or overshoots fails here.
    assert_eq!(
        panel,
        Color::Indexed(235),
        "default panel lands on gray 235"
    );
    assert_eq!(band, Color::Indexed(234), "band steps one gray cell darker");
    assert_eq!(
        wash,
        Color::Indexed(236),
        "wash steps one gray cell lighter"
    );
    assert!(
        luminance(band) < luminance(panel) && luminance(panel) < luminance(wash),
        "band < panel < wash: {} < {} < {}",
        luminance(band),
        luminance(panel),
        luminance(wash),
    );
}

#[test]
fn selection_band_and_unread_wash_drop_under_no_color() {
    let plain = Theme::fixed(true);
    assert_eq!(plain.selection_band(), None);
    assert_eq!(plain.unread_wash(), None);
}

#[test]
fn unread_wash_is_a_lighter_tint_of_the_selection_blue() {
    let theme = truecolor_default();
    let selection = theme
        .selection_band()
        .expect("a selection band at truecolor");
    let wash = theme
        .unread_wash()
        .expect("an unread card washes at truecolor");

    // One uniform tone for every unread row — the status rides the glyph, not the
    // panel — and it is its own surface, never the selection band itself.
    assert_ne!(
        wash, selection,
        "the unread wash is not the selection panel"
    );

    // It is a *lighter* tint of the same blue: the unread marker takes the brighter
    // fill — the "needs you" surface — while the selection stays marked by its
    // bright spine rather than by the brightest fill.
    assert!(
        luminance(wash) > luminance(selection),
        "the unread wash is a lighter tint than the selection band"
    );

    // And it holds the selection's cool blue rather than drifting to a neutral gray
    // or a green cast: blue leads, green sits between, red trails — the panel's own
    // channel order, just lifted.
    let (red, green, blue) = color_to_rgb(wash).expect("a concrete wash tone");
    assert!(
        blue > green && green > red,
        "the wash stays in the scheme's cool-blue family: {wash:?}"
    );
}

#[test]
fn unread_wash_lifts_at_every_lit_depth_and_drops_under_no_color() {
    // The unread surface holds across depths: a lighter tint of the panel at
    // truecolor, one xterm cell lighter at indexed depth — both lifting above the
    // panel. NO_COLOR drops it and the unread bold weight carries the cue.
    let truecolor = truecolor_default();
    assert!(
        luminance(truecolor.unread_wash().expect("a truecolor wash"))
            > luminance(truecolor.palette.selection_bg),
        "truecolor wash lifts above the panel"
    );
    let indexed = Theme::fixed(false);
    assert!(
        luminance(indexed.unread_wash().expect("an indexed wash"))
            > luminance(indexed.palette.selection_bg),
        "indexed wash lifts above the panel"
    );
    assert_eq!(
        Theme::fixed(true).unread_wash(),
        None,
        "NO_COLOR drops the wash; weight carries the unread look"
    );
}
