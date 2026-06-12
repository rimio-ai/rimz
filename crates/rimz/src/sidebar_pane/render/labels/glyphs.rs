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

/// The brightness modifier for a breathing attention glyph (`?` / `!`) on this
/// frame, paced by the age cadence tier while color slides continuously. Below
/// the half hour it is a slow triangle pulse — `DIM` at the troughs, normal
/// through the middle, `BOLD` at the peak — so the marker swells and fades like
/// a breath (~2.4s at the 100ms animation tick), pulling the eye back to an
/// unanswered row without strobing. Amber doubles the tempo (~1.2s): the row
/// sits past the half hour and the breath quickens with it. Red switches to
/// [`hard_blink`] — past the hour the glyph earns the strobe the young breath
/// avoids. Every tier holds the glyph in its cell (never blanking, so the
/// column never shifts) and is modifier-only, so the urgency cadence survives
/// under `NO_COLOR`.
pub(in crate::sidebar_pane::render) fn attention_breath(
    animation_phase: u64,
    age_secs: i64,
) -> Modifier {
    match heat_cadence(age_secs) {
        Some(HeatCadence::Red) => hard_blink(animation_phase),
        // Amber: the same triangle at double-time.
        Some(HeatCadence::Amber) => breath_wave(animation_phase.wrapping_mul(2)),
        None => breath_wave(animation_phase),
    }
}

/// A hard square-wave blink: `BOLD` for three phases, `DIM` for three, holding
/// the glyph in place so the column stays fixed under color and `NO_COLOR`.
pub(in crate::sidebar_pane::render) fn hard_blink(animation_phase: u64) -> Modifier {
    if animation_phase % 6 < 3 {
        Modifier::BOLD
    } else {
        Modifier::DIM
    }
}

/// One step of the breath's triangle wave: rise `DIM` → normal → `BOLD` over
/// the first half-cycle, fall back over the second.
pub(in crate::sidebar_pane::render) fn breath_wave(phase: u64) -> Modifier {
    const CYCLE: u64 = 24;
    let pos = phase % CYCLE;
    // Distance toward the peak at the half-cycle: rise 0→12, then fall 12→24.
    let level = if pos <= CYCLE / 2 { pos } else { CYCLE - pos };
    match level {
        0..=3 => Modifier::DIM,
        4..=8 => Modifier::empty(),
        _ => Modifier::BOLD,
    }
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
        AgentStatus::Idle | AgentStatus::Success => {
            frame_at(theme.animations.status(status), animation_phase)
        }
        other => status_glyph(theme, other),
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
    role_style_with_modifier(theme, role, effect_modifier(animation, animation_phase))
}

fn role_style_with_modifier(theme: &Theme, role: AnimationRole, modifier: Modifier) -> Style {
    let animation = theme.animations.role(role);
    if role == AnimationRole::Idle && !animation.color_overridden() {
        Style::default().add_modifier(modifier)
    } else {
        theme.style(animation.color(), modifier)
    }
}

/// The compacting head's tone: cool violet, the token/context-domain color the
/// `◇` token glyph already uses, so a pulsing context-condense reads as
/// housekeeping rather than the clay working fill.
pub(in crate::sidebar_pane::render) fn compacting_style(theme: &Theme) -> Style {
    role_style(theme, AnimationRole::Compacting, 0)
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

/// Style for an agent row's leading cell. A running agent's working spinner and
/// its thinking head both paint in Claude clay by default, so the live head
/// aligns with the agent's own UI; every other state takes its [`status_style`].
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

pub(in crate::sidebar_pane::render) fn agent_lead_style(
    theme: &Theme,
    status: AgentStatus,
    phase: TurnPhase,
    age_secs: i64,
    animation_phase: u64,
    unread: bool,
) -> Style {
    let role = agent_role(status, phase);
    if unread && status.is_actionable() {
        let color =
            age_heat_color(theme, age_secs).unwrap_or_else(|| attention_floor_color(theme, status));
        theme.style(color, hard_blink(animation_phase))
    } else if unread {
        role_style_with_modifier(theme, role, hard_blink(animation_phase))
    } else if status.is_actionable() {
        let color =
            age_heat_color(theme, age_secs).unwrap_or_else(|| attention_floor_color(theme, status));
        theme.style(color, attention_breath(animation_phase, age_secs))
    } else {
        role_style(theme, role, animation_phase)
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
