use super::*;
use crate::config::ContextSeverityConfig;
use crate::sidebar_pane::render::theme::Component;
use jiff::SignedDuration;

/// Token-composition glyphs for the `◇ ↘ ↗ ◌` fleet breakdown: a diamond for
/// the cumulative total (input with cache-write folded in, plus output), the
/// directional arrows for input read in / output generated, and a hollow ring
/// for cache reads. The breakdown reads the same on the cockpit, the provider
/// dashboard, and the W/M ledger rows — one grammar, built by
/// [`token_breakdown_spans`], each marker in its one color everywhere. The `◍`
/// marker belongs to the agent card's context-composition line, which answers
/// a different question — what is in the window, not what the fleet burned —
/// so it leads with `▤` and reorders the same four columns by how the window
/// filled ([`context_breakdown_spans`]).
pub(in crate::sidebar_pane::render) const TOKENS_TOTAL: &str = "◇";
pub(in crate::sidebar_pane::render) const TOKENS_IN: &str = "↘";
pub(in crate::sidebar_pane::render) const TOKENS_OUT: &str = "↗";
pub(in crate::sidebar_pane::render) const TOKENS_CACHE_WRITE: &str = "◍";
pub(in crate::sidebar_pane::render) const TOKENS_CACHED: &str = "◌";
/// The agent card's context-line marker: a filled square for the taken part of
/// the context window, sibling to the `▣` meter glyph so the two context reads
/// pair visually while staying distinct from the `◇` fleet totals.
pub(in crate::sidebar_pane::render) const CONTEXT_FILLED: &str = "▤";
/// The agent card's compaction marker: a recycle arrow for how many times the
/// session has condensed its window. Yellow, distinct from cache-write violet.
pub(in crate::sidebar_pane::render) const CONTEXT_COMPACTIONS: &str = "↻";

/// The `◇ ↘ ↗ ◌` token breakdown as styled spans — the one shape every fleet
/// token line shares (cockpit today line, provider today line, W/M ledger
/// rows). Each marker wears its one color everywhere: the `◇` total in blue,
/// `↘` input in context-alarm red, `↗` output in green, and `◌` cache-read in
/// teal. The figures read at the soft tier
/// ([`Theme::soft`]) like every stat figure; under `NO_COLOR` the glyph shapes
/// still spell the split. `fmt` chooses the magnitude form (`tokens_int` live,
/// `tokens_short` for the precise W/M rows). `total` is the caller's `◇` value
/// (`input` with cache-write folded in, plus output), passed in so a row can
/// read it straight from its accumulated window.
pub(in crate::sidebar_pane::render) fn token_breakdown_spans(
    theme: &Theme,
    total: u64,
    input: u64,
    output: u64,
    cache_read: u64,
    fmt: fn(u64) -> String,
) -> Vec<Span<'static>> {
    let mut spans = tokens_total_spans(theme, total, fmt);
    let mut field = |glyph: &str, component: Component, value: u64| {
        spans.push(Span::styled(
            format!(" {glyph} "),
            theme.styled(component, Modifier::empty()),
        ));
        spans.push(Span::styled(fmt(value), theme.body()));
    };
    field(TOKENS_IN, Component::Input, input);
    field(TOKENS_OUT, Component::Output, output);
    field(TOKENS_CACHED, Component::CacheRead, cache_read);
    spans
}

