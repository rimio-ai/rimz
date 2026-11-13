use std::collections::BTreeSet;
use std::ops::Range;

use crate::agents::AgentStatus;
use crate::config::{GlyphRole, ScrollbarMode};
use crate::sidebar_pane::pixel::meter::MeterPixels;
use crate::sidebar_pane::view::VisibleRoster;
use crate::store::snapshot::{
    SidebarInactive, SidebarRow, SidebarSnapshot, SidebarWorktreeGroup, SkippedSettings,
    actionable_unread_count, lead_unread_row,
};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::chrome::{
    FooterParts, alert_lines, footer_lines, footer_parts, gate_notice_lines, hairline_rule,
    hook_drainer_alert_lines, key_row_line, pane_notice_line, repo_header_lines, search_line,
    skipped_settings_line, truth_notice_lines,
};
use super::layout::{pad_line_to, text_width, trim_spans_to_width};
use super::sections::{
    CardWalk, CockpitBadges, DashboardContext, RowCtx, Tier, WorktreeRenderContext,
    cockpit_spend_line, cockpit_summary_line, content_width, dashboard_block, engaged_base_lines,
    fleet_header_lines, fleet_size, fleet_store_lines, fleet_total_lines, is_fresh,
    open_pr_attention, open_pr_total, worktree_group_lines_windowed,
};
use super::theme::{Component, Theme};
use super::{
    Alert, BodyFilter, DashboardMode, FrameInteractions, HitRegion, HitTarget, RenderedBlock,
    ScrollEase, UiState, ViewState, active_dashboard_tab, cockpit_spend_target, dashboard_mode,
    dashboard_present, labels, selected_sub_provider,
};

const INACTIVE_PAGE_SIZE: usize = 10;

#[cfg(test)]
pub(in crate::sidebar_pane) fn compose_lines(
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    view: &ViewState,
    ui: &UiState,
    theme: &Theme,
    width: u16,
    height: u16,
) -> ComposedFrame {
    compose_lines_with_meter(snapshot, alert, view, ui, theme, width, height, None, None)
}

