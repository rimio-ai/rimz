//! Unit tests for [`super`]: palette resolution, the gray/brand tone
//! ladder, depth-aware brand emission, and capability gating.

use super::*;
use crate::config::{SidebarAnimationsConfig, ThemeMode};

fn indices(palette: Palette) -> [Color; 13] {
    [
        palette.good,
        palette.warn,
        palette.caution,
        palette.alarm,
        palette.accent,
        palette.cool,
        palette.meta,
        palette.soft,
        palette.dim,
        palette.faint,
        palette.rule,
        palette.selection,
        palette.clay,
    ]
}

#[test]
fn default_const_matches_bundled_default() {
    assert_eq!(
        PaletteTones::DEFAULT,
        scheme::explicit_palette_tones(DEFAULT_SCHEME).expect("bundled default scheme resolves"),
        "PaletteTones::DEFAULT must mirror the bundled `{DEFAULT_SCHEME}` tones"
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
            Color::Indexed(210),
            Color::Indexed(210),
            Color::Indexed(117),
            Color::Indexed(111),
            Color::Indexed(141),
            Color::Indexed(103),
            Color::Indexed(60),
            Color::Indexed(238),
            Color::Indexed(237),
            Color::Indexed(111),
            Color::Indexed(173),
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
    assert_eq!(
        theme.style(Color::Green, Modifier::empty()).fg,
        Some(Color::Indexed(34))
    );
    assert_eq!(
        theme.style(Color::Red, Modifier::empty()).fg,
        Some(Color::Indexed(210))
    );
    assert_eq!(
        theme.style(Color::LightRed, Modifier::empty()).fg,
        Some(Color::Indexed(210))
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
        theme.style(Color::LightRed, Modifier::empty()).fg,
        Some(Color::Indexed(214))
    );
    assert_eq!(
        theme.style(Color::Yellow, Modifier::empty()).fg,
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
        truecolor.style(Color::Green, Modifier::empty()).fg,
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
        indexed.style(Color::Green, Modifier::empty()).fg,
        Some(Color::Indexed(nearest_xterm_index(0xa3, 0xbe, 0x8c)))
    );
}

#[test]
fn heat_tone_walks_good_to_alarm_across_stops() {
    let theme = Theme::fixed(false);
    // Four stops — good → warn → caution → alarm — at 0, ⅓, ⅔, 1; the
    // default scheme quantizes each tone to its xterm index. Endpoints clamp.
    let (good_r, good_g, good_b) = PaletteTones::DEFAULT.good;
    let good = Color::Indexed(nearest_xterm_index(good_r, good_g, good_b));
    assert_eq!(theme.heat_tone(-0.1), good);
    assert_eq!(theme.heat_tone(0.0), good);
    assert_eq!(theme.heat_tone(1.0 / 3.0), Color::Indexed(179));
    assert_eq!(theme.heat_tone(2.0 / 3.0), Color::Indexed(210));
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
    let truecolor_trough = truecolor.breathe(Color::Yellow, trough);
    let truecolor_peak = truecolor.breathe(Color::Yellow, peak);
    assert!(matches!(truecolor_trough.fg, Some(Color::Rgb(..))));
    assert!(matches!(truecolor_peak.fg, Some(Color::Rgb(..))));
    assert_ne!(truecolor_trough.fg, truecolor_peak.fg);
    assert!(truecolor_peak.add_modifier.is_empty());

    let indexed = Theme::fixed(false);
    let indexed_trough = indexed.breathe(Color::Yellow, trough);
    let indexed_peak = indexed.breathe(Color::Yellow, peak);
    assert!(matches!(indexed_trough.fg, Some(Color::Indexed(_))));
    assert!(matches!(indexed_peak.fg, Some(Color::Indexed(_))));
    assert_ne!(indexed_trough.fg, indexed_peak.fg);

    let plain = Theme::fixed(true);
    assert_eq!(plain.breathe(Color::Yellow, trough).fg, None);
    assert_eq!(
        plain.breathe(Color::Yellow, trough).add_modifier,
        Modifier::DIM
    );
    assert_eq!(
        plain.breathe(Color::Yellow, shallow_peak).add_modifier,
        Modifier::empty()
    );
    assert_eq!(
        plain.breathe(Color::Yellow, peak).add_modifier,
        Modifier::BOLD
    );
}

#[test]
fn indexed_breathe_uses_modifier_when_quantization_hides_the_color_step() {
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
        "the fallback modifier keeps a visible step when indexed color cannot move"
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
        theme.style(Color::Red, Modifier::empty()).fg,
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
    let (good_r, good_g, good_b) = PaletteTones::DEFAULT.good;
    assert_eq!(
        theme.style(Color::Green, Modifier::empty()).fg,
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
    assert_ne!(truecolor.money_tone(), truecolor.tone(Color::Green));
}

#[test]
fn gray_ladder_is_plain_when_lit_and_a_dim_weight_under_no_color() {
    let lit = Theme::fixed(false);
    for (style, index) in [(lit.soft(), 103), (lit.dim(), 60), (lit.faint(), 238)] {
        assert_eq!(style.fg, Some(Color::Indexed(index)));
        assert!(style.add_modifier.is_empty(), "no DIM attenuation when lit");
    }
    assert_eq!(lit.rule().fg, Some(Color::Indexed(237)));
    assert!(
        lit.rule().add_modifier.contains(Modifier::DIM),
        "rule rides faint's gray under the DIM attenuation"
    );

    let dark = Theme::fixed(true);
    for style in [dark.soft(), dark.dim(), dark.faint(), dark.rule()] {
        assert_eq!(style.fg, None);
        assert!(style.add_modifier.contains(Modifier::DIM));
    }

    let themed = Theme {
        palette: Palette::resolve_fixed(
            &SidebarThemeConfig {
                soft: Some(ThemeColor::Indexed(252)),
                ..SidebarThemeConfig::default()
            },
            ColorDepth::Indexed,
        ),
        ..Theme::default()
    };
    assert_eq!(themed.soft().fg, Some(Color::Indexed(252)));
}

#[test]
fn soft_brand_mutes_the_brand_hue_toward_the_body_tier() {
    let truecolor = Theme {
        depth: ColorDepth::Truecolor,
        palette: Palette::resolve_fixed(&SidebarThemeConfig::default(), ColorDepth::Truecolor),
        ..Theme::default()
    };
    let brand = Color::Rgb(CLAUDE_CLAY_RGB.0, CLAUDE_CLAY_RGB.1, CLAUDE_CLAY_RGB.2);
    let muted = truecolor.soft_brand(brand);
    assert!(matches!(muted.fg, Some(Color::Rgb(..))), "keeps a hue");
    assert!(muted.add_modifier.is_empty(), "no DIM attenuation when lit");
    assert_ne!(
        muted.fg,
        truecolor.soft().fg,
        "softened brand is not the flat soft gray"
    );
    assert_ne!(
        muted.fg,
        Some(brand),
        "softened brand is recessed below full brand"
    );

    let dark = Theme::fixed(true);
    assert_eq!(
        dark.soft_brand(brand),
        dark.soft(),
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
    let style = theme.style(Color::Red, Modifier::BOLD);
    assert_eq!(style.fg, None, "NO_COLOR suppresses even a themed tone");
    assert!(style.add_modifier.contains(Modifier::BOLD));

    let lit = Theme::fixed(false).chip(Color::Indexed(16), Color::Indexed(173), Modifier::BOLD);
    assert_eq!(lit.fg, Some(Color::Indexed(16)));
    assert_eq!(
        lit.bg,
        Some(Color::Indexed(173)),
        "brand fill passes through unmapped"
    );
    assert!(lit.add_modifier.contains(Modifier::BOLD));

    let dark = Theme::fixed(true).chip(Color::Indexed(16), Color::Indexed(173), Modifier::BOLD);
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
