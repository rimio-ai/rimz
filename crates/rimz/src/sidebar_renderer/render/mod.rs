//! Ratatui rendering for the sidebar snapshot model.
//!
//! `draw` is the entry point a Ratatui frame calls; `render_fixed` is the
//! offscreen variant used by the vt100-backed snapshot tests. Section
//! composition lives in [`sections`]; vocabulary labels in [`labels`];
//! pure formatting helpers in [`fmt`].
//!
//! Every entry point takes an optional [`Alert`] alongside the snapshot. The
//! alert is the sticky health line pinned to the bottom of the sidebar: while
//! the refresh loop is unhealthy it shows the reason and elapsed time, and
//! after recovery it lingers as a dismissable "last alert" notice. This is the
//! reload-recovery contract documented in
//! [`docs/internals/sidebar.md`](../../docs/internals/sidebar.md).

mod effects;
mod fmt;
mod labels;
mod odometer;
mod scrollbar;
mod sections;
mod theme;

pub(crate) use effects::EffectState;
pub(crate) use odometer::{CLICK_PHASES, CostRolls, TallyAnim};
pub(crate) use scrollbar::ScrollbarFade;

use std::io::{self, Write};

use crate::config::ScrollbarMode;
use crate::feed::AgentStatus;
use crate::ids::PaneId;
use crate::{SidebarRow, SidebarSnapshot};
use jiff::Timestamp;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};

use self::fmt::age_short;
pub(crate) use self::sections::{MakeUpHit, ProviderTabHit, status_total};
use self::sections::{
    cockpit_spend_line, cockpit_summary_line, content_width, first_run_hint_lines,
    fleet_header_lines, fleet_ledger_lines, fleet_size, provider_panel_lines, worktree_group_lines,
};
use self::theme::Theme;

