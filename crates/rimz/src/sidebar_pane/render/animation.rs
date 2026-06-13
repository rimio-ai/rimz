use crate::config::{
    AnimationColor, AnimationEffect as ConfigEffect, AnimationSpec, AnimationSpeed as ConfigSpeed,
    SidebarAnimationsConfig,
};
use crate::feed::{ATTENTION_AGE_CEILING_SECS, AgentStatus};
use ratatui::style::{Color, Modifier, Style};

use super::theme::{Palette, Theme};

const THINKING_FRAMES: &[&str] = &[
    "⠁", "⠂", "⠄", "⡀", "⡈", "⡐", "⡠", "⣀", "⣁", "⣂", "⣄", "⣌", "⣔", "⣤", "⣥", "⣦", "⣮", "⣶", "⣷",
    "⣿", "⡿", "⠿", "⢟", "⠟", "⡛", "⠛", "⠫", "⢋", "⠋", "⠍", "⡉", "⠉", "⠑", "⠡", "⢁",
];

pub(crate) const DEFAULT_BREATH_PERIOD: f32 = 24.0;
const FRESH_ATTENTION_PERIOD: f32 = 26.0;
const HOT_ATTENTION_PERIOD: f32 = 12.0;
const BREATH_MIDPOINT: f32 = 0.35;

