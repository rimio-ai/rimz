use super::*;
use crate::sidebar_pane::pixel::meter::{MeterPixels, MeterRaster};
use crate::sidebar_pane::pixel::{
    image_id_color, placeholder_cluster, placeholder_columns_supported,
};

/// The session's statusline enrichment, when it published any.
pub(super) fn ctx(row: &SidebarRow) -> Option<&AgentContext> {
    agent(row).and_then(|agent| agent.context.as_ref())
}

/// Model name preferred from the provider display label over the normalized
/// context model id, then the coarser lifecycle scalar, and shortened for the
/// row (`Opus 4.8 (1M)`); never synthesized.
pub(super) fn display_model(row: &SidebarRow) -> Option<String> {
    ctx(row)
        .and_then(|context| context.model_display_name.as_deref())
        .or_else(|| ctx(row).and_then(|context| context.model_id.as_deref()))
        .or_else(|| agent(row).and_then(|agent| agent.model.as_deref()))
        .filter(|model| !model.is_empty())
        .map(model_label)
}

/// Reasoning configuration: the session's observed live effort is preferred;
/// the hook/store scalar falls back before the first observation, then an
/// explicit live thinking flag supplies the provider-neutral fallback token.
pub(super) fn display_reasoning(row: &SidebarRow) -> Option<&str> {
    ctx(row)
        .and_then(|context| context.effort.as_deref())
        .filter(|effort| !effort.is_empty())
        .or_else(|| {
            agent(row)
                .and_then(|agent| agent.effort.as_deref())
                .filter(|effort| !effort.is_empty())
        })
        .or_else(|| {
            (ctx(row).and_then(|context| context.thinking_enabled) == Some(true))
                .then_some("thinking")
        })
}

/// Column widths for the per-row context meter: a one-cell lead-glyph label
/// (`▣`, sharing the column with the `◇`/`◷` glyphs on the lines below it) and a
/// fixed 5-cell right value, with the bar filling the middle. The value
/// (`78.2%`) fits five cells. The provider dashboard's budget bars carry their
/// own label/value widths but the same shape.
const BAR_LABEL_WIDTH: usize = 1;
const BAR_VALUE_WIDTH: usize = 5;

/// One aligned meter row: `<indent><label:3> <bar> <value:5>`. The caller's
/// `make_bar` builds the colored bar spans to the supplied width and supplies the
/// `label_style` for the lead glyph (the context meter tints its `▣` with the
/// bar's severity); this helper owns the indent, the fixed label and value
/// columns, and the gaps — so every row built through it shares one bar-start
/// column and one value-end column by construction, with no per-call alignment
/// math. The value column reads at the dim chrome weight, matching the token
/// figures below it — the bar's fill carries the urgency.
pub(super) fn bar_row(
    theme: &Theme,
    label: &str,
    label_style: Style,
    value: &str,
    make_bar: impl FnOnce(usize) -> Vec<Span<'static>>,
    width: usize,
) -> Line<'static> {
    // "  "(2) + label(3) + " "(1) + bar + " "(1) + value(5)
    let bar_width = width
        .saturating_sub(2 + BAR_LABEL_WIDTH + 1 + 1 + BAR_VALUE_WIDTH)
        .max(1);
    let mut spans = vec![
        Span::raw("  "),
        Span::styled(format!("{label:<BAR_LABEL_WIDTH$}"), label_style),
        Span::raw(" "),
    ];
    spans.extend(make_bar(bar_width));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        format!("{value:>BAR_VALUE_WIDTH$}"),
        theme.muted(),
    ));
    Line::from(trim_spans_to_width(spans, width))
}

