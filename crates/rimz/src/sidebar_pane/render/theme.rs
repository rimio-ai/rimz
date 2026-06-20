//! Capability-aware styling. Picks the palette depth and modifier set the
//! renderer is allowed to emit, so the grammar stays identical across tiers
//! while the chrome adapts.
//!
//! The default palette depth is automatic: truecolor terminals get RGB
//! palette tones, and other terminals get the same tones quantized to xterm
//! 256-color indexes. `NO_COLOR` strips color but keeps Unicode and
//! modifiers, so every gauge still reads by shape and fill. The color-only
//! transition effects pass ([`super::effects`]) remains a separate tier
//! controlled by `[theme.display] glow`: it runs only when glow permits it and
//! `NO_COLOR` is off.
//!
//! Palette choice is data in the snapshot's `[theme]`: `scheme`
//! selects a bundled Alacritty theme or an Alacritty TOML file, defaulting to
//! `TokyoNight Night`; per-slot overrides then win over the selected scheme. The
//! renderer resolves depth because terminal capability is a renderer-local fact.

use crate::config::{
    AnimationColor, ColorDepth, GlowMode, GlyphRole, ThemeColor, ThemeConfig, nearest_xterm_index,
    xterm_rgb,
};
use ratatui::style::{Color, Modifier, Style};

use super::animation::{BreathSample, ResolvedAnimations};
use super::oklab;
use super::scheme;

mod component;
mod glyphs;
mod identity;
mod raw;

pub(crate) use component::Component;
pub(crate) use glyphs::{GlyphSet, GlyphSetKind};
pub(crate) use identity::Identity;
pub(crate) use raw::RawPalette;

/// How far a calm card name's brand lightness dims below full brand, in OKLab L
/// (`0.0` = full brand). Hue and saturation hold; only the lightness drops, so
/// the name reads a touch quieter while staying recognizably the provider's
/// color. A fixed step (rather than a blend toward the body tone) keeps the
/// recession visible for every brand, including one already at the body weight.
const SOFT_BRAND_DIM: f32 = 0.05;

/// The selected card's background band recesses a flat `SELECTION_BAND_DIM` below
/// `selection_bg` in OKLab lightness, so the selected card reads as a recessed well
/// marked by its bright `▌` spine, sitting clearly apart from the lighter unread
/// wash that rises above the card surface. This is the truecolor sub-cell step; the
/// indexed depth carries the same recess by stepping one xterm cell darker
/// ([`INDEXED_SELECTION_STEP`]).
const SELECTION_BAND_DIM: f32 = 0.05;

/// The one-cell OKLab-lightness step the indexed band and wash take from
/// `selection_bg`, sized to cross a single xterm cell so the cube carries the same
/// ordering the truecolor sub-cell steps draw: the selected band steps one cell
/// darker, the unread wash one cell lighter, and `selection_bg`'s own cell sits
/// between them. The truecolor `SELECTION_BAND_DIM`/`UNREAD_WASH_LIFT` steps are
/// finer than one cell, so the cube would collapse them onto the panel; this lift
/// is tuned (against the default scheme's near-background panel, which lands on the
/// fine 24-step gray ramp) to land cleanly on the neighbouring cell instead of
/// flattening. Symmetric: the band and wash sit one cell either side of the panel.
const INDEXED_SELECTION_STEP: f32 = 0.04;