#[derive(Clone, Debug, Default)]
pub struct UiState {
    pub selected_index: usize,
    pub help_visible: bool,
    /// Wall-clock animation frame counter, advanced by the serve loop's
    /// animation tick. The renderer derives the running-agent spin frame from
    /// it; freshness gating (per row) keeps a quiet agent frozen.
    pub animation_phase: u64,
    /// The cockpit spend's count-up state — one stepped roll for today's `$`.
    /// Folded forward on each data refresh (`TallyAnim::observe`) and read by the
    /// renderer at `animation_phase`; the serve loop keeps the fast tick alive
    /// while a roll is in flight. Crate-internal: an implementation detail of the
    /// renderer, not part of the public `UiState` surface.
    pub(crate) tally: TallyAnim,
    /// The agent cards' `$cost` count-up state — one stepped roll per row,
    /// keyed by the row's durable id so a reorder or refresh re-anchors a
    /// climb to its agent. Folded next to `tally` on each data refresh
    /// (`CostRolls::observe`, which also prunes departed rows) and read by the
    /// card at `animation_phase`; ORed into the serve loop's animation gate
    /// beside the tally. Crate-internal, like `tally`.
    pub(crate) cost_rolls: CostRolls,
    /// The post-render effects pass's memory — the transition detector's diff
    /// base and the live one-shot flashes ([`effects::EffectState`]). Observed
    /// and painted as a byproduct of every draw, after the paragraph render;
    /// the serve loop keeps the fast tick alive while a flash decays
    /// (`EffectState::any_active`), the tally's twin — and like the tally,
    /// crate-internal, not part of the public `UiState` surface.
    pub(crate) effects: EffectState,
    /// Hit-test map of the most recently drawn frame: one entry per inner-area
    /// content line, `Some(row)` for a jump-target row line (in
    /// `app::visible_rows()` order) and `None` for chrome. The renderer writes
    /// it as a byproduct of every draw; the mouse hit-test reads it. Empty
    /// before the first draw.
    pub line_map: Vec<Option<usize>>,
    /// The pane the highlight is pinned to — selection keyed by identity, not
    /// position. Re-derived each fold by `app::reconcile_selection` from the
    /// derived `baseline_pane` and any live `browse`. Keying on the pane means
    /// a status-churn reorder re-anchors the highlight to the same pane
    /// instead of sliding it onto a neighbour.
    pub selected_pane: Option<PaneId>,
    /// The hold-last derived baseline: the own view's active working pane from
    /// the last frame that reported one. Selection is *derived* — recomputed
    /// from the queried mux state every fold, so it is same-tab by construction
    /// and can never desynchronize, only lag a frame. It advances on a `Some`
    /// derivation and holds across a `None` (the sidebar itself is the view's
    /// active pane, or the active pane is not a row).
    pub(crate) baseline_pane: Option<PaneId>,
    /// The transient arrow-key browse pick riding above the baseline, or `None`
    /// when not browsing (see [`Browse`]).
    pub(crate) browse: Option<Browse>,
    /// First scroll-zone content line visible in the agent-cards viewport.
    /// Resolved by every draw — clamped to the zone, then auto-scrolled so the
    /// selected card stays in view unless a [`ManualScroll`] pin or the open
    /// help overlay holds it — and written back as a byproduct of the draw,
    /// like `line_map`.
    pub(crate) scroll_offset: usize,
    /// The transient wheel-scroll pin riding above the auto-follow, or `None`
    /// while the viewport follows the selection (see [`ManualScroll`]).
    pub(crate) manual_scroll: Option<ManualScroll>,
    /// The agent-cards scrollbar's auto-hide fade: every draw folds the
    /// resolved viewport offset into it as a write-back byproduct, and the
    /// `auto` scrollbar mode reads it to paint the bar only while the viewport
    /// moves plus a short settle window. Crate-internal, like `tally`.
    pub(crate) scrollbar: ScrollbarFade,
    /// The dashboard tab the user picked by hand (`←`/`→` or a click on a tab
    /// label), riding above the selection-derived default. Ends like a browse:
    /// it clears when the selection-derived provider kind *genuinely* changes
    /// from the value captured at pick time (a `None` derivation — a process
    /// row — holds it), or when its panel leaves the dashboard.
    pub(crate) dashboard_tab: Option<DashboardTab>,
    /// Hit-test map of the dashboard tab rail in the most recently drawn
    /// frame: the absolute screen line and column range of each tab's
    /// cap-to-cap footprint, written as a byproduct of every draw like
    /// `line_map`. Empty when no rail is on screen.
    pub(crate) tab_hits: Vec<ProviderTabHit>,
    /// The cockpit make-up bucket the user clicked to filter the agent-card
    /// body to one status, or `None` for the resting show-all view.
    /// Renderer-local display state — the producer, the ledger, and the
    /// cockpit counts (always the full fleet) are untouched; only the body
    /// iteration narrows, through the one shared [`row_passes_filter`]
    /// predicate. A pure toggle: a click on the active bucket clears it, and
    /// it auto-clears when its bucket's count drops to zero — the make-up
    /// twin of a dashboard tab pick ending when its panel leaves.
    pub(crate) make_up_filter: Option<AgentStatus>,
    /// Hit-test map of the cockpit make-up line in the most recently drawn
    /// frame: the absolute screen line and column range of each non-zero
    /// bucket's footprint, written as a byproduct of every draw like
    /// `line_map` and `tab_hits`. Empty when no make-up line is on screen.
    pub(crate) make_up_hits: Vec<MakeUpHit>,
}

/// The manual dashboard-tab pick: the provider kind to show, plus the
/// selection-derived kind captured when the pick was made — the clear
/// condition, mirroring [`Browse`]: the pick holds until the derived kind
/// genuinely changes from `derived_at_start`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DashboardTab {
    pub(crate) kind: String,
    pub(crate) derived_at_start: Option<String>,
}

/// Arrow-key browse: pins `pane` WITHOUT moving focus, roaming every visible
/// row — other tabs' rows included, so any card is one keystroke from
/// expanding. Holds until the derived baseline genuinely changes from
/// `baseline_at_start` — the value captured when browsing began. A `None`
/// derivation holds the baseline, so an inert frame never ends a browse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Browse {
    pub(crate) pane: PaneId,
    pub(crate) baseline_at_start: Option<PaneId>,
}

/// Wheel scroll: pins the viewport offset WITHOUT moving the selection, so the
/// user can peek at cards beyond the fold. Holds until the selection genuinely
/// changes from `selection_at_start` — the value captured when the scroll began
/// — then the viewport snaps back to following the selected card. The browse
/// twin, one layer down: browse pins *which card*, this pins *which window*.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManualScroll {
    pub(crate) selection_at_start: Option<PaneId>,
}

