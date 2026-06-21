//! Layer 2 — palette resolution: the scheme's raw tones (Layer 1) derived
//! into the depth-resolved semantic slots the renderer paints, plus the heat
//! ramp and the fresh-input expense tone. Component tokens (Layer 3) and the
//! Theme facade read these slots; this is the one place depth quantization and
//! slot overrides are applied.

use crate::config::{
    AnimationColor, ColorDepth, PaletteRole, ThemeColor, ThemeConfig, nearest_xterm_index,
    xterm_rgb,
};
use ratatui::style::Color;

use super::super::{oklab, scheme};
use super::{Identity, RawPalette};

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
pub(super) const HEAT_RAMP_WARM_START: f32 = 1.0 / (HEAT_RAMP_STOPS as f32 - 1.0);

/// The fresh-input "expense" tone is the reddest marker in the sidebar: it sits
/// past the ramp's `alarm` stop, so the input read always reads redder than the
/// context bar's scaled-to-red cache-read run — even at a near-full window, where
/// that run reaches `alarm`. Take `alarm` (`heat_ramp[3]`, the ramp's reddest
/// stop) directly, enrich its chroma toward the gamut edge, and deepen its
/// lightness: a deep, hot red that holds alarm's hue but burns hotter than the
/// lighter rose. The deepen step also carries the separation at indexed depth —
/// without it a tone only a touch off the rose collapses into alarm's xterm cell.
/// Levers: `CHROMA` enriches where a scheme leaves gamut room; `DEEPEN` makes it
/// read hotter and lands its own indexed cell. Tuned against a rendered frame.
const INPUT_EXPENSE_CHROMA: f32 = 1.30;
const INPUT_EXPENSE_DEEPEN: f32 = -0.09;

/// The active palette, one named slot per semantic tone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Palette {
    depth: ColorDepth,
    raw: RawPalette,
    pub(super) heat_ramp: [(u8, u8, u8); HEAT_RAMP_STOPS],
    pub(super) good: Color,
    pub(super) warn: Color,
    pub(super) caution: Color,
    pub(super) alarm: Color,
    /// The fresh-input cost tone — `alarm` deepened a step past the ramp's red
    /// stop into the reddest marker on screen, so the costliest read always reads
    /// hotter than the bar's scaled-to-red health run. Derived like `heat_ramp`,
    /// not a tunable slot.
    pub(super) expense: Color,
    pub(super) accent: Color,
    pub(super) cool: Color,
    pub(super) meta: Color,
    pub(super) body: Color,
    pub(super) muted: Color,
    pub(super) faint: Color,
    pub(super) rule: Color,
    pub(super) selection: Color,
    pub(super) selection_bg: Color,
}

impl Palette {
    pub(crate) fn resolve(theme: &ThemeConfig, depth: ColorDepth) -> Palette {
        Self::resolve_with_raw(theme, depth, raw_palette_for_theme(theme))
    }

    fn resolve_with_raw(theme: &ThemeConfig, depth: ColorDepth, raw: RawPalette) -> Palette {
        let tones = raw.derive_tones();
        let slot = |override_color: Option<ThemeColor>, builtin| {
            override_color
                .map(|color| theme_color(color, depth, &raw))
                .unwrap_or_else(|| rgb_color(builtin, depth))
        };
        let heat_ramp = [
            derived_rgb_slot(theme.good, tones.good, &raw),
            derived_rgb_slot(theme.warn, tones.warn, &raw),
            derived_rgb_slot(theme.caution, tones.caution, &raw),
            derived_rgb_slot(theme.alarm, tones.alarm, &raw),
        ];
        // `alarm` (stop 3) is the ramp's reddest tone; the input read must read
        // redder still. Take it directly, enrich its chroma in place (a rotation
        // of zero holds the hue), then deepen its lightness — a hotter red on the
        // same hue that lands its own cell at any depth.
        let expense = rgb_color(
            oklab::lift_lightness(
                oklab::warm_toward(heat_ramp[3], heat_ramp[3], 0.0, INPUT_EXPENSE_CHROMA),
                INPUT_EXPENSE_DEEPEN,
            ),
            depth,
        );
        Palette {
            depth,
            raw,
            heat_ramp,
            good: slot(theme.good, tones.good),
            warn: slot(theme.warn, tones.warn),
            caution: slot(theme.caution, tones.caution),
            alarm: slot(theme.alarm, tones.alarm),
            expense,
            accent: slot(theme.accent, tones.accent),
            cool: slot(theme.cool, tones.cool),
            meta: slot(theme.meta, tones.meta),
            body: slot(theme.body, tones.body),
            muted: slot(theme.muted, tones.muted),
            faint: slot(theme.faint, tones.faint),
            rule: slot(theme.rule, tones.rule),
            selection: slot(theme.selection, tones.selection),
            selection_bg: slot(theme.selection_bg, tones.selection_bg),
        }
    }

