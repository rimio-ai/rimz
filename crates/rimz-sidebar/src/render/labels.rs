//! Semantic sidebar vocabulary: the canonical status glyphs, posture pills,
//! and the gauge / spinner / pulse glyph helpers.
//!
//! Every meter in the sidebar — context-window %, todo progress, diff stats —
//! renders through the same vocabulary so they read as siblings, not as
//! one-off widgets (see [the sidebar grammar](../../../docs/internals/sidebar.md)).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use rimz::feed::{AgentStatus, PermissionPosture};

use super::fmt::tokens_short;
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
        // agent). These two carry every attention state.
        AgentStatus::Waiting => "?",
        AgentStatus::Failed => "!",
        AgentStatus::Running => WORKING_FRAMES[3],
        AgentStatus::Idle => "○",
        AgentStatus::Success => "✓",
    }
}

/// Working: a braille spinner cycling its dots. Spans the most time of any
/// state, so it is the steady motion the eye learns to ignore until something
/// changes. No frame matches idle `○`, so a frozen frame still reads as "working".
const WORKING_FRAMES: [&str; 8] = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];

/// Thinking: a sparkle that grows and fades. Reserved for read-only plan mode —
/// the agent is reasoning, not writing — so its motion reads as lighter than the
/// working fill.
const THINKING_FRAMES: [&str; 8] = ["·", "✢", "✳", "✶", "✻", "✽", "✻", "✶"];

/// Resolver answering: a braille spinner while a resolver composes the answer on
/// the bridge. This is the one "waiting for an answer" motion — it is genuinely
/// active and time-bounded by the resolver budget, unlike a human-blocked `?`,
/// which stays still.
const RESOLVER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn frame(frames: &[&'static str], animation_phase: u64) -> &'static str {
    frames[(animation_phase as usize) % frames.len()]
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

/// The still sparkle representing the thinking (plan-mode) bucket in the fleet
/// header — the fullest thinking frame, so a count reads as a thinking cell at
/// rest. The working bucket reuses the static [`status_glyph`] for `Running`.
pub(super) fn thinking_still() -> &'static str {
    THINKING_FRAMES[5]
}

/// The leading cell for an agent row, animated when the agent is actively doing
/// something. A `running` agent fills (working) or sparkles (thinking, when the
/// slider is in `plan`); every other state is the static [`status_glyph`]. Stall
/// is already folded into `Failed` upstream, so it falls through to the static
/// `!`.
pub(super) fn agent_glyph(
    status: AgentStatus,
    posture: Option<PermissionPosture>,
    animation_phase: u64,
) -> &'static str {
    match status {
        AgentStatus::Running if posture == Some(PermissionPosture::Plan) => {
            thinking_glyph(animation_phase)
        }
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
    }
}

/// Style for an agent row's leading cell. A running agent's working spinner and
/// its plan-mode thinking sparkle both paint in Claude clay, so the live head
/// aligns with the agent's own UI; every other state takes its [`status_style`].
pub(super) fn agent_style(theme: &Theme, status: AgentStatus) -> Style {
    if status == AgentStatus::Running {
        return theme.style(ORANGE, Modifier::empty());
    }
    status_style(theme, status)
}

/// Style for an agent row's leading glyph. Both attention states — `?` waiting
/// and `!` failed — rest in bold yellow ("a human is needed here") and escalate
/// to bold red once the row has gone unanswered past `redden_secs` (the
/// configurable neglect window), so a fresh ask reads calm-urgent and a
/// long-ignored one visibly heats up. Every calm state keeps its resting
/// [`agent_style`] tone.
pub(super) fn attention_glyph_style(
    theme: &Theme,
    status: AgentStatus,
    age_secs: i64,
    redden_secs: i64,
) -> Style {
    if matches!(status, AgentStatus::Waiting | AgentStatus::Failed) {
        let color = if age_secs >= redden_secs {
            Color::Red
        } else {
            Color::Yellow
        };
        theme.style(color, Modifier::BOLD)
    } else {
        agent_style(theme, status)
    }
}

/// Human label for the permission posture. `Default` is the omitted baseline,
/// so it returns `None` and disappears from the row. `Unknown` is also
/// suppressed — an unparseable mode word is not a warning surface. `plan` shows
/// in every state (like `auto`/`yolo`): the thinking sparkle only fires while
/// `running`, so the pill is what keeps a plan-slider tab legible when it is
/// idle or waiting.
pub(super) fn posture_pill(posture: PermissionPosture) -> Option<&'static str> {
    match posture {
        PermissionPosture::Default | PermissionPosture::Unknown => None,
        PermissionPosture::Plan => Some("plan"),
        PermissionPosture::Auto => Some("auto"),
        PermissionPosture::Yolo => Some("yolo"),
    }
}

