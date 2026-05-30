//! Semantic sidebar vocabulary: the canonical status glyphs, posture pills,
//! and the gauge / spinner / pulse glyph helpers.
//!
//! Every meter in the sidebar — context-window %, todo progress, diff stats —
//! renders through the same vocabulary so they read as siblings, not as
//! one-off widgets (see [the sidebar grammar](../../../docs/internals/sidebar.md)).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use rimz::feed::{AgentStatus, PermissionPosture, STALL_WINDOW_SECS};

use super::fmt::tokens_short;
use super::theme::{ORANGE, Theme};

/// The static status glyph — used for the legend, the worktree tally, the
/// attention line, and as the leading cell for every non-animated state. The
/// shape carries the status under `NO_COLOR`; color reinforces it. `Running`
/// returns a representative working frame `⢿` as the still fallback (distinct
/// from idle `◌`, a todo `●`, and a todo `○`); the *animated* working/thinking cells live
/// in [`working_glyph`]/[`thinking_glyph`]. A `running` agent that has gone
/// silent past the stall window is projected to `Failed` upstream, so it reads
/// here as the attention `!` — there is no separate stalled glyph.
pub(super) fn status_glyph(status: AgentStatus) -> &'static str {
    match status {
        // `?` needs your answer; `!` needs a look (a failed turn or a wedged
        // agent). These two carry every attention state.
        AgentStatus::Waiting => "?",
        AgentStatus::Failed => "!",
        AgentStatus::Running => WORKING_FRAMES[3],
        AgentStatus::Idle => "◌",
        AgentStatus::Success => "✓",
    }
}

/// Working: a braille spinner cycling its dots. Spans the most time of any
/// state, so it is the steady motion the eye learns to ignore until something
/// changes. No frame matches idle `◌`, so a frozen frame still reads as "working".
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
/// something. A `running` agent fills (working) or sparkles (thinking, in plan
/// mode); every other state is the static [`status_glyph`]. Stall is already
/// folded into `Failed` upstream, so it falls through to the static `!`.
pub(super) fn agent_glyph(
    status: AgentStatus,
    plan_mode: bool,
    animation_phase: u64,
) -> &'static str {
    match status {
        AgentStatus::Running if plan_mode => thinking_glyph(animation_phase),
        AgentStatus::Running => working_glyph(animation_phase),
        other => status_glyph(other),
    }
}