pub(crate) const BREATH_SHALLOW_AMPLITUDE: f32 = 0.08;
/// The unread/attention pulse depth: a hard upward swing so the blink grows from
/// the element's resting tone to a bright crest, never dimming below it.
pub(crate) const BREATH_DEEP_AMPLITUDE: f32 = 0.42;
const BREATH_CONFIG_AMPLITUDE: f32 = 0.12;

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

    pub(crate) fn for_age(phase: u64, age_secs: i64, amplitude: f32) -> Self {
        Self::new(phase, breath_tempo(age_secs), amplitude)
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
        // Widen both poles with depth so a deep pulse spends real time at each
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

    /// The grow-only lightness delta for the unread blink: zero at the trough
    /// (the element rests at its normal tone) up to the same bright crest the
    /// two-pole peak reached, never negative — the blink swells to bright and
    /// settles back without ever dimming below the resting tone.
    pub(crate) fn grow_delta(self) -> f32 {
        self.level * (1.0 - BREATH_MIDPOINT) * self.amplitude
    }

    /// The grow-only weight: bold near the crest, plain elsewhere, never DIM —
    /// the weight half of the same swell-to-bright blink.
    pub(crate) fn grow_modifier(self) -> Modifier {
        if self.amplitude == 0.0 {
            return Modifier::empty();
        }
        let depth = (self.amplitude / BREATH_DEEP_AMPLITUDE).clamp(0.0, 1.0);
        let bold_floor = 1.0 - 0.38 * depth;
        if self.amplitude >= BREATH_DEEP_AMPLITUDE * 0.75 && self.level >= bold_floor {
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

    /// The unread/attention pulse sample for this role at `age_secs` of waiting,
    /// or `None` when a configured `effect = "static"` quiets the pulse. The one
    /// source every grouped element shares — the lead glyph, the agent name, the
    /// description, and the cockpit make-up bucket — so they swing in unison.
    pub(crate) fn attention_pulse(
        &self,
        phase: u64,
        age_secs: i64,
        amplitude: f32,
    ) -> Option<BreathSample> {
        self.attention_breath_phase(phase)
            .map(|phase| BreathSample::for_age(phase, age_secs, amplitude))
    }

    #[cfg(test)]
    fn has_motion(&self) -> bool {
        self.frames.len() > 1 || self.effect != Effect::Static
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedAnimations {
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
        let palette = Palette::resolve_fixed(
            &crate::config::SidebarThemeConfig::default(),
            crate::config::ColorDepth::Indexed,
        );
        Self::resolve(&SidebarAnimationsConfig::default(), &palette)
    }
}

impl ResolvedAnimations {
    pub(crate) fn resolve(config: &SidebarAnimationsConfig, palette: &Palette) -> Self {
        Self {
            thinking: resolve_role(AnimationRole::Thinking, config.thinking.as_ref(), palette),
            working: resolve_role(AnimationRole::Working, config.working.as_ref(), palette),
            compacting: resolve_role(
                AnimationRole::Compacting,
                config.compacting.as_ref(),
                palette,
            ),
            delegating: resolve_role(
                AnimationRole::Delegating,
                config.delegating.as_ref(),
                palette,
            ),
            resolving: resolve_role(AnimationRole::Resolving, config.resolving.as_ref(), palette),
            idle: resolve_role(AnimationRole::Idle, config.idle.as_ref(), palette),
            success: resolve_role(AnimationRole::Success, config.success.as_ref(), palette),
            paused: resolve_role(AnimationRole::Paused, config.paused.as_ref(), palette),
            waiting: resolve_role(AnimationRole::Waiting, config.waiting.as_ref(), palette),
            failed: resolve_role(AnimationRole::Failed, config.failed.as_ref(), palette),
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

pub(crate) fn still_frame(animation: &Animation) -> String {
    animation.frames[0].clone()
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

fn resolve_role(role: AnimationRole, spec: Option<&AnimationSpec>, palette: &Palette) -> Animation {
    let mut animation = builtin(role, palette);
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

fn builtin(role: AnimationRole, palette: &Palette) -> Animation {
    let (frames, color, effect, speed) = match role {
        AnimationRole::Thinking => (
            THINKING_FRAMES.to_vec(),
            palette.animation_color(AnimationColor::Clay),
            Effect::Static,
            Speed::Fast,
        ),
        AnimationRole::Working => (
            vec!["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"],
            palette.animation_color(AnimationColor::Clay),
            Effect::Static,
            Speed::Fast,
        ),
        AnimationRole::Compacting => (
            vec!["▁", "▃", "▄", "▅", "▆", "▇", "▆", "▅", "▄", "▃"],
            palette.animation_color(AnimationColor::Meta),
            Effect::Static,
            Speed::Fast,
        ),
        AnimationRole::Delegating => (
            vec!["⢄", "⢂", "⢁", "⡁", "⡈", "⡐", "⡠"],
            palette.animation_color(AnimationColor::Clay),
            Effect::Static,
            Speed::Fast,
        ),
        AnimationRole::Resolving => (
            vec!["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
            palette.animation_color(AnimationColor::Meta),
            Effect::Static,
            Speed::Fast,
        ),
        AnimationRole::Idle => (
            vec!["○"],
            palette.animation_color(AnimationColor::Good),
            Effect::Static,
            Speed::Normal,
        ),
        AnimationRole::Success => (
            vec!["✓"],
            palette.animation_color(AnimationColor::Good),
            Effect::Static,
            Speed::Normal,
        ),
        AnimationRole::Paused => (
            vec!["⏸\u{FE0E}"],
            palette.animation_color(AnimationColor::Warn),
            Effect::Static,
            Speed::Normal,
        ),
        AnimationRole::Waiting => (
            vec!["?"],
            palette.animation_color(AnimationColor::Warn),
            Effect::Static,
            Speed::Normal,
        ),
        AnimationRole::Failed => (
            vec!["!"],
            palette.animation_color(AnimationColor::Alarm),
            Effect::Static,
            Speed::Normal,
        ),
    };
    Animation {
        frames: frames.into_iter().map(ToOwned::to_owned).collect(),
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

    fn test_palette() -> Palette {
        Palette::resolve_fixed(
            &crate::config::SidebarThemeConfig::default(),
            crate::config::ColorDepth::Indexed,
        )
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
    fn partial_override_changes_only_the_named_field() {
        let config: SidebarAnimationsConfig =
            toml::from_str("[thinking]\nframes = \"ab\"\n").expect("config");
        let palette = test_palette();
        let animations = ResolvedAnimations::resolve(&config, &palette);
        let thinking = animations.role(AnimationRole::Thinking);
        assert_eq!(thinking.frames(), ["a", "b"]);
        assert_eq!(thinking.color(), Color::Indexed(173));
        assert_eq!(frame_at(thinking, 0), "a");
        assert_eq!(frame_at(thinking, 1), "b", "fast advances every tick");
        assert_eq!(frame_at(thinking, 2), "a");
    }

    #[test]
    fn clay_and_semantic_colors_resolve_to_palette_tones() {
        let config: SidebarAnimationsConfig = toml::from_str(
            "[working]\ncolor = \"clay\"\n\n[idle]\ncolor = \"good\"\n\n[success]\ncolor = 34\n",
        )
        .expect("config");
        let palette = Palette::resolve_fixed(
            &crate::config::SidebarThemeConfig {
                good: Some(crate::config::ThemeColor::Indexed(34)),
                ..crate::config::SidebarThemeConfig::default()
            },
            crate::config::ColorDepth::Indexed,
        );
        let animations = ResolvedAnimations::resolve(&config, &palette);
        assert_eq!(
            animations.role(AnimationRole::Working).color(),
            Color::Indexed(173)
        );
        assert_eq!(
            animations.role(AnimationRole::Idle).color(),
            Color::Indexed(34),
            "named slots retune through [sidebar.theme]"
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
        let palette = Palette::resolve_fixed(
            &crate::config::SidebarThemeConfig::default(),
            crate::config::ColorDepth::Truecolor,
        );
        let animations = ResolvedAnimations::resolve(&SidebarAnimationsConfig::default(), &palette);
        let clay = palette.animation_color(AnimationColor::Clay);
        assert_eq!(animations.role(AnimationRole::Thinking).color(), clay);
        assert_eq!(animations.role(AnimationRole::Working).color(), clay);
        assert_eq!(animations.role(AnimationRole::Delegating).color(), clay);
    }

    #[test]
    fn effects_and_resting_motion_are_resolved() {
        let config: SidebarAnimationsConfig =
            toml::from_str("[idle]\neffect = \"breathe\"\n").expect("config");
        let palette = test_palette();
        let animations = ResolvedAnimations::resolve(&config, &palette);
        assert!(animations.has_resting_motion());
        assert_eq!(
            effect_weight(animations.role(AnimationRole::Idle), 0),
            Modifier::DIM
        );
    }

    #[test]
    fn speed_modulates_effect_cadence() {
        let config: SidebarAnimationsConfig = toml::from_str(
            "[idle]\neffect = \"breathe\"\nspeed = \"slow\"\n\n[success]\neffect = \"breathe\"\nspeed = \"fast\"\n",
        )
        .expect("config");
        let palette = test_palette();
        let animations = ResolvedAnimations::resolve(&config, &palette);
        assert_ne!(
            effect_weight(animations.role(AnimationRole::Idle), 5),
            effect_weight(animations.role(AnimationRole::Success), 5),
            "slow and fast breathe effects must diverge on the same render phase"
        );
    }

    #[test]
    fn attention_and_paused_roles_accept_effect_and_speed() {
        let config: SidebarAnimationsConfig = toml::from_str(
            "[waiting]\neffect = \"breathe\"\nspeed = \"fast\"\n\n[paused]\neffect = \"breathe\"\nspeed = \"fast\"\n",
        )
        .expect("config");
        let palette = test_palette();
        let animations = ResolvedAnimations::resolve(&config, &palette);
        assert_eq!(
            animations
                .role(AnimationRole::Waiting)
                .attention_breath_phase(3),
            Some(6),
            "configured speed reaches the attention pulse phase"
        );
        assert!(
            animations.has_resting_motion(),
            "a paused effect override now participates in the uniform model"
        );

        let quiet: SidebarAnimationsConfig =
            toml::from_str("[waiting]\neffect = \"static\"\n").expect("config");
        let animations = ResolvedAnimations::resolve(&quiet, &palette);
        assert_eq!(
            animations
                .role(AnimationRole::Waiting)
                .attention_breath_phase(3),
            None,
            "configured static quiets the default attention pulse"
        );
    }

    #[test]
    fn breath_curve_is_smooth_and_age_tempo_is_clamped() {
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

        let trough = BreathSample::new(0, DEFAULT_BREATH_PERIOD, BREATH_DEEP_AMPLITUDE);
        let middle = BreathSample::new(6, DEFAULT_BREATH_PERIOD, BREATH_DEEP_AMPLITUDE);
        let peak = BreathSample::new(12, DEFAULT_BREATH_PERIOD, BREATH_DEEP_AMPLITUDE);
        assert!(trough.level() < middle.level());
        assert!(middle.level() < peak.level());
        assert!(trough.lightness_delta() < 0.0);
        assert!(peak.lightness_delta() > 0.0);

        let old = BreathSample::for_age(0, 2 * ATTENTION_AGE_CEILING_SECS, BREATH_DEEP_AMPLITUDE);
        let next = BreathSample::for_age(1, 2 * ATTENTION_AGE_CEILING_SECS, BREATH_DEEP_AMPLITUDE);
        assert!(
            (next.lightness_delta() - old.lightness_delta()).abs() < 0.05,
            "hot attention still eases rather than strobing"
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
