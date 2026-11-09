use crate::agents::AgentStatus;
use crate::config::GlyphRole;
use crate::config::keys::sidebar::{SidebarAction, SidebarChord, SidebarKey, SidebarKeymap};
use crate::pane::RoomMode;
use crate::sidebar_pane::render::labels::value_seam;
use crate::sidebar_pane::view::VisibleRoster;
use crate::store::snapshot::{
    HookDrainerOutageAgent, SidebarLinkFreshness, SidebarLinkHealth, SidebarPresence,
    SidebarSnapshot, SidebarStallItem, SidebarStallPhase, SidebarStallResource, TruthNotice,
};
use jiff::Timestamp;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::fmt::age_short;
use super::labels::{status_glyph, status_rest_style};
use super::theme::{Component, Theme};
use super::{Alert, GateNotice, ViewState, layout};
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
fn abbreviate_home(path: &str) -> String {
    let home = std::env::var_os("HOME").map(|home| home.to_string_lossy().into_owned());
    abbreviate_under(path, home.as_deref())
}

/// The pure core of [`abbreviate_home`]: collapse a leading `home` prefix to
/// `~`. A path outside `home`, or with no `home`, passes through unchanged.
fn abbreviate_under(path: &str, home: Option<&str>) -> String {
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

pub(super) fn alert_lines(
    theme: &Theme,
    alert: &Alert,
    keys: &SidebarKeymap,
    now: Timestamp,
) -> Vec<Line<'static>> {
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
                "{} last alert {elapsed} ago: {}{}",
                theme.glyph(GlyphRole::ChromeAlert),
                alert.reason,
                dismiss_hint(theme, keys)
            ),
            theme.warn(Modifier::DIM),
        )]
    }
}

/// The ` · Esc dismiss` tail of a dismissible notice, from the key bound to
/// `view.cancel`; empty when nothing is bound.
fn dismiss_hint(theme: &Theme, keys: &SidebarKeymap) -> String {
    sequence_label(keys, SidebarAction::ViewCancel)
        .map(|key| format!("{}{key} dismiss", value_seam(theme)))
        .unwrap_or_default()
}

/// A refusal from the last in-pane key, in the bottom chrome until the next key.
pub(super) fn pane_notice_line(theme: &Theme, notice: &str, keys: &SidebarKeymap) -> Line<'static> {
    Line::styled(
        format!(
            "{} {notice}{}",
            theme.glyph(GlyphRole::ChromeAlert),
            dismiss_hint(theme, keys)
        ),
        theme.styled(Component::PaneNotice, Modifier::DIM),
    )
}

/// The bottom key row: the room modes of this pane's viewers on the left,
/// then, for a pending prefix, the keys pressed so far and each continuation
/// key with its short label, right-aligned. The hint is cut from the right
/// first, so the marks keep their place.
pub(super) fn key_row_line(
    theme: &Theme,
    keys: &SidebarKeymap,
    modes: &[RoomMode],
    pending: &[SidebarChord],
    width: usize,
) -> Line<'static> {
    let marks = modes
        .iter()
        .map(|mode| room_mode_label(*mode))
        .collect::<Vec<_>>()
        .join(" ");
    let mut spans = vec![Span::styled(
        marks,
        theme.styled(Component::RoomModeMark, Modifier::BOLD),
    )];
    let marks_width = layout::spans_width(&spans);
    if !pending.is_empty() {
        let hint = key_hint_spans(theme, keys, pending);
        let gap = usize::from(marks_width > 0);
        let room = width.saturating_sub(marks_width + gap);
        let pad = room.saturating_sub(layout::spans_width(&hint));
        spans.push(Span::raw(" ".repeat(gap + pad)));
        spans.extend(layout::trim_spans_to_width(hint, room));
    }
    let used = layout::spans_width(&spans);
    spans.push(Span::raw(" ".repeat(width.saturating_sub(used))));
    Line::from(layout::trim_spans_to_width(spans, width))
}

fn key_hint_spans(
    theme: &Theme,
    keys: &SidebarKeymap,
    pending: &[SidebarChord],
) -> Vec<Span<'static>> {
    let key_style = theme.styled(Component::KeyHint, Modifier::BOLD);
    let mut spans = vec![Span::styled(sequence_text(pending), key_style)];
    for (rest, action) in keys.continuations(pending) {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(sequence_text(rest), key_style));
        spans.push(Span::styled(format!(" {}", action.label()), theme.faint()));
    }
    spans
}