/// The context meter — the resting card's one bar. `ctx` on the left, the
/// **percent used** on the right (always — the window *size* moves to the
/// expanded token line), the bar between. The fill amount and its calm-blue →
/// continuous OKLab warn/caution/alarm severity ([`row_severity`], bands from
/// `[theme.display.context_meter]`) come from the used percentage and the
/// absolute tokens. Fill geometry is log-scaled by default, with curve strength
/// scaling from linear at windows up to 256k to the full curve at 1M, so
/// large-window working ranges stay visible; the displayed percentage remains
/// the raw measurement. When the statusline reports the per-message token
/// breakdown, every severity splits the fill into cache-read / cache-write /
/// fresh-input segments. Components below 0.5% of the filled window fold into
/// the lead run; each remaining accent starts with a gap-fronted `╺` cap, and
/// the segmented fill ends flush at a whole cell. The `▣` glyph wears the same
/// severity, so glyph, bar, and the `▤` line below speak one urgency. The value
/// prefers a one-decimal precise fraction (`78.2%`) over the integer gauge. An
/// empty (0%) window reads the hollow `▢`; any usage fills it to `▣`.
pub(super) fn gauge_line(
    ctx: &RowCtx<'_>,
    row: &SidebarRow,
    meter_pixels: Option<&mut MeterPixels>,
) -> Line<'static> {
    let theme = ctx.theme;
    let bands = ctx.bands;
    let width = content_width(ctx.width);
    let percent = gauge_percent(row).unwrap_or(0);
    let precise = precise_context_pct(row);
    let value = pct_label(precise, percent);
    let fill = precise.unwrap_or_else(|| f64::from(percent));
    let fill = if bands.log_scale {
        log_scaled_fill(fill, display_context_window(row))
    } else {
        fill
    };
    let severity = row_severity(row, bands);
    // One health amount drives the whole row: the bar's filled run, the `▣`
    // glyph, and the `▤` line below all use the same tone. The composition
    // segments (where the window went) ride the bar at every severity; the
    // dominant cache-read run carries the row health color while cache-write and
    // fresh input stay flat accents beside it.
    let amount = severity_heat_amount(severity, percent, row.context_used_tokens(), bands);
    let color = theme.heat_tone(amount);
    let segments = gauge_segments(theme, row);
    let glyph = if percent == 0 {
        theme.glyph(GlyphRole::MeterContextEmpty)
    } else {
        theme.glyph(GlyphRole::MeterContextFull)
    };
    bar_row(
        theme,
        glyph,
        theme.style(color, Modifier::empty()),
        &value,
        |bar_width| {
            if let Some(pixels) = meter_pixels
                && let Some(spans) = pixel_gauge_spans(
                    theme,
                    fill,
                    color,
                    segments
                        .as_ref()
                        .map(|segments| &segments[..])
                        .unwrap_or(&[]),
                    bar_width,
                    pixels,
                )
            {
                return spans;
            }
            match &segments {
                Some(segments) => context_gauge_spans(theme, amount, segments, fill, bar_width),
                None => context_gauge_spans(theme, amount, &[], fill, bar_width),
            }
        },
        width,
    )
}

/// Window below which the drawn fill stays linear.
const LOG_SCALE_LINEAR_FLOOR_TOKENS: f64 = 256_000.0;
/// Window at which the curve reaches full strength ([`LOG_SCALE_MAX_K`]).
const LOG_SCALE_FULL_CURVE_TOKENS: f64 = 1_000_000.0;
/// Log-curve strength ceiling (`ln(1 + K·f) / ln(1 + K)` at a 1M window).
const LOG_SCALE_MAX_K: f64 = 6.0;

/// Scale the drawn fill's log-curve strength with the resolved context window.
fn log_scaled_fill(pct: f64, window: Option<u64>) -> f64 {
    let Some(window) = window else { return pct };
    let ramp = ((window as f64 - LOG_SCALE_LINEAR_FLOOR_TOKENS)
        / (LOG_SCALE_FULL_CURVE_TOKENS - LOG_SCALE_LINEAR_FLOOR_TOKENS))
        .clamp(0.0, 1.0);
    let k = LOG_SCALE_MAX_K * ramp;
    if k == 0.0 {
        // The k→0 limit is the identity; evaluating it directly produces 0/0.
        return pct;
    }
    let fraction = (pct / 100.0).clamp(0.0, 1.0);
    100.0 * (1.0 + k * fraction).ln() / (1.0 + k).ln()
}

