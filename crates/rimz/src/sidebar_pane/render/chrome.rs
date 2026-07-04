use crate::agents::AgentStatus;
use crate::config::{AnimationRole, GlyphRole, SidebarKeys};
use crate::{SidebarLinkFreshness, SidebarLinkHealth, SidebarPresence, SidebarSnapshot};
use jiff::Timestamp;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::fmt::{age_label, age_short};
use super::labels::{role_glyph, status_glyph, status_rest_style};
use super::layout;
use super::theme::Theme;
use super::{Alert, GateNotice};
use crate::remote::link::link_badge_heat;

/// The borderless repo header (dashboard L1): the workspace name behind a `⌘`
/// glyph in the bold `good` green identity tone on the left, and — when the
/// project root is known — its home-abbreviated path dim on the right edge of
/// the same line. Identity and location at a glance, on one line so the spend
/// line can sit below it. The path left-truncates with a leading `…` (keeping
/// the meaningful tail) when it can't fit, so the name is never crowded out.
pub(super) fn repo_header_lines(
    theme: &Theme,
    snapshot: &SidebarSnapshot,
    width: usize,
) -> Vec<Line<'static>> {
    let title = theme.good(Modifier::BOLD);
    let name = layout::clip(
        &format!(
            "{} {}",
            theme.glyph(GlyphRole::CockpitWorkspace),
            snapshot.display_name
        ),
        width.max(1),
    );
    let name_width = layout::text_width(&name);

    let Some(root) = snapshot.project_root.as_deref() else {
        return vec![Line::styled(name, title)];
    };
    let path = abbreviate_home(&root.to_string_lossy());
    let path_budget = width.saturating_sub(name_width + 1);
    if path_budget == 0 {
        return vec![Line::styled(name, title)];
    }
    let path = layout::truncate_left(&path, path_budget);
    let gap = width
        .saturating_sub(name_width + layout::text_width(&path))
        .max(1);
    vec![Line::from(vec![
        Span::styled(name, title),
        Span::raw(" ".repeat(gap)),
        Span::styled(path, theme.muted()),
    ])]
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

fn gate_rule_label(rule: crate::diag::record::GateRule) -> &'static str {
    match rule {
        crate::diag::record::GateRule::FramelessOverFrame => "frameless update",
        crate::diag::record::GateRule::AgentDemotedToProcess => "agent demotion",
        crate::diag::record::GateRule::EmptyStampedFrame => "empty pane frame",
    }
}

pub(super) fn footer_lines(
    snapshot: &SidebarSnapshot,
    theme: &Theme,
    width: usize,
) -> Vec<Line<'static>> {
    vec![footer_line(footer_parts(snapshot, theme, width), width)]
}

#[derive(Clone)]
pub(super) struct FooterParts {
    pub(super) left: Vec<Span<'static>>,
    pub(super) help: Span<'static>,
}

pub(super) fn footer_parts(snapshot: &SidebarSnapshot, theme: &Theme, width: usize) -> FooterParts {
    const HELP_TEXT: &str = "? for help";

    let presence = presence_badge(snapshot.presence, theme, width);
    let link = snapshot
        .link
        .as_ref()
        .map(|link| link_badge(link, theme, width));
    let has_presence = presence.is_some();
    let help_text = layout::clip(HELP_TEXT, width);
    let help = Span::styled(help_text, theme.faint());
    let help_start = width.saturating_sub(help.width());

    let left = footer_left_spans(presence.clone(), link.clone());
    if footer_left_fits(&left, help_start) {
        return FooterParts { left, help };
    }
    if has_presence {
        let left = footer_left_spans(presence, None);
        if footer_left_fits(&left, help_start) {
            return FooterParts { left, help };
        }
    }
    if !has_presence {
        let left = footer_left_spans(None, link);
        if footer_left_fits(&left, help_start) {
            return FooterParts { left, help };
        }
    }
    FooterParts {
        left: Vec::new(),
        help,
    }
}

