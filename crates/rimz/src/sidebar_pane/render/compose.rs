use crate::agents::AgentStatus;
use crate::config::ScrollbarMode;
use crate::{SidebarSnapshot, SidebarWorktreeGroup, SidebarWorktreeKind, lead_unread_row};
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::chrome::{
    alert_lines, footer_lines, gate_notice_lines, hairline_rule, repo_header_lines,
    truth_notice_lines,
};
use super::sections::{
    MakeUpHit, ProviderTabHit, cockpit_spend_line, cockpit_summary_line, content_width,
    dashboard_panel_lines_with_footer, fleet_header_lines, fleet_ledger_lines, fleet_size,
    fleet_total_lines, trim_spans_to_width, unread_total, worktree_group_lines,
};
use super::theme::Theme;
use super::{
    Alert, BodyFilter, UiState, active_dashboard_tab, dashboard_present, dashboard_tabbed, labels,
    row_passes_filter,
};

/// Lay out the frame as three vertical zones: the top-pinned cockpit (identity,
/// summary, make-up line, the conditional unread banner, and fixed separator),
/// a scroll viewport over the agent cards, and the bottom chrome pinned to the
/// bottom edge like a status bar — the provider dashboard, ledger, centered
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
pub(crate) fn compose_lines(
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    ui: &UiState,
    width: u16,
    height: u16,
) -> ComposedFrame {
    // One `Theme` per frame, handed to the body and the bottom chrome alike:
    // the cached `NO_COLOR` reading plus the palette and glow mode the
    // producer resolved from `[theme]` onto the snapshot — so a re-themed
    // config lands with the next snapshot, identically on every renderer of
    // the workspace.
    let theme = Theme::for_sidebar(&snapshot.theme);
    let cells = usize::from(width.max(1));
    // The whole sidebar sits inside a one-cell frame: chrome is built to the inner
    // width and opened with a blank gutter, reserving the trailing right rail —
    // the same frame the cards carry (see `with_gutter`).
    let inner = content_width(cells);
    let (mut lines, mut map, mut make_up_hits) = top_lines(snapshot, ui, cells, &theme);
    let (scroll, scroll_map) = scroll_lines(snapshot, ui, cells, &theme);

    // The tab hits arrive from the bottom chrome relative to its own lines;
    // they are translated to absolute screen coordinates once the block's final
    // position is known, below.
    let (bottom, mut tab_hits, mut pet_pixel_rect) =
        build_bottom_chrome(snapshot, alert, &theme, inner, ui);

    let height = usize::from(height);
    let bottom_height = bottom
        .iter()
        // Every line occupies at least one row — a blank separator has width 0
        // but still takes a row, so `.max(1)` keeps the reservation honest and
        // the footer from being pushed off the frame.
        .map(|line| line.width().div_ceil(cells).max(1))
        .sum::<usize>()
        .min(height);

    // Zone heights, reserved pinned-first: the bottom chrome, then the cockpit,
    // and the scroll viewport takes what remains — zero on a degenerate frame,
    // so the cards give way before either pinned zone is ever clipped.
    let after_bottom = height.saturating_sub(bottom_height);
    let scroll_len = scroll.len();

    // Pass 1 — size the top zone and resolve the offset as if the banner is
    // hidden. Its row is added only when shown, so the viewport is a byproduct of
    // the decision rather than a fixed reservation.
    let top_base_len = lines.len();
    let top_shown_hidden = top_base_len.min(after_bottom);
    let viewport_hidden = after_bottom - top_shown_hidden;
    let offset_hidden =
        resolve_scroll_offset(snapshot, ui, &scroll_map, scroll_len, viewport_hidden);

    // The `↑ N need you` jump banner appears only while the lead-unread card is
    // scrolled out of the window — on screen (e.g. at the top) it's redundant.
    let lead = lead_unread_row(&snapshot.worktree_groups);
    let show_banner = if let Some(lead) = lead {
        viewport_hidden > 0
            && !lead_unread_visible(
                snapshot,
                ui,
                &lead.id,
                &scroll_map,
                offset_hidden,
                viewport_hidden,
            )
    } else {
        false
    };

    let mut banner_line = None;
    if show_banner && let Some(lead) = lead {
        let count = snapshot
            .worktree_groups
            .iter()
            .flat_map(|group| &group.rows)
            .filter(|row| row.unread && row.status().is_some_and(AgentStatus::is_actionable))
            .count();
        let status = lead.status().unwrap_or(AgentStatus::Waiting);
        // Above the pinned separator blank (last top-zone line) so cards keep
        // their breathing row. Structural in the row map — the click scrolls to
        // the top via `banner_line`, not a row jump.
        let at = top_base_len - 1;
        lines.insert(at, pad_chrome(unread_banner_line(&theme, status, count)));
        map.insert(at, None);
        banner_line = Some(at);
    }

    // Pass 2 — final top height (with the banner if any), then re-resolve for the
    // possibly-smaller viewport. `show_banner` implies `viewport_hidden > 0`, i.e.
    // `after_bottom >= top_base_len + 1`, so the banner never truncates here.
    let top_shown = lines.len().min(after_bottom);
    let viewport = after_bottom - top_shown;
    lines.truncate(top_shown);
    map.truncate(top_shown);
    // The make-up hits arrive from `top_lines` already absolute — the cockpit
    // starts at screen row 0, so unlike the bottom-block tab hits there is no
    // base to add — but a degenerate-height frame can truncate the cockpit, so
    // a hit on a clipped line is dropped rather than left aimed at the body.
    make_up_hits.retain(|hit| hit.line < top_shown);
    let offset = if show_banner {
        resolve_scroll_offset(snapshot, ui, &scroll_map, scroll_len, viewport)
    } else {
        offset_hidden
    };

    // Window the scroll zone, riding the scrollbar glyph on each visible line's
    // right rail column when the cards overflow the viewport — gated by the
    // `[theme.display] scrollbar` mode. `auto` paints the bar on the very frame the
    // viewport moves (the fade's baseline still holds the pre-move offset; the
    // caller stamps it at the write-back) and through the settle window after.
    // The column is reserved in every mode, so the gate reflows nothing.
    let show_bar = match snapshot.theme.display.scrollbar {
        ScrollbarMode::Always => true,
        ScrollbarMode::Never => false,
        ScrollbarMode::Auto => {
            ui.scrollbar.moved_from(offset) || ui.scrollbar.visible(ui.animation_phase)
        }
    };
    let end = (offset + viewport).min(scroll_len);
    let overflow = scroll_len > viewport && viewport > 0;
    for (index, line) in scroll
        .into_iter()
        .enumerate()
        .skip(offset)
        .take(end - offset)
    {
        lines.push(if overflow && show_bar {
            with_scrollbar(
                line,
                &theme,
                cells,
                index - offset,
                offset,
                scroll_len,
                viewport,
            )
        } else {
            line
        });
    }
    map.extend(scroll_map[offset..end].iter().copied());

    let pad = height.saturating_sub(lines.len() + bottom_height);
    lines.extend(std::iter::repeat_n(Line::from(""), pad));
    map.extend(std::iter::repeat_n(None, pad));
    // The bottom block's final position is now fixed, so the tab hits land on
    // their absolute screen lines.
    for hit in &mut tab_hits {
        hit.line += lines.len();
    }
    if let Some(rect) = pet_pixel_rect.as_mut() {
        rect.y = rect.y.saturating_add(lines.len() as u16);
    }
    // The footer and alert are pinned chrome, never jump targets: one `None`
    // per line. The dashboard's tabs are the bottom block's only hit
    // targets, carried by `tab_hits` rather than the row map.
    map.extend(std::iter::repeat_n(None, bottom.len()));
    lines.extend(bottom);
    ComposedFrame {
        lines,
        line_map: map,
        tab_hits,
        make_up_hits,
        pet_pixel_rect,
        banner_line,
        scroll_offset: offset,
        bottom_height,
    }
}