fn pixel_gauge_spans(
    theme: &Theme,
    fill_pct: f64,
    health: Color,
    segments: &[(u64, Color)],
    width: usize,
    pixels: &mut MeterPixels,
) -> Option<Vec<Span<'static>>> {
    if !placeholder_columns_supported(width) {
        return None;
    }
    let health = theme.pixel_rgb(health)?;
    let track = theme.pixel_rgb(theme.faint().fg?)?;
    let segments = segments
        .iter()
        .filter(|(weight, _)| *weight > 0)
        .map(|(weight, color)| Some((*weight, theme.pixel_rgb(*color)?)))
        .collect::<Option<Vec<_>>>()?;
    let width_cells = u16::try_from(width).ok()?;
    let image_id = pixels.intern(MeterRaster::new(
        width_cells,
        (fill_pct / 100.0).clamp(0.0, 1.0),
        health,
        segments,
        track,
    ))?;
    let style = Style::default().fg(image_id_color(image_id));
    Some(
        (0..width_cells)
            .map(|col| Span::styled(placeholder_cluster(0, col), style))
            .collect(),
    )
}

/// The row's severity verdict: the tier the producer classified and stamped
/// ([`SidebarRow::context_severity`]) when present, else classified locally
/// from the same inputs and bands — the fallback for a snapshot produced
/// before the stamp (an older producer mid-upgrade). Either way it is
/// [`ContextSeverity::classify`]'s verdict, never a renderer-private ramp.
pub(super) fn row_severity(row: &SidebarRow, bands: &ContextMeterConfig) -> ContextSeverity {
    agent(row)
        .and_then(|agent| agent.context_severity)
        .unwrap_or_else(|| {
            ContextSeverity::classify(
                gauge_percent(row).unwrap_or(0),
                row.context_used_tokens(),
                bands,
            )
        })
}

fn row_severity_color(
    theme: &Theme,
    row: &SidebarRow,
    bands: &ContextMeterConfig,
    severity: ContextSeverity,
) -> Color {
    severity_heat_color(
        theme,
        severity,
        gauge_percent(row).unwrap_or(0),
        row.context_used_tokens(),
        bands,
    )
}

/// A precise context-used fraction (0..=100) from the current-message token
/// composition over the window size, so the `ctx` value can read a decimal
/// (`78.2%`). The composition (`input + cache_creation + cache_read`) is exactly
/// what `used_percentage` measures, so the decimal refines the same number a
/// statusline reports. `None` (no breakdown, or no window size) falls the value
/// back to the integer gauge percent — which the fold now derives from the same
/// window the card displays, so a hook-only agent still reads a consistent
/// (integer) percentage.
pub(super) fn precise_context_pct(row: &SidebarRow) -> Option<f64> {
    let window = ctx(row)?.tokens.as_ref()?.context_window_size? as f64;
    if window <= 0.0 {
        return None;
    }
    let used = row.context_used_tokens()? as f64;
    Some((used / window * 100.0).clamp(0.0, 100.0))
}

/// The context bar's value — [`SidebarRow::context_gauge_percent`], the same
/// input the producer classified the stamped severity from.
pub(super) fn gauge_percent(row: &SidebarRow) -> Option<u8> {
    row.context_gauge_percent()
}

fn correlated_current_usage(row: &SidebarRow) -> Option<&AgentCurrentUsage> {
    let tokens = ctx(row)?.tokens.as_ref()?;
    let usage = tokens.current_usage.as_ref()?;
    let filled = usage.input_tokens.unwrap_or(0)
        + usage.cache_creation_input_tokens.unwrap_or(0)
        + usage.cache_read_input_tokens.unwrap_or(0);
    (tokens.current_context_tokens.is_none() || tokens.current_context_tokens == Some(filled))
        .then_some(usage)
}