fn room_mode_label(mode: RoomMode) -> &'static str {
    match mode {
        RoomMode::Prefix => "PREFIX",
        RoomMode::Resize => "RESIZE",
        RoomMode::Scrollback => "SCROLL",
        RoomMode::Move => "MOVE",
    }
}

pub(super) fn hook_drainer_alert_lines(
    theme: &Theme,
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
) -> Vec<Line<'static>> {
    if let Some(outage) = &snapshot.hook_drainer_outage {
        let Some(agent) = outage.agents.first() else {
            return Vec::new();
        };
        let elapsed = age_short(outage.since, snapshot.now);
        let label = hook_drainer_card_label(snapshot, agent);
        let cards = if outage.agents.len() == 1 {
            format!("{label}'s card")
        } else {
            format!("{label} and {} other cards", outage.agents.len() - 1)
        };
        return vec![Line::styled(
            format!(
                "{} hook drainer unreachable for {elapsed}{}{cards} may be stale",
                theme.glyph(GlyphRole::ChromeAlert),
                value_seam(theme)
            ),
            theme.alarm(Modifier::BOLD),
        )];
    }
    let Some(alert) = alert else {
        return Vec::new();
    };
    let Some(recovered_at) = alert.recovered_at else {
        return Vec::new();
    };
    let recovered = Alert {
        reason: format!(
            "{} for {}",
            alert.reason,
            age_short(alert.since, recovered_at)
        ),
        ..alert.clone()
    };
    alert_lines(theme, &recovered, &snapshot.keys, snapshot.now)
}

fn hook_drainer_card_label(snapshot: &SidebarSnapshot, outage: &HookDrainerOutageAgent) -> String {
    let row = snapshot
        .agents
        .iter()
        .filter(|agent| agent.kind == outage.agent_kind && agent.name == outage.agent_name)
        .find_map(|agent| {
            snapshot.rows().find(|row| {
                row.is_agent() && row.name == agent.kind && row.id == agent.agent_id.as_str()
            })
        });
    if let Some(row) = row {
        return row.display_name().to_owned();
    }
    if let Some(name) = &outage.agent_name {
        return format!("@{name}");
    }
    crate::agents::spec_by_kind(outage.agent_kind.as_str())
        .map_or(outage.agent_kind.as_str(), |spec| spec.display_name)
        .to_owned()
}

pub(super) fn throttle_notice_line(
    theme: &Theme,
    notice: &crate::store::snapshot::ThrottleNotice,
    width: usize,
) -> Line<'static> {
    let style = theme.styled(Component::ThrottleNotice, Modifier::empty());
    let prefix = if notice.kind == crate::ids::HoldKind::CheckFailed {
        ""
    } else {
        "starts held: "
    };
    let left = vec![Span::styled(
        format!(
            "{} {prefix}{}",
            theme.glyph(GlyphRole::ChromeAlert),
            notice.reason
        ),
        style,
    )];
    let count = if notice.count > 1 {
        format!("{} held{}", notice.count, value_seam(theme))
    } else {
        String::new()
    };
    let right = vec![Span::styled(
        format!("{}{count}rimz doctor", value_seam(theme).trim_start()),
        style,
    )];
    let full = left.iter().chain(&right).map(Span::width).sum::<usize>() + 1;
    layout::pin_right(left, right, width.min(full))
}

pub(super) fn truth_notice_lines(
    theme: &Theme,
    notice: &TruthNotice,
    now: Timestamp,
) -> Vec<Line<'static>> {
    let since_ms = notice.since_ms.min(i64::MAX as u64) as i64;
    let since = Timestamp::from_millisecond(since_ms).unwrap_or(now);
    let elapsed = age_short(since, now);
    let noun = if notice.carried == 1 { "pane" } else { "panes" };
    vec![Line::styled(
        format!(
            "{} pane source degraded{}{} carried {noun}{}{elapsed}",
            theme.glyph(GlyphRole::ChromeAlert),
            value_seam(theme),
            notice.carried,
            value_seam(theme)
        ),
        theme.warn(Modifier::DIM),
    )]
}

