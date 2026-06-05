//! The pinned provider dashboard — per-provider header, brand emblem, stats and
//! budget bars — and the W/M fleet ledger rows that seal the bottom.

use jiff::{SignedDuration, Timestamp};
use ratatui::style::{Color, Modifier};
use ratatui::text::{Line, Span};
use rimz::agents::RateLimitWindow;
use rimz::{SidebarProviderPanel, SpendTally, SpendWindow};

use crate::render::fmt::{dollars2, reset_countdown, tokens_int, tokens_short, window_label};
use crate::render::labels::{
    SEGMENT_CACHE_READ, SEGMENT_INPUT, SEGMENT_OUTPUT, TOKENS_CACHED, TOKENS_IN, TOKENS_OUT,
    TOKENS_TOTAL, infinite_bar_spans, mana_bar_spans, mana_color, token_breakdown_spans,
};
use crate::render::theme::Theme;

use super::{SESSIONS_GLYPH, pin_right, trim_spans_to_width};

/// The provider dashboard's fixed art column width: the brand emblem is padded
/// to this many cells so the stats/bar column to its right starts at one shared
/// cell for every provider block — the bars align across providers by
/// construction. Dropped (bars run full-width) below [`PROVIDER_ART_MIN_WIDTH`].
const PROVIDER_ART_WIDTH: usize = 9;

/// Narrowest sidebar that still affords the art column beside a bar; below it
/// the emblem is dropped so the bar keeps a legible length.
const PROVIDER_ART_MIN_WIDTH: usize = 34;

/// The provider bar's label slot (`5h` / `7d` / `30d` / `∞`) and reset-value
/// column, shared by every provider bar so they align front and back. The label
/// fits three cells (`30d`); the value holds `↻ ` plus a two-unit reset countdown
/// (up to `↻ 30d10h`).
const PROVIDER_LABEL_WIDTH: usize = 3;
const PROVIDER_VALUE_WIDTH: usize = 8;

/// How close to a full window-length a reset must read to count as "not started".
/// A not-started window keeps its reset slid to `now + duration`, but a live
/// reading lands a hair under (minute-flooring + read latency) and a cached one
/// drifts down until the next refresh — so allow this margin below the full
/// window. A *started* window's reset has ticked well below full, so it clears
/// the margin easily.
const NOT_STARTED_GRACE: SignedDuration = SignedDuration::from_secs(120);

/// The fleet ledger rows pinned to the bottom of the dashboard: the trailing
/// week (`W:`) and month (`M:`), each reading `◎ sessions  ◇ ↘ ↗ ◌  $spend`
/// across every provider (today's headline lives in the cockpit, so these climb
/// `week → month`). The token figures read the precise one-decimal form (`16.5k`)
/// at full strength — the ledger is the exact record next to the cockpit's
/// coarse live read — each marker in its one shared color (the sky-blue window
/// tag, the teal `◎`, the violet `◇`, the segment-toned arrows and ring) and
/// the `$` bold money-green; the
/// spend deliberately does **not** animate (only today's headline does). Both
/// rows share one set of right-aligned column widths so the labels stack and
/// every number column lines up. Empty (dropped) until something is recorded.
pub(in crate::render) fn fleet_ledger_lines(
    theme: &Theme,
    tally: Option<&SpendTally>,
    width: usize,
) -> Vec<Line<'static>> {
    let Some(tally) = tally.filter(|t| !t.is_zero()) else {
        return Vec::new();
    };
    let cols = WmColumns::measure(&tally.week, &tally.month);
    vec![
        wm_row(theme, "W", &tally.week, &cols, width),
        wm_row(theme, "M", &tally.month, &cols, width),
    ]
}

/// The shared right-aligned column widths for the `W:`/`M:` ledger rows, measured
/// across both windows so a 2- and a 3-digit figure stack on one right edge.
struct WmColumns {
    sessions: usize,
    total: usize,
    input: usize,
    output: usize,
    cache_read: usize,
    usd: usize,
}

impl WmColumns {
    fn measure(week: &SpendWindow, month: &SpendWindow) -> Self {
        let max2 = |a: String, b: String| a.chars().count().max(b.chars().count());
        Self {
            sessions: max2(week.sessions.to_string(), month.sessions.to_string()),
            total: max2(tokens_short(week.tokens), tokens_short(month.tokens)),
            input: max2(tokens_short(week.input), tokens_short(month.input)),
            output: max2(tokens_short(week.output), tokens_short(month.output)),
            cache_read: max2(
                tokens_short(week.cache_read),
                tokens_short(month.cache_read),
            ),
            usd: max2(dollars2(week.usd), dollars2(month.usd)),
        }
    }
}