/// The agent card's `▤ · ◌ ◍ ↘ ↗` context line as styled spans: the filled part
/// of the window (`input + cache_write + cache_read` — exactly the `▣` meter's
/// numerator, so the meter's percent and this absolute figure are one
/// measurement), a `·` seam, then the latest API call's composition ordered by
/// how the window filled — `◌` read back from cache, `◍` newly written to it,
/// `↘` fresh input, `↗` output generated (which joins the window next turn).
/// A zero column drops whole — the line shows what filled the window, and a
/// provider with no per-call cache-write (Codex) simply never grows a `◍`.
/// The `▤` head wears the bar's `severity` tone and each composition marker its
/// bar-segment color, so the line is the bar's color-keyed legend; the figures
/// read at the dim chrome weight — a step under the name line's soft tokens, so
/// the colored markers carry the line.
#[allow(clippy::too_many_arguments)]
pub(in crate::sidebar_pane::render) fn context_breakdown_spans(
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
    // The `·` seam frames the first *rendered* column, wherever it lands.
    let mut seam = " · ";
    for (glyph, component, value) in [
        (TOKENS_CACHED, Component::CacheRead, cache_read),
        (TOKENS_CACHE_WRITE, Component::CacheWrite, cache_write),
        (TOKENS_IN, Component::Input, input),
        (TOKENS_OUT, Component::Output, output),
    ] {
        if value == 0 {
            continue;
        }
        spans.push(Span::styled(seam, theme.muted()));
        spans.push(Span::styled(
            glyph,
            theme.styled(component, Modifier::empty()),
        ));
        spans.push(Span::styled(format!(" {}", fmt(value)), theme.muted()));
        seam = " ";
    }
    spans
}

/// The `· ↻ N` compaction tail for the context line, shown from the first
/// completed compaction. The `·` seam reads at the dim chrome like the
/// composition seams; only the marker wears the yellow compaction tone, and the
/// count stays dim like the adjacent context figures.
pub(in crate::sidebar_pane::render) fn context_compaction_spans(
    theme: &Theme,
    count: u32,
) -> Vec<Span<'static>> {
    if count == 0 {
        return Vec::new();
    }
    vec![
        Span::styled(" · ", theme.muted()),
        Span::styled(CONTEXT_COMPACTIONS, compacting_style(theme)),
        Span::styled(format!(" {count}"), theme.muted()),
    ]
}

/// The `▤ {filled}` head of the card's context line: the filled-square marker +
/// the filled-window figure. The marker wears the caller's `severity` — the
/// same [`severity_color`] tone the bar and the `▣` glyph paint — so the
/// absolute figure and the meter above it read as one measurement at one
/// urgency. A card whose context carries no per-call split (a Codex
/// rollout-only total, or Claude before its first API call) uses it alone, with
/// the provider's rollup total standing in for the filled window. Display-only,
/// never a decision driver.
pub(in crate::sidebar_pane::render) fn context_total_spans(
    theme: &Theme,
    severity: Color,
    filled: u64,
    fmt: fn(u64) -> String,
) -> Vec<Span<'static>> {
    vec![
        Span::styled(CONTEXT_FILLED, theme.style(severity, Modifier::empty())),
        Span::styled(format!(" {}", fmt(filled)), theme.muted()),
    ]
}

/// Heavy `━` for the thin context/rule bars' filled run, light `─` for the
/// remaining track. The weight difference — not just the color — carries the
/// meter, so it reads with color off.
const BAR_FILLED: char = '━';
const BAR_TRACK: char = '─';
const BAR_SEGMENT_CAP: char = '╸';
// Context bars spend one cell on a half-rule cap between composition segments,
// giving the split a narrow visible notch while the bar still ends exactly at
// its fill level.

/// Segmented `▰` / `▱` for the provider dashboard's draining "mana / stamina"
/// bars: a thin, ticked energy gauge that reads lighter than a solid `█` block
/// while still distinct from the `━`/`─` context rule. The fill/hollow shape
/// carries the meter, so it survives `NO_COLOR`.
const MANA_FILLED: char = '▰';
const MANA_TRACK: char = '▱';

/// The agent-cards viewport scrollbar, ridden on the right rail column when the
/// cards overflow: a solid `▐` thumb over a hairline `▕` track. The solid/thin
/// shape difference carries the position, so it survives `NO_COLOR`.
pub(in crate::sidebar_pane::render) const SCROLL_THUMB: &str = "▐";
pub(in crate::sidebar_pane::render) const SCROLL_TRACK: &str = "▕";