fn footer_line(parts: FooterParts, width: usize) -> Line<'static> {
    let help_start = width.saturating_sub(parts.help.width());
    positioned_footer_line(parts.left, Some((help_start, parts.help)))
        .unwrap_or_else(|| Line::from(Vec::<Span<'static>>::new()))
}

fn footer_left_fits(left: &[Span<'static>], help_start: usize) -> bool {
    let mut cursor = 0;
    for span in left {
        let width = span.width();
        if width == 0 {
            return false;
        }
        cursor += width;
    }
    help_start > cursor || left.is_empty()
}

fn positioned_footer_line(
    left: Vec<Span<'static>>,
    help: Option<(usize, Span<'static>)>,
) -> Option<Line<'static>> {
    let mut cursor = 0;
    let mut spans = Vec::new();
    for span in left {
        if span.width() == 0 {
            return None;
        }
        cursor += span.width();
        spans.push(span);
    }
    if let Some((start, span)) = help {
        if start <= cursor && !spans.is_empty() {
            return None;
        }
        spans.push(Span::raw(" ".repeat(start.saturating_sub(cursor))));
        spans.push(span);
    }
    Some(Line::from(spans))
}

fn footer_left_spans(
    presence: Option<Span<'static>>,
    link: Option<Span<'static>>,
) -> Vec<Span<'static>> {
    match (presence, link) {
        (Some(presence), Some(link)) => vec![presence, Span::raw("  "), link],
        (Some(presence), None) => vec![presence],
        (None, Some(link)) => vec![link],
        (None, None) => Vec::new(),
    }
}

fn presence_badge(
    presence: Option<SidebarPresence>,
    theme: &Theme,
    width: usize,
) -> Option<Span<'static>> {
    let presence = presence.filter(|presence| presence.shows_badge())?;
    let glyph = theme.glyph(GlyphRole::ChromePresenceAway);
    let mut text = match presence {
        SidebarPresence::Active => return None,
        SidebarPresence::Idle { idle_ms } => {
            let seconds = (idle_ms / 1_000).min(i64::MAX as u64) as i64;
            format!("{glyph} idle · {}", age_label(seconds))
        }
        SidebarPresence::Detached => format!("{glyph} away"),
    };
    text = layout::clip(&text, width);
    Some(Span::styled(text, theme.muted()))
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
        if layout::text_width(&text) + layout::text_width(&loss) <= width {
            text.push_str(&loss);
        }
    }
    let text = layout::clip(&text, width);
    let style = match link.freshness {
        SidebarLinkFreshness::Stale => theme.muted(),
        SidebarLinkFreshness::Fresh => {
            link_badge_heat(link.rtt_ms, link.miss_pct).map_or(theme.body(), |amount| {
                // A critical link (red end of the ramp) keeps its bold weight so
                // it stays loud where color is off.
                let modifier = if amount >= 1.0 {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                };
                theme.style(theme.heat_tone(amount), modifier)
            })
        }
    };
    Span::styled(text, style)
}

/// The `?` popup: a which-key style block with action glyphs and the status
/// legend merged into the filter rows.
pub(super) fn help_lines(
    theme: &Theme,
    focus_key: Option<&str>,
    keys: &SidebarKeys,
    width: usize,
) -> Vec<Line<'static>> {
    const TITLE: &str = "keys & legend";
    const MIN_FRAME_WIDTH: usize = 22;

    let rows = help_body_rows(theme, focus_key, keys);
    if width < MIN_FRAME_WIDTH {
        return rows
            .into_iter()
            .map(|line| borderless_line(line, width))
            .collect();
    }

    let max_row_width = rows.iter().map(Line::width).max().unwrap_or(0);
    let title_width = layout::text_width(TITLE) + 2;
    let desired_width = (max_row_width + 4).max(title_width + 3);
    framed_box(
        theme,
        TITLE,
        rows,
        desired_width.min(width),
        Some("any key to close"),
    )
}

