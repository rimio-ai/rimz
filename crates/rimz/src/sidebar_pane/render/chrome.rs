use crate::SidebarSnapshot;
use jiff::Timestamp;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::Alert;
use super::fmt::age_short;
use super::theme::Theme;

/// The borderless repo header (dashboard L1): the workspace name behind a `⌘`
/// glyph in bold on the left, and — when the project root is known — its
/// home-abbreviated path dim on the right edge of the same line. Identity and
/// location at a glance, on one line so the spend line can sit below it. The
/// path left-truncates with a leading `…` (keeping the meaningful tail) when it
/// can't fit, so the name is never crowded out.
pub(super) fn repo_header_lines(
    theme: &Theme,
    snapshot: &SidebarSnapshot,
    width: usize,
) -> Vec<Line<'static>> {
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let clip = |text: &str| -> String { text.chars().take(width.max(1)).collect() };
    let name = clip(&format!("⌘ {}", snapshot.display_name));
    let name_width = name.chars().count();

    let Some(root) = snapshot.project_root.as_deref() else {
        return vec![Line::styled(name, bold)];
    };
    let path = abbreviate_home(&root.to_string_lossy());
    let path_budget = width.saturating_sub(name_width + 1);
    if path_budget == 0 {
        return vec![Line::styled(name, bold)];
    }
    let path = truncate_left(&path, path_budget);
    let gap = width
        .saturating_sub(name_width + path.chars().count())
        .max(1);
    vec![Line::from(vec![
        Span::styled(name, bold),
        Span::raw(" ".repeat(gap)),
        Span::styled(path, theme.dim()),
    ])]
}

/// Truncate `text` from its left to fit `budget` cells, marking the cut with a
/// leading `…` so the meaningful tail (`…engine/main`) survives. Shorter text
/// passes through unchanged.
pub(super) fn truncate_left(text: &str, budget: usize) -> String {
    let len = text.chars().count();
    if len <= budget {
        return text.to_owned();
    }
    if budget <= 1 {
        return "…".chars().take(budget).collect();
    }
    let tail: String = text.chars().skip(len - (budget - 1)).collect();
    format!("…{tail}")
}

/// Abbreviate a leading `$HOME` to `~` for the path line, so a deep home path
/// reads `~/code/query-engine` rather than spilling the absolute prefix.
pub(super) fn abbreviate_home(path: &str) -> String {
    let home = std::env::var_os("HOME").map(|home| home.to_string_lossy().into_owned());
    abbreviate_under(path, home.as_deref())
}

/// The pure core of [`abbreviate_home`]: collapse a leading `home` prefix to
/// `~`. A path outside `home`, or with no `home`, passes through unchanged.
pub(super) fn abbreviate_under(path: &str, home: Option<&str>) -> String {
    match home {
        Some(home) if !home.is_empty() && path == home => "~".to_owned(),
        Some(home) if !home.is_empty() => match path.strip_prefix(home) {
            Some(rest) if rest.starts_with('/') => format!("~{rest}"),
            _ => path.to_owned(),
        },
        _ => path.to_owned(),
    }
}

/// A full-width `─` hairline rule in the soft gray — the structural seams read
/// at a glance rather than receding into the chrome. Seals the header from
/// the cockpit and brackets the provider dashboard — the structure the dropped
/// border once carried.
pub(super) fn hairline_rule(theme: &Theme, width: usize) -> Line<'static> {
    Line::styled("─".repeat(width.max(1)), theme.soft())
}

pub(super) fn alert_lines(theme: &Theme, alert: &Alert, now: Timestamp) -> Vec<Line<'static>> {
    if alert.is_active() {
        let elapsed = age_short(alert.since, now);
        vec![Line::styled(
            format!("! Sidebar degraded for {elapsed}: {}", alert.reason),
            theme.style(Color::Red, Modifier::BOLD),
        )]
    } else {
        let elapsed = alert
            .recovered_at
            .map(|recovered_at| age_short(recovered_at, now))
            .unwrap_or_else(|| "0s".to_owned());
        vec![Line::styled(
            format!("⚠ last alert {elapsed} ago: {}  ·  x dismiss", alert.reason),
            theme.style(Color::Yellow, Modifier::DIM),
        )]
    }
}

pub(super) fn footer_lines(
    snapshot: &SidebarSnapshot,
    theme: &Theme,
    width: usize,
) -> Vec<Line<'static>> {
    let needs_attention = snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .any(|row| {
            row.status()
                .is_some_and(crate::feed::AgentStatus::is_actionable)
        });
    // Faint chrome — the deepest legible gray, so the footer recedes to pure
    // scaffolding without vanishing. `? for help` is the resting hint; the
    // `␣ next ?!` triage key joins it only when something actually needs you,
    // so the signature key stays discoverable without shouting at rest. The
    // full key model lives behind the `?` overlay.
    let text = if needs_attention {
        "␣ next ?!   ? for help"
    } else {
        "? for help"
    };
    vec![center_line(
        Line::styled(text.to_owned(), theme.faint()),
        width,
    )]
}

/// Center a single line within `width` by prepending padding — used to pin the
/// navigation footer to the bottom edge, horizontally centered. A line already
/// at or past the width is returned unchanged. The line-level style survives
/// the rebuild, so the footer's hairline tone reaches the screen.
pub(super) fn center_line(line: Line<'static>, width: usize) -> Line<'static> {
    let content_width: usize = line
        .spans
        .iter()
        .map(|span| span.content.chars().count())
        .sum();
    let pad = width.saturating_sub(content_width) / 2;
    if pad == 0 {
        return line;
    }
    let style = line.style;
    let mut spans = Vec::with_capacity(line.spans.len() + 1);
    spans.push(Span::raw(" ".repeat(pad)));
    spans.extend(line.spans);
    Line::from(spans).style(style)
}

/// The `?` overlay: keys and the glyph legend, every line in the faint chrome
/// tier — reference material a reader summoned, not live state, so it recedes
/// below the cards it sits under.
pub(super) fn help_lines(theme: &Theme) -> Vec<Line<'static>> {
    let faint = theme.faint();
    vec![
        Line::styled("keys & legend", faint),
        Line::styled("move     j/k rows   J/K worktrees", faint),
        Line::styled("focus    l or ↵     1-9 direct", faint),
        Line::styled("triage   ␣ next ?!  ←/→ accounts", faint),
        Line::styled("filter   q waiting   !/e attention", faint),
        Line::styled("         p paused   d done", faint),
        Line::styled("         w working  o idle   a all", faint),
        Line::styled("system   r reload   x dismiss", faint),
        Line::styled("help     ? close", faint),
        Line::styled("? waiting  ! attention  ⏸ paused", faint),
        Line::styled("✓ done    ⢿ working  ✻ think  ○ idle", faint),
    ]
}