/// Bottom-pinned chrome, top to bottom: a fixed separator when the provider
/// dashboard is present, the per-provider dashboard (account-scoped budgets +
/// brand emblem, which opens with its own top hairline — the tab rail when
/// several accounts register), the fallback fleet ledger for no-table layouts,
/// the navigation footer (centered), then the sticky health alert. While an
/// alert is active the body is a stale/empty fetch, so the panel and footer step
/// aside and the alert speaks alone. Every chrome line is gutter-padded so it
/// breathes in the same one-cell frame as the body.
pub(super) fn build_bottom_chrome(
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    theme: &Theme,
    inner: usize,
    ui: &UiState,
) -> (Vec<Line<'static>>, Vec<ProviderTabHit>, Option<Rect>) {
    let active = alert.is_some_and(Alert::is_active);
    let mut bottom: Vec<Line<'static>> = Vec::new();
    let mut tab_hits: Vec<ProviderTabHit> = Vec::new();
    let mut pet_pixel_rect: Option<Rect> = None;
    let dashboard_present = dashboard_present(snapshot, active);
    let tabbed = dashboard_present && dashboard_tabbed(snapshot);
    let dashboard_owns_ledger = dashboard_present
        && tabbed
        && !snapshot.providers.is_empty()
        && snapshot.theme.pets.enabled;
    let fold_footer_into_dashboard = dashboard_owns_ledger
        && !active
        && snapshot.truth_degraded.is_none()
        && ui.gate_notice.is_none()
        && alert.is_none();
    let folded_footer = fold_footer_into_dashboard
        .then(|| footer_lines(snapshot, theme, inner).into_iter().next())
        .flatten();
    if dashboard_present {
        // The pinned separator lifts the dashboard off the cards. It is part
        // of bottom chrome, so the viewport reserves it before windowing.
        bottom.push(Line::from(""));
        // The panel owns its top hairline (the tab rail when several accounts
        // register), so its line 0 lands after the separator.
        let panel_base = bottom.len();
        let active_tab = active_dashboard_tab(snapshot, ui);
        let (panel_lines, panel_hits, panel_pet_pixel_rect) = dashboard_panel_lines_with_footer(
            theme,
            &snapshot.providers,
            active_tab.as_ref(),
            tabbed,
            snapshot.value_tally.as_ref(),
            ui.pet.as_ref(),
            snapshot.theme.pets.enabled,
            folded_footer.clone(),
            inner,
            &snapshot.theme.display.budget_bar,
            snapshot.now,
        );
        tab_hits = panel_hits
            .into_iter()
            .map(|hit| ProviderTabHit {
                // Position within the bottom block; the absolute base lands on
                // top once the body's final height is known.
                line: panel_base + hit.line,
                // The chrome gutter `pad_chrome` opens every panel line with.
                col_start: hit.col_start + 1,
                col_end: hit.col_end + 1,
                kind: hit.kind,
            })
            .collect();
        pet_pixel_rect = panel_pet_pixel_rect.map(|rect| {
            Rect::new(
                (rect.col + 1) as u16,
                (panel_base + rect.line) as u16,
                rect.width,
                rect.height,
            )
        });
        bottom.extend(panel_lines.into_iter().map(pad_chrome));
    }
    // The static `W:`/`M:` rows seal the main dashboard. The pet-enabled tall
    // provider block owns those totals inside its `Total:` section.
    if !active && !dashboard_owns_ledger {
        let corner = if dashboard_present {
            fleet_total_lines(theme, snapshot.value_tally.as_ref(), inner)
        } else {
            fleet_ledger_lines(theme, snapshot.value_tally.as_ref(), inner)
        };
        if !corner.is_empty() {
            if !dashboard_present {
                bottom.push(pad_chrome(hairline_rule(theme, inner)));
            }
            bottom.extend(corner.into_iter().map(pad_chrome));
        }
    }
    if !active && let Some(notice) = snapshot.truth_degraded.as_ref() {
        bottom.extend(
            truth_notice_lines(theme, notice, snapshot.now)
                .into_iter()
                .map(pad_chrome),
        );
    }
    if !active && let Some(notice) = ui.gate_notice.as_ref() {
        bottom.extend(gate_notice_lines(theme, notice).into_iter().map(pad_chrome));
    }
    if !active {
        let footer = footer_lines(snapshot, theme, inner);
        if folded_footer.is_none() && !footer.is_empty() {
            // No rule above the footer — it sits quietly under the dashboard's
            // own top rule, with one blank line of breathing room when a
            // dashboard is present (skipped in an empty room so the footer
            // doesn't float).
            if !bottom.is_empty() {
                bottom.push(Line::from(""));
            }
            bottom.extend(footer.into_iter().map(pad_chrome));
        }
    }
    if let Some(alert) = alert {
        bottom.extend(
            alert_lines(theme, alert, snapshot.now)
                .into_iter()
                .map(pad_chrome),
        );
    }
    (bottom, tab_hits, pet_pixel_rect)
}

