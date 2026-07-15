use super::*;
use crate::config::ContextMeterConfig;
use crate::config::GlyphRole;
use crate::sidebar_pane::render::theme::Component;
use jiff::SignedDuration;

/// Token-composition glyphs for the `◇ ↘ ↗ ◌` fleet breakdown: a diamond for
/// the cumulative total (input with cache-write folded in, plus output), the
/// directional arrows for input read in / output generated, and a hollow ring
/// for cache reads. The breakdown reads the same on the cockpit, the provider
/// dashboard, and the W/M store rows — one grammar, built by
/// [`token_breakdown_spans`], each marker in its one color everywhere. The `◍`
/// marker belongs to the agent card's context-composition line, which answers
/// a different question — what is in the window, not what the fleet burned —
/// so it leads with `▤` and reorders the same four columns by how the window
/// filled ([`context_breakdown_spans`]).
pub(in crate::sidebar_pane::render) fn token_total_glyph(theme: &Theme) -> String {
    theme.glyph(GlyphRole::TokensTotal).to_owned()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sidebar_pane::render) enum TokenDetail {
    Full,
    Summary,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::sidebar_pane::render) struct TokenColumns {
    pub(in crate::sidebar_pane::render) total: usize,
    pub(in crate::sidebar_pane::render) input: usize,
    pub(in crate::sidebar_pane::render) output: usize,
    pub(in crate::sidebar_pane::render) cache_read: usize,
}

