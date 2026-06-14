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
    AnimationColor, ColorDepth, GlowMode, Semantic, SidebarConfig, SidebarThemeConfig, ThemeColor,
    nearest_xterm_index, xterm_rgb,
};
use crate::feed::AgentStatus;
use ratatui::style::{Color, Modifier, Style};
use std::sync::OnceLock;

use super::animation::{BreathSample, ResolvedAnimations};
use super::oklab;
use super::scheme;

mod component;
mod identity;
mod raw;

pub(crate) use component::Component;
pub(crate) use identity::Identity;
pub(crate) use raw::RawPalette;

/// How far a calm card name's brand lightness dims below full brand, in OKLab L
/// (`0.0` = full brand). Hue and saturation hold; only the lightness drops, so
/// the name reads a touch quieter while staying recognizably the provider's
/// color. A fixed step (rather than a blend toward the body tone) keeps the
/// recession visible for every brand, including one already at the body weight.
const SOFT_BRAND_DIM: f32 = 0.05;

/// How far the selected card's background band eases darker in OKLab lightness
/// from its bright `▌` spine (column 0) to the right rail — the "lit panel"
/// falloff. A subtle step: reads as depth on the selection anchor, never a
/// stripe. Truecolor only; the indexed cube is too coarse for the sub-cell ramp
/// and keeps the flat band.
const SELECTION_BAND_FALLOFF: f32 = 0.08;

/// How far the unread card wash pulls the dark selection panel toward the row's
/// status hue, as an OKLab blend fraction. The wash grounds on the same
/// `selection_bg` panel selection uses — so an unread card reads as a card panel,
/// one depth language with selection — then tints it toward `good` (a finished
/// `✓`), `warn` (a waiting `?`), or `alarm` (a failed `!`): far enough that the
/// hue is unmistakable at a scanning glance, short enough that the panel stays
/// dark behind the text. A whole-card surface carries the unread cue where a
/// one-cell glyph is too small to catch the eye, while staying perfectly still —
/// the fleet stays calm and the single lead row keeps the only motion.
const UNREAD_WASH_TINT: f32 = 0.42;

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

/// The fresh-input "expense" tone derives from `caution` the same way `caution`
/// itself derives from `warn` — [`oklab::warm_toward`], one step further toward
/// the alarm, then deepened a touch. `caution` is the gold `warn` rotated `0.22`
/// toward the red; rotating the resulting amber a further fraction toward the
/// alarm hue, enriching its chroma, and dropping its lightness a step lands a
/// deep, hot vermilion: costlier-looking than the bright amber tier, yet warmer
/// and quieter than the rose `alarm` so danger keeps its exclusive slot. The
/// deepen step also carries the separation at indexed depth — without it the
/// light amber and the light rose collapse into adjacent xterm cells. Levers:
/// raise `ROTATE` toward the alarm hue, `CHROMA` enriches where a scheme leaves
/// gamut room, `DEEPEN` makes it read hotter and lands its own indexed cell.
/// Tuned against a rendered frame.
const INPUT_EXPENSE_ROTATE: f32 = 0.40;
const INPUT_EXPENSE_CHROMA: f32 = 1.15;
const INPUT_EXPENSE_DEEPEN: f32 = -0.05;

/// The scheme that ships as the default look, drawn from the bundled Alacritty
/// catalog. `[sidebar.theme] scheme` left unset resolves to this. The baked-in
/// tones live in [`Semantic::DEFAULT`].
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
    /// The fresh-input cost tone — `caution` warmed further toward `alarm` into a
    /// vermilion, costlier-looking than the amber tier yet short of the danger
    /// alarm. Derived like `heat_ramp`, not a tunable slot.
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
    pub(crate) fn resolve(theme: &SidebarThemeConfig, depth: ColorDepth) -> Palette {
        Self::resolve_with_fallback(theme, depth, default_palette_tones())
    }

    pub(crate) fn resolve_fixed(theme: &SidebarThemeConfig, depth: ColorDepth) -> Palette {
        Self::resolve_with_fallback(theme, depth, Semantic::DEFAULT)
    }

    fn resolve_with_fallback(
        theme: &SidebarThemeConfig,
        depth: ColorDepth,
        fallback: Semantic,
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
        let heat_ramp = [
            derived_rgb_slot(theme.good, tones.good),
            derived_rgb_slot(theme.warn, tones.warn),
            derived_rgb_slot(theme.caution, tones.caution),
            derived_rgb_slot(theme.alarm, tones.alarm),
        ];
        // Reuse the ramp's already-derived caution (stop 2) and alarm (stop 3)
        // RGB to warm the fresh-input vermilion one step past the amber tier,
        // then deepen it so it reads hotter and lands its own cell at any depth.
        let expense = rgb_color(
            oklab::lift_lightness(
                oklab::warm_toward(
                    heat_ramp[2],
                    heat_ramp[3],
                    INPUT_EXPENSE_ROTATE,
                    INPUT_EXPENSE_CHROMA,
                ),
                INPUT_EXPENSE_DEEPEN,
            ),
            depth,
        );
        Palette {
            depth,
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
        }
    }
}