/// Lay out the frame as three vertical zones: the top-pinned cockpit (identity,
/// summary, make-up line, the conditional unread banner, and fixed separator),
/// a scroll viewport over the agent cards, and the bottom chrome pinned to the
/// bottom edge like a status bar — the search line, provider dashboard, store, centered
/// navigation footer, and beneath them the drainer and refresh alerts. Space for the
/// pinned zones is always reserved — including the fixed separators under the
/// cockpit and above the provider dashboard — so the scroll zone is windowed
/// before either pinned edge is ever clipped. While a refresh alert is *active* the
/// body is a stale/empty fetch, so the footer steps aside and the alert speaks
/// beneath any committed search.
///
/// The viewport target is `ViewState::scroll_offset`, resolved here each frame:
/// anchored to the selected or topmost surviving card from the previous target
/// window, then clamped to the zone. Only the sidebar's own input reveals a
/// changed card selection when no card line is visible, or an entry selection
/// whose lines are clipped inside an already visible card. A birth reveal shows a
/// newborn under the focus whole beneath its group header, gliding when short, and
/// its hold anchors that header until the card leaves `Fresh`; births above a view
/// resting at the top leave it there.
/// Wheel, banner, and toggle inputs make the offset authoritative once; opening
/// the inactive chip also reveals its first newly shown receipt, and the banner
/// reveals the full-roster lead where it sits. The unread
/// banner decides from the target; the cards, scrollbar, and hit geometry use
/// the visible row from `ViewState::scroll_ease`, clamped to that viewport. Both
/// offsets are returned for the caller to write back, a draw byproduct like the
/// hit-test map. When the cards overflow the viewport, each visible scroll line
/// carries a track/thumb glyph in the right rail column — part of the composed
/// line, so every consumer of the frame sees it.
///
/// A cold height epoch paints the scroll zone once while filling card heights.
/// A warm frame measures it with cached heights and row-owning placeholders,
/// then paints the eased viewport in a second walk; a measurement miss carries
/// its lines into that walk, so no card is painted twice. Both walks use the
/// previous selections' expansions, or the carried opening set on a shared
/// landing. An armed layout freeze retains them regardless of geometry or
/// authoritative scrolling; its expiry releases them together with the frozen
/// order.
/// The borrowed client view supplies zone-relative anchor rows, openings, and toggles.
/// A ready off-screen focus marker overwrites an edge line only after layout and placement capture.
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
/// every clip and every scroll position: the frame is painted without wrapping,
/// so line `i` is screen row `i`. A line meant to take several rows is split into
/// rows where it is built, and a line wider than the pane is cut at its edge.
#[expect(
    clippy::too_many_arguments,
    reason = "lend pane-local caches independently of immutable UI state"
)]
pub(super) fn compose_lines_with_meter(
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    view: &ViewState,
    ui: &UiState,
    theme: &Theme,
    width: u16,
    height: u16,
    mut meter_pixels: Option<&mut MeterPixels>,
    heights: Option<&mut super::heights::CardHeights>,
) -> ComposedFrame {
    // One `Theme` per frame, handed to the body and the bottom chrome alike:
    // the cached `NO_COLOR` reading plus the palette the producer resolved from
    // `[theme]` onto the snapshot — so a re-themed config lands with the next
    // snapshot, identically on every renderer of the workspace.
    let cells = usize::from(width.max(1));
    // The whole sidebar sits inside a one-cell frame: chrome is built to the inner
    // width and opened with a blank gutter, reserving the trailing right rail —
    // the same frame the cards carry (see `with_gutter`).
    let roster = view.visible_roster(snapshot);
    let mut walk = CardWalk::new(heights);
    #[cfg(test)]
    if let Some(cache) = walk.heights.as_deref_mut() {
        cache.painted.clear();
    }
    let mut top = top_lines(snapshot, view, cells, theme);

    // The tab hits arrive from the bottom chrome relative to its own lines;
    // they are translated to absolute screen coordinates once the block's final
    // position is known, below.
    let bottom = build_bottom_chrome(
        snapshot,
        alert,
        theme,
        cells,
        view,
        ui,
        &roster,
        meter_pixels.as_deref_mut(),
    );

    let height = usize::from(height);
    let bottom_height = bottom.lines.len().min(height);

    let held_expansions = view.effective_held_expansions(&roster);
    let ScrollContent {
        block: scroll,
        inactive_span,
        group_spans,
    } = scroll_lines_windowed(
        snapshot,
        view,
        ui,
        &roster,
        &held_expansions,
        cells,
        theme,
        meter_pixels.as_deref_mut(),
        &mut walk,
    );
    let layout = plan_zones(ZoneInputs {
        snapshot,
        view,
        ui,
        roster: &roster,
        scroll_map: scroll.interactions.row_map(),
        scroll_interactions: &scroll.interactions,
        card_starts: &scroll.card_starts,
        group_spans: &group_spans,
        inactive_span,
        scroll_len: scroll.lines.len(),
        top_len: top.lines.len(),
        height,
        bottom_height,
    });
    let scroll_len = scroll.lines.len();
    let placements = |offset, top: usize| {
        scroll
            .card_starts
            .iter()
            .filter_map(|&(ordinal, first)| {
                if !card_visible(
                    scroll.interactions.row_map(),
                    ordinal,
                    first,
                    offset,
                    layout.viewport,
                ) {
                    return None;
                }
                roster.row(ordinal).map(|row| {
                    (
                        row.id.clone(),
                        top as isize + first as isize - offset as isize,
                    )
                })
            })
            .collect()
    };
    let painted_rows = placements(layout.offset, layout.top_shown);
    let anchored_rows = placements(layout.resolution.offset, 0);
    let laid_out = view
        .newborns
        .iter()
        .filter(|(id, born)| {
            !born.laid_out
                && roster
                    .ordinal_of_id(id)
                    .is_some_and(|ordinal| scroll.interactions.row_map().contains(&Some(ordinal)))
        })
        .map(|(id, _)| id.clone())
        .collect();
    let target_window =
        layout.resolution.offset..layout.resolution.offset.saturating_add(layout.viewport);
    let group_placements = group_spans
        .iter()
        .filter(|(_, span)| {
            !target_window.is_empty()
                && span.start < target_window.end
                && target_window.start < span.end
        })
        .map(|(index, span)| {
            (
                roster.groups()[*index].source().key.clone(),
                span.start as isize - target_window.start as isize,
            )
        })
        .collect();
    let marker = view
        .selected_index
        .filter(|_| view.focus_marker_ready && layout.viewport > 0)
        .and_then(|ordinal| {
            let &(_, first) = scroll
                .card_starts
                .iter()
                .find(|&&(index, _)| index == ordinal)?;
            if card_visible(
                scroll.interactions.row_map(),
                ordinal,
                first,
                layout.offset,
                layout.viewport,
            ) {
                return None;
            }
            let above = first < layout.offset;
            if above && layout.show_banner {
                return None;
            }
            let row = roster.row(ordinal)?;
            let glyph = theme.glyph(if above {
                GlyphRole::ChromeFocusAbove
            } else {
                GlyphRole::ChromeFocusBelow
            });
            let name_style = if row.as_agent().is_some() {
                theme.body_brand(theme.provider_brand_tone(&row.name))
            } else {
                theme.body()
            };
            let spans = trim_spans_to_width(
                vec![
                    Span::raw(" "),
                    Span::styled(format!("{glyph} "), theme.muted()),
                    Span::styled(row.display_name().to_owned(), name_style),
                ],
                cells.saturating_sub(1),
            );
            Some((above, Line::from(spans)))
        });
    let lead_above = lead_unread_row(&snapshot.worktree_groups)
        .and_then(|lead| roster.ordinal_of_id(&lead.id))
        .and_then(|ordinal| {
            scroll
                .card_starts
                .iter()
                .find(|(index, _)| *index == ordinal)
        })
        .is_none_or(|(_, first)| *first < layout.offset);
    insert_unread_banner(layout.show_banner, lead_above, snapshot, theme, &mut top);
    top.tag("cockpit");
    let scroll = if walk.heights.is_some() && walk.window.is_empty() {
        walk.window = layout.offset..layout.offset.saturating_add(layout.viewport);
        walk.measuring = false;
        let painted = scroll_lines_windowed(
            snapshot,
            view,
            ui,
            &roster,
            &held_expansions,
            cells,
            theme,
            meter_pixels.as_deref_mut(),
            &mut walk,
        );
        debug_assert_eq!(
            scroll.interactions.row_map(),
            painted.block.interactions.row_map()
        );
        debug_assert_eq!(scroll.card_starts, painted.block.card_starts);
        debug_assert_eq!(inactive_span, painted.inactive_span);
        debug_assert_eq!(group_spans, painted.group_spans);
        debug_assert_eq!(scroll.lines.len(), painted.block.lines.len());
        painted.block
    } else {
        scroll
    };
    let mut scroll_block = visible_scroll_block(scroll, snapshot, view, theme, cells, layout);
    if let Some((above, line)) = marker
        && !scroll_block.lines.is_empty()
    {
        let edge = if above {
            0
        } else {
            scroll_block.lines.len() - 1
        };
        scroll_block.replace_line_with_target(edge, line, HitTarget::FocusMarker);
    }

    let mut frame = top.window(0, layout.top_shown);
    frame.append(scroll_block);
    let content_height = frame.lines.len().saturating_add(bottom_height);
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
        sections: frame.sections,
        interactions: frame.interactions,
        scroll_offset: layout.resolution.offset,
        visible_scroll_offset: layout.offset,
        scroll_max: scroll_len.saturating_sub(layout.viewport),
        top_height: layout.top_shown,
        bottom_height,
        content_height,
        painted_rows,
        anchored_rows,
        group_placements,
        drawn_selection: view
            .selected_index
            .and_then(|index| roster.row(index))
            .map(|row| row.id.clone()),
        held_expansions,
        banner_shown: layout.show_banner,
        anchor_clamped: layout.resolution.clamped,
        birth_hold: next_birth_hold(view, &roster),
        laid_out,
        correction: layout.resolution.correction,
        scroll_ease: layout.scroll_ease,
    }
}

