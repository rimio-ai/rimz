//! Capability-aware styling. Picks the palette depth and modifier set the
//! renderer is allowed to emit, so the grammar stays identical across tiers
//! while the chrome adapts.
//!
//! The default palette depth is automatic: truecolor terminals get RGB
//! palette tones, and other terminals get the same tones quantized to xterm
//! 256-color indexes. `NO_COLOR` strips color but keeps Unicode and
//! modifiers, so every gauge still reads by shape and fill. The color-only
//! effects pass ([`super::effects`]) remains a separate tier controlled by
//! `[sidebar] glow`: it runs only when glow permits it and `NO_COLOR` is off.
//!
//! Palette choice is data in the snapshot's `[sidebar.theme]`: `scheme`
//! selects a built-in palette, a Ghostty-format theme file, or Ghostty's
//! active theme when set to `auto`; per-slot overrides then win over the
//! selected scheme. The renderer resolves depth because terminal capability
//! is a renderer-local fact.

use crate::config::{
    AnimationColor, ColorDepth, GlowMode, SidebarConfig, SidebarThemeConfig, ThemeColor,
    nearest_xterm_index, xterm_rgb,
};
use ratatui::style::{Color, Modifier, Style};
use std::sync::OnceLock;

use super::animation::ResolvedAnimations;
use super::scheme;

pub(crate) const CLAUDE_CLAY_RGB: (u8, u8, u8) = (0xd9, 0x77, 0x57);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PaletteTones {
    pub(crate) good: (u8, u8, u8),
    pub(crate) warn: (u8, u8, u8),
    pub(crate) caution: (u8, u8, u8),
    pub(crate) alarm: (u8, u8, u8),
    pub(crate) accent: (u8, u8, u8),
    pub(crate) cool: (u8, u8, u8),
    pub(crate) meta: (u8, u8, u8),
    pub(crate) soft: (u8, u8, u8),
    pub(crate) dim: (u8, u8, u8),
    pub(crate) faint: (u8, u8, u8),
    pub(crate) rule: (u8, u8, u8),
    pub(crate) selection: (u8, u8, u8),
}

impl PaletteTones {
    pub(crate) const CLAY: Self = Self {
        good: (0x96, 0xc2, 0x93),
        warn: (0xdf, 0xb6, 0x6d),
        caution: (0xe0, 0x91, 0x5c),
        alarm: (0xde, 0x6e, 0x6e),
        accent: (0x72, 0xb3, 0xaa),
        cool: (0x7f, 0xa8, 0xde),
        meta: (0xb4, 0x9b, 0xe0),
        soft: (0xa6, 0xa1, 0x9a),
        dim: (0x76, 0x71, 0x68),
        faint: (0x45, 0x42, 0x3d),
        rule: (0x34, 0x32, 0x30),
        selection: (0x8a, 0xb3, 0xe0),
    };

    pub(crate) const SLATE: Self = Self {
        good: (0x9e, 0xce, 0x6a),
        warn: (0xe0, 0xaf, 0x68),
        caution: (0xff, 0x9e, 0x64),
        alarm: (0xf7, 0x76, 0x8e),
        accent: (0x41, 0xa6, 0xb5),
        cool: (0x7a, 0xa2, 0xf7),
        meta: (0xbb, 0x9a, 0xf7),
        soft: (0xa9, 0xb1, 0xd6),
        dim: (0x56, 0x5f, 0x89),
        faint: (0x3b, 0x42, 0x61),
        rule: (0x29, 0x2e, 0x42),
        selection: (0x7a, 0xa2, 0xf7),
    };

