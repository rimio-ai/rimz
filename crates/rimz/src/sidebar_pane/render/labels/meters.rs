use super::*;
use jiff::SignedDuration;

/// Token-composition glyphs for the `◇ ↘ ↗ ◌` fleet breakdown: a diamond for
/// the cumulative total (input with cache-write folded in, plus output), the
/// directional arrows for input read in / output generated, and a hollow ring
/// for cache reads. The breakdown reads the same on the cockpit, the provider
/// dashboard, and the W/M ledger rows — one grammar, built by
/// [`token_breakdown_spans`], each marker in its one color everywhere (the
/// `◇` violet, the rest their [`SEGMENT_INPUT`]-family segment tones). The
/// `◍` marker belongs to the agent card's context-composition line, which
/// answers a different question — what is in the window, not what the fleet
/// burned — so it leads with `▤` and reorders the same four columns by how the
/// window filled ([`context_breakdown_spans`]).
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
/// session has condensed its window. Violet — the compaction vocabulary's tone.
pub(in crate::sidebar_pane::render) const CONTEXT_COMPACTIONS: &str = "↻";

/// The context-window composition colors — one tone per segment, shared by the
/// bar's colored runs and the context line's `◌`/`◍`/`↘` markers so the line
/// reads as the bar's legend by construction. Cache-write reads violet (the
/// `meta` family) to stay clear of the severity ramp's yellow/amber/red; it
/// shares the `◇` total's violet, but those markers never co-occur because `◇`
/// is fleet vocabulary and `◍` belongs to the card context line. `↗` output is
/// not in the window (it joins next turn), so it carries no bar segment; its
/// green is free because the meter's calm tier reads blue.
pub(in crate::sidebar_pane::render) const SEGMENT_CACHE_READ: Color = Color::Blue;
pub(in crate::sidebar_pane::render) const SEGMENT_CACHE_WRITE: Color = Color::Magenta;
pub(in crate::sidebar_pane::render) const SEGMENT_INPUT: Color = Color::Red;
pub(in crate::sidebar_pane::render) const SEGMENT_OUTPUT: Color = Color::Green;

/// The `◇ ↘ ↗ ◌` token breakdown as styled spans — the one shape every fleet
/// token line shares (cockpit today line, provider today line, W/M ledger
/// rows). Each marker wears its one color everywhere: the `◇` total its
/// soft-violet, the rest the same bar-segment tones the card's context line
/// legends ([`SEGMENT_INPUT`] and siblings, `↗` output in the segment green) —
/// one glyph, one color, across the whole sidebar. The figures read at the
/// soft tier ([`Theme::soft`]) like every stat figure; under `NO_COLOR` the
/// glyph shapes still spell the split. `fmt` chooses the magnitude form
/// (`tokens_int` live, `tokens_short` for the precise W/M rows). `total` is the
/// caller's `◇` value (`input` with cache-write folded in, plus output), passed
/// in so a row can read it straight from its accumulated window.
pub(in crate::sidebar_pane::render) fn token_breakdown_spans(
    theme: &Theme,
    total: u64,
    input: u64,
    output: u64,
    cache_read: u64,
    fmt: fn(u64) -> String,
) -> Vec<Span<'static>> {
    let mut spans = tokens_total_spans(theme, total, fmt);
    let mut field = |glyph: &str, color: Color, value: u64| {
        spans.push(Span::styled(
            format!(" {glyph} "),
            theme.style(color, Modifier::empty()),
        ));
        spans.push(Span::styled(fmt(value), theme.soft()));
    };
    field(TOKENS_IN, SEGMENT_INPUT, input);
    field(TOKENS_OUT, SEGMENT_OUTPUT, output);
    field(TOKENS_CACHED, SEGMENT_CACHE_READ, cache_read);
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
/// bar-segment color ([`SEGMENT_CACHE_READ`] and siblings), so the line is the
/// bar's color-keyed legend; the figures read at the dim chrome weight — a step
/// under the name line's soft tokens, so the colored markers carry the line.
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
    for (glyph, color, value) in [
        (TOKENS_CACHED, SEGMENT_CACHE_READ, cache_read),
        (TOKENS_CACHE_WRITE, SEGMENT_CACHE_WRITE, cache_write),
        (TOKENS_IN, SEGMENT_INPUT, input),
        (TOKENS_OUT, SEGMENT_OUTPUT, output),
    ] {
        if value == 0 {
            continue;
        }
        spans.push(Span::styled(seam, theme.dim()));
        spans.push(Span::styled(glyph, theme.style(color, Modifier::empty())));
        spans.push(Span::styled(format!(" {}", fmt(value)), theme.dim()));
        seam = " ";
    }
    spans
}