/// One ledger row — `W: ◎ {sessions}  ◇ {total} ↘ {in} ↗ {out} ◌ {cache_read}`
/// left-clustered, the `$ {spend}` pinned to the right edge. The `W:`/`M:`
/// window tag wears sky blue — distinct from the teal `◎` beside it — and each
/// token marker its one shared color, with the figures at full strength
/// ([`Theme::value`]). Every numeric field is right-aligned to the shared
/// [`WmColumns`] width, so the `W:` and `M:` rows stack into one tidy grid. The
/// `◍` cache-write field is intentionally omitted here — the ledger keeps to
/// the four headline figures the all-time read needs.
fn wm_row(
    theme: &Theme,
    label: &str,
    window: &SpendWindow,
    cols: &WmColumns,
    width: usize,
) -> Line<'static> {
    let value = theme.value();
    let marker = |color: Color| theme.style(color, Modifier::empty());
    let left = vec![
        Span::styled(format!("{label}: "), marker(Color::Blue)),
        Span::styled(SESSIONS_GLYPH, marker(Color::Cyan)),
        Span::styled(
            format!(" {:>w$}", window.sessions, w = cols.sessions),
            value,
        ),
        Span::raw("  "),
        Span::styled(TOKENS_TOTAL, marker(Color::Magenta)),
        Span::styled(
            format!(" {:>w$}", tokens_short(window.tokens), w = cols.total),
            value,
        ),
        Span::styled(format!(" {TOKENS_IN} "), marker(SEGMENT_INPUT)),
        Span::styled(
            format!("{:>w$}", tokens_short(window.input), w = cols.input),
            value,
        ),
        Span::styled(format!(" {TOKENS_OUT} "), marker(SEGMENT_OUTPUT)),
        Span::styled(
            format!("{:>w$}", tokens_short(window.output), w = cols.output),
            value,
        ),
        Span::styled(format!(" {TOKENS_CACHED} "), marker(SEGMENT_CACHE_READ)),
        Span::styled(
            format!(
                "{:>w$}",
                tokens_short(window.cache_read),
                w = cols.cache_read
            ),
            value,
        ),
    ];
    let right = vec![Span::styled(
        format!("{:>w$}", dollars2(window.usd), w = cols.usd),
        theme.style(Color::Green, Modifier::BOLD),
    )];
    pin_right(left, right, width)
}

/// One clickable tab in the dashboard's tab bar: the tab-bar line's position,
/// the half-open column range its label occupies, and the provider kind it
/// activates. [`provider_panel_lines`] emits positions relative to its returned
/// lines and columns relative to the unpadded content; `compose_lines`
/// translates both into absolute screen coordinates (the bottom-block base and
/// the one-cell chrome gutter) before storing them on `UiState::tab_hits` for
/// the mouse hit-test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProviderTabHit {
    pub(crate) line: usize,
    pub(crate) col_start: u16,
    pub(crate) col_end: u16,
    pub(crate) kind: String,
}

/// The pinned per-provider dashboard. With one account it is that provider's
/// block — a header line then the brand emblem zipped against the aggregate
/// stats and the account-scoped budget bars. With several it is **tabbed**: a
/// tab bar of agent-kind labels (each in its brand color, the active tab led by
/// a `▸` marker so the pick survives `NO_COLOR` by shape) over the active
/// provider's block alone — the budgets read one account at a time instead of
/// stacking. A metered account drains one "mana" bar per budget window toward
/// its reset; an unmetered (API-key) account shows the `∞` "infinite power" bar
/// in the label slot with no countdown. The bars share one start and one end
/// column across every block, so the dashboard reads as one aligned grid.
/// Bottom chrome — the tab labels are its only hit targets, never a jump.
pub(in crate::render) fn provider_panel_lines(
    theme: &Theme,
    providers: &[SidebarProviderPanel],
    active_kind: Option<&str>,
    width: usize,
) -> (Vec<Line<'static>>, Vec<ProviderTabHit>) {
    let mut lines = Vec::new();
    let Some(first) = providers.first() else {
        return (lines, Vec::new());
    };
    let active = active_kind
        .and_then(|kind| providers.iter().find(|panel| panel.kind == kind))
        .unwrap_or(first);
    let mut hits = Vec::new();
    if providers.len() > 1 {
        let (tab_line, tab_hits) = provider_tab_line(theme, providers, &active.kind, width);
        hits = tab_hits;
        lines.push(tab_line);
    }
    lines.push(provider_header_line(theme, active, width));
    // A blank line below the provider name sets the identity apart from the
    // emblem + stats body, matching the cockpit's breathing room.
    lines.push(Line::from(""));
    lines.extend(provider_body_lines(theme, active, width));
    (lines, hits)
}