#[derive(Clone, Copy)]
struct ZoneLayout {
    show_banner: bool,
    top_shown: usize,
    viewport: usize,
    offset: usize,
    resolution: ScrollResolution,
    scroll_ease: ScrollEase,
}

#[derive(Clone, Copy)]
struct ScrollResolution {
    offset: usize,
    correction: isize,
    clamped: bool,
    snap: bool,
    /// A birth reveal moved the offset by at most one viewport: it glides there.
    glide: bool,
}

struct ZoneInputs<'a, 'snapshot> {
    snapshot: &'snapshot SidebarSnapshot,
    view: &'a ViewState,
    ui: &'a UiState,
    roster: &'a VisibleRoster<'snapshot>,
    scroll_map: &'a [Option<usize>],
    scroll_interactions: &'a FrameInteractions,
    card_starts: &'a [(usize, usize)],
    group_spans: &'a [(usize, Range<usize>)],
    inactive_span: Option<(usize, usize)>,
    scroll_len: usize,
    top_len: usize,
    height: usize,
    bottom_height: usize,
}

fn plan_zones(input: ZoneInputs<'_, '_>) -> ZoneLayout {
    let after_bottom = input.height.saturating_sub(input.bottom_height);
    let top_without_banner = input.top_len.min(after_bottom);
    let viewport_without_banner = after_bottom - top_without_banner;
    let offset_without_banner = resolve_scroll_offset(&input, viewport_without_banner, false);
    let show_banner = lead_unread_row(&input.snapshot.worktree_groups).is_some_and(|lead| {
        viewport_without_banner > 0
            && !lead_unread_visible(
                input.roster,
                &lead.id,
                input.scroll_map,
                offset_without_banner.offset,
                viewport_without_banner,
            )
    });
    let top_shown = input
        .top_len
        .saturating_add(usize::from(show_banner))
        .min(after_bottom);
    let viewport = after_bottom - top_shown;
    let resolution = if show_banner {
        resolve_scroll_offset(&input, viewport, true)
    } else {
        offset_without_banner
    };
    let mut scroll_ease = input.view.scroll_ease;
    scroll_ease.shift(resolution.correction);
    if resolution.snap {
        scroll_ease.cancel();
    }
    if resolution.glide {
        scroll_ease.arm_birth();
    }
    ZoneLayout {
        show_banner,
        top_shown,
        viewport,
        offset: scroll_ease
            .visible(resolution.offset, input.view.frame_ms)
            .min(input.scroll_len.saturating_sub(viewport)),
        resolution,
        scroll_ease,
    }
}

fn insert_unread_banner(
    show: bool,
    above: bool,
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
        pad_chrome(unread_banner_line(theme, status, count, above)),
        HitTarget::UnreadBanner,
    );
    with_banner.push_inert(separator);
    *top = with_banner;
}

fn visible_scroll_block(
    scroll: RenderedBlock,
    snapshot: &SidebarSnapshot,
    view: &ViewState,
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
    let mut scrollbar = view.scrollbar;
    scrollbar.shift(layout.resolution.correction);
    let show_bar = match snapshot.theme.display.scrollbar {
        ScrollbarMode::Always => true,
        ScrollbarMode::Never => false,
        ScrollbarMode::Auto => {
            scrollbar.moved_from(layout.offset)
                || scrollbar.visible(view.frame_ms, snapshot.theme.display.resolved_refresh_ms())
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

/// Bottom-pinned chrome, top to bottom: the search line or a fixed separator when the provider
/// dashboard is present, the per-provider dashboard (account-scoped budgets +
/// brand emblem, which opens with its own top hairline — the tab rail when
/// several accounts register), the fallback fleet store for no-table layouts,
/// the navigation footer (centered), then the drainer and refresh alerts. While a
/// refresh alert is active the body is a stale/empty fetch, so the panel and footer
/// step aside and that alert speaks beneath any committed search. Every chrome line
/// is gutter-padded so it breathes in the same one-cell frame as the body.
#[derive(Clone, Copy)]
enum BottomCorner {
    FleetTotal,
    FleetStore,
    None,
}

/// Selects alert-only, dashboard-folded, dashboard-with-totals, or bare-store
/// chrome before rendering any lines.
struct BottomPlan<'a> {
    dashboard: Option<DashboardMode>,
    folded_footer: bool,
    corner: BottomCorner,
    truth_notice: bool,
    gate_notice: bool,
    skipped_notice: Option<&'a SkippedSettings>,
    footer: bool,
    alert: bool,
}

/// The skipped-settings list this frame draws a notice for: none while an alert is active,
/// when nothing was skipped, or once this view dismissed the same list.
pub(super) fn shown_skipped<'a>(
    snapshot: &'a SidebarSnapshot,
    alert: Option<&Alert>,
    view: &ViewState,
) -> Option<&'a SkippedSettings> {
    if alert.is_some_and(Alert::is_active) {
        return None;
    }
    snapshot
        .skipped_settings
        .as_ref()
        .filter(|skipped| view.dismissed_skipped.as_deref() != Some(skipped.digest.as_str()))
}