/// Filled-cell count for `percent` of `width`, to the nearest whole cell: 0%
/// stays an unbroken track, 100% fills the whole width.
fn filled_cells(percent: u8, width: usize) -> usize {
    ((percent.min(100) as usize) * width.max(1) + 50) / 100
}

/// A two-tone bar: `filled` cells of `filled_glyph` in `filled_style`, then
/// `track_glyph` in `track_style` out to `width`. The shared shape behind every
/// meter — the thin `━`/`─` context gauge and the segmented `▰`/`▱` mana bars —
/// so they read as one family differing only in weight. Styles and fill differ
/// per meter; the shape does not.
fn two_tone_bar(
    filled: usize,
    width: usize,
    filled_style: Style,
    track_style: Style,
    filled_glyph: char,
    track_glyph: char,
) -> Vec<Span<'static>> {
    let width = width.max(1);
    let filled = filled.min(width);
    let mut spans = Vec::with_capacity(2);
    if filled > 0 {
        spans.push(Span::styled(
            std::iter::repeat_n(filled_glyph, filled).collect::<String>(),
            filled_style,
        ));
    }
    if filled < width {
        spans.push(Span::styled(
            std::iter::repeat_n(track_glyph, width - filled).collect::<String>(),
            track_style,
        ));
    }
    spans
}

/// The context meter's heat amount for a concrete row, `0.0` (healthy green) →
/// `1.0` (alarm red) along the [`Theme::heat_tone`] ramp. Severity remains the
/// domain verdict, while the renderer uses the row's position inside the
/// configured stops to choose the amount, taking the worse of percent and token
/// axes just like [`ContextSeverity::classify`]. A missing or stale axis falls
/// back to the tier anchor so the tone never understates a stamped severity:
/// green starts warming toward yellow, then yellow toward amber, then amber
/// toward red. This amount also drives the bar's filled health tone, so glyph,
/// bar, and `▤` line read one urgency.
pub(in crate::sidebar_pane::render) fn severity_heat_amount(
    severity: ContextSeverity,
    percent: u8,
    used_tokens: Option<u64>,
    bands: &ContextSeverityConfig,
) -> f32 {
    let anchor = severity_heat_anchor(severity);
    context_heat_amount(percent, used_tokens, bands).map_or(anchor, |amount| amount.max(anchor))
}

/// The [`severity_heat_amount`] resolved through the ramp — the row's severity
/// tone for the `▣` glyph and `▤` line, matching the bar's filled run.
pub(in crate::sidebar_pane::render) fn severity_heat_color(
    theme: &Theme,
    severity: ContextSeverity,
    percent: u8,
    used_tokens: Option<u64>,
    bands: &ContextSeverityConfig,
) -> Color {
    theme.heat_tone(severity_heat_amount(severity, percent, used_tokens, bands))
}

fn severity_heat_anchor(severity: ContextSeverity) -> f32 {
    match severity {
        ContextSeverity::Calm | ContextSeverity::Yellow => 0.0,
        ContextSeverity::Amber => 2.0 / 3.0,
        ContextSeverity::Red => 1.0,
    }
}

fn context_heat_amount(
    percent: u8,
    used_tokens: Option<u64>,
    bands: &ContextSeverityConfig,
) -> Option<f32> {
    let pct_amount = axis_heat_amount(
        u64::from(percent.min(100)),
        u64::from(bands.green.percent),
        u64::from(bands.yellow.percent),
        u64::from(bands.amber.percent),
        u64::from(bands.red.percent),
    );
    let token_amount = used_tokens.and_then(|tokens| {
        axis_heat_amount(
            tokens,
            bands.green.tokens,
            bands.yellow.tokens,
            bands.amber.tokens,
            bands.red.tokens,
        )
    });
    match (pct_amount, token_amount) {
        (Some(percent), Some(tokens)) => Some(percent.max(tokens)),
        (Some(percent), None) => Some(percent),
        (None, Some(tokens)) => Some(tokens),
        (None, None) => None,
    }
}

