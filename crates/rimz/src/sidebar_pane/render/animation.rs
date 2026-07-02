use crate::agents::{ATTENTION_AGE_CEILING_SECS, AgentStatus};
use crate::config::{
    AnimationColor, AnimationEffect as ConfigEffect, AnimationSpec, AnimationSpeed as ConfigSpeed,
    GlyphRole, ThemeAnimationsConfig, UnreadEffect as ConfigUnreadEffect,
};
use ratatui::style::{Color, Modifier, Style};

use super::compose::lead_unread;
use super::theme::{GlyphSet, Palette, Theme};
use crate::SidebarSnapshot;

const THINKING_FRAMES: &[&str] = &[
    "⠁", "⠂", "⠄", "⡀", "⡈", "⡐", "⡠", "⣀", "⣁", "⣂", "⣄", "⣌", "⣔", "⣤", "⣥", "⣦", "⣮", "⣶", "⣷",
    "⣿", "⡿", "⠿", "⢟", "⠟", "⡛", "⠛", "⠫", "⢋", "⠋", "⠍", "⡉", "⠉", "⠑", "⠡", "⢁",
];
pub(crate) const DEFAULT_BREATH_PERIOD: f32 = 24.0;
const FRESH_ATTENTION_PERIOD: f32 = 26.0;
const HOT_ATTENTION_PERIOD: f32 = 12.0;
const BREATH_MIDPOINT: f32 = 0.35;

#[cfg(test)]
pub(crate) const BREATH_SHALLOW_AMPLITUDE: f32 = 0.08;
/// The unread/attention blink depth: the full upward swing of the 2-pole square
/// wave — the element sits at its resting tone on the off-pole and snaps to a
/// bright crest on the on-pole, never dimming below rest.
pub(crate) const BREATH_DEEP_AMPLITUDE: f32 = 0.42;
const BREATH_CONFIG_AMPLITUDE: f32 = 0.12;
/// The unread blink's peak OKLab-L lift at the on-pole. Held small so a bright
/// resting tone brightens toward its crest without clipping to white; the
/// gamut-safe `lift_lightness` then keeps the hue while saturation eases. The
/// blink's punch also rides held bold weight and the animated head, so the lift
/// itself can stay gentle and keep the color true through the swing.
const BLINK_PEAK_LIFT: f32 = 0.08;

/// The fastest animation class currently visible in the snapshot. Fast motion
/// changes every frame (working/thinking spinners, resolver work, active
/// process rows). Breath motion is the attention/result blink and the calm
/// resting breathe, sampled near the base grid without paying the full spinner
/// cadence for calm rooms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimationCadence {
    None,
    Breath,
    Fast,
}

/// Whether any visible row is in an animated state — a running agent (working
/// or pre-edit thinking), a resolver mid-flight, an active process spinning on
/// real work (a build, a test, a `sudo` install), or the single lead unread
/// `?`/`!` row whose configured effect flows. The serve loop uses this as the
/// broad "does anything move?" gate; [`animation_cadence`] decides whether the
/// movement needs the fast frame grid or the breath grid. A fully settled
/// sidebar — quiet read idle/done rows, and every unread row past the lead
/// resting at its static crest — keeps idling on the slow data tick. A stalled
/// agent is projected to `failed` upstream, so it reads as a pulsing `!` here.
/// The cockpit's headline-spend count-up rides a separate gate (`UiState::tally`),
/// so a finished-turn climb keeps the tick alive even when every row is
/// otherwise static.
pub fn has_live_animation(snapshot: &SidebarSnapshot) -> bool {
    animation_cadence(snapshot) != AnimationCadence::None
}

// Deliberately unfiltered by the make-up filter: the cockpit's attention
// buckets still animate (and the counts still tick) for rows a filter hides,
// so the gate must track the whole room, not the narrowed body.
pub fn animation_cadence(snapshot: &SidebarSnapshot) -> AnimationCadence {
    let mut breath = false;
    for row in snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
    {
        if row.is_agent() {
            if row.resolver().is_some() || row.status() == Some(AgentStatus::Running) {
                return AnimationCadence::Fast;
            }
            // A read `?`/`!` row honours its configured effect. Unread motion is
            // reserved to the single lead row (checked once below); every other
            // unread row settles to the static `bright` crest and asks nothing
            // of the grid.
            if !row.unread
                && let Some(status) = row.status()
                && status.is_actionable()
            {
                breath |= status_needs_motion(&snapshot.theme.animations, status);
            }
        } else if row.process_is_busy() {
            return AnimationCadence::Fast;
        }
    }
    // The lead unread row wears the continuous unread effect, so it keeps the
    // breath grid warm — but only when that effect actually moves frame to
    // frame, not when it rests at the static `bright` crest or its role is
    // quieted to `static`. The cockpit lead bucket pulses with it, so this one
    // condition covers both the row and its bucket.
    breath |= lead_unread_needs_motion(snapshot);
    if breath || snapshot.theme.animations.has_resting_motion() {
        AnimationCadence::Breath
    } else {
        AnimationCadence::None
    }
}

/// Whether the single lead unread row carries per-frame motion the breath grid
/// must serve. The lead is the oldest actionable unread ask ([`lead_unread`]);
/// it animates when the configured unread effect flows (shimmer or blink, not
/// the held `bright` crest) and the lead's role has not been quieted to
/// `static`.
fn lead_unread_needs_motion(snapshot: &SidebarSnapshot) -> bool {
    let Some((_, status)) = lead_unread(&snapshot.worktree_groups) else {
        return false;
    };
    unread_effect_animates(snapshot.theme.animations.unread)
        && status_needs_motion(&snapshot.theme.animations, status)
}

