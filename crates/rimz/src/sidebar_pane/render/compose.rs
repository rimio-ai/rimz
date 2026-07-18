use crate::agents::AgentStatus;
use crate::config::ScrollbarMode;
use crate::sidebar_pane::pixel::meter::MeterPixels;
use crate::sidebar_pane::view::VisibleRoster;
use crate::{
    SidebarSnapshot, SidebarWorktreeGroup, SidebarWorktreeKind, actionable_unread_count,
    lead_unread_row,
};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::chrome::{
    FooterParts, alert_lines, footer_lines, footer_parts, gate_notice_lines, hairline_rule,
    repo_header_lines, truth_notice_lines,
};
use super::sections::{
    CockpitBadges, DashboardContext, RowCtx, Tier, WorktreeRenderContext, cockpit_spend_line,
    cockpit_summary_line, content_width, dashboard_block, fleet_header_lines, fleet_size,
    fleet_store_lines, fleet_total_lines, open_pr_total, open_pr_worst_ci, trim_spans_to_width,
    worktree_group_lines_projected,
};
use super::theme::Theme;
use super::{
    Alert, BodyFilter, DashboardMode, FrameInteractions, HitRegion, HitTarget, RenderedBlock,
    UiState, active_dashboard_tab, cockpit_spend_target, dashboard_mode, dashboard_present, labels,
};

/// Lay out the frame as three vertical zones: the top-pinned cockpit (identity,
/// summary, make-up line, the conditional unread banner, and fixed separator),
/// a scroll viewport over the agent cards, and the bottom chrome pinned to the
/// bottom edge like a status bar — the provider dashboard, store, centered
/// navigation footer, and beneath them the sticky health alert. Space for the
/// pinned zones is always reserved — including the fixed separators under the
/// cockpit and above the provider dashboard — so the scroll zone is windowed
/// before either pinned edge is ever clipped. While an alert is *active* the
/// body is a stale/empty fetch, so the footer steps aside and the alert speaks
/// alone.
///
/// The viewport window is `UiState::scroll_offset`, resolved here each frame:
/// clamped to the zone, then minimally auto-scrolled so the selected card — its
/// expanded subagent lines included — sits fully in view, unless a manual wheel
/// pin ([`ManualScroll`]) holds the window. A one-shot external-focus reveal can
/// widen that span to include the selected row's worktree header. The effective
/// offset is returned for the caller to write back, a draw byproduct like the
/// hit-test map. When the cards overflow the viewport, each visible scroll line
/// carries a track/thumb glyph in the right rail column — part of the composed
/// line, so every consumer of the frame sees it.
///
/// Returns the composed frame with its hit-test maps and the effective scroll
/// offset ([`ComposedFrame`]). Row-map entry `i` is the visible row index that
/// on-screen content line `i` belongs to (`app::visible_rows()` order), or
/// `None` for structural lines (cockpit header, unread banner, gaps, the
/// external divider, `+K more`, help, footer, alert); a worktree header routes
/// to the row it jumps into. The dashboard tab, make-up bucket hits, and unread
/// banner screen row ride beside it in absolute screen coordinates. The maps
/// are the single authority on hit geometry — built from the same final line
/// vector that is rendered, so they stay 1:1 with what the user sees through
/// every clip and every scroll position.
#[cfg(test)]
pub(crate) fn compose_lines(
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    ui: &UiState,
    theme: &Theme,
    width: u16,
    height: u16,
) -> ComposedFrame {
    compose_lines_with_meter(snapshot, alert, ui, theme, width, height, None)
}