/// A sticky health alert pinned to the bottom of the sidebar.
///
/// `since` is when the unhealthy episode began, so an active alert can show
/// `for Ns`. `recovered_at` is `None` while the loop is still unhealthy and
/// `Some(t)` once it healed — a recovered alert lingers as a dismissable
/// "last alert" notice rather than vanishing the instant a fetch succeeds.
#[derive(Clone, Debug)]
pub struct Alert {
    pub reason: String,
    pub since: Timestamp,
    pub recovered_at: Option<Timestamp>,
}

impl Alert {
    pub fn active(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            since: Timestamp::now(),
            recovered_at: None,
        }
    }

    pub fn is_active(&self) -> bool {
        self.recovered_at.is_none()
    }
}

pub fn draw(frame: &mut Frame<'_>, snapshot: &SidebarSnapshot, alert: Option<&Alert>) {
    draw_with_ui(frame, snapshot, alert, &mut UiState::default());
}

/// The fastest animation class currently visible in the snapshot. Fast motion
/// changes every frame (working/thinking spinners, resolver work, active process
/// rows). Slow motion is cosmetic attention movement whose visible state is
/// held for several base frames, so the serve loop can redraw it less often
/// without making the sidebar feel stale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimationCadence {
    None,
    Slow,
    Fast,
}

/// Whether any visible row is in an animated state — a running agent (working
/// or pre-edit thinking), a resolver mid-flight, an active process spinning on
/// real work (a build, a test, a `sudo` install), or an attention row whose
/// `?`/`!` glyph breathes. The serve loop uses this as the broad "does anything
/// move?" gate; [`animation_cadence`] decides whether the movement needs the
/// fast frame grid or the slower cosmetic cadence. A fully settled sidebar
/// (only quiet idle/done rows) keeps idling on the slow data tick. A stalled
/// agent is projected to `failed` upstream, so it reads as a breathing `!`
/// here. The cockpit's today-spend count-up rides a separate gate
/// (`UiState::tally`), so a finished-turn climb keeps the tick alive even when
/// every row is otherwise static.
pub fn has_live_animation(snapshot: &SidebarSnapshot) -> bool {
    animation_cadence(snapshot) != AnimationCadence::None
}

// Deliberately unfiltered by the make-up filter: the cockpit's attention
// buckets still breathe (and the counts still tick) for rows a filter hides,
// so the gate must track the whole room, not the narrowed body.
pub fn animation_cadence(snapshot: &SidebarSnapshot) -> AnimationCadence {
    let mut slow = false;
    for row in snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
    {
        if row.is_agent() {
            if row.resolver().is_some() || row.status() == Some(AgentStatus::Running) {
                return AnimationCadence::Fast;
            }
            // `?`/`!` breathe to pull the eye back to an unanswered row,
            // quickening with age up to the red blink, which flips every
            // 300ms by design so it samples cleanly on this grid.
            if row.status().is_some_and(AgentStatus::is_actionable) {
                slow = true;
            }
        } else if row.process_is_busy() {
            return AnimationCadence::Fast;
        }
    }
    if slow {
        AnimationCadence::Slow
    } else {
        AnimationCadence::None
    }
}

pub fn draw_with_ui(
    frame: &mut Frame<'_>,
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    ui: &mut UiState,
) {
    let area = frame.area();
    // Borderless: the sidebar already sits inside a framed mux pane, so a second
    // 4-sided border double-frames it and eats two precious columns. The body
    // fills the whole area; a title line and faint hairline rules carry the
    // structure the border used to.
    //
    // The composed maps and the resolved scroll offset are byproducts of the
    // draw: store them so the mouse hit-test and the next frame's viewport read
    // the geometry of the frame the user is actually looking at.
    let composed = compose_lines(snapshot, alert, ui, area.width, area.height);
    ui.line_map = composed.line_map;
    ui.tab_hits = composed.tab_hits;
    ui.make_up_hits = composed.make_up_hits;
    ui.scrollbar
        .observe(composed.scroll_offset, ui.animation_phase);
    ui.scroll_offset = composed.scroll_offset;
    let paragraph = Paragraph::new(Text::from(composed.lines)).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
    // The truecolor garnish tier: a color-only effects pass over the buffer the
    // paragraph just rendered, geometry-locked to the line map this draw wrote.
    // Gated here rather than inside the pass so a non-truecolor terminal — or a
    // `[sidebar] glow = "never"` opt-out — pays nothing, not even the
    // transition observation.
    let theme = Theme::for_sidebar(&snapshot.sidebar);
    if theme.effects_enabled() {
        ui.effects.apply(
            snapshot,
            &theme,
            ui.make_up_filter,
            &ui.line_map,
            ui.selected_pane.as_ref(),
            ui.animation_phase,
            frame.buffer_mut(),
            area,
        );
    }
}