fn help_body_rows(
    theme: &Theme,
    focus_key: Option<&str>,
    keys: &SidebarKeys,
) -> Vec<Line<'static>> {
    let key_rows = vec![
        (
            key_cell(
                theme,
                Some(key_icon(theme, GlyphRole::KeysMove)),
                &format!("{}/{}", chord_label(&keys.down), chord_label(&keys.up)),
                "rows",
            ),
            Some(key_cell(
                theme,
                Some(key_icon(theme, GlyphRole::KeysMove)),
                &format!(
                    "{}/{}",
                    chord_label(&keys.worktree_down),
                    chord_label(&keys.worktree_up)
                ),
                "worktrees",
            )),
        ),
        (
            key_cell(
                theme,
                Some(key_icon(theme, GlyphRole::KeysMove)),
                &format!("{}/{}", chord_label(&keys.top), chord_label(&keys.bottom)),
                "ends",
            ),
            Some(
                key_cell(
                    theme,
                    Some(key_icon(theme, GlyphRole::KeysMove)),
                    &format!(
                        "{}/{}",
                        chord_label(&keys.page_down),
                        chord_label(&keys.page_up)
                    ),
                    "page",
                )
                .into_iter()
                .chain([
                    Span::raw("  "),
                    Span::styled(
                        format!(
                            "{}/{}",
                            chord_label(&keys.screen_top),
                            chord_label(&keys.screen_bottom)
                        ),
                        theme.body().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled("screen".to_owned(), theme.faint()),
                ])
                .collect(),
            ),
        ),
        (
            key_cell(
                theme,
                Some(key_icon(theme, GlyphRole::KeysFocus)),
                "l",
                "focus",
            ),
            Some(key_cell(
                theme,
                Some(key_icon(theme, GlyphRole::KeysFocus)),
                "1-9",
                "direct",
            )),
        ),
        (
            key_cell(
                theme,
                Some(key_icon(theme, GlyphRole::KeysInbox)),
                "n/N",
                "needs-you  (Space = n)",
            ),
            Some(key_cell(
                theme,
                Some(key_icon(theme, GlyphRole::KeysRead)),
                "m/M",
                "read / unread",
            )),
        ),
        (
            key_cell(
                theme,
                Some(key_icon(theme, GlyphRole::KeysAccounts)),
                "←/→",
                "account tabs",
            ),
            Some(key_cell(
                theme,
                Some(key_icon(theme, GlyphRole::KeysReload)),
                "r",
                "reload",
            )),
        ),
        (
            key_cell(
                theme,
                Some(key_icon(theme, GlyphRole::KeysDismiss)),
                "x",
                "dismiss",
            ),
            focus_key.map(|key| key_cell(theme, None, key, "sidebar (toggle)")),
        ),
    ];

    let mut lines = vec![subheader(theme, "keys")];
    lines.extend(two_column(theme, key_rows));
    lines.push(subheader(theme, "filter"));
    lines.extend(two_column(
        theme,
        vec![
            (
                status_cell(theme, AgentStatus::Waiting, "q", "waiting"),
                Some(status_cell(theme, AgentStatus::Failed, "e", "attention")),
            ),
            (
                status_cell(theme, AgentStatus::Paused, "p", "paused"),
                Some(status_cell(theme, AgentStatus::Success, "d", "done")),
            ),
            (
                status_cell(theme, AgentStatus::Running, "w", "working"),
                Some(status_cell(theme, AgentStatus::Idle, "o", "idle")),
            ),
            (
                key_cell(theme, None, "u", "unread"),
                Some(key_cell(theme, None, "a", "all")),
            ),
        ],
    ));
    lines.push(subheader(theme, "legend"));
    lines.extend(two_column(
        theme,
        vec![(
            animation_cell(theme, AnimationRole::Thinking, "thinking"),
            Some(animation_cell(
                theme,
                AnimationRole::Delegating,
                "delegating",
            )),
        )],
    ));
    lines
}

fn chord_label(spec: &str) -> String {
    let Some(token) = spec.split_whitespace().next() else {
        return "?".to_owned();
    };
    let parts = token.split(['+', '-']).collect::<Vec<_>>();
    let Some((base, modifiers)) = parts.split_last() else {
        return "?".to_owned();
    };
    let base = key_label(base.trim());
    let mut label = String::new();
    for modifier in modifiers {
        match modifier.trim().to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "c" => label.push('^'),
            "alt" | "meta" | "m" => label.push_str("M-"),
            _ => {}
        }
    }
    label.push_str(&base);
    label
}

fn key_label(base: &str) -> String {
    match base.to_ascii_lowercase().as_str() {
        "up" => "↑".to_owned(),
        "down" => "↓".to_owned(),
        "left" => "←".to_owned(),
        "right" => "→".to_owned(),
        "pageup" => "PgUp".to_owned(),
        "pagedown" => "PgDn".to_owned(),
        "home" => "Home".to_owned(),
        "end" => "End".to_owned(),
        "enter" => "Enter".to_owned(),
        "space" => "Space".to_owned(),
        _ => base.to_owned(),
    }
}

fn two_column(
    theme: &Theme,
    rows: Vec<(Vec<Span<'static>>, Option<Vec<Span<'static>>>)>,
) -> Vec<Line<'static>> {
    const GAP: usize = 2;

    let left_w = rows
        .iter()
        .filter_map(|(left, right)| right.as_ref().map(|_| layout::spans_width(left)))
        .max()
        .unwrap_or(0);

    rows.into_iter()
        .map(|(mut left, right)| {
            if let Some(right) = right {
                let pad = left_w.saturating_sub(layout::spans_width(&left)) + GAP;
                left.push(Span::raw(" ".repeat(pad)));
                left.extend(right);
            }
            Line::from(left).style(theme.body())
        })
        .collect()
}

fn key_cell(
    theme: &Theme,
    glyph: Option<Span<'static>>,
    keys: &str,
    label: &str,
) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(5);
    if let Some(glyph) = glyph {
        spans.push(glyph);
        spans.push(Span::raw(" "));
    }
    spans.push(Span::styled(
        keys.to_owned(),
        theme.body().add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(label.to_owned(), theme.faint()));
    spans
}

