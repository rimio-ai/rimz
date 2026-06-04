//! Semantic sidebar vocabulary: the canonical status glyphs and the
//! gauge / spinner / pulse glyph helpers.
//!
//! Every meter in the sidebar — context-window %, todo progress, diff stats —
//! renders through the same vocabulary so they read as siblings, not as
//! one-off widgets (see [the sidebar grammar](../../../docs/internals/sidebar.md)).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use rimz::agents::TurnPhase;
use rimz::config::ContextSeverityConfig;
use rimz::feed::AgentStatus;

use super::theme::{ORANGE, Theme};

/// The static status glyph — used for the legend, the worktree tally, the
/// attention line, and as the leading cell for every non-animated state. The
/// shape carries the status under `NO_COLOR`; color reinforces it. `Running`
/// returns a representative working frame `⢿` as the still fallback (distinct
/// from idle `○`); the *animated* working/thinking cells live in
/// [`working_glyph`]/[`thinking_glyph`]. Idle is a hollow `○` (the filled `◌`
/// reads as "cached" in the token line). A `running` agent that has gone silent
/// past the stall window is projected to `Failed` upstream, so it reads here as
/// the attention `!` — there is no separate stalled glyph.
pub(super) fn status_glyph(status: AgentStatus) -> &'static str {
    match status {
        // `?` needs your answer; `!` needs a look (a failed turn or a wedged
        // agent); `⏸` is parked on a spent account. The three attention-class
        // states — the first two actionable, the last a non-actionable wait.
        AgentStatus::Waiting => "?",
        AgentStatus::Failed => "!",
        AgentStatus::Running => WORKING_FRAMES[3],
        AgentStatus::Idle => "○",
        AgentStatus::Success => "✓",
        AgentStatus::RateLimited => RATE_LIMITED_GLYPH,
    }
}

/// Rate-limited: a media `pause` mark carrying the text-presentation selector
/// (`U+FE0E`) so it renders as a single-cell monochrome glyph, never a
/// double-width color emoji that would shift the cockpit columns after it. The
/// account's budget is spent, so the agent is parked until the window resets —
/// auto-resumable with a `continue`, nothing to do but wait.
const RATE_LIMITED_GLYPH: &str = "⏸\u{FE0E}";

/// Working: a braille spinner cycling its dots. Spans the most time of any
/// state, so it is the steady motion the eye learns to ignore until something
/// changes. No frame matches idle `○`, so a frozen frame still reads as "working".
const WORKING_FRAMES: [&str; 8] = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];

/// Thinking: a sparkle that grows and fades. The opening phase of every turn —
/// the agent is reasoning and reading, not yet writing — so its motion reads as
/// lighter than the working fill. The turn's first file edit flips the cell to
/// the working spinner.
const THINKING_FRAMES: [&str; 8] = ["·", "✢", "✳", "✶", "✻", "✽", "✻", "✶"];

/// Resolver answering: a braille spinner while a resolver composes the answer on
/// the bridge. This is the one "waiting for an answer" motion — it is genuinely
/// active and time-bounded by the resolver budget, unlike a human-blocked `?`,
/// which stays still.
const RESOLVER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Compacting: a single bar pulsing taller then shorter, like a compression
/// meter squeezing the context window down. Short-lived — the next lifecycle
/// event returns the agent to its resting head — so it never earns a cockpit
/// bucket; it paints in cool violet (the token/context-domain color) to read as
/// housekeeping, not the clay working fill.
const COMPACTING_FRAMES: [&str; 10] = ["▁", "▃", "▄", "▅", "▆", "▇", "▆", "▅", "▄", "▃"];

/// Waiting on subagents: a low tick bobbing up off the baseline and back — a
/// quiet wave that reads as "the work is in the children below", distinct from
/// the dense working braille. Stays in the agent's clay: the parent is still its
/// live head, just delegating.
const SUBAGENT_FRAMES: [&str; 8] = ["_", "-", "`", "´", "'", "´", "`", "-"];