pub(crate) fn compose_lines_with_meter(
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    ui: &UiState,
    theme: &Theme,
    width: u16,
    height: u16,
    mut meter_pixels: Option<&mut MeterPixels>,
) -> ComposedFrame {
    // One `Theme` per frame, handed to the body and the bottom chrome alike:
    // the cached `NO_COLOR` reading plus the palette the producer resolved from
    // `[theme]` onto the snapshot — so a re-themed config lands with the next
    // snapshot, identically on every renderer of the workspace.
    let cells = usize::from(width.max(1));
    // The whole sidebar sits inside a one-cell frame: chrome is built to the inner
    // width and opened with a blank gutter, reserving the trailing right rail —
    // the same frame the cards carry (see `with_gutter`).
    let inner = content_width(cells);
    let roster = VisibleRoster::new(
        snapshot,
        ui.make_up_filter,
        &ui.expanded_groups,
        ui.held_visible(),
    );
    let mut top = top_lines(snapshot, ui, cells, theme);
    let scroll = scroll_lines(
        snapshot,
        ui,
        &roster,
        cells,
        theme,
        meter_pixels.as_deref_mut(),
    );

    // The tab hits arrive from the bottom chrome relative to its own lines;
    // they are translated to absolute screen coordinates once the block's final
    // position is known, below.
    let bottom = build_bottom_chrome(snapshot, alert, theme, inner, ui);

    let height = usize::from(height);
    let bottom_height = bottom
        .lines
        .iter()
        // Every line occupies at least one row — a blank separator has width 0
        // but still takes a row, so `.max(1)` keeps the reservation honest and
        // the footer from being pushed off the frame.
        .map(|line| line.width().div_ceil(cells).max(1))
        .sum::<usize>()
        .min(height);

    let scroll_len = scroll.lines.len();
    let layout = plan_zones(ZoneInputs {
        snapshot,
        ui,
        roster: &roster,
        scroll_map: scroll.interactions.row_map(),
        scroll_len,
        top_len: top.lines.len(),
        height,
        bottom_height,
    });
    insert_unread_banner(layout.show_banner, snapshot, theme, &mut top);
    let scroll_block = visible_scroll_block(scroll, snapshot, ui, theme, cells, layout);

    let mut frame = top.window(0, layout.top_shown);
    frame.append(scroll_block);
    let pad = height.saturating_sub(frame.lines.len() + bottom_height);
    for _ in 0..pad {
        frame.push_inert(Line::from(""));
    }
    frame.append(bottom);
    if let Some(pixels) = meter_pixels {
        pixels.observe_visible(&frame.lines);
    }
    ComposedFrame {
        lines: frame.lines,
        interactions: frame.interactions,
        scroll_offset: layout.offset,
        top_height: layout.top_shown,
        bottom_height,
    }
}

#[derive(Clone, Copy)]
struct ZoneLayout {
    show_banner: bool,
    top_shown: usize,
    viewport: usize,
    offset: usize,
}

struct ZoneInputs<'a, 'snapshot> {
    snapshot: &'snapshot SidebarSnapshot,
    ui: &'a UiState,
    roster: &'a VisibleRoster<'snapshot>,
    scroll_map: &'a [Option<usize>],
    scroll_len: usize,
    top_len: usize,
    height: usize,
    bottom_height: usize,
}

fn plan_zones(input: ZoneInputs<'_, '_>) -> ZoneLayout {
    let ZoneInputs {
        snapshot,
        ui,
        roster,
        scroll_map,
        scroll_len,
        top_len,
        height,
        bottom_height,
    } = input;
    let after_bottom = height.saturating_sub(bottom_height);
    let top_without_banner = top_len.min(after_bottom);
    let viewport_without_banner = after_bottom - top_without_banner;
    let offset_without_banner =
        resolve_scroll_offset(roster, ui, scroll_map, scroll_len, viewport_without_banner);
    let show_banner = lead_unread_row(&snapshot.worktree_groups).is_some_and(|lead| {
        viewport_without_banner > 0
            && !lead_unread_visible(
                roster,
                &lead.id,
                scroll_map,
                offset_without_banner,
                viewport_without_banner,
            )
    });
    let top_shown = top_len
        .saturating_add(usize::from(show_banner))
        .min(after_bottom);
    let viewport = after_bottom - top_shown;
    let offset = if show_banner {
        resolve_scroll_offset(roster, ui, scroll_map, scroll_len, viewport)
    } else {
        offset_without_banner
    };
    ZoneLayout {
        show_banner,
        top_shown,
        viewport,
        offset,
    }
}

