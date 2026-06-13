use super::*;

/// Paused: a media `pause` mark carrying the text-presentation selector
/// (`U+FE0E`) so it renders as a single-cell monochrome glyph, never a
/// double-width color emoji that would shift the cockpit columns after it. The
/// agent stopped mid-turn on a provider limit, so it waits at rest until the
/// provider recovers or the window resets.
#[cfg(test)]
pub(in crate::sidebar_pane::render) const PAUSED_GLYPH: &str = "⏸\u{FE0E}";

/// The static status glyph — used for the legend, the worktree tally, the
/// attention line, and as the leading cell for every non-animated state. The
/// shape carries the status under `NO_COLOR`; color reinforces it. `Running`
/// returns the representative working frame `⢿` as the still fallback
/// (distinct from idle `○`); the animated working/thinking cells live in the
/// role-specific helpers below.
pub(in crate::sidebar_pane::render) fn status_glyph(theme: &Theme, status: AgentStatus) -> String {
    let animation = theme.animations.status(status);
    if status == AgentStatus::Running {
        return frame_at(animation, 3);
    }
    still_frame(animation)
}

/// Idle, waiting-for-a-prompt: a static `...` placeholder that stands in for the
/// em-dash on a just-started agent with nothing to describe yet.
const LOADING_DOTS: &str = "...";

/// The idle loading-dots cue. The phase argument is accepted so the card render
/// path stays aligned with the other glyph helpers, but idle agents stay still.
pub(in crate::sidebar_pane::render) fn loading_dots(_animation_phase: u64) -> &'static str {
    LOADING_DOTS
}

/// The clock-fill glyph for an elapsed span: the face fills a quarter per
/// quarter hour — `◔` to 15m, `◑` to 30m, `◕` to 45m, `●` to the hour — and
/// past the hour reads the ringed `◉`, so any time readout on a card carries
/// its magnitude iconographically. One cell, so it never disturbs alignment.
pub(in crate::sidebar_pane::render) fn elapsed_glyph(secs: i64) -> &'static str {
    match secs {
        i64::MIN..=900 => "◔",
        901..=1800 => "◑",
        1801..=2700 => "◕",
        2701..=3600 => "●",
        _ => "◉",
    }
}

pub(in crate::sidebar_pane::render) fn working_glyph(
    theme: &Theme,
    animation_phase: u64,
) -> String {
    frame_at(
        theme.animations.role(AnimationRole::Working),
        animation_phase,
    )
}

pub(in crate::sidebar_pane::render) fn thinking_glyph(
    theme: &Theme,
    animation_phase: u64,
) -> String {
    frame_at(
        theme.animations.role(AnimationRole::Thinking),
        animation_phase,
    )
}

pub(in crate::sidebar_pane::render) fn resolver_glyph(
    theme: &Theme,
    animation_phase: u64,
) -> String {
    frame_at(
        theme.animations.role(AnimationRole::Resolving),
        animation_phase,
    )
}

pub(in crate::sidebar_pane::render) fn compacting_glyph(
    theme: &Theme,
    animation_phase: u64,
) -> String {
    frame_at(
        theme.animations.role(AnimationRole::Compacting),
        animation_phase,
    )
}

pub(in crate::sidebar_pane::render) fn subagent_glyph(
    theme: &Theme,
    animation_phase: u64,
) -> String {
    frame_at(
        theme.animations.role(AnimationRole::Delegating),
        animation_phase,
    )
}

/// The leading cell for an agent row. A `running` agent shows the thinking
/// head (reasoning, before the turn's first file edit) or fills (acting or
/// parked); calm terminal states use their status animation frames; attention
/// states keep their single fixed head while their urgency lives in color and
/// modifier effects. Stall is already folded into `Failed` upstream, so it
/// falls through to the static `!`.
pub(in crate::sidebar_pane::render) fn agent_glyph(
    theme: &Theme,
    status: AgentStatus,
    phase: TurnPhase,
    animation_phase: u64,
) -> String {
    match status {
        AgentStatus::Running if phase == TurnPhase::Reasoning => {
            thinking_glyph(theme, animation_phase)
        }
        AgentStatus::Running => working_glyph(theme, animation_phase),
        AgentStatus::Idle
        | AgentStatus::Success
        | AgentStatus::Paused
        | AgentStatus::Waiting
        | AgentStatus::Failed => frame_at(theme.animations.status(status), animation_phase),
    }
}