/// Idle, waiting-for-a-prompt: a quiet `.` → `..` → `...` loading cue that stands
/// in for the em-dash on a just-started agent with nothing to describe yet. Held
/// several ticks per step so it breathes rather than flickers.
const LOADING_FRAMES: [&str; 3] = [".", "..", "..."];

fn frame(frames: &[&'static str], animation_phase: u64) -> &'static str {
    frames[(animation_phase as usize) % frames.len()]
}

/// The idle loading-dots cue (`.` / `..` / `...`), each step held eight ticks
/// (~800ms) — a 2.4s full cycle, the same lazy cadence as the attention breath —
/// so an idle row drifts rather than strobes.
pub(super) fn loading_dots(animation_phase: u64) -> &'static str {
    LOADING_FRAMES[((animation_phase / 8) as usize) % LOADING_FRAMES.len()]
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
pub(super) fn attention_breath(animation_phase: u64, age_secs: i64) -> Modifier {
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
pub(super) fn elapsed_glyph(secs: i64) -> &'static str {
    match secs {
        i64::MIN..=900 => "◔",
        901..=1800 => "◑",
        1801..=2700 => "◕",
        2701..=3600 => "●",
        _ => "◉",
    }
}

/// The shared age heat: one tone ramp for every idle-age reader — the clock
/// cluster, the breathing `?`/`!`, and the cockpit attention buckets — stepping
/// with the quarter-hour buckets that fill the clock face ([`elapsed_glyph`]).
/// `None` through the first quarter (callers pick the resting tone), yellow to
/// the half hour, amber beyond it, red past the hour — when resuming would
/// almost certainly re-read the whole context at uncached input rates.
pub(super) fn age_heat(age_secs: i64) -> Option<Color> {
    match age_secs {
        i64::MIN..=900 => None,
        901..=1800 => Some(Color::Yellow),
        1801..=3600 => Some(ORANGE),
        _ => Some(Color::Red),
    }
}

/// Tone for the card's elapsed-age cluster at `age_secs` of inactivity: the
/// shared [`age_heat`] over a dim resting tone, so a fresh age stays chrome
/// and a red one reads as the cost warning it is. The figure itself still
/// carries the magnitude under `NO_COLOR`.
pub(super) fn activity_age_style(theme: &Theme, age_secs: i64) -> Style {
    age_heat(age_secs).map_or(theme.dim(), |color| theme.style(color, Modifier::empty()))
}

pub(super) fn working_glyph(animation_phase: u64) -> &'static str {
    frame(&WORKING_FRAMES, animation_phase)
}

pub(super) fn thinking_glyph(animation_phase: u64) -> &'static str {
    frame(&THINKING_FRAMES, animation_phase)
}

pub(super) fn resolver_glyph(animation_phase: u64) -> &'static str {
    frame(&RESOLVER_FRAMES, animation_phase)
}

pub(super) fn compacting_glyph(animation_phase: u64) -> &'static str {
    frame(&COMPACTING_FRAMES, animation_phase)
}

pub(super) fn subagent_glyph(animation_phase: u64) -> &'static str {
    frame(&SUBAGENT_FRAMES, animation_phase)
}