/// The dashboard's tab bar: every provider's kind label in its brand color,
/// the active tab bold behind a `▸` marker, the rest dim. Each tab reserves
/// the marker cell whether or not it is active, so switching tabs never shifts
/// a label — the bar holds still and only the marker and weight move. The bar
/// holds to one screen row: a tab that would overflow `width` is dropped
/// whole (label and hit together), so the hit map stays in lockstep with the
/// frame however many kinds register or however narrow the pane. Returns the
/// line plus one [`ProviderTabHit`] per rendered label (line index 0, columns
/// over the marker + label cells) for the mouse hit-test.
fn provider_tab_line(
    theme: &Theme,
    providers: &[SidebarProviderPanel],
    active_kind: &str,
    width: usize,
) -> (Line<'static>, Vec<ProviderTabHit>) {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut hits = Vec::new();
    let mut col: u16 = 0;
    for (index, panel) in providers.iter().enumerate() {
        let gap: u16 = if index > 0 { 2 } else { 0 };
        let active = panel.kind == active_kind;
        let label = if active {
            format!("{TAB_MARKER}{}", panel.kind)
        } else {
            format!(" {}", panel.kind)
        };
        // Kind labels are registry-fixed ASCII slugs, so chars == cells.
        let cells = label.chars().count() as u16;
        if usize::from(col + gap + cells) > width {
            break;
        }
        if gap > 0 {
            spans.push(Span::raw("  "));
            col += gap;
        }
        let style = if active {
            theme.style(Color::Indexed(panel.color), Modifier::BOLD)
        } else {
            theme.style(Color::Indexed(panel.color), Modifier::DIM)
        };
        spans.push(Span::styled(label, style));
        hits.push(ProviderTabHit {
            line: 0,
            col_start: col,
            col_end: col + cells,
            kind: panel.kind.clone(),
        });
        col += cells;
    }
    (Line::from(spans), hits)
}

/// The active-tab marker: the lead cell every tab reserves, filled on the
/// active one — the pick's `NO_COLOR`-surviving shape.
const TAB_MARKER: char = '▸';

/// `Claude v2.1.158 · Claude Max          ⇅ rc`: the product name in the
/// brand color and the version + plan dim on the left, with the violet `⇅ rc`
/// flag pinned to the top-right corner of the block when remote control is on for
/// the provider. Fields drop out when unknown.
fn provider_header_line(
    theme: &Theme,
    panel: &SidebarProviderPanel,
    width: usize,
) -> Line<'static> {
    let mut left = vec![Span::styled(
        panel.product_name.clone(),
        theme.style(Color::Indexed(panel.color), Modifier::BOLD),
    )];
    if let Some(version) = panel.version.as_deref() {
        left.push(Span::styled(format!(" v{version}"), theme.dim()));
    }
    if let Some(plan) = panel.plan.as_deref() {
        left.push(Span::styled(" · ", theme.faint()));
        left.push(Span::styled(plan.to_owned(), theme.dim()));
    }
    let right = if panel.remote_control {
        vec![Span::styled(
            "⇅ rc",
            theme.style(Color::Magenta, Modifier::BOLD),
        )]
    } else {
        Vec::new()
    };
    pin_right(left, right, width)
}

/// The block beneath the header: the brand emblem in a fixed left column zipped
/// against the right column — aggregate stats on the first line, the budget bars
/// below. The art is dropped (and the bars run full width) when the sidebar is
/// too narrow to fit both.
fn provider_body_lines(
    theme: &Theme,
    panel: &SidebarProviderPanel,
    width: usize,
) -> Vec<Line<'static>> {
    let show_art = !panel.art.is_empty() && width >= PROVIDER_ART_MIN_WIDTH;
    let art_column = if show_art { PROVIDER_ART_WIDTH + 1 } else { 0 };
    let bar_region = width.saturating_sub(art_column);

    // The right column, top to bottom: aggregate stats then the budget bars,
    // packed directly so the three rows line up against the three-line emblem and
    // the bars sit right under the numbers (no separator row).
    let mut rights: Vec<Vec<Span<'static>>> = vec![provider_stats_spans(theme, panel, bar_region)];
    rights.extend(provider_bar_rows(theme, panel, bar_region));

    let rows = panel.art.len().max(rights.len());
    let mut lines = Vec::with_capacity(rows);
    for index in 0..rows {
        let mut spans: Vec<Span<'static>> = Vec::new();
        if show_art {
            let art_line = panel.art.get(index).map(String::as_str).unwrap_or("");
            spans.push(Span::styled(
                pad_to(art_line, PROVIDER_ART_WIDTH),
                theme.style(Color::Indexed(panel.color), Modifier::empty()),
            ));
            spans.push(Span::raw(" "));
        }
        if let Some(right) = rights.get(index) {
            spans.extend(right.iter().cloned());
        }
        lines.push(Line::from(trim_spans_to_width(spans, width)));
    }
    lines
}