fn insert_unread_banner(
    show: bool,
    snapshot: &SidebarSnapshot,
    theme: &Theme,
    top: &mut RenderedBlock,
) {
    let Some(lead) = show
        .then(|| lead_unread_row(&snapshot.worktree_groups))
        .flatten()
    else {
        return;
    };
    let at = top.lines.len().saturating_sub(1);
    let count = actionable_unread_count(&snapshot.worktree_groups);
    let status = lead.status().unwrap_or(AgentStatus::Waiting);
    let separator = top.lines.last().cloned().unwrap_or_default();
    let mut with_banner = std::mem::take(top).window(0, at);
    with_banner.push_target(
        pad_chrome(unread_banner_line(theme, status, count)),
        HitTarget::UnreadBanner,
    );
    with_banner.push_inert(separator);
    *top = with_banner;
}

fn visible_scroll_block(
    scroll: RenderedBlock,
    snapshot: &SidebarSnapshot,
    ui: &UiState,
    theme: &Theme,
    cells: usize,
    layout: ZoneLayout,
) -> RenderedBlock {
    let scroll_len = scroll.lines.len();
    let end = layout
        .offset
        .saturating_add(layout.viewport)
        .min(scroll_len);
    let mut block = scroll.window(layout.offset, end.saturating_sub(layout.offset));
    let overflow = scroll_len > layout.viewport && layout.viewport > 0;
    let show_bar = match snapshot.theme.display.scrollbar {
        ScrollbarMode::Always => true,
        ScrollbarMode::Never => false,
        ScrollbarMode::Auto => {
            ui.scrollbar.moved_from(layout.offset) || ui.scrollbar.visible(ui.animation_phase)
        }
    };
    if overflow && show_bar {
        for (visible_index, line) in block.lines.iter_mut().enumerate() {
            *line = with_scrollbar(
                std::mem::take(line),
                theme,
                cells,
                visible_index,
                layout.offset,
                scroll_len,
                layout.viewport,
            );
        }
    }
    block
}

/// Bottom-pinned chrome, top to bottom: a fixed separator when the provider
/// dashboard is present, the per-provider dashboard (account-scoped budgets +
/// brand emblem, which opens with its own top hairline — the tab rail when
/// several accounts register), the fallback fleet store for no-table layouts,
/// the navigation footer (centered), then the sticky health alert. While an
/// alert is active the body is a stale/empty fetch, so the panel and footer step
/// aside and the alert speaks alone. Every chrome line is gutter-padded so it
/// breathes in the same one-cell frame as the body.
#[derive(Clone, Copy)]
enum BottomCorner {
    FleetTotal,
    FleetStore,
    None,
}

/// Selects alert-only, dashboard-folded, dashboard-with-totals, or bare-store
/// chrome before rendering any lines.
struct BottomPlan {
    dashboard: Option<DashboardMode>,
    folded_footer: bool,
    corner: BottomCorner,
    truth_notice: bool,
    gate_notice: bool,
    footer: bool,
    alert: bool,
}

fn plan_bottom_chrome(
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    ui: &UiState,
) -> BottomPlan {
    let alert_active = alert.is_some_and(Alert::is_active);
    let dashboard = dashboard_present(snapshot, alert_active).then(|| dashboard_mode(snapshot));
    let owns_store = dashboard.is_some_and(|mode| mode.owns_store(!snapshot.providers.is_empty()));
    let folded_footer = owns_store
        && snapshot.truth_degraded.is_none()
        && ui.gate_notice.is_none()
        && alert.is_none();
    let corner = if alert_active || owns_store {
        BottomCorner::None
    } else if dashboard.is_some() {
        BottomCorner::FleetTotal
    } else {
        BottomCorner::FleetStore
    };
    BottomPlan {
        dashboard,
        folded_footer,
        corner,
        truth_notice: !alert_active && snapshot.truth_degraded.is_some(),
        gate_notice: !alert_active && ui.gate_notice.is_some(),
        footer: !alert_active && !folded_footer,
        alert: alert.is_some(),
    }
}