pub(super) fn gate_notice_lines(theme: &Theme, notice: &GateNotice) -> Vec<Line<'static>> {
    vec![Line::styled(
        format!(
            "{} pane updates held{}{}",
            theme.glyph(GlyphRole::ChromeAlert),
            value_seam(theme),
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

pub(super) fn search_line(
    theme: &Theme,
    ui: &ViewState,
    roster: &VisibleRoster<'_>,
    snapshot: &SidebarSnapshot,
    cells: usize,
) -> Option<Line<'static>> {
    let query = ui
        .toggles
        .search_draft
        .as_ref()
        .or(ui.toggles.make_up_filter.query.as_ref())?;
    let typing = ui.toggles.search_draft.is_some();
    let inner = super::sections::content_width(cells);
    let lead = format!("{} ", theme.glyph(GlyphRole::ChromeSearch));
    let left = if typing {
        vec![
            Span::styled(lead, theme.body()),
            Span::styled(query.clone(), theme.body()),
        ]
    } else {
        vec![Span::styled(
            format!("{lead}{query}"),
            theme.picked_chip(theme.body_tone(), Modifier::empty()),
        )]
    };
    let total: usize = snapshot
        .worktree_groups
        .iter()
        .map(|group| group.rows.len().max(usize::from(group.workflow.is_some())))
        .sum();
    let shown = roster.len()
        + roster
            .groups()
            .iter()
            .filter(|group| group.shown() && group.source().rows.is_empty())
            .count();
    let count = Span::styled(format!("{shown}/{total}"), theme.muted());
    let count = (!query.is_empty()
        && layout::spans_width(&left) + usize::from(typing) + 1 + count.width() <= inner)
        .then_some(count);
    let mut content = layout::trim_spans_to_width(left, inner.saturating_sub(usize::from(typing)));
    if typing && inner > 0 {
        content.push(Span::styled(
            " ",
            theme.body().add_modifier(Modifier::REVERSED),
        ));
    }
    let padding =
        inner.saturating_sub(layout::spans_width(&content) + count.as_ref().map_or(0, Span::width));
    content.push(Span::raw(" ".repeat(padding)));
    if let Some(count) = count {
        content.push(count);
    }
    let mut spans = vec![Span::raw(" ".repeat(cells.min(1)))];
    spans.extend(content);
    if cells > 1 {
        spans.push(Span::raw(" "));
    }
    if typing && let Some(bg) = theme.selection_band() {
        for span in &mut spans {
            span.style = span.style.bg(bg);
        }
    }
    Some(Line::from(spans))
}

pub(super) fn footer_lines(
    snapshot: &SidebarSnapshot,
    theme: &Theme,
    width: usize,
) -> Vec<Line<'static>> {
    vec![footer_line(footer_parts(snapshot, theme), width)]
}

#[derive(Clone)]
pub(super) struct FooterParts {
    link: Option<FooterLink>,
    stall: Option<FooterStall>,
    presence: Option<Span<'static>>,
    help: Span<'static>,
    help_key: String,
}

#[derive(Clone)]
struct FooterLink {
    full: Span<'static>,
    bare: Span<'static>,
    keep_figure: bool,
}

#[derive(Clone)]
struct FooterStall {
    lead: Span<'static>,
    glyph: Span<'static>,
    items: Vec<(&'static str, Vec<Span<'static>>)>,
    separator: Span<'static>,
    overflow: Style,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum FooterStep {
    Full,
    NoPresence,
    ShortHelp,
    BareLink,
    NoHelp,
    NamedOverflow,
    CountOverflow,
    GlyphOnly,
}

pub(super) fn footer_parts(snapshot: &SidebarSnapshot, theme: &Theme) -> FooterParts {
    let help_key = sequence_label(&snapshot.keys, SidebarAction::Help).unwrap_or_default();
    FooterParts {
        link: snapshot.link.as_ref().map(|link| link_badge(link, theme)),
        stall: (!snapshot.stall.is_empty()).then(|| stall_badge(&snapshot.stall, theme)),
        presence: presence_badge(snapshot.presence, snapshot.now, theme),
        help: Span::styled(
            if help_key.is_empty() {
                String::new()
            } else {
                format!("{help_key} for help")
            },
            theme.faint(),
        ),
        help_key,
    }
}

pub(super) fn footer_line(parts: FooterParts, width: usize) -> Line<'static> {
    use FooterStep::*;
    for step in [
        Full,
        NoPresence,
        ShortHelp,
        BareLink,
        NoHelp,
        NamedOverflow,
        CountOverflow,
        GlyphOnly,
    ] {
        let mut left = parts.left(step);
        let left_width = layout::spans_width(&left);
        let fits = |help_width| {
            let gap = if left_width > 0 && help_width > 0 {
                2
            } else {
                0
            };
            left_width + gap + help_width <= width
        };
        let mut help = parts.help.clone();
        if step >= ShortHelp {
            help.content = parts.help_key.clone().into();
        }
        if step >= NoHelp && !fits(help.width()) {
            help.content = "".into();
        }
        if !fits(help.width()) {
            continue;
        }
        if help.width() > 0 {
            left.push(Span::raw(" ".repeat(width - left_width - help.width())));
            left.push(help);
        }
        return Line::from(left);
    }
    Line::from(layout::trim_spans_to_width(parts.left(GlyphOnly), width))
}

impl FooterParts {
    fn left(&self, step: FooterStep) -> Vec<Span<'static>> {
        let mut left = Vec::new();
        if let Some(link) = &self.link {
            left.push(if step >= FooterStep::BareLink && !link.keep_figure {
                link.bare.clone()
            } else {
                link.full.clone()
            });
        }
        if let Some(stall) = &self.stall {
            if !left.is_empty() {
                left.push(Span::raw("  "));
            }
            left.extend(stall.spans(step));
        }
        if step == FooterStep::Full
            && let Some(presence) = &self.presence
        {
            if !left.is_empty() {
                left.push(Span::raw("  "));
            }
            left.push(presence.clone());
        }
        left
    }
}