fn plan_bottom_chrome<'a>(
    snapshot: &'a SidebarSnapshot,
    alert: Option<&Alert>,
    view: &ViewState,
    ui: &UiState,
) -> BottomPlan<'a> {
    let skipped_notice = shown_skipped(snapshot, alert, view);
    let alert_active = alert.is_some_and(Alert::is_active);
    let dashboard = dashboard_present(snapshot, alert_active).then(|| dashboard_mode(snapshot));
    let owns_store = dashboard.is_some_and(|mode| mode.owns_store(!snapshot.providers.is_empty()));
    let folded_footer = owns_store
        && snapshot.truth_degraded.is_none()
        && ui.gate_notice.is_none()
        && skipped_notice.is_none()
        && snapshot.throttle_notices.is_empty()
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
        skipped_notice,
        footer: !alert_active && !folded_footer,
        alert: alert.is_some(),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "lend the frame's meter images beside the immutable frame inputs"
)]
pub(super) fn build_bottom_chrome(
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    theme: &Theme,
    cells: usize,
    view: &ViewState,
    ui: &UiState,
    roster: &VisibleRoster<'_>,
    meter_pixels: Option<&mut MeterPixels>,
) -> RenderedBlock {
    let plan = plan_bottom_chrome(snapshot, alert, view, ui);
    let inner = content_width(cells);
    let mut header = RenderedBlock::default();
    if let Some(line) = search_line(theme, view, roster, snapshot, cells) {
        header.push_inert(line);
    } else if plan.dashboard.is_some() {
        header.push_inert(Line::from(""));
    }
    let mut bottom = RenderedBlock::default();
    let folded_footer = plan.folded_footer.then(|| footer_parts(snapshot, theme));
    bottom.append(dashboard_chrome(
        snapshot,
        theme,
        inner,
        view,
        ui,
        plan.dashboard,
        folded_footer,
        meter_pixels,
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
        bottom.extend_inert(notice_rows(
            truth_notice_lines(theme, notice, snapshot.now),
            inner,
        ));
    }
    if plan.gate_notice
        && let Some(notice) = ui.gate_notice.as_ref()
    {
        bottom.extend_inert(notice_rows(gate_notice_lines(theme, notice), inner));
    }
    if let Some(skipped) = plan.skipped_notice {
        let notice = skipped_settings_line(theme, skipped.count, &snapshot.keys);
        bottom.extend_inert(notice_rows(vec![notice], inner));
    }
    if !alert.is_some_and(Alert::is_active) {
        let notices = snapshot
            .throttle_notices
            .iter()
            .map(|notice| super::chrome::throttle_notice_line(theme, notice, inner))
            .collect();
        bottom.extend_inert(notice_rows(notices, inner));
    }
    if plan.footer {
        let footer = footer_lines(snapshot, theme, inner);
        if !footer.is_empty() {
            // No rule above the footer — it sits quietly under the dashboard's
            // own top rule, with one blank line of breathing room. A footer
            // never draws on an empty block: without an alert the dashboard
            // or the fleet store's `W:`/`M:` rows always precede it.
            bottom.push_inert(Line::from(""));
            bottom.extend_inert(footer.into_iter().map(pad_chrome));
        }
    }
    if !alert.is_some_and(Alert::is_active) {
        bottom.extend_inert(notice_rows(
            hook_drainer_alert_lines(theme, snapshot, view.hook_drainer_alert.as_ref()),
            inner,
        ));
    }
    if plan.alert
        && let Some(alert) = alert
    {
        bottom.extend_inert(notice_rows(
            alert_lines(theme, alert, &snapshot.keys, snapshot.now),
            inner,
        ));
    }
    if let Some(notice) = &view.notice {
        bottom.extend_inert(notice_rows(
            vec![pane_notice_line(theme, notice, &snapshot.keys)],
            inner,
        ));
    }
    if !view.pending_keys.is_empty() || !ui.room_modes.is_empty() {
        bottom.push_inert(pad_chrome(key_row_line(
            theme,
            &snapshot.keys,
            &ui.room_modes,
            &view.pending_keys,
            inner,
        )));
    }
    header.append(bottom);
    header.tag("chrome");
    header
}

/// Notice or alert lines as chrome rows: split to the chrome's inner width, each row opened
/// with the gutter, since the frame never wraps a line onto the next row.
fn notice_rows(lines: Vec<Line<'static>>, inner: usize) -> impl Iterator<Item = Line<'static>> {
    lines
        .into_iter()
        .flat_map(move |line| super::layout::wrap_line(line, inner))
        .map(pad_chrome)
}

#[expect(
    clippy::too_many_arguments,
    reason = "lend the frame's meter images beside the immutable frame inputs"
)]
fn dashboard_chrome(
    snapshot: &SidebarSnapshot,
    theme: &Theme,
    inner: usize,
    view: &ViewState,
    ui: &UiState,
    mode: Option<DashboardMode>,
    folded_footer: Option<FooterParts>,
    pixels: Option<&mut MeterPixels>,
) -> RenderedBlock {
    let Some(mode) = mode else {
        return RenderedBlock::default();
    };
    let mut block = RenderedBlock::default();
    let active_tab = active_dashboard_tab(snapshot, view);
    let mut panel = dashboard_block(DashboardContext {
        theme,
        providers: &snapshot.providers,
        active_provider: active_tab.as_ref(),
        selected_sub_provider: selected_sub_provider(snapshot, view),
        last_sub_providers: &view.last_sub_providers,
        mode,
        fleet_tally: snapshot.value_tally.as_deref(),
        pet: ui.pet.as_ref(),
        folded_footer,
        width: inner,
        zones: &snapshot.theme.display.budget_bar,
        now: snapshot.now,
        redeem_breath: theme.animations.redeem_breathes().then(|| {
            super::animation::redeem_breath_amount(
                view.cosmetic_phase.unwrap_or(view.animation_phase),
                snapshot.theme.display.resolved_refresh_ms(),
            )
        }),
        pixels,
        logo_reserve: ui.logo_reserve,
    });
    panel.map_lines(pad_chrome);
    panel.translate_columns(1);
    block.append(panel);
    block.tag("providers");
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
        BottomCorner::FleetTotal => {
            fleet_total_lines(theme, snapshot.value_tally.as_deref(), inner)
        }
        BottomCorner::FleetStore => {
            fleet_store_lines(theme, snapshot.value_tally.as_deref(), inner)
        }
        BottomCorner::None => return RenderedBlock::default(),
    };
    let mut block = RenderedBlock::default();
    if !lines.is_empty() {
        if !dashboard_present {
            block.push_inert(pad_chrome(hairline_rule(theme, inner)));
        }
        block.extend_inert(lines.into_iter().map(pad_chrome));
    }
    if dashboard_present {
        block.tag("providers");
    }
    block
}