/// The leading cell for an agent row, animated when the agent is actively doing
/// something. A `running` agent sparkles (reasoning, before the turn's first
/// file edit) or fills (acting or parked); every other state is the static
/// [`status_glyph`]. Stall is already folded into `Failed` upstream, so it
/// falls through to the static `!`.
pub(super) fn agent_glyph(
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

pub(super) fn status_style(theme: &Theme, status: AgentStatus) -> Style {
    match status {
        AgentStatus::Waiting => theme.style(Color::Yellow, Modifier::BOLD),
        AgentStatus::Failed => theme.style(Color::Red, Modifier::BOLD),
        AgentStatus::Running => theme.style(Color::Green, Modifier::empty()),
        // Idle and success are the two calm "nothing needs you" states, so both
        // read in a quiet green — the hollow `○` and the `✓` carry the meaning,
        // the dim weight keeps them from competing with live attention.
        AgentStatus::Idle => theme.style(Color::Green, Modifier::DIM),
        AgentStatus::Success => theme.style(Color::Green, Modifier::DIM),
        // Rate-limited stays in the amber attention family but at rest weight
        // (not bold, and `attention_glyph_style` never heats it): it is
        // attention-class, but parked with nothing to do but wait — the held
        // tone sets it apart from the loud, actionable `?`/`!`.
        AgentStatus::RateLimited => theme.style(Color::Yellow, Modifier::empty()),
    }
}

/// The compacting head's tone: cool violet, the token/context-domain color the
/// `◇` token glyph already uses, so a pulsing context-condense reads as
/// housekeeping rather than the clay working fill.
pub(super) fn compacting_style(theme: &Theme) -> Style {
    theme.style(Color::Magenta, Modifier::empty())
}

/// The waiting-on-subagents head's tone: the agent's clay, same as the working
/// fill — the parent is still its live head, just delegating; the quiet wave
/// motion, not the color, carries "the work is in the children".
pub(super) fn subagent_style(theme: &Theme) -> Style {
    theme.style(ORANGE, Modifier::empty())
}

/// Style for an agent row's leading cell. A running agent's working spinner and
/// its thinking sparkle both paint in Claude clay, so the live head aligns with
/// the agent's own UI; every other state takes its [`status_style`].
pub(super) fn agent_style(theme: &Theme, status: AgentStatus) -> Style {
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
pub(super) fn attention_glyph_style(
    theme: &Theme,
    status: AgentStatus,
    age_secs: i64,
    animation_phase: u64,
) -> Style {
    if matches!(status, AgentStatus::Waiting | AgentStatus::Failed) {
        let color = age_heat(age_secs).unwrap_or(Color::Yellow);
        theme.style(color, attention_breath(animation_phase, age_secs))
    } else {
        agent_style(theme, status)
    }
}

/// Token-composition glyphs for the `◇ ↘ ↗ ◍ ◌` breakdown: a diamond for the
/// cumulative total (input + output), the directional arrows for input read in /
/// output generated, a half-filled ring for cache writes, and a hollow ring for
/// cache reads. The breakdown reads the same on the cockpit, the provider
/// dashboard, and the W/M ledger rows — one grammar, built by
/// [`token_breakdown_spans`], each marker in its one color everywhere (the
/// `◇` violet, the rest their [`SEGMENT_INPUT`]-family segment tones). The
/// agent card's stat line answers a different question — what is in the
/// window, not what the fleet burned — so it leads with `▤` and reorders the
/// same four columns by how the window filled ([`context_breakdown_spans`]).
pub(super) const TOKENS_TOTAL: &str = "◇";
pub(super) const TOKENS_IN: &str = "↘";
pub(super) const TOKENS_OUT: &str = "↗";
pub(super) const TOKENS_CACHE_WRITE: &str = "◍";
pub(super) const TOKENS_CACHED: &str = "◌";
/// The agent card's context-line marker: a filled square for the taken part of
/// the context window, sibling to the `▣` meter glyph so the two context reads
/// pair visually while staying distinct from the `◇` fleet totals.
pub(super) const CONTEXT_FILLED: &str = "▤";

/// The context-window composition colors — one tone per segment, shared by the
/// bar's colored runs and the context line's `◌`/`◍`/`↘` markers so the line
/// reads as the bar's legend by construction. `↗` output is not in the window
/// (it joins next turn), so it carries no bar segment; its green is free
/// because the meter's calm tier reads blue.
pub(super) const SEGMENT_CACHE_READ: Color = Color::Blue;
pub(super) const SEGMENT_CACHE_WRITE: Color = Color::Yellow;
pub(super) const SEGMENT_INPUT: Color = Color::Red;
pub(super) const SEGMENT_OUTPUT: Color = Color::Green;

/// The `◇ ↘ ↗ ◍ ◌` token breakdown as styled spans — the one shape every fleet
/// token line shares (cockpit today line, provider today line, W/M ledger
/// rows). Each marker wears its one color everywhere: the `◇` total its
/// soft-violet, the rest the same bar-segment tones the card's context line
/// legends ([`SEGMENT_INPUT`] and siblings, `↗` output in the segment green) —
/// one glyph, one color, across the whole sidebar. The figures read at full
/// strength ([`Theme::value`]); under `NO_COLOR` the glyph shapes still spell
/// the split. `fmt` chooses the magnitude form (`tokens_int` live,
/// `tokens_short` for the precise W/M rows); `include_cache_write` drops the
/// `◍` field for the W/M rows, which omit it. `total` is the caller's `◇` value
/// (input + output), passed in so a row can read it straight from its
/// accumulated window.
#[allow(clippy::too_many_arguments)]
pub(super) fn token_breakdown_spans(
    theme: &Theme,
    total: u64,
    input: u64,
    output: u64,
    cache_write: u64,
    cache_read: u64,
    fmt: fn(u64) -> String,
    include_cache_write: bool,
) -> Vec<Span<'static>> {
    let mut spans = tokens_total_spans(theme, total, fmt, theme.value());
    let mut field = |glyph: &str, color: Color, value: u64| {
        spans.push(Span::styled(
            format!(" {glyph} "),
            theme.style(color, Modifier::empty()),
        ));
        spans.push(Span::styled(fmt(value), theme.value()));
    };
    field(TOKENS_IN, SEGMENT_INPUT, input);
    field(TOKENS_OUT, SEGMENT_OUTPUT, output);
    if include_cache_write {
        field(TOKENS_CACHE_WRITE, SEGMENT_CACHE_WRITE, cache_write);
    }
    field(TOKENS_CACHED, SEGMENT_CACHE_READ, cache_read);
    spans
}

/// The agent card's `▤ · ◌ ◍ ↘ ↗` context line as styled spans: the filled part
/// of the window (`input + cache_write + cache_read` — exactly the `▣` meter's
/// numerator, so the meter's percent and this absolute figure are one
/// measurement), a `·` seam, then the latest API call's composition ordered by
/// how the window filled — `◌` read back from cache, `◍` newly written to it,
/// `↘` fresh input, `↗` output generated (which joins the window next turn).
/// The `▤` head wears the bar's `severity` tone and each composition marker its
/// bar-segment color ([`SEGMENT_CACHE_READ`] and siblings), so the line is the
/// bar's color-keyed legend; the figures stay dim chrome.
#[allow(clippy::too_many_arguments)]
pub(super) fn context_breakdown_spans(
    theme: &Theme,
    severity: Color,
    filled: u64,
    cache_read: u64,
    cache_write: u64,
    input: u64,
    output: u64,
    fmt: fn(u64) -> String,
) -> Vec<Span<'static>> {
    let mut spans = context_total_spans(theme, severity, filled, fmt);
    for (seam, glyph, color, value) in [
        (" · ", TOKENS_CACHED, SEGMENT_CACHE_READ, cache_read),
        (" ", TOKENS_CACHE_WRITE, SEGMENT_CACHE_WRITE, cache_write),
        (" ", TOKENS_IN, SEGMENT_INPUT, input),
        (" ", TOKENS_OUT, SEGMENT_OUTPUT, output),
    ] {
        spans.push(Span::styled(seam, theme.dim()));
        spans.push(Span::styled(glyph, theme.style(color, Modifier::empty())));
        spans.push(Span::styled(format!(" {}", fmt(value)), theme.dim()));
    }
    spans
}

