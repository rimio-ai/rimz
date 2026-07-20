//! The selection model: an identity-keyed highlight over a derived baseline,
//! the transient arrow-key browse layer above it, the key/mouse handlers that
//! act on it, and the typed effects they return to the serve loop.

use crate::ids::PaneId;
use crate::mux::WidthAdjust;
use crate::{SidebarSnapshot, triage_key};

use crate::sidebar_pane::render::HitTarget;
use crate::sidebar_pane::render::{
    BodyFilter, Browse, DashboardTab, ManualScroll, UiState, active_dashboard_tab,
    dashboard_tabbed, dashboard_tabs, selected_agent_kind,
};
use crate::sidebar_pane::view::VisibleRoster;

use super::input::KeyAction;

/// Content lines a wheel tick moves the viewport — about one card line-group
/// per notch, so a flick traverses cards rather than crawling line by line.
const SCROLL_STEP: usize = 3;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct InputOutcome {
    pub(super) redraw: bool,
    pub(super) effect: Option<InputEffect>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum InputEffect {
    /// Fire the one-way focus command without moving the highlight. Selection
    /// remains derived state until the baseline catches up.
    Focus(PaneId),
    /// Dispatch one resize step. Resize wakeups own repaint and persistence.
    Width(WidthAdjust),
    DismissAlert,
    /// Write the durable receipt in the loop, which owns runtime paths.
    MarkRead(String),
    MarkUnread(String),
    MarkAllRead,
    /// Persist and broadcast the shared cockpit lens from the loop, which owns
    /// the room runtime paths.
    SyncFilter(Option<BodyFilter>),
}

impl InputOutcome {
    pub(super) fn redraw() -> Self {
        Self {
            redraw: true,
            ..Self::default()
        }
    }

    pub(super) fn focus(pane: PaneId) -> Self {
        Self {
            effect: Some(InputEffect::Focus(pane)),
            ..Self::default()
        }
    }

    fn width(width: WidthAdjust) -> Self {
        Self {
            effect: Some(InputEffect::Width(width)),
            ..Self::default()
        }
    }

    pub(super) fn dismiss() -> Self {
        Self {
            redraw: true,
            effect: Some(InputEffect::DismissAlert),
        }
    }

    /// Mark the selected row read / unread. No redraw here: the loop clears the
    /// row and repaints once after the durable write, so the frame never
    /// flashes the pre-clear state first.
    fn mark_read(row_id: String) -> Self {
        Self {
            effect: Some(InputEffect::MarkRead(row_id)),
            ..Self::default()
        }
    }

    fn mark_unread(row_id: String) -> Self {
        Self {
            effect: Some(InputEffect::MarkUnread(row_id)),
            ..Self::default()
        }
    }

    fn mark_all_read() -> Self {
        Self {
            effect: Some(InputEffect::MarkAllRead),
            ..Self::default()
        }
    }

    fn sync_filter(filter: Option<BodyFilter>) -> Self {
        Self {
            redraw: true,
            effect: Some(InputEffect::SyncFilter(filter)),
        }
    }
}

/// Step the inbox triage list `forward` or backward from the current selection
/// and focus that row's pane — the `n`/`N` (and `Space`) walk through the rows
/// that need you in the visible roster, oldest episode first. A walk with
/// nothing to triage does nothing.
fn inbox_jump(ui: &mut UiState, snapshot: &SidebarSnapshot, forward: bool) -> InputOutcome {
    let roster = active_roster(snapshot, ui);
    if let Some(index) = step_attention_in(&roster, ui.selected_index, forward)
        && let Some(pane) = roster.pane_at_ordinal(index)
    {
        return InputOutcome::focus(pane);
    }
    InputOutcome::default()
}

#[derive(Clone, Copy)]
enum End {
    Top,
    Bottom,
}

/// Move the browse pick to the first or last visible row — the `g`/`G` Vim
/// jump. Selection only, no focus, over the same filtered row universe ordinary
/// selection walks. A no-op when already there or the body is empty.
fn select_end_row(ui: &mut UiState, snapshot: &SidebarSnapshot, end: End) -> InputOutcome {
    let roster = active_roster(snapshot, ui);
    let len = roster.len();
    if len == 0 {
        return InputOutcome::default();
    }
    let target = match end {
        End::Top => 0,
        End::Bottom => len - 1,
    };
    select_to_index_in(ui, &roster, target)
}

fn select_to_index(ui: &mut UiState, snapshot: &SidebarSnapshot, target: usize) -> InputOutcome {
    let roster = active_roster(snapshot, ui);
    select_to_index_in(ui, &roster, target)
}

fn select_to_index_in(ui: &mut UiState, roster: &VisibleRoster<'_>, target: usize) -> InputOutcome {
    let len = roster.len();
    if len == 0 {
        return InputOutcome::default();
    }
    let target = target.min(len - 1);
    if ui.selected_index == target {
        return InputOutcome::default();
    }
    select_row_in(ui, roster, target);
    begin_or_continue_browse(ui);
    InputOutcome::redraw()
}

fn visible_row_span(ui: &UiState) -> Option<(usize, usize)> {
    ui.interactions.visible_row_span()
}

fn select_screen_edge(ui: &mut UiState, snapshot: &SidebarSnapshot, end: End) -> InputOutcome {
    let Some((first, last)) = visible_row_span(ui) else {
        return InputOutcome::default();
    };
    let target = match end {
        End::Top => first,
        End::Bottom => last,
    };
    select_to_index(ui, snapshot, target)
}

fn select_page(ui: &mut UiState, snapshot: &SidebarSnapshot, down: bool) -> InputOutcome {
    let Some((first, last)) = visible_row_span(ui) else {
        return InputOutcome::default();
    };
    let page = last.saturating_sub(first).saturating_add(1).max(1);
    let target = if down {
        ui.selected_index.saturating_add(page)
    } else {
        ui.selected_index.saturating_sub(page)
    };
    select_to_index(ui, snapshot, target)
}

pub(super) fn handle_key(
    action: KeyAction,
    ui: &mut UiState,
    snapshot: &SidebarSnapshot,
) -> InputOutcome {
    if action == KeyAction::Other {
        return InputOutcome::default();
    }
    match action {
        KeyAction::WidthNarrower => InputOutcome::width(WidthAdjust::Narrower),
        KeyAction::WidthWider => InputOutcome::width(WidthAdjust::Wider),
        KeyAction::Up => {
            if ui.selected_index > 0 {
                let roster = active_roster(snapshot, ui);
                select_row_in(ui, &roster, ui.selected_index - 1);
                begin_or_continue_browse(ui);
                return InputOutcome::redraw();
            }
            InputOutcome::default()
        }
        KeyAction::Down => {
            let roster = active_roster(snapshot, ui);
            let len = roster.len();
            if ui.selected_index + 1 < len {
                select_row_in(ui, &roster, ui.selected_index + 1);
                begin_or_continue_browse(ui);
                return InputOutcome::redraw();
            }
            InputOutcome::default()
        }
        KeyAction::WorktreeUp => select_adjacent_worktree(ui, snapshot, -1),
        KeyAction::WorktreeDown => select_adjacent_worktree(ui, snapshot, 1),
        KeyAction::Top => select_end_row(ui, snapshot, End::Top),
        KeyAction::Bottom => select_end_row(ui, snapshot, End::Bottom),
        KeyAction::PageUp => select_page(ui, snapshot, false),
        KeyAction::PageDown => select_page(ui, snapshot, true),
        KeyAction::ScreenTop => select_screen_edge(ui, snapshot, End::Top),
        KeyAction::ScreenBottom => select_screen_edge(ui, snapshot, End::Bottom),
        KeyAction::Enter => {
            // Focus the current visible row without moving the highlight — it
            // follows once the derived baseline catches up, identical to a
            // click.
            match ui.selected_pane.clone() {
                Some(pane) => InputOutcome::focus(pane),
                None => InputOutcome::default(),
            }
        }
        KeyAction::InboxNext => inbox_jump(ui, snapshot, true),
        KeyAction::InboxPrev => inbox_jump(ui, snapshot, false),
        KeyAction::MarkToggle => match agent_row_mark_target_at(snapshot, ui, ui.selected_index) {
            Some(target) if target.unread => InputOutcome::mark_read(target.row_id),
            Some(target) => InputOutcome::mark_unread(target.row_id),
            None => InputOutcome::default(),
        },
        KeyAction::MarkAllRead => InputOutcome::mark_all_read(),
        KeyAction::Help => {
            ui.help_visible = true;
            InputOutcome::redraw()
        }
        KeyAction::Filter(action) => apply_make_up_filter(ui, snapshot, action),
        KeyAction::Dismiss => InputOutcome::dismiss(),
        KeyAction::Digit(digit) => {
            let index = usize::from(digit.saturating_sub(1));
            if let Some(pane) = active_roster(snapshot, ui).pane_at_ordinal(index) {
                return InputOutcome::focus(pane);
            }
            InputOutcome::default()
        }
        KeyAction::TabPrev => cycle_dashboard_tab(ui, snapshot, -1),
        KeyAction::TabNext => cycle_dashboard_tab(ui, snapshot, 1),
        KeyAction::Other => InputOutcome::default(),
    }
}

/// Step the dashboard's tab `step` entries left or right of the currently
/// active one, wrapping at the ends — the manual layer over the dashboard
/// default. A dashboard with fewer than two tabs has nothing to cycle.
fn cycle_dashboard_tab(ui: &mut UiState, snapshot: &SidebarSnapshot, step: isize) -> InputOutcome {
    if !dashboard_tabbed(snapshot) {
        return InputOutcome::default();
    }
    let tabs = dashboard_tabs(snapshot);
    if tabs.len() < 2 {
        return InputOutcome::default();
    }
    let current = active_dashboard_tab(snapshot, ui)
        .and_then(|active| tabs.iter().position(|tab| *tab == active))
        .unwrap_or(0);
    let len = tabs.len() as isize;
    let next = (current as isize + step).rem_euclid(len) as usize;
    pick_dashboard_tab(ui, snapshot, tabs[next].clone());
    InputOutcome::redraw()
}

/// Pin a manual dashboard-tab pick. The first pick captures the
/// selection-derived kind it began from — the clear condition — and a later
/// pick only moves the tab, so a browse through the tabs keeps one anchor and
/// a genuine selection change still ends it (the [`Browse`] discipline).
fn pick_dashboard_tab(ui: &mut UiState, snapshot: &SidebarSnapshot, kind: String) {
    let derived_at_start = match ui.dashboard_tab.take() {
        Some(tab) => tab.derived_at_start,
        None => selected_agent_kind(snapshot, ui),
    };
    ui.dashboard_tab = Some(DashboardTab {
        kind,
        derived_at_start,
    });
}

pub(super) fn handle_mouse_click(
    column: u16,
    row: u16,
    ui: &mut UiState,
    snapshot: &SidebarSnapshot,
) -> InputOutcome {
    match ui.interactions.target_at(column, row) {
        Some(HitTarget::ProviderTab(kind)) => {
            pick_dashboard_tab(ui, snapshot, kind);
            InputOutcome::redraw()
        }
        Some(HitTarget::BodyFilter(filter)) => {
            if toggle_make_up_filter(ui, snapshot, filter) {
                InputOutcome::sync_filter(ui.make_up_filter)
            } else {
                InputOutcome::default()
            }
        }
        Some(HitTarget::UnreadBanner) => {
            pin_manual_scroll(ui);
            ui.scroll_offset = 0;
            InputOutcome::redraw()
        }
        Some(HitTarget::ToggleGroup(group_key)) => {
            toggle_group_expanded(ui, snapshot, group_key);
            InputOutcome::redraw()
        }
        Some(HitTarget::Row(index)) => active_roster(snapshot, ui)
            .pane_at_ordinal(index)
            .map_or_else(InputOutcome::default, InputOutcome::focus),
        None => InputOutcome::default(),
    }
}

/// A wheel tick: move the viewport, never the selection. The first tick pins a
/// [`ManualScroll`] capturing the selection it began over — the clear condition
/// — and later ticks only move the window, so a long peek keeps one anchor.
/// Overshoot is fine: the draw clamps the offset to the zone and writes the
/// effective value back.
pub(super) fn handle_scroll(down: bool, ui: &mut UiState) -> InputOutcome {
    pin_manual_scroll(ui);
    ui.scroll_offset = if down {
        ui.scroll_offset.saturating_add(SCROLL_STEP)
    } else {
        ui.scroll_offset.saturating_sub(SCROLL_STEP)
    };
    InputOutcome::redraw()
}

/// Pin the viewport against auto-follow, capturing the current selection as the
/// snap-back condition. A pin already held keeps its original anchor.
fn pin_manual_scroll(ui: &mut UiState) {
    if ui.manual_scroll.is_none() {
        ui.manual_scroll = Some(ManualScroll {
            selection_at_start: ui.selected_pane.clone(),
        });
    }
}

fn toggle_group_expanded(ui: &mut UiState, snapshot: &SidebarSnapshot, group_key: String) {
    if !ui.expanded_groups.remove(&group_key) {
        ui.expanded_groups.insert(group_key);
    }
    anchor_selection(ui, snapshot);
    // A toggle reshapes the body below the clicked line, so hold the viewport
    // instead of snapping back to the selected card. Capture the post-toggle
    // selection because collapsing a selected group can clear it.
    ui.manual_scroll = Some(ManualScroll {
        selection_at_start: ui.selected_pane.clone(),
    });
}

/// Flip the make-up filter a bucket click asked for: the active bucket clears
/// back to show-all, any other becomes the pick. A pure toggle — no captured
/// baseline, unlike [`DashboardTab`], because there is no derived default to
/// fall back to. The body reshapes, so the explicit pick ends any viewport pin
/// (the [`select_row`] discipline) and the selection re-anchors at once: a
/// highlight whose row the filter hides drops to a clamped index, re-seated by
/// the held baseline when the filter clears.
fn apply_make_up_filter(
    ui: &mut UiState,
    snapshot: &SidebarSnapshot,
    filter: Option<BodyFilter>,
) -> InputOutcome {
    let changed = match filter {
        None => set_make_up_filter(ui, snapshot, None),
        Some(filter) => toggle_make_up_filter(ui, snapshot, filter),
    };
    if changed {
        InputOutcome::sync_filter(ui.make_up_filter)
    } else {
        InputOutcome::default()
    }
}

fn toggle_make_up_filter(ui: &mut UiState, snapshot: &SidebarSnapshot, filter: BodyFilter) -> bool {
    let target = if ui.make_up_filter == Some(filter) {
        None
    } else if filter.total(&snapshot.worktree_groups) > 0 {
        Some(filter)
    } else {
        return false;
    };
    set_make_up_filter(ui, snapshot, target)
}

pub(super) fn set_make_up_filter(
    ui: &mut UiState,
    snapshot: &SidebarSnapshot,
    filter: Option<BodyFilter>,
) -> bool {
    if ui.make_up_filter == filter {
        return false;
    }
    ui.make_up_filter = filter;
    ui.manual_scroll = None;
    anchor_selection(ui, snapshot);
    true
}

/// Jump the browse pick to the first visible row of the neighbouring worktree.
/// The walk uses the same filtered row universe as ordinary selection, so an
/// active make-up filter skips groups it emptied and the line map stays 1:1
/// with the highlighted row.
fn select_adjacent_worktree(
    ui: &mut UiState,
    snapshot: &SidebarSnapshot,
    step: isize,
) -> InputOutcome {
    let roster = active_roster(snapshot, ui);
    let selected = ui.selected_index.min(roster.len().saturating_sub(1));
    let Some(target) = roster.neighboring_group_head(selected, step) else {
        return InputOutcome::default();
    };
    select_row_in(ui, &roster, target);
    begin_or_continue_browse(ui);
    InputOutcome::redraw()
}

/// Point the highlight at a visible row by index — the identity-keyed selection
/// (`selected_pane`) plus its derived render index. A pure positioner for the
/// arrow-key browse; focus actions resolve their target through the active
/// roster instead and never move the highlight. An explicit pick ends any
/// viewport pin, so the viewport snaps back to following the selection.
fn select_row_in(ui: &mut UiState, roster: &VisibleRoster<'_>, index: usize) {
    ui.selected_index = index;
    ui.selected_pane = roster.pane_at_ordinal(index);
    ui.manual_scroll = None;
}

/// The id and unread bit of the visible agent row at `index` — the read/unread
/// toggle target, unlike the jump target, which is the row's pane. `m` acts on
/// inbox rows only, so a process row (no status) and an out-of-range
/// index both yield `None`, making the key a no-op rather than a durable write
/// the unread path would have to reject.
struct RowMarkTarget {
    row_id: String,
    unread: bool,
}

fn agent_row_mark_target_at(
    snapshot: &SidebarSnapshot,
    ui: &UiState,
    index: usize,
) -> Option<RowMarkTarget> {
    active_roster(snapshot, ui)
        .row(index)
        .filter(|row| row.status().is_some())
        .map(|row| RowMarkTarget {
            row_id: row.id.clone(),
            unread: row.unread,
        })
}

/// Pin the just-selected pane as the arrow-browse pick. The first arrow of a
/// browse captures the baseline it began from — the clear condition — and a
/// later arrow only moves the pick, so a long browse keeps one anchor and a
/// mid-browse baseline change still ends it. Roams every visible row, other
/// tabs' rows included.
fn begin_or_continue_browse(ui: &mut UiState) {
    if let Some(pane) = ui.selected_pane.clone() {
        let baseline_at_start = match ui.browse.take() {
            Some(browse) => browse.baseline_at_start,
            None => ui.baseline_pane.clone(),
        };
        ui.browse = Some(Browse {
            pane,
            baseline_at_start,
        });
    }
}

fn clamp_selection_in(ui: &mut UiState, roster: &VisibleRoster<'_>) {
    let len = roster.len();
    if len == 0 {
        ui.selected_index = 0;
    } else if ui.selected_index >= len {
        ui.selected_index = len - 1;
    }
}

/// Reconcile the highlight after folding a new snapshot. Selection is *derived*
/// state: the baseline is the session focus register, re-queried from the mux
/// every fold and updated by focus events between pulls; it cannot
/// desynchronize, only lag a frame. One transient local layer rides above it:
/// the arrow-key [`Browse`] pick. A jump moves no local state — its highlight
/// arrives here, when the baseline catches up. Keyed on pane identity, never
/// position.
///
/// `derived` is the snapshot's session-register derivation, pre-filtered at the
/// call site to a non-sidebar row: `Some(pane)` iff the focused pane is a row in
/// this snapshot; `None` otherwise.
///
/// Ordered rules:
/// 0. **Make-up filter.** The status filter clears when its bucket's
///    full-fleet count drops to zero — the body's twin of a dashboard-tab
///    pick ending when its panel leaves. First, because every rule below
///    walks the filtered universe.
/// 1. **Hold-last baseline.** A `Some` derivation advances `baseline_pane`; a
///    `None` holds it, so a momentary "no active row" gap (the sidebar itself
///    focused) never blanks or moves the highlight.
/// 2. **Browse.** A live browse pins its pick while the baseline still equals
///    the value captured at browse start; a genuine baseline change ends it.
/// 3. **Follow the baseline** — the steady state.
/// 4. **Reanchor.** State whose pane left the room is dropped, and
///    `anchor_selection` re-derives `selected_index` by identity.
/// 5. **Dashboard hold-last.** A selection-derived provider kind with a
///    dashboard panel advances the remembered dashboard default; non-agent
///    rows hold it.
/// 6. **Dashboard tab.** A manual tab pick ends when the selection-derived
///    provider kind genuinely changes from the value captured at pick time —
///    the dashboard's twin of the browse end-condition.
pub(super) fn reconcile_selection(
    ui: &mut UiState,
    snapshot: &SidebarSnapshot,
    derived: Option<PaneId>,
) {
    reconcile_filter_and_baseline(ui, snapshot, derived);
    reconcile_browse_and_selection(ui, snapshot);
    reconcile_dashboard(ui, snapshot);
}

fn reconcile_filter_and_baseline(
    ui: &mut UiState,
    snapshot: &SidebarSnapshot,
    derived: Option<PaneId>,
) {
    if let Some(filter) = ui.make_up_filter
        && filter.total(&snapshot.worktree_groups) == 0
    {
        ui.make_up_filter = None;
    }
    if let Some(pane) = derived {
        ui.baseline_pane = Some(pane);
    }
}

fn reconcile_browse_and_selection(ui: &mut UiState, snapshot: &SidebarSnapshot) {
    let mut browse_pinned = false;
    if let Some(browse) = ui.browse.take()
        && ui.baseline_pane == browse.baseline_at_start
    {
        ui.selected_pane = Some(browse.pane.clone());
        ui.browse = Some(browse);
        browse_pinned = true;
    }
    if !browse_pinned && let Some(pane) = ui.baseline_pane.clone() {
        ui.selected_pane = Some(pane);
    }

    let baseline = VisibleRoster::baseline(snapshot);
    if let Some(pane) = ui.baseline_pane.clone()
        && baseline.ordinal_of_pane(&pane).is_none()
    {
        ui.baseline_pane = None;
    }
    let roster = active_roster(snapshot, ui);
    if let Some(browse) = &ui.browse
        && roster.ordinal_of_pane(&browse.pane).is_none()
    {
        ui.browse = None;
    }
    anchor_selection_in(ui, &roster);

    if let Some(manual) = &ui.manual_scroll
        && ui.selected_pane != manual.selection_at_start
    {
        ui.manual_scroll = None;
    }
}

fn reconcile_dashboard(ui: &mut UiState, snapshot: &SidebarSnapshot) {
    let derived_kind = selected_agent_kind(snapshot, ui);
    let tabs = dashboard_tabs(snapshot);
    if let Some(kind) = &derived_kind
        && tabs.iter().any(|tab| tab == kind)
    {
        ui.last_agent_kind = Some(kind.clone());
    }
    if let Some(tab) = &ui.dashboard_tab {
        let derived_moved = derived_kind.is_some() && derived_kind != tab.derived_at_start;
        let tab_gone = !tabs.iter().any(|kind| kind == &tab.kind);
        if derived_moved || tab_gone {
            ui.dashboard_tab = None;
        }
    }
}

/// Re-derive `selected_index` from the identity-keyed `selected_pane`. When the
/// selected pane has left the room — or the make-up filter hides its row — drop
/// the dangling identity and clamp the index; the held baseline or the next
/// pick re-seats it.
pub(super) fn anchor_selection(ui: &mut UiState, snapshot: &SidebarSnapshot) {
    let roster = active_roster(snapshot, ui);
    anchor_selection_in(ui, &roster);
}

fn anchor_selection_in(ui: &mut UiState, roster: &VisibleRoster<'_>) {
    if let Some(pane) = ui.selected_pane.clone() {
        if let Some(index) = roster.ordinal_of_pane(&pane) {
            ui.selected_index = index;
            return;
        }
        ui.selected_pane = None;
    }
    clamp_selection_in(ui, roster);
}

/// The visible-row index backing `pane_id`, in `visible_rows` order under
/// `filter`. Pass `None` to ask about room membership regardless of the
/// make-up filter (the baseline's question), the active filter to ask about
/// the rendered body (the highlight's).
pub(super) fn row_index_of_pane(
    snapshot: &SidebarSnapshot,
    filter: Option<BodyFilter>,
    pane_id: &PaneId,
) -> Option<usize> {
    VisibleRoster::new(snapshot, filter, &Default::default(), None).ordinal_of_pane(pane_id)
}

/// The inbox triage list, stepped one row `forward` or backward from
/// `selected`. The list is unread needs-a-look rows (oldest episode first) then
/// read actionable rows (oldest first); `forward` wraps to the next, backward to
/// the previous, and a selection outside the list enters at the first row
/// forward or the last row backward.
fn step_attention_in(roster: &VisibleRoster<'_>, selected: usize, forward: bool) -> Option<usize> {
    let mut candidates = roster
        .rows()
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(index, row)| triage_key(row).map(|key| (index, key)))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(_, key)| *key);
    let candidates = candidates
        .into_iter()
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }
    let len = candidates.len();
    candidates
        .iter()
        .position(|index| *index == selected)
        .map(|position| {
            let stepped = if forward {
                position + 1
            } else {
                position + len - 1
            };
            candidates[stepped % len]
        })
        .or_else(|| {
            if forward {
                candidates.first().copied()
            } else {
                candidates.last().copied()
            }
        })
}

#[cfg(test)]
fn select_row(ui: &mut UiState, snapshot: &SidebarSnapshot, index: usize) {
    let roster = active_roster(snapshot, ui);
    select_row_in(ui, &roster, index);
}

#[cfg(test)]
fn step_attention_index(
    snapshot: &SidebarSnapshot,
    filter: Option<BodyFilter>,
    expanded_groups: &std::collections::BTreeSet<String>,
    selected: usize,
    forward: bool,
) -> Option<usize> {
    let roster = VisibleRoster::new(snapshot, filter, expanded_groups, None);
    step_attention_in(&roster, selected, forward)
}

fn active_roster<'a>(snapshot: &'a SidebarSnapshot, ui: &UiState) -> VisibleRoster<'a> {
    VisibleRoster::new(
        snapshot,
        ui.make_up_filter,
        &ui.expanded_groups,
        ui.held_visible(),
    )
}

#[cfg(test)]
mod tests;