/// The context bar's color segments, when the per-message breakdown is known,
/// left to right: cache reads (row health tone, seeded with green), cache writes
/// (compaction/delegation violet), fresh `input` (the expense vermilion) — the
/// same tones the context line's markers wear, so the line legends the bar by
/// construction. The rich statusline blob is preferred; the row-level
/// [`SidebarRow::call_split`] stands in when the blob carries no split. `None`
/// when neither source reported one (a
/// fresh session, or a statusline blob cleared by `/compact` — a rollout-fed
/// split refreshes with the next call instead), so the bar falls back to a
/// single-color ramp.
pub(super) fn gauge_segments(theme: &Theme, row: &SidebarRow) -> Option<[(u64, Color); 3]> {
    if let Some(usage) = correlated_current_usage(row) {
        let input = usage.input_tokens.unwrap_or(0);
        let writes = usage.cache_creation_input_tokens.unwrap_or(0);
        let reads = usage.cache_read_input_tokens.unwrap_or(0);
        return (input + writes + reads > 0).then_some([
            (reads, theme.component(Component::CacheRead)),
            (writes, theme.component(Component::CacheWrite)),
            (input, theme.component(Component::Input)),
        ]);
    }
    let split = row.call_split()?;
    (split.filled() > 0).then_some([
        (split.cache_read, theme.component(Component::CacheRead)),
        (split.cache_write, theme.component(Component::CacheWrite)),
        (split.fresh_input, theme.component(Component::Input)),
    ])
}