pub(super) fn build_bottom_chrome(
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    theme: &Theme,
    inner: usize,
    ui: &UiState,
) -> RenderedBlock {
    let plan = plan_bottom_chrome(snapshot, alert, ui);
    let mut bottom = RenderedBlock::default();
    let folded_footer = plan
        .folded_footer
        .then(|| footer_parts(snapshot, theme, inner));
    bottom.append(dashboard_chrome(
        snapshot,
        theme,
        inner,
        ui,
        plan.dashboard,
        folded_footer,
    ));
    bottom.append(bottom_corner_chrome(
        snapshot,
        theme,
        inner,
        plan.corner,
        plan.dashboard.is_some(),
    ));
    if plan.truth_notice
        && let Some(notice) = snapshot.truth_degraded.as_ref()
    {
        append_inert_lines(
            &mut bottom,
            truth_notice_lines(theme, notice, snapshot.now)
                .into_iter()
                .map(pad_chrome),
        );
    }
    if plan.gate_notice
        && let Some(notice) = ui.gate_notice.as_ref()
    {
        append_inert_lines(
            &mut bottom,
            gate_notice_lines(theme, notice).into_iter().map(pad_chrome),
        );
    }
    if plan.footer {
        let footer = footer_lines(snapshot, theme, inner);
        if !footer.is_empty() {
            // No rule above the footer — it sits quietly under the dashboard's
            // own top rule, with one blank line of breathing room when a
            // dashboard is present (skipped in an empty room so the footer
            // doesn't float).
            if !bottom.lines.is_empty() {
                bottom.push_inert(Line::from(""));
            }
            append_inert_lines(&mut bottom, footer.into_iter().map(pad_chrome));
        }
    }
    if plan.alert
        && let Some(alert) = alert
    {
        append_inert_lines(
            &mut bottom,
            alert_lines(theme, alert, snapshot.now)
                .into_iter()
                .map(pad_chrome),
        );
    }
    bottom
}

fn dashboard_chrome(
    snapshot: &SidebarSnapshot,
    theme: &Theme,
    inner: usize,
    ui: &UiState,
    mode: Option<DashboardMode>,
    folded_footer: Option<FooterParts>,
) -> RenderedBlock {
    let Some(mode) = mode else {
        return RenderedBlock::default();
    };
    // The pinned separator lifts the dashboard off the cards. It is part of
    // bottom chrome, so the viewport reserves it before windowing.
    let mut block = RenderedBlock::default();
    block.push_inert(Line::from(""));
    let active_tab = active_dashboard_tab(snapshot, ui);
    let mut panel = dashboard_block(DashboardContext {
        theme,
        providers: &snapshot.providers,
        active_provider: active_tab.as_deref(),
        mode,
        fleet_tally: snapshot.value_tally.as_ref(),
        pet: ui.pet.as_ref(),
        folded_footer,
        width: inner,
        zones: &snapshot.theme.display.budget_bar,
        now: snapshot.now,
        animation_phase: ui.animation_phase,
    });
    panel.map_lines(pad_chrome);
    panel.translate_columns(1);
    block.append(panel);
    block
}

fn bottom_corner_chrome(
    snapshot: &SidebarSnapshot,
    theme: &Theme,
    inner: usize,
    corner: BottomCorner,
    dashboard_present: bool,
) -> RenderedBlock {
    // The static `W:`/`M:` rows seal the main dashboard. The pet-enabled tall
    // provider block owns those totals inside its `Total:` section.
    let lines = match corner {
        BottomCorner::FleetTotal => fleet_total_lines(theme, snapshot.value_tally.as_ref(), inner),
        BottomCorner::FleetStore => fleet_store_lines(theme, snapshot.value_tally.as_ref(), inner),
        BottomCorner::None => return RenderedBlock::default(),
    };
    let mut block = RenderedBlock::default();
    if !lines.is_empty() {
        if !dashboard_present {
            block.push_inert(pad_chrome(hairline_rule(theme, inner)));
        }
        append_inert_lines(&mut block, lines.into_iter().map(pad_chrome));
    }
    block
}

fn append_inert_lines(block: &mut RenderedBlock, lines: impl IntoIterator<Item = Line<'static>>) {
    block.extend_inert(lines);
}