/// One draw's composed output: the final line vector plus the byproducts the
/// caller writes back onto [`UiState`] — the row hit-test map, the dashboard tab
/// and make-up bucket hit maps (absolute screen coordinates), the unread banner
/// line, and the resolved viewport offset.
pub(crate) struct ComposedFrame {
    pub(crate) lines: Vec<Line<'static>>,
    pub(crate) line_map: Vec<Option<usize>>,
    pub(crate) tab_hits: Vec<ProviderTabHit>,
    pub(crate) make_up_hits: Vec<MakeUpHit>,
    pub(crate) pet_pixel_rect: Option<Rect>,
    pub(crate) banner_line: Option<usize>,
    pub(crate) scroll_offset: usize,
    pub(crate) bottom_height: usize,
}

fn resolve_scroll_offset(
    snapshot: &SidebarSnapshot,
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
    let resolved = if let Some(target) = unread_focus_ordinal(snapshot, ui) {
        // A freshly-arrived unread outranks everything: it ranks to the top,
        // so targeting it scrolls to the top.
        auto_scroll_to_selection(scroll_map, target, offset, viewport)
    } else if ui.focus_group_reveal
        && let Some(group_first) =
            selected_group_first_ordinal(snapshot, ui.make_up_filter, ui.selected_index)
    {
        auto_scroll_reveal_group(scroll_map, group_first, ui.selected_index, offset, viewport)
    } else {
        auto_scroll_to_selection(scroll_map, ui.selected_index, offset, viewport)
    };
    resolved.min(max_offset)
}

