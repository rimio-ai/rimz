use super::*;

/// The static status glyph — used for the legend, the worktree tally, the
/// attention line, and as the leading cell for every non-animated state. The
/// shape carries the status under `NO_COLOR`; color reinforces it. `Running`
/// returns a representative working frame `⢿` as the still fallback (distinct
/// from idle `○`); the *animated* working/thinking cells live in
/// [`working_glyph`]/[`thinking_glyph`]. Idle is a hollow `○` (the filled `◌`
/// reads as "cached" in the token line). A `running` agent that has gone silent
/// past the stall window is projected to `Failed` upstream, so it reads here as
/// the attention `!` — there is no separate stalled glyph.
pub(in crate::sidebar_pane::render) fn status_glyph(status: AgentStatus) -> &'static str {
    match status {
        // `?` needs your answer; `!` needs a look (a failed turn or a wedged
        // agent); `⏸` is paused mid-turn on a provider limit. The three attention-class
        // states — the first two actionable, the last a non-actionable wait.
        AgentStatus::Waiting => "?",
        AgentStatus::Failed => "!",
        AgentStatus::Running => WORKING_FRAMES[3],
        AgentStatus::Idle => "○",
        AgentStatus::Success => "✓",
        AgentStatus::Paused => PAUSED_GLYPH,
    }
}

/// Paused: a media `pause` mark carrying the text-presentation selector
/// (`U+FE0E`) so it renders as a single-cell monochrome glyph, never a
/// double-width color emoji that would shift the cockpit columns after it. The
/// agent stopped mid-turn on a provider limit, so it waits at rest until the
/// provider recovers or the window resets.
pub(in crate::sidebar_pane::render) const PAUSED_GLYPH: &str = "⏸\u{FE0E}";

/// Working: a braille spinner cycling its dots. Spans the most time of any
/// state, so it is the steady motion the eye learns to ignore until something
/// changes. No frame matches idle `○`, so a frozen frame still reads as "working".
pub(in crate::sidebar_pane::render) const WORKING_FRAMES: [&str; 8] =
    ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];

/// Thinking: a sparkle that grows, fades back down, then repeats. The opening
/// phase of every turn — the agent is reasoning and reading, not yet writing —
/// so its motion reads as lighter than the working fill. The turn's first file
/// edit flips the cell to the working spinner.
pub(in crate::sidebar_pane::render) const THINKING_FRAMES: [&str; 8] =
    ["·", "✢", "✳", "✶", "✻", "✶", "✳", "✢"];
pub(in crate::sidebar_pane::render) const THINKING_FRAME_HOLD: u64 = 3;

/// Resolver answering: a braille spinner while a resolver composes the answer on
/// the bridge. This is the one "waiting for an answer" motion — it is genuinely
/// active and time-bounded by the resolver budget, unlike a human-blocked `?`,
/// which stays still.
pub(in crate::sidebar_pane::render) const RESOLVER_FRAMES: [&str; 10] =
    ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Compacting: a single bar pulsing taller then shorter, like a compression
/// meter squeezing the context window down. Short-lived — the next lifecycle
/// event returns the agent to its resting head — so it never earns a cockpit
/// bucket; it paints in cool violet (the token/context-domain color) to read as
/// housekeeping, not the clay working fill.
pub(in crate::sidebar_pane::render) const COMPACTING_FRAMES: [&str; 10] =
    ["▁", "▃", "▄", "▅", "▆", "▇", "▆", "▅", "▄", "▃"];

/// Waiting on subagents: a low tick bobbing up off the baseline and back — a
/// quiet wave that reads as "the work is in the children below", distinct from
/// the dense working braille. Stays in the agent's clay: the parent is still its
/// live head, just delegating.
pub(in crate::sidebar_pane::render) const SUBAGENT_FRAMES: [&str; 8] =
    ["_", "-", "`", "´", "'", "´", "`", "-"];

/// Idle, waiting-for-a-prompt: a static `...` placeholder that stands in for the
/// em-dash on a just-started agent with nothing to describe yet.
const LOADING_DOTS: &str = "...";

fn frame(frames: &[&'static str], animation_phase: u64) -> &'static str {
    frames[(animation_phase as usize) % frames.len()]
}

/// The idle loading-dots cue. The phase argument is accepted so the card render
/// path stays aligned with the other glyph helpers, but idle agents stay still.
pub(in crate::sidebar_pane::render) fn loading_dots(_animation_phase: u64) -> &'static str {
    LOADING_DOTS
}

/// The brightness modifier for a breathing attention glyph (`?` / `!`) on this
/// frame, paced by the same [`age_heat`] ramp the glyph's color wears. While
/// yellow it is a slow triangle pulse — `DIM` at the troughs, normal through
/// the middle, `BOLD` at the peak — so the marker swells and fades like a
/// breath (~2.4s at the 100ms animation tick), pulling the eye back to an
/// unanswered row without strobing. Amber doubles the tempo (~1.2s): the row
/// sits past the half hour and the breath quickens with it. Red drops the
/// swell for a hard `BOLD`↔`DIM` blink (~0.6s, flipping on the slow paint
/// grid) — past the hour the glyph earns the strobe the young breath avoids.
/// Every tier holds the glyph in its cell (never blanking, so the column never
/// shifts) and is modifier-only, so the urgency cadence survives under
/// `NO_COLOR`.
pub(in crate::sidebar_pane::render) fn attention_breath(
    animation_phase: u64,
    age_secs: i64,
) -> Modifier {
    match age_heat(age_secs) {
        // Red: a hard square wave, flipping every third tick.
        Some(Color::Red) => {
            if animation_phase % 6 < 3 {
                Modifier::BOLD
            } else {
                Modifier::DIM
            }
        }
        // Amber: the same triangle at double-time.
        Some(color) if color == ORANGE => breath_wave(animation_phase.wrapping_mul(2)),
        // Yellow (including the fresh yellow floor): the resting cadence.
        _ => breath_wave(animation_phase),
    }
}

/// One step of the breath's triangle wave: rise `DIM` → normal → `BOLD` over
/// the first half-cycle, fall back over the second.
fn breath_wave(phase: u64) -> Modifier {
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

pub(in crate::sidebar_pane::render) fn working_glyph(animation_phase: u64) -> &'static str {
    frame(&WORKING_FRAMES, animation_phase)
}

pub(in crate::sidebar_pane::render) fn thinking_glyph(animation_phase: u64) -> &'static str {
    frame(&THINKING_FRAMES, animation_phase / THINKING_FRAME_HOLD)
}

