use super::*;

/// The session's statusline enrichment, when it published any.
pub(super) fn ctx(row: &SidebarRow) -> Option<&AgentContext> {
    agent(row).and_then(|agent| agent.context.as_ref())
}

/// Model name preferred from the statusline (`Opus 4.8 (1M context)`) over the
/// coarser transcript scalar (`Opus`), then shortened for the row
/// (`Opus 4.8 (1M)`); never synthesized.
pub(super) fn display_model(row: &SidebarRow) -> Option<String> {
    ctx(row)
        .and_then(|context| context.model_display_name.as_deref())
        .or_else(|| agent(row).and_then(|agent| agent.model.as_deref()))
        .filter(|model| !model.is_empty())
        .map(model_label)
}

/// Reasoning effort: the hook/ledger value (what the user configured) is
/// preferred; context falls back for sessions whose lifecycle observation has
/// not named it. This means a configured `xhigh` wins over provider/catalog
/// defaults such as `medium` or `high`.
pub(super) fn display_effort(row: &SidebarRow) -> Option<&str> {
    agent(row)
        .and_then(|agent| agent.effort.as_deref())
        .or_else(|| ctx(row).and_then(|context| context.effort.as_deref()))
        .filter(|effort| !effort.is_empty())
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
/// `[sidebar.context]`) come from the used percentage and the absolute tokens;
/// when the statusline reports the per-message token breakdown a *calm* fill is
/// split into colored segments (cache reads / cache writes / fresh input) that
/// add up to exactly that percentage, and a warmed bar goes one solid severity
/// run. The `▣` glyph wears the same severity, so glyph, bar, and the `▤` line
/// below speak one urgency. The value prefers a one-decimal precise fraction
/// (`78.2%`) over the integer gauge. An empty (0%) window reads the hollow
/// `▢`; any usage fills it to `▣`.
pub(super) fn gauge_line(
    theme: &Theme,
    row: &SidebarRow,
    bands: &ContextSeverityConfig,
    width: usize,
) -> Option<Line<'static>> {
    let percent = gauge_percent(row)?;
    let value = pct_label(precise_context_pct(row), percent);
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
        CONTEXT_EMPTY_GLYPH
    } else {
        CONTEXT_GLYPH
    };
    Some(bar_row(
        theme,
        glyph,
        theme.style(color, Modifier::empty()),
        &value,
        |bar_width| match &segments {
            Some(segments) => context_gauge_spans(theme, amount, segments, percent, bar_width),
            None => context_gauge_spans(theme, amount, &[], percent, bar_width),
        },
        width,
    ))
}

/// The row's severity verdict: the tier the producer classified and stamped
/// ([`SidebarRow::context_severity`]) when present, else classified locally
/// from the same inputs and bands — the fallback for a snapshot produced
/// before the stamp (an older producer mid-upgrade). Either way it is
/// [`ContextSeverity::classify`]'s verdict, never a renderer-private ramp.
pub(super) fn row_severity(row: &SidebarRow, bands: &ContextSeverityConfig) -> ContextSeverity {
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
    bands: &ContextSeverityConfig,
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
/// what `used_percentage` measures, so the decimal refines the same number.
/// `None` (no breakdown, or no window size) falls the value back to the integer
/// gauge percent.
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

/// The context bar's color segments, when the per-message breakdown is known,
/// left to right: cache reads (row health tone, seeded with teal), cache writes
/// (compaction/delegation violet), fresh `input` (context-alarm red) — the same
/// tones the context line's markers wear, so the line legends the bar by
/// construction. The rich statusline blob is preferred; the row-level
/// [`SidebarRow::call_split`]
/// (Codex's rollout `last_token_usage`, which reports no cache-write) stands in
/// when the blob carries no split. `None` when neither source reported one (a
/// fresh session, or a statusline blob cleared by `/compact` — a rollout-fed
/// split refreshes with the next call instead), so the bar falls back to a
/// single-color ramp.
pub(super) fn gauge_segments(theme: &Theme, row: &SidebarRow) -> Option<[(u64, Color); 3]> {
    if let Some(usage) = ctx(row)
        .and_then(|context| context.tokens.as_ref())
        .and_then(|tokens| tokens.current_usage.as_ref())
    {
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
        (0, theme.component(Component::CacheWrite)),
        (split.fresh_input, theme.component(Component::Input)),
    ])
}

