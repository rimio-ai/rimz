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
        AgentStatus::Running => "▸",
        AgentStatus::Idle => "○",
        AgentStatus::Success => "✓",
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

/// Eight Braille frames — same shape vocabulary every TUI uses for "working".
/// The renderer indexes by the agent's event-pulse counter, so the frame only
/// advances when the agent emits a new lifecycle event. A wedged agent's pulse
/// freezes; a busy one shimmers. One motion, one meaning.
const PULSE_FRAMES: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

/// Pulse glyph for an agent's current event-pulse counter. `0` renders as
/// the first frame, which doubles as the "no activity yet" frame — the agent
/// has been observed but has not emitted any events that bump the counter.
pub(super) fn pulse_glyph(pulse: u64) -> &'static str {
    PULSE_FRAMES[(pulse as usize) % PULSE_FRAMES.len()]
}

/// Segmented-block gauge: a fixed-width bar with the same shape vocabulary
/// every meter shares. The ramp green → amber → red lights up by the value
/// alone (low fill = green, mid = amber, high = red), so the gauge reads under
/// `NO_COLOR` from the count of `▰` cells too. The label `38%` is appended in
/// dim chrome.
pub(super) fn gauge_spans(theme: &Theme, percent: u8, width: usize) -> Vec<Span<'static>> {
    let percent = percent.min(100);
    let width = width.max(1);
    let filled = ((percent as usize) * width + 50) / 100;
    let bar: String = std::iter::repeat_n('▰', filled)
        .chain(std::iter::repeat_n('▱', width.saturating_sub(filled)))
        .collect();
    let color = match percent {
        0..=40 => Color::Green,
        41..=75 => Color::Yellow,
        _ => Color::Red,
    };
    vec![
        Span::styled(bar, theme.style(color, Modifier::empty())),
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

    /// `NO_COLOR` strips the green→amber→red ramp but the `▰`/`▱` count
    /// and the numeric label still spell the meter — the shape carries the
    /// reading by itself.
    #[test]
    fn gauge_under_no_color_reads_by_shape_and_label() {
        let theme = Theme::fixed(true);
        let spans = gauge_spans(&theme, 60, 5);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "▰▰▰▱▱ 60%");
        for span in &spans {
            assert!(
                span.style.fg.is_none(),
                "NO_COLOR theme must not emit fg color: {span:?}"
            );
        }
    }

    /// Even at 0% the bar still paints the empty cells, so a "no progress"
    /// reading is visibly the same shape as a started one rather than a
    /// blank.
    #[test]
    fn gauge_zero_percent_keeps_empty_cells() {
        let theme = Theme::fixed(true);
        let spans = gauge_spans(&theme, 0, 5);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "▱▱▱▱▱ 0%");
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

    /// The pulse glyph is a pure function of the agent's lifecycle-event
    /// counter — no clock input, so a wedged agent's frame is fixed until
    /// the next event lands. Indexing wraps after eight frames so the
    /// counter can grow without bound.
    #[test]
    fn pulse_glyph_is_pure_function_of_event_count() {
        for (count, expected) in PULSE_FRAMES.iter().enumerate() {
            assert_eq!(pulse_glyph(count as u64), *expected);
        }
        // Wraps after eight frames; the counter can grow without overflow.
        assert_eq!(pulse_glyph(8), PULSE_FRAMES[0]);
        assert_eq!(pulse_glyph(u64::MAX), PULSE_FRAMES[(u64::MAX % 8) as usize]);
    }
}
