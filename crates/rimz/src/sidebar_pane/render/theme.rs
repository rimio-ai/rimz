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
//! selects a bundled Alacritty theme or an Alacritty TOML file, defaulting to
//! `TokyoNight Night`; per-slot overrides then win over the selected scheme. The
//! renderer resolves depth because terminal capability is a renderer-local fact.

use crate::config::{
    AnimationColor, ColorDepth, GlowMode, PaletteTones, SidebarConfig, SidebarThemeConfig,
    ThemeColor, nearest_xterm_index, xterm_rgb,
};
use ratatui::style::{Color, Modifier, Style};
use std::sync::OnceLock;

use super::animation::{AnimationRole, BreathSample, ResolvedAnimations};
use super::scheme;

pub(crate) const CLAUDE_CLAY_RGB: (u8, u8, u8) = (0xd9, 0x77, 0x57);
const MONEY_GREEN_RGB: (u8, u8, u8) = (0x85, 0xbb, 0x65);

/// How far an unselected card name's brand hue blends toward the soft body gray
/// (`0.0` = full brand, `1.0` = plain soft). Balanced: recessed to the body tier
/// while staying recognizably the provider's color.
const SOFT_BRAND_BLEND: f32 = 0.6;

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

/// The scheme that ships as the default look, drawn from the bundled Alacritty
/// catalog. `[sidebar.theme] scheme` left unset resolves to this. The baked-in
/// tones live in [`PaletteTones::DEFAULT`].
pub(crate) const DEFAULT_SCHEME: &str = "TokyoNight Night";

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
        Self::resolve_with_fallback(theme, depth, default_palette_tones())
    }

    pub(crate) fn resolve_fixed(theme: &SidebarThemeConfig, depth: ColorDepth) -> Palette {
        Self::resolve_with_fallback(theme, depth, PaletteTones::DEFAULT)
    }

    fn resolve_with_fallback(
        theme: &SidebarThemeConfig,
        depth: ColorDepth,
        fallback: PaletteTones,
    ) -> Palette {
        let tones = match theme.scheme.as_deref() {
            None => fallback,
            Some(name) => selected_palette_tones(name).unwrap_or(fallback),
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

/// The shipped default tones: [`DEFAULT_SCHEME`] resolved from the bundled
/// catalog, with the baked-in [`PaletteTones::DEFAULT`] as the backstop.
fn default_palette_tones() -> PaletteTones {
    scheme::explicit_palette_tones(DEFAULT_SCHEME).unwrap_or(PaletteTones::DEFAULT)
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

    /// Build a deterministic test theme. Tests use the default indexed palette
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

    /// The hard attention pulse: a grow-only lightness swell from the element's
    /// resting tone to a bright crest and back, held bold for the whole colored
    /// cycle so the blink stays smooth instead of snapping weight at the peak.
    /// It is stronger than the calm [`breathe`](Self::breathe). The glyph,
    /// name, and description of an unread row, and the `?`/`!`/`✓` make-up
    /// buckets, share one sample so they brighten in unison. `no_color` and a
    /// colorless `fg` keep the grow-only weight toggle as the fallback signal.
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
            .add_modifier(Modifier::BOLD)
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

    /// A provider brand tone softened to the body tier: the brand hue blended
    /// toward the soft body gray in OKLab, so a calm unselected card's name
    /// keeps its provider color while resting at the same recessed weight as the
    /// rest of the row. `no_color` keeps the soft `DIM` fallback — no hue
    /// survives there — and an unresolvable color falls back to plain `soft()`.
    pub(crate) fn soft_brand(&self, brand: Color) -> Style {
        if self.no_color {
            return self.soft();
        }
        match (
            color_to_rgb(self.resolve(brand)),
            color_to_rgb(self.palette.soft),
        ) {
            (Some(brand_rgb), Some(soft_rgb)) => {
                let muted = scheme::blend_oklab(brand_rgb, soft_rgb, SOFT_BRAND_BLEND);
                Style::default().fg(rgb_color(muted, self.depth))
            }
            _ => self.soft(),
        }
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
mod tests;
