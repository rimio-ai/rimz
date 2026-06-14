use super::*;
use crate::sidebar_pane::render::theme::Component;

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
/// states keep their single fixed head and fixed status tone while their urgency
/// lives in the unread modifier effects. Stall is already folded into `Failed`
/// upstream, so it falls through to the static `!`.
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

/// The completed-compaction count marker's tone: the warn slot, kept separate
/// from cache-write's violet.
pub(in crate::sidebar_pane::render) fn compacting_style(theme: &Theme) -> Style {
    theme.styled(Component::Compaction, Modifier::empty())
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
    /// The shared unread treatment (shimmer / bright / blink), `Some` only on an
    /// unread (`Blink`-emphasis) row and `None` when a per-role `effect =
    /// "static"` quiets it — the lead glyph, the name, the description, and the
    /// make-up buckets all read from this one value.
    pub(in crate::sidebar_pane::render) anim: Option<UnreadAnim>,
}

impl CardAttention {
    pub(in crate::sidebar_pane::render) fn new(
        theme: &Theme,
        status: AgentStatus,
        age_secs: i64,
        animation_phase: u64,
        unread: bool,
        selected: bool,
        is_lead: bool,
    ) -> Self {
        let emphasis = card_emphasis(status, unread, selected);
        let anim = if emphasis == CardEmphasis::Blink {
            unread_anim(theme, status, age_secs, animation_phase, is_lead)
        } else {
            None
        };
        Self { emphasis, anim }
    }
}

pub(in crate::sidebar_pane::render) fn card_emphasis(
    status: AgentStatus,
    unread: bool,
    selected: bool,
) -> CardEmphasis {
    if unread {
        CardEmphasis::Blink
    } else if status.needs_a_look() || selected {
        CardEmphasis::Normal
    } else {
        CardEmphasis::Soft
    }
}

/// The shared unread treatment for a row of `status` at `age_secs`. The single
/// **lead** unread row — the oldest one that needs an answer — wears the
/// configured [`unread_effect`](crate::sidebar_pane::render::animation::ResolvedAnimations::unread_effect)
/// (the shimmer beam or the 2-pole blink), so the one pane that most needs you is
/// the only thing in continuous motion; every other unread row settles to the
/// steady [`bright`](crate::sidebar_pane::render::animation::UnreadEffect::Bright)
/// crest — unmistakable by contrast, but still. `None` when a per-role `effect =
/// "static"` quiets it. `is_lead` is the renderer's reservation flag; with no
/// reservation context (`None` lead, as in a single-row unit test) every unread
/// row reads as its own lead.
pub(in crate::sidebar_pane::render) fn unread_anim(
    theme: &Theme,
    status: AgentStatus,
    age_secs: i64,
    animation_phase: u64,
    is_lead: bool,
) -> Option<UnreadAnim> {
    let effect = if is_lead {
        theme.animations.unread_effect()
    } else {
        UnreadEffect::Bright
    };
    theme
        .animations
        .status(status)
        .unread_anim(effect, animation_phase, age_secs)
}

/// One cell under a concrete unread treatment, on its resolved base `color`. The
/// glyph and each make-up bucket are single cells (`len` 1); a multi-cell run
/// (the name, the description) calls this per cell with its own `index` so the
/// shimmer beam flows. Blink pulses on the 2-pole sample, bright holds the
/// crest, shimmer lifts by the beam — each falling back to a weight modifier
/// when `color` is absent or the depth cannot carry the lift.
pub(in crate::sidebar_pane::render) fn attention_cell_style(
    theme: &Theme,
    color: Option<Color>,
    anim: UnreadAnim,
    index: usize,
    len: usize,
) -> Style {
    match anim {
        UnreadAnim::Blink(sample) => pulse_or_weight(theme, color, sample),
        UnreadAnim::Bright => pulse_or_weight(
            theme,
            color,
            BreathSample::steady_peak(BREATH_DEEP_AMPLITUDE),
        ),
        UnreadAnim::Shimmer(wave) => theme.shimmer_cell(color, shimmer_lift(wave, index, len)),
    }
}