pub(in crate::sidebar_pane::render) fn resolver_glyph(animation_phase: u64) -> &'static str {
    frame(&RESOLVER_FRAMES, animation_phase)
}

pub(in crate::sidebar_pane::render) fn compacting_glyph(animation_phase: u64) -> &'static str {
    frame(&COMPACTING_FRAMES, animation_phase)
}

pub(in crate::sidebar_pane::render) fn subagent_glyph(animation_phase: u64) -> &'static str {
    frame(&SUBAGENT_FRAMES, animation_phase)
}

/// The leading cell for an agent row, animated when the agent is actively doing
/// something. A `running` agent sparkles (reasoning, before the turn's first
/// file edit) or fills (acting or parked); every other state is the static
/// [`status_glyph`]. Stall is already folded into `Failed` upstream, so it
/// falls through to the static `!`.
pub(in crate::sidebar_pane::render) fn agent_glyph(
    status: AgentStatus,
    phase: TurnPhase,
    animation_phase: u64,
) -> &'static str {
    match status {
        AgentStatus::Running if phase == TurnPhase::Reasoning => thinking_glyph(animation_phase),
        AgentStatus::Running => working_glyph(animation_phase),
        other => status_glyph(other),
    }
}

pub(in crate::sidebar_pane::render) fn status_style(theme: &Theme, status: AgentStatus) -> Style {
    match status {
        AgentStatus::Waiting => theme.style(Color::Yellow, Modifier::BOLD),
        AgentStatus::Failed => theme.style(Color::Red, Modifier::BOLD),
        AgentStatus::Running => theme.style(Color::Green, Modifier::empty()),
        // Idle and success are the two calm "nothing needs you" states, so both
        // read in a quiet green at full strength — the hollow `○` and the `✓`
        // carry the meaning, and the rest weight (no bold, no breath) already
        // keeps them below the live attention states.
        AgentStatus::Idle => theme.style(Color::Green, Modifier::empty()),
        AgentStatus::Success => theme.style(Color::Green, Modifier::empty()),
        // Paused stays in the amber attention family but at rest weight
        // (not bold, and `attention_glyph_style` never heats it): it is
        // attention-class, but parked with nothing to do right now — the held
        // tone sets it apart from the loud, actionable `?`/`!`.
        AgentStatus::Paused => theme.style(Color::Yellow, Modifier::empty()),
    }
}

/// The compacting head's tone: cool violet, the token/context-domain color the
/// `◇` token glyph already uses, so a pulsing context-condense reads as
/// housekeeping rather than the clay working fill.
pub(in crate::sidebar_pane::render) fn compacting_style(theme: &Theme) -> Style {
    theme.style(Color::Magenta, Modifier::empty())
}

/// The waiting-on-subagents head's tone: the agent's clay, same as the working
/// fill — the parent is still its live head, just delegating; the quiet wave
/// motion, not the color, carries "the work is in the children".
pub(in crate::sidebar_pane::render) fn subagent_style(theme: &Theme) -> Style {
    theme.style(ORANGE, Modifier::empty())
}

/// Style for an agent row's leading cell. A running agent's working spinner and
/// its thinking sparkle both paint in Claude clay, so the live head aligns with
/// the agent's own UI; every other state takes its [`status_style`].
pub(in crate::sidebar_pane::render) fn agent_style(theme: &Theme, status: AgentStatus) -> Style {
    if status == AgentStatus::Running {
        return theme.style(ORANGE, Modifier::empty());
    }
    status_style(theme, status)
}

/// Style for an agent row's leading glyph. Both attention states — `?` waiting
/// and `!` failed — breathe (a `DIM`↔`BOLD` brightness pulse, see
/// [`attention_breath`]) and wear the shared [`age_heat`] over a yellow floor:
/// a fresh ask reads calm-urgent yellow ("a human is needed here") and heats
/// through amber to red on the same quarter-hour ramp as the age clock beside
/// it, so the glyph and the clock never disagree while warm. The breath paces
/// with the same heat — slow while yellow, double-time at amber, a hard blink
/// at red. Every calm state keeps its resting [`agent_style`] tone,
/// unbreathing.
pub(in crate::sidebar_pane::render) fn attention_glyph_style(
    theme: &Theme,
    status: AgentStatus,
    age_secs: i64,
    animation_phase: u64,
) -> Style {
    if status.is_actionable() {
        let color = age_heat(age_secs).unwrap_or(Color::Yellow);
        theme.style(color, attention_breath(animation_phase, age_secs))
    } else {
        agent_style(theme, status)
    }
}