/// One draw's lines, typed interactions, and resolved zone positions.
#[cfg_attr(test, derive(Debug, PartialEq))]
pub(in crate::sidebar_pane) struct ComposedFrame {
    pub(super) painted_rows: Vec<(String, isize)>,
    pub(super) anchored_rows: Vec<(String, isize)>,
    /// Drawn groups overlapping the target window, with signed zone-relative first lines.
    pub(super) group_placements: Vec<(String, isize)>,
    pub(super) drawn_selection: Option<String>,
    pub(super) held_expansions: BTreeSet<String>,
    pub(super) banner_shown: bool,
    pub(super) anchor_clamped: bool,
    pub(super) birth_hold: Option<String>,
    /// Newborns this draw laid out for the first time, marked on their records.
    pub(super) laid_out: Vec<String>,
    pub(super) correction: isize,
    pub(super) scroll_ease: ScrollEase,
    pub(in crate::sidebar_pane) lines: Vec<Line<'static>>,
    pub(super) sections: Vec<super::SectionLabel>,
    pub(in crate::sidebar_pane) interactions: FrameInteractions,
    pub(in crate::sidebar_pane) scroll_offset: usize,
    pub(super) visible_scroll_offset: usize,
    pub(in crate::sidebar_pane) scroll_max: usize,
    pub(in crate::sidebar_pane) top_height: usize,
    pub(in crate::sidebar_pane) bottom_height: usize,
    /// Rows occupied by composed content before viewport padding.
    pub(in crate::sidebar_pane) content_height: usize,
}

fn card_visible(
    map: &[Option<usize>],
    ordinal: usize,
    first: usize,
    offset: usize,
    viewport: usize,
) -> bool {
    map.get(offset.max(first)..offset.saturating_add(viewport).min(map.len()))
        .is_some_and(|lines| lines.contains(&Some(ordinal)))
}

fn resolve_scroll_offset(
    input: &ZoneInputs<'_, '_>,
    viewport: usize,
    show_banner: bool,
) -> ScrollResolution {
    let view = input.view;
    let max_offset = input.scroll_len.saturating_sub(viewport);
    let mut resolved = ScrollResolution {
        offset: view.scroll_offset.min(max_offset),
        correction: 0,
        clamped: false,
        snap: false,
        glide: false,
    };
    if view.viewport.offset_authoritative {
        let reveal = view.viewport.reveal_row.as_deref();
        if let Some(index) = reveal.and_then(|id| input.roster.ordinal_of_id(id)) {
            resolved.offset =
                auto_scroll_to_selection(input.scroll_map, index, resolved.offset, viewport)
                    .min(max_offset);
        }
        if view.viewport.inactive_reveal
            && let Some((chip, last)) = input.inactive_span
        {
            resolved.offset = if last - chip + 1 > viewport {
                chip
            } else {
                nudge_into_view(chip, last, resolved.offset, viewport)
            }
            .min(max_offset);
        }
        return resolved;
    }
    let selected = view
        .selected_index
        .and_then(|index| input.roster.row(index));
    let changed = selected.map(|row| row.id.as_str()) != view.drawn_selection.as_deref();
    resolved.snap = changed || view.pending_jump().is_some() || input.ui.drawn_scroll_max.is_none();
    let first_line = |id: &str| {
        let ordinal = input.roster.ordinal_of_id(id)?;
        input
            .card_starts
            .iter()
            .find_map(|&(row, first)| (row == ordinal).then_some(first))
    };
    let anchor = selected
        .and_then(|row| view.anchored_rows.iter().find(|(id, _)| id == &row.id))
        .or_else(|| {
            view.anchored_rows
                .iter()
                .find(|(id, _)| first_line(id).is_some())
        });
    let shared = view
        .pending_jump()
        .filter(|placement| selected.is_some_and(|row| row.id == placement.row_id))
        .and_then(|placement| {
            first_line(&placement.row_id)
                .map(|first| (first as isize).saturating_sub(placement.viewport_row))
        });
    let held_header = view
        .birth_hold
        .as_deref()
        .filter(|id| selected.is_some_and(|row| row.id == *id))
        .and_then(|id| {
            let (key, header) = group_header(input, first_line(id)?)?;
            Some((header, view.drawn_group_row(key).filter(|row| *row >= 0)?))
        });
    let target = shared.or_else(|| {
        let (line, y) = held_header.or_else(|| {
            let (id, y) = anchor?;
            Some((first_line(id)?, *y))
        })?;
        Some(
            (line as isize).saturating_sub(y) + isize::from(show_banner)
                - isize::from(view.anchor_banner_shown)
                - births_above(input, line) as isize,
        )
    });
    if let Some(target) = target {
        resolved.offset = target.clamp(0, max_offset as isize) as usize;
        resolved.clamped = resolved.offset as isize != target;
        if shared.is_none() {
            resolved.correction = resolved.offset as isize - view.scroll_offset as isize;
        }
    }
    let Some(selected_index) = view.selected_index.filter(|_| selected.is_some()) else {
        return resolved;
    };
    if let Some(row) = selected.filter(|row| reveals_birth(view, row))
        && let Some(first) = first_line(&row.id)
        && let Some(offset) =
            birth_reveal_offset(input, selected_index, first, resolved.offset, viewport)
    {
        let anchored = resolved.offset;
        resolved.offset = offset.min(max_offset);
        let moved = resolved.offset.abs_diff(anchored);
        resolved.glide = view.viewport.birth_reveal.as_deref() == Some(row.id.as_str())
            && moved > 0
            && moved <= viewport
            && input.ui.drawn_scroll_max.is_some()
            && !view.arrival_pending;
        resolved.snap &= !resolved.glide;
        return resolved;
    }
    if view.viewport.own_input_reveal
        && let (Some(super::EntryCursor::SubAgent(pane)), Some(_)) = (&view.entry_cursor, selected)
    {
        let target = HitTarget::SubAgentPane(pane.clone());
        if let Some(span) = input.scroll_interactions.target_rows(&target) {
            resolved.offset = nudge_into_view(
                span.start,
                span.end.saturating_sub(1),
                resolved.offset,
                viewport,
            )
            .min(max_offset);
            resolved.snap = true;
            return resolved;
        }
    }
    let visible = input
        .card_starts
        .iter()
        .find(|&&(ordinal, _)| ordinal == selected_index)
        .is_some_and(|&(_, first)| {
            card_visible(
                input.scroll_map,
                selected_index,
                first,
                resolved.offset,
                viewport,
            )
        });
    // A placement from a taller source zone can land below this one: reveal it.
    if visible || (!view.viewport.own_input_reveal && shared.is_none()) {
        return resolved;
    }
    resolved.offset =
        auto_scroll_to_selection(input.scroll_map, selected_index, resolved.offset, viewport)
            .min(max_offset);
    resolved
}