/// The unread card wash: a soft, uniform background marking an unread row at a
/// scanning glance — the shade-marks-unread pattern of a mail inbox, with the row's
/// status carried by its `?`/`!`/`✓` glyph. It is a lighter tint of the selection
/// blue: the `selection_bg` panel lifted in OKLab lightness with its cool hue held,
/// landing on the same cool-blue family the scheme derives for the selection band,
/// one clear step brighter. The selected card keeps the attention through its
/// bright `▌` spine and recessed band, so the unread wash can take the
/// brighter fill — the "needs you" surface — while the selection band stays the
/// selected card's signature and wins when a card is both selected and unread. One
/// tone for every unread row, perfectly still: the fleet stays calm and the single
/// lead row keeps the only motion. This is the truecolor sub-cell step; the indexed
/// depth carries the same wash by stepping one xterm cell lighter
/// ([`INDEXED_SELECTION_STEP`]), so the unread surface holds across depths and
/// `NO_COLOR` leans on the unread bold weight alone. Lever: `UNREAD_WASH_LIFT`, the
/// lightness step above the selection band — a larger lift makes a brighter, more
/// present marker.
const UNREAD_WASH_LIFT: f32 = 0.01;

/// The chip ink: a fixed near-black laid on a colored chip fill, crisp on every
/// mid-brightness fill — the provider tab rail's brand fill and the make-up
/// bucket fills alike. Held fixed, not a palette slot.
const CHIP_INK: Color = Color::Indexed(16);

/// OKLab-L lift at which the shimmer beam reads as "lit" for the discrete
/// fallback: at 256-color and `NO_COLOR` depth the cell under the beam (within
/// roughly its inner half) turns bold while the rest stay plain, so the beam
/// still reads as motion where the cube cannot carry the sub-cell lift. Half the
/// beam crest.
const SHIMMER_BOLD_THRESHOLD: f32 = 0.04;

/// The money settle flash: a brighter sage than the resting dollar green, held
/// for a couple of frames as a value lands — the quiet "ka-chunk" of a count-up.
/// Shared by the cockpit headline and the agent cards' `$cost`. A fixed
/// identity-adjacent tone; drops to plain bold under `no_color` like every other.
const VALUE_FLASH_INK: Color = Color::Indexed(150);

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

/// The scheme that ships as the default look, drawn from the bundled Alacritty
/// catalog. `[theme] scheme` left unset resolves to this. The baked-in
/// tones live in [`Semantic::DEFAULT`].
pub(crate) const DEFAULT_SCHEME: &str = "TokyoNight Night";

/// The active palette, one named slot per semantic tone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Palette {
    depth: ColorDepth,
    raw: RawPalette,
    heat_ramp: [(u8, u8, u8); HEAT_RAMP_STOPS],
    good: Color,
    warn: Color,
    caution: Color,
    alarm: Color,
    /// The fresh-input cost tone — `alarm` deepened a step past the ramp's red
    /// stop into the reddest marker on screen, so the costliest read always reads
    /// hotter than the bar's scaled-to-red health run. Derived like `heat_ramp`,
    /// not a tunable slot.
    expense: Color,
    accent: Color,
    cool: Color,
    meta: Color,
    body: Color,
    muted: Color,
    faint: Color,
    rule: Color,
    selection: Color,
    selection_bg: Color,
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
            AnimationColor::Role(role) => rgb_color(self.raw.role_rgb(role), self.depth),
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
            oklab::blend(ramp[lower], ramp[lower + 1], scaled - lower as f32)
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
    /// The terminal advertises 24-bit color. Gates the
    /// post-render effects pass; palette depth has its own mode.
    truecolor: bool,
    depth: ColorDepth,
    glow: GlowMode,
    palette: Palette,
    glyphs: GlyphSet,
    pub(crate) animations: ResolvedAnimations,
}

impl Default for Theme {
    fn default() -> Self {
        let palette = Palette::resolve(&ThemeConfig::default(), ColorDepth::Indexed);
        let glyphs = GlyphSet::default();
        Self {
            no_color: false,
            truecolor: false,
            depth: ColorDepth::Indexed,
            glow: GlowMode::Auto,
            animations: ResolvedAnimations::resolve(
                &crate::config::ThemeAnimationsConfig::default(),
                &glyphs,
                &palette,
            ),
            glyphs,
            palette,
        }
    }
}

