//! Semantic sidebar vocabulary: the canonical status glyphs, posture pills,
//! and the gauge / spinner / pulse glyph helpers.
//!
//! Every meter in the sidebar — context-window %, todo progress, diff stats —
//! renders through the same vocabulary so they read as siblings, not as
//! one-off widgets (see [the sidebar grammar](../../../docs/internals/sidebar.md)).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use rimz::feed::{AgentStatus, PermissionPosture};

use super::theme::Theme;

/// The static status glyph — used for the legend, the worktree tally, the
/// attention line, and as the leading cell for every non-animated state. The
/// shape carries the status under `NO_COLOR`; color reinforces it. `Running`
/// returns its mid-fill frame `◕` as the still fallback (distinct from idle
/// `◌`, a todo `●`, and a todo `○`); the *animated* working/thinking cells live
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

/// Working: a circle filling and emptying. Spans the most time of any state, so
/// it is the calm motion the eye learns to ignore until something changes. The
/// fill never settles on idle `◌`, so a frozen frame still reads as "working".
const WORKING_FRAMES: [&str; 8] = ["○", "◔", "◑", "◕", "●", "◕", "◑", "◔"];

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

/// Style for an agent row's leading cell: plan-mode thinking is cyan to set it
/// apart from the green working fill; everything else takes its [`status_style`].
pub(super) fn agent_style(theme: &Theme, status: AgentStatus, plan_mode: bool) -> Style {
    if status == AgentStatus::Running && plan_mode {
        return theme.style(Color::Cyan, Modifier::empty());
    }
    status_style(theme, status)
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

/// Heavy `━` for the bar's filled run, light `─` for the remaining track. The
/// weight difference — not just the color — carries the meter, so the bar still
/// reads with color off.
const BAR_FILLED: char = '━';
const BAR_TRACK: char = '─';

/// Full-width context bar: a thin rule whose filled run grows left-to-right and
/// ramps green → amber → red by value. Drawn as its own line that underlines
/// the model name, it starts at the same column on every agent, so the bars
/// line up with no alignment bookkeeping. There is no label — the heavy run
/// against the light track is the whole meter, and the weight split keeps it
/// legible under `NO_COLOR`.
pub(super) fn gauge_spans(theme: &Theme, percent: u8, width: usize) -> Vec<Span<'static>> {
    let percent = percent.min(100);
    let width = width.max(1);
    // Nearest-cell fill: 0% stays an unbroken track, 100% fills the whole width.
    let filled = ((percent as usize) * width + 50) / 100;
    let color = match percent {
        0..=40 => Color::Green,
        41..=75 => Color::Yellow,
        _ => Color::Red,
    };
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
            theme.dim(),
        ));
    }
    spans
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
    let text = if total >= 1_000_000 {
        let m = total as f64 / 1_000_000.0;
        format!("{m:.1}M tok")
    } else if total >= 1_000 {
        let k = total as f64 / 1_000.0;
        format!("{k:.1}k tok")
    } else {
        format!("{total} tok")
    };
    Span::styled(text, theme.dim())
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