/// Whether this draw shows the newborn `row` whole beneath its header: the paint after the
/// focus reached it, and the paint on which the held card first draws past `Fresh`.
fn reveals_birth(view: &ViewState, row: &SidebarRow) -> bool {
    view.viewport.birth_reveal.as_deref() == Some(&row.id)
        || (view.birth_hold.as_deref() == Some(&row.id) && !is_fresh(row))
}

/// The card that stays held by its header on the next draw: a newborn revealed or held, while
/// it is selected and still `Fresh` and no input set the offset.
fn next_birth_hold(view: &ViewState, roster: &VisibleRoster<'_>) -> Option<String> {
    let row = view.selected_index.and_then(|index| roster.row(index))?;
    let held = view
        .viewport
        .birth_reveal
        .as_ref()
        .or(view.birth_hold.as_ref());
    (!view.viewport.offset_authoritative && held == Some(&row.id) && is_fresh(row))
        .then(|| row.id.clone())
}

/// The key and header line of the group drawing the card whose first line is `first`, when
/// that group draws a header which routes to a row.
fn group_header<'a>(input: &ZoneInputs<'a, '_>, first: usize) -> Option<(&'a str, usize)> {
    let (index, span) = input
        .group_spans
        .iter()
        .find(|(_, span)| span.contains(&first))?;
    let header = span.start;
    (header < first && input.scroll_map.get(header).is_some_and(Option::is_some))
        .then(|| (input.roster.groups()[*index].source().key.as_str(), header))
}

/// Lines above `line` that rows new to this view add on the first draw to lay them out,
/// while the last draw rested at the top: a birth there leaves the view at the top. A group
/// that maps rows, all of them such, counts whole with its gap; in another, its header line
/// was drawn before, and a team born into it adds its new pipeline line.
fn births_above(input: &ZoneInputs<'_, '_>, line: usize) -> usize {
    let view = input.view;
    if view.scroll_offset != 0 || view.newborns.is_empty() {
        return 0;
    }
    let unplaced = |ordinal: usize| {
        input
            .roster
            .row(ordinal)
            .and_then(|row| view.newborns.get(&row.id))
            .is_some_and(|born| !born.laid_out)
    };
    input
        .group_spans
        .iter()
        .filter(|(_, span)| span.start < line)
        .map(|(index, span)| {
            let lines = &input.scroll_map[span.clone()];
            let mut rows = lines.iter().flatten().peekable();
            let maps_rows = rows.peek().is_some();
            if span.end <= line
                && input.group_spans.len() > 1
                && maps_rows
                && rows.all(|&ordinal| unplaced(ordinal))
            {
                return span.len() + 1;
            }
            let end = line.min(span.end);
            let born = input.scroll_map[span.start + 1..end]
                .iter()
                .filter(|entry| entry.is_some_and(unplaced))
                .count();
            born + (span.start + 1..end)
                .filter(|&at| born_strip(input, *index, lines, at, &unplaced))
                .count()
        })
        .sum()
}

/// Whether scroll line `at` is the pipeline line of group `index` (span `lines`) that a team
/// born whole into the group brought with it.
fn born_strip(
    input: &ZoneInputs<'_, '_>,
    index: usize,
    lines: &[Option<usize>],
    at: usize,
    unplaced: &impl Fn(usize) -> bool,
) -> bool {
    let group = input.roster.groups()[index].source();
    if group.pipeline.is_none()
        || input.scroll_map[at].is_some()
        || !matches!(
            u16::try_from(at).map(|row| input.scroll_interactions.target_at(0, row)),
            Ok(Some(HitTarget::Row(_)))
        )
    {
        return false;
    }
    let mut team = lines
        .iter()
        .flatten()
        .filter(|&&ordinal| {
            input
                .roster
                .row(ordinal)
                .is_some_and(|row| row.team().is_some() && row.team() == group.team.as_deref())
        })
        .peekable();
    team.peek().is_some() && team.all(|&ordinal| unplaced(ordinal))
}