/// The one row-visibility predicate the make-up filter narrows the body by —
/// the single authority the body composer ([`worktree_group_lines`]) and the
/// selection model (`app::visible_rows` and friends) share, so the row
/// ordinals in `line_map` can never drift from the indices selection counts.
/// With no filter every row passes; with a bucket active only agent rows of
/// that status pass, so process rows (status `None`) drop out entirely.
pub(crate) fn row_passes_filter(row: &SidebarRow, filter: Option<AgentStatus>) -> bool {
    filter.is_none_or(|status| row.status() == Some(status))
}

/// The provider kind the dashboard's tab focus derives from the selection: the
/// selected row's agent kind (agent rows carry the kind in `SidebarRow::name`),
/// or `None` for a process row or an empty room — the caller falls back to the
/// first tab. Reads the same filtered universe `selected_index` is an ordinal
/// of, so the dashboard's follow-the-selection stays honest under a make-up
/// filter.
pub(crate) fn selected_agent_kind(snapshot: &SidebarSnapshot, ui: &UiState) -> Option<String> {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter(|row| row_passes_filter(row, ui.make_up_filter))
        .nth(ui.selected_index)
        .filter(|row| row.is_agent())
        .map(|row| row.name.clone())
}

/// The provider kind whose block the dashboard shows: the manual tab pick while
/// its panel is still on the dashboard, else the selection-derived kind
/// ([`selected_agent_kind`]) when a panel exists for it, else the first panel.
/// `None` only when the dashboard is empty.
pub(crate) fn active_provider_kind(snapshot: &SidebarSnapshot, ui: &UiState) -> Option<String> {
    let panels = &snapshot.providers;
    let has_panel = |kind: &str| panels.iter().any(|panel| panel.kind == kind);
    if let Some(tab) = &ui.dashboard_tab
        && has_panel(&tab.kind)
    {
        return Some(tab.kind.clone());
    }
    if let Some(kind) = selected_agent_kind(snapshot, ui)
        && has_panel(&kind)
    {
        return Some(kind);
    }
    panels.first().map(|panel| panel.kind.clone())
}

