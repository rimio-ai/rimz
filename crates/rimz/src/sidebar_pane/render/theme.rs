//! Capability-aware terminal carrier and motion styling. Picks palette depth
//! and modifiers the renderer may emit, while shared theme resolution owns
//! semantic palettes, provider brands, and glyph vocabulary.
//!
//! The default palette depth is automatic: truecolor terminals get RGB
//! palette tones, and other terminals get the same tones quantized to xterm
//! 256-color indexes. `NO_COLOR` strips color but keeps Unicode and
//! modifiers, so every gauge still reads by shape and fill.
//!
//! Palette choice is data in the snapshot's `[theme]`: `scheme`
//! selects a bundled Alacritty theme or an Alacritty TOML file, defaulting to
//! `TokyoNight Night`; per-slot overrides then win over the selected scheme. The
//! renderer resolves depth because terminal capability is a renderer-local fact.

use std::collections::BTreeMap;

use crate::config::{
    ColorDepth, GlyphRole, HighlightStepsConfig, ThemeConfig, ThemeProviderStyle, xterm_rgb,
};
pub(crate) use crate::theme::Palette;
use crate::theme::{
    GlyphSet, GlyphSetKind, HEAT_RAMP_WARM_START, Identity, Tone, oklab, ramp_tone,
    resolve_provider_brand,
};
use ratatui::style::{Color, Modifier, Style};

use super::animation::{BreathSample, ResolvedAnimations};

mod component;

pub(crate) use component::Component;

/// How far a calm card name's brand lightness dims below full brand, in OKLab L
/// (`0.0` = full brand). Hue and saturation hold; only the lightness drops, so
/// the name reads a touch quieter while staying recognizably the provider's
/// color. A fixed step (rather than a blend toward the body tone) keeps the
/// recession visible for every brand, including one already at the body weight.
const SOFT_BRAND_DIM: f32 = 0.05;

/// One highlight step in OKLab lightness. `[theme.display.highlight_steps]`
/// counts the selected-band and unread-wash offsets in these units, so
/// `band = 5` is a 0.05 step.
const HIGHLIGHT_STEP_UNIT: f32 = 0.01;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Theme {
    no_color: bool,
    depth: ColorDepth,
    palette: Palette,
    provider_styles: BTreeMap<String, ThemeProviderStyle>,
    glyphs: GlyphSet,
    highlight_steps: HighlightStepsConfig,
    pub(crate) animations: ResolvedAnimations,
}

impl Default for Theme {
    fn default() -> Self {
        Self::assemble(false, ColorDepth::Indexed, &ThemeConfig::default())
    }
}

impl Theme {
    fn assemble(no_color: bool, depth: ColorDepth, theme: &ThemeConfig) -> Self {
        let palette = Palette::resolve(theme, depth);
        let glyphs = GlyphSet::resolve(theme);
        let animations = ResolvedAnimations::resolve(&theme.animations, &glyphs, &palette);
        Self {
            no_color,
            depth,
            animations,
            glyphs,
            palette,
            provider_styles: theme.providers.clone(),
            highlight_steps: theme.display.highlight_steps,
        }
    }

    /// The active theme for a frame: cached terminal color-capability readings
    /// plus the palette and depth resolved from the snapshot's `[theme]` config.
    pub(crate) fn for_sidebar(theme: &ThemeConfig) -> Self {
        let truecolor = crate::tui::truecolor();
        let depth = theme.effective_theme_mode().depth(truecolor);
        Self::assemble(crate::tui::no_color(), depth, theme)
    }

    /// Build a deterministic test theme. Tests use the default indexed palette
    /// unless they explicitly pass a theme config to [`Self::fixed_for_theme`].
    #[cfg(test)]
    pub(crate) fn fixed(no_color: bool) -> Self {
        Self::assemble(no_color, ColorDepth::Indexed, &ThemeConfig::default())
    }