/// The `▤ {filled}` head of the card's context line: the filled-square marker +
/// the filled-window figure. The marker wears the caller's `severity` — the
/// same [`context_severity_color`] the bar and the `▣` glyph paint — so the
/// absolute figure and the meter above it read as one measurement at one
/// urgency. A card whose context carries no per-call split (a Codex
/// rollout-only total, or Claude before its first API call) uses it alone, with
/// the provider's rollup total standing in for the filled window. Display-only,
/// never a decision driver.
pub(super) fn context_total_spans(
    theme: &Theme,
    severity: Color,
    filled: u64,
    fmt: fn(u64) -> String,
) -> Vec<Span<'static>> {
    vec![
        Span::styled(CONTEXT_FILLED, theme.style(severity, Modifier::empty())),
        Span::styled(format!(" {}", fmt(filled)), theme.dim()),
    ]
}

/// Heavy `━` for the thin context/rule bars' filled run, light `─` for the
/// remaining track. The weight difference — not just the color — carries the
/// meter, so it reads with color off.
const BAR_FILLED: char = '━';
const BAR_TRACK: char = '─';

/// Segmented `▰` / `▱` for the provider dashboard's draining "mana / stamina"
/// bars: a thin, ticked energy gauge that reads lighter than a solid `█` block
/// while still distinct from the `━`/`─` context rule. The fill/hollow shape
/// carries the meter, so it survives `NO_COLOR`.
const MANA_FILLED: char = '▰';
const MANA_TRACK: char = '▱';