    pub(crate) const CLASSIC: Self = Self {
        good: (0x8d, 0xbe, 0x8d),
        warn: (0xdc, 0xb1, 0x68),
        caution: (0xdc, 0x8c, 0x62),
        alarm: (0xdc, 0x66, 0x66),
        accent: (0x66, 0xb0, 0xb0),
        cool: (0x6b, 0xaa, 0xf5),
        meta: (0xb2, 0x8f, 0xf5),
        soft: (0x99, 0x99, 0x99),
        dim: (0x6e, 0x6e, 0x6e),
        faint: (0x46, 0x46, 0x46),
        // Keep classic@256 exactly on the legacy rule index 238; the rule's
        // DIM modifier supplies the darker visual step.
        rule: (0x46, 0x46, 0x46),
        selection: (0x8a, 0xb1, 0xdb),
    };
}

pub(crate) fn builtin_palette_tones(name: &str) -> Option<PaletteTones> {
    match name {
        "clay" => Some(PaletteTones::CLAY),
        "slate" => Some(PaletteTones::SLATE),
        "classic" => Some(PaletteTones::CLASSIC),
        _ => None,
    }
}

/// The active palette, one named slot per semantic tone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Palette {
    depth: ColorDepth,
    heat_ramp: [(u8, u8, u8); 3],
    good: Color,
    warn: Color,
    caution: Color,
    alarm: Color,
    accent: Color,
    cool: Color,
    meta: Color,
    soft: Color,
    dim: Color,
    faint: Color,
    rule: Color,
    selection: Color,
    clay: Color,
}

impl Palette {
    pub(crate) fn resolve(theme: &SidebarThemeConfig, depth: ColorDepth) -> Palette {
        Self::resolve_with_auto_fallback(theme, depth, PaletteTones::CLAY, true)
    }

    pub(crate) fn resolve_fixed(theme: &SidebarThemeConfig, depth: ColorDepth) -> Palette {
        Self::resolve_with_auto_fallback(theme, depth, PaletteTones::CLASSIC, false)
    }

    fn resolve_with_auto_fallback(
        theme: &SidebarThemeConfig,
        depth: ColorDepth,
        auto_fallback: PaletteTones,
        detect_auto_scheme: bool,
    ) -> Palette {
        let tones = match theme.scheme.as_deref() {
            None | Some("auto") if detect_auto_scheme => {
                scheme::auto_palette_tones().unwrap_or(auto_fallback)
            }
            None | Some("auto") => auto_fallback,
            Some(name) => selected_palette_tones(name).unwrap_or(PaletteTones::CLAY),
        };
        let slot = |override_color: Option<ThemeColor>, builtin| {
            override_color
                .map(|color| theme_color(color, depth))
                .unwrap_or_else(|| rgb_color(builtin, depth))
        };
        Palette {
            depth,
            heat_ramp: [
                heat_ramp_slot(theme.warn, tones.warn),
                heat_ramp_slot(theme.caution, tones.caution),
                heat_ramp_slot(theme.alarm, tones.alarm),
            ],
            good: slot(theme.good, tones.good),
            warn: slot(theme.warn, tones.warn),
            caution: slot(theme.caution, tones.caution),
            alarm: slot(theme.alarm, tones.alarm),
            accent: slot(theme.accent, tones.accent),
            cool: slot(theme.cool, tones.cool),
            meta: slot(theme.meta, tones.meta),
            soft: slot(theme.soft, tones.soft),
            dim: slot(theme.dim, tones.dim),
            faint: slot(theme.faint, tones.faint),
            rule: slot(theme.rule, tones.rule),
            selection: slot(theme.selection, tones.selection),
            clay: rgb_color(CLAUDE_CLAY_RGB, depth),
        }
    }

    pub(crate) fn animation_color(&self, color: AnimationColor) -> Color {
        match color {
            AnimationColor::Good => self.good,
            AnimationColor::Warn => self.warn,
            AnimationColor::Alarm => self.alarm,
            AnimationColor::Accent => self.accent,
            AnimationColor::Cool => self.cool,
            AnimationColor::Meta => self.meta,
            AnimationColor::Soft => self.soft,
            AnimationColor::Dim => self.dim,
            AnimationColor::Faint => self.faint,
            AnimationColor::Clay => self.clay,
            AnimationColor::Indexed(index) => Color::Indexed(index),
            AnimationColor::Rgb(red, green, blue) => rgb_color((red, green, blue), self.depth),
        }
    }
}

