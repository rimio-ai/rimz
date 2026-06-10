use crate::SidebarSnapshot;
use crate::config::ScrollbarMode;
use ratatui::text::{Line, Span};

use super::chrome::{
    alert_lines, footer_lines, gate_notice_lines, hairline_rule, help_lines, repo_header_lines,
    truth_notice_lines,
};
use super::sections::{
    MakeUpHit, ProviderTabHit, cockpit_spend_line, cockpit_summary_line, content_width,
    fleet_header_lines, fleet_ledger_lines, fleet_size, provider_panel_lines, worktree_group_lines,
};
use super::theme::Theme;
use super::{Alert, UiState, active_provider_kind, dashboard_tabbed, labels, row_passes_filter};

/// Lay out the frame as three vertical zones: the top-pinned cockpit (identity,
/// summary, make-up line, and fixed separator), a scroll viewport over the
/// agent cards, and the bottom chrome pinned to the bottom edge like a status
/// bar — the provider dashboard, ledger, centered navigation footer, and
/// beneath them the sticky health alert. Space for the pinned zones is always
/// reserved — including the fixed separators under the cockpit and above the
/// provider dashboard — so the scroll zone is windowed before either pinned
/// edge is ever clipped. While an alert is *active* the body is a stale/empty
/// fetch, so the footer steps aside and the alert speaks alone.
///
/// The viewport window is `UiState::scroll_offset`, resolved here each frame:
/// clamped to the zone, then minimally auto-scrolled so the selected card —
/// its expanded subagent lines included — sits fully in view, unless a manual
/// wheel pin ([`ManualScroll`]) or the open help overlay holds the window —
/// the overlay owns the viewport while it is up, immune to selection churn
/// beneath it. The effective
/// offset is returned for the caller to write back, a draw byproduct like the
/// hit-test map. When the cards overflow the viewport, each visible scroll line
/// carries a track/thumb glyph in the right-margin column the content leaves
/// free — part of the composed line, so every consumer of the frame sees it.
///
/// Returns the composed frame with its hit-test maps and the effective scroll
/// offset ([`ComposedFrame`]). Row-map entry `i` is the visible row index that
/// on-screen content line `i` belongs to (`app::visible_rows()` order), or
/// `None` for structural lines (cockpit header, gaps, the external divider,
/// `+K more`, help, footer, alert); a worktree header routes to the row it
/// jumps into. The dashboard tab and make-up bucket hits ride beside it in
/// absolute screen coordinates. The maps are the single authority on hit
/// geometry — built from the same final line vector that is rendered, so they
/// stay 1:1 with what the user sees through every clip and every scroll
/// position.
pub(crate) fn compose_lines(
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    ui: &UiState,
    width: u16,
    height: u16,
) -> ComposedFrame {
    // One `Theme` per frame, handed to the body and the bottom chrome alike:
    // the cached `NO_COLOR` reading plus the palette and glow mode the
    // producer resolved from `[sidebar]` onto the snapshot — so a re-themed
    // config lands with the next snapshot, identically on every renderer of
    // the workspace.
    let theme = Theme::for_sidebar(&snapshot.sidebar);
    let cells = usize::from(width.max(1));
    // The whole sidebar sits inside a one-cell frame: chrome is built to the inner
    // width and opened with a blank gutter, leaving the trailing column as the
    // matching right margin — the same frame the cards carry (see `with_gutter`).
    let inner = content_width(cells);
    let (mut lines, mut map, mut make_up_hits) = top_lines(snapshot, ui, cells, &theme);
    let (scroll, scroll_map) = scroll_lines(snapshot, alert, ui, cells, &theme);

    // The tab hits arrive from the bottom chrome relative to its own lines;
    // they are translated to absolute screen coordinates once the block's final
    // position is known, below.
    let (bottom, mut tab_hits) = build_bottom_chrome(snapshot, alert, &theme, inner, ui);

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
    let top_shown = lines.len().min(after_bottom);
    let viewport = after_bottom - top_shown;
    lines.truncate(top_shown);
    map.truncate(top_shown);
    // The make-up hits arrive from `top_lines` already absolute — the cockpit
    // starts at screen row 0, so unlike the bottom-block tab hits there is no
    // base to add — but a degenerate-height frame can truncate the cockpit, so
    // a hit on a clipped line is dropped rather than left aimed at the body.
    make_up_hits.retain(|hit| hit.line < top_shown);

    // Resolve the viewport offset: clamp to the zone, then — unless a manual
    // wheel pin or the open help overlay holds the window — minimally
    // auto-scroll the selected card fully into view. The clamp runs first so
    // the help toggle's `usize::MAX` jump-to-end lands on the zone's last
    // window; while the overlay is open it owns the viewport, so selection
    // churn beneath it never pulls the view away mid-read.
    let scroll_len = scroll.len();
    let max_offset = scroll_len.saturating_sub(viewport);
    let mut offset = ui.scroll_offset.min(max_offset);
    if ui.manual_scroll.is_none() && !ui.help_visible {
        offset = auto_scroll_to_selection(&scroll_map, ui.selected_index, offset, viewport)
            .min(max_offset);
    }

    // Window the scroll zone, riding the scrollbar glyph on each visible line's
    // right-margin column when the cards overflow the viewport — gated by the
    // `[sidebar] scrollbar` mode. `auto` paints the bar on the very frame the
    // viewport moves (the fade's baseline still holds the pre-move offset; the
    // caller stamps it at the write-back) and through the settle window after.
    // The column is reserved in every mode, so the gate reflows nothing.
    let show_bar = match snapshot.sidebar.scrollbar {
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
        scroll_offset: offset,
    }
}