/// The spans for a text run on an **unread** (`Blink`-emphasis) row, on its
/// resolved base `color`: shimmer flows the beam across one span per character,
/// while blink and bright keep a single uniform span, and a quieted row (`None`)
/// holds a constant bold tone. Callers own the non-unread `Normal`/`Soft` tiers.
pub(in crate::sidebar_pane::render) fn unread_run_spans(
    theme: &Theme,
    color: Option<Color>,
    anim: Option<UnreadAnim>,
    text: &str,
) -> Vec<Span<'static>> {
    match anim {
        Some(UnreadAnim::Shimmer(wave)) => {
            let len = text.chars().count();
            text.chars()
                .enumerate()
                .map(|(index, ch)| {
                    Span::styled(
                        ch.to_string(),
                        theme.shimmer_cell(color, shimmer_lift(wave, index, len)),
                    )
                })
                .collect()
        }
        Some(anim) => vec![Span::styled(
            text.to_owned(),
            attention_cell_style(theme, color, anim, 0, 1),
        )],
        None => vec![Span::styled(text.to_owned(), bold_tone(theme, color))],
    }
}

/// A blink/bright sample as a cell style: pulse the resolved `color`, or fall
/// back to the bold-by-pole weight when the element is colorless.
fn pulse_or_weight(theme: &Theme, color: Option<Color>, sample: BreathSample) -> Style {
    color.map_or_else(
        || Style::default().add_modifier(sample.grow_modifier()),
        |color| theme.pulse(color, sample),
    )
}

/// The quiet (statically-quieted) unread fallback: the resting tone held bold,
/// dropping to plain bold weight when colorless.
fn bold_tone(theme: &Theme, color: Option<Color>) -> Style {
    color.map_or_else(
        || Style::default().add_modifier(Modifier::BOLD),
        |color| theme.style(color, Modifier::BOLD),
    )
}

/// Paint an element's natural tone under the row's card emphasis, so the lead
/// glyph and name move together: blink/bright/shimmer carry the shared unread
/// treatment (a single cell — the glyph, or a name read uniformly when not
/// shimmering), normal wears the tone at full strength, and soft dims its
/// lightness a step (`body_brand`, keeping the hue) so a calm unselected card
/// keeps its color, just quietly. A colorless element (an idle lead) rests at
/// the plain soft body tone. The description mirrors the same split through its
/// own body-style path — it is body text, never brand-colored, so its soft tone
/// is the plain body gray.
pub(in crate::sidebar_pane::render) fn emphasize(
    theme: &Theme,
    natural_color: Option<Color>,
    emphasis: CardEmphasis,
    anim: Option<UnreadAnim>,
) -> Style {
    match emphasis {
        CardEmphasis::Blink => match anim {
            Some(anim) => attention_cell_style(theme, natural_color, anim, 0, 1),
            None => bold_tone(theme, natural_color),
        },
        CardEmphasis::Normal => natural_color.map_or_else(Style::default, |color| {
            theme.style(color, Modifier::empty())
        }),
        CardEmphasis::Soft => {
            natural_color.map_or_else(|| theme.body(), |color| theme.body_brand(color))
        }
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
    let attention = CardAttention::new(
        theme,
        status,
        age_secs,
        animation_phase,
        unread,
        selected,
        true,
    );
    agent_lead_style_with_attention(theme, status, phase, attention)
}

pub(in crate::sidebar_pane::render) fn agent_lead_style_with_attention(
    theme: &Theme,
    status: AgentStatus,
    phase: TurnPhase,
    attention: CardAttention,
) -> Style {
    let natural_color = agent_natural_color(theme, status, phase);
    emphasize(theme, natural_color, attention.emphasis, attention.anim)
}

/// The lead glyph's resting color: the status/phase role's fixed animation tone —
/// waiting yellow, failed red, paused blue, a working head clay — held steady,
/// with a bare (un-themed) idle staying transparent so an idle card reads as
/// plain identity. The unread attention effect (carried by [`CardAttention`])
/// supplies any motion; the tone itself never slides with age.
fn agent_natural_color(theme: &Theme, status: AgentStatus, phase: TurnPhase) -> Option<Color> {
    let role = agent_role(status, phase);
    let animation = theme.animations.role(role);
    if role == AnimationRole::Idle && !animation.color_overridden() {
        None
    } else {
        Some(animation.color())
    }
}

fn agent_role(status: AgentStatus, phase: TurnPhase) -> AnimationRole {
    if status == AgentStatus::Running && phase == TurnPhase::Reasoning {
        AnimationRole::Thinking
    } else {
        crate::sidebar_pane::render::animation::ResolvedAnimations::status_role(status)
    }
}
