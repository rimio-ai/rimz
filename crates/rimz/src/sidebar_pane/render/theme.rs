//! Capability-aware styling. Picks the palette depth and modifier set the
//! renderer is allowed to emit, so the grammar stays identical across tiers
//! while the chrome adapts.
//!
//! The default palette depth is automatic: truecolor terminals get RGB
//! palette tones, and other terminals get the same tones quantized to xterm
//! 256-color indexes. `NO_COLOR` strips color but keeps Unicode and
//! modifiers, so every gauge still reads by shape and fill. The color-only
//! transition effects pass ([`super::effects`]) remains a separate tier
//! controlled by `[sidebar] glow`: it runs only when glow permits it and
//! `NO_COLOR` is off.
//!
//! Palette choice is data in the snapshot's `[sidebar.theme]`: `scheme`
//! selects a built-in palette, a bundled Alacritty theme, or an Alacritty TOML
//! file; per-slot overrides then win over the selected scheme. The renderer
//! resolves depth because terminal capability is a renderer-local fact.

use crate::config::{
    AnimationColor, ColorDepth, GlowMode, SidebarConfig, SidebarThemeConfig, ThemeColor,
    nearest_xterm_index, xterm_rgb,
};
use ratatui::style::{Color, Modifier, Style};
use std::sync::OnceLock;

use super::animation::{AnimationRole, BreathSample, ResolvedAnimations};
use super::scheme;

pub(crate) const CLAUDE_CLAY_RGB: (u8, u8, u8) = (0xd9, 0x77, 0x57);
const MONEY_GREEN_RGB: (u8, u8, u8) = (0x85, 0xbb, 0x65);

/// Stops on the context **health** ramp, ordered calm → alarm:
/// `[good, warn, caution, alarm]` — green → gold → orange → rose-red. Prepending
/// the scheme's green to the warm trio widens the visible range so a filling
/// context reads as a health sweep at a glance, while every stop stays
/// scheme-tunable through its existing slot. [`Theme::heat_tone`] interpolates
/// across these in OKLab.
const HEAT_RAMP_STOPS: usize = 4;