pub(in crate::sidebar_pane::render) fn status_style(theme: &Theme, status: AgentStatus) -> Style {
    status_style_at(theme, status, 0)
}

pub(in crate::sidebar_pane::render) fn status_style_at(
    theme: &Theme,
    status: AgentStatus,
    animation_phase: u64,
) -> Style {
    role_style(
        theme,
        crate::sidebar_pane::render::animation::ResolvedAnimations::status_role(status),
        animation_phase,
    )
}

pub(in crate::sidebar_pane::render) fn status_rest_style(
    theme: &Theme,
    status: AgentStatus,
) -> Style {
    status_style_with_modifier(theme, status, Modifier::empty())
}

pub(in crate::sidebar_pane::render) fn status_style_with_modifier(
    theme: &Theme,
    status: AgentStatus,
    modifier: Modifier,
) -> Style {
    role_style_with_modifier(
        theme,
        crate::sidebar_pane::render::animation::ResolvedAnimations::status_role(status),
        modifier,
    )
}

pub(in crate::sidebar_pane::render) fn status_chip_color(
    theme: &Theme,
    status: AgentStatus,
) -> Option<Color> {
    let animation = theme.animations.status(status);
    if status == AgentStatus::Idle && !animation.color_overridden() {
        None
    } else {
        Some(animation.color())
    }
}

fn role_style(theme: &Theme, role: AnimationRole, animation_phase: u64) -> Style {
    let animation = theme.animations.role(role);
    if role == AnimationRole::Idle && !animation.color_overridden() {
        Style::default().add_modifier(effect_weight(animation, animation_phase))
    } else {
        effect_style(theme, animation, animation_phase)
    }
}

fn role_style_with_modifier(theme: &Theme, role: AnimationRole, modifier: Modifier) -> Style {
    let animation = theme.animations.role(role);
    if role == AnimationRole::Idle && !animation.color_overridden() {
        Style::default().add_modifier(modifier)
    } else {
        theme.style(animation.color(), modifier)
    }
}

/// The completed-compaction count marker's tone: yellow, so the count stays
/// separate from cache-write's violet.
pub(in crate::sidebar_pane::render) fn compacting_style(theme: &Theme) -> Style {
    theme.style(Color::Yellow, Modifier::empty())
}

pub(in crate::sidebar_pane::render) fn compacting_head_style(
    theme: &Theme,
    animation_phase: u64,
) -> Style {
    role_style(theme, AnimationRole::Compacting, animation_phase)
}

/// The waiting-on-subagents head's tone: the agent's clay, same as the working
/// fill — the parent is still its live head, just delegating; the quiet wave
/// motion, not the color, carries "the work is in the children".
pub(in crate::sidebar_pane::render) fn subagent_head_style(
    theme: &Theme,
    animation_phase: u64,
) -> Style {
    role_style(theme, AnimationRole::Delegating, animation_phase)
}

pub(in crate::sidebar_pane::render) fn resolver_style(
    theme: &Theme,
    animation_phase: u64,
) -> Style {
    role_style(theme, AnimationRole::Resolving, animation_phase)
}

pub(in crate::sidebar_pane::render) fn working_style(theme: &Theme, animation_phase: u64) -> Style {
    role_style(theme, AnimationRole::Working, animation_phase)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sidebar_pane::render) enum CardEmphasis {
    Blink,
    Normal,
    Soft,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::sidebar_pane::render) struct CardAttention {
    pub(in crate::sidebar_pane::render) emphasis: CardEmphasis,
    pub(in crate::sidebar_pane::render) pulse: Option<BreathSample>,
}

impl CardAttention {
    pub(in crate::sidebar_pane::render) fn new(
        theme: &Theme,
        status: AgentStatus,
        age_secs: i64,
        animation_phase: u64,
        unread: bool,
        selected: bool,
    ) -> Self {
        let emphasis = card_emphasis(status, unread, selected);
        let pulse = if emphasis == CardEmphasis::Blink {
            attention_blink_sample(theme, status, age_secs, animation_phase)
        } else {
            None
        };
        Self { emphasis, pulse }
    }
}