/// One draw's lines, typed interactions, and resolved zone positions.
pub(crate) struct ComposedFrame {
    pub(crate) lines: Vec<Line<'static>>,
    pub(crate) interactions: FrameInteractions,
    pub(crate) scroll_offset: usize,
    pub(crate) top_height: usize,
    pub(crate) bottom_height: usize,
}

fn resolve_scroll_offset(
    roster: &VisibleRoster<'_>,
    ui: &UiState,
    scroll_map: &[Option<usize>],
    scroll_len: usize,
    viewport: usize,
) -> usize {
    let max_offset = scroll_len.saturating_sub(viewport);
    let offset = ui.scroll_offset.min(max_offset);
    if ui.manual_scroll.is_some() {
        return offset;
    }
    let resolved = if ui.focus_group_reveal
        && let Some(group_first) = selected_group_first_ordinal(roster, ui.selected_index)
    {
        auto_scroll_reveal_group(scroll_map, group_first, ui.selected_index, offset, viewport)
    } else {
        auto_scroll_to_selection(scroll_map, ui.selected_index, offset, viewport)
    };
    resolved.min(max_offset)
}

/// True when the lead-unread row selected by the triage key has a line inside
/// the window `[offset, offset + viewport)`. The banner is the "get back to it"
/// affordance, so it shows only when this is false. A zero-height viewport or a
/// lead the make-up filter hides reads as not visible.
fn lead_unread_visible(
    roster: &VisibleRoster<'_>,
    lead_id: &str,
    scroll_map: &[Option<usize>],
    offset: usize,
    viewport: usize,
) -> bool {
    if viewport == 0 {
        return false;
    }
    let Some(ordinal) = roster.ordinal_of_id(lead_id) else {
        return false;
    };
    let end = (offset + viewport).min(scroll_map.len());
    scroll_map[offset..end].contains(&Some(ordinal))
}

/// Minimally nudge the viewport so the selected row's full line range — its
/// expanded subagent lines, and the group header when the first row of a group
/// is selected (both carry the row's map entry) — sits inside the window. A
/// fully visible selection moves nothing; a card taller than the viewport pins
/// its first line to the top; a selection with no lines in the scroll zone
/// holds the clamped offset.
pub(super) fn auto_scroll_to_selection(
    map: &[Option<usize>],
    selected: usize,
    offset: usize,
    viewport: usize,
) -> usize {
    if viewport == 0 {
        return offset;
    }
    let Some(first) = map.iter().position(|entry| *entry == Some(selected)) else {
        return offset;
    };
    let last = map
        .iter()
        .rposition(|entry| *entry == Some(selected))
        .unwrap_or(first);
    if last - first + 1 >= viewport {
        return first;
    }
    if first < offset {
        return first;
    }
    if last >= offset + viewport {
        return last + 1 - viewport;
    }
    offset
}

/// Minimally scroll so the selected row and its worktree header sit in view
/// together. `group_first` is the visible-row ordinal of the group's first row,
/// which the header line carries in the map. When that span outgrows the
/// viewport — or the header line can't be located — fall back to revealing the
/// card alone, so the focused row stays on-screen.
pub(super) fn auto_scroll_reveal_group(
    map: &[Option<usize>],
    group_first: usize,
    selected: usize,
    offset: usize,
    viewport: usize,
) -> usize {
    if viewport == 0 {
        return offset;
    }
    let Some(top) = map.iter().position(|entry| *entry == Some(group_first)) else {
        return auto_scroll_to_selection(map, selected, offset, viewport);
    };
    let Some(bottom) = map.iter().rposition(|entry| *entry == Some(selected)) else {
        return offset;
    };
    if bottom < top || bottom - top + 1 > viewport {
        return auto_scroll_to_selection(map, selected, offset, viewport);
    }
    if top < offset {
        top
    } else if bottom >= offset + viewport {
        bottom + 1 - viewport
    } else {
        offset
    }
}

/// First visible ordinal of the group containing `selected`.
fn selected_group_first_ordinal(roster: &VisibleRoster<'_>, selected: usize) -> Option<usize> {
    let group = roster.group_containing(selected)?;
    (group.source().kind != SidebarWorktreeKind::External).then(|| group.range().start)
}