/// Where the warm tail (`warn`) sits on the full ramp: the second of four stops,
/// i.e. one third of the way along. Scales whose "low" should read warm rather
/// than healthy-green — idle age, where fifteen minutes is stale, not optimal —
/// map their amount into `[HEAT_RAMP_WARM_START, 1.0]` via
/// [`Theme::warm_heat_tone`], reproducing the legacy warn → caution → alarm
/// sweep.
const HEAT_RAMP_WARM_START: f32 = 1.0 / (HEAT_RAMP_STOPS as f32 - 1.0);

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
    heat_ramp: [(u8, u8, u8); HEAT_RAMP_STOPS],
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
        Self::resolve_with_fallback(theme, depth, PaletteTones::CLAY)
    }

    pub(crate) fn resolve_fixed(theme: &SidebarThemeConfig, depth: ColorDepth) -> Palette {
        Self::resolve_with_fallback(theme, depth, PaletteTones::CLASSIC)
    }

    fn resolve_with_fallback(
        theme: &SidebarThemeConfig,
        depth: ColorDepth,
        fallback: PaletteTones,
    ) -> Palette {
        let tones = match theme.scheme.as_deref() {
            None => fallback,
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
                derived_rgb_slot(theme.good, tones.good),
                derived_rgb_slot(theme.warn, tones.warn),
                derived_rgb_slot(theme.caution, tones.caution),
                derived_rgb_slot(theme.alarm, tones.alarm),
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
    scheme::explicit_palette_tones(name)
}

fn theme_color(color: ThemeColor, depth: ColorDepth) -> Color {
    match color {
        ThemeColor::Indexed(index) => Color::Indexed(index),
        ThemeColor::Rgb(red, green, blue) => rgb_color((red, green, blue), depth),
    }
}

fn derived_rgb_slot(color: Option<ThemeColor>, builtin: (u8, u8, u8)) -> (u8, u8, u8) {
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

/// Piecewise OKLab interpolation across an N-stop ramp: `amount` ∈ `[0, 1]` maps
/// across the `N - 1` segments, blending within the active one. Endpoints clamp,
/// so `0.0` is the first stop and `1.0` the last. One blend regardless of stop
/// count — the ramp can grow or shrink without touching the math.
fn ramp_tone(ramp: &[(u8, u8, u8)], amount: f32) -> (u8, u8, u8) {
    match ramp {
        [] => (0, 0, 0),
        [only] => *only,
        _ => {
            let segments = (ramp.len() - 1) as f32;
            let scaled = amount.clamp(0.0, 1.0) * segments;
            let lower = (scaled.floor() as usize).min(ramp.len() - 2);
            scheme::blend_oklab(ramp[lower], ramp[lower + 1], scaled - lower as f32)
        }
    }
}

pub(super) fn color_to_rgb(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Reset => None,
        Color::Black => Some((0x00, 0x00, 0x00)),
        Color::Red => Some((0x80, 0x00, 0x00)),
        Color::Green => Some((0x00, 0x80, 0x00)),
        Color::Yellow => Some((0x80, 0x80, 0x00)),
        Color::Blue => Some((0x00, 0x00, 0x80)),
        Color::Magenta => Some((0x80, 0x00, 0x80)),
        Color::Cyan => Some((0x00, 0x80, 0x80)),
        Color::Gray => Some((0xc0, 0xc0, 0xc0)),
        Color::DarkGray => Some((0x80, 0x80, 0x80)),
        Color::LightRed => Some((0xff, 0x00, 0x00)),
        Color::LightGreen => Some((0x00, 0xff, 0x00)),
        Color::LightYellow => Some((0xff, 0xff, 0x00)),
        Color::LightBlue => Some((0x00, 0x00, 0xff)),
        Color::LightMagenta => Some((0xff, 0x00, 0xff)),
        Color::LightCyan => Some((0x00, 0xff, 0xff)),
        Color::White => Some((0xff, 0xff, 0xff)),
        Color::Indexed(index) if index < 16 => Some(ansi_index_rgb(index)),
        Color::Indexed(index) => Some(xterm_rgb(index)),
        Color::Rgb(red, green, blue) => Some((red, green, blue)),
    }
}

fn ansi_index_rgb(index: u8) -> (u8, u8, u8) {
    const ANSI: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0x80, 0x00, 0x00),
        (0x00, 0x80, 0x00),
        (0x80, 0x80, 0x00),
        (0x00, 0x00, 0x80),
        (0x80, 0x00, 0x80),
        (0x00, 0x80, 0x80),
        (0xc0, 0xc0, 0xc0),
        (0x80, 0x80, 0x80),
        (0xff, 0x00, 0x00),
        (0x00, 0xff, 0x00),
        (0xff, 0xff, 0x00),
        (0x00, 0x00, 0xff),
        (0xff, 0x00, 0xff),
        (0x00, 0xff, 0xff),
        (0xff, 0xff, 0xff),
    ];
    ANSI[index as usize]
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

    pub(crate) fn breathe(&self, fg: Color, sample: BreathSample) -> Style {
        if self.no_color {
            return Style::default().add_modifier(sample.modifier());
        }
        let resolved = self.resolve(fg);
        let Some(rgb) = color_to_rgb(resolved) else {
            return Style::default();
        };
        let lifted = scheme::lift_lightness(rgb, sample.lightness_delta());
        let color = rgb_color(lifted, self.depth);
        let mut style = Style::default().fg(color);
        if self.depth == ColorDepth::Indexed
            && sample.lightness_delta() != 0.0
            && color == rgb_color(rgb, self.depth)
        {
            style = style.add_modifier(sample.modifier());
        }
        style
    }

    /// The hard attention pulse: a grow-only swell from the element's resting
    /// tone up to a bright, bold crest and back — never dimming below rest. It
    /// lifts both the lightness **and** the weight together, in every depth, so
    /// the blink is unmistakable; stronger than the calm
    /// [`breathe`](Self::breathe) (a quiet truecolor wave with no weight). The
    /// glyph, name, and description of an unread row, and the `?`/`!` make-up
    /// buckets, share one sample so they swell in unison. `no_color` and a
    /// colorless `fg` keep the weight swell alone.
    pub(super) fn pulse(&self, fg: Color, sample: BreathSample) -> Style {
        if self.no_color {
            return Style::default().add_modifier(sample.grow_modifier());
        }
        let Some(rgb) = color_to_rgb(self.resolve(fg)) else {
            return Style::default().add_modifier(sample.grow_modifier());
        };
        let lifted = scheme::lift_lightness(rgb, sample.grow_delta());
        Style::default()
            .fg(rgb_color(lifted, self.depth))
            .add_modifier(sample.grow_modifier())
    }

    /// The body-text tone as a concrete color, so a pulsing description can lift
    /// and dim (and join the glow pass) instead of riding the terminal default
    /// the lightness shift cannot move.
    pub(super) fn soft_tone(&self) -> Color {
        self.palette.soft
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

    /// The context **health** tone for `amount` ∈ `[0, 1]`: a piecewise OKLab
    /// blend across the full `[good, warn, caution, alarm]` ramp, so `0.0` reads
    /// healthy green and `1.0` reads alarm red, with even perceptual steps
    /// between. Drives the context meter's health tone and severity glyph.
    pub(super) fn heat_tone(&self, amount: f32) -> Color {
        rgb_color(ramp_tone(&self.palette.heat_ramp, amount), self.depth)
    }

    /// The warm tail of the ramp (`warn` → `caution` → `alarm`) for `amount` ∈
    /// `[0, 1]`. Age and attention readers start warm — an idle agent is stale,
    /// not healthy — so they map into `[HEAT_RAMP_WARM_START, 1.0]` instead of
    /// the full green→red sweep the context meter owns.
    pub(super) fn warm_heat_tone(&self, amount: f32) -> Color {
        let mapped = HEAT_RAMP_WARM_START + amount.clamp(0.0, 1.0) * (1.0 - HEAT_RAMP_WARM_START);
        self.heat_tone(mapped)
    }

    /// Cache-write tone: the compaction/delegation violet, matching the color
    /// family the completed-compaction marker used before it moved to yellow.
    pub(super) fn cache_write_tone(&self) -> Color {
        self.animations.role(AnimationRole::Compacting).color()
    }

    /// Fresh-input tone: the same alarm red a 100% context-fill meter wears.
    pub(super) fn input_tone(&self) -> Color {
        self.heat_tone(1.0)
    }

    /// Money tone: a fixed dollar green emitted like provider brand colors —
    /// true RGB at truecolor depth, nearest xterm bucket at indexed depth.
    pub(crate) fn money_tone(&self) -> Color {
        rgb_color(MONEY_GREEN_RGB, self.depth)
    }

    pub(crate) fn money_style(&self, modifier: Modifier) -> Style {
        self.style(self.money_tone(), modifier)
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
    fn heat_tone_walks_good_to_alarm_across_stops() {
        let theme = Theme::fixed(false);
        // Four stops — good → warn → caution → alarm — at 0, ⅓, ⅔, 1; the
        // classic scheme quantizes warn/caution/alarm to the legacy indexes and
        // good to its green slot. Endpoints clamp.
        let good = Color::Indexed(nearest_xterm_index(0x8d, 0xbe, 0x8d));
        assert_eq!(theme.heat_tone(-0.1), good);
        assert_eq!(theme.heat_tone(0.0), good);
        assert_eq!(theme.heat_tone(1.0 / 3.0), Color::Indexed(179));
        assert_eq!(theme.heat_tone(2.0 / 3.0), Color::Indexed(173));
        assert_eq!(theme.heat_tone(1.0), Color::Indexed(167));
        assert_eq!(theme.heat_tone(1.1), Color::Indexed(167));
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
    fn truecolor_heat_gradient_sweeps_green_to_alarm() {
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