impl FooterStall {
    fn spans(&self, step: FooterStep) -> Vec<Span<'static>> {
        let lead = if step == FooterStep::GlyphOnly {
            &self.glyph
        } else {
            &self.lead
        };
        let mut spans = vec![lead.clone(), Span::raw(" ")];
        let visible = if step >= FooterStep::NamedOverflow {
            self.items.len().min(1)
        } else {
            self.items.len()
        };
        for (index, (_, item)) in self.items[..visible].iter().enumerate() {
            if index > 0 {
                spans.push(self.separator.clone());
            }
            spans.extend(item.iter().cloned());
        }
        let overflow = match &self.items[visible..] {
            [] => return spans,
            [(name, _)] if step == FooterStep::NamedOverflow => format!(" +{name}"),
            hidden => format!(" +{}", hidden.len()),
        };
        spans.push(Span::styled(overflow, self.overflow));
        spans
    }
}

fn stall_badge(items: &[SidebarStallItem], theme: &Theme) -> FooterStall {
    let quiet = theme.muted().add_modifier;
    let lead_style = if items
        .iter()
        .any(|item| item.phase == SidebarStallPhase::Active)
    {
        theme.styled(Component::StallLeadActive, Modifier::BOLD)
    } else {
        theme.styled(Component::StallLeadFading, quiet)
    };
    FooterStall {
        lead: Span::styled(
            format!("{} stall", theme.glyph(GlyphRole::ChromeStall)),
            lead_style,
        ),
        glyph: Span::styled(theme.glyph(GlyphRole::ChromeStall).to_owned(), lead_style),
        items: items
            .iter()
            .map(|item| {
                let name = match item.resource {
                    SidebarStallResource::Memory => "mem",
                    SidebarStallResource::Cpu => "cpu",
                    SidebarStallResource::Io => "io",
                };
                let spans = match item.phase {
                    SidebarStallPhase::Active => vec![
                        Span::styled(
                            format!("{name} "),
                            theme.styled(Component::StallWord, Modifier::empty()),
                        ),
                        Span::styled(
                            format!("{}%", item.percent),
                            theme.styled(Component::StallFigure, Modifier::BOLD),
                        ),
                    ],
                    SidebarStallPhase::Fading => vec![Span::styled(
                        format!(
                            "{name} {}%{}",
                            item.percent,
                            theme.glyph(GlyphRole::ChromeStallFading)
                        ),
                        theme.styled(Component::StallFading, quiet),
                    )],
                };
                (name, spans)
            })
            .collect(),
        separator: Span::styled(
            value_seam(theme),
            theme.styled(Component::StallSeparator, theme.faint().add_modifier),
        ),
        overflow: theme.styled(Component::StallOverflow, quiet),
    }
}

fn presence_badge(
    presence: Option<SidebarPresence>,
    now: jiff::Timestamp,
    theme: &Theme,
) -> Option<Span<'static>> {
    let presence = presence.filter(|presence| presence.shows_badge())?;
    let glyph = theme.glyph(GlyphRole::ChromePresenceAway);
    let text = match presence {
        SidebarPresence::Active => return None,
        SidebarPresence::Idle { last_input_ms } => {
            let minutes =
                (now.as_millisecond().max(0) as u64).saturating_sub(last_input_ms) / 60_000;
            if minutes == 0 {
                format!("{glyph} idle")
            } else {
                format!("{glyph} idle{}{minutes}m", value_seam(theme))
            }
        }
        SidebarPresence::Detached => format!("{glyph} away"),
    };
    Some(Span::styled(text, theme.muted()))
}