/// The `↑ N need you` jump banner, toned by the lead's status (`failed` the
/// alarm red, else the caution warn). Rendered only while the lead is scrolled
/// out of view; its click scrolls the card list back to the top, where the lead
/// ranks.
fn unread_banner_line(theme: &Theme, lead_status: AgentStatus, count: usize) -> Line<'static> {
    let style = if lead_status == AgentStatus::Failed {
        theme.alarm(Modifier::empty())
    } else {
        theme.warn(Modifier::empty())
    };
    Line::styled(format!("↑ {count} need you"), style)
}

/// Ride the scrollbar on a visible scroll-zone line: the right rail is the last
/// sidebar column, already present on framed card lines and reserved by
/// chrome/help lines. Overflow trims the line to the column before the rail,
/// then writes the track or thumb glyph into that rail instead of appending
/// another column. Framed cards lose their spine rail; over-wide chrome keeps
/// its clipped text instead of losing a whole span. The solid `▐` thumb against
/// the hairline `▕` track carries the position by shape, so it survives
/// `NO_COLOR`.
pub(super) fn with_scrollbar(
    mut line: Line<'static>,
    theme: &Theme,
    cells: usize,
    row: usize,
    offset: usize,
    scroll_len: usize,
    viewport: usize,
) -> Line<'static> {
    let (thumb_start, thumb_len) = scroll_thumb(offset, scroll_len, viewport);
    let rail_column = cells.saturating_sub(1);
    line.spans = trim_spans_to_width(line.spans, rail_column);
    let pad = rail_column.saturating_sub(line.width());
    if pad > 0 {
        line.spans.push(Span::raw(" ".repeat(pad)));
    }
    let in_thumb = (thumb_start..thumb_start + thumb_len).contains(&row);
    line.spans.push(if in_thumb {
        Span::styled(labels::scroll_thumb_glyph(theme), theme.muted())
    } else {
        Span::styled(labels::scroll_track_glyph(theme), theme.rule())
    });
    line
}

/// Proportional thumb geometry over a `viewport`-tall track: the thumb's length
/// scales with how much of the zone is visible (never below one row) and its
/// start maps the offset across the track, reaching the last row exactly at the
/// bottom — so "at the top" and "at the bottom" always read true. Caller
/// guarantees `scroll_len > viewport > 0`.
pub(super) fn scroll_thumb(offset: usize, scroll_len: usize, viewport: usize) -> (usize, usize) {
    let thumb_len = (viewport * viewport / scroll_len).clamp(1, viewport);
    let max_start = viewport - thumb_len;
    let max_offset = scroll_len - viewport;
    let thumb_start = if offset >= max_offset {
        max_start
    } else {
        offset * max_start / max_offset
    };
    (thumb_start, thumb_len)
}