/// Filled-cell count for `percent` of `width`, to the nearest whole cell: 0%
/// stays an unbroken track, 100% fills the whole width.
fn filled_cells(percent: u8, width: usize) -> usize {
    ((percent.min(100) as usize) * width.max(1) + 50) / 100
}

/// A two-tone bar: `filled` cells of `filled_glyph` in `color`, then a faint
/// `track_glyph` out to `width`. The shared shape behind every meter — the thin
/// `━`/`─` context gauge and the heavy `█`/`░` dashboard bars — so they read as
/// one family differing only in weight. Color and fill differ per meter; the
/// shape does not.
fn two_tone_bar(
    theme: &Theme,
    filled: usize,
    width: usize,
    color: Color,
    filled_glyph: char,
    track_glyph: char,
) -> Vec<Span<'static>> {
    let width = width.max(1);
    let filled = filled.min(width);
    let mut spans = Vec::with_capacity(2);
    if filled > 0 {
        spans.push(Span::styled(
            std::iter::repeat_n(filled_glyph, filled).collect::<String>(),
            theme.style(color, Modifier::empty()),
        ));
    }
    if filled < width {
        spans.push(Span::styled(
            std::iter::repeat_n(track_glyph, width - filled).collect::<String>(),
            theme.faint(),
        ));
    }
    spans
}

/// The context meter's **severity** tone — one calm-blue → yellow → amber →
/// red ramp shared by the bar, the `▣` glyph, and the `▤` context-line head,
/// so every context read on the card speaks one urgency. Calm reads **blue**
/// ("cold — plenty of headroom"), keeping the meter clear of the green running
/// vocabulary and of the composition segments. For the bar it also decides
/// composition-vs-solid: the segments (where the window went) paint only while
/// calm; once the meter warms the bar goes one solid severity run.
pub(super) fn context_severity_color(
    percent: u8,
    used_tokens: Option<u64>,
    bands: &ContextSeverityConfig,
) -> Color {
    match severity_tier(percent, used_tokens, bands) {
        0 => Color::Blue,
        1 => Color::Yellow,
        2 => ORANGE,
        _ => Color::Red,
    }
}