    #[cfg(test)]
    pub(crate) fn fixed_for_theme(no_color: bool, theme: &ThemeConfig) -> Self {
        let depth = theme.effective_theme_mode().depth(false);
        Self::assemble(no_color, depth, theme)
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

    /// Resolve a tone for kitty pixel payloads only when this frame is allowed
    /// to carry truecolor. Indexed and `NO_COLOR` frames stay on the cell tier.
    pub(super) fn pixel_rgb(&self, color: Color) -> Option<[u8; 3]> {
        if self.no_color || self.depth != ColorDepth::Truecolor {
            return None;
        }
        match color {
            Color::Rgb(red, green, blue) => Some([red, green, blue]),
            _ => None,
        }
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

    /// One lifted cell at the active depth: truecolor rides the gamut-safe
    /// OKLab-L lift (with `truecolor_mod` held), while indexed, `no_color`, and
    /// a colorless `fg` carry `fallback_mod` over the base tone.
    fn lifted(
        &self,
        fg: Option<Color>,
        lift: f32,
        truecolor_mod: Modifier,
        fallback_mod: Modifier,
    ) -> Style {
        if self.no_color {
            return Style::default().add_modifier(fallback_mod);
        }
        let Some(rgb) = fg.and_then(color_to_rgb) else {
            return Style::default().add_modifier(fallback_mod);
        };
        match self.depth {
            ColorDepth::Truecolor => {
                let lifted = oklab::lift_lightness(rgb, lift);
                Style::default()
                    .fg(rgb_color(lifted, self.depth))
                    .add_modifier(truecolor_mod)
            }
            ColorDepth::Indexed => Style::default()
                .fg(rgb_color(rgb, self.depth))
                .add_modifier(fallback_mod),
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
        self.lifted(
            Some(fg),
            sample.grow_delta(),
            Modifier::BOLD,
            sample.grow_modifier(),
        )
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
        self.lifted(fg, lift, Modifier::BOLD, bold)
    }

    /// The body-text tone as a concrete color, so a pulsing description can lift
    /// and dim instead of riding the terminal default the lightness shift cannot
    /// move.
    pub(super) fn body_tone(&self) -> Color {
        tone_color(self.palette.body)
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

    /// A selected chip whose `no_color` fallback marks the same cells with
    /// reverse video.
    pub(crate) fn picked_chip(&self, bg: Color, modifier: Modifier) -> Style {
        let chip = self.chip(bg, modifier);
        if chip.bg.is_none() {
            chip.add_modifier(Modifier::REVERSED)
        } else {
            chip
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
        self.chrome(tone_color(self.palette.muted))
    }

    pub(crate) fn body(&self) -> Style {
        self.chrome(tone_color(self.palette.body))
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
        self.chrome(tone_color(self.palette.faint))
    }

    pub(crate) fn rule(&self) -> Style {
        self.style(tone_color(self.palette.rule), Modifier::DIM)
    }

    pub(crate) fn selection(&self) -> Style {
        self.style(tone_color(self.palette.selection), Modifier::BOLD)
    }

    fn step(&self, units: u8) -> f32 {
        f32::from(units) * HIGHLIGHT_STEP_UNIT
    }

    /// A selection surface: `selection_bg` stepped in OKLab lightness, emitted at
    /// the active depth, or `None` under `NO_COLOR` where the bright spine and bold
    /// weight carry the cue alone. `truecolor_delta` is the sub-cell perceptual
    /// step the RGB path renders directly; `indexed_delta` is the one-cell step
    /// `[theme.display.highlight_steps].indexed` gives the cube to carry the same
    /// ordering rather than collapse the finer step onto the panel. Shared by the
    /// recessed selected band and the lifted unread wash, which sit one step either
    /// side of the panel.
    fn selection_surface(&self, truecolor_delta: f32, indexed_delta: f32) -> Option<Color> {
        if self.no_color {
            return None;
        }
        // `selection_bg` always resolves to a concrete Indexed/Rgb tone, so this is
        // total in practice; `None` only guards a `Reset` that never reaches here.
        let rgb = color_to_rgb(tone_color(self.palette.selection_bg))?;
        let delta = match self.depth {
            ColorDepth::Truecolor => truecolor_delta,
            ColorDepth::Indexed => indexed_delta,
        };
        Some(rgb_color(oklab::lift_lightness(rgb, delta), self.depth))
    }

    /// The selected card's background band, one flat tone behind every line of the
    /// card, or `None` under `NO_COLOR`. The band recesses below `selection_bg` in
    /// OKLab lightness, so the whole card reads as one recessed well: depth, no
    /// motion, set off from the lighter unread wash. `[theme.display.highlight_steps].band`
    /// controls the fine truecolor sub-cell step, and `.indexed` controls the one
    /// xterm cell step at indexed depth. `None` under `NO_COLOR`, where the bright
    /// spine and bold weight carry the selection alone.
    pub(super) fn selection_band(&self) -> Option<Color> {
        self.selection_surface(
            -self.step(self.highlight_steps.band),
            -self.step(self.highlight_steps.indexed),
        )
    }

    /// The soft, uniform background an unread card rests on: a lighter tint of the
    /// selection blue — the `selection_bg` panel lifted in lightness with its cool
    /// hue held, so the whole card reads as the same cool-blue family as the
    /// selection band, one clear step brighter, the prominent "needs you" surface. A
    /// fine `[theme.display.highlight_steps].wash` sub-cell step at truecolor, one
    /// `.indexed` xterm cell lighter at indexed depth, and `None` under `NO_COLOR`
    /// where the unread bold weight carries the cue. One tone for every unread
    /// status — the row's meaning rides its `?`/`!`/`✓` glyph, the wash only says
    /// "unseen" — and it holds still, so motion stays reserved to the single lead
    /// row. The selected card keeps its identity through the bright `▌` spine and
    /// its recessed band, so the brighter unread fill never reads as selection; the
    /// wash is a distinct, lighter tone than the band, and the band wins when a card
    /// is both selected and unread. The caller applies it only to the unread
    /// look-worthy rows (the `Blink` card emphasis), so no status branch is needed
    /// here.
    pub(super) fn unread_wash(&self) -> Option<Color> {
        self.selection_surface(
            self.step(self.highlight_steps.wash),
            self.step(self.highlight_steps.indexed),
        )
    }

    /// Flat health-tone accessors for fixed chrome: `good` for the positive tier
    /// (diff additions, trunk markers), `warn` for the attention/gate-notice
    /// floor, and `alarm` for the negative tier (diff removals, a spent budget's
    /// red track, a critical gate). The continuous green→red sweep lives in
    /// [`heat_tone`](Self::heat_tone) / [`warm_heat_tone`](Self::warm_heat_tone),
    /// so `caution` is reached only through the ramp and needs no flat accessor —
    /// as `accent`/`cool`/`meta` always name a [`Component`] rather than a tier.
    pub(crate) fn good(&self, modifier: Modifier) -> Style {
        self.style(tone_color(self.palette.good), modifier)
    }

    pub(crate) fn warn(&self, modifier: Modifier) -> Style {
        self.style(tone_color(self.palette.warn), modifier)
    }

    pub(crate) fn alarm(&self, modifier: Modifier) -> Style {
        self.style(tone_color(self.palette.alarm), modifier)
    }

    /// An external-identity tone (brand clay, dollar green) at the active depth.
    pub(crate) fn identity(&self, id: Identity) -> Color {
        tone_color(self.palette.identity(id))
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

    /// The cool tail for `amount` ∈ `[0, 1]`: resting soft at `0.0` through
    /// to full `good` green at `1.0`, the under-pace mirror of
    /// [`warm_heat_tone`](Self::warm_heat_tone).
    pub(super) fn calm_tone(&self, amount: f32) -> Color {
        rgb_color(ramp_tone(&self.palette.calm_ramp, amount), self.depth)
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

    /// A brand tone from its 256-color index plus optional truecolor RGB,
    /// resolved at the active depth.
    pub(crate) fn brand_rgb_tone(&self, color: u8, color_rgb: Option<(u8, u8, u8)>) -> Color {
        match (self.depth, color_rgb) {
            (ColorDepth::Truecolor, Some((red, green, blue))) => Color::Rgb(red, green, blue),
            _ => Color::Indexed(color),
        }
    }

    pub(crate) fn brand_tone(&self, panel: &crate::SidebarProviderPanel) -> Color {
        if let Some(role) = panel.color_role {
            return tone_color(self.palette.role_tone(role));
        }
        self.brand_rgb_tone(panel.color, panel.color_rgb)
    }

    /// Resolve provider brand independently of dashboard panel visibility.
    pub(crate) fn provider_brand_tone(&self, kind: &str) -> Color {
        tone_color(resolve_provider_brand(kind, &self.provider_styles).tone(&self.palette))
    }
}

pub(super) fn tone_color(tone: Tone) -> Color {
    match tone {
        Tone::Indexed(index) => Color::Indexed(index),
        Tone::Rgb(red, green, blue) => Color::Rgb(red, green, blue),
    }
}

fn rgb_color(rgb: (u8, u8, u8), depth: ColorDepth) -> Color {
    tone_color(Tone::from_rgb(rgb, depth))
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

#[cfg(test)]
mod tests;