fn status_cell(theme: &Theme, status: AgentStatus, keys: &str, label: &str) -> Vec<Span<'static>> {
    key_cell(theme, Some(status_icon(theme, status)), keys, label)
}

fn animation_cell(theme: &Theme, role: AnimationRole, label: &str) -> Vec<Span<'static>> {
    vec![
        Span::styled(role_glyph(theme, role, 0), animation_style(theme, role)),
        Span::raw(" "),
        Span::styled(label.to_owned(), theme.faint()),
    ]
}

fn subheader(theme: &Theme, text: &str) -> Line<'static> {
    Line::styled(text.to_owned(), theme.faint())
}

fn key_icon(theme: &Theme, role: GlyphRole) -> Span<'static> {
    Span::styled(theme.glyph(role).to_owned(), theme.muted())
}

fn status_icon(theme: &Theme, status: AgentStatus) -> Span<'static> {
    let style = if status == AgentStatus::Idle {
        theme.body()
    } else {
        status_rest_style(theme, status)
    };
    Span::styled(status_glyph(theme, status), style)
}

fn animation_style(theme: &Theme, role: AnimationRole) -> Style {
    theme.animations.natural_color(role).map_or_else(
        || theme.body(),
        |color| theme.style(color, Modifier::empty()),
    )
}

fn framed_box(
    theme: &Theme,
    title: &str,
    rows: Vec<Line<'static>>,
    box_width: usize,
    bottom_caption: Option<&str>,
) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(rows.len() + 2);
    let title = format!(" {title} ");
    let fill = box_width.saturating_sub(2 + layout::text_width(&title));
    lines.push(
        Line::from(vec![
            rule_span(theme, GlyphRole::ChromeBoxTopLeft),
            Span::styled(title, theme.muted()),
            Span::styled(
                theme.glyph(GlyphRole::ChromeHairline).repeat(fill),
                theme.muted(),
            ),
            rule_span(theme, GlyphRole::ChromeBoxTopRight),
        ])
        .style(theme.body()),
    );

    let inner_width = box_width.saturating_sub(4);
    for row in rows {
        let row = layout::pad_line_to(row, inner_width);
        let mut spans = Vec::with_capacity(row.spans.len() + 4);
        spans.push(rule_span(theme, GlyphRole::ChromeBoxVertical));
        spans.push(Span::raw(" "));
        spans.extend(row.spans);
        spans.push(Span::raw(" "));
        spans.push(rule_span(theme, GlyphRole::ChromeBoxVertical));
        lines.push(Line::from(spans).style(theme.body()));
    }

    let inner = box_width.saturating_sub(2);
    let bottom_spans = match bottom_caption {
        Some(caption) => {
            let caption = format!(" {caption} ");
            let caption_width = layout::text_width(&caption);
            if caption_width <= inner {
                let left = (inner - caption_width) / 2;
                let right = inner - caption_width - left;
                vec![
                    rule_span(theme, GlyphRole::ChromeBoxBottomLeft),
                    Span::styled(
                        theme.glyph(GlyphRole::ChromeHairline).repeat(left),
                        theme.muted(),
                    ),
                    Span::styled(caption, theme.muted()),
                    Span::styled(
                        theme.glyph(GlyphRole::ChromeHairline).repeat(right),
                        theme.muted(),
                    ),
                    rule_span(theme, GlyphRole::ChromeBoxBottomRight),
                ]
            } else {
                vec![
                    rule_span(theme, GlyphRole::ChromeBoxBottomLeft),
                    Span::styled(
                        theme.glyph(GlyphRole::ChromeHairline).repeat(inner),
                        theme.muted(),
                    ),
                    rule_span(theme, GlyphRole::ChromeBoxBottomRight),
                ]
            }
        }
        None => vec![
            rule_span(theme, GlyphRole::ChromeBoxBottomLeft),
            Span::styled(
                theme.glyph(GlyphRole::ChromeHairline).repeat(inner),
                theme.muted(),
            ),
            rule_span(theme, GlyphRole::ChromeBoxBottomRight),
        ],
    };
    lines.push(Line::from(bottom_spans).style(theme.body()));
    lines
}

fn rule_span(theme: &Theme, role: GlyphRole) -> Span<'static> {
    Span::styled(theme.glyph(role).to_owned(), theme.muted())
}

fn borderless_line(line: Line<'static>, width: usize) -> Line<'static> {
    if width == 0 {
        return Line::from(layout::trim_spans_to_width(line.spans, width)).style(line.style);
    }
    let style = line.style;
    let mut spans = Vec::with_capacity(line.spans.len() + 1);
    spans.push(Span::raw(" "));
    spans.extend(line.spans);
    layout::pad_line_to(Line::from(spans).style(style), width)
}