/// The `◇ ↘ ↗ ◌` token breakdown as styled spans — the one shape every fleet
/// token line shares (cockpit headline line, provider headline line, W/M store
/// rows). Each marker wears its one color everywhere: the `◇` total in blue,
/// `↘` input in the expense vermilion, `↗` output in blue, and `◌` cache-read
/// in green. The figures read at the soft tier
/// ([`Theme::soft`]) like every stat figure; under `NO_COLOR` the glyph shapes
/// still spell the split. `fmt` chooses the magnitude form (`tokens_int` live,
/// `tokens_short` for the precise W/M rows). `total` is the caller's `◇` value
/// (`input` with cache-write folded in, plus output), passed in so a row can
/// read it straight from its accumulated window.
#[allow(clippy::too_many_arguments)]
pub(in crate::sidebar_pane::render) fn token_breakdown_spans(
    theme: &Theme,
    total: u64,
    input: u64,
    output: u64,
    cache_read: u64,
    fmt: fn(u64) -> String,
    detail: TokenDetail,
    cols: &TokenColumns,
) -> Vec<Span<'static>> {
    let mut spans = vec![
        Span::styled(
            token_total_glyph(theme),
            theme.styled(Component::TokenTotal, Modifier::empty()),
        ),
        Span::styled(
            format!(" {:>width$}", fmt(total), width = cols.total),
            theme.body(),
        ),
    ];
    let mut field = |role: GlyphRole, component: Component, value: u64, width: usize| {
        let glyph = theme.glyph(role);
        spans.push(Span::styled(
            format!(" {glyph} "),
            theme.styled(component, Modifier::empty()),
        ));
        spans.push(Span::styled(
            format!("{:>width$}", fmt(value), width = width),
            theme.body(),
        ));
    };
    if detail == TokenDetail::Full {
        field(GlyphRole::TokensInput, Component::Input, input, cols.input);
        field(
            GlyphRole::TokensOutput,
            Component::Output,
            output,
            cols.output,
        );
    }
    field(
        GlyphRole::TokensCacheRead,
        Component::CacheRead,
        cache_read,
        cols.cache_read,
    );
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
    for (role, component, value) in [
        (GlyphRole::TokensCacheRead, Component::CacheRead, cache_read),
        (
            GlyphRole::TokensCacheWrite,
            Component::CacheWrite,
            cache_write,
        ),
        (GlyphRole::TokensInput, Component::Input, input),
        (GlyphRole::TokensOutput, Component::Output, output),
    ] {
        if value == 0 {
            continue;
        }
        spans.push(Span::styled(seam, theme.muted()));
        spans.push(Span::styled(
            theme.glyph(role).to_owned(),
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
        Span::styled(
            theme.glyph(GlyphRole::TokensCompaction).to_owned(),
            compacting_style(theme),
        ),
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
        Span::styled(
            theme.glyph(GlyphRole::TokensFilled).to_owned(),
            theme.style(severity, Modifier::empty()),
        ),
        Span::styled(format!(" {}", fmt(filled)), theme.muted()),
    ]
}

pub(in crate::sidebar_pane::render) fn scroll_thumb_glyph(theme: &Theme) -> String {
    theme.glyph(GlyphRole::MeterScrollThumb).to_owned()
}

pub(in crate::sidebar_pane::render) fn scroll_track_glyph(theme: &Theme) -> String {
    theme.glyph(GlyphRole::MeterScrollTrack).to_owned()
}

/// Filled-cell count for `percent` of `width`, to the nearest whole cell: 0%
/// stays an unbroken track, 100% fills the whole width.
fn filled_cells(percent: u8, width: usize) -> usize {
    ((percent.min(100) as usize) * width.max(1) + 50) / 100
}

/// Quantize a percentage to the nearest horizontal half cell.
pub(super) fn filled_half_cells(fill_pct: f64, width: usize) -> (usize, bool) {
    let fill_pct = fill_pct.clamp(0.0, 100.0);
    let half_cells = (fill_pct * width as f64 * 2.0 / 100.0).round() as usize;
    let half_cells = if fill_pct > 0.0 { half_cells.max(1) } else { 0 };
    (half_cells / 2, half_cells % 2 == 1)
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
    filled_glyph: &str,
    track_glyph: &str,
) -> Vec<Span<'static>> {
    let width = width.max(1);
    let filled = filled.min(width);
    let mut spans = Vec::with_capacity(2);
    if filled > 0 {
        spans.push(Span::styled(filled_glyph.repeat(filled), filled_style));
    }
    if filled < width {
        spans.push(Span::styled(
            track_glyph.repeat(width - filled),
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
    bands: &ContextMeterConfig,
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
    bands: &ContextMeterConfig,
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
    bands: &ContextMeterConfig,
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

/// Position along the warm tail (`warn → caution → alarm`) for a value crossing
/// three escalating thresholds `yellow < amber < red`. `None` at or below
/// `yellow` so the caller keeps its resting tone; then `0.0` just past `yellow`,
/// `0.5` at `amber`, and `1.0` at `red` and beyond — the sweep
/// [`Theme::warm_heat_tone`] renders. Checked worst-first, so a misordered
/// config degrades to the worse tier.
fn warm_band_amount(value: u64, yellow: u64, amber: u64, red: u64) -> Option<f32> {
    if value > red {
        Some(1.0)
    } else if value > amber {
        Some(interpolate_heat(value, amber, red, 0.5, 1.0))
    } else if value > yellow {
        Some(interpolate_heat(value, yellow, amber, 0.0, 0.5))
    } else {
        None
    }
}

/// Position along the cool tail (`body → good`) for a value crossing two
/// descending thresholds `green > deep_green`. `None` at or above `green` so
/// the caller keeps its resting tone; then the amount climbs toward `1.0` at
/// `deep_green` and below. Checked greenest-first, so a misordered config
/// degrades to the more visible signal.
fn cool_band_amount(value: u64, green: u64, deep_green: u64) -> Option<f32> {
    if value <= deep_green {
        Some(1.0)
    } else if value < green {
        Some(interpolate_heat(value, deep_green, green, 1.0, 0.0))
    } else {
        None
    }
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

/// Keep a segment at ≥ 1/200th (0.5%) of the filled window; smaller is noise.
const SEGMENT_VISIBILITY_FLOOR: u128 = 200;

/// The context meter's health bar: the first visible segment paints in the
/// row's current health tone (`theme.heat_tone(amount)`), while later accent
/// segments (cache-write, fresh input) stay flat in their own tones. Components
/// below 0.5% of the filled window fold into the lead run; each remaining accent
/// starts with a gap-fronted half-rule cap (`╺` by default). A segmented bar
/// quantizes to whole cells so its last cap sits flush against the track, while
/// a flat bar keeps nearest-half-cell resolution. Segments too numerous for the
/// filled run drop smallest-first. Under `NO_COLOR` color collapses while the
/// leading gap in each accent cap keeps composition boundaries legible by shape.
pub(in crate::sidebar_pane::render) fn context_gauge_spans(
    theme: &Theme,
    amount: f32,
    segments: &[(u64, Color)],
    fill_pct: f64,
    width: usize,
) -> Vec<Span<'static>> {
    let width = width.max(1);
    let (filled, trailing_half) = filled_half_cells(fill_pct, width);
    let weight: u128 = segments.iter().map(|(value, _)| u128::from(*value)).sum();
    let survivors: Vec<(usize, u64)> = segments
        .iter()
        .enumerate()
        .filter_map(|(index, (value, _))| {
            (u128::from(*value) * SEGMENT_VISIBILITY_FLOOR >= weight && *value > 0)
                .then_some((index, *value))
        })
        .collect();
    let bar_track = theme.glyph(GlyphRole::MeterBarTrack).to_owned();
    let bar_cap = theme.glyph(GlyphRole::MeterBarCap).to_owned();
    let bar_half = theme.glyph(GlyphRole::MeterBarHalf).to_owned();
    let track = |drawn: usize| -> Option<Span<'static>> {
        (drawn < width).then(|| Span::styled(bar_track.repeat(width - drawn), theme.faint()))
    };
    if filled == 0 || weight == 0 || survivors.len() <= 1 {
        // No split to draw: the whole filled run uses the current health tone.
        let color = theme.heat_tone(amount);
        let mut spans = filled_run_spans(theme, color, filled);
        if trailing_half {
            spans.push(Span::styled(
                bar_half.clone(),
                theme.style(color, Modifier::empty()),
            ));
        }
        spans.extend(track(filled + usize::from(trailing_half)));
        return spans;
    }
    let cells = ((fill_pct.clamp(0.0, 100.0) / 100.0) * width as f64).round() as usize;
    let mut active = survivors;
    while active.len() > cells {
        let smallest = active.iter().map(|(_, value)| *value).min().unwrap_or(0);
        let drop = active
            .iter()
            .rposition(|(_, value)| *value == smallest)
            .unwrap_or(active.len() - 1);
        active.remove(drop);
    }

    let budget = cells * 2;
    let active_weight: u128 = active.iter().map(|(_, value)| u128::from(*value)).sum();
    let mut accents: Vec<(usize, u64, usize)> = active
        .iter()
        .skip(1)
        .map(|(index, value)| {
            (
                *index,
                *value,
                nearest_odd_share(*value, budget, active_weight),
            )
        })
        .collect();
    let mut accent_halves: usize = accents.iter().map(|(_, _, ink)| ink + 1).sum();

    // Each accent can yield whole cells back to the lead while retaining its
    // half-cell floor. Fit first, then repair representable weight inversions;
    // minimum-shape inversions remain when the fixed semantic order forces one.
    while accent_halves + 2 > budget {
        let Some((_, _, ink)) = accents
            .iter_mut()
            .max_by_key(|(_, weight, ink)| (*ink, *weight))
        else {
            break;
        };
        if *ink == 1 {
            break;
        }
        *ink -= 2;
        accent_halves -= 2;
    }
    let mut lead_ink = budget - accent_halves;
    loop {
        let mut inks = Vec::with_capacity(active.len());
        inks.push(lead_ink);
        inks.extend(accents.iter().map(|(_, _, ink)| *ink));
        let offender = accents
            .iter()
            .enumerate()
            .filter(|(_, (_, weight, ink))| {
                *ink > 1
                    && active
                        .iter()
                        .zip(&inks)
                        .any(|((_, other_weight), other_ink)| {
                            other_weight > weight && other_ink < ink
                        })
            })
            .max_by_key(|(_, (_, weight, ink))| (*ink, *weight))
            .map(|(index, _)| index);
        let Some(index) = offender else { break };
        accents[index].2 -= 2;
        lead_ink += 2;
    }

    let mut spans = Vec::with_capacity(active.len() * 2 + 1);
    spans.extend(filled_run_spans(
        theme,
        theme.heat_tone(amount),
        lead_ink / 2,
    ));
    for (index, _, ink) in accents {
        let color = segments[index].1;
        spans.push(Span::styled(
            bar_cap.clone(),
            theme.style(color, Modifier::empty()),
        ));
        spans.extend(filled_run_spans(theme, color, (ink - 1) / 2));
    }
    spans.extend(track(cells));
    spans
}

/// Quantize one proportional half-cell share to its nearest positive odd ink
/// count. An accent's leading blank half makes `ink + 1` an even cell budget.
fn nearest_odd_share(weight: u64, budget: usize, total_weight: u128) -> usize {
    let exact = u128::from(weight) * budget as u128;
    let quotient = exact / total_weight;
    if quotient % 2 == 1 {
        return quotient as usize;
    }
    let lower = quotient.saturating_sub(1).max(1);
    let upper = (quotient + 1).max(1);
    let lower_distance = exact.abs_diff(lower * total_weight);
    let upper_distance = exact.abs_diff(upper * total_weight);
    if lower_distance <= upper_distance {
        lower as usize
    } else {
        upper as usize
    }
}

fn filled_run_spans(theme: &Theme, color: Color, count: usize) -> Vec<Span<'static>> {
    if count == 0 {
        return Vec::new();
    }
    vec![Span::styled(
        theme.glyph(GlyphRole::MeterBarFilled).repeat(count),
        theme.style(color, Modifier::empty()),
    )]
}

/// The provider dashboard's draining budget ("mana / stamina") bar:
/// `remaining_pct` of the width in `▰`, the rest a `▱` track, with no brackets.
/// A full bar means budget *left*: it shortens as the window is spent, and the
/// reset countdown beside it says when it refills. Slides continuously green →
/// gold → clay-amber → red along the health ramp by how much remains, with the
/// `[theme.display.budget_bar]` zones as the warm stops ([`mana_style`]), so a near-spent
/// window reddens regardless of which window it is. The drained `▱` run rides
/// the dim chrome — a step up from the faint
/// context-gauge track, so the spent share stays legible on the dashboard. At
/// 0% remaining — the budget fully spent — the whole empty track turns red;
/// any nonzero remaining budget keeps at least one filled cell.
pub(in crate::sidebar_pane::render) fn mana_bar_spans(
    theme: &Theme,
    remaining_pct: u8,
    width: usize,
    zones: &BudgetBarConfig,
) -> Vec<Span<'static>> {
    // A fully spent window (0% remaining) reads as a full-width *red* empty track,
    // not the gray "no fill" track a plain drain leaves — an absent fill alone
    // would read as the same calm chrome as a barely-touched window. Paint the
    // empty `▱` track the bar's own spent tone (`mana_style` at 0%, the ramp's
    // alarm-red endpoint) so the label keeps mirroring its bar and "used up" is
    // unmistakable; only the tone changes, the glyph stays the empty track.
    if remaining_pct == 0 {
        return vec![Span::styled(
            theme.glyph(GlyphRole::MeterManaTrack).repeat(width.max(1)),
            mana_style(theme, remaining_pct, zones),
        )];
    }
    let filled = filled_cells(remaining_pct, width).max(1);
    two_tone_bar(
        filled,
        width,
        mana_style(theme, remaining_pct, zones),
        theme.muted(),
        theme.glyph(GlyphRole::MeterManaFilled),
        theme.glyph(GlyphRole::MeterManaTrack),
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
        theme.glyph(GlyphRole::MeterManaTrack).repeat(width.max(1)),
        theme.muted(),
    )]
}