/// The `· ↻ N` compaction tail for the context line, shown from the first
/// completed compaction. The `·` seam reads at the dim chrome like the
/// composition seams; only the marker wears the violet compaction tone, and
/// the count stays dim like the adjacent context figures.
pub(in crate::sidebar_pane::render) fn context_compaction_spans(
    theme: &Theme,
    count: u32,
) -> Vec<Span<'static>> {
    if count == 0 {
        return Vec::new();
    }
    vec![
        Span::styled(" · ", theme.dim()),
        Span::styled(CONTEXT_COMPACTIONS, compacting_style(theme)),
        Span::styled(format!(" {count}"), theme.dim()),
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

/// The agent-cards viewport scrollbar, ridden on the right-margin column when
/// the cards overflow: a solid `▐` thumb over a hairline `▕` track. The
/// solid/thin shape difference carries the position, so it survives `NO_COLOR`.
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

/// The context meter's **severity** tone — one calm-blue → yellow → amber →
/// red ramp shared by the bar, the `▣` glyph, and the `▤` context-line head,
/// so every context read on the card speaks one urgency. Calm reads **blue**
/// ("cold — plenty of headroom"), keeping the meter clear of the green running
/// vocabulary and of the composition segments. The *tier* is the domain's
/// verdict ([`ContextSeverity`], classified on the producer and stamped on the
/// row); the renderer only maps it to a tone here.
pub(in crate::sidebar_pane::render) fn severity_color(severity: ContextSeverity) -> Color {
    match severity {
        ContextSeverity::Calm => Color::Blue,
        ContextSeverity::Yellow => Color::Yellow,
        ContextSeverity::Amber => ORANGE,
        ContextSeverity::Red => Color::Red,
    }
}

/// The window token's tone: subordinate chrome — a capability label, not a
/// status signal; the context-meter severity ramp owns the loud color slot —
/// tinted by size class so the magnitude reads at a glance: clay amber for a
/// 1m+ window, gold for the 258k tier, sky blue for 128k, and the dim chrome
/// below that, level with the model/effort tokens beside it. The tinted bands
/// ride the `DIM` modifier so the token never outshines the meter; under
/// `NO_COLOR` every band collapses to the same bare DIM weight.
pub(in crate::sidebar_pane::render) fn window_style(theme: &Theme, window: u64) -> Style {
    let color = match window {
        1_000_000.. => ORANGE,
        258_000.. => Color::Yellow,
        128_000.. => Color::Blue,
        _ => return theme.dim(),
    };
    theme.style(color, Modifier::DIM)
}

/// Context bar: a thin rule whose filled run grows left-to-right as the window
/// fills, painted in `color` (the caller's [`severity_color`]) over a faint
/// track. The label and value columns live in the renderer's shared bar row;
/// here we paint just the meter.
pub(in crate::sidebar_pane::render) fn gauge_spans(
    theme: &Theme,
    color: Color,
    percent: u8,
    width: usize,
) -> Vec<Span<'static>> {
    two_tone_bar(
        filled_cells(percent, width),
        width,
        theme.style(color, Modifier::empty()),
        theme.faint(),
        BAR_FILLED,
        BAR_TRACK,
    )
}