/// The provider's aggregate stats line beside the emblem: today's `◎` session
/// count then the token breakdown `◇ ↘ ↗ ◌` (integer magnitudes) on the
/// left — the fleet-ledger rows' exact vocabulary, scoped to this provider —
/// with the bold money-green spend pinned to the right edge of the bar
/// `region`. Always rendered — an idle account reads `◎ 0  ◇ 0 …  $0.00` so
/// the line above the budget bars is never blank. Every figure is today's
/// transcript-history burn for this provider, summed across every session from
/// the JSONL — the accurate cross-session total, and the only cost source for
/// token-only providers like Codex. The `◍` cache-write figure is omitted like
/// the ledger rows' — the line keeps to the headline figures so the `◎` count
/// fits beside the emblem. The summed `+/-` churn is intentionally absent —
/// a noisy per-account aggregate; per-worktree churn lives on the group
/// headers and per-agent churn on the work line.
fn provider_stats_spans(
    theme: &Theme,
    panel: &SidebarProviderPanel,
    region: usize,
) -> Vec<Span<'static>> {
    let today = panel
        .spending
        .as_ref()
        .map(|spending| spending.today)
        .unwrap_or_default();
    let mut left = vec![
        Span::styled(SESSIONS_GLYPH, theme.style(Color::Cyan, Modifier::empty())),
        Span::styled(format!(" {}", today.sessions), theme.value()),
        Span::raw("  "),
    ];
    left.extend(token_breakdown_spans(
        theme,
        today.tokens,
        today.input,
        today.output,
        today.cache_write,
        today.cache_read,
        tokens_int,
        false,
    ));
    let right = vec![Span::styled(
        dollars2(today.usd),
        theme.style(Color::Green, Modifier::BOLD),
    )];
    pin_right(left, right, region).spans
}

/// The provider's budget bars within `region`: a metered account drains one
/// "mana" bar per reported window (`5h`, `7d`, `30d`, …, ordered short→long);
/// an unmetered account shows the single `∞` bar. Each reset reads a two-unit
/// countdown scaled to its magnitude. Each row aligns front and back within
/// `region`, so they line up across providers too.
fn provider_bar_rows(
    theme: &Theme,
    panel: &SidebarProviderPanel,
    region: usize,
) -> Vec<Vec<Span<'static>>> {
    if !panel.metered {
        return vec![infinite_bar_row(theme, panel.color, region)];
    }
    panel
        .windows
        .iter()
        .filter_map(|window| {
            metered_bar_row(theme, window, region, longer_window_spent(panel, window))
        })
        .collect()
}

/// Whether a window with a strictly longer duration is spent — a higher-level cap
/// being exhausted gates this shorter window (its budget is unusable until the
/// longer one resets), so the renderer paints the shorter row exhausted too (e.g.
/// a spent 7-day cap gating the 5-hour bar).
fn longer_window_spent(panel: &SidebarProviderPanel, window: &RateLimitWindow) -> bool {
    let mins = window.duration_mins.unwrap_or(0);
    panel.windows.iter().any(|other| {
        other.duration_mins.unwrap_or(0) > mins
            && other.used_percentage.is_some_and(|used| used >= 100)
    })
}

/// Whether a window has not started its clock. These budgets are sliding windows:
/// the provider keeps `resets_at` slid a full window-length ahead until the first
/// token, so a reset still within [`NOT_STARTED_GRACE`] of the full window means
/// the clock hasn't begun — the displayed countdown would be a placeholder.
///
/// The not-started floor is ~1% used (a fresh 5h window reads `usedPercent: 1`,
/// never 0), so detection keys on the reset distance, not a 0% reading. Any usage
/// **above** that floor means the window has clearly started — its reset is then a
/// real countdown — so >1% short-circuits to "started" regardless of the reset
/// (this also covers a spent window at 100%). An absent reset or duration can't be
/// judged, so it isn't flagged.
fn window_not_started(window: &RateLimitWindow) -> bool {
    if window.used_percentage > Some(1) {
        return false;
    }
    let (Some(reset), Some(mins)) = (window.resets_at, window.duration_mins) else {
        return false;
    };
    let full = SignedDuration::from_secs(i64::from(mins) * 60);
    reset.duration_since(Timestamp::now()) >= full - NOT_STARTED_GRACE
}