/// Posture pills carry a permission-heat gradient, so a row's blast radius
/// reads at a glance: `plan` is the cautious read-only posture (calm blue),
/// `auto` edits within the sandbox (amber), `yolo` bypasses every gate (bold
/// red — the security surface, loud even when every other capability token
/// dims). `Default`/`Unknown` carry no pill, so they never reach here.
pub(super) fn posture_style(theme: &Theme, posture: PermissionPosture) -> Style {
    match posture {
        PermissionPosture::Plan => theme.style(Color::Blue, Modifier::empty()),
        PermissionPosture::Auto => theme.style(Color::Yellow, Modifier::empty()),
        PermissionPosture::Yolo => theme.style(Color::Red, Modifier::BOLD),
        PermissionPosture::Default | PermissionPosture::Unknown => theme.dim(),
    }
}

/// Token-composition glyphs for the expanded card's token line: a diamond for
/// the cumulative total, the directional arrows for input read in / output
/// generated, and a filled ring for the cached reads. Aggregate sites (the
/// cockpit, the provider stats) use [`TOKENS_TOTAL`] alone.
pub(super) const TOKENS_TOTAL: &str = "◇";
pub(super) const TOKENS_IN: &str = "↘";
pub(super) const TOKENS_OUT: &str = "↗";
pub(super) const TOKENS_CACHED: &str = "◌";

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

/// The context **bar's** tone — the calm green / amber / red severity ramp from
/// the shared [`severity_tier`]. Decides whether the bar shows its composition
/// (only while calm-green) or goes solid, and the solid color once it warns.
pub(super) fn context_severity_color(percent: u8, used_tokens: Option<u64>) -> Color {
    match severity_tier(percent, used_tokens) {
        0 => Color::Green,
        1 => Color::Yellow,
        _ => Color::Red,
    }
}

/// The shared usage tier (0 calm / 1 warn / 2 alarm) behind both the bar's
/// severity color and the `▣` glyph's tone: the worse of the fill-percentage ramp
/// (≤40 / ≤75 / above) and the absolute-token overlay (≤200k / ≤400k / above), so
/// a large-window model green by percentage still climbs by sheer volume.
fn severity_tier(percent: u8, used_tokens: Option<u64>) -> u8 {
    let by_percent = match percent.min(100) {
        0..=40 => 0u8,
        41..=75 => 1,
        _ => 2,
    };
    let by_tokens = match used_tokens.unwrap_or(0) {
        0..=200_000 => 0,
        200_001..=400_000 => 1,
        _ => 2,
    };
    by_percent.max(by_tokens)
}