/// Bottom-pinned chrome, top to bottom: a fixed separator when the provider
/// dashboard is present, the per-provider dashboard (account-scoped budgets +
/// brand emblem, which opens with its own top hairline — the tab rail when
/// several accounts register), the fleet ledger, the navigation footer
/// (centered), then the sticky health alert. While an alert is active the body
/// is a stale/empty fetch, so the panel and footer step aside and the alert
/// speaks alone. Every chrome line is gutter-padded so it breathes in the same
/// one-cell frame as the body.
pub(super) fn build_bottom_chrome(
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    theme: &Theme,
    inner: usize,
    ui: &UiState,
) -> (Vec<Line<'static>>, Vec<ProviderTabHit>) {
    let active = alert.is_some_and(Alert::is_active);
    let mut bottom: Vec<Line<'static>> = Vec::new();
    let mut tab_hits: Vec<ProviderTabHit> = Vec::new();
    let dashboard_present = !active && !snapshot.providers.is_empty();
    if dashboard_present {
        // The pinned separator lifts the dashboard off the cards. It is part
        // of bottom chrome, so the viewport reserves it before windowing.
        bottom.push(Line::from(""));
        // The panel owns its top hairline (the tab rail when several accounts
        // register), so its line 0 lands after the separator.
        let panel_base = bottom.len();
        let active_kind = active_provider_kind(snapshot, ui);
        let tabbed = dashboard_tabbed(snapshot);
        let (panel_lines, panel_hits) = provider_panel_lines(
            theme,
            &snapshot.providers,
            active_kind.as_deref(),
            tabbed,
            inner,
            &snapshot.sidebar.budget,
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
        bottom.extend(panel_lines.into_iter().map(pad_chrome));
    }
    // The fleet ledger — the static `W:`/`M:` week/month rows — seals the bottom
    // of the dashboard. It rides under the dashboard's blank-line block
    // separator when an account block is present, else carries its own hairline
    // so it never floats unsealed against the body.
    if !active {
        let corner = fleet_ledger_lines(theme, snapshot.value_tally.as_ref(), inner);
        if !corner.is_empty() {
            if dashboard_present {
                bottom.push(Line::from(""));
            } else {
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
        if !footer.is_empty() {
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
    (bottom, tab_hits)
}

/// One draw's composed output: the final line vector plus the byproducts the
/// caller writes back onto [`UiState`] — the row hit-test map, the dashboard
/// tab and make-up bucket hit maps (absolute screen coordinates), and the
/// resolved viewport offset.
pub(crate) struct ComposedFrame {
    pub(crate) lines: Vec<Line<'static>>,
    pub(crate) line_map: Vec<Option<usize>>,
    pub(crate) tab_hits: Vec<ProviderTabHit>,
    pub(crate) make_up_hits: Vec<MakeUpHit>,
    pub(crate) scroll_offset: usize,
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

/// Ride the scrollbar on a visible scroll-zone line: pad to the right-margin
/// column the content leaves free (content builds to `content_width`, the
/// gutter takes column 0) and append the track or thumb glyph — so the bar is
/// part of the composed frame, reflows nothing, and adds no line the hit-test
/// map would have to account for. The solid `▐` thumb against the hairline `▕`
/// track carries the position by shape, so it survives `NO_COLOR`. The pad
/// measures display cells (`Line::width`) against the char-count budget the
/// content was built to; the two agree because every glyph in the sidebar
/// lexicon is single-cell.
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
    let pad = cells.saturating_sub(1).saturating_sub(line.width());
    if pad > 0 {
        line.spans.push(Span::raw(" ".repeat(pad)));
    }
    let in_thumb = (thumb_start..thumb_start + thumb_len).contains(&row);
    line.spans.push(if in_thumb {
        Span::styled(labels::SCROLL_THUMB, theme.dim())
    } else {
        Span::styled(labels::SCROLL_TRACK, theme.rule())
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
/// Every row-map entry is `None` — identity, summary, and the make-up line are
/// never jump targets — but the make-up line's status buckets are *filter*
/// targets, returned as [`MakeUpHit`]s already translated to this zone's line
/// indices and the chrome-gutter column space. Fixed height for a given room
/// population, never windowed, so the scroll zone below starts at a stable row.
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
    // The cockpit summary, two lines: line 1 is `◎` sessions today on the left
    // with today's accumulated token breakdown pinned right — both halves read
    // today's window; line 2 is `¤` live agents on the left with today's spend
    // pinned right. The counts read from the live fleet and the JSONL
    // `value_tally`'s today window, so the cockpit reflects all of today's
    // sessions rather than only the live statusline sum.
    let today = snapshot.value_tally.as_ref().map(|tally| &tally.today);
    let sessions = today.map(|window| window.sessions).unwrap_or(0);
    header.push(cockpit_summary_line(theme, sessions, today, inner));
    // Line 2 is always present — an empty room reads `¤ 0` — with the spend
    // joining the right edge and counting up as a turn lands. The roll targets
    // the live overlay when the snapshot carries one — the walked figure plus
    // each session's post-publish overshoot, so the headline moves with every
    // statusline push — and falls back to the tally on a pre-overlay snapshot.
    let live_agents = fleet_size(&snapshot.worktree_groups).0;
    let today_usd = snapshot
        .today_spend_live_usd
        .or(today.map(|window| window.usd))
        .unwrap_or(0.0);
    header.push(cockpit_spend_line(
        theme,
        live_agents,
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
    let (fleet_lines, mut make_up_hits) = fleet_header_lines(
        theme,
        &snapshot.worktree_groups,
        snapshot.now,
        ui.make_up_filter,
        ui.animation_phase,
        inner,
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
/// chrome (gaps, the external divider, help, `+K more`).
/// Populated rooms take their opening gap from the pinned cockpit separator;
/// empty rooms keep the scroll zone clear. [`compose_lines`] windows this zone
/// by the scroll offset and pins the cockpit above it and the bottom chrome
/// below.
pub(super) fn scroll_lines(
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    ui: &UiState,
    width: usize,
    theme: &Theme,
) -> (Vec<Line<'static>>, Vec<Option<usize>>) {
    // An *active* alert means the body is a stale/empty fetch, not a live room:
    // suppress the footer and help so the alert speaks alone.
    // A recovered alert is just a lingering notice — the room below it is live.
    let active = alert.is_some_and(Alert::is_active);
    let mut lines = Vec::new();
    let mut map: Vec<Option<usize>> = Vec::new();

    if !snapshot.worktree_groups.is_empty() {
        let mut row_index = 0;
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
                &snapshot.sidebar.context,
                snapshot.sidebar.card_density,
                ui.make_up_filter,
                &mut row_index,
                ui.selected_index,
                ui.animation_phase,
                &ui.cost_rolls,
                &mut lines,
                &mut map,
            );
        }
        if ui.help_visible && !active {
            lines.push(Line::from(""));
            map.push(None);
            extend_inert(&mut lines, &mut map, help_lines(theme));
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
/// so the whole sidebar sits inside a one-cell frame — the trailing column the
/// content leaves free is the matching right margin. A genuinely empty line (a
/// blank separator, or the cockpit's reserved-but-empty totals slot) is left as
/// is, so it stays zero-width and reads as a true blank row. A line-level style
/// (a `Line::styled` hairline or help line) is patched into the rebuilt spans —
/// the same carry the cards' `with_gutter` does — so the rebuild never silently
/// strips a tone.
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