fn axis_heat_amount(value: u64, green: u64, yellow: u64, amber: u64, red: u64) -> Option<f32> {
    if value >= red {
        Some(1.0)
    } else if value >= amber {
        Some(interpolate_heat(value, amber, red, 2.0 / 3.0, 1.0))
    } else if value >= yellow {
        Some(interpolate_heat(value, yellow, amber, 1.0 / 3.0, 2.0 / 3.0))
    } else if value >= green {
        Some(interpolate_heat(value, green, yellow, 0.0, 1.0 / 3.0))
    } else {
        None
    }
}

fn interpolate_heat(value: u64, start: u64, end: u64, low: f32, high: f32) -> f32 {
    if end <= start {
        return high;
    }
    let position = (value - start) as f32 / (end - start) as f32;
    low + (high - low) * position.clamp(0.0, 1.0)
}

/// The window token's tone: subordinate chrome — a capability label, not a
/// status signal; the context-meter severity ramp owns the loud color slot — so
/// the magnitude reads at a glance through a neutral→cool→accent *salience*
/// ramp by size class: faint below 128k, the muted body gray at 128k+, sky blue
/// (cool) at 258k+, and the loud accent at 1m+. The ramp borrows no provider
/// identity, so a big window never reads as a brand. Every band rides the `DIM`
/// modifier so the token stays level with the model/effort tokens beside it and
/// never outshines the meter; under `NO_COLOR` every band collapses to the same
/// bare DIM weight.
pub(in crate::sidebar_pane::render) fn window_style(theme: &Theme, window: u64) -> Style {
    let component = match window {
        1_000_000.. => Component::WindowHuge,
        258_000.. => Component::WindowLarge,
        128_000.. => Component::WindowMedium,
        _ => Component::WindowSmall,
    };
    theme.styled(component, Modifier::DIM)
}

/// The context meter's health bar: the dominant cache-read run paints in the
/// row's current health tone (`theme.heat_tone(amount)`), while the trailing
/// accent segments (cache-write, fresh input) stay flat in their own tones. A
/// half-rule cap carved from the fill separates each segment, so composition
/// stays legible without moving the bar end. `segments[0]` is the cache-read run
/// — its color is ignored because the row health tone owns that span; later
/// segments paint in their own color. `total_pct` sizes the filled run exactly
/// as the plain gauge would. With no split to draw, the whole filled run is one
/// flat health run. Under `NO_COLOR` the health run and accents collapse to one
/// heavy run separated by the same cap notches — the fill level still reads by
/// shape.
pub(in crate::sidebar_pane::render) fn context_gauge_spans(
    theme: &Theme,
    amount: f32,
    segments: &[(u64, Color)],
    total_pct: u8,
    width: usize,
) -> Vec<Span<'static>> {
    let width = width.max(1);
    let filled = filled_cells(total_pct.min(100), width);
    let weight: u64 = segments.iter().map(|(value, _)| *value).sum();
    let track = |drawn: usize| -> Option<Span<'static>> {
        (drawn < width).then(|| {
            Span::styled(
                std::iter::repeat_n(BAR_TRACK, width - drawn).collect::<String>(),
                theme.faint(),
            )
        })
    };
    if filled == 0 || weight == 0 {
        // No split to draw: the whole filled run uses the current health tone.
        let mut spans = filled_run_spans(theme, theme.heat_tone(amount), filled);
        spans.extend(track(filled));
        return spans;
    }
    // Reserve one cell per separator cap between non-empty segments, so the
    // notches come out of the fill and the bar still ends at `filled`.
    let probe = apportion(segments.iter().map(|(value, _)| *value), filled);
    let separators = probe
        .iter()
        .filter(|&&count| count > 0)
        .count()
        .saturating_sub(1);
    let cells = apportion(
        segments.iter().map(|(value, _)| *value),
        filled.saturating_sub(separators),
    );
    let mut spans = Vec::with_capacity(filled + 2);
    let mut drawn = 0usize;
    let non_empty = cells.iter().filter(|&&count| count > 0).count();
    let mut rendered = 0usize;
    for (index, ((_, color), &count)) in segments.iter().zip(&cells).enumerate() {
        if count == 0 {
            continue;
        }
        let cap = rendered + 1 < non_empty;
        if index == 0 {
            spans.extend(filled_run_spans(theme, theme.heat_tone(amount), count));
            if cap {
                spans.push(Span::styled(
                    BAR_SEGMENT_CAP.to_string(),
                    theme.style(theme.heat_tone(amount), Modifier::empty()),
                ));
            }
        } else {
            spans.extend(filled_run_spans(theme, *color, count));
            if cap {
                spans.push(Span::styled(
                    BAR_SEGMENT_CAP.to_string(),
                    theme.style(*color, Modifier::empty()),
                ));
            }
        }
        drawn += count + usize::from(cap);
        rendered += 1;
    }
    spans.extend(track(drawn));
    spans
}