/// The mana bar's tone at `remaining_pct` budget left: the full health ramp the
/// context meter rides ([`Theme::heat_tone`]), anchored green at a brimming
/// window and warming smoothly to red as it drains. Shared by the bar fill and
/// the `5h`/`7d` label beside it so the label mirrors its bar's tone.
pub(in crate::sidebar_pane::render) fn mana_style(
    theme: &Theme,
    remaining_pct: u8,
    zones: &BudgetBarConfig,
) -> Style {
    theme.style(
        theme.heat_tone(mana_heat_amount(remaining_pct, zones)),
        Modifier::empty(),
    )
}

/// The mana bar's heat amount: a full green→red drain anchored green at `100%`,
/// with the `[theme.display.budget_bar]` zones as the warm stops the remaining figure
/// falls through — `100 → 0.0` green, `yellow → ⅓` warn, `amber → ⅔` caution,
/// `red`/below `→ 1.0` alarm — so the bar warms continuously as it empties. Each
/// zone names the exclusive upper bound of remaining budget where its tier is
/// reached ([`BudgetBarConfig`]); checked worst-first, so a misordered user
/// config degrades to the worse tier.
fn mana_heat_amount(remaining_pct: u8, zones: &BudgetBarConfig) -> f32 {
    let remaining = u64::from(remaining_pct.min(100));
    let yellow = u64::from(zones.yellow);
    let amber = u64::from(zones.amber);
    let red = u64::from(zones.red);
    if remaining < red {
        1.0
    } else if remaining < amber {
        interpolate_heat(remaining, red, amber, 1.0, 2.0 / 3.0)
    } else if remaining < yellow {
        interpolate_heat(remaining, amber, yellow, 2.0 / 3.0, 1.0 / 3.0)
    } else {
        interpolate_heat(remaining, yellow, 100, 1.0 / 3.0, 0.0)
    }
}

