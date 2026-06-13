use crate::feed::AgentStatus;
use crate::{SidebarLinkFreshness, SidebarLinkHealth, SidebarSnapshot};
use jiff::Timestamp;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::fmt::age_short;
use super::labels::{status_glyph, thinking_glyph};
use super::theme::Theme;
use super::{Alert, GateNotice};
use crate::remote::link::link_badge_heat;

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
        Span::styled(path, theme.muted()),
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

/// A full-width `─` hairline rule in the dedicated rule tone — the structural
/// seam reads as chrome a step below the body text rather than competing with
/// it. Seals the header from the cockpit and brackets the provider dashboard —
/// the structure the dropped border once carried.
pub(super) fn hairline_rule(theme: &Theme, width: usize) -> Line<'static> {
    Line::styled("─".repeat(width.max(1)), theme.rule())
}

pub(super) fn alert_lines(theme: &Theme, alert: &Alert, now: Timestamp) -> Vec<Line<'static>> {
    if alert.is_active() {
        let elapsed = age_short(alert.since, now);
        vec![Line::styled(
            format!("! Sidebar degraded for {elapsed}: {}", alert.reason),
            theme.alarm(Modifier::BOLD),
        )]
    } else {
        let elapsed = alert
            .recovered_at
            .map(|recovered_at| age_short(recovered_at, now))
            .unwrap_or_else(|| "0s".to_owned());
        vec![Line::styled(
            format!("⚠ last alert {elapsed} ago: {}  ·  x dismiss", alert.reason),
            theme.warn(Modifier::DIM),
        )]
    }
}

pub(super) fn truth_notice_lines(
    theme: &Theme,
    notice: &crate::TruthNotice,
    now: Timestamp,
) -> Vec<Line<'static>> {
    let since_ms = notice.since_ms.min(i64::MAX as u64) as i64;
    let since = Timestamp::from_millisecond(since_ms).unwrap_or(now);
    let elapsed = age_short(since, now);
    let noun = if notice.carried == 1 { "pane" } else { "panes" };
    vec![Line::styled(
        format!(
            "⚠ pane source degraded · {} carried {noun} · {elapsed}",
            notice.carried
        ),
        theme.warn(Modifier::DIM),
    )]
}

pub(super) fn gate_notice_lines(theme: &Theme, notice: &GateNotice) -> Vec<Line<'static>> {
    vec![Line::styled(
        format!("⚠ pane updates held · {}", gate_rule_label(notice.rule)),
        theme.warn(Modifier::DIM),
    )]
}

fn gate_rule_label(rule: crate::schema::diag::GateRule) -> &'static str {
    match rule {
        crate::schema::diag::GateRule::FramelessOverFrame => "frameless update",
        crate::schema::diag::GateRule::AgentDemotedToProcess => "agent demotion",
        crate::schema::diag::GateRule::EmptyStampedFrame => "empty pane frame",
    }
}

pub(super) fn footer_lines(
    snapshot: &SidebarSnapshot,
    theme: &Theme,
    width: usize,
) -> Vec<Line<'static>> {
    vec![footer_line(snapshot.link.as_ref(), theme, width)]
}

fn footer_line(link: Option<&SidebarLinkHealth>, theme: &Theme, width: usize) -> Line<'static> {
    const HELP_TEXT: &str = "? for help";

    let badge = link.map(|link| link_badge(link, theme, width));
    let help_text: String = HELP_TEXT.chars().take(width).collect();
    let help = Span::styled(help_text, theme.faint());
    let help_start = right_start(width, span_width(&help)).unwrap_or(0);

    if let Some(line) = positioned_footer_line(badge, Some((help_start, help.clone()))) {
        return line;
    }
    positioned_footer_line(None, Some((help_start, help.clone())))
        .unwrap_or_else(|| Line::from(vec![help]))
}

fn positioned_footer_line(
    badge: Option<Span<'static>>,
    help: Option<(usize, Span<'static>)>,
) -> Option<Line<'static>> {
    let mut placements = Vec::new();
    if let Some(badge) = badge {
        if span_width(&badge) == 0 {
            return None;
        }
        placements.push((0, badge));
    }
    if let Some(help) = help {
        placements.push(help);
    }
    placements.sort_by_key(|(start, _)| *start);

    let mut cursor = 0;
    let mut spans = Vec::new();
    for (start, span) in placements {
        if start <= cursor && !spans.is_empty() {
            return None;
        }
        spans.push(Span::raw(" ".repeat(start.saturating_sub(cursor))));
        cursor = start;
        cursor += span_width(&span);
        spans.push(span);
    }
    Some(Line::from(spans))
}

fn link_badge(link: &SidebarLinkHealth, theme: &Theme, width: usize) -> Span<'static> {
    let mut text = match link.freshness {
        SidebarLinkFreshness::Stale => "⇄ remote ?".to_owned(),
        SidebarLinkFreshness::Fresh => match link.rtt_ms {
            Some(rtt) => format!("⇄ remote {rtt}ms"),
            None => "⇄ remote …".to_owned(),
        },
    };
    if link.freshness == SidebarLinkFreshness::Fresh && link.miss_pct > 10 {
        let loss = format!(" {}%", link.miss_pct);
        if text.chars().count() + loss.chars().count() <= width {
            text.push_str(&loss);
        }
    }
    let text = if text.chars().count() > width {
        text.chars().take(width).collect()
    } else {
        text
    };
    let style = match link.freshness {
        SidebarLinkFreshness::Stale => theme.muted(),
        SidebarLinkFreshness::Fresh => {
            link_badge_heat(link.rtt_ms, link.miss_pct).map_or(theme.body(), |amount| {
                // A critical link (top of the warm tail) keeps its bold weight so
                // it stays loud where color is off.
                let modifier = if amount >= 1.0 {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                };
                theme.style(theme.warm_heat_tone(amount), modifier)
            })
        }
    };
    Span::styled(text, style)
}

fn right_start(width: usize, content_width: usize) -> Option<usize> {
    (content_width <= width).then(|| width - content_width)
}

fn span_width(span: &Span<'_>) -> usize {
    span.content.chars().count()
}

/// Center a single line within `width` by prepending padding. A line already
/// at or past the width is returned unchanged. The line-level style survives
/// the rebuild, so styled chrome stays styled through the helper.
#[cfg(test)]
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
    let waiting = status_glyph(theme, AgentStatus::Waiting);
    let attention = status_glyph(theme, AgentStatus::Failed);
    let paused = status_glyph(theme, AgentStatus::Paused);
    let done = status_glyph(theme, AgentStatus::Success);
    let working = status_glyph(theme, AgentStatus::Running);
    let thinking = thinking_glyph(theme, 0);
    let idle = status_glyph(theme, AgentStatus::Idle);
    vec![
        Line::styled("keys & legend", faint),
        Line::styled("move     j/k rows   J/K worktrees", faint),
        Line::styled("focus    l or ↵     1-9 direct", faint),
        Line::styled("accounts ←/→ tabs", faint),
        Line::styled("filter   u unread   q waiting   !/e attention", faint),
        Line::styled("         p paused   d done      w working", faint),
        Line::styled("         o idle     a all", faint),
        Line::styled("system   r reload   x dismiss", faint),
        Line::styled("help     ? close", faint),
        Line::styled(
            format!("{waiting} waiting  {attention} attention  {paused} paused"),
            faint,
        ),
        Line::styled(
            format!("{done} done    {working} working  {thinking} think  {idle} idle"),
            faint,
        ),
    ]
}