fn filled_run_spans(theme: &Theme, color: Color, count: usize) -> Vec<Span<'static>> {
    if count == 0 {
        return Vec::new();
    }
    vec![Span::styled(
        std::iter::repeat_n(BAR_FILLED, count).collect::<String>(),
        theme.style(color, Modifier::empty()),
    )]
}

/// Distribute `total` whole cells across `weights` by the largest-remainder
/// method: floor each share, then hand the leftover cells to the largest
/// fractional remainders. The result always sums to `total`, so a segmented bar
/// fills exactly its run with no rounding drift.
pub(in crate::sidebar_pane::render) fn apportion(
    weights: impl IntoIterator<Item = u64>,
    total: usize,
) -> Vec<usize> {
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
/// reset countdown beside it says when it refills. Ramps green → gold →
/// clay-amber → red by how much remains on the `[sidebar.budget]` zones
/// ([`mana_style`]), so a near-spent window reddens regardless of which window
/// it is. The drained `▱` run rides the dim chrome — a step up from the faint
/// context-gauge track, so the spent share stays legible on the dashboard. At
/// 0% remaining — the budget fully spent — the whole empty track turns red;
/// any nonzero remaining budget keeps at least one filled cell.
pub(in crate::sidebar_pane::render) fn mana_bar_spans(
    theme: &Theme,
    remaining_pct: u8,
    width: usize,
    zones: &BudgetZonesConfig,
) -> Vec<Span<'static>> {
    // A fully spent window (0% remaining) reads as a full-width *red* empty track,
    // not the gray "no fill" track a plain drain leaves — an absent fill alone
    // would read as the same calm chrome as a barely-touched window. Alarm the
    // track itself so "used up" is unmistakable; it stays the empty `▱` glyph
    // (no fill), only its tone changes.
    if remaining_pct == 0 {
        return vec![Span::styled(
            std::iter::repeat_n(MANA_TRACK, width.max(1)).collect::<String>(),
            theme.alarm(Modifier::empty()),
        )];
    }
    let filled = filled_cells(remaining_pct, width).max(1);
    two_tone_bar(
        filled,
        width,
        mana_style(theme, remaining_pct, zones),
        theme.muted(),
        MANA_FILLED,
        MANA_TRACK,
    )
}

/// An unknown provider budget: the window identity is known (`5h`, `7d`, …) but
/// the account reading is older than the longest reset. Paint a plain dim empty
/// track, distinct from a full green bar and from the fully-spent red track.
pub(in crate::sidebar_pane::render) fn unknown_mana_bar_spans(
    theme: &Theme,
    width: usize,
) -> Vec<Span<'static>> {
    vec![Span::styled(
        std::iter::repeat_n(MANA_TRACK, width.max(1)).collect::<String>(),
        theme.muted(),
    )]
}