fn selected_palette_tones(name: &str) -> Option<Semantic> {
    scheme::explicit_palette_tones(name)
}

/// The shipped default tones: [`DEFAULT_SCHEME`] resolved from the bundled
/// catalog, with the baked-in [`Semantic::DEFAULT`] as the backstop.
fn default_palette_tones() -> Semantic {
    scheme::explicit_palette_tones(DEFAULT_SCHEME).unwrap_or(Semantic::DEFAULT)
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

    /// The selected card's background band tone for `column` of a `span`-cell
    /// card, or `None` under `NO_COLOR`. At truecolor the band reads as lit from
    /// its bright `▌` spine: column 0 holds the full `selection_bg` and each
    /// column eases up to [`SELECTION_BAND_FALLOFF`] darker in OKLab lightness
    /// toward the right rail, so the whole card reads as one lit panel — depth,
    /// no motion. At indexed depth the 6×6×6 cube is too coarse for that
    /// sub-cell ramp, so every column returns the flat dark fill (the
    /// subtle-step-falls-back-to-flat rule the breathe lift already follows),
    /// which lands on its own cube cell and paints at both indexed and
    /// truecolor. `None` under `NO_COLOR`, where the bright spine and bold weight
    /// carry the selection alone.
    pub(super) fn selection_band_at(&self, column: usize, span: usize) -> Option<Color> {
        if self.no_color {
            return None;
        }
        let base = self.palette.selection_bg;
        match self.depth {
            ColorDepth::Indexed => Some(base),
            ColorDepth::Truecolor => {
                let Some(rgb) = color_to_rgb(base) else {
                    return Some(base);
                };
                let t = if span <= 1 {
                    0.0
                } else {
                    column.min(span - 1) as f32 / (span - 1) as f32
                };
                let dimmed = oklab::lift_lightness(rgb, -t * SELECTION_BAND_FALLOFF);
                Some(rgb_color(dimmed, self.depth))
            }
        }
    }

    /// The flat selection-band tone composition lays behind every selected-card
    /// line — the spine-column reading of the band ([`selection_band_at`] at
    /// column 0), so the truecolor lift post-pass recognises a banded cell by
    /// this exact tone. `None` under `NO_COLOR`.
    ///
    /// [`selection_band_at`]: Self::selection_band_at
    pub(super) fn selection_band(&self) -> Option<Color> {
        self.selection_band_at(0, 1)
    }

    /// Whether the truecolor lit-panel band post-pass has anything to do: the
    /// per-column lightness falloff is a sub-cell step the indexed cube cannot
    /// carry, so it paints only at truecolor depth (and never under `NO_COLOR`,
    /// which drops the band entirely).
    pub(super) fn band_is_lit(&self) -> bool {
        !self.no_color && matches!(self.depth, ColorDepth::Truecolor)
    }

    /// The faint full-card background an unread card rests on, hued by what the
    /// row needs: the dark selection panel cast [`UNREAD_WASH_TINT`] toward the
    /// status tone — `good` for a finished `✓`, `warn` for a waiting `?`, `alarm`
    /// for a failed `!`. A whole-card surface reads at a scanning glance where a
    /// one-cell glyph cannot, and it holds still, so motion stays reserved to the
    /// single lead row. The tone is flat — the per-column lit falloff stays the
    /// selected card's signature, so an unread card never reads as selected, and
    /// [`super::lift_selection_band`] leaves it untouched. `None` under
    /// `NO_COLOR`, where weight carries the unread look, and for the calm states
    /// that never originate one ([`AgentStatus::marks_unread`]).
    pub(super) fn unread_wash(&self, status: AgentStatus) -> Option<Color> {
        if self.no_color {
            return None;
        }
        let hue = match status {
            AgentStatus::Success => self.palette.good,
            AgentStatus::Waiting => self.palette.warn,
            AgentStatus::Failed => self.palette.alarm,
            AgentStatus::Idle | AgentStatus::Running | AgentStatus::Paused => return None,
        };
        let (Some(ground), Some(tint)) =
            (color_to_rgb(self.palette.selection_bg), color_to_rgb(hue))
        else {
            return None;
        };
        Some(rgb_color(
            oklab::blend(ground, tint, UNREAD_WASH_TINT),
            self.depth,
        ))
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
        match (self.depth, panel.color_rgb) {
            (ColorDepth::Truecolor, Some((red, green, blue))) => Color::Rgb(red, green, blue),
            _ => Color::Indexed(panel.color),
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