/// Lay out the frame as three vertical zones: the top-pinned cockpit (identity,
/// summary, make-up line), a scroll viewport over the agent cards, and the
/// bottom chrome pinned to the bottom edge like a status bar — the provider
/// dashboard, the centered navigation footer, and beneath it the sticky health
/// alert. Space for the pinned zones is always reserved — the scroll zone is
/// windowed before either is ever clipped — so the cockpit and the footer can
/// never scroll off a full sidebar. While an alert is *active* the body is a
/// stale/empty fetch, so the footer steps aside and the alert speaks alone.
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

    // Bottom-pinned chrome, top to bottom: the per-provider dashboard (account-
    // scoped budgets + brand emblem, which opens with its own top hairline — the
    // tab rail when several accounts register), the navigation footer (centered),
    // then the sticky health alert. While an alert is active the body is a
    // stale/empty fetch, so the panel and footer step aside and the alert speaks
    // alone. Every chrome line is gutter-padded so it breathes in the same
    // one-cell frame as the body.
    let active = alert.is_some_and(Alert::is_active);
    let mut bottom: Vec<Line<'static>> = Vec::new();
    // The tab hits arrive from the panel relative to its own lines; they are
    // translated to absolute screen coordinates once the bottom block's final
    // position is known, below.
    let mut tab_hits: Vec<ProviderTabHit> = Vec::new();
    let dashboard_present = !active && !snapshot.providers.is_empty();
    if dashboard_present {
        // The panel owns its top hairline (the tab rail when several accounts
        // register), so its line 0 lands directly at the block base.
        let panel_base = bottom.len();
        let active_kind = active_provider_kind(snapshot, ui);
        let (panel_lines, panel_hits) = provider_panel_lines(
            &theme,
            &snapshot.providers,
            active_kind.as_deref(),
            inner,
            &snapshot.sidebar.budget,
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
    // of the dashboard. It rides under the dashboard's blank-line block separator
    // when an account block is present, else carries its own hairline so it never
    // floats unsealed against the body.
    if !active {
        let corner = fleet_ledger_lines(&theme, snapshot.value_tally.as_ref(), inner);
        if !corner.is_empty() {
            if dashboard_present {
                bottom.push(Line::from(""));
            } else {
                bottom.push(pad_chrome(hairline_rule(&theme, inner)));
            }
            bottom.extend(corner.into_iter().map(pad_chrome));
        }
    }
    if !active {
        let footer = footer_lines(snapshot, &theme, inner);
        if !footer.is_empty() {
            // No rule above the footer — it sits quietly under the dashboard's own
            // top rule, with one blank line of breathing room when a dashboard is
            // present (skipped in an empty room so the footer doesn't float).
            if !bottom.is_empty() {
                bottom.push(Line::from(""));
            }
            bottom.extend(footer.into_iter().map(pad_chrome));
        }
    }
    if let Some(alert) = alert {
        bottom.extend(alert_lines(&theme, alert).into_iter().map(pad_chrome));
    }

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
fn auto_scroll_to_selection(
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
fn with_scrollbar(
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
fn scroll_thumb(offset: usize, scroll_len: usize, viewport: usize) -> (usize, usize) {
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

pub fn draw_to_terminal<B: Backend>(
    terminal: &mut Terminal<B>,
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
) -> Result<(), B::Error> {
    draw_to_terminal_with_ui(terminal, snapshot, alert, &mut UiState::default())
}

pub fn draw_to_terminal_with_ui<B: Backend>(
    terminal: &mut Terminal<B>,
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    ui: &mut UiState,
) -> Result<(), B::Error> {
    terminal
        .draw(|frame| draw_with_ui(frame, snapshot, alert, ui))
        .map(|_| ())
}

pub fn render_fixed<W: Write>(
    writer: W,
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    width: u16,
    height: u16,
) -> io::Result<()> {
    let backend = CrosstermBackend::new(writer);
    let viewport = Viewport::Fixed(Rect::new(0, 0, width, height));
    let mut terminal = Terminal::with_options(backend, TerminalOptions { viewport })?;
    terminal.clear()?;
    draw_to_terminal(&mut terminal, snapshot, alert)?;
    Ok(())
}

/// Compose the top-pinned cockpit zone and, in lockstep, its hit-test maps.
/// Every row-map entry is `None` — identity, summary, and the make-up line are
/// never jump targets — but the make-up line's status buckets are *filter*
/// targets, returned as [`MakeUpHit`]s already translated to this zone's line
/// indices and the chrome-gutter column space. Fixed height for a given room
/// population, never windowed, so the scroll zone below starts at a stable row.
fn top_lines(
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
    let (fleet_lines, mut make_up_hits) =
        fleet_header_lines(theme, &snapshot.worktree_groups, ui.make_up_filter, inner);
    for hit in &mut make_up_hits {
        hit.line += make_up_base;
        hit.col_start += 1;
        hit.col_end += 1;
    }
    extend_inert(&mut lines, &mut map, fleet_lines);
    (lines, map, make_up_hits)
}

/// Compose the scrollable agent-cards zone and, in lockstep, its hit-test map:
/// every content line gets one map entry, `Some(row)` for an agent/process row
/// line and the worktree header that jumps into it, `None` for structural
/// chrome (gaps, the external divider, first-run hint, help, `+K more`). The
/// zone opens with its own section gap — the top zone always ends on a
/// non-empty line — so an unscrolled frame composes exactly as the unsplit
/// body did. [`compose_lines`] windows this zone by the scroll offset and pins
/// the cockpit above it and the footer chrome below.
fn scroll_lines(
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    ui: &UiState,
    width: usize,
    theme: &Theme,
) -> (Vec<Line<'static>>, Vec<Option<usize>>) {
    // An *active* alert means the body is a stale/empty fetch, not a live room:
    // suppress the first-run hint, footer, and help so the alert speaks alone.
    // A recovered alert is just a lingering notice — the room below it is live.
    let active = alert.is_some_and(Alert::is_active);
    let mut lines = Vec::new();
    let mut map: Vec<Option<usize>> = Vec::new();

    if snapshot.worktree_groups.is_empty() {
        if !active && should_show_first_run_hint(snapshot) {
            lines.push(Line::from(""));
            map.push(None);
            extend_inert(
                &mut lines,
                &mut map,
                first_run_hint_lines(theme, snapshot.agent_hooks_ready),
            );
        }
    } else {
        lines.push(Line::from(""));
        map.push(None);
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
                width,
                &snapshot.sidebar.context,
                ui.make_up_filter,
                &mut row_index,
                ui.selected_index,
                ui.animation_phase,
                &ui.cost_rolls,
                &mut lines,
                &mut map,
            );
        }
        if !active && should_show_first_run_hint(snapshot) {
            lines.push(Line::from(""));
            map.push(None);
            extend_inert(
                &mut lines,
                &mut map,
                first_run_hint_lines(theme, snapshot.agent_hooks_ready),
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
fn extend_inert(
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
fn pad_chrome(line: Line<'static>) -> Line<'static> {
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

/// The borderless repo header (dashboard L1): the workspace name behind a `⌘`
/// glyph in bold on the left, and — when the project root is known — its
/// home-abbreviated path dim on the right edge of the same line. Identity and
/// location at a glance, on one line so the spend line can sit below it. The
/// path left-truncates with a leading `…` (keeping the meaningful tail) when it
/// can't fit, so the name is never crowded out.
fn repo_header_lines(
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
fn truncate_left(text: &str, budget: usize) -> String {
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

/// A full-width `─` hairline rule in the soft gray — the structural seams read
/// at a glance rather than receding into the chrome. Seals the header from
/// the cockpit and brackets the provider dashboard — the structure the dropped
/// border once carried.
fn hairline_rule(theme: &Theme, width: usize) -> Line<'static> {
    Line::styled("─".repeat(width.max(1)), theme.soft())
}

fn alert_lines(theme: &Theme, alert: &Alert) -> Vec<Line<'static>> {
    if alert.is_active() {
        let elapsed = age_short(alert.since);
        vec![Line::styled(
            format!("! Sidebar degraded for {elapsed}: {}", alert.reason),
            theme.style(Color::Red, Modifier::BOLD),
        )]
    } else {
        let elapsed = alert
            .recovered_at
            .map(age_short)
            .unwrap_or_else(|| "0s".to_owned());
        vec![Line::styled(
            format!("⚠ last alert {elapsed} ago: {}  ·  x dismiss", alert.reason),
            theme.style(Color::Yellow, Modifier::DIM),
        )]
    }
}

fn should_show_first_run_hint(snapshot: &SidebarSnapshot) -> bool {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .all(|row| row.is_process() && !is_known_agent_process(row))
}

fn is_known_agent_process(row: &crate::SidebarRow) -> bool {
    // tmux can expose Claude/Codex as the shared Node host before hook
    // enrichment claims the pane, so `node` is agent-like for the empty-room cue.
    row.is_process()
        && (crate::agents::known_kinds().any(|kind| kind == row.name) || row.name == "node")
}

fn footer_lines(snapshot: &SidebarSnapshot, theme: &Theme, width: usize) -> Vec<Line<'static>> {
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
fn center_line(line: Line<'static>, width: usize) -> Line<'static> {
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
fn help_lines(theme: &Theme) -> Vec<Line<'static>> {
    let faint = theme.faint();
    vec![
        Line::styled("keys & legend", faint),
        Line::styled("move     j/k rows   J/K worktrees", faint),
        Line::styled("focus    l or ↵     1-9 direct", faint),
        Line::styled("triage   ␣ next ?!  ←/→ accounts", faint),
        Line::styled("filter   q waiting   !/e attention", faint),
        Line::styled("         o idle      p paused", faint),
        Line::styled("         w working   d done   a all", faint),
        Line::styled("system   r reload   x dismiss", faint),
        Line::styled("help     ? close", faint),
        Line::styled("? waiting   ! attention   ⏸ paused", faint),
        Line::styled("⢿ working  ✻ thinking   ○ idle   ✓ done", faint),
    ]
}

#[cfg(test)]
mod compose_budget_tests;
#[cfg(test)]
mod tests;
