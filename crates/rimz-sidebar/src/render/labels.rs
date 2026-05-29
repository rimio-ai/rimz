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

pub(super) fn status_glyph(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Waiting => "◆",
        AgentStatus::Failed => "✗",
        // A running agent's head is the only animated cell; see `running_glyph`.
        // The static fallback is the first spin frame, so a frozen running row
        // still reads distinctly from idle `○` and from todo `●`.
        AgentStatus::Running => RUNNING_FRAMES[0],
        AgentStatus::Idle => "○",
        AgentStatus::Success => "✓",
    }
}

/// Leading glyph for a running agent's row. The head rotates `◐ ◓ ◑ ◒` so the
/// eye lands on motion, but only while the agent is *fresh* — `fresh` is gated
/// on the agent's last activity (see [`super::sections`]). A stale (wedged or
/// quiet) agent freezes on the first frame, so motion never lies about a hung
/// agent. The rotating set is chosen over a filling circle so no spin frame
/// ever collides with idle `○` or a todo `●`.
const RUNNING_FRAMES: [&str; 4] = ["◐", "◓", "◑", "◒"];

pub(super) fn running_glyph(animation_phase: u64, fresh: bool) -> &'static str {
    if !fresh {
        return RUNNING_FRAMES[0];
    }
    RUNNING_FRAMES[(animation_phase as usize) % RUNNING_FRAMES.len()]
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

/// Fractional cell glyphs (1/8 steps) for the gauge's leading edge, the same
/// trick ratatui's native `Gauge` uses for sub-cell resolution. Index `0` is an
/// empty edge (no partial); `1..=7` are the eighth blocks.
const EIGHTHS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];

/// Smooth fixed-width gauge, modeled on ratatui's `Gauge`: solid `█` cells plus
/// a fractional eighth-block at the fill edge, over a dim `░` track, with the
/// `38%` label in dim chrome. The ramp green → amber → red lights up by the
/// value alone, so the gauge still reads under `NO_COLOR` from the `█`/`░`
/// count — the shape carries the meter without the color.
pub(super) fn gauge_spans(theme: &Theme, percent: u8, width: usize) -> Vec<Span<'static>> {
    let percent = percent.min(100);
    let width = width.max(1);
    // Total fill in eighths-of-a-cell, rounded to the nearest eighth.
    let eighths = ((percent as usize) * width * 8 + 4) / 100;
    let full = (eighths / 8).min(width);
    let partial = eighths % 8;
    let mut fill: String = std::iter::repeat_n('█', full).collect();
    let mut used = full;
    if used < width && partial > 0 {
        fill.push(EIGHTHS[partial]);
        used += 1;
    }
    let track: String = std::iter::repeat_n('░', width - used).collect();
    let color = match percent {
        0..=40 => Color::Green,
        41..=75 => Color::Yellow,
        _ => Color::Red,
    };
    vec![
        Span::styled(fill, theme.style(color, Modifier::empty())),
        Span::styled(track, theme.dim()),
        Span::styled(format!(" {percent}%"), theme.dim()),
    ]
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
/// chrome — telemetry, never a decision driver.
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

    /// `NO_COLOR` strips the green→amber→red ramp but the `█`/`░` count
    /// and the numeric label still spell the meter — the shape carries the
    /// reading by itself.
    #[test]
    fn gauge_under_no_color_reads_by_shape_and_label() {
        let theme = Theme::fixed(true);
        let spans = gauge_spans(&theme, 60, 5);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "███░░ 60%");
        for span in &spans {
            assert!(
                span.style.fg.is_none(),
                "NO_COLOR theme must not emit fg color: {span:?}"
            );
        }
    }

    /// The fractional eighth-block resolves sub-cell fill the way ratatui's
    /// native `Gauge` does: 38% of ten cells is 3.8, so three full `█`, a
    /// partial edge, then a dim `░` track.
    #[test]
    fn gauge_renders_a_fractional_edge_cell() {
        let theme = Theme::fixed(true);
        let spans = gauge_spans(&theme, 38, 10);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "███▊░░░░░░ 38%");
    }

    /// Even at 0% the bar still paints the empty track, so a "no progress"
    /// reading is visibly the same shape as a started one rather than a
    /// blank.
    #[test]
    fn gauge_zero_percent_keeps_empty_cells() {
        let theme = Theme::fixed(true);
        let spans = gauge_spans(&theme, 0, 5);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "░░░░░ 0%");
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

    /// A fresh running agent's head advances with the animation phase and
    /// wraps after four frames so the phase can grow without bound.
    #[test]
    fn running_glyph_spins_while_fresh() {
        for (phase, expected) in RUNNING_FRAMES.iter().enumerate() {
            assert_eq!(running_glyph(phase as u64, true), *expected);
        }
        assert_eq!(running_glyph(4, true), RUNNING_FRAMES[0]);
        assert_eq!(
            running_glyph(u64::MAX, true),
            RUNNING_FRAMES[(u64::MAX % 4) as usize]
        );
    }

    /// Honesty: a stale (wedged or quiet) agent freezes on the first frame no
    /// matter how the animation phase advances, so motion never pretends a
    /// hung agent is working.
    #[test]
    fn running_glyph_freezes_when_stale() {
        for phase in 0..8 {
            assert_eq!(running_glyph(phase, false), RUNNING_FRAMES[0]);
        }
    }
}