fn link_badge(link: &SidebarLinkHealth, theme: &Theme) -> FooterLink {
    let glyph = theme.glyph(GlyphRole::ChromeRemoteLink);
    let mut text = match link.freshness {
        SidebarLinkFreshness::Stale => format!("{glyph} stale"),
        SidebarLinkFreshness::Fresh => match link.rtt_ms {
            Some(rtt) => format!("{glyph} {rtt}ms"),
            None => format!("{glyph} …"),
        },
    };
    if link.freshness == SidebarLinkFreshness::Fresh && link.miss_pct > 10 {
        text.push_str(&format!(" {}%", link.miss_pct));
    }
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
    FooterLink {
        full: Span::styled(text, style),
        bare: Span::styled(glyph.to_owned(), style),
        keep_figure: link.freshness == SidebarLinkFreshness::Stale
            || link.tier == crate::ids::LinkTier::Bad,
    }
}

/// The `?` popup: a which-key style block with action keys, status filters, and
/// the room-scoped focus and zoom chords.
pub(super) fn help_lines(
    theme: &Theme,
    focus_key: Option<&str>,
    zoom_key: Option<&str>,
    keys: &SidebarKeymap,
    width: usize,
) -> Vec<Line<'static>> {
    const TITLE: &str = "help";
    const MIN_FRAME_WIDTH: usize = 22;

    let rows = help_body_rows(theme, focus_key, zoom_key, keys);
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
    zoom_key: Option<&str>,
    keys: &SidebarKeymap,
) -> Vec<Line<'static>> {
    use SidebarAction as A;
    let key = |action| chord_label(keys, action);
    let pair = |first, second| format!("{}/{}", key(first), key(second));
    let icon = |role| Some(key_icon(theme, role));
    let movement = |key: String, label| key_entry(theme, icon(GlyphRole::KeysMove), key, label);
    let slots = format!("{}-{}", key(A::CardSlot(1)), key(A::CardSlot(9)));
    let screen = format!(
        "{}/{}",
        pair(A::MoveScreenTop, A::MoveScreenMiddle),
        key(A::MoveScreenBottom)
    );
    let keys_section = vec![
        (
            key_entry(theme, None, key(A::SearchStart), "search"),
            Some(key_entry(
                theme,
                icon(GlyphRole::KeysDismiss),
                key(A::ViewCancel),
                "clear",
            )),
        ),
        (
            movement(pair(A::MoveDown, A::MoveUp), "cards/entries"),
            Some(movement(
                pair(A::MoveWorktreeDown, A::MoveWorktreeUp),
                "worktrees",
            )),
        ),
        (
            movement(pair(A::MoveTop, A::MoveBottom), "ends"),
            Some(movement(pair(A::MovePageDown, A::MovePageUp), "page")),
        ),
        (
            key_entry(theme, icon(GlyphRole::KeysFocus), slots, "direct"),
            Some(movement(
                pair(A::MoveHalfPageDown, A::MoveHalfPageUp),
                "half page",
            )),
        ),
        (
            key_entry(
                theme,
                icon(GlyphRole::KeysInbox),
                pair(A::SearchNext, A::SearchPrev),
                "next/prev",
            ),
            Some(movement(screen, "screen")),
        ),
        (
            key_entry(
                theme,
                icon(GlyphRole::KeysFocus),
                key(A::CardLast),
                "last card",
            ),
            Some(key_entry(
                theme,
                icon(GlyphRole::KeysFocus),
                key(A::CardFocus),
                "focus/open",
            )),
        ),
        (
            key_entry(
                theme,
                icon(GlyphRole::KeysRead),
                key(A::ReadToggle),
                "read/unread",
            ),
            Some(key_entry(
                theme,
                icon(GlyphRole::KeysRead),
                key(A::ReadAll),
                "read all",
            )),
        ),
        (
            key_entry(
                theme,
                icon(GlyphRole::KeysAccounts),
                pair(A::DashboardPrev, A::DashboardNext),
                "account tabs",
            ),
            Some(key_entry(
                theme,
                icon(GlyphRole::KeysReload),
                key(A::ViewReload),
                "reload",
            )),
        ),
        (
            key_entry(theme, None, key(A::FoldInactive), "inactive fold"),
            Some(movement(pair(A::WidthNarrower, A::WidthWider), "width")),
        ),
        (
            key_entry(theme, None, key(A::FoldToggle), "card fold"),
            Some(key_entry(
                theme,
                None,
                pair(A::FoldOpenAll, A::FoldCloseAll),
                "all groups",
            )),
        ),
        (
            key_entry(theme, None, key(A::CardClose), "close"),
            Some(key_entry(theme, None, key(A::PaletteCommands), "commands")),
        ),
        (
            key_entry(
                theme,
                None,
                pair(A::PaletteNewAgent, A::PaletteNewAgentWorktree),
                "new agent",
            ),
            Some(key_entry(theme, None, key(A::PaletteRepeat), "repeat")),
        ),
        (
            key_entry(
                theme,
                icon(GlyphRole::KeysFocus),
                key(A::PaneReturn),
                "return",
            ),
            Some(movement(key(A::WidthReset), "reset width")),
        ),
        (key_entry(theme, None, key(A::ViewRedraw), "redraw"), None),
    ];
    let filter_section = vec![
        (
            status_entry(theme, AgentStatus::Waiting, key(A::FilterAsks), "waiting"),
            Some(status_entry(
                theme,
                AgentStatus::Failed,
                key(A::FilterFailures),
                "attention",
            )),
        ),
        (
            status_entry(theme, AgentStatus::Paused, key(A::FilterPaused), "paused"),
            Some(status_entry(
                theme,
                AgentStatus::Success,
                key(A::FilterDone),
                "done",
            )),
        ),
        (
            status_entry(
                theme,
                AgentStatus::Running,
                key(A::FilterWorking),
                "working",
            ),
            Some(status_entry(
                theme,
                AgentStatus::Idle,
                key(A::FilterIdle),
                "idle",
            )),
        ),
        (
            status_entry(
                theme,
                AgentStatus::Sleeping,
                key(A::FilterSleeping),
                "sleeping",
            ),
            Some(key_entry(
                theme,
                icon(GlyphRole::KeysInbox),
                key(A::FilterNeedsYou),
                "needs you",
            )),
        ),
        (
            key_entry(
                theme,
                icon(GlyphRole::KeysUnread),
                key(A::FilterUnread),
                "unread",
            ),
            Some(key_entry(
                theme,
                icon(GlyphRole::KeysAll),
                key(A::FilterClear),
                "all",
            )),
        ),
    ];
    let mut lines = help_grid(
        theme,
        vec![("keys", keys_section), ("filter", filter_section)],
    );
    let room_keys = [
        (focus_key, Some(GlyphRole::KeysSidebar), "sidebar"),
        (zoom_key, None, "zoom"),
    ];
    if room_keys.iter().any(|(key, _, _)| key.is_some()) {
        lines.push(Line::default());
        for (key, role, label) in room_keys {
            let Some(key) = key else { continue };
            let entry = key_entry(
                theme,
                role.map(|role| key_icon(theme, role)),
                focus_chord_label(key),
                label,
            );
            let key_w = layout::text_width(&entry.key);
            lines.push(Line::from(render_entry(theme, entry, key_w)).style(theme.body()));
        }
    }
    lines
}