/// True when the lead-unread row (oldest actionable unread — it ranks to the
/// very top) has a line inside the window `[offset, offset + viewport)`. The
/// banner is the "get back to it" affordance, so it shows only when this is
/// false. A zero-height viewport or a lead the make-up filter hides reads as
/// not visible.
fn lead_unread_visible(
    snapshot: &SidebarSnapshot,
    ui: &UiState,
    lead_id: &str,
    scroll_map: &[Option<usize>],
    offset: usize,
    viewport: usize,
) -> bool {
    if viewport == 0 {
        return false;
    }
    let Some(ordinal) = visible_row_ordinal(snapshot, ui, lead_id) else {
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

/// The visible-row ordinal of the first row of the group containing `selected`,
/// in the filtered body order the `line_map` indexes. `None` when `selected` is
/// out of range.
fn selected_group_first_ordinal(
    snapshot: &SidebarSnapshot,
    filter: Option<BodyFilter>,
    selected: usize,
) -> Option<usize> {
    let mut start = 0;
    for group in &snapshot.worktree_groups {
        let len = group
            .rows
            .iter()
            .filter(|row| row_passes_filter(row, filter))
            .count();
        if len == 0 {
            continue;
        }
        if selected < start + len {
            return (group.kind != SidebarWorktreeKind::External).then_some(start);
        }
        start += len;
    }
    None
}

/// The visible-row ordinal of `id`, in the filtered body order the `line_map`
/// indexes — the same order `app::visible_rows` builds, so the ordinal lines up
/// with `selected_index` and the map entries. `None` when the id is absent or the
/// make-up filter hides its row.
fn visible_row_ordinal(snapshot: &SidebarSnapshot, ui: &UiState, id: &str) -> Option<usize> {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter(|row| row_passes_filter(row, ui.make_up_filter))
        .position(|row| row.id == id)
}

/// The visible-row ordinal of the armed unread-focus row, the auto-scroll target
/// that outranks the selection. `None` when no snap is armed or the make-up filter
/// hides the row, leaving the viewport to follow the selection.
fn unread_focus_ordinal(snapshot: &SidebarSnapshot, ui: &UiState) -> Option<usize> {
    visible_row_ordinal(snapshot, ui, ui.unread_focus.as_deref()?)
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

/// Compose the top-pinned cockpit zone and, in lockstep, its hit-test maps.
/// Populated rooms end this fixed zone with a separator blank, so scrolled
/// cards never touch the cockpit make-up line.
/// Identity, summary, and the make-up line are never jump targets, so they map to
/// `None`; the make-up line's status buckets are *filter* targets, returned as
/// [`MakeUpHit`]s already translated to this zone's line indices and the
/// chrome-gutter column space. Fixed height for a given room population, never
/// windowed, so the scroll zone below starts at a stable row.
pub(super) fn top_lines(
    snapshot: &SidebarSnapshot,
    ui: &UiState,
    width: usize,
    theme: &Theme,
) -> (Vec<Line<'static>>, Vec<Option<usize>>, Vec<MakeUpHit>) {
    let mut lines = Vec::new();
    let mut map: Vec<Option<usize>> = Vec::new();

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
    // is `¤` live agents on the left with headline spend pinned right. The
    // counts read from the live fleet and the JSONL `workspace_value_tally`'s
    // headline window, so the cockpit reflects this room's sessions rather than
    // account-global provider history.
    let headline = snapshot
        .workspace_value_tally
        .as_ref()
        .map(|tally| &tally.headline);
    let sessions = headline.map(|window| window.sessions).unwrap_or(0);
    header.push(cockpit_summary_line(theme, sessions, headline, inner));
    // Line 2 is always present — an empty room reads `¤ 0` — with the spend
    // joining the right edge and counting up as a turn lands. The roll targets
    // the live overlay when the snapshot carries one — the walked figure plus
    // each session's post-publish overshoot, so the headline moves with every
    // statusline push — and falls back to the tally on a pre-overlay snapshot.
    let live_agents = fleet_size(&snapshot.worktree_groups).0;
    let unread_agents = unread_total(&snapshot.worktree_groups);
    let today_usd = snapshot
        .today_spend_live_usd
        .or(headline.map(|window| window.usd))
        .unwrap_or(0.0);
    header.push(cockpit_spend_line(
        theme,
        live_agents,
        unread_agents,
        today_usd,
        &ui.tally,
        ui.animation_phase,
        inner,
    ));
    header.push(hairline_rule(theme, inner));
    extend_inert(&mut lines, &mut map, header);

    // The fleet header (the cockpit make-up line) is always present and a fixed
    // height — one line for a populated room, none for an empty one — so the body
    // below never shifts vertically as agents change state. It is chrome, never a
    // jump target, so every header line maps to `None` in the row map; its
    // status buckets carry their own hit map instead, translated here onto the
    // zone's line index and into the `pad_chrome` gutter's column space.
    let make_up_base = lines.len();
    let lead_unread_status = lead_unread(&snapshot.worktree_groups).map(|(_, status)| status);
    let (fleet_lines, mut make_up_hits) = fleet_header_lines(
        theme,
        &snapshot.worktree_groups,
        snapshot.now,
        ui.make_up_filter,
        ui.animation_phase,
        inner,
        lead_unread_status,
    );
    for hit in &mut make_up_hits {
        hit.line += make_up_base;
        hit.col_start += 1;
        hit.col_end += 1;
    }
    extend_inert(&mut lines, &mut map, fleet_lines);
    if !snapshot.worktree_groups.is_empty() {
        lines.push(Line::from(""));
        map.push(None);
    }
    (lines, map, make_up_hits)
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
    width: usize,
    theme: &Theme,
) -> (Vec<Line<'static>>, Vec<Option<usize>>) {
    let mut lines = Vec::new();
    let mut map: Vec<Option<usize>> = Vec::new();

    if !snapshot.worktree_groups.is_empty() {
        let mut row_index = 0;
        let lead_unread_id = lead_unread(&snapshot.worktree_groups).map(|(id, _)| id);
        // A group the make-up filter empties is skipped whole — header,
        // rows, and separator — so the filtered body holds only worktrees
        // with a matching row; the external catch-all is just another group.
        let mut emitted = false;
        for group in &snapshot.worktree_groups {
            let has_visible = group
                .rows
                .iter()
                .any(|row| row_passes_filter(row, ui.make_up_filter));
            if !has_visible {
                continue;
            }
            if emitted {
                lines.push(Line::from(""));
                map.push(None);
            }
            emitted = true;
            worktree_group_lines(
                theme,
                group,
                &snapshot.providers,
                snapshot.now,
                width,
                &snapshot.theme.display.context_meter,
                snapshot.theme.display.card_density,
                ui.make_up_filter,
                &mut row_index,
                ui.selected_index,
                ui.animation_phase,
                &ui.cost_rolls,
                lead_unread_id,
                &mut lines,
                &mut map,
            );
        }
    }

    (lines, map)
}

/// Append structural (non-row) lines, tagging each map slot `None` and opening
/// each with a blank one-cell gutter so chrome breathes in the same one-cell
/// frame as the cards (which carry their own gutter via `with_gutter`).
pub(super) fn extend_inert(
    lines: &mut Vec<Line<'static>>,
    map: &mut Vec<Option<usize>>,
    inert: Vec<Line<'static>>,
) {
    map.extend(std::iter::repeat_n(None, inert.len()));
    lines.extend(inert.into_iter().map(pad_chrome));
}

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