/// Like [`gauge_spans`], but the filled run is split into colored segments by
/// token weight — showing *where* the context window went (cache reads, cache
/// writes, and fresh input in the caller-provided order). `total_pct` sizes
/// that filled run exactly as the single-color gauge would; the segments
/// apportion that run by their weights with largest-remainder rounding, so the
/// colored cells always sum to the filled count and the bar never over- or
/// under-fills. With no breakdown to
/// draw it falls back to the plain gauge. Under `NO_COLOR` the segments merge
/// into one heavy run — the split is a color enrichment; the fill level still
/// reads by shape.
pub(in crate::sidebar_pane::render) fn segmented_gauge_spans(
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
            theme.style(Color::Red, Modifier::empty()),
        )];
    }
    let filled = filled_cells(remaining_pct, width).max(1);
    two_tone_bar(
        filled,
        width,
        mana_style(theme, remaining_pct, zones),
        theme.dim(),
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
        theme.dim(),
    )]
}

/// The mana bar's tone at `remaining_pct` budget left: red when near-spent (or
/// fully spent), then the same gold → clay-amber escalation the age and
/// context ramps speak, resting green while the budget sits above every
/// warning zone. Each `[sidebar.budget]` zone names the exclusive upper bound
/// of remaining budget where its tier applies ([`BudgetZonesConfig`]); checked
/// worst-first, so a misordered user config degrades to the worse tier. Shared
/// by the bar fill and the `5h`/`7d` label beside it so the label mirrors its
/// bar's tone.
pub(in crate::sidebar_pane::render) fn mana_style(
    theme: &Theme,
    remaining_pct: u8,
    zones: &BudgetZonesConfig,
) -> Style {
    if remaining_pct < zones.red {
        theme.style(Color::Red, Modifier::empty())
    } else if remaining_pct < zones.amber {
        theme.style(ORANGE, Modifier::empty())
    } else if remaining_pct < zones.yellow {
        theme.style(Color::Yellow, Modifier::empty())
    } else {
        theme.style(Color::Green, Modifier::empty())
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
/// worse visible warning. Sustainable pace stays unstyled; color starts only
/// once the burn rate outruns the configured yellow threshold.
pub(in crate::sidebar_pane::render) fn pace_style(
    theme: &Theme,
    ratio: f64,
    pace: &BudgetPaceConfig,
) -> Style {
    let pace_pct = ratio * 100.0;
    let style = if pace_pct > f64::from(pace.red) {
        theme.style(Color::Red, Modifier::empty())
    } else if pace_pct > f64::from(pace.amber) {
        theme.style(ORANGE, Modifier::empty())
    } else if pace_pct > f64::from(pace.yellow) {
        theme.style(Color::Yellow, Modifier::empty())
    } else {
        Style::default()
    };
    if style.fg.is_none() && pace_pct > f64::from(pace.yellow) {
        theme.soft()
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
pub(in crate::sidebar_pane::render) fn infinite_bar_spans(
    theme: &Theme,
    color: u8,
    width: usize,
) -> Vec<Span<'static>> {
    vec![Span::styled(
        std::iter::repeat_n(MANA_TRACK, width.max(1)).collect::<String>(),
        theme.style(Color::Indexed(color), Modifier::empty()),
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
        Span::styled(dots, theme.dim()),
        Span::styled(format!(" {done}/{total}"), theme.soft()),
    ]
}

/// The `◇ {total}` marker: the soft-violet diamond + the formatted cumulative
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
        Span::styled(TOKENS_TOTAL, theme.style(Color::Magenta, Modifier::empty())),
        Span::styled(format!(" {}", fmt(total)), theme.soft()),
    ]
}

/// `⇡3 ⇣1`-style commit delta against the trunk: ahead then behind, zero
/// components omitted. Both dim cyan — commit-level branch facts rhyme with
/// the bold-cyan worktree name and stay a category apart from the green/red
/// line-level churn; the `⇡`/`⇣` shape carries the direction under `NO_COLOR`.
pub(in crate::sidebar_pane::render) fn branch_delta_spans(
    theme: &Theme,
    ahead: u32,
    behind: u32,
) -> Vec<Span<'static>> {
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
        theme.style(Color::Green, Modifier::DIM),
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
        theme.style(Color::Green, Modifier::DIM),
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