/// The context meter's `▣` glyph tone — driven by *total* window usage, never by
/// which composition segment fills the most cells. The calm tier reads **blue**
/// ("cold — plenty of headroom"), not green, so a barely-used window whose bar is
/// dominated by amber cache-writes still flags its `▣` blue; it warms to amber
/// then red only as the window genuinely fills. Decoupling glyph from bar is
/// deliberate: the bar shows *where* the tokens went, the glyph *how full* it is.
pub(super) fn ctx_glyph_color(percent: u8, used_tokens: Option<u64>) -> Color {
    match severity_tier(percent, used_tokens) {
        0 => Color::Blue,
        1 => Color::Yellow,
        _ => Color::Red,
    }
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
/// `remaining_pct` of the width in solid `█`, the rest a light `░` track, with
/// no brackets. A full bar means budget *left*: it shortens as the window is
/// spent, and the reset countdown beside it says when it refills. Ramps green →
/// yellow → red by how much remains, so a near-spent window reddens regardless
/// of which window it is. At 0% remaining — the budget fully spent — the whole
/// empty track turns red, so an exhausted window can never be mistaken for a
/// faint untouched one.
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
    two_tone_bar(
        theme,
        filled_cells(remaining_pct, width),
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

/// The unmetered ("infinite") bar: a full-width empty `▱` track in the same faint
/// tone as a drained mana bar's track, so an API-key account aligns with the
/// metered `5h`/`7d` bars and reads as "no meter to spend." The brand color and
/// the meaning ride the `∞` icon in the label slot; the track stays faint so it
/// never competes with a real draining bar. Under `NO_COLOR` the unbroken `▱`
/// run reads as an empty track by shape.
pub(super) fn infinite_bar_spans(theme: &Theme, width: usize) -> Vec<Span<'static>> {
    vec![Span::styled(
        std::iter::repeat_n(MANA_TRACK, width.max(1)).collect::<String>(),
        theme.faint(),
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

/// Aggregate token total behind the `◇` diamond (`◇ 12.4k`, `◇ 523`) — the
/// cockpit and provider stats read the cumulative total alone. The diamond is a
/// soft-violet icon; the value stays dim so the glyph reads as a colored marker,
/// not noise. Display-only, never a decision driver.
pub(super) fn tokens_label(theme: &Theme, total: u64) -> Vec<Span<'static>> {
    vec![
        Span::styled(TOKENS_TOTAL, theme.style(Color::Magenta, Modifier::empty())),
        Span::styled(format!(" {}", tokens_short(total)), theme.dim()),
    ]
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
mod tests {
    use super::*;

    /// `NO_COLOR` strips the green→amber→red ramp, but the heavy/light weight
    /// split still spells the meter — the `━`/`─` shape carries the reading by
    /// itself, without any label.
    #[test]
    fn gauge_under_no_color_reads_by_shape() {
        let theme = Theme::fixed(true);
        let spans = gauge_spans(&theme, Color::Green, 60, 5);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "━━━──");
        for span in &spans {
            assert!(
                span.style.fg.is_none(),
                "NO_COLOR theme must not emit fg color: {span:?}"
            );
        }
    }

    /// Fill rounds to the nearest whole cell: 38% of ten cells is 3.8, so four
    /// heavy cells then a light track. At full width the bar has cells to spare,
    /// so whole-cell resolution reads smoothly without a fractional edge.
    #[test]
    fn gauge_rounds_fill_to_whole_cells() {
        let theme = Theme::fixed(true);
        let spans = gauge_spans(&theme, Color::Green, 38, 10);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "━━━━──────");
    }

    /// At 0% the bar is an unbroken light track, so a "no progress" reading is
    /// the same full-width shape as a started one rather than a blank.
    #[test]
    fn gauge_zero_percent_is_all_track() {
        let theme = Theme::fixed(true);
        let spans = gauge_spans(&theme, Color::Green, 0, 5);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "─────");
    }

    /// At 100% the heavy rule fills the whole width and leaves no track.
    #[test]
    fn gauge_full_has_no_track() {
        let theme = Theme::fixed(true);
        let spans = gauge_spans(&theme, Color::Red, 100, 5);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "━━━━━");
    }

    /// The segmented bar fills the same run as the plain gauge, split into
    /// colored sub-runs whose cell counts sum to the filled total. Under
    /// `NO_COLOR` the segments merge into one heavy run — the shape still reads.
    #[test]
    fn segmented_gauge_sums_to_filled_and_merges_under_no_color() {
        let theme = Theme::fixed(true);
        let segments = [
            (8_000_u64, Color::Green),
            (5_000, Color::Cyan),
            (2_000, Color::Blue),
        ];
        let spans = segmented_gauge_spans(&theme, &segments, Color::Green, 60, 10);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        // 60% of 10 = 6 filled; segments apportion 6 → 3/2/1; then a 4-cell track.
        assert_eq!(text, "━━━━━━────");
        let filled = text.chars().filter(|c| *c == '━').count();
        assert_eq!(filled, 6);
        for span in &spans {
            assert!(span.style.fg.is_none());
        }
    }

    /// With nothing to break down (all-zero weights) the segmented bar is just
    /// the plain single-color gauge.
    #[test]
    fn segmented_gauge_falls_back_with_zero_weights() {
        let theme = Theme::fixed(true);
        let spans = segmented_gauge_spans(
            &theme,
            &[(0, Color::Green), (0, Color::Cyan)],
            Color::Green,
            50,
            4,
        );
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "━━──");
    }

    /// The context tone takes the worse of two severities: the fill-percentage
    /// ramp and an absolute-token overlay (pricey past 200k, dear past 400k), so
    /// a large-window model green by percentage still warns by volume.
    #[test]
    fn context_severity_takes_the_worse_of_percent_and_tokens() {
        // Low fill, low tokens: calm green.
        assert_eq!(context_severity_color(20, Some(50_000)), Color::Green);
        // The percentage ramp alone still reddens a full window.
        assert_eq!(context_severity_color(60, Some(10_000)), Color::Yellow);
        assert_eq!(context_severity_color(80, Some(10_000)), Color::Red);
        // Green by percentage, but the token volume escalates it.
        assert_eq!(context_severity_color(20, Some(250_000)), Color::Yellow);
        assert_eq!(context_severity_color(20, Some(500_000)), Color::Red);
        // The worse severity wins regardless of which axis it comes from.
        assert_eq!(context_severity_color(10, Some(450_000)), Color::Red);
        // No token reading falls back to the percentage ramp alone.
        assert_eq!(context_severity_color(80, None), Color::Red);
        assert_eq!(context_severity_color(10, None), Color::Green);
    }

    /// The `▣` glyph follows total usage on a blue → amber → red ramp (no green),
    /// independent of the bar's composition: a calm window's glyph is blue even
    /// when its bar is amber cache-write dominant, and it warms only as the window
    /// genuinely fills — including the absolute-token overlay.
    #[test]
    fn ctx_glyph_color_is_calm_blue_then_warms() {
        assert_eq!(ctx_glyph_color(0, None), Color::Blue);
        assert_eq!(ctx_glyph_color(16, Some(40_000)), Color::Blue);
        assert_eq!(ctx_glyph_color(40, Some(200_000)), Color::Blue);
        assert_eq!(ctx_glyph_color(60, None), Color::Yellow);
        assert_eq!(ctx_glyph_color(10, Some(250_000)), Color::Yellow); // token overlay
        assert_eq!(ctx_glyph_color(90, None), Color::Red);
        assert_eq!(ctx_glyph_color(10, Some(500_000)), Color::Red);
    }

    /// Largest-remainder apportionment always sums to the requested total.
    #[test]
    fn apportion_sums_to_total() {
        assert_eq!(apportion([3, 1, 1], 5), vec![3, 1, 1]);
        assert_eq!(apportion([1, 1, 1], 4).iter().sum::<usize>(), 4);
        assert_eq!(apportion([0, 0], 3), vec![0, 0]);
    }

    /// The mana bar drains (filled = remaining) in the segmented `▰`/`▱` style
    /// and reads by that fill/hollow shape under `NO_COLOR`; its color ramps
    /// green → yellow → red by how much budget is left — one ramp for both the
    /// 5-hour and weekly windows.
    #[test]
    fn mana_bar_drains_and_ramps() {
        let plain = Theme::fixed(true);
        let spans = mana_bar_spans(&plain, 70, 10);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "▰▰▰▰▰▰▰▱▱▱");
        for span in &spans {
            assert!(span.style.fg.is_none());
        }

        let lit = Theme::fixed(false);
        let fg = |remaining| mana_bar_spans(&lit, remaining, 10)[0].style.fg.unwrap();
        // Green when full, amber mid-drain, red nearly spent.
        assert_eq!(fg(80), Color::Indexed(108));
        assert_eq!(fg(40), Color::Indexed(179));
        assert_eq!(fg(10), Color::Indexed(167));
    }

    /// A fully spent window (0% remaining) is a full-width *empty* `▱` track —
    /// never a `▰` fill — painted red, so "used up" never reads as the faint
    /// untouched track a plain absent-fill would leave. The reset-time text is a
    /// separate span the row owns, so it stays unalarmed; only the bar reddens.
    #[test]
    fn mana_bar_spent_is_a_full_width_red_empty_track() {
        let plain = Theme::fixed(true);
        let spans = mana_bar_spans(&plain, 0, 10);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        // Still an empty track (no `▰`), spanning the full width as one run.
        assert_eq!(text, "▱▱▱▱▱▱▱▱▱▱");
        assert_eq!(spans.len(), 1);
        // Under NO_COLOR the red is suppressed; the empty-track shape still reads.
        assert!(spans[0].style.fg.is_none());

        // With color on, the spent track shares the mana ramp's red — not the
        // faint track tone a non-spent drain leaves behind.
        let lit = Theme::fixed(false);
        let spent = mana_bar_spans(&lit, 0, 10);
        assert_eq!(spent[0].style.fg, Some(Color::Indexed(167)));
        assert_ne!(spent[0].style.fg, lit.faint().fg);
    }

    /// The infinite bar is a full-width empty `▱` track in the same faint tone as
    /// a drained mana bar's track — so an unmetered (API-key) account aligns with
    /// the metered bars and reads as "no meter to spend." Under `NO_COLOR` the
    /// unbroken `▱` run reads as an empty track by shape.
    #[test]
    fn infinite_bar_is_an_empty_faint_track() {
        let plain = Theme::fixed(true);
        let spans = infinite_bar_spans(&plain, 8);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "▱▱▱▱▱▱▱▱");
        for span in &spans {
            assert!(span.style.fg.is_none());
        }

        // With color on it shares the faint track tone, not a brand fill.
        let lit = Theme::fixed(false);
        let spans = infinite_bar_spans(&lit, 8);
        assert_eq!(spans[0].style.fg, lit.faint().fg);
    }

    /// Todo dots use the same fill/empty grammar as the gauge — the dot
    /// count plus the `n/m` label survive `NO_COLOR`.
    #[test]
    fn todo_under_no_color_reads_by_shape_and_label() {
        let theme = Theme::fixed(true);
        let spans = todo_spans(&theme, 3, 5);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "●●●○○ 3/5");
        for span in &spans {
            assert!(span.style.fg.is_none());
        }
    }

    /// Posture pills ramp by permission heat: `plan` calm blue, `auto` amber,
    /// `yolo` bold red. `Default`/`Unknown` carry no pill but still resolve to a
    /// dim baseline.
    #[test]
    fn posture_style_ramps_by_permission_heat() {
        let theme = Theme::fixed(false);
        assert_eq!(
            posture_style(&theme, PermissionPosture::Plan).fg,
            Some(Color::Indexed(75))
        );
        assert_eq!(
            posture_style(&theme, PermissionPosture::Auto).fg,
            Some(Color::Indexed(179))
        );
        let yolo = posture_style(&theme, PermissionPosture::Yolo);
        assert_eq!(yolo.fg, Some(Color::Indexed(167)));
        assert!(yolo.add_modifier.contains(Modifier::BOLD));
    }

    /// Diff stats fall back to the numbers when color is stripped; the
    /// `+`/`-` prefixes still distinguish the two counts.
    #[test]
    fn diff_under_no_color_keeps_signed_numbers() {
        let theme = Theme::fixed(true);
        let spans = diff_spans(&theme, 127, 43);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "+127 -43");
        for span in &spans {
            assert!(span.style.fg.is_none());
        }
    }

    /// The attention glyph reddens only past the 30-minute neglect window, and
    /// only for the `waiting`/`failed` states; a fresh attention row and every
    /// calm state keep their resting tone, however old.
    #[test]
    fn attention_glyph_is_yellow_until_the_neglect_window_then_red() {
        let theme = Theme::fixed(false);
        let red = theme.style(Color::Red, Modifier::BOLD).fg;
        let yellow = theme.style(Color::Yellow, Modifier::BOLD).fg;
        let redden = 30 * 60;

        // Both attention states rest yellow while fresh — `!` no longer starts
        // red; it earns red only by going unanswered.
        for status in [AgentStatus::Waiting, AgentStatus::Failed] {
            let fresh = attention_glyph_style(&theme, status, 5 * 60, redden);
            assert_eq!(fresh.fg, yellow);
            assert!(fresh.add_modifier.contains(Modifier::BOLD));
            let stale = attention_glyph_style(&theme, status, 31 * 60, redden);
            assert_eq!(stale.fg, red);
            assert!(stale.add_modifier.contains(Modifier::BOLD));
        }
        // The threshold is honoured: a shorter window reddens sooner.
        assert_eq!(
            attention_glyph_style(&theme, AgentStatus::Waiting, 6 * 60, 5 * 60).fg,
            red
        );
        // Calm states never redden, however old — they take their plain style.
        assert_eq!(
            attention_glyph_style(&theme, AgentStatus::Idle, 60 * 60, redden).fg,
            agent_style(&theme, AgentStatus::Idle).fg
        );
        assert_eq!(
            attention_glyph_style(&theme, AgentStatus::Running, 60 * 60, redden).fg,
            agent_style(&theme, AgentStatus::Running).fg
        );
    }

    /// Each animation cycles through its frames and wraps, so the phase can grow
    /// without bound.
    #[test]
    fn animations_cycle_and_wrap() {
        for (phase, expected) in WORKING_FRAMES.iter().enumerate() {
            assert_eq!(working_glyph(phase as u64), *expected);
        }
        assert_eq!(
            working_glyph(WORKING_FRAMES.len() as u64),
            WORKING_FRAMES[0]
        );
        assert_eq!(
            thinking_glyph(THINKING_FRAMES.len() as u64),
            THINKING_FRAMES[0]
        );
        assert_eq!(
            resolver_glyph(RESOLVER_FRAMES.len() as u64),
            RESOLVER_FRAMES[0]
        );
        // The phase can grow without bound and still indexes a frame.
        assert_eq!(
            working_glyph(u64::MAX),
            WORKING_FRAMES[(u64::MAX % WORKING_FRAMES.len() as u64) as usize]
        );
    }

    /// A running agent animates the working fill; with a `plan` posture it
    /// sparkles; a stalled agent (folded to `Failed` upstream) and every other
    /// state takes the static glyph, regardless of phase.
    #[test]
    fn agent_glyph_animates_only_active_states() {
        let acting = Some(PermissionPosture::Default);
        let planning = Some(PermissionPosture::Plan);
        assert_eq!(
            agent_glyph(AgentStatus::Running, acting, 2),
            WORKING_FRAMES[2]
        );
        assert_eq!(
            agent_glyph(AgentStatus::Running, planning, 2),
            THINKING_FRAMES[2]
        );
        // A plan posture on a non-running agent never sparkles — the slider is
        // sticky, but the sparkle is the running-state indicator.
        assert_eq!(agent_glyph(AgentStatus::Idle, planning, 2), "○");
        assert_eq!(agent_glyph(AgentStatus::Waiting, acting, 2), "?");
        assert_eq!(agent_glyph(AgentStatus::Failed, acting, 2), "!");
        assert_eq!(agent_glyph(AgentStatus::Idle, acting, 2), "○");
        assert_eq!(agent_glyph(AgentStatus::Success, acting, 2), "✓");
    }
}