/// Treat the first five percent of a budget window as already elapsed for pace
/// math. This renderer-only damping keeps a tiny early spend from exploding the
/// reset countdown's color, and is deliberately a tuning constant rather than a
/// user-facing band like `[theme.display.budget_bar.burn_rate]`.
const PACE_ELAPSED_FLOOR: f64 = 0.05;

/// Suppress the cool pace signal until enough of the window has elapsed to
/// make underspend meaningful: two hours of a five-hour window, or about 2.8
/// days of a seven-day window. The warm side keeps its early-window behavior.
const COOL_PACE_MIN_ELAPSED: f64 = 0.4;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::sidebar_pane::render) struct PaceReading {
    pub(in crate::sidebar_pane::render) ratio: f64,
    pub(in crate::sidebar_pane::render) elapsed_share: f64,
}

/// Burn-rate pace for a budget window: used share divided by elapsed share.
/// `1.0` means the current spend rate exactly lasts to reset. The first slice
/// of a fresh window is floored so a tiny amount of usage does not explode the
/// ratio, while the returned elapsed share remains raw for signal gating.
/// Overdue reset times clamp to a full elapsed window.
pub(in crate::sidebar_pane::render) fn pace_reading(
    used_percentage: u8,
    duration: SignedDuration,
    until_reset: SignedDuration,
) -> Option<PaceReading> {
    let duration_secs = duration.as_secs();
    if duration_secs <= 0 {
        return None;
    }
    let elapsed_secs = duration_secs - until_reset.as_secs();
    if elapsed_secs <= 0 {
        return None;
    }
    let elapsed_share = (elapsed_secs as f64 / duration_secs as f64).clamp(0.0, 1.0);
    let ratio = (f64::from(used_percentage) / 100.0) / elapsed_share.max(PACE_ELAPSED_FLOOR);
    Some(PaceReading {
        ratio,
        elapsed_share,
    })
}