/// Whether the configured unread effect flows on the phase grid. `shimmer` and
/// `blink` move; the held `bright` crest is static, so a lead row wearing it
/// asks nothing of the breath grid.
fn unread_effect_animates(effect: Option<ConfigUnreadEffect>) -> bool {
    !matches!(effect, Some(ConfigUnreadEffect::Bright))
}

fn status_needs_motion(animations: &ThemeAnimationsConfig, status: AgentStatus) -> bool {
    let spec = match status {
        AgentStatus::Waiting => animations.waiting.as_ref(),
        AgentStatus::Failed => animations.failed.as_ref(),
        _ => None,
    };
    spec_needs_motion(spec)
}

pub(super) fn spec_needs_motion(spec: Option<&AnimationSpec>) -> bool {
    match spec {
        Some(spec) if spec.disables_effect_motion() => spec.has_frame_motion(),
        _ => true,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AnimationRole {
    Thinking,
    Working,
    Compacting,
    Delegating,
    Resolving,
    Idle,
    Success,
    Paused,
    Waiting,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    Static,
    Breathe,
}

impl From<ConfigEffect> for Effect {
    fn from(value: ConfigEffect) -> Self {
        match value {
            ConfigEffect::Static => Self::Static,
            ConfigEffect::Breathe => Self::Breathe,
        }
    }
}

/// How an unread attention row reads — the resolved twin of the config
/// [`ConfigUnreadEffect`], shared by the lead glyph, the card name, the
/// description, and the make-up buckets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum UnreadEffect {
    #[default]
    Shimmer,
    Bright,
    Blink,
}

impl From<ConfigUnreadEffect> for UnreadEffect {
    fn from(value: ConfigUnreadEffect) -> Self {
        match value {
            ConfigUnreadEffect::Shimmer => Self::Shimmer,
            ConfigUnreadEffect::Bright => Self::Bright,
            ConfigUnreadEffect::Blink => Self::Blink,
        }
    }
}

/// The resolved unread treatment for one row, built once and shared by every
/// grouped element so they animate from the same clock. `Blink` carries the
/// 2-pole sample, `Bright` is the held crest, and `Shimmer` carries the flowing
/// beam's phase and age.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum UnreadAnim {
    Blink(BreathSample),
    Bright,
    Shimmer(ShimmerWave),
}

/// The flowing shimmer beam for one element: a speed-scaled `phase` and the
/// row's `age_secs`, from which [`shimmer_lift`] derives a per-cell lift. The
/// beam runs over each element's own length, so the glyph, name, and description
/// each sweep independently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ShimmerWave {
    phase: u64,
    age_secs: i64,
}

/// OKLab-L crest of the shimmer beam — the lift its center cell reaches. It
/// rides well above the blink crest ([`BLINK_PEAK_LIFT`]) on purpose: the blink
/// lifts every cell at once, while the beam lights only the cells around its
/// center with a soft falloff, so a matching ceiling would read far fainter. A
/// brighter crest makes the moving highlight read as light flowing across the
/// run.
const SHIMMER_PEAK_LIFT: f32 = 0.26;

/// Beam half-width as a fraction of the run's length: the lit band scales with
/// the element, so a long description carries the same proportional glint as a
/// short name instead of a fixed dot lost on a long line.
const SHIMMER_BEAM_FRACTION: f32 = 0.13;
/// Floor on the beam half-width, in cells, so the glyph and short names still
/// get a real beam rather than a single lit cell.
const SHIMMER_BEAM_HALF_MIN: f32 = 2.0;
/// Ceiling on the beam half-width, in cells, so a very long line never glows
/// end to end — the beam stays a travelling highlight, not a wash.
const SHIMMER_BEAM_HALF_MAX: f32 = 7.0;

/// Fraction of the full sweep the beam advances per render frame for a fresh
/// ask. Measured in proportion, so a longer run sweeps faster in cells/frame to
/// keep pace — up to [`SHIMMER_MAX_VELOCITY`], past which it would step at the
/// refresh rate instead of flowing.
const SHIMMER_FRESH_SWEEP: f32 = 0.03;
/// Sweep fraction per frame at the age ceiling — a longer-ignored ask flows
/// faster, the same "quickens with age" pacing the blink rides.
const SHIMMER_HOT_SWEEP: f32 = 0.06;
/// Speed ceiling, in cells per render frame. Left uncapped the proportional
/// sweep pushes a long description to several cells per frame, which strobes at
/// the ~10 Hz refresh; the cap holds it to a steady glide. At this value a
/// full-width (~50-cell) description crosses in about four seconds while shorter
/// runs still sweep in proportion.
const SHIMMER_MAX_VELOCITY: f32 = 1.17;

/// The beam half-width for a run of `len` cells: proportional to the length,
/// clamped to a floor and ceiling.
fn shimmer_half(len: usize) -> f32 {
    (len as f32 * SHIMMER_BEAM_FRACTION).clamp(SHIMMER_BEAM_HALF_MIN, SHIMMER_BEAM_HALF_MAX)
}

/// Sweep fraction per frame for the given age, easing from fresh to hot.
fn shimmer_sweep(age_secs: i64) -> f32 {
    let heat = (age_secs.max(0) as f32 / ATTENTION_AGE_CEILING_SECS as f32).clamp(0.0, 1.0);
    SHIMMER_FRESH_SWEEP + (SHIMMER_HOT_SWEEP - SHIMMER_FRESH_SWEEP) * heat
}