/// The legend's spelling of the first sequence bound to `action`, `?` when
/// the table leaves it unbound.
fn chord_label(keys: &SidebarKeymap, action: SidebarAction) -> String {
    sequence_label(keys, action).unwrap_or_else(|| "?".to_owned())
}

/// The first sequence bound to `action`, as the chrome spells keys.
fn sequence_label(keys: &SidebarKeymap, action: SidebarAction) -> Option<String> {
    keys.sequences(action).next().map(sequence_text)
}

fn sequence_text(sequence: &[SidebarChord]) -> String {
    sequence
        .iter()
        .map(|chord| {
            let mut label = String::new();
            if chord.ctrl {
                label.push('^');
            }
            if chord.alt {
                label.push_str("M-");
            }
            label.push_str(&key_label(*chord));
            label
        })
        .collect()
}

fn key_label(chord: SidebarChord) -> String {
    match chord.key {
        SidebarKey::Up => "↑".to_owned(),
        SidebarKey::Down => "↓".to_owned(),
        SidebarKey::Left => "←".to_owned(),
        SidebarKey::Right => "→".to_owned(),
        SidebarKey::PageUp => "PgUp".to_owned(),
        SidebarKey::PageDown => "PgDn".to_owned(),
        SidebarKey::Char(key) if key != ' ' => key.to_string(),
        _ => SidebarChord {
            ctrl: false,
            alt: false,
            ..chord
        }
        .to_string(),
    }
}