    /// Resolve an external-identity tone at the palette's depth. The base hue is
    /// fixed; only the truecolor-vs-indexed emission differs.
    pub(crate) fn identity(&self, id: Identity) -> Color {
        rgb_color(id.base_rgb(), self.depth)
    }

    /// A palette-role tone at the palette's depth — a provider brand pinned to a
    /// scheme role tracks the active palette.
    pub(super) fn role_tone(&self, role: PaletteRole) -> Color {
        rgb_color(self.raw.role_rgb(role), self.depth)
    }

    pub(crate) fn animation_color(&self, color: AnimationColor) -> Color {
        match color {
            AnimationColor::Good => self.good,
            AnimationColor::Warn => self.warn,
            AnimationColor::Caution => self.caution,
            AnimationColor::Alarm => self.alarm,
            AnimationColor::Accent => self.accent,
            AnimationColor::Cool => self.cool,
            AnimationColor::Meta => self.meta,
            AnimationColor::Body => self.body,
            AnimationColor::Muted => self.muted,
            AnimationColor::Faint => self.faint,
            AnimationColor::Clay => self.identity(Identity::Claude),
            AnimationColor::Indexed(index) => Color::Indexed(index),
            AnimationColor::Rgb(red, green, blue) => rgb_color((red, green, blue), self.depth),
            AnimationColor::Role(role) => self.role_tone(role),
        }
    }
}

fn raw_palette_for_theme(theme: &ThemeConfig) -> RawPalette {
    theme
        .colors
        .as_ref()
        .and_then(|colors| scheme::inline_raw_palette(colors).ok())
        .or_else(|| {
            theme
                .scheme
                .as_deref()
                .and_then(scheme::explicit_raw_palette)
        })
        .unwrap_or_else(scheme::default_raw_palette)
}

fn theme_color(color: ThemeColor, depth: ColorDepth, raw: &RawPalette) -> Color {
    match color {
        ThemeColor::Role(role) => rgb_color(raw.role_rgb(role), depth),
        ThemeColor::Indexed(index) => Color::Indexed(index),
        ThemeColor::Rgb(red, green, blue) => rgb_color((red, green, blue), depth),
    }
}

fn derived_rgb_slot(
    color: Option<ThemeColor>,
    builtin: (u8, u8, u8),
    raw: &RawPalette,
) -> (u8, u8, u8) {
    match color {
        Some(ThemeColor::Role(role)) => raw.role_rgb(role),
        Some(ThemeColor::Rgb(red, green, blue)) => (red, green, blue),
        Some(ThemeColor::Indexed(index)) if index >= 16 => xterm_rgb(index),
        Some(ThemeColor::Indexed(_)) | None => builtin,
    }
}

pub(super) fn rgb_color((red, green, blue): (u8, u8, u8), depth: ColorDepth) -> Color {
    match depth {
        ColorDepth::Truecolor => Color::Rgb(red, green, blue),
        ColorDepth::Indexed => Color::Indexed(nearest_xterm_index(red, green, blue)),
    }
}

/// Piecewise OKLab interpolation across an N-stop ramp: `amount` ∈ `[0, 1]` maps
/// across the `N - 1` segments, blending within the active one. Endpoints clamp,
/// so `0.0` is the first stop and `1.0` the last. One blend regardless of stop
/// count — the ramp can grow or shrink without touching the math.
pub(super) fn ramp_tone(ramp: &[(u8, u8, u8)], amount: f32) -> (u8, u8, u8) {
    match ramp {
        [] => (0, 0, 0),
        [only] => *only,
        _ => {
            let segments = (ramp.len() - 1) as f32;
            let scaled = amount.clamp(0.0, 1.0) * segments;
            let lower = (scaled.floor() as usize).min(ramp.len() - 2);
            oklab::blend(ramp[lower], ramp[lower + 1], scaled - lower as f32)
        }
    }
}

pub(in crate::sidebar_pane::render) fn color_to_rgb(color: Color) -> Option<(u8, u8, u8)> {
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