/// Compose the top-pinned cockpit zone and its typed interactions in lockstep.
/// Populated rooms end this fixed zone with a separator blank, so scrolled
/// cards never touch the cockpit make-up line.
/// Identity, summary, and the make-up line are never jump targets, so they map
/// to `None`; the make-up line's status buckets and the summary's unread and
/// open-PR counts are *filter* targets, returned as [`HitRegion`]s already
/// translated to this zone's line indices and the chrome-gutter column space.
/// Fixed height for a given room population, never windowed, so the scroll zone
/// below starts at a stable row.
pub(super) fn top_lines(
    snapshot: &SidebarSnapshot,
    ui: &UiState,
    width: usize,
    theme: &Theme,
) -> RenderedBlock {
    // The whole sidebar is built one cell narrow on each side; chrome lines pick
    // up their blank gutter in `extend_inert`, the cards carry their own.
    let inner = content_width(width);

    // Borderless repo header: the workspace name and its path behind their
    // glyphs, a blank line, then the cockpit summary and spend lines and a faint
    // hairline rule sealing the header from the cockpit. Inert chrome, so every
    // line maps to `None`.
    let mut header = repo_header_lines(theme, snapshot, inner);
    // A blank line sets the repo identity apart from the cockpit summary below.
    header.push(Line::from(""));
    // The cockpit summary, two lines: line 1 is `◎` sessions in the configured
    // headline window on the left with its token breakdown pinned right; line 2
    // is `¤` live agents, unread agents, and open lane PRs on the left with
    // headline spend pinned right. The counts read from the live fleet and the
    // JSONL `workspace_value_tally`'s headline window, so the cockpit reflects
    // this room's sessions rather than account-global provider history.
    let headline = snapshot
        .workspace_value_tally
        .as_ref()
        .map(|tally| &tally.headline);
    let sessions = headline.map(|window| window.sessions).unwrap_or(0);
    header.push(cockpit_summary_line(theme, sessions, headline, inner));
    // Line 2 is always present — an empty room reads `¤ 0` — with spend on the
    // right edge and counting up as a turn lands. The roll targets the live
    // overlay when the snapshot carries one — walked headline USD with live
    // card sessions excluded plus their current costs — and falls back to the
    // tally on a pre-overlay snapshot.
    let live_agents = fleet_size(&snapshot.worktree_groups).0;
    let unread_agents = BodyFilter::Unread.total(&snapshot.worktree_groups);
    let open_prs = open_pr_total(&snapshot.worktree_groups);
    let open_pr_ci = open_pr_worst_ci(&snapshot.worktree_groups);
    let tripped = snapshot
        .fleet_budget
        .as_ref()
        .filter(|budget| budget.parked);
    let (today_usd, spend_epoch) = cockpit_spend_target(snapshot).unwrap_or((0.0, None));
    let today_usd = ui.spend_ratchet.display(spend_epoch, today_usd);
    let spend_line = header.len();
    let unread_picked = ui.make_up_filter == Some(BodyFilter::Unread);
    let (spend, chip_hits) = cockpit_spend_line(
        theme,
        live_agents,
        CockpitBadges {
            unread_agents,
            unread_picked,
            open_prs,
            open_pr_ci,
            pr_picked: ui.make_up_filter == Some(BodyFilter::OpenPr),
        },
        (today_usd, tripped),
        &ui.tally,
        ui.animation_phase,
        inner,
    );
    header.push(spend);
    header.push(hairline_rule(theme, inner));
    let header_len = header.len();
    let mut cockpit_hits = Vec::with_capacity(2);
    if let Some((start, end)) = chip_hits.unread {
        cockpit_hits.push(HitRegion::line(
            spend_line,
            start + 1..end + 1,
            HitTarget::BodyFilter(BodyFilter::Unread),
        ));
    }
    if let Some((start, end)) = chip_hits.open_pr {
        cockpit_hits.push(HitRegion::line(
            spend_line,
            start + 1..end + 1,
            HitTarget::BodyFilter(BodyFilter::OpenPr),
        ));
    }
    let mut top = RenderedBlock::from_parts(
        header.into_iter().map(pad_chrome).collect(),
        vec![None; header_len],
        cockpit_hits,
    );

    // The fleet header (the cockpit make-up line) is always present and a fixed
    // height — one line for a populated room, none for an empty one — so the body
    // below never shifts vertically as agents change state. It is chrome, never a
    // jump target, so every header line maps to `None` in the row map; its
    // filter targets carry their own hit map instead, translated here onto the
    // zone's line index and into the `pad_chrome` gutter's column space.
    let lead_unread_status = lead_unread(&snapshot.worktree_groups).map(|(_, status)| status);
    let (fleet_lines, fleet_hits) = fleet_header_lines(
        theme,
        &snapshot.worktree_groups,
        snapshot.now,
        ui.make_up_filter,
        ui.animation_phase,
        inner,
        lead_unread_status,
    );
    let fleet_len = fleet_lines.len();
    let mut fleet = RenderedBlock::from_parts(
        fleet_lines.into_iter().map(pad_chrome).collect(),
        vec![None; fleet_len],
        fleet_hits,
    );
    fleet.translate_columns(1);
    top.append(fleet);
    if !snapshot.worktree_groups.is_empty() {
        top.push_inert(Line::from(""));
    }
    top
}