/// One metered budget bar row: the window's label (`5h`/`7d`/`30d`), the draining
/// mana bar (filled = remaining), and the `↻ <reset>` countdown right-aligned in
/// the value column. The label mirrors its bar's severity color. `force_exhausted`
/// paints the row as fully spent — red, no countdown — regardless of the window's
/// own reading (a longer spent window gates it). `None` when the window reported
/// no usage percentage and is not force-exhausted.
///
/// A window that has **not started** drops its countdown — a full bar with no
/// `↻` reads "send a message to start it" rather than a misleading ticking reset.
/// These are sliding windows that begin counting only on the first token, so until
/// then the provider keeps `resets_at` slid a full window-length ahead. Detect that
/// by the reset distance ([`window_not_started`]), not a 0% reading — a fresh 5h
/// window still reports ~1% used, never 0. Codex reports a placeholder usedPercent
/// (~99) with no `resets_at` before the first token; that variant is caught by the
/// absent-reset + known-duration check in the `remaining` computation below.
fn metered_bar_row(
    theme: &Theme,
    window: &RateLimitWindow,
    region: usize,
    force_exhausted: bool,
) -> Option<Vec<Span<'static>>> {
    let not_started = !force_exhausted && window_not_started(window);
    let remaining = if force_exhausted {
        0
    } else {
        let raw = 100u8.saturating_sub(window.used_percentage?);
        // Codex reports a placeholder usedPercent (≈99) with no resetsAt before the
        // first token and a known duration — normalise to full so the bar matches
        // the empty countdown.
        if not_started || (window.resets_at.is_none() && window.duration_mins.is_some() && raw > 0)
        {
            100
        } else {
            raw
        }
    };
    let label = window_label(window.duration_mins);
    let value = if force_exhausted || not_started {
        String::new()
    } else {
        window
            .resets_at
            .map(|at| format!("↻ {}", reset_countdown(at)))
            .unwrap_or_default()
    };
    let bar_width = provider_bar_width(region);
    let mut spans = vec![
        Span::styled(
            format!("{label:<PROVIDER_LABEL_WIDTH$}"),
            theme.style(mana_color(remaining), Modifier::empty()),
        ),
        Span::raw(" "),
    ];
    spans.extend(mana_bar_spans(theme, remaining, bar_width));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        format!("{value:>PROVIDER_VALUE_WIDTH$}"),
        theme.dim(),
    ));
    Some(spans)
}

/// The unmetered `∞` bar row: the infinity icon rides the label slot (aligned
/// with `5h`/`7d`), then the full infinite bar — icon and track in the one
/// brand color, so the row reads as a single branded unmetered bar. The value
/// column is reserved but empty — no countdown — so the bar's right edge still
/// aligns with the metered bars'.
fn infinite_bar_row(theme: &Theme, color: u8, region: usize) -> Vec<Span<'static>> {
    let bar_width = provider_bar_width(region);
    let mut spans = vec![
        Span::styled(
            format!("{:<PROVIDER_LABEL_WIDTH$}", "∞"),
            theme.style(Color::Indexed(color), Modifier::BOLD),
        ),
        Span::raw(" "),
    ];
    spans.extend(infinite_bar_spans(theme, color, bar_width));
    spans.push(Span::raw(" "));
    spans.push(Span::raw(" ".repeat(PROVIDER_VALUE_WIDTH)));
    spans
}

/// The bar's cell width inside a provider `region`: the region less the label,
/// the value column, and the two single-cell gaps that frame the bar. At least
/// one cell, so a narrow sidebar still paints a (short) bar.
fn provider_bar_width(region: usize) -> usize {
    region
        .saturating_sub(PROVIDER_LABEL_WIDTH + 1 + 1 + PROVIDER_VALUE_WIDTH)
        .max(1)
}

/// Pad (or clip) a string to exactly `width` terminal cells — the fixed art
/// column, so the right column starts at one shared cell for every block.
fn pad_to(value: &str, width: usize) -> String {
    let count = value.chars().count();
    if count >= width {
        value.chars().take(width).collect()
    } else {
        let mut padded = value.to_owned();
        padded.extend(std::iter::repeat_n(' ', width - count));
        padded
    }
}
