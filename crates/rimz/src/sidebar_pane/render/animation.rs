use crate::config::{
    AnimationEffect as ConfigEffect, AnimationSpec, AnimationSpeed as ConfigSpeed,
    SidebarAnimationsConfig,
};
use crate::feed::AgentStatus;
use ratatui::style::{Color, Modifier};

use super::labels::{breath_wave, hard_blink};
use super::theme::{ORANGE, Palette};

const THINKING_FRAMES: &[&str] = &[
    "⠁", "⠂", "⠄", "⡀", "⡈", "⡐", "⡠", "⣀", "⣁", "⣂", "⣄", "⣌", "⣔", "⣤", "⣥", "⣦", "⣮", "⣶", "⣷",
    "⣿", "⡿", "⠿", "⢟", "⠟", "⡛", "⠛", "⠫", "⢋", "⠋", "⠍", "⡉", "⠉", "⠑", "⠡", "⢁",
];

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
    Blink,
}

impl From<ConfigEffect> for Effect {
    fn from(value: ConfigEffect) -> Self {
        match value {
            ConfigEffect::Static => Self::Static,
            ConfigEffect::Breathe => Self::Breathe,
            ConfigEffect::Blink => Self::Blink,
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
        Self::resolve(&SidebarAnimationsConfig::default(), &Palette::BUILTIN)
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
        [AnimationRole::Idle, AnimationRole::Success]
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

pub(crate) fn effect_modifier(animation: &Animation, phase: u64) -> Modifier {
    let phase = animation.speed.effect_phase(phase);
    match animation.effect {
        Effect::Static => Modifier::empty(),
        Effect::Breathe => breath_wave(phase),
        Effect::Blink => hard_blink(phase),
    }
}

fn resolve_role(role: AnimationRole, spec: Option<&AnimationSpec>, palette: &Palette) -> Animation {
    let mut animation = builtin(role);
    if let Some(spec) = spec {
        if let Some(frames) = spec.frames.as_ref() {
            animation.frames = frames.as_slice().to_vec();
        }
        if let Some(color) = spec.color {
            animation.color = palette.animation_color(color);
            animation.color_overridden = true;
        }
        if role_allows_effect(role) {
            if let Some(effect) = spec.effect {
                animation.effect = effect.into();
            }
            if let Some(speed) = spec.speed {
                animation.speed = speed.into();
            }
        }
    }
    animation
}

fn role_allows_effect(role: AnimationRole) -> bool {
    !matches!(
        role,
        AnimationRole::Waiting | AnimationRole::Failed | AnimationRole::Paused
    )
}

fn builtin(role: AnimationRole) -> Animation {
    let (frames, color, effect, speed) = match role {
        AnimationRole::Thinking => (
            THINKING_FRAMES.to_vec(),
            ORANGE,
            Effect::Static,
            Speed::Fast,
        ),
        AnimationRole::Working => (
            vec!["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"],
            ORANGE,
            Effect::Static,
            Speed::Fast,
        ),
        AnimationRole::Compacting => (
            vec!["▁", "▃", "▄", "▅", "▆", "▇", "▆", "▅", "▄", "▃"],
            Color::Magenta,
            Effect::Static,
            Speed::Fast,
        ),
        AnimationRole::Delegating => (
            vec!["⢄", "⢂", "⢁", "⡁", "⡈", "⡐", "⡠"],
            ORANGE,
            Effect::Static,
            Speed::Fast,
        ),
        AnimationRole::Resolving => (
            vec!["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
            Color::Magenta,
            Effect::Static,
            Speed::Fast,
        ),
        AnimationRole::Idle => (vec!["○"], Color::Green, Effect::Static, Speed::Normal),
        AnimationRole::Success => (vec!["✓"], Color::Green, Effect::Static, Speed::Normal),
        AnimationRole::Paused => (
            vec!["⏸\u{FE0E}"],
            Color::Yellow,
            Effect::Static,
            Speed::Normal,
        ),
        AnimationRole::Waiting => (vec!["?"], Color::Yellow, Effect::Static, Speed::Normal),
        AnimationRole::Failed => (vec!["!"], Color::Red, Effect::Static, Speed::Normal),
    };
    Animation {
        frames: frames.into_iter().map(ToOwned::to_owned).collect(),
        color,
        effect,
        speed,
        color_overridden: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_config_resolves_to_builtins() {
        let animations = ResolvedAnimations::default();
        assert_eq!(
            animations.role(AnimationRole::Thinking).frames(),
            THINKING_FRAMES
        );
        assert_eq!(animations.role(AnimationRole::Working).color(), ORANGE);
        assert_eq!(
            animations.role(AnimationRole::Failed).color(),
            Color::Red,
            "the static failed marker remains alarm-red"
        );
    }

    #[test]
    fn partial_override_changes_only_the_named_field() {
        let config: SidebarAnimationsConfig =
            toml::from_str("[thinking]\nframes = \"ab\"\n").expect("config");
        let animations = ResolvedAnimations::resolve(&config, &Palette::BUILTIN);
        let thinking = animations.role(AnimationRole::Thinking);
        assert_eq!(thinking.frames(), ["a", "b"]);
        assert_eq!(thinking.color(), ORANGE);
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
        let palette = Palette::resolve(&crate::config::SidebarThemeConfig {
            good: Some(34),
            ..crate::config::SidebarThemeConfig::default()
        });
        let animations = ResolvedAnimations::resolve(&config, &palette);
        assert_eq!(animations.role(AnimationRole::Working).color(), ORANGE);
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
    fn effects_and_resting_motion_are_resolved() {
        let config: SidebarAnimationsConfig =
            toml::from_str("[idle]\neffect = \"breathe\"\n").expect("config");
        let animations = ResolvedAnimations::resolve(&config, &Palette::BUILTIN);
        assert!(animations.has_resting_motion());
        assert_eq!(
            effect_modifier(animations.role(AnimationRole::Idle), 0),
            Modifier::DIM
        );
    }

    #[test]
    fn speed_modulates_effect_cadence() {
        let config: SidebarAnimationsConfig = toml::from_str(
            "[idle]\neffect = \"breathe\"\nspeed = \"slow\"\n\n[success]\neffect = \"breathe\"\nspeed = \"fast\"\n",
        )
        .expect("config");
        let animations = ResolvedAnimations::resolve(&config, &Palette::BUILTIN);
        assert_ne!(
            effect_modifier(animations.role(AnimationRole::Idle), 5),
            effect_modifier(animations.role(AnimationRole::Success), 5),
            "slow and fast breathe effects must diverge on the same render phase"
        );

        let config: SidebarAnimationsConfig = toml::from_str(
            "[idle]\neffect = \"blink\"\nspeed = \"slow\"\n\n[success]\neffect = \"blink\"\nspeed = \"fast\"\n",
        )
        .expect("config");
        let animations = ResolvedAnimations::resolve(&config, &Palette::BUILTIN);
        assert_ne!(
            effect_modifier(animations.role(AnimationRole::Idle), 2),
            effect_modifier(animations.role(AnimationRole::Success), 2),
            "slow and fast blink effects must diverge on the same render phase"
        );
    }

    #[test]
    fn attention_and_paused_roles_ignore_effect_and_speed() {
        let config: SidebarAnimationsConfig = toml::from_str(
            "[waiting]\neffect = \"blink\"\nspeed = \"fast\"\n\n[paused]\neffect = \"breathe\"\nspeed = \"fast\"\n",
        )
        .expect("config");
        let animations = ResolvedAnimations::resolve(&config, &Palette::BUILTIN);
        assert_eq!(
            effect_modifier(animations.role(AnimationRole::Waiting), 0),
            Modifier::empty()
        );
        assert_eq!(
            effect_modifier(animations.role(AnimationRole::Paused), 0),
            Modifier::empty()
        );
        assert!(
            !animations.has_resting_motion(),
            "a paused effect override is ignored and should not wake cadence"
        );
    }
}