/// The beam's speed for a run of `len` cells, in cells per frame: the
/// proportional sweep over the run's span, capped at [`SHIMMER_MAX_VELOCITY`] so
/// a long line stays smooth at the refresh rate.
fn shimmer_velocity(len: usize, age_secs: i64) -> f32 {
    let span = len as f32 + 2.0 * shimmer_half(len);
    (span * shimmer_sweep(age_secs)).min(SHIMMER_MAX_VELOCITY)
}

/// The OKLab-L lift for one cell under the shimmer beam. The beam center cycles
/// around a ring at a length-scaled, capped speed ([`shimmer_velocity`]); a cell's
/// lift eases from the crest ([`SHIMMER_PEAK_LIFT`]) at the beam center to zero at
/// its edge on a raised-cosine curve — a soft bell that reads like cast light
/// rather than a hard wedge — measured by circular distance so the beam bridges
/// the seam. A run long enough to hold the beam rides a ring of its own length, so
/// leaving the last cell re-enters the first with no gap and the light loops
/// seamlessly, start to end. A short element (the glyph, a make-up bucket) rides a
/// ring just wider than the beam, so it pulses bright then rests rather than
/// holding a constant glow.
pub(crate) fn shimmer_lift(wave: ShimmerWave, index: usize, len: usize) -> f32 {
    let half = shimmer_half(len);
    let ring = (len as f32).max(2.0 * half + 1.0);
    let center = (wave.phase as f32 * shimmer_velocity(len, wave.age_secs)) % ring;
    let offset = (index as f32 - center).abs();
    let distance = offset.min(ring - offset);
    if distance >= half {
        return 0.0;
    }
    let falloff = 0.5 * (1.0 + (std::f32::consts::PI * distance / half).cos());
    SHIMMER_PEAK_LIFT * falloff
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Speed {
    Slow,
    Normal,
    Fast,
}

impl Speed {
    fn divisor(self) -> u64 {
        match self {
            Self::Slow => 3,
            Self::Normal => 2,
            Self::Fast => 1,
        }
    }

    fn effect_phase(self, phase: u64) -> u64 {
        match self {
            Self::Slow => phase / 2,
            Self::Normal => phase,
            Self::Fast => phase.wrapping_mul(2),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BreathSample {
    level: f32,
    amplitude: f32,
}

impl BreathSample {
    pub(crate) fn new(phase: u64, period: f32, amplitude: f32) -> Self {
        Self {
            level: breath_unit(breath_theta(phase, period)),
            amplitude,
        }
    }

    pub(crate) fn blink_for_age(phase: u64, age_secs: i64, amplitude: f32) -> Self {
        Self {
            level: blink_level(phase, breath_tempo(age_secs)),
            amplitude,
        }
    }

    /// A sample pinned to the on-pole: a constant crest, no swing. Drives the
    /// `bright` unread effect — through [`Theme::pulse`](super::theme::Theme::pulse)
    /// it reads as a steady lift plus bold, the blink's bright pole held still.
    pub(crate) fn steady_peak(amplitude: f32) -> Self {
        Self {
            level: 1.0,
            amplitude,
        }
    }

    #[cfg(test)]
    pub(crate) fn level(self) -> f32 {
        self.level
    }

    pub(crate) fn lightness_delta(self) -> f32 {
        (self.level - BREATH_MIDPOINT) * self.amplitude
    }

    pub(crate) fn modifier(self) -> Modifier {
        if self.amplitude == 0.0 {
            return Modifier::empty();
        }
        let depth = (self.amplitude / BREATH_DEEP_AMPLITUDE).clamp(0.0, 1.0);
        // Widen both poles with depth so a deep breathe spends real time at each
        // end — a hard bright↔dim swing rather than a momentary peak.
        let dim_cutoff = 0.30 * depth;
        let bold_floor = 1.0 - 0.38 * depth;
        match self.level {
            level if level <= dim_cutoff => Modifier::DIM,
            level if self.amplitude >= BREATH_DEEP_AMPLITUDE * 0.75 && level >= bold_floor => {
                Modifier::BOLD
            }
            _ => Modifier::empty(),
        }
    }

    /// The lightness lift for the unread blink: zero on the off-pole (the element
    /// rests at its normal tone) and a fixed gentle crest ([`BLINK_PEAK_LIFT`]) on
    /// the on-pole, with nothing in between and never negative — a hard 2-pole
    /// square wave between the resting tone and a brighter, same-hue crest.
    pub(crate) fn grow_delta(self) -> f32 {
        self.level * BLINK_PEAK_LIFT
    }

    /// The weight half of the unread blink for the colorless fallback: bold on
    /// the on-pole, plain on the off-pole, so a `NO_COLOR` or colorless element
    /// still blinks on weight alone. The colored path holds bold at both poles.
    pub(crate) fn grow_modifier(self) -> Modifier {
        if self.amplitude == 0.0 {
            return Modifier::empty();
        }
        if self.level >= 0.5 {
            Modifier::BOLD
        } else {
            Modifier::empty()
        }
    }
}

pub(crate) fn breath_tempo(age_secs: i64) -> f32 {
    let heat = (age_secs.max(0) as f32 / ATTENTION_AGE_CEILING_SECS as f32).clamp(0.0, 1.0);
    FRESH_ATTENTION_PERIOD - ((FRESH_ATTENTION_PERIOD - HOT_ATTENTION_PERIOD) * heat)
}

pub(crate) fn breath_theta(phase: u64, period: f32) -> f32 {
    let period = period.max(1.0);
    std::f32::consts::TAU * ((phase as f32 / period) % 1.0) - std::f32::consts::FRAC_PI_2
}

pub(crate) fn breath_unit(theta: f32) -> f32 {
    let floor = (-1.0_f32).exp();
    ((theta.sin().exp() - floor) / (std::f32::consts::E - floor)).clamp(0.0, 1.0)
}

/// The 2-pole blink level for the unread attention signal: full on the first
/// half of each cycle, off the second — a hard square swing between the resting
/// tone and the bright crest, no easing between them. `period` is the age tempo
/// the calm breath also rides, so an older ask blinks faster.
pub(crate) fn blink_level(phase: u64, period: f32) -> f32 {
    let frac = (phase as f32 / period.max(1.0)) % 1.0;
    if frac < 0.5 { 1.0 } else { 0.0 }
}

impl From<ConfigSpeed> for Speed {
    fn from(value: ConfigSpeed) -> Self {
        match value {
            ConfigSpeed::Slow => Self::Slow,
            ConfigSpeed::Normal => Self::Normal,
            ConfigSpeed::Fast => Self::Fast,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Animation {
    frames: Vec<String>,
    color: Color,
    effect: Effect,
    speed: Speed,
    effect_overridden: bool,
    color_overridden: bool,
}

impl Animation {
    #[cfg(test)]
    pub(crate) fn frames(&self) -> &[String] {
        &self.frames
    }

    pub(crate) fn color(&self) -> Color {
        self.color
    }

    pub(crate) fn color_overridden(&self) -> bool {
        self.color_overridden
    }

    pub(crate) fn attention_breath_phase(&self, phase: u64) -> Option<u64> {
        match (self.effect, self.effect_overridden) {
            (Effect::Static, true) => None,
            _ => Some(self.speed.effect_phase(phase)),
        }
    }

    /// The unread treatment for this role at `age_secs` under the chosen
    /// [`UnreadEffect`], or `None` when a configured `effect = "static"` quiets
    /// it (the caller then falls back to a constant bold tone). One source every
    /// grouped element shares — the lead glyph, the agent name, the description,
    /// and the cockpit make-up bucket — so they animate from the same clock.
    pub(crate) fn unread_anim(
        &self,
        unread: UnreadEffect,
        phase: u64,
        age_secs: i64,
    ) -> Option<UnreadAnim> {
        let phase = self.attention_breath_phase(phase)?;
        Some(match unread {
            UnreadEffect::Blink => UnreadAnim::Blink(BreathSample::blink_for_age(
                phase,
                age_secs,
                BREATH_DEEP_AMPLITUDE,
            )),
            UnreadEffect::Bright => UnreadAnim::Bright,
            UnreadEffect::Shimmer => UnreadAnim::Shimmer(ShimmerWave { phase, age_secs }),
        })
    }

    #[cfg(test)]
    fn has_motion(&self) -> bool {
        self.frames.len() > 1 || self.effect != Effect::Static
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedAnimations {
    unread: UnreadEffect,
    thinking: Animation,
    working: Animation,
    compacting: Animation,
    delegating: Animation,
    resolving: Animation,
    idle: Animation,
    success: Animation,
    paused: Animation,
    waiting: Animation,
    failed: Animation,
}

impl Default for ResolvedAnimations {
    fn default() -> Self {
        let palette = Palette::resolve(
            &crate::config::ThemeConfig::default(),
            crate::config::ColorDepth::Indexed,
        );
        Self::resolve(
            &ThemeAnimationsConfig::default(),
            &GlyphSet::default(),
            &palette,
        )
    }
}

impl ResolvedAnimations {
    pub(crate) fn resolve(
        config: &ThemeAnimationsConfig,
        glyphs: &GlyphSet,
        palette: &Palette,
    ) -> Self {
        Self {
            unread: config.unread.map(UnreadEffect::from).unwrap_or_default(),
            thinking: resolve_role(
                AnimationRole::Thinking,
                config.thinking.as_ref(),
                glyphs,
                palette,
            ),
            working: resolve_role(
                AnimationRole::Working,
                config.working.as_ref(),
                glyphs,
                palette,
            ),
            compacting: resolve_role(
                AnimationRole::Compacting,
                config.compacting.as_ref(),
                glyphs,
                palette,
            ),
            delegating: resolve_role(
                AnimationRole::Delegating,
                config.delegating.as_ref(),
                glyphs,
                palette,
            ),
            resolving: resolve_role(
                AnimationRole::Resolving,
                config.resolving.as_ref(),
                glyphs,
                palette,
            ),
            idle: resolve_role(AnimationRole::Idle, config.idle.as_ref(), glyphs, palette),
            success: resolve_role(
                AnimationRole::Success,
                config.success.as_ref(),
                glyphs,
                palette,
            ),
            paused: resolve_role(
                AnimationRole::Paused,
                config.paused.as_ref(),
                glyphs,
                palette,
            ),
            waiting: resolve_role(
                AnimationRole::Waiting,
                config.waiting.as_ref(),
                glyphs,
                palette,
            ),
            failed: resolve_role(
                AnimationRole::Failed,
                config.failed.as_ref(),
                glyphs,
                palette,
            ),
        }
    }

    pub(crate) fn role(&self, role: AnimationRole) -> &Animation {
        match role {
            AnimationRole::Thinking => &self.thinking,
            AnimationRole::Working => &self.working,
            AnimationRole::Compacting => &self.compacting,
            AnimationRole::Delegating => &self.delegating,
            AnimationRole::Resolving => &self.resolving,
            AnimationRole::Idle => &self.idle,
            AnimationRole::Success => &self.success,
            AnimationRole::Paused => &self.paused,
            AnimationRole::Waiting => &self.waiting,
            AnimationRole::Failed => &self.failed,
        }
    }

    pub(crate) fn status_role(status: AgentStatus) -> AnimationRole {
        match status {
            AgentStatus::Waiting => AnimationRole::Waiting,
            AgentStatus::Failed => AnimationRole::Failed,
            AgentStatus::Running => AnimationRole::Working,
            AgentStatus::Idle => AnimationRole::Idle,
            AgentStatus::Success => AnimationRole::Success,
            AgentStatus::Paused => AnimationRole::Paused,
        }
    }

    pub(crate) fn status(&self, status: AgentStatus) -> &Animation {
        self.role(Self::status_role(status))
    }

    /// The configured unread attention effect (default [`UnreadEffect::Shimmer`]).
    pub(crate) fn unread_effect(&self) -> UnreadEffect {
        self.unread
    }

    #[cfg(test)]
    pub(crate) fn has_resting_motion(&self) -> bool {
        [
            AnimationRole::Paused,
            AnimationRole::Idle,
            AnimationRole::Success,
        ]
        .into_iter()
        .any(|role| self.role(role).has_motion())
    }
}

pub(crate) fn frame_at(animation: &Animation, phase: u64) -> String {
    let index = ((phase / animation.speed.divisor()) as usize) % animation.frames.len();
    animation.frames[index].clone()
}

pub(crate) fn effect_style(theme: &Theme, animation: &Animation, phase: u64) -> Style {
    let phase = animation.speed.effect_phase(phase);
    match animation.effect {
        Effect::Static => theme.style(animation.color, Modifier::empty()),
        Effect::Breathe => theme.breathe(
            animation.color,
            BreathSample::new(phase, DEFAULT_BREATH_PERIOD, BREATH_CONFIG_AMPLITUDE),
        ),
    }
}

pub(crate) fn effect_weight(animation: &Animation, phase: u64) -> Modifier {
    let phase = animation.speed.effect_phase(phase);
    match animation.effect {
        Effect::Static => Modifier::empty(),
        Effect::Breathe => {
            BreathSample::new(phase, DEFAULT_BREATH_PERIOD, BREATH_CONFIG_AMPLITUDE).modifier()
        }
    }
}

fn resolve_role(
    role: AnimationRole,
    spec: Option<&AnimationSpec>,
    glyphs: &GlyphSet,
    palette: &Palette,
) -> Animation {
    let mut animation = builtin(role, glyphs, palette);
    if let Some(spec) = spec {
        if let Some(frames) = spec.frames.as_ref() {
            animation.frames = frames.as_slice().to_vec();
        }
        if let Some(color) = spec.color {
            animation.color = palette.animation_color(color);
            animation.color_overridden = true;
        }
        if let Some(effect) = spec.effect {
            animation.effect = effect.into();
            animation.effect_overridden = true;
        }
        if let Some(speed) = spec.speed {
            animation.speed = speed.into();
        }
    }
    animation
}

/// The built-in animation for a role, before any `[theme.animations.<role>]`
/// override. The animated spinners (thinking/working/delegating/resolving) and
/// the compacting wave keep their Unicode braille/block frame sequences here in
/// every preset; the single-frame status heads (idle/success/paused/waiting/
/// failed) draw their one frame from the glyph set's `status` group, so
/// `[theme.glyphs.<set>.status]` is the one place the head shapes are configured
/// while this function keeps their colour, effect, and speed.
fn builtin(role: AnimationRole, glyphs: &GlyphSet, palette: &Palette) -> Animation {
    let head = |role| vec![glyphs.glyph(role).to_owned()];
    let seq = |frames: &[&str]| {
        frames
            .iter()
            .map(|frame| frame.to_string())
            .collect::<Vec<_>>()
    };
    let (frames, color, effect, speed) = match role {
        AnimationRole::Thinking => (
            seq(THINKING_FRAMES),
            palette.animation_color(AnimationColor::Clay),
            Effect::Static,
            Speed::Fast,
        ),
        AnimationRole::Working => (
            seq(&["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"]),
            palette.animation_color(AnimationColor::Clay),
            Effect::Static,
            Speed::Fast,
        ),
        AnimationRole::Compacting => (
            seq(&["▁", "▃", "▄", "▅", "▆", "▇", "▆", "▅", "▄", "▃"]),
            palette.animation_color(AnimationColor::Meta),
            Effect::Static,
            Speed::Fast,
        ),
        AnimationRole::Delegating => (
            seq(&["⢄", "⢂", "⢁", "⡁", "⡈", "⡐", "⡠"]),
            palette.animation_color(AnimationColor::Clay),
            Effect::Static,
            Speed::Fast,
        ),
        AnimationRole::Resolving => (
            seq(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
            palette.animation_color(AnimationColor::Meta),
            Effect::Static,
            Speed::Fast,
        ),
        AnimationRole::Idle => (
            head(GlyphRole::StatusIdle),
            palette.animation_color(AnimationColor::Good),
            Effect::Static,
            Speed::Normal,
        ),
        AnimationRole::Success => (
            head(GlyphRole::StatusDone),
            palette.animation_color(AnimationColor::Good),
            Effect::Static,
            Speed::Normal,
        ),
        AnimationRole::Paused => (
            head(GlyphRole::StatusPaused),
            palette.animation_color(AnimationColor::Cool),
            Effect::Static,
            Speed::Normal,
        ),
        AnimationRole::Waiting => (
            head(GlyphRole::StatusWaiting),
            palette.animation_color(AnimationColor::Warn),
            Effect::Static,
            Speed::Normal,
        ),
        AnimationRole::Failed => (
            head(GlyphRole::StatusAttention),
            palette.animation_color(AnimationColor::Alarm),
            Effect::Static,
            Speed::Normal,
        ),
    };
    Animation {
        frames,
        color,
        effect,
        speed,
        effect_overridden: false,
        color_overridden: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::validate_single_cell;

    fn test_palette() -> Palette {
        Palette::resolve(
            &crate::config::ThemeConfig::default(),
            crate::config::ColorDepth::Indexed,
        )
    }

    fn resolve_for_test(config: &ThemeAnimationsConfig, palette: &Palette) -> ResolvedAnimations {
        ResolvedAnimations::resolve(config, &GlyphSet::default(), palette)
    }

    fn nerd_glyph_set() -> GlyphSet {
        GlyphSet::resolve(&crate::config::ThemeGlyphsConfig {
            set: Some("nerd_font".to_owned()),
            ..Default::default()
        })
    }

    #[test]
    fn builtin_spinner_frames_are_single_cell() {
        for frame in THINKING_FRAMES {
            validate_single_cell(frame)
                .unwrap_or_else(|err| panic!("thinking frame {frame:?}: {err}"));
        }
    }

    #[test]
    fn unset_config_resolves_to_builtins() {
        let animations = ResolvedAnimations::default();
        assert_eq!(
            animations.role(AnimationRole::Thinking).frames(),
            THINKING_FRAMES
        );
        assert_eq!(
            animations.role(AnimationRole::Working).color(),
            Color::Indexed(173)
        );
        assert_eq!(
            animations.role(AnimationRole::Failed).color(),
            test_palette().animation_color(AnimationColor::Alarm),
            "the static failed marker remains alarm-red"
        );
    }

    #[test]
    fn nerd_font_swaps_static_heads_but_keeps_unicode_spinners() {
        let palette = test_palette();
        let unicode = resolve_for_test(&ThemeAnimationsConfig::default(), &palette);
        let nerd = ResolvedAnimations::resolve(
            &ThemeAnimationsConfig::default(),
            &nerd_glyph_set(),
            &palette,
        );
        // The agent's working/thinking motion keeps its Unicode spinner in every
        // preset — the Nerd Font set does not theme the animated frames.
        for role in [
            AnimationRole::Thinking,
            AnimationRole::Working,
            AnimationRole::Delegating,
            AnimationRole::Resolving,
            AnimationRole::Compacting,
        ] {
            assert_eq!(
                nerd.role(role).frames(),
                unicode.role(role).frames(),
                "{role:?} keeps its Unicode frames"
            );
        }
        // The single-frame status heads take the curated Nerd Font icons.
        assert_eq!(nerd.role(AnimationRole::Idle).frames(), ["\u{f2dd}"]);
        assert_eq!(nerd.role(AnimationRole::Success).frames(), ["\u{f00c}"]);
        assert_eq!(nerd.role(AnimationRole::Failed).frames(), ["\u{f12a}"]);
    }

    #[test]
    fn partial_override_changes_only_the_named_field() {
        let config: ThemeAnimationsConfig =
            toml::from_str("[thinking]\nframes = \"ab\"\n").expect("config");
        let palette = test_palette();
        let animations = resolve_for_test(&config, &palette);
        let thinking = animations.role(AnimationRole::Thinking);
        assert_eq!(thinking.frames(), ["a", "b"]);
        assert_eq!(thinking.color(), Color::Indexed(173));
        assert_eq!(frame_at(thinking, 0), "a");
        assert_eq!(frame_at(thinking, 1), "b", "fast advances every tick");
        assert_eq!(frame_at(thinking, 2), "a");
    }

    #[test]
    fn clay_and_semantic_colors_resolve_to_palette_tones() {
        let config: ThemeAnimationsConfig = toml::from_str(
            "[working]\ncolor = \"clay\"\n\n[idle]\ncolor = \"good\"\n\n[success]\ncolor = 34\n",
        )
        .expect("config");
        let palette = Palette::resolve(
            &crate::config::ThemeConfig {
                good: Some(crate::config::ThemeColor::Indexed(34)),
                ..crate::config::ThemeConfig::default()
            },
            crate::config::ColorDepth::Indexed,
        );
        let animations = resolve_for_test(&config, &palette);
        assert_eq!(
            animations.role(AnimationRole::Working).color(),
            Color::Indexed(173)
        );
        assert_eq!(
            animations.role(AnimationRole::Idle).color(),
            Color::Indexed(34),
            "named slots retune through [theme]"
        );
        assert_eq!(
            animations.role(AnimationRole::Success).color(),
            Color::Indexed(34)
        );
        assert!(
            animations.role(AnimationRole::Idle).color_overridden(),
            "the render layer keeps the override bit for attention floors"
        );
    }

    #[test]
    fn default_clay_animations_follow_truecolor_depth() {
        let palette = Palette::resolve(
            &crate::config::ThemeConfig::default(),
            crate::config::ColorDepth::Truecolor,
        );
        let animations = resolve_for_test(&ThemeAnimationsConfig::default(), &palette);
        let clay = palette.animation_color(AnimationColor::Clay);
        assert_eq!(animations.role(AnimationRole::Thinking).color(), clay);
        assert_eq!(animations.role(AnimationRole::Working).color(), clay);
        assert_eq!(animations.role(AnimationRole::Delegating).color(), clay);
    }

    #[test]
    fn effects_and_resting_motion_are_resolved() {
        let config: ThemeAnimationsConfig =
            toml::from_str("[idle]\neffect = \"breathe\"\n").expect("config");
        let palette = test_palette();
        let animations = resolve_for_test(&config, &palette);
        assert!(animations.has_resting_motion());
        assert_eq!(
            effect_weight(animations.role(AnimationRole::Idle), 0),
            Modifier::DIM
        );
    }

    #[test]
    fn speed_modulates_effect_cadence() {
        let config: ThemeAnimationsConfig = toml::from_str(
            "[idle]\neffect = \"breathe\"\nspeed = \"slow\"\n\n[success]\neffect = \"breathe\"\nspeed = \"fast\"\n",
        )
        .expect("config");
        let palette = test_palette();
        let animations = resolve_for_test(&config, &palette);
        assert_ne!(
            effect_weight(animations.role(AnimationRole::Idle), 5),
            effect_weight(animations.role(AnimationRole::Success), 5),
            "slow and fast breathe effects must diverge on the same render phase"
        );
    }

    #[test]
    fn attention_and_paused_roles_accept_effect_and_speed() {
        let config: ThemeAnimationsConfig = toml::from_str(
            "[waiting]\neffect = \"breathe\"\nspeed = \"fast\"\n\n[paused]\neffect = \"breathe\"\nspeed = \"fast\"\n",
        )
        .expect("config");
        let palette = test_palette();
        let animations = resolve_for_test(&config, &palette);
        assert_eq!(
            animations
                .role(AnimationRole::Waiting)
                .attention_breath_phase(3),
            Some(6),
            "configured speed reaches the attention blink phase"
        );
        assert!(
            animations.has_resting_motion(),
            "a paused effect override now participates in the uniform model"
        );

        let quiet: ThemeAnimationsConfig =
            toml::from_str("[waiting]\neffect = \"static\"\n").expect("config");
        let animations = resolve_for_test(&quiet, &palette);
        assert_eq!(
            animations
                .role(AnimationRole::Waiting)
                .attention_breath_phase(3),
            None,
            "configured static quiets the default attention blink"
        );
    }

    #[test]
    fn breath_curve_is_smooth_but_attention_blink_is_two_pole() {
        assert_eq!(breath_tempo(-1), FRESH_ATTENTION_PERIOD);
        assert_eq!(
            breath_tempo(ATTENTION_AGE_CEILING_SECS),
            HOT_ATTENTION_PERIOD
        );
        assert_eq!(
            breath_tempo(2 * ATTENTION_AGE_CEILING_SECS),
            HOT_ATTENTION_PERIOD
        );
        assert!(breath_tempo(1_800) < breath_tempo(0));

        // The calm, configurable breathe effect still eases smoothly.
        let trough = BreathSample::new(0, DEFAULT_BREATH_PERIOD, BREATH_DEEP_AMPLITUDE);
        let middle = BreathSample::new(6, DEFAULT_BREATH_PERIOD, BREATH_DEEP_AMPLITUDE);
        let peak = BreathSample::new(12, DEFAULT_BREATH_PERIOD, BREATH_DEEP_AMPLITUDE);
        assert!(trough.level() < middle.level());
        assert!(middle.level() < peak.level());
        assert!(trough.lightness_delta() < 0.0);
        assert!(peak.lightness_delta() > 0.0);

        // The unread attention blink is a hard 2-pole square wave: the lightness
        // snaps between the resting tone (off-pole, delta 0) and the bright crest
        // (on-pole), with no eased value between them, and never below rest.
        let peak_delta = BLINK_PEAK_LIFT;
        let on =
            BreathSample::blink_for_age(0, 2 * ATTENTION_AGE_CEILING_SECS, BREATH_DEEP_AMPLITUDE);
        let off =
            BreathSample::blink_for_age(6, 2 * ATTENTION_AGE_CEILING_SECS, BREATH_DEEP_AMPLITUDE);
        assert_eq!(on.grow_delta(), peak_delta);
        assert_eq!(off.grow_delta(), 0.0);
        for phase in 0..12 {
            let delta = BreathSample::blink_for_age(
                phase,
                2 * ATTENTION_AGE_CEILING_SECS,
                BREATH_DEEP_AMPLITUDE,
            )
            .grow_delta();
            assert!(
                delta == 0.0 || delta == peak_delta,
                "the blink only ever sits at a pole, never an eased value between them"
            );
            assert!(delta >= 0.0, "the blink never dims below the resting tone");
        }
    }

    #[test]
    fn attention_blink_is_a_hard_two_pole_square_wave() {
        // A 50/50 square wave on the period: on for the first half, off the second.
        assert_eq!(blink_level(0, 12.0), 1.0);
        assert_eq!(blink_level(5, 12.0), 1.0);
        assert_eq!(blink_level(6, 12.0), 0.0);
        assert_eq!(blink_level(11, 12.0), 0.0);
        assert_eq!(blink_level(12, 12.0), 1.0, "the cycle wraps");

        // Older asks blink faster: a shorter period reaches the off-pole sooner.
        let first_off = |age: i64| {
            (0..64)
                .find(|&phase| {
                    BreathSample::blink_for_age(phase, age, BREATH_DEEP_AMPLITUDE).grow_delta()
                        == 0.0
                })
                .expect("the blink turns off within a cycle")
        };
        assert!(
            first_off(2 * ATTENTION_AGE_CEILING_SECS) < first_off(0),
            "a hot ask reaches its off-pole sooner — the blink keeps pacing with age"
        );
    }

    #[test]
    fn steady_peak_is_a_constant_bright_crest() {
        let bright = BreathSample::steady_peak(BREATH_DEEP_AMPLITUDE);
        assert_eq!(
            bright.grow_delta(),
            BLINK_PEAK_LIFT,
            "bright holds the blink's bright pole",
        );
        assert_eq!(bright.grow_modifier(), Modifier::BOLD);
    }

    #[test]
    fn shimmer_lift_peaks_under_the_beam_and_is_flat_beyond_it() {
        let wave = ShimmerWave {
            phase: 12,
            age_secs: 0,
        };
        let len = 12;
        let lifts: Vec<f32> = (0..len).map(|i| shimmer_lift(wave, i, len)).collect();
        // The beam is local: not every cell lights at one phase, and the lit
        // cells peak at the shimmer crest, never above it.
        assert!(
            lifts.iter().any(|&l| l > 0.0),
            "some cell is under the beam: {lifts:?}"
        );
        assert!(
            lifts.contains(&0.0),
            "the beam is narrower than the element: {lifts:?}"
        );
        for &l in &lifts {
            assert!(
                (0.0..=SHIMMER_PEAK_LIFT + 1e-6).contains(&l),
                "lift in range: {l}"
            );
        }
    }

    #[test]
    fn shimmer_beam_travels_with_phase() {
        let len = 12;
        let center = |phase| {
            (0..len)
                .map(|i| (i, shimmer_lift(ShimmerWave { phase, age_secs: 0 }, i, len)))
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| i)
                .expect("a brightest cell")
        };
        assert!(
            center(80) >= center(2),
            "the brightest cell moves to the right as the beam travels",
        );
    }

    #[test]
    fn single_cell_shimmer_pulses_over_a_cycle() {
        // The glyph and the make-up buckets are one cell: their ring is just wider
        // than the beam, so the beam passing produces a periodic lift — bright
        // then rest — rather than a flowing run or a constant glow.
        let lifts: Vec<f32> = (0..40)
            .map(|phase| shimmer_lift(ShimmerWave { phase, age_secs: 0 }, 0, 1))
            .collect();
        assert!(lifts.iter().any(|&l| l > 0.0), "the beam reaches the cell");
        assert!(lifts.contains(&0.0), "and leaves it again");
    }

    #[test]
    fn shimmer_beam_widens_with_the_run_then_clamps() {
        // The lit band tracks the run's length so a long line is not a dot,
        // floored for short runs and capped so a very long line never washes.
        assert_eq!(
            shimmer_half(4),
            SHIMMER_BEAM_HALF_MIN,
            "short runs hit the floor"
        );
        assert!(
            shimmer_half(40) > shimmer_half(12),
            "a longer run carries a wider beam"
        );
        assert_eq!(
            shimmer_half(400),
            SHIMMER_BEAM_HALF_MAX,
            "very long runs cap"
        );
    }

    #[test]
    fn shimmer_speed_scales_with_length_then_caps_for_smoothness() {
        // A longer run sweeps faster in cells/frame to keep pace, but the speed
        // is capped so a long line glides at the refresh rate instead of stepping
        // several cells per frame.
        assert!(
            shimmer_velocity(50, 0) > shimmer_velocity(10, 0),
            "a longer run flows faster to keep pace"
        );
        for len in [8_usize, 24, 50, 80, 200] {
            for age in [0_i64, ATTENTION_AGE_CEILING_SECS] {
                assert!(
                    shimmer_velocity(len, age) <= SHIMMER_MAX_VELOCITY + 1e-6,
                    "velocity stays within the smooth cap (len={len}, age={age})"
                );
            }
        }
        assert!(
            shimmer_velocity(10, 0) < SHIMMER_MAX_VELOCITY,
            "a short name sweeps proportionally, below the cap"
        );
    }

    #[test]
    fn unread_anim_picks_the_variant_and_honors_the_static_quiet() {
        let palette = test_palette();
        let animations = resolve_for_test(&ThemeAnimationsConfig::default(), &palette);
        let waiting = animations.role(AnimationRole::Waiting);
        assert!(matches!(
            waiting.unread_anim(UnreadEffect::Blink, 0, 0),
            Some(UnreadAnim::Blink(_))
        ));
        assert!(matches!(
            waiting.unread_anim(UnreadEffect::Bright, 0, 0),
            Some(UnreadAnim::Bright)
        ));
        assert!(matches!(
            waiting.unread_anim(UnreadEffect::Shimmer, 0, 0),
            Some(UnreadAnim::Shimmer(_))
        ));

        let quiet: ThemeAnimationsConfig =
            toml::from_str("[waiting]\neffect = \"static\"\n").expect("config");
        let quieted = resolve_for_test(&quiet, &palette);
        assert_eq!(
            quieted
                .role(AnimationRole::Waiting)
                .unread_anim(UnreadEffect::Shimmer, 0, 0),
            None,
            "a static-quieted role suppresses every unread effect",
        );
    }

    #[test]
    fn unread_effect_resolves_from_config_and_defaults_to_shimmer() {
        let palette = test_palette();
        assert_eq!(
            resolve_for_test(&ThemeAnimationsConfig::default(), &palette).unread_effect(),
            UnreadEffect::Shimmer,
        );
        let config: ThemeAnimationsConfig =
            toml::from_str("unread = \"bright\"\n").expect("config");
        assert_eq!(
            resolve_for_test(&config, &palette).unread_effect(),
            UnreadEffect::Bright,
        );
    }

    #[test]
    fn no_color_modifier_preserves_pulse_depth_ordering() {
        let shallow_lift = BreathSample::new(12, DEFAULT_BREATH_PERIOD, BREATH_SHALLOW_AMPLITUDE);
        let deep_lift = BreathSample::new(12, DEFAULT_BREATH_PERIOD, BREATH_DEEP_AMPLITUDE);
        assert_eq!(shallow_lift.modifier(), Modifier::empty());
        assert_eq!(deep_lift.modifier(), Modifier::BOLD);

        let shallow_fade = BreathSample::new(4, DEFAULT_BREATH_PERIOD, BREATH_SHALLOW_AMPLITUDE);
        let deep_fade = BreathSample::new(4, DEFAULT_BREATH_PERIOD, BREATH_DEEP_AMPLITUDE);
        assert_eq!(shallow_fade.modifier(), Modifier::empty());
        assert_eq!(deep_fade.modifier(), Modifier::DIM);
    }
}
