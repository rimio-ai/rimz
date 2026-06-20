use crate::config::GlyphRole;
use crate::feed::AgentStatus;
use crate::{SidebarLinkFreshness, SidebarLinkHealth, SidebarSnapshot};
use jiff::Timestamp;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::fmt::age_short;
use super::labels::{status_glyph, subagent_glyph, thinking_glyph};
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
    let name = clip(&format!(
        "{} {}",
        theme.glyph(GlyphRole::CockpitWorkspace),
        snapshot.display_name
    ));
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
    Line::styled(
        theme.glyph(GlyphRole::ChromeHairline).repeat(width.max(1)),
        theme.rule(),
    )
}

pub(super) fn alert_lines(theme: &Theme, alert: &Alert, now: Timestamp) -> Vec<Line<'static>> {
    if alert.is_active() {
        let elapsed = age_short(alert.since, now);
        vec![Line::styled(
            format!(
                "{} Sidebar degraded for {elapsed}: {}",
                theme.glyph(GlyphRole::ChromeAlert),
                alert.reason
            ),
            theme.alarm(Modifier::BOLD),
        )]
    } else {
        let elapsed = alert
            .recovered_at
            .map(|recovered_at| age_short(recovered_at, now))
            .unwrap_or_else(|| "0s".to_owned());
        vec![Line::styled(
            format!(
                "{} last alert {elapsed} ago: {}  ·  x dismiss",
                theme.glyph(GlyphRole::ChromeAlert),
                alert.reason
            ),
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
            "{} pane source degraded · {} carried {noun} · {elapsed}",
            theme.glyph(GlyphRole::ChromeAlert),
            notice.carried
        ),
        theme.warn(Modifier::DIM),
    )]
}