pub(super) fn status_style(theme: &Theme, status: AgentStatus) -> Style {
    match status {
        AgentStatus::Waiting => theme.style(Color::Yellow, Modifier::BOLD),
        AgentStatus::Failed => theme.style(Color::Red, Modifier::BOLD),
        AgentStatus::Running => theme.style(Color::Green, Modifier::empty()),
        AgentStatus::Idle => theme.dim(),
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

/// Color for a row's activity age, ramped by how stale it is. For the attention
/// states (`waiting` / `failed`) the age is a neglect timer: quiet while fresh,
/// amber once it has sat a couple of minutes, and red + bold past the
/// [`STALL_WINDOW_SECS`] (10-minute) window — so a long-ignored ask visibly
/// heats up. Idle and done are calm: their age is informational, never an alarm,
/// so it stays dim. The working states never call this — their head animates
/// and their age is suppressed.
pub(super) fn age_style(theme: &Theme, status: AgentStatus, age_secs: i64) -> Style {
    const WARM_SECS: i64 = 2 * 60;
    match status {
        AgentStatus::Waiting | AgentStatus::Failed => {
            if age_secs >= STALL_WINDOW_SECS {
                theme.style(Color::Red, Modifier::BOLD)
            } else if age_secs >= WARM_SECS {
                theme.style(Color::Yellow, Modifier::empty())
            } else {
                theme.dim()
            }
        }
        _ => theme.dim(),
    }
}

/// Human label for the permission posture. `Default` is the omitted baseline,
/// so it returns `None` and disappears from the row. `Unknown` is also
/// suppressed — an unparseable mode word is not a warning surface.
pub(super) fn posture_pill(posture: PermissionPosture) -> Option<&'static str> {
    match posture {
        PermissionPosture::Default | PermissionPosture::Unknown => None,
        PermissionPosture::Auto => Some("auto"),
        PermissionPosture::Yolo => Some("yolo"),
    }
}

/// `yolo` is the security surface — keep it warn-colored and bold even when
/// every other capability token dims. `auto` is informational and dim.
pub(super) fn posture_style(theme: &Theme, posture: PermissionPosture) -> Style {
    match posture {
        PermissionPosture::Yolo => theme.style(Color::Yellow, Modifier::BOLD),
        _ => theme.dim(),
    }
}

/// Heavy `━` for a bar's filled run, light `─` for the remaining track. The
/// weight difference — not just the color — carries the meter, so every bar
/// still reads with color off. One glyph pair for all three meters (context
/// gauge and the two draining budget bars), so they read as one aligned family.
const BAR_FILLED: char = '━';
const BAR_TRACK: char = '─';

/// Filled-cell count for `percent` of `width`, to the nearest whole cell: 0%
/// stays an unbroken track, 100% fills the whole width.
fn filled_cells(percent: u8, width: usize) -> usize {
    ((percent.min(100) as usize) * width.max(1) + 50) / 100
}

/// A single-color rule bar: `filled` heavy cells, then a light track out to
/// `width`. The shared shape behind the context gauge and the draining budget
/// bars — color and fill amount differ per meter, the rule shape does not.
fn rule_bar(theme: &Theme, filled: usize, width: usize, color: Color) -> Vec<Span<'static>> {
    let width = width.max(1);
    let filled = filled.min(width);
    let mut spans = Vec::with_capacity(2);
    if filled > 0 {
        spans.push(Span::styled(
            std::iter::repeat_n(BAR_FILLED, filled).collect::<String>(),
            theme.style(color, Modifier::empty()),
        ));
    }
    if filled < width {
        spans.push(Span::styled(
            std::iter::repeat_n(BAR_TRACK, width - filled).collect::<String>(),
            theme.faint(),
        ));
    }
    spans
}