/// The shared usage tier (0 calm / 1 yellow / 2 amber / 3 red) behind every
/// context tone: the worse of the fill-percentage ramp and the absolute-token
/// overlay, each tier entered at its configured inclusive lower bound
/// (`[sidebar.context]`, [`ContextSeverityConfig`]), so a large-window model
/// calm by percentage still climbs by sheer volume. Checked worst-first, so a
/// misordered user config degrades to the highest matching tier.
fn severity_tier(percent: u8, used_tokens: Option<u64>, bands: &ContextSeverityConfig) -> u8 {
    let percent = percent.min(100);
    let tokens = used_tokens.unwrap_or(0);
    let reaches = |band: &rimz::config::ContextBand| -> bool {
        percent >= band.percent || tokens >= band.tokens
    };
    if reaches(&bands.red) {
        3
    } else if reaches(&bands.amber) {
        2
    } else if reaches(&bands.yellow) {
        1
    } else {
        0
    }
}

/// The window token's tone: still dim — a capability label, not a status
/// signal; the context-meter severity ramp owns the loud color slot — but
/// tinted by size class so the magnitude reads at a glance: clay amber for a
/// 1m+ window, gold for the 258k tier, sky blue for 128k, and the plain dim
/// gray below that. The `DIM` modifier rides every band, so the token never
/// outshines the meter; under `NO_COLOR` all bands collapse to the same dim.
pub(super) fn window_style(theme: &Theme, window: u64) -> Style {
    let color = match window {
        1_000_000.. => ORANGE,
        258_000.. => Color::Yellow,
        128_000.. => Color::Blue,
        _ => Color::DarkGray,
    };
    theme.style(color, Modifier::DIM)
}

/// Context bar: a thin rule whose filled run grows left-to-right as the window
/// fills, painted in `color` (the caller's [`context_severity_color`]). The label
/// and value columns live in the renderer's shared bar row; here we paint just
/// the meter.
pub(super) fn gauge_spans(
    theme: &Theme,
    color: Color,
    percent: u8,
    width: usize,
) -> Vec<Span<'static>> {
    two_tone_bar(
        theme,
        filled_cells(percent, width),
        width,
        color,
        BAR_FILLED,
        BAR_TRACK,
    )
}

/// Like [`gauge_spans`], but the filled run is split into colored segments by
/// token weight — showing *where* the context window went (fresh input vs cache
/// writes vs cache reads). `total_pct` sizes the filled run exactly as the
/// single-color gauge would; the segments apportion that run by their weights
/// with largest-remainder rounding, so the colored cells always sum to the
/// filled count and the bar never over- or under-fills. With no breakdown to
/// draw it falls back to the plain gauge. Under `NO_COLOR` the segments merge
/// into one heavy run — the split is a color enrichment; the fill level still
/// reads by shape.
pub(super) fn segmented_gauge_spans(
    theme: &Theme,
    segments: &[(u64, Color)],
    fallback_color: Color,
    total_pct: u8,
    width: usize,
) -> Vec<Span<'static>> {
    let width = width.max(1);
    let total_pct = total_pct.min(100);
    let filled = filled_cells(total_pct, width);
    let weight: u64 = segments.iter().map(|(value, _)| *value).sum();
    if filled == 0 || weight == 0 {
        return gauge_spans(theme, fallback_color, total_pct, width);
    }
    let cells = apportion(segments.iter().map(|(value, _)| *value), filled);
    let mut spans = Vec::with_capacity(segments.len() + 1);
    for ((_, color), count) in segments.iter().zip(cells) {
        if count > 0 {
            spans.push(Span::styled(
                std::iter::repeat_n(BAR_FILLED, count).collect::<String>(),
                theme.style(*color, Modifier::empty()),
            ));
        }
    }
    if filled < width {
        spans.push(Span::styled(
            std::iter::repeat_n(BAR_TRACK, width - filled).collect::<String>(),
            theme.faint(),
        ));
    }
    spans
}