/// The reset countdown marker's tone at a burn-rate reading. Sustainable pace
/// rests at the countdown's soft tier. Beyond the configured yellow threshold
/// the marker slides gold through amber to red; below green it slides from soft
/// toward good green, once [`COOL_PACE_MIN_ELAPSED`] admits a meaningful
/// underspend signal. Each side checks its most visible stop first so
/// misordered config degrades toward visibility. With color off the marker
/// keeps the soft tier — the countdown beside it carries the pace.
pub(in crate::sidebar_pane::render) fn pace_style(
    theme: &Theme,
    reading: PaceReading,
    pace: &BudgetBurnRateConfig,
) -> Style {
    let pace_pct = (reading.ratio * 100.0).max(0.0).round() as u64;
    warm_band_amount(
        pace_pct,
        u64::from(pace.yellow),
        u64::from(pace.amber),
        u64::from(pace.red),
    )
    .map(|amount| theme.style(theme.warm_heat_tone(amount), Modifier::empty()))
    .or_else(|| {
        (reading.elapsed_share >= COOL_PACE_MIN_ELAPSED)
            .then(|| cool_band_amount(pace_pct, u64::from(pace.green), u64::from(pace.deep_green)))
            .flatten()
            .map(|amount| theme.style(theme.calm_tone(amount), Modifier::empty()))
    })
    .filter(|style| style.fg.is_some())
    .unwrap_or_else(|| theme.body())
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
        theme.glyph(GlyphRole::MeterManaTrack).repeat(width.max(1)),
        theme.style(color, Modifier::empty()),
    )]
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
        spans.push(Span::styled(
            format!("{}{ahead}", theme.glyph(GlyphRole::WorktreeAhead)),
            style,
        ));
    }
    if behind > 0 {
        if !spans.is_empty() {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            format!("{}{behind}", theme.glyph(GlyphRole::WorktreeBehind)),
            style,
        ));
    }
    spans
}

/// Render a trunk marker as `<glyph> <trunk>` in the component tone named by
/// the caller. Used by the trunk-state ladder after branch/diff stats.
pub(in crate::sidebar_pane::render) fn trunk_glyph_spans(
    theme: &Theme,
    role: GlyphRole,
    trunk: &str,
    component: Component,
) -> Vec<Span<'static>> {
    vec![Span::styled(
        format!("{} {trunk}", theme.glyph(role)),
        theme.styled(component, Modifier::empty()),
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