fn focus_chord_label(key: &str) -> String {
    key.to_ascii_lowercase().replace(['+', '-'], " ")
}

#[derive(Clone)]
struct Entry {
    icon: Option<Span<'static>>,
    key: String,
    label: String,
    key_style: Style,
}

type HelpRow = (Entry, Option<Entry>);
type HelpSection = (&'static str, Vec<HelpRow>);

fn help_grid(theme: &Theme, sections: Vec<HelpSection>) -> Vec<Line<'static>> {
    const GAP: usize = 2;
    const ICON_W: usize = 1;

    let left_key_w = sections
        .iter()
        .flat_map(|(_, rows)| rows.iter().map(|(left, _)| layout::text_width(&left.key)))
        .max()
        .unwrap_or(0);
    let left_label_w = sections
        .iter()
        .flat_map(|(_, rows)| rows.iter().map(|(left, _)| layout::text_width(&left.label)))
        .max()
        .unwrap_or(0);
    let right_key_w = sections
        .iter()
        .flat_map(|(_, rows)| {
            rows.iter()
                .filter_map(|(_, right)| right.as_ref().map(|entry| layout::text_width(&entry.key)))
        })
        .max()
        .unwrap_or(0);
    let left_half_w = ICON_W + 1 + left_key_w + 1 + left_label_w;

    let mut lines = Vec::new();
    for (section, rows) in sections {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.push(subheader(theme, section));
        lines.extend(rows.into_iter().map(|(left, right)| {
            let mut spans = render_entry(theme, left, left_key_w);
            let left_w = layout::spans_width(&spans);
            if let Some(right) = right {
                spans.push(Span::raw(
                    " ".repeat(left_half_w.saturating_sub(left_w) + GAP),
                ));
                spans.extend(render_entry(theme, right, right_key_w));
            }
            Line::from(spans).style(theme.body())
        }));
    }
    lines
}

fn render_entry(theme: &Theme, entry: Entry, key_w: usize) -> Vec<Span<'static>> {
    const ICON_W: usize = 1;

    let mut spans = Vec::with_capacity(5);
    if let Some(icon) = entry.icon {
        let icon_w = layout::spans_width(std::slice::from_ref(&icon));
        spans.push(icon);
        if icon_w < ICON_W {
            spans.push(Span::raw(" ".repeat(ICON_W - icon_w)));
        }
    } else {
        spans.push(Span::raw(" ".repeat(ICON_W)));
    }
    spans.push(Span::raw(" "));
    let key_w_actual = layout::text_width(&entry.key);
    spans.push(Span::styled(entry.key, entry.key_style));
    if key_w_actual < key_w {
        spans.push(Span::raw(" ".repeat(key_w - key_w_actual)));
    }
    spans.push(Span::raw(" "));
    spans.push(Span::styled(entry.label, theme.faint()));
    spans
}

fn key_entry(
    theme: &Theme,
    icon: Option<Span<'static>>,
    key: impl Into<String>,
    label: &str,
) -> Entry {
    Entry {
        icon,
        key: key.into(),
        label: label.to_owned(),
        key_style: theme.styled(Component::HelpKey, Modifier::BOLD),
    }
}