/// Context bar: a thin rule whose filled run grows left-to-right as the window
/// fills and ramps green → amber → red by value. The label and value columns
/// live in the renderer's shared bar row; here we paint just the meter.
pub(super) fn gauge_spans(theme: &Theme, percent: u8, width: usize) -> Vec<Span<'static>> {
    let color = match percent.min(100) {
        0..=40 => Color::Green,
        41..=75 => Color::Yellow,
        _ => Color::Red,
    };
    rule_bar(theme, filled_cells(percent, width), width, color)
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
    total_pct: u8,
    width: usize,
) -> Vec<Span<'static>> {
    let width = width.max(1);
    let total_pct = total_pct.min(100);
    let filled = filled_cells(total_pct, width);
    let weight: u64 = segments.iter().map(|(value, _)| *value).sum();
    if filled == 0 || weight == 0 {
        return gauge_spans(theme, total_pct, width);
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

/// A draining budget bar: `remaining_pct` of the width is heavy `━`, the rest a
/// light `─` track — the same rule shape as the context gauge, so the three
/// meters read as one aligned family. Opposite the gauge, a full bar means
/// budget *left*: it shortens as the window is spent, and the reset countdown
/// beside it says when it refills. Ramps green → yellow → red by how much
/// remains, so a near-spent window reddens regardless of which window it is.
pub(super) fn resource_bar_spans(
    theme: &Theme,
    remaining_pct: u8,
    width: usize,
) -> Vec<Span<'static>> {
    let color = match remaining_pct.min(100) {
        0..=20 => Color::Red,
        21..=50 => Color::Yellow,
        _ => Color::Green,
    };
    rule_bar(theme, filled_cells(remaining_pct, width), width, color)
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

/// Total tokens formatted with a thin unit (`12.4k tok`, `523 tok`). Dim
/// chrome — display-only, never a decision driver.
pub(super) fn tokens_label(theme: &Theme, total: u64) -> Span<'static> {
    Span::styled(format!("{} tok", tokens_short(total)), theme.dim())
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
        let spans = gauge_spans(&theme, 60, 5);
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
        let spans = gauge_spans(&theme, 38, 10);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "━━━━──────");
    }

    /// At 0% the bar is an unbroken light track, so a "no progress" reading is
    /// the same full-width shape as a started one rather than a blank.
    #[test]
    fn gauge_zero_percent_is_all_track() {
        let theme = Theme::fixed(true);
        let spans = gauge_spans(&theme, 0, 5);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "─────");
    }

    /// At 100% the heavy rule fills the whole width and leaves no track.
    #[test]
    fn gauge_full_has_no_track() {
        let theme = Theme::fixed(true);
        let spans = gauge_spans(&theme, 100, 5);
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
        let spans = segmented_gauge_spans(&theme, &segments, 60, 10);
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
        let spans = segmented_gauge_spans(&theme, &[(0, Color::Green), (0, Color::Cyan)], 50, 4);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "━━──");
    }

    /// Largest-remainder apportionment always sums to the requested total.
    #[test]
    fn apportion_sums_to_total() {
        assert_eq!(apportion([3, 1, 1], 5), vec![3, 1, 1]);
        assert_eq!(apportion([1, 1, 1], 4).iter().sum::<usize>(), 4);
        assert_eq!(apportion([0, 0], 3), vec![0, 0]);
    }

    /// The budget bar drains (filled = remaining) and reads by the same heavy/
    /// light rule shape as the context gauge under `NO_COLOR`; its color ramps
    /// green → yellow → red by how much budget is left — one ramp for both the
    /// 5-hour and weekly windows.
    #[test]
    fn resource_bar_drains_and_ramps() {
        let plain = Theme::fixed(true);
        let spans = resource_bar_spans(&plain, 70, 10);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "━━━━━━━───");
        for span in &spans {
            assert!(span.style.fg.is_none());
        }

        let lit = Theme::fixed(false);
        let fg = |remaining| resource_bar_spans(&lit, remaining, 10)[0].style.fg.unwrap();
        // Green when full, amber mid-drain, red nearly spent.
        assert_eq!(fg(80), Color::Indexed(108));
        assert_eq!(fg(40), Color::Indexed(179));
        assert_eq!(fg(10), Color::Indexed(167));
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

    /// The age ramp heats up only for the attention states, and steps
    /// dim → amber → red as it crosses the warm (2m) and stall (10m) thresholds.
    /// Calm states stay dim no matter how old.
    #[test]
    fn age_style_heats_attention_and_leaves_calm_dim() {
        let theme = Theme::fixed(false);
        let dim = theme.dim().fg;
        let amber = theme.style(Color::Yellow, Modifier::empty()).fg;
        let red = theme.style(Color::Red, Modifier::BOLD).fg;

        // Waiting: fresh is dim, a few minutes warms to amber, past 10m reddens.
        assert_eq!(age_style(&theme, AgentStatus::Waiting, 30).fg, dim);
        assert_eq!(age_style(&theme, AgentStatus::Waiting, 5 * 60).fg, amber);
        assert_eq!(age_style(&theme, AgentStatus::Waiting, 11 * 60).fg, red);
        assert!(
            age_style(&theme, AgentStatus::Failed, 11 * 60)
                .add_modifier
                .contains(Modifier::BOLD)
        );
        // Idle and done never alarm.
        assert_eq!(age_style(&theme, AgentStatus::Idle, 11 * 60).fg, dim);
        assert_eq!(age_style(&theme, AgentStatus::Success, 11 * 60).fg, dim);
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

    /// A running agent animates the working fill; in plan mode it sparkles; a
    /// stalled agent (folded to `Failed` upstream) and every other state takes
    /// the static glyph, regardless of phase.
    #[test]
    fn agent_glyph_animates_only_active_states() {
        assert_eq!(
            agent_glyph(AgentStatus::Running, false, 2),
            WORKING_FRAMES[2]
        );
        assert_eq!(
            agent_glyph(AgentStatus::Running, true, 2),
            THINKING_FRAMES[2]
        );
        assert_eq!(agent_glyph(AgentStatus::Waiting, false, 2), "?");
        assert_eq!(agent_glyph(AgentStatus::Failed, false, 2), "!");
        assert_eq!(agent_glyph(AgentStatus::Idle, false, 2), "◌");
        assert_eq!(agent_glyph(AgentStatus::Success, false, 2), "✓");
    }
}