/// Distribute `total` whole cells across `weights` by the largest-remainder
/// method: floor each share, then hand the leftover cells to the largest
/// fractional remainders. The result always sums to `total`, so a segmented bar
/// fills exactly its run with no rounding drift.
fn apportion(weights: impl IntoIterator<Item = u64>, total: usize) -> Vec<usize> {
    let weights: Vec<u64> = weights.into_iter().collect();
    let sum: u128 = weights.iter().map(|w| u128::from(*w)).sum();
    if sum == 0 {
        return vec![0; weights.len()];
    }
    let mut cells = Vec::with_capacity(weights.len());
    let mut remainders = Vec::with_capacity(weights.len());
    let mut assigned = 0usize;
    for (index, weight) in weights.iter().enumerate() {
        let exact = u128::from(*weight) * total as u128;
        let floor = (exact / sum) as usize;
        cells.push(floor);
        remainders.push((index, exact % sum));
        assigned += floor;
    }
    // Largest remainder first; the stable sort keeps a tie in index order, so
    // the leftover cell lands on the earliest (leftmost) segment.
    remainders.sort_by_key(|&(_, remainder)| std::cmp::Reverse(remainder));
    for (index, _) in remainders.into_iter().take(total.saturating_sub(assigned)) {
        cells[index] += 1;
    }
    cells
}

/// The provider dashboard's draining budget ("mana / stamina") bar:
/// `remaining_pct` of the width in `▰`, the rest a `▱` track, with no brackets.
/// A full bar means budget *left*: it shortens as the window is spent, and the
/// reset countdown beside it says when it refills. Ramps green → yellow → red by
/// how much remains, so a near-spent window reddens regardless of which window it
/// is. At 0% remaining — the budget fully spent — the whole empty track turns
/// red; any nonzero remaining budget keeps at least one filled cell.
pub(super) fn mana_bar_spans(theme: &Theme, remaining_pct: u8, width: usize) -> Vec<Span<'static>> {
    // A fully spent window (0% remaining) reads as a full-width *red* empty track,
    // not the faint "no fill" track a plain drain leaves — `two_tone_bar` always
    // paints the track faint, so an absent fill alone would read as the same calm
    // gray as a barely-touched window. Alarm the track itself so "used up" is
    // unmistakable; it stays the empty `▱` glyph (no fill), only its tone changes.
    if remaining_pct == 0 {
        return vec![Span::styled(
            std::iter::repeat_n(MANA_TRACK, width.max(1)).collect::<String>(),
            theme.style(Color::Red, Modifier::empty()),
        )];
    }
    let filled = filled_cells(remaining_pct, width).max(1);
    two_tone_bar(
        theme,
        filled,
        width,
        mana_color(remaining_pct),
        MANA_FILLED,
        MANA_TRACK,
    )
}

/// The severity color for a mana bar at `remaining_pct` budget left: red when
/// near-spent (or fully spent), amber mid-drain, sage green with plenty left.
/// Shared by the bar fill and the `5h`/`7d` label beside it so the label mirrors
/// its bar's tone.
pub(super) fn mana_color(remaining_pct: u8) -> Color {
    match remaining_pct.min(100) {
        0..=20 => Color::Red,
        21..=50 => Color::Yellow,
        _ => Color::Green,
    }
}

/// The unmetered ("infinite") bar: a full-width empty `▱` track aligned with
/// the metered `5h`/`7d` bars, reading as "no meter to spend." The brand
/// `color` rides the `∞` icon *and* the track, so the two read as one branded
/// unmetered bar; the empty `▱` shape keeps it from competing with a real
/// draining fill, and under `NO_COLOR` the unbroken run still reads as an
/// empty track by shape.
pub(super) fn infinite_bar_spans(theme: &Theme, color: u8, width: usize) -> Vec<Span<'static>> {
    vec![Span::styled(
        std::iter::repeat_n(MANA_TRACK, width.max(1)).collect::<String>(),
        theme.style(Color::Indexed(color), Modifier::empty()),
    )]
}