/// The mana bar's tone at `remaining_pct` budget left: alarm when near-spent
/// (or fully spent), then the same gold → amber escalation the age and context
/// ramps speak through the shared health slots, resting green while the budget
/// sits above every warning zone. Each `[sidebar.budget]` zone names the
/// exclusive upper bound of remaining budget where its tier applies
/// ([`BudgetZonesConfig`]); checked worst-first, so a misordered user config
/// degrades to the worse tier. Shared by the bar fill and the `5h`/`7d` label
/// beside it so the label mirrors its bar's tone.
pub(in crate::sidebar_pane::render) fn mana_style(
    theme: &Theme,
    remaining_pct: u8,
    zones: &BudgetZonesConfig,
) -> Style {
    if remaining_pct < zones.red {
        theme.alarm(Modifier::empty())
    } else if remaining_pct < zones.amber {
        theme.caution(Modifier::empty())
    } else if remaining_pct < zones.yellow {
        theme.warn(Modifier::empty())
    } else {
        theme.good(Modifier::empty())
    }
}

/// Treat the first five percent of a budget window as already elapsed for pace
/// math. This renderer-only damping keeps a tiny early spend from exploding the
/// reset countdown's color, and is deliberately a tuning constant rather than a
/// user-facing band like `[sidebar.budget.pace]`.
const PACE_ELAPSED_FLOOR: f64 = 0.05;

/// Burn-rate pace for a budget window: used share divided by elapsed share.
/// `1.0` means the current spend rate exactly lasts to reset. The first slice
/// of a fresh window is floored so a tiny amount of usage does not explode the
/// ratio, and overdue reset times clamp to a full elapsed window.
pub(in crate::sidebar_pane::render) fn pace_ratio(
    used_percentage: u8,
    duration: SignedDuration,
    until_reset: SignedDuration,
) -> Option<f64> {
    let duration_secs = duration.as_secs();
    if duration_secs <= 0 {
        return None;
    }
    let elapsed_secs = duration_secs - until_reset.as_secs();
    if elapsed_secs <= 0 {
        return None;
    }
    let elapsed_share = (elapsed_secs as f64 / duration_secs as f64).clamp(PACE_ELAPSED_FLOOR, 1.0);
    Some((f64::from(used_percentage) / 100.0) / elapsed_share)
}

/// The reset countdown marker's tone at a burn-rate ratio. Each
/// `[sidebar.budget.pace]` threshold names the exclusive upper bound of the
/// calmer tier, checked worst-first so a misordered config degrades to the
/// worse visible warning. Sustainable pace rests at the countdown's soft tier;
/// color starts only once the burn rate outruns the configured yellow threshold.
pub(in crate::sidebar_pane::render) fn pace_style(
    theme: &Theme,
    ratio: f64,
    pace: &BudgetPaceConfig,
) -> Style {
    let pace_pct = ratio * 100.0;
    let style = if pace_pct > f64::from(pace.red) {
        theme.alarm(Modifier::empty())
    } else if pace_pct > f64::from(pace.amber) {
        theme.caution(Modifier::empty())
    } else if pace_pct > f64::from(pace.yellow) {
        theme.warn(Modifier::empty())
    } else {
        theme.body()
    };
    if style.fg.is_none() && pace_pct > f64::from(pace.yellow) {
        theme.body()
    } else {
        style
    }
}

/// The unmetered ("infinite") bar: a full-width empty `▱` track aligned with
/// the metered `5h`/`7d` bars, reading as "no meter to spend." The brand
/// `color` rides the `∞` icon *and* the track, so the two read as one branded
/// unmetered bar; the empty `▱` shape keeps it from competing with a real
/// draining fill, and under `NO_COLOR` the unbroken run still reads as an
/// empty track by shape.
#[cfg(test)]
pub(in crate::sidebar_pane::render) fn infinite_bar_spans(
    theme: &Theme,
    color: Color,
    width: usize,
) -> Vec<Span<'static>> {
    vec![Span::styled(
        std::iter::repeat_n(MANA_TRACK, width.max(1)).collect::<String>(),
        theme.style(color, Modifier::empty()),
    )]
}