/// The card's stats line with the last-activity age pinned right. Current-window
/// truth retains the `▤` form: `▤` is
/// `input + cache_write + cache_read` of the latest API call — exactly the
/// numerator the `▣` meter scales — so the bar's percent and this absolute
/// figure read as one measurement, and the `▤` head wears the bar's severity
/// tone to seal that pairing. A `·` seam separates the headline from the
/// latest call's composition, ordered by how the window filled: `◌` read back
/// from cache, `◍` newly written to it, `↘` fresh input, `↗` output generated
/// (which joins the window next turn) — each marker in its bar-segment color,
/// so the line doubles as the bar's legend. The `◇` totals stay the cockpit /
/// fleet-store / subagent vocabulary. An authoritative occupancy scalar with
/// no matching composition renders as a bare `▤` total and flat meter. When no
/// current-window occupancy is available but the provider exposes cumulative
/// session counters, the line instead uses that shared `◇ ↘ ↗ ◌` grammar
/// without implying occupancy. The
/// rich statusline blob is preferred; the row-level
/// [`SidebarRow::call_split`] stands in when the blob carries no split. The
/// exact latest-call `▤` composition wins over cumulative `◇` session totals;
/// the scalar wins over both when the split disagrees.
/// Falls back to the bare `▤` rollup total when no categorized source exists,
/// using zero before the first token measurement. The age rides the right edge
/// only once it crosses five minutes
/// — a recently active agent shows the breakdown alone, left-aligned, rather than
/// a noisy sub-`5m` clock — as the clock-fill glyph ([`elapsed_glyph`]) over the
/// continuous age tone ([`activity_age_style`]): dim while warm, then sliding
/// through warn, caution, and alarm toward the hour, when resuming would likely
/// re-read the whole context uncached. A finished row heats on the same ramp —
/// its context is exactly what a follow-up prompt would pay to re-read.
pub(super) fn context_tokens_line(row_ctx: &RowCtx<'_>, row: &SidebarRow) -> Line<'static> {
    let theme = row_ctx.theme;
    let bands = row_ctx.bands;
    let now = row_ctx.now;
    let width = content_width(row_ctx.width);
    // The age clock is the line's one right pin — resource stats are
    // process-row vocabulary and never ride an agent card.
    let age = activity_short(row.last_activity, now)
        .map(|label| {
            let secs = age_secs(row.last_activity, now);
            vec![Span::styled(
                format!("{} {label}", elapsed_glyph(theme, secs)),
                activity_age_style(theme, secs),
            )]
        })
        .unwrap_or_default();
    // The `▤` head mirrors the bar's row-specific severity tone, so the absolute
    // figure and the meter above it read at one urgency. A row with no gauge
    // percent folds to 0 and lets the token overlay alone speak.
    let severity = row_severity_color(theme, row, bands, row_severity(row, bands));
    let mut left = vec![Span::raw("  ")];
    if let Some(usage) = correlated_current_usage(row) {
        let input = usage.input_tokens.unwrap_or(0);
        let output = usage.output_tokens.unwrap_or(0);
        let cache_write = usage.cache_creation_input_tokens.unwrap_or(0);
        let cache_read = usage.cache_read_input_tokens.unwrap_or(0);
        left.extend(context_breakdown_spans(
            theme,
            severity,
            input + cache_write + cache_read,
            cache_read,
            cache_write,
            input,
            output,
            tokens_int,
        ));
    } else if let Some(split) = row.call_split() {
        // The row-level split — the lifecycle rail's per-call composition.
        left.extend(context_breakdown_spans(
            theme,
            severity,
            split.filled(),
            split.cache_read,
            split.cache_write,
            split.fresh_input,
            split.output,
            tokens_int,
        ));
    } else if let Some(total) = ctx(row)
        .and_then(|context| context.tokens.as_ref())
        .and_then(|tokens| tokens.current_context_tokens)
    {
        left.extend(context_total_spans(theme, severity, total, tokens_int));
    } else if let Some(usage) = ctx(row)
        .and_then(|context| context.tokens.as_ref())
        .and_then(|tokens| tokens.session_usage.as_ref())
    {
        left.extend(token_breakdown_spans(
            theme,
            usage.displayed_total_tokens(),
            usage.displayed_input_tokens(),
            usage.displayed_output_tokens(),
            usage.cache_read_tokens(),
            tokens_int,
            TokenDetail::Full,
            &TokenColumns::default(),
        ));
    } else {
        let total = row
            .context_used_tokens()
            .or_else(|| agent(row).and_then(|agent| agent.usage.total_tokens))
            .unwrap_or(0);
        left.extend(context_total_spans(theme, severity, total, tokens_int));
    }
    if let Some(percent) = ctx(row)
        .and_then(|context| context.tokens.as_ref())
        .and_then(|tokens| tokens.session_usage.as_ref())
        .and_then(crate::agents::AgentSessionUsage::cache_hit_percent)
    {
        let style = match CacheHealth::classify(percent) {
            CacheHealth::Good => theme.good(Modifier::empty()),
            CacheHealth::Caution => theme.warn(Modifier::empty()),
            CacheHealth::Alarm => theme.alarm(Modifier::empty()),
        };
        left.push(Span::styled(" · ", theme.muted()));
        left.push(Span::styled(format!("{percent}%"), style));
    }
    left.extend(context_compaction_spans(
        theme,
        agent(row).map_or(0, |agent| agent.compaction_count),
    ));
    left.extend(context_tool_repeat_spans(
        theme,
        agent(row).and_then(|agent| agent.tool_repeat.as_ref()),
        row_ctx.tool_repeat_warn_after,
    ));
    pin_right(left, age, width)
}

#[cfg(test)]
mod tests {
    use super::log_scaled_fill;

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= 0.1,
            "expected {expected:.1}, got {actual:.3}"
        );
    }

    #[test]
    fn log_scaled_fill_at_one_million_preserves_bounds_and_known_points() {
        assert_close(log_scaled_fill(0.0, Some(1_000_000)), 0.0);
        assert_close(log_scaled_fill(10.0, Some(1_000_000)), 24.1);
        assert_close(log_scaled_fill(40.0, Some(1_000_000)), 62.9);
        assert_close(log_scaled_fill(80.0, Some(1_000_000)), 90.3);
        assert_close(log_scaled_fill(100.0, Some(1_000_000)), 100.0);
    }

    #[test]
    fn log_scaled_fill_is_linear_for_small_or_unknown_windows() {
        for window in [None, Some(200_000), Some(256_000)] {
            assert_eq!(log_scaled_fill(40.0, window), 40.0);
        }
    }

    #[test]
    fn log_scaled_fill_is_monotonic_in_fill() {
        for pct in 0..100 {
            assert!(
                log_scaled_fill(f64::from(pct), Some(1_000_000))
                    < log_scaled_fill(f64::from(pct + 1), Some(1_000_000))
            );
        }
    }

    #[test]
    fn log_scaled_fill_is_monotonic_in_window() {
        let fills = [256_000, 384_000, 512_000, 1_000_000]
            .map(|window| log_scaled_fill(40.0, Some(window)));

        assert!(fills.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(fills[0] < fills[1]);
        assert!(fills[1] < fills[3]);
    }
}