fn selected_palette_tones(name: &str) -> Option<PaletteTones> {
    builtin_palette_tones(name).or_else(|| scheme::explicit_palette_tones(name))
}

fn theme_color(color: ThemeColor, depth: ColorDepth) -> Color {
    match color {
        ThemeColor::Indexed(index) => Color::Indexed(index),
        ThemeColor::Rgb(red, green, blue) => rgb_color((red, green, blue), depth),
    }
}

fn heat_ramp_slot(color: Option<ThemeColor>, builtin: (u8, u8, u8)) -> (u8, u8, u8) {
    match color {
        Some(ThemeColor::Rgb(red, green, blue)) => (red, green, blue),
        Some(ThemeColor::Indexed(index)) if index >= 16 => xterm_rgb(index),
        Some(ThemeColor::Indexed(_)) | None => builtin,
    }
}

fn rgb_color((red, green, blue): (u8, u8, u8), depth: ColorDepth) -> Color {
    match depth {
        ColorDepth::Truecolor => Color::Rgb(red, green, blue),
        ColorDepth::Indexed => Color::Indexed(nearest_xterm_index(red, green, blue)),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Theme {
    no_color: bool,
    /// The terminal advertises 24-bit color (`COLORTERM`). Gates the
    /// post-render effects pass; palette depth has its own mode.
    truecolor: bool,
    depth: ColorDepth,
    glow: GlowMode,
    palette: Palette,
    pub(crate) animations: ResolvedAnimations,
}

impl Default for Theme {
    fn default() -> Self {
        let palette = Palette::resolve_fixed(&SidebarThemeConfig::default(), ColorDepth::Indexed);
        Self {
            no_color: false,
            truecolor: false,
            depth: ColorDepth::Indexed,
            glow: GlowMode::Auto,
            animations: ResolvedAnimations::resolve(
                &crate::config::SidebarAnimationsConfig::default(),
                &palette,
            ),
            palette,
        }
    }
}

impl Theme {
    /// The active theme for a frame: cached `NO_COLOR` and `COLORTERM`
    /// readings plus the palette, depth, and glow mode resolved from the
    /// snapshot's `[sidebar]` config.
    pub(crate) fn for_sidebar(sidebar: &SidebarConfig) -> Self {
        let truecolor = truecolor_env();
        let depth = sidebar.theme.mode.depth(truecolor);
        let palette = Palette::resolve(&sidebar.theme, depth);
        Self {
            no_color: crate::tui::no_color(),
            truecolor,
            depth,
            glow: sidebar.glow,
            animations: ResolvedAnimations::resolve(&sidebar.animations, &palette),
            palette,
        }
    }

    /// Build a deterministic test theme. Tests use the classic indexed palette
    /// unless they explicitly pass a sidebar config to [`Self::fixed_for_sidebar`].
    #[cfg(test)]
    pub(crate) fn fixed(no_color: bool) -> Self {
        let palette = Palette::resolve_fixed(&SidebarThemeConfig::default(), ColorDepth::Indexed);
        Self {
            no_color,
            truecolor: false,
            depth: ColorDepth::Indexed,
            glow: GlowMode::Auto,
            animations: ResolvedAnimations::resolve(
                &crate::config::SidebarAnimationsConfig::default(),
                &palette,
            ),
            palette,
        }
    }

    #[cfg(test)]
    pub(crate) fn fixed_for_sidebar(no_color: bool, sidebar: &SidebarConfig) -> Self {
        let depth = sidebar.theme.mode.depth(false);
        let palette = Palette::resolve_fixed(&sidebar.theme, depth);
        Self {
            no_color,
            truecolor: false,
            depth,
            glow: sidebar.glow,
            animations: ResolvedAnimations::resolve(&sidebar.animations, &palette),
            palette,
        }
    }

    pub(crate) fn effects_enabled(&self) -> bool {
        !self.no_color
            && match self.glow {
                GlowMode::Never => false,
                GlowMode::Always => true,
                GlowMode::Auto => self.truecolor,
            }
    }

    pub(crate) fn style(&self, fg: Color, modifier: Modifier) -> Style {
        let style = Style::default().add_modifier(modifier);
        if self.no_color {
            style
        } else {
            style.fg(self.resolve(fg))
        }
    }

    pub(crate) fn chip(&self, fg: Color, bg: Color, modifier: Modifier) -> Style {
        let style = Style::default().add_modifier(modifier);
        if self.no_color {
            style
        } else {
            style.fg(self.resolve(fg)).bg(self.resolve(bg))
        }
    }

    fn gray(&self, color: Color) -> Style {
        if self.no_color {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(color)
        }
    }

    pub(crate) fn dim(&self) -> Style {
        self.gray(self.palette.dim)
    }

    pub(crate) fn soft(&self) -> Style {
        self.gray(self.palette.soft)
    }

    pub(crate) fn faint(&self) -> Style {
        self.gray(self.palette.faint)
    }

    pub(crate) fn rule(&self) -> Style {
        self.style(self.palette.rule, Modifier::DIM)
    }

    pub(crate) fn selection(&self) -> Style {
        self.style(self.palette.selection, Modifier::BOLD)
    }

    pub(crate) fn clay(&self) -> Color {
        self.palette.clay
    }

    pub(super) fn heat_tone(&self, amount: f32) -> Color {
        let [warn, caution, alarm] = self.palette.heat_ramp;
        let rgb = if amount <= 0.0 {
            warn
        } else if amount >= 1.0 {
            alarm
        } else if amount < 0.5 {
            scheme::blend_oklab(warn, caution, amount * 2.0)
        } else {
            scheme::blend_oklab(caution, alarm, (amount - 0.5) * 2.0)
        };
        rgb_color(rgb, self.depth)
    }

    pub(crate) fn brand_tone(&self, panel: &crate::SidebarProviderPanel) -> Color {
        match (self.depth, panel.color_rgb) {
            (ColorDepth::Truecolor, Some((red, green, blue))) => Color::Rgb(red, green, blue),
            _ => Color::Indexed(panel.color),
        }
    }

    pub(super) fn tone(&self, color: Color) -> Color {
        self.resolve(color)
    }

    fn resolve(&self, color: Color) -> Color {
        match color {
            Color::Green => self.palette.good,
            Color::Yellow => self.palette.warn,
            Color::LightRed => self.palette.caution,
            Color::Red => self.palette.alarm,
            Color::Cyan => self.palette.accent,
            Color::Blue => self.palette.cool,
            Color::Magenta => self.palette.meta,
            Color::DarkGray | Color::Gray => self.palette.dim,
            other => other,
        }
    }
}

fn truecolor_env() -> bool {
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("COLORTERM").is_ok_and(|v| matches!(v.as_str(), "truecolor" | "24bit"))
    })
}