/// Todo progress: filled dots for done, hollow dots for remaining, with the
/// numeric ratio appended. The shape carries it; the dots stay dim chrome and
/// the ratio reads at the card's soft middle weight.
pub(in crate::sidebar_pane::render) fn todo_spans(
    theme: &Theme,
    done: u32,
    total: u32,
) -> Vec<Span<'static>> {
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
        Span::styled(dots, theme.muted()),
        Span::styled(format!(" {done}/{total}"), theme.body()),
    ]
}

/// The `◇ {total}` marker: the blue diamond + the formatted cumulative
/// total. The shared head of every token line — [`token_breakdown_spans`] builds
/// on it, and a breakdown-less line (a Codex rollup-only total) uses it alone.
/// `fmt` picks the magnitude form ([`tokens_int`](super::fmt::tokens_int) live,
/// `tokens_short` for the precise W/M rows). The diamond is a colored marker;
/// the figure reads at the soft tier ([`Theme::soft`]) like every stat figure.
/// Display-only, never a decision driver.
pub(in crate::sidebar_pane::render) fn tokens_total_spans(
    theme: &Theme,
    total: u64,
    fmt: fn(u64) -> String,
) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            TOKENS_TOTAL,
            theme.styled(Component::TokenTotal, Modifier::empty()),
        ),
        Span::styled(format!(" {}", fmt(total)), theme.body()),
    ]
}

/// `⇡3 ⇣1`-style commit delta against the trunk: ahead then behind, zero
/// components omitted. Both the dim accent — commit-level branch facts rhyme
/// with the worktree name's accent and stay a category apart from the green/red
/// line-level churn; the `⇡`/`⇣` shape carries the direction under `NO_COLOR`.
pub(in crate::sidebar_pane::render) fn branch_delta_spans(
    theme: &Theme,
    ahead: u32,
    behind: u32,
) -> Vec<Span<'static>> {
    let style = theme.styled(Component::BranchDelta, Modifier::DIM);
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

/// `≡ main` — the worktree IS the trunk tip: zero commits ahead and behind,
/// a zero diff, and a clean working tree (untracked included). Dim green, the
/// calm-positive tone an idle/done agent wears — quiet enough to stay chrome
/// yet scannable when hunting removable worktrees; the `≡` shape carries the
/// verdict under `NO_COLOR`. The trunk worktree itself never wears it — the
/// caller gates on the group's live branch.
pub(in crate::sidebar_pane::render) fn trunk_equal_spans(
    theme: &Theme,
    trunk: &str,
) -> Vec<Span<'static>> {
    vec![Span::styled(
        format!("≡ {trunk}"),
        theme.good(Modifier::DIM),
    )]
}

/// `✓ main` — the worktree holds no work of its own (zero ahead, zero diff,
/// clean tree untracked included) but the trunk has moved on, so it is done
/// and safe to remove. The same dim green as the `≡` equal marker — one
/// calm-positive family, told apart by shape under `NO_COLOR`: `≡` "this is
/// the trunk", `✓` "finished, removable". The trunk worktree itself never
/// wears it — the caller gates on the group's live branch.
pub(in crate::sidebar_pane::render) fn trunk_clear_spans(
    theme: &Theme,
    trunk: &str,
) -> Vec<Span<'static>> {
    vec![Span::styled(
        format!("✓ {trunk}"),
        theme.good(Modifier::DIM),
    )]
}

/// `+127 -43`-style diff stat. Added in green, removed in red, both dim to
/// stay chrome — the gauge ramp owns the loud color slots.
pub(in crate::sidebar_pane::render) fn diff_spans(
    theme: &Theme,
    added: u32,
    removed: u32,
) -> Vec<Span<'static>> {
    vec![
        Span::styled(format!("+{added}"), theme.good(Modifier::DIM)),
        Span::raw(" "),
        Span::styled(format!("-{removed}"), theme.alarm(Modifier::DIM)),
    ]
}