impl Theme {
    /// The active theme for a frame: cached terminal color-capability readings
    /// plus the palette, depth, and glow mode resolved from the snapshot's
    /// `[theme.display]` config.
    pub(crate) fn for_sidebar(theme: &ThemeConfig) -> Self {
        let truecolor = crate::tui::truecolor();
        let depth = theme.effective_theme_mode().depth(truecolor);
        let palette = Palette::resolve(theme, depth);
        let glyphs = GlyphSet::resolve_with_set(theme.glyph_set_source().as_deref(), &theme.glyphs);
        Self {
            no_color: crate::tui::no_color(),
            truecolor,
            depth,
            glow: theme.display.glow,
            animations: ResolvedAnimations::resolve(&theme.animations, &glyphs, &palette),
            glyphs,
            palette,
        }
    }

    /// Build a deterministic test theme. Tests use the default indexed palette
    /// unless they explicitly pass a theme config to [`Self::fixed_for_theme`].
    #[cfg(test)]
    pub(crate) fn fixed(no_color: bool) -> Self {
        let palette = Palette::resolve(&ThemeConfig::default(), ColorDepth::Indexed);
        let glyphs = GlyphSet::default();
        Self {
            no_color,
            truecolor: false,
            depth: ColorDepth::Indexed,
            glow: GlowMode::Auto,
            animations: ResolvedAnimations::resolve(
                &crate::config::ThemeAnimationsConfig::default(),
                &glyphs,
                &palette,
            ),
            glyphs,
            palette,
        }
    }