/// The offset showing the newborn card at `ordinal`, first line `first`, beneath its group
/// header with room for an Engaged card's base height, by the least move from `offset`. A
/// span taller than the view puts the header on top when no line between them maps a row
/// (a team leader's pipeline line may sit there), else shows the card alone at that height,
/// else the card's first line on top.
fn birth_reveal_offset(
    input: &ZoneInputs<'_, '_>,
    ordinal: usize,
    first: usize,
    offset: usize,
    viewport: usize,
) -> Option<usize> {
    if viewport == 0 {
        return None;
    }
    let last = input
        .scroll_map
        .iter()
        .rposition(|entry| *entry == Some(ordinal))?;
    let reserved = last.max(first + engaged_base_lines() - 1);
    let header = group_header(input, first).map(|(_, header)| header);
    let top = header.unwrap_or(first);
    Some(if reserved - top < viewport {
        nudge_into_view(top, reserved, offset, viewport)
    } else if let Some(header) = header.filter(|header| {
        input.scroll_map[header + 1..first]
            .iter()
            .all(Option::is_none)
    }) {
        header
    } else if reserved - first < viewport {
        nudge_into_view(first, reserved, offset, viewport)
    } else {
        first
    })
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

fn nudge_into_view(top: usize, bottom: usize, offset: usize, viewport: usize) -> usize {
    if bottom >= offset + viewport {
        bottom + 1 - viewport
    } else if top < offset {
        top
    } else {
        offset
    }
}

/// The directional `↑/↓ N need you` jump banner, toned by the lead's status (`failed` the
/// alarm red, else the caution warn). Rendered only while the lead is scrolled
/// out of view; its click minimally reveals the lead where it sits, or scrolls
/// to the top if the filter hides it.
fn unread_banner_line(
    theme: &Theme,
    lead_status: AgentStatus,
    count: usize,
    above: bool,
) -> Line<'static> {
    let style = if lead_status == AgentStatus::Failed {
        theme.alarm(Modifier::empty())
    } else {
        theme.warn(Modifier::empty())
    };
    let glyph = theme.glyph(if above {
        GlyphRole::ChromeFocusAbove
    } else {
        GlyphRole::ChromeFocusBelow
    });
    Line::styled(format!("{glyph} {count} need you"), style)
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
    line = pad_line_to(line, rail_column);
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
/// to `None`; the make-up line's status buckets and the summary's open-PR
/// count are *filter* targets, returned as [`HitRegion`]s already
/// translated to this zone's line indices and the chrome-gutter column space.
/// Fixed height for a given room population, never windowed, so the scroll zone
/// below starts at a stable row.
pub(super) fn top_lines(
    snapshot: &SidebarSnapshot,
    view: &ViewState,
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
    // is `¤` live agents and open lane PRs on the left with
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
    let open_prs = open_pr_total(&snapshot.worktree_groups);
    let open_pr_attention = open_pr_attention(&snapshot.worktree_groups);
    let tripped = snapshot
        .fleet_budget
        .as_ref()
        .filter(|budget| budget.parked);
    let (today_usd, spend_epoch) = cockpit_spend_target(snapshot).unwrap_or((0.0, None));
    let today_usd = view.spend_ratchet.display(spend_epoch, today_usd);
    let spend_line = header.len();
    let (spend, pr_hit) = cockpit_spend_line(
        theme,
        live_agents,
        CockpitBadges {
            open_prs,
            open_pr_attention,
            pr_picked: view.toggles.make_up_filter.filter == Some(BodyFilter::OpenPr),
        },
        (today_usd, tripped),
        &view.tally,
        (view.frame_ms, snapshot.theme.display.resolved_refresh_ms()),
        inner,
    );
    header.push(spend);
    header.push(hairline_rule(theme, inner));
    let header_len = header.len();
    let mut cockpit_hits = Vec::with_capacity(1);
    if let Some((start, end)) = pr_hit {
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
        &view.toggles.make_up_filter,
        view.cosmetic_phase.unwrap_or(view.animation_phase),
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
/// shifts it, and it mirrors the `n` triage head (oldest actionable first).
/// `None` when nothing unread needs an answer.
pub(super) fn lead_unread(groups: &[SidebarWorktreeGroup]) -> Option<(&str, AgentStatus)> {
    lead_unread_row(groups).map(|row| {
        let status = row
            .status()
            .expect("a lead unread row is actionable, so it carries a status");
        (row.id.as_str(), status)
    })
}

#[cfg(test)]
#[expect(
    clippy::too_many_arguments,
    reason = "lend pane resources separately from the client presentation"
)]
pub(super) fn scroll_lines(
    snapshot: &SidebarSnapshot,
    view: &ViewState,
    ui: &UiState,
    roster: &VisibleRoster<'_>,
    held_expansions: &BTreeSet<String>,
    width: usize,
    theme: &Theme,
    meter_pixels: Option<&mut MeterPixels>,
) -> (RenderedBlock, Option<(usize, usize)>) {
    let scroll = scroll_lines_windowed(
        snapshot,
        view,
        ui,
        roster,
        held_expansions,
        width,
        theme,
        meter_pixels,
        &mut CardWalk::new(None),
    );
    (scroll.block, scroll.inactive_span)
}

struct ScrollContent {
    block: RenderedBlock,
    inactive_span: Option<(usize, usize)>,
    group_spans: Vec<(usize, Range<usize>)>,
}

#[expect(
    clippy::too_many_arguments,
    reason = "one scroll walker serves full paints, measurement, and viewport paints"
)]
fn scroll_lines_windowed(
    snapshot: &SidebarSnapshot,
    view: &ViewState,
    ui: &UiState,
    roster: &VisibleRoster<'_>,
    held_expansions: &BTreeSet<String>,
    width: usize,
    theme: &Theme,
    mut meter_pixels: Option<&mut MeterPixels>,
    walk: &mut CardWalk<'_>,
) -> ScrollContent {
    let mut block = RenderedBlock::default();
    let mut inactive_span = None;
    let mut group_spans = Vec::new();

    let lead_unread_id = lead_unread(&snapshot.worktree_groups).map(|(id, _)| id);
    let ctx = RowCtx {
        theme,
        now: snapshot.now,
        typing_until: &snapshot.typing_until,
        width,
        tier: Tier::for_width(content_width(width)),
        bands: &snapshot.theme.display.context_meter,
        tool_repeat_warn_after: snapshot.attention.tool_repeat_warn_after.get(),
        stalled_after_secs: snapshot.attention.stalled_after_secs.get(),
        card_density: snapshot.theme.display.card_density,
        stale_after_secs: snapshot.theme.display.stale_after_secs,
        selected_index: view.selected_index,
        held_expansions,
        entry_cursor: view.entry_cursor.as_ref(),
        animation_phase: view.cosmetic_phase.unwrap_or(view.animation_phase),
        head_step: view.cosmetic_phase.unwrap_or(view.head_step),
        money_ms: view.frame_ms,
        refresh_ms: snapshot.theme.display.resolved_refresh_ms(),
        cost_rolls: &view.cost_rolls,
        lead_unread: lead_unread_id,
        delegation_overrides: &view.toggles.delegation_overrides,
    };
    if walk.measuring
        && let Some(cache) = walk.heights.as_deref_mut()
    {
        let theme = ui
            .cached_theme(&snapshot.theme)
            .filter(|cached| std::ptr::eq(cached.as_ref(), theme))
            .unwrap_or_else(|| std::rc::Rc::new(theme.clone()));
        let cold_epoch = cache.prepare(&snapshot.worktree_groups, theme, &ctx);
        if cold_epoch {
            // Every card needs measurement; retain that full paint instead of walking twice.
            walk.window = 0..usize::MAX;
        }
    }
    if !roster.groups().iter().any(|group| group.shown())
        && view
            .toggles
            .search_draft
            .as_deref()
            .or(view.toggles.make_up_filter.query.as_deref())
            .is_some_and(|query| !query.is_empty())
    {
        block.push_inert(pad_chrome(Line::styled("no match", theme.muted())));
        block.tag("chrome");
        return ScrollContent {
            block,
            inactive_span,
            group_spans,
        };
    }
    if !snapshot.worktree_groups.is_empty() {
        let hide_inactive_chip = view.toggles.make_up_filter.filter.is_some()
            || view
                .toggles
                .search_draft
                .as_ref()
                .map_or(view.toggles.make_up_filter.query.is_some(), |draft| {
                    !draft.is_empty()
                });
        let inactive_count = snapshot
            .worktree_groups
            .iter()
            .filter(|group| group.inactive.is_some())
            .count();
        let unlanded_count = snapshot
            .worktree_groups
            .iter()
            .filter(|group| group.inactive == Some(SidebarInactive::Unlanded))
            .count();
        let mut inactive_chip = None;
        let page_limit = view.toggles.inactive_peek.as_ref().map_or(0, |peek| {
            peek.pages.get().saturating_mul(INACTIVE_PAGE_SIZE)
        });
        let mut inactive_seen = 0;
        let mut older = 0;
        let mut emitted = false;
        for (index, group) in roster.groups().iter().enumerate() {
            if !group.shown() {
                continue;
            }
            if group.source().inactive.is_none() && older > 0 {
                block.append(inactive_older_line(theme, older));
                older = 0;
            }
            if group.source().inactive.is_some() && !hide_inactive_chip {
                if inactive_chip.is_none() {
                    if emitted {
                        block.push_inert(Line::from(""));
                    }
                    inactive_chip = Some(block.lines.len());
                    let mut chip = inactive_line(
                        theme,
                        width,
                        inactive_count,
                        unlanded_count,
                        view.toggles.inactive_peek.is_some(),
                    );
                    chip.tag("chrome");
                    block.append(chip);
                    emitted = true;
                }
                inactive_seen += 1;
                if inactive_seen > page_limit && group.is_empty() {
                    if view.toggles.inactive_peek.is_some() {
                        older += 1;
                    }
                    continue;
                }
            }
            if emitted {
                block.push_inert(Line::from(""));
            }
            emitted = true;
            walk.origin = block.lines.len();
            block.append(worktree_group_lines_windowed(
                WorktreeRenderContext {
                    row: &ctx,
                    roster,
                    group,
                    meter_pixels: meter_pixels.as_deref_mut(),
                },
                walk,
            ));
            group_spans.push((index, walk.origin..block.lines.len()));
            if inactive_span.is_none()
                && group.source().inactive.is_some()
                && group.is_empty()
                && let Some(chip) = inactive_chip
            {
                inactive_span = Some((chip, block.lines.len() - 1));
            }
        }
        if older > 0 {
            block.append(inactive_older_line(theme, older));
        }
        inactive_span = inactive_span.or(inactive_chip.map(|chip| (chip, chip)));
    }

    ScrollContent {
        block,
        inactive_span,
        group_spans,
    }
}