pub(super) fn gate_notice_lines(theme: &Theme, notice: &GateNotice) -> Vec<Line<'static>> {
    vec![Line::styled(
        format!(
            "{} pane updates held · {}",
            theme.glyph(GlyphRole::ChromeAlert),
            gate_rule_label(notice.rule)
        ),
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
        SidebarLinkFreshness::Stale => {
            format!("{} remote ?", theme.glyph(GlyphRole::ChromeRemoteLink))
        }
        SidebarLinkFreshness::Fresh => match link.rtt_ms {
            Some(rtt) => format!(
                "{} remote {rtt}ms",
                theme.glyph(GlyphRole::ChromeRemoteLink)
            ),
            None => format!("{} remote …", theme.glyph(GlyphRole::ChromeRemoteLink)),
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
pub(super) fn center_line(line: Line<'static>, width: usize) -> Line<'static> {
    let content_width = line.width();
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

/// The `?` overlay: a which-key style block with action glyphs and the status
/// legend merged into the filter rows. It replaces the card body while open, so
/// the reader gets summoned reference material without losing the pinned cockpit
/// or footer.
pub(super) fn help_lines(
    theme: &Theme,
    focus_key: Option<&str>,
    width: usize,
) -> Vec<Line<'static>> {
    const TITLE: &str = "keys & legend";
    const MIN_FRAME_WIDTH: usize = 22;

    let rows = help_body_rows(theme, focus_key);
    if width < MIN_FRAME_WIDTH {
        return rows
            .into_iter()
            .map(|line| trim_line_to_width(line, width))
            .collect();
    }

    let max_row_width = rows.iter().map(Line::width).max().unwrap_or(0);
    let title_width = UnicodeWidthStr::width(TITLE) + 2;
    let desired_width = (max_row_width + 4).max(title_width + 3);
    framed_box(theme, TITLE, rows, desired_width.min(width))
}

fn help_body_rows(theme: &Theme, focus_key: Option<&str>) -> Vec<Line<'static>> {
    let waiting = status_glyph(theme, AgentStatus::Waiting);
    let attention = status_glyph(theme, AgentStatus::Failed);
    let paused = status_glyph(theme, AgentStatus::Paused);
    let done = status_glyph(theme, AgentStatus::Success);
    let working = status_glyph(theme, AgentStatus::Running);
    let idle = status_glyph(theme, AgentStatus::Idle);
    let thinking = thinking_glyph(theme, 0);
    let delegating = subagent_glyph(theme, 0);
    let mut lines = vec![
        key_row(
            theme,
            GlyphRole::KeysMove,
            "j/k rows   J/K worktrees  g/G ends",
        ),
        key_row(theme, GlyphRole::KeysFocus, "l focus    1-9 direct"),
        key_row(theme, GlyphRole::KeysInbox, "n/N needs-you  (Space = n)"),
        key_row(theme, GlyphRole::KeysRead, "m/M read / unread"),
        key_row(theme, GlyphRole::KeysAccounts, "←/→ account tabs"),
        plain_row(theme, "filter"),
        help_row(
            theme,
            vec![
                Span::raw("  "),
                Span::raw(waiting),
                Span::raw(" q waiting    "),
                Span::raw(attention),
                Span::raw(" e attention"),
            ],
        ),
        help_row(
            theme,
            vec![
                Span::raw("  "),
                Span::raw(paused),
                Span::raw(" p paused     "),
                Span::raw(done),
                Span::raw(" d done"),
            ],
        ),
        help_row(
            theme,
            vec![
                Span::raw("  "),
                Span::raw(working),
                Span::raw(" w working    "),
                Span::raw(idle),
                Span::raw(" o idle"),
            ],
        ),
        plain_row(theme, "  u unread       a all"),
    ];
    // The focus chord is a mux-level binding the renderer can't fire itself; it
    // shows only when one is configured, naming the user's actual key.
    if let Some(key) = focus_key {
        lines.push(plain_row(theme, format!("global  {key} sidebar (toggle)")));
    }
    lines.extend([
        help_row(
            theme,
            vec![
                Span::raw(theme.glyph(GlyphRole::KeysReload).to_owned()),
                Span::raw(" r reload   "),
                Span::raw(theme.glyph(GlyphRole::KeysDismiss).to_owned()),
                Span::raw(" x dismiss   ? close"),
            ],
        ),
        help_row(
            theme,
            vec![
                Span::raw("  "),
                Span::raw(thinking),
                Span::raw(" thinking     "),
                Span::raw(delegating),
                Span::raw(" delegating"),
            ],
        ),
    ]);
    lines
}

fn key_row(theme: &Theme, role: GlyphRole, text: &str) -> Line<'static> {
    help_row(
        theme,
        vec![
            Span::raw(theme.glyph(role).to_owned()),
            Span::raw(" "),
            Span::raw(text.to_owned()),
        ],
    )
}

fn plain_row(theme: &Theme, text: impl Into<String>) -> Line<'static> {
    Line::from(text.into()).style(theme.faint())
}

fn help_row(theme: &Theme, spans: Vec<Span<'static>>) -> Line<'static> {
    Line::from(spans).style(theme.faint())
}

fn framed_box(
    theme: &Theme,
    title: &str,
    rows: Vec<Line<'static>>,
    box_width: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(rows.len() + 2);
    let title = format!(" {title} ");
    let fill = box_width.saturating_sub(2 + UnicodeWidthStr::width(title.as_str()));
    lines.push(
        Line::from(vec![
            rule_span(theme, GlyphRole::ChromeBoxTopLeft),
            Span::styled(title, theme.rule()),
            Span::styled(
                theme.glyph(GlyphRole::ChromeHairline).repeat(fill),
                theme.rule(),
            ),
            rule_span(theme, GlyphRole::ChromeBoxTopRight),
        ])
        .style(theme.faint()),
    );

    let inner_width = box_width.saturating_sub(4);
    for row in rows {
        let row = pad_line_to_width(row, inner_width);
        let mut spans = Vec::with_capacity(row.spans.len() + 4);
        spans.push(rule_span(theme, GlyphRole::ChromeBoxVertical));
        spans.push(Span::raw(" "));
        spans.extend(row.spans);
        spans.push(Span::raw(" "));
        spans.push(rule_span(theme, GlyphRole::ChromeBoxVertical));
        lines.push(Line::from(spans).style(theme.faint()));
    }

    lines.push(
        Line::from(vec![
            rule_span(theme, GlyphRole::ChromeBoxBottomLeft),
            Span::styled(
                theme
                    .glyph(GlyphRole::ChromeHairline)
                    .repeat(box_width.saturating_sub(2)),
                theme.rule(),
            ),
            rule_span(theme, GlyphRole::ChromeBoxBottomRight),
        ])
        .style(theme.faint()),
    );
    lines
}

fn rule_span(theme: &Theme, role: GlyphRole) -> Span<'static> {
    Span::styled(theme.glyph(role).to_owned(), theme.rule())
}

fn pad_line_to_width(line: Line<'static>, width: usize) -> Line<'static> {
    let mut line = trim_line_to_width(line, width);
    let pad = width.saturating_sub(line.width());
    if pad > 0 {
        line.spans.push(Span::raw(" ".repeat(pad)));
    }
    line
}

fn trim_line_to_width(line: Line<'static>, width: usize) -> Line<'static> {
    let style = line.style;
    let mut remaining = width;
    let mut spans = Vec::new();
    for span in line.spans {
        if remaining == 0 {
            break;
        }
        let span_width = UnicodeWidthStr::width(span.content.as_ref());
        if span_width <= remaining {
            remaining -= span_width;
            spans.push(span);
            continue;
        }
        let mut content = String::new();
        let mut used = 0;
        for ch in span.content.chars() {
            let width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + width > remaining {
                break;
            }
            used += width;
            content.push(ch);
        }
        if !content.is_empty() {
            spans.push(Span::styled(content, span.style));
        }
        break;
    }
    Line::from(spans).style(style)
}