pub(in crate::sidebar_pane::render) fn card_emphasis(
    status: AgentStatus,
    unread: bool,
    selected: bool,
) -> CardEmphasis {
    if unread && (status.is_actionable() || status == AgentStatus::Success) {
        CardEmphasis::Blink
    } else if status.needs_a_look() || selected {
        CardEmphasis::Normal
    } else {
        CardEmphasis::Soft
    }
}

pub(in crate::sidebar_pane::render) fn attention_blink_sample(
    theme: &Theme,
    status: AgentStatus,
    age_secs: i64,
    animation_phase: u64,
) -> Option<BreathSample> {
    theme.animations.status(status).attention_pulse(
        animation_phase,
        age_secs,
        BREATH_DEEP_AMPLITUDE,
    )
}

pub(in crate::sidebar_pane::render) fn emphasize(
    theme: &Theme,
    natural_color: Option<Color>,
    emphasis: CardEmphasis,
    pulse: Option<BreathSample>,
) -> Style {
    match emphasis {
        CardEmphasis::Blink => match (natural_color, pulse) {
            (Some(color), Some(sample)) => theme.pulse(color, sample),
            (Some(color), None) => theme.style(color, Modifier::BOLD),
            (None, Some(sample)) => Style::default().add_modifier(sample.grow_modifier()),
            (None, None) => Style::default().add_modifier(Modifier::BOLD),
        },
        CardEmphasis::Normal => natural_color.map_or_else(Style::default, |color| {
            theme.style(color, Modifier::empty())
        }),
        CardEmphasis::Soft => theme.soft(),
    }
}

/// Style for an agent row's leading cell. A running agent's working spinner and
/// its thinking head both paint in Claude clay by default, so the live head
/// aligns with the agent's own UI; every other state takes its [`status_style`].
#[cfg(test)]
pub(in crate::sidebar_pane::render) fn agent_style_at(
    theme: &Theme,
    status: AgentStatus,
    animation_phase: u64,
) -> Style {
    status_style_at(theme, status, animation_phase)
}

pub(in crate::sidebar_pane::render) fn agent_role_style_at(
    theme: &Theme,
    status: AgentStatus,
    phase: TurnPhase,
    animation_phase: u64,
) -> Style {
    role_style(theme, agent_role(status, phase), animation_phase)
}

#[cfg(test)]
pub(in crate::sidebar_pane::render) fn agent_lead_style(
    theme: &Theme,
    status: AgentStatus,
    phase: TurnPhase,
    age_secs: i64,
    animation_phase: u64,
    unread: bool,
    selected: bool,
) -> Style {
    let attention = CardAttention::new(theme, status, age_secs, animation_phase, unread, selected);
    agent_lead_style_with_attention(theme, status, phase, age_secs, attention)
}

pub(in crate::sidebar_pane::render) fn agent_lead_style_with_attention(
    theme: &Theme,
    status: AgentStatus,
    phase: TurnPhase,
    age_secs: i64,
    attention: CardAttention,
) -> Style {
    let natural_color = agent_natural_color(theme, status, phase, age_secs);
    emphasize(theme, natural_color, attention.emphasis, attention.pulse)
}

fn agent_natural_color(
    theme: &Theme,
    status: AgentStatus,
    phase: TurnPhase,
    age_secs: i64,
) -> Option<Color> {
    let role = agent_role(status, phase);
    if status.is_actionable() {
        Some(
            age_heat_color(theme, age_secs).unwrap_or_else(|| attention_floor_color(theme, status)),
        )
    } else {
        let animation = theme.animations.role(role);
        if role == AnimationRole::Idle && !animation.color_overridden() {
            None
        } else {
            Some(animation.color())
        }
    }
}

fn agent_role(status: AgentStatus, phase: TurnPhase) -> AnimationRole {
    if status == AgentStatus::Running && phase == TurnPhase::Reasoning {
        AnimationRole::Thinking
    } else {
        crate::sidebar_pane::render::animation::ResolvedAnimations::status_role(status)
    }
}

pub(in crate::sidebar_pane::render) fn attention_floor_color(
    theme: &Theme,
    status: AgentStatus,
) -> Color {
    let animation = theme.animations.status(status);
    if animation.color_overridden() {
        animation.color()
    } else {
        Color::Yellow
    }
}