#[cfg(test)]
mod tests {
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
    fn classic_indexed_palette_matches_legacy_indices() {
        let palette = Palette::resolve_fixed(&SidebarThemeConfig::default(), ColorDepth::Indexed);
        assert_eq!(
            indices(palette),
            [
                Color::Indexed(108),
                Color::Indexed(179),
                Color::Indexed(173),
                Color::Indexed(167),
                Color::Indexed(73),
                Color::Indexed(75),
                Color::Indexed(141),
                Color::Indexed(246),
                Color::Indexed(242),
                Color::Indexed(238),
                Color::Indexed(238),
                Color::Indexed(110),
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
            Some(Color::Indexed(167))
        );
        assert_eq!(
            theme.style(Color::LightRed, Modifier::empty()).fg,
            Some(Color::Indexed(173))
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
                scheme: Some("classic".to_owned()),
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
                scheme: Some("classic".to_owned()),
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
    fn heat_tone_hits_endpoints_and_midpoint() {
        let theme = Theme::fixed(false);
        assert_eq!(theme.heat_tone(-0.1), Color::Indexed(179));
        assert_eq!(theme.heat_tone(0.0), Color::Indexed(179));
        assert_eq!(theme.heat_tone(0.5), Color::Indexed(173));
        assert_eq!(theme.heat_tone(1.0), Color::Indexed(167));
        assert_eq!(theme.heat_tone(1.1), Color::Indexed(167));
    }

    #[test]
    fn heat_tone_honors_interpolatable_overrides() {
        let truecolor = Theme::fixed_for_sidebar(
            false,
            &SidebarConfig {
                theme: SidebarThemeConfig {
                    mode: ThemeMode::Truecolor,
                    scheme: Some("classic".to_owned()),
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
                    scheme: Some("classic".to_owned()),
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
                    scheme: Some("classic".to_owned()),
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
                    scheme: Some("classic".to_owned()),
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
            Color::Indexed(167),
            "the ramp uses the scheme alarm because ANSI RGB is terminal-defined"
        );
    }

    #[test]
    fn truecolor_heat_gradient_changes_smoothly_toward_alarm() {
        let theme = Theme::fixed_for_sidebar(
            false,
            &SidebarConfig {
                theme: SidebarThemeConfig {
                    mode: ThemeMode::Truecolor,
                    scheme: Some("classic".to_owned()),
                    ..SidebarThemeConfig::default()
                },
                ..SidebarConfig::default()
            },
        );
        let sample = |minutes: i64| {
            let amount = super::super::age_heat_amount_for_test(minutes * 60);
            match theme.heat_tone(amount) {
                Color::Rgb(red, green, blue) => (red, green, blue),
                other => panic!("truecolor heat tone should be RGB, got {other:?}"),
            }
        };
        let samples: Vec<_> = [16, 20, 25, 30, 35, 40, 45, 50, 55, 60]
            .into_iter()
            .map(sample)
            .collect();
        assert!(
            samples.windows(2).all(|pair| pair[0] != pair[1]),
            "each five-minute truecolor sample should move: {samples:?}"
        );
        assert!(
            samples.windows(2).all(|pair| pair[0].1 > pair[1].1),
            "classic heat lowers green toward alarm: {samples:?}"
        );
    }

    #[test]
    fn builtin_schemes_resolve_by_name() {
        let sidebar = SidebarConfig {
            theme: SidebarThemeConfig {
                scheme: Some("slate".to_owned()),
                ..SidebarThemeConfig::default()
            },
            ..SidebarConfig::default()
        };
        let theme = Theme::fixed_for_sidebar(false, &sidebar);
        assert_eq!(
            theme.style(Color::Green, Modifier::empty()).fg,
            Some(Color::Indexed(nearest_xterm_index(0x9e, 0xce, 0x6a)))
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
    fn gray_ladder_is_plain_when_lit_and_a_dim_weight_under_no_color() {
        let lit = Theme::fixed(false);
        for (style, index) in [(lit.soft(), 246), (lit.dim(), 242), (lit.faint(), 238)] {
            assert_eq!(style.fg, Some(Color::Indexed(index)));
            assert!(style.add_modifier.is_empty(), "no DIM attenuation when lit");
        }
        assert_eq!(lit.rule().fg, Some(Color::Indexed(238)));
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
            let palette =
                Palette::resolve_fixed(&SidebarThemeConfig::default(), ColorDepth::Indexed);
            Theme {
                no_color,
                truecolor,
                depth: ColorDepth::Indexed,
                glow,
                animations: ResolvedAnimations::resolve(
                    &SidebarAnimationsConfig::default(),
                    &palette,
                ),
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
}