fn inactive_older_line(theme: &Theme, count: usize) -> RenderedBlock {
    let mut block = RenderedBlock::default();
    block.push_target(
        pad_chrome(Line::styled(format!("  +{count} older"), theme.muted())),
        HitTarget::MoreInactive,
    );
    block.tag("chrome");
    block
}

fn inactive_line(
    theme: &Theme,
    width: usize,
    count: usize,
    unlanded: usize,
    open: bool,
) -> RenderedBlock {
    let glyph = theme.glyph(if open {
        GlyphRole::WorktreeInactiveOpen
    } else {
        GlyphRole::WorktreeInactiveClosed
    });
    let width = content_width(width);
    let prefix = format!("[ {glyph} ");
    let frame_width = text_width(&prefix) + 2;
    let separator = labels::value_seam(theme);
    let separator_width = text_width(&separator);
    let mut name = format!("{count} inactive");
    let mut breakdown = (unlanded > 0).then(|| format!("{unlanded} unlanded"));
    if breakdown.as_ref().is_some_and(|text| {
        frame_width + text_width(&name) + separator_width + text_width(text) > width
    }) {
        breakdown = None;
    }
    if frame_width + text_width(&name) > width {
        name = count.to_string();
    }
    let framed = frame_width + text_width(&name) <= width;
    let label = if framed {
        let text = breakdown
            .as_ref()
            .map_or_else(|| name.clone(), |text| format!("{name}{separator}{text}"));
        format!("{prefix}{text} ]")
    } else {
        name.clone()
    };
    let spare = width.saturating_sub(text_width(&label));
    let left = spare / 2;
    let right = spare - left;
    let dotted = theme.glyph(GlyphRole::WorktreeDotted);
    let mut spans = Vec::new();
    if left > 0 {
        spans.push(Span::styled(
            format!("{} ", dotted.repeat(left - 1)),
            theme.faint(),
        ));
    }
    let tone = theme.component(Component::InactiveChip);
    let style = theme.picked_chip(tone, Modifier::empty());
    spans.push(Span::styled(label, style));
    if right > 0 {
        spans.push(Span::styled(
            format!(" {}", dotted.repeat(right - 1)),
            theme.faint(),
        ));
    }
    let mut block = RenderedBlock::default();
    block.push_target(
        pad_chrome(Line::from(trim_spans_to_width(spans, width))),
        HitTarget::ToggleInactive,
    );
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