/// Todo progress: filled dots for done, hollow dots for remaining, with the
/// numeric ratio appended. The shape carries it; color stays dim chrome.
pub(super) fn todo_spans(theme: &Theme, done: u32, total: u32) -> Vec<Span<'static>> {
    let total = total.max(done);
    let cap = 5_u32;
    let scaled_total = total.min(cap);
    let scaled_done = if total <= cap {
        done
    } else {
        // Scale down proportionally so the dot count stays bounded.
        ((done as u64) * cap as u64 / total.max(1) as u64) as u32
    };
    let dots: String = std::iter::repeat_n('●', scaled_done as usize)
        .chain(std::iter::repeat_n(
            '○',
            scaled_total.saturating_sub(scaled_done) as usize,
        ))
        .collect();
    vec![
        Span::styled(dots, theme.dim()),
        Span::styled(format!(" {done}/{total}"), theme.dim()),
    ]
}

/// The `◇ {total}` marker: the soft-violet diamond + the formatted cumulative
/// total. The shared head of every token line — [`token_breakdown_spans`] builds
/// on it, and a breakdown-less line (a Codex rollup-only total) uses it alone.
/// `fmt` picks the magnitude form ([`tokens_int`](super::fmt::tokens_int) live,
/// `tokens_short` for the precise W/M rows). The diamond is a colored marker;
/// `value_style` sets the figure's weight — full-strength ([`Theme::value`]) on
/// the fleet lines, dim on a subordinate card line (a subagent's spend).
/// Display-only, never a decision driver.
pub(super) fn tokens_total_spans(
    theme: &Theme,
    total: u64,
    fmt: fn(u64) -> String,
    value_style: Style,
) -> Vec<Span<'static>> {
    vec![
        Span::styled(TOKENS_TOTAL, theme.style(Color::Magenta, Modifier::empty())),
        Span::styled(format!(" {}", fmt(total)), value_style),
    ]
}

/// `⇡3 ⇣1`-style commit delta against the trunk: ahead then behind, zero
/// components omitted. Both dim cyan — commit-level branch facts rhyme with
/// the bold-cyan worktree name and stay a category apart from the green/red
/// line-level churn; the `⇡`/`⇣` shape carries the direction under `NO_COLOR`.
pub(super) fn branch_delta_spans(theme: &Theme, ahead: u32, behind: u32) -> Vec<Span<'static>> {
    let style = theme.style(Color::Cyan, Modifier::DIM);
    let mut spans = Vec::new();
    if ahead > 0 {
        spans.push(Span::styled(format!("⇡{ahead}"), style));
    }
    if behind > 0 {
        if !spans.is_empty() {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(format!("⇣{behind}"), style));
    }
    spans
}

/// `≡ main` — the worktree is fully landed on the trunk: zero commits ahead
/// and a zero diff against the fork point, so it is safe to remove. Dim green,
/// the calm-positive tone an idle/done agent wears — quiet enough to stay
/// chrome yet scannable when hunting removable worktrees; the `≡` shape
/// carries the verdict under `NO_COLOR`. The trunk worktree itself never wears
/// it — the caller gates on the group's live branch.
pub(super) fn trunk_equal_spans(theme: &Theme, trunk: &str) -> Vec<Span<'static>> {
    vec![Span::styled(
        format!("≡ {trunk}"),
        theme.style(Color::Green, Modifier::DIM),
    )]
}

/// `+127 -43`-style diff stat. Added in green, removed in red, both dim to
/// stay chrome — the gauge ramp owns the loud color slots.
pub(super) fn diff_spans(theme: &Theme, added: u32, removed: u32) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            format!("+{added}"),
            theme.style(Color::Green, Modifier::DIM),
        ),
        Span::raw(" "),
        Span::styled(
            format!("-{removed}"),
            theme.style(Color::Red, Modifier::DIM),
        ),
    ]
}

#[cfg(test)]
mod tests;