fn status_entry(theme: &Theme, status: AgentStatus, keys: String, label: &str) -> Entry {
    key_entry(theme, Some(status_icon(theme, status)), keys, label)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_abbreviation_collapses_only_a_home_prefix() {
        assert_eq!(
            abbreviate_under("/home/dev/code/query-engine", Some("/home/dev")),
            "~/code/query-engine"
        );
        assert_eq!(abbreviate_under("/home/dev", Some("/home/dev")), "~");
        // A path that merely shares a textual prefix is not under home.
        assert_eq!(
            abbreviate_under("/home/developer/x", Some("/home/dev")),
            "/home/developer/x"
        );
        // Outside home, or no home, passes through.
        assert_eq!(
            abbreviate_under("/srv/code", Some("/home/dev")),
            "/srv/code"
        );
        assert_eq!(abbreviate_under("/srv/code", None), "/srv/code");
    }

    #[test]
    fn help_shows_width_chords_and_remapped_filters() {
        let theme = Theme::fixed(false);
        let legend = |keys: &SidebarKeymap| {
            help_body_rows(&theme, None, None, keys)
                .iter()
                .flat_map(|line| line.spans.iter())
                .map(|span| span.content.as_ref())
                .collect::<String>()
        };
        let text = legend(&SidebarKeymap::default());
        for shown in [
            "</>", "width", "fd", "done", "F", "all", "Esc", "clear", "za", "zR/zM",
        ] {
            assert!(text.contains(shown), "{shown:?} missing from {text:?}");
        }

        let text = legend(&SidebarKeymap::rebound(&[
            ("f d", "none"),
            ("Q", "filter.done"),
            ("Esc", "none"),
            ("Ctrl+g", "view.cancel"),
        ]));
        assert!(!text.contains("fd"), "{text:?}");
        assert!(text.contains("Q"), "{text:?}");
        assert!(text.contains("^g"), "{text:?}");
        assert!(!text.contains("Esc"), "{text:?}");
    }

    #[test]
    fn key_hint_row_lists_the_live_continuations_of_the_pending_prefix() {
        let theme = Theme::fixed(false);
        let f = [SidebarChord {
            key: SidebarKey::Char('f'),
            ctrl: false,
            alt: false,
        }];
        let text = |keys: &SidebarKeymap| {
            key_row_line(&theme, keys, &[], &f, 200)
                .spans
                .iter()
                .map(|span| span.content.to_string())
                .collect::<String>()
        };
        let line = text(&SidebarKeymap::default());
        assert!(line.trim_start().starts_with('f'), "{line:?}");
        for shown in ["d done", "w working", "n needs you", "? asks"] {
            assert!(line.contains(shown), "{shown:?} missing from {line:?}");
        }
        assert!(!line.contains("clear filter"), "{line:?}");

        let line = text(&SidebarKeymap::rebound(&[
            ("f d", "none"),
            ("f x", "filter.done"),
        ]));
        assert!(line.contains("x done"), "{line:?}");
        assert!(!line.contains("d done"), "{line:?}");
        assert_eq!(
            key_row_line(&theme, &SidebarKeymap::default(), &[], &f, 12).width(),
            12
        );
    }

    #[test]
    fn mode_marks_sit_left_of_the_hint_and_stay_when_it_truncates() {
        use crate::pane::RoomMode;
        let theme = Theme::fixed(false);
        let keys = SidebarKeymap::default();
        let f = [SidebarChord {
            key: SidebarKey::Char('f'),
            ctrl: false,
            alt: false,
        }];
        let text = |modes: &[RoomMode], pending: &[SidebarChord], width| {
            let line = key_row_line(&theme, &keys, modes, pending, width);
            assert_eq!(line.width(), width);
            line.spans
                .iter()
                .map(|span| span.content.to_string())
                .collect::<String>()
        };
        assert_eq!(text(&[RoomMode::Prefix], &[], 20).trim_end(), "PREFIX");
        let modes = [RoomMode::Resize, RoomMode::Scrollback, RoomMode::Move];
        assert_eq!(text(&modes, &[], 30).trim_end(), "RESIZE SCROLL MOVE");

        let wide = text(&[RoomMode::Prefix], &f, 200);
        assert!(wide.starts_with("PREFIX "), "{wide:?}");
        assert!(wide.contains("f  "), "{wide:?}");
        let narrow = text(&[RoomMode::Prefix], &f, 12);
        assert!(narrow.starts_with("PREFIX "), "{narrow:?}");
        assert_eq!(
            text(&modes, &f, 10),
            "RESIZE SCR",
            "marks fill a row too narrow for them"
        );
    }

    #[test]
    fn help_shows_configured_room_chords() {
        let theme = Theme::fixed(false);
        let lines = help_body_rows(
            &theme,
            Some("Alt+p"),
            Some("Alt+g"),
            &SidebarKeymap::default(),
        );
        let text = |line: &Line<'_>| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        };
        let sidebar = lines
            .iter()
            .map(text)
            .find(|line| line.contains("sidebar"))
            .expect("sidebar chord row");
        let zoom = lines
            .iter()
            .map(text)
            .find(|line| line.contains("zoom"))
            .expect("zoom chord row");

        assert!(sidebar.contains("alt p"), "{sidebar}");
        assert!(zoom.contains("alt g"), "{zoom}");
    }
}