    #[cfg(test)]
    pub(crate) fn fixed_for_theme(no_color: bool, theme: &ThemeConfig) -> Self {
        let depth = theme.effective_theme_mode().depth(false);
        let palette = Palette::resolve(theme, depth);
        let glyphs = GlyphSet::resolve_with_set(theme.glyph_set_source().as_deref(), &theme.glyphs);
        Self {
            no_color,
            truecolor: false,
            depth,
            glow: theme.display.glow,
            animations: ResolvedAnimations::resolve(&theme.animations, &glyphs, &palette),
            glyphs,
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

    /// Emit a style at `fg` with `modifier`, dropping the color under
    /// `NO_COLOR`. `fg` is an already-resolved tone — a component token, a
    /// semantic accessor, an identity, or dynamic ramp output — never a raw
    /// terminal carrier (the `ensure_no_hardcoded_ui_colors` gate keeps render
    /// code honest), so there is no remap here.
    pub(crate) fn style(&self, fg: Color, modifier: Modifier) -> Style {
        let style = Style::default().add_modifier(modifier);
        if self.no_color { style } else { style.fg(fg) }
    }

    /// The resolved tone for a component token, at the active depth.
    pub(crate) fn component(&self, component: Component) -> Color {
        component.resolve(&self.palette)
    }

    pub(crate) fn glyph(&self, role: GlyphRole) -> &str {
        self.glyphs.glyph(role)
    }

    /// Which glyph preset is active — Unicode or Nerd Font. The mana bar reads
    /// this to choose between its box-drawing fill and the `nf-extra` progress
    /// segments; most render code routes through [`Self::glyph`] and never needs
    /// the kind.
    pub(crate) fn glyph_kind(&self) -> GlyphSetKind {
        self.glyphs.kind()
    }

    /// A component token's tone as a `Style` with `modifier`, honoring
    /// `NO_COLOR`.
    pub(crate) fn styled(&self, component: Component, modifier: Modifier) -> Style {
        self.style(self.component(component), modifier)
    }

    pub(crate) fn breathe(&self, fg: Color, sample: BreathSample) -> Style {
        if self.no_color {
            return Style::default().add_modifier(sample.modifier());
        }
        let Some(rgb) = color_to_rgb(fg) else {
            return Style::default();
        };
        // The breathing lift is a sub-cube-cell lightness step: truecolor renders
        // it as color, while the 256-color cube carries it as a weight modifier
        // over the base tone — the same shape `no_color` uses (see theme.md,
        // "Subtle steps and color depth").
        match self.depth {
            ColorDepth::Truecolor => {
                let lifted = oklab::lift_lightness(rgb, sample.lightness_delta());
                Style::default().fg(rgb_color(lifted, self.depth))
            }
            ColorDepth::Indexed => Style::default()
                .fg(rgb_color(rgb, self.depth))
                .add_modifier(sample.modifier()),
        }
    }

    /// The unread attention blink: a hard 2-pole brightness toggle between the
    /// element's resting tone and a bright crest. At truecolor the crest is a
    /// lightness lift, held bold the whole cycle so weight never flickers with the
    /// color. The 256-color cube can't carry that subtle crest, so indexed depth —
    /// like `no_color` and a colorless `fg` — toggles bold by pole over the base
    /// tone instead (see theme.md, "Subtle steps and color depth"). It is stronger
    /// than the calm [`breathe`](Self::breathe). The glyph, name, and description
    /// of an unread row, and the `?`/`!`/`✓` make-up buckets, share one sample so
    /// they flip in unison.
    pub(super) fn pulse(&self, fg: Color, sample: BreathSample) -> Style {
        if self.no_color {
            return Style::default().add_modifier(sample.grow_modifier());
        }
        let Some(rgb) = color_to_rgb(fg) else {
            return Style::default().add_modifier(sample.grow_modifier());
        };
        match self.depth {
            ColorDepth::Truecolor => {
                let lifted = oklab::lift_lightness(rgb, sample.grow_delta());
                Style::default()
                    .fg(rgb_color(lifted, self.depth))
                    .add_modifier(Modifier::BOLD)
            }
            ColorDepth::Indexed => Style::default()
                .fg(rgb_color(rgb, self.depth))
                .add_modifier(sample.grow_modifier()),
        }
    }

    /// One cell of the unread **shimmer** beam: a per-cell OKLab-L `lift` from
    /// [`shimmer_lift`](super::animation::shimmer_lift), so a beam flowing across
    /// an element brightens each cell in turn. At truecolor the lift rides the
    /// gamut-safe lightness raise (held bold under the crest); the 256-color cube
    /// can't carry the sub-cell step, so indexed depth — like `no_color` and a
    /// colorless `fg` — bolds the cells under the beam center over the base tone,
    /// a *moving* bold cell that reads as motion the cube carries honestly (see
    /// theme.md, "Subtle steps and color depth").
    pub(super) fn shimmer_cell(&self, fg: Option<Color>, lift: f32) -> Style {
        let bold = if lift >= SHIMMER_BOLD_THRESHOLD {
            Modifier::BOLD
        } else {
            Modifier::empty()
        };
        if self.no_color {
            return Style::default().add_modifier(bold);
        }
        let Some(rgb) = fg.and_then(color_to_rgb) else {
            return Style::default().add_modifier(bold);
        };
        match self.depth {
            ColorDepth::Truecolor => {
                let lifted = oklab::lift_lightness(rgb, lift);
                Style::default()
                    .fg(rgb_color(lifted, self.depth))
                    .add_modifier(Modifier::BOLD)
            }
            ColorDepth::Indexed => Style::default()
                .fg(rgb_color(rgb, self.depth))
                .add_modifier(bold),
        }
    }

    /// The body-text tone as a concrete color, so a pulsing description can lift
    /// and dim (and join the glow pass) instead of riding the terminal default
    /// the lightness shift cannot move.
    pub(super) fn body_tone(&self) -> Color {
        self.palette.body
    }

    /// A chip: a fixed near-black ink ([`CHIP_INK`]) on a colored fill. Under
    /// `no_color` the fill drops and only the modifier survives.
    pub(crate) fn chip(&self, bg: Color, modifier: Modifier) -> Style {
        let style = Style::default().add_modifier(modifier);
        if self.no_color {
            style
        } else {
            style.fg(CHIP_INK).bg(bg)
        }
    }

    pub(crate) fn pet_body_enabled(&self) -> bool {
        !self.no_color
    }

    /// A neutral chrome tone laid flat: the color as `fg`, or a `DIM` weight
    /// toggle under `no_color`. The shared body of [`body`](Self::body),
    /// [`muted`](Self::muted), and [`faint`](Self::faint).
    fn chrome(&self, color: Color) -> Style {
        if self.no_color {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(color)
        }
    }

    pub(crate) fn muted(&self) -> Style {
        self.chrome(self.palette.muted)
    }

    pub(crate) fn body(&self) -> Style {
        self.chrome(self.palette.body)
    }

    /// A provider brand tone for a calm card. At truecolor the brand keeps its
    /// full hue and saturation while its OKLab lightness dims one fixed step
    /// ([`SOFT_BRAND_DIM`]), so an unselected card's name rests a touch quieter
    /// than the full-brand selected/attention state. The 256-color cube is too
    /// coarse for that subtle step — its nearest darker cell is a hard
    /// ~40-per-channel jump that reads as a heavy darkening, not a soft
    /// recession — so indexed depth keeps the full brand and lets the selection
    /// bar and description carry the calm cue. `no_color`, and an unresolvable
    /// color, fall back to the plain `body()` tone.
    pub(crate) fn body_brand(&self, brand: Color) -> Style {
        if self.no_color {
            return self.body();
        }
        let Some(brand_rgb) = color_to_rgb(brand) else {
            return self.body();
        };
        let tone = match self.depth {
            ColorDepth::Indexed => brand_rgb,
            ColorDepth::Truecolor => oklab::lift_lightness(brand_rgb, -SOFT_BRAND_DIM),
        };
        Style::default().fg(rgb_color(tone, self.depth))
    }

    pub(crate) fn faint(&self) -> Style {
        self.chrome(self.palette.faint)
    }

    pub(crate) fn rule(&self) -> Style {
        self.style(self.palette.rule, Modifier::DIM)
    }

    pub(crate) fn selection(&self) -> Style {
        self.style(self.palette.selection, Modifier::BOLD)
    }

    /// A selection surface: `selection_bg` stepped in OKLab lightness, emitted at
    /// the active depth, or `None` under `NO_COLOR` where the bright spine and bold
    /// weight carry the cue alone. `truecolor_delta` is the sub-cell perceptual
    /// step the RGB path renders directly; `indexed_delta` is the one-cell step the
    /// cube needs to carry the same ordering ([`INDEXED_SELECTION_STEP`]) rather than
    /// collapse the finer step onto the panel. Shared by the recessed selected band
    /// and the lifted unread wash, which sit one step either side of the panel.
    fn selection_surface(&self, truecolor_delta: f32, indexed_delta: f32) -> Option<Color> {
        if self.no_color {
            return None;
        }
        // `selection_bg` always resolves to a concrete Indexed/Rgb tone, so this is
        // total in practice; `None` only guards a `Reset` that never reaches here.
        let rgb = color_to_rgb(self.palette.selection_bg)?;
        let delta = match self.depth {
            ColorDepth::Truecolor => truecolor_delta,
            ColorDepth::Indexed => indexed_delta,
        };
        Some(rgb_color(oklab::lift_lightness(rgb, delta), self.depth))
    }

    /// The selected card's background band, one flat tone behind every line of the
    /// card, or `None` under `NO_COLOR`. The band recesses below `selection_bg` in
    /// OKLab lightness, so the whole card reads as one recessed well — depth, no
    /// motion — set off from the lighter unread wash: a fine [`SELECTION_BAND_DIM`]
    /// sub-cell step at truecolor, one xterm cell darker
    /// ([`INDEXED_SELECTION_STEP`]) at indexed depth. `None` under `NO_COLOR`, where
    /// the bright spine and bold weight carry the selection alone.
    pub(super) fn selection_band(&self) -> Option<Color> {
        self.selection_surface(-SELECTION_BAND_DIM, -INDEXED_SELECTION_STEP)
    }

    /// The soft, uniform background an unread card rests on: a lighter tint of the
    /// selection blue — the `selection_bg` panel lifted in lightness with its cool
    /// hue held, so the whole card reads as the same cool-blue family as the
    /// selection band, one clear step brighter, the prominent "needs you" surface. A
    /// fine [`UNREAD_WASH_LIFT`] sub-cell step at truecolor, one xterm cell lighter
    /// ([`INDEXED_SELECTION_STEP`]) at indexed depth, and `None` under `NO_COLOR`
    /// where the unread bold weight carries the cue. One tone for every unread status
    /// — the row's meaning rides its `?`/`!`/`✓` glyph, the wash only says "unseen"
    /// — and it holds still, so motion stays reserved to the single lead row. The
    /// selected card keeps its identity through the bright `▌` spine and its recessed
    /// band, so the brighter unread fill never reads as selection; the wash is a
    /// distinct, lighter tone than the band, and the band wins when a card is both
    /// selected and unread. The caller applies it only to the unread look-worthy rows
    /// (the `Blink` card emphasis), so no status branch is needed here.
    pub(super) fn unread_wash(&self) -> Option<Color> {
        self.selection_surface(UNREAD_WASH_LIFT, INDEXED_SELECTION_STEP)
    }

    /// Flat health-tone accessors for fixed chrome: `good` for the positive tier
    /// (diff additions, trunk markers), `warn` for the attention/gate-notice
    /// floor, and `alarm` for the negative tier (diff removals, a spent budget's
    /// red track, a critical gate). The continuous green→red sweep lives in
    /// [`heat_tone`](Self::heat_tone) / [`warm_heat_tone`](Self::warm_heat_tone),
    /// so `caution` is reached only through the ramp and needs no flat accessor —
    /// as `accent`/`cool`/`meta` always name a [`Component`] rather than a tier.
    pub(crate) fn good(&self, modifier: Modifier) -> Style {
        self.style(self.palette.good, modifier)
    }

    pub(crate) fn warn(&self, modifier: Modifier) -> Style {
        self.style(self.palette.warn, modifier)
    }

    pub(crate) fn alarm(&self, modifier: Modifier) -> Style {
        self.style(self.palette.alarm, modifier)
    }

    /// An external-identity tone (brand clay, dollar green) at the active depth.
    pub(crate) fn identity(&self, id: Identity) -> Color {
        self.palette.identity(id)
    }

    pub(crate) fn clay(&self) -> Color {
        self.identity(Identity::Claude)
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

    /// Money tone: the fixed dollar green emitted like any identity tone —
    /// true RGB at truecolor depth, nearest xterm bucket at indexed depth.
    pub(crate) fn money_tone(&self) -> Color {
        self.identity(Identity::Money)
    }

    pub(crate) fn money_style(&self, modifier: Modifier) -> Style {
        self.style(self.money_tone(), modifier)
    }

    /// The bold money settle flash ([`VALUE_FLASH_INK`]) a count-up holds for a
    /// couple of frames as a figure lands. `no_color` keeps the bold weight.
    pub(crate) fn value_flash(&self) -> Style {
        self.style(VALUE_FLASH_INK, Modifier::BOLD)
    }

    pub(crate) fn brand_tone(&self, panel: &crate::SidebarProviderPanel) -> Color {
        if let Some(role) = panel.color_role {
            return rgb_color(self.palette.raw.role_rgb(role), self.depth);
        }
        match (self.depth, panel.color_rgb) {
            (ColorDepth::Truecolor, Some((red, green, blue))) => Color::Rgb(red, green, blue),
            _ => Color::Indexed(panel.color),
        }
    }
}

#[cfg(test)]
mod tests;