/// The card's context line — `▤` the filled part of the window (integer
/// magnitudes) with the last-activity age pinned right. `▤` is
/// `input + cache_write + cache_read` of the latest API call — exactly the
/// numerator the `▣` meter scales — so the bar's percent and this absolute
/// figure read as one measurement, and the `▤` head wears the bar's severity
/// tone to seal that pairing. A `·` seam separates the headline from the
/// latest call's composition, ordered by how the window filled: `◌` read back
/// from cache, `◍` newly written to it, `↘` fresh input, `↗` output generated
/// (which joins the window next turn) — each marker in its bar-segment color,
/// so the line doubles as the bar's legend. The `◇` totals stay the cockpit /
/// fleet-ledger / subagent vocabulary — this line answers "what is in the
/// window", not "what did today burn". The rich statusline blob is preferred;
/// the row-level [`SidebarRow::call_split`] (Codex's rollout
/// `last_token_usage`) stands in when the blob carries no split — its
/// cache-write column is unreported there, so it drops from the line. Falls
/// back to the bare `▤` rollup total when neither source has a split (Claude
/// before the first API call and right after `/compact`), so the line shows
/// *something* for every agent. The age rides the right edge only once it
/// crosses a full minute
/// — a just-active agent shows the breakdown alone, left-aligned, rather than
/// a misleading `1m` — as the clock-fill glyph ([`elapsed_glyph`]) over the
/// continuous age tone ([`activity_age_style`]): dim while warm, then sliding
/// through warn, caution, and alarm toward the hour, when resuming would likely
/// re-read the whole context uncached.
pub(super) fn context_tokens_line(
    theme: &Theme,
    row: &SidebarRow,
    bands: &ContextSeverityConfig,
    now: Timestamp,
    width: usize,
) -> Option<Line<'static>> {
    // The age clock is the line's one right pin — resource stats are
    // process-row vocabulary and never ride an agent card.
    let age = activity_short(row.last_activity, now)
        .map(|label| {
            let secs = age_secs(row.last_activity, now);
            vec![Span::styled(
                format!("{} {label}", elapsed_glyph(secs)),
                activity_age_style(theme, secs),
            )]
        })
        .unwrap_or_default();
    // The `▤` head mirrors the bar's row-specific severity tone, so the absolute
    // figure and the meter above it read at one urgency. A row with no gauge
    // percent folds to 0 and lets the token overlay alone speak.
    let severity = row_severity_color(theme, row, bands, row_severity(row, bands));
    let mut left = vec![Span::raw("  ")];
    if let Some(usage) = ctx(row)
        .and_then(|context| context.tokens.as_ref())
        .and_then(|tokens| tokens.current_usage.as_ref())
    {
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
        // The row-level split — the lifecycle rail's per-call composition
        // (Codex's rollout `last_token_usage`). Its protocol reports no
        // per-call cache-write, so that column passes 0 and drops from the
        // line.
        left.extend(context_breakdown_spans(
            theme,
            severity,
            split.filled(),
            split.cache_read,
            0,
            split.fresh_input,
            split.output,
            tokens_int,
        ));
    } else if let Some(total) = agent(row).and_then(|agent| agent.total_tokens) {
        left.extend(context_total_spans(theme, severity, total, tokens_int));
    } else {
        return None;
    }
    left.extend(context_compaction_spans(
        theme,
        agent(row).map_or(0, |agent| agent.compaction_count),
    ));
    Some(pin_right(left, age, width))
}