/// Compose the scrollable agent-cards zone and, in lockstep, its hit-test map:
/// every content line gets one map entry, `Some(row)` for an agent/process row
/// line and the worktree header that jumps into it, `None` for structural
/// chrome (gaps, the external divider, `+K more`).
/// Populated rooms take their opening gap from the pinned cockpit separator;
/// empty rooms keep the scroll zone clear. [`compose_lines`] windows this zone
/// by the scroll offset and pins the cockpit above it and the bottom chrome
/// below.
/// The single highest-priority unread row that warrants the continuous attention
/// signal: the oldest (min `last_activity`) unread row that needs an *answer* —
/// `waiting` or `failed`. Only this row keeps the configured shimmer/blink; every
/// other unread row (an unread `✓` result is a look, not an act) settles to the
/// steady bright crest, so the one pane that most needs you is the only thing in
/// continuous motion. Computed over the whole unfiltered roster — the lead is a
/// global attention fact, like the cockpit buckets — so a make-up filter never
/// shifts it, and it mirrors the `␣` triage head (oldest actionable first).
/// `None` when nothing unread needs an answer.
pub(super) fn lead_unread(groups: &[SidebarWorktreeGroup]) -> Option<(&str, AgentStatus)> {
    lead_unread_row(groups).map(|row| {
        let status = row
            .status()
            .expect("a lead unread row is actionable, so it carries a status");
        (row.id.as_str(), status)
    })
}

pub(super) fn scroll_lines(
    snapshot: &SidebarSnapshot,
    ui: &UiState,
    roster: &VisibleRoster<'_>,
    width: usize,
    theme: &Theme,
    mut meter_pixels: Option<&mut MeterPixels>,
) -> RenderedBlock {
    let mut block = RenderedBlock::default();

    if !snapshot.worktree_groups.is_empty() {
        let lead_unread_id = lead_unread(&snapshot.worktree_groups).map(|(id, _)| id);
        let ctx = RowCtx {
            theme,
            now: snapshot.now,
            width,
            tier: Tier::for_width(content_width(width)),
            bands: &snapshot.theme.display.context_meter,
            card_density: snapshot.theme.display.card_density,
            selected_index: ui.selected_index,
            animation_phase: ui.animation_phase,
            cost_rolls: &ui.cost_rolls,
            lead_unread: lead_unread_id,
        };
        // A group the make-up filter empties is skipped whole — header,
        // rows, and separator — so the filtered body holds only worktrees
        // with a matching row. A finished pod whose collapse hides every row
        // still renders its header and collapsed roster toggle.
        let mut emitted = false;
        for group in roster.groups() {
            if group.is_empty() && group.hidden_count() == 0 {
                continue;
            }
            if emitted {
                block.push_inert(Line::from(""));
            }
            emitted = true;
            block.append(worktree_group_lines_projected(WorktreeRenderContext {
                row: &ctx,
                roster,
                group,
                meter_pixels: meter_pixels.as_deref_mut(),
            }));
        }
    }

    block
}

/// Append structural (non-row) lines, tagging each map slot `None` and opening
/// each with a blank one-cell gutter so chrome breathes in the same one-cell
/// frame as the cards (which carry their own gutter via `with_gutter`).
/// Open a chrome line with the same one-cell blank left gutter the cards carry,
/// so the whole sidebar sits inside a one-cell frame and reserves the trailing
/// right rail. A genuinely empty line (a blank separator, or the cockpit's
/// reserved-but-empty totals slot) is left as is, so it stays zero-width and
/// reads as a true blank row. A line-level style (a `Line::styled` hairline or
/// help line) is patched into the rebuilt spans — the same carry the cards'
/// `with_gutter` does — so the rebuild never silently strips a tone.
pub(super) fn pad_chrome(line: Line<'static>) -> Line<'static> {
    if line.spans.iter().all(|span| span.content.is_empty()) {
        return line;
    }
    let base = line.style;
    let mut spans = Vec::with_capacity(line.spans.len() + 1);
    spans.push(Span::raw(" "));
    spans.extend(
        line.spans
            .into_iter()
            .map(|span| Span::styled(span.content, base.patch(span.style))),
    );
    Line::from(spans)
}
