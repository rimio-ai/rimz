//! The selection model: an identity-keyed highlight over a derived baseline,
//! the transient arrow-key browse layer above it, the key/mouse handlers that
//! act on it, and the hit-test reader over the render-built line map.

use crate::SidebarSnapshot;
use crate::feed::AgentStatus;
use crate::ids::PaneId;

use crate::sidebar_pane::render::{
    BodyFilter, Browse, DashboardTab, ManualScroll, UiState, active_provider_kind,
    dashboard_tabbed, row_passes_filter, selected_agent_kind, status_total, unread_total,
};

use super::input::{FilterAction, KeyAction};

/// Content lines a wheel tick moves the viewport — about one card line-group
/// per notch, so a flick traverses cards rather than crawling line by line.
const SCROLL_STEP: usize = 3;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct InputOutcome {
    pub(super) redraw: bool,
    /// The pane to fire the one-way focus command at — `Some` only on a jump
    /// action. The handler resolves the target and returns it without moving
    /// the highlight: selection stays derived state, so there is nothing to
    /// repaint until the baseline catches up.
    pub(super) focus: Option<PaneId>,
    pub(super) dismiss: bool,
    /// The row id to mark read / unread without jumping — `Some` only on the
    /// `m`/`M` keys. The durable receipt write, the instant local clear, and
    /// the re-derive live in the loop (`on_input`), which owns the read-mark
    /// store and the runtime paths; the handler only names the target row.
    pub(super) mark_read: Option<String>,
    pub(super) mark_unread: Option<String>,
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
            focus: Some(pane),
            ..Self::default()
        }
    }

    pub(super) fn dismiss() -> Self {
        Self {
            redraw: true,
            dismiss: true,
            ..Self::default()
        }
    }

    /// Mark the selected row read / unread. No redraw here: the loop clears the
    /// row and repaints once after the durable write, so the frame never
    /// flashes the pre-clear state first.
    fn mark_read(row_id: String) -> Self {
        Self {
            mark_read: Some(row_id),
            ..Self::default()
        }
    }

    fn mark_unread(row_id: String) -> Self {
        Self {
            mark_unread: Some(row_id),
            ..Self::default()
        }
    }
}

/// Fire a jump at `pane` and end any make-up filter it leaves behind. A status
/// filter is a transient lens on one tab's body; carrying it past a jump would
/// leave that tab silently narrowed on return, out of step with the fleet every
/// other tab shows. The caller resolves the target in the filtered body first,
/// so the jump still lands where the user pointed; the clear follows. Clearing
/// reshapes the body, so the outcome repaints when a filter was actually live.
fn jump_to(ui: &mut UiState, snapshot: &SidebarSnapshot, pane: PaneId) -> InputOutcome {
    let mut outcome = InputOutcome::focus(pane);
    outcome.redraw = set_make_up_filter(ui, snapshot, None);
    outcome
}

/// Step the inbox triage list `forward` or backward from the current selection
/// and jump to that row, focusing its pane — the `n`/`N` (and `Space`) walk
/// through the rows that need you, oldest episode first. A walk with nothing to
/// triage does nothing.
fn inbox_jump(ui: &mut UiState, snapshot: &SidebarSnapshot, forward: bool) -> InputOutcome {
    if let Some(index) =
        step_attention_index(snapshot, ui.make_up_filter, ui.selected_index, forward)
        && let Some(pane) = pane_at_row(snapshot, ui.make_up_filter, index)
    {
        return jump_to(ui, snapshot, pane);
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
    let len = visible_row_count(snapshot, ui.make_up_filter);
    if len == 0 {
        return InputOutcome::default();
    }
    let target = match end {
        End::Top => 0,
        End::Bottom => len - 1,
    };
    if ui.selected_index == target {
        return InputOutcome::default();
    }
    select_row(ui, snapshot, target);
    begin_or_continue_browse(ui);
    InputOutcome::redraw()
}

pub(super) fn handle_key(
    action: KeyAction,
    ui: &mut UiState,
    snapshot: &SidebarSnapshot,
) -> InputOutcome {
    match action {
        KeyAction::Up => {
            if ui.selected_index > 0 {
                select_row(ui, snapshot, ui.selected_index - 1);
                begin_or_continue_browse(ui);
                return InputOutcome::redraw();
            }
            InputOutcome::default()
        }
        KeyAction::Down => {
            let len = visible_row_count(snapshot, ui.make_up_filter);
            if ui.selected_index + 1 < len {
                select_row(ui, snapshot, ui.selected_index + 1);
                begin_or_continue_browse(ui);
                return InputOutcome::redraw();
            }
            InputOutcome::default()
        }
        KeyAction::WorktreeUp => select_adjacent_worktree(ui, snapshot, -1),
        KeyAction::WorktreeDown => select_adjacent_worktree(ui, snapshot, 1),
        KeyAction::Top => select_end_row(ui, snapshot, End::Top),
        KeyAction::Bottom => select_end_row(ui, snapshot, End::Bottom),
        KeyAction::Enter => {
            // Jump on the current row: fire the focus command at the selected
            // pane without moving the highlight — it follows once the derived
            // baseline catches up, identical to a click.
            match ui.selected_pane.clone() {
                Some(pane) => jump_to(ui, snapshot, pane),
                None => InputOutcome::default(),
            }
        }
        KeyAction::InboxNext => inbox_jump(ui, snapshot, true),
        KeyAction::InboxPrev => inbox_jump(ui, snapshot, false),
        KeyAction::MarkRead => {
            match agent_row_id_at(snapshot, ui.make_up_filter, ui.selected_index) {
                Some(row_id) => InputOutcome::mark_read(row_id),
                None => InputOutcome::default(),
            }
        }
        KeyAction::MarkUnread => {
            match agent_row_id_at(snapshot, ui.make_up_filter, ui.selected_index) {
                Some(row_id) => InputOutcome::mark_unread(row_id),
                None => InputOutcome::default(),
            }
        }
        KeyAction::Help => {
            ui.help_visible = !ui.help_visible;
            if ui.help_visible {
                // The overlay lives at the scroll zone's tail: jump the
                // viewport to the end so toggling help always reveals it — the
                // draw clamps the sentinel to the zone's last window. The open
                // overlay itself owns the viewport (the draw suppresses
                // auto-follow while `help_visible`), so selection churn beneath
                // it never pulls the view away mid-read; the wheel may roam.
                ui.scroll_offset = usize::MAX;
            } else {
                // Closing drops any wheel peek made while reading, so the view
                // snaps back to the selected card.
                ui.manual_scroll = None;
            }
            InputOutcome::redraw()
        }
        KeyAction::Filter(action) => apply_make_up_filter(ui, snapshot, action),
        KeyAction::Dismiss => InputOutcome::dismiss(),
        KeyAction::Digit(digit) => {
            let index = usize::from(digit.saturating_sub(1));
            if let Some(pane) = pane_at_row(snapshot, ui.make_up_filter, index) {
                return jump_to(ui, snapshot, pane);
            }
            InputOutcome::default()
        }
        KeyAction::TabPrev => cycle_dashboard_tab(ui, snapshot, -1),
        KeyAction::TabNext => cycle_dashboard_tab(ui, snapshot, 1),
    }
}

/// Step the dashboard's tab `step` panels left or right of the currently
/// active one, wrapping at the ends — the manual layer over the
/// selection-derived default ([`active_provider_kind`]). A dashboard with
/// fewer than two panels has nothing to cycle.
fn cycle_dashboard_tab(ui: &mut UiState, snapshot: &SidebarSnapshot, step: isize) -> InputOutcome {
    if !dashboard_tabbed(snapshot) {
        return InputOutcome::default();
    }
    let panels = &snapshot.providers;
    if panels.len() < 2 {
        return InputOutcome::default();
    }
    let current = active_provider_kind(snapshot, ui)
        .and_then(|kind| panels.iter().position(|panel| panel.kind == kind))
        .unwrap_or(0);
    let len = panels.len() as isize;
    let next = (current as isize + step).rem_euclid(len) as usize;
    pick_dashboard_tab(ui, snapshot, panels[next].kind.clone());
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
    // The dashboard's tabs are the bottom block's only hit targets — a
    // click on one picks that tab in place, never a jump.
    if let Some(kind) = tab_kind_at(ui, column, row) {
        pick_dashboard_tab(ui, snapshot, kind);
        return InputOutcome::redraw();
    }
    // The cockpit's make-up buckets are the top block's only hit targets — a
    // click on one toggles the body's status filter in place, never a jump.
    if let Some(status) = make_up_status_at(ui, column, row) {
        return if toggle_make_up_filter(ui, snapshot, BodyFilter::Status(status)) {
            InputOutcome::redraw()
        } else {
            InputOutcome::default()
        };
    }
    if let Some(index) = row_index_at_screen_position(ui, row)
        && let Some(pane) = pane_at_row(snapshot, ui.make_up_filter, index)
    {
        return jump_to(ui, snapshot, pane);
    }
    InputOutcome::default()
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

/// The provider kind whose tab sits under a click, from the tab hit map
/// the renderer emitted in lockstep with the frame (`UiState::tab_hits`, the
/// tab rail's twin of `line_map`).
fn tab_kind_at(ui: &UiState, column: u16, row: u16) -> Option<String> {
    ui.tab_hits
        .iter()
        .find(|hit| hit.line == usize::from(row) && column >= hit.col_start && column < hit.col_end)
        .map(|hit| hit.kind.clone())
}

/// The status whose make-up bucket sits under a click, from the make-up hit
/// map the renderer emitted in lockstep with the frame (`UiState::make_up_hits`,
/// the cockpit's twin of `tab_hits`). A zero bucket emitted no hit, so it can
/// never match — inert, as if not a tab.
fn make_up_status_at(ui: &UiState, column: u16, row: u16) -> Option<AgentStatus> {
    ui.make_up_hits
        .iter()
        .find(|hit| hit.line == usize::from(row) && column >= hit.col_start && column < hit.col_end)
        .map(|hit| hit.status)
}

/// Flip the make-up filter a bucket click asked for: the active bucket clears
/// back to show-all, any other becomes the pick. A pure toggle — no captured
/// baseline, unlike [`DashboardTab`], because there is no derived default to
/// fall back to. The body reshapes, so the explicit pick ends any wheel pin
/// (the [`select_row`] discipline) and the selection re-anchors at once: a
/// highlight whose row the filter hides drops to a clamped index, re-seated by
/// the held baseline when the filter clears.
fn apply_make_up_filter(
    ui: &mut UiState,
    snapshot: &SidebarSnapshot,
    action: FilterAction,
) -> InputOutcome {
    let changed = match action {
        FilterAction::All => set_make_up_filter(ui, snapshot, None),
        FilterAction::Status(status) => {
            toggle_make_up_filter(ui, snapshot, BodyFilter::Status(status))
        }
        FilterAction::Unread => toggle_make_up_filter(ui, snapshot, BodyFilter::Unread),
    };
    if changed {
        InputOutcome::redraw()
    } else {
        InputOutcome::default()
    }
}

fn toggle_make_up_filter(ui: &mut UiState, snapshot: &SidebarSnapshot, filter: BodyFilter) -> bool {
    let target = if ui.make_up_filter == Some(filter) {
        None
    } else if filter_total(snapshot, filter) > 0 {
        Some(filter)
    } else {
        return false;
    };
    set_make_up_filter(ui, snapshot, target)
}

fn set_make_up_filter(
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
    let ranges = visible_group_ranges(snapshot, ui.make_up_filter);
    if ranges.len() < 2 {
        return InputOutcome::default();
    }
    let Some(row_count) = ranges.last().map(|range| range.end()) else {
        return InputOutcome::default();
    };
    let selected = ui.selected_index.min(row_count.saturating_sub(1));
    let Some(current) = ranges.iter().position(|range| range.contains(selected)) else {
        return InputOutcome::default();
    };
    let target = if step < 0 {
        current.checked_sub(1)
    } else {
        (current + 1 < ranges.len()).then_some(current + 1)
    };
    let Some(target) = target else {
        return InputOutcome::default();
    };
    select_row(ui, snapshot, ranges[target].start);
    begin_or_continue_browse(ui);
    InputOutcome::redraw()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VisibleGroupRange {
    start: usize,
    len: usize,
}

impl VisibleGroupRange {
    fn end(&self) -> usize {
        self.start + self.len
    }

    fn contains(&self, index: usize) -> bool {
        (self.start..self.end()).contains(&index)
    }
}

fn visible_group_ranges(
    snapshot: &SidebarSnapshot,
    filter: Option<BodyFilter>,
) -> Vec<VisibleGroupRange> {
    let mut start = 0;
    let mut ranges = Vec::new();
    for group in &snapshot.worktree_groups {
        let len = group
            .rows
            .iter()
            .filter(|row| row_passes_filter(row, filter))
            .count();
        if len > 0 {
            ranges.push(VisibleGroupRange { start, len });
            start += len;
        }
    }
    ranges
}

/// Point the highlight at a visible row by index — the identity-keyed selection
/// (`selected_pane`) plus its derived render index. A pure positioner for the
/// arrow-key browse; the jump actions resolve their target through
/// [`pane_at_row`] instead and never move the highlight. An explicit pick ends
/// any wheel pin, so the viewport snaps back to following the selection.
fn select_row(ui: &mut UiState, snapshot: &SidebarSnapshot, index: usize) {
    ui.selected_index = index;
    ui.selected_pane = pane_at_row(snapshot, ui.make_up_filter, index);
    ui.manual_scroll = None;
}

/// The pane backing visible row `index`, or `None` for a pane-less row or an
/// out-of-range index.
fn pane_at_row(
    snapshot: &SidebarSnapshot,
    filter: Option<BodyFilter>,
    index: usize,
) -> Option<PaneId> {
    visible_rows(snapshot, filter)
        .nth(index)
        .and_then(|row| row.pane.as_ref())
        .map(|pane| pane.pane_id.clone())
}

/// The id of the visible agent row at `index` — the read/unread mark target
/// (the receipt key), unlike the jump target, which is the row's pane. `m`/`M`
/// act on inbox rows only, so a process row (no status) and an out-of-range
/// index both yield `None`, making the key a no-op rather than a durable write
/// the unread path would have to reject.
fn agent_row_id_at(
    snapshot: &SidebarSnapshot,
    filter: Option<BodyFilter>,
    index: usize,
) -> Option<String> {
    visible_rows(snapshot, filter)
        .nth(index)
        .filter(|row| row.status().is_some())
        .map(|row| row.id.clone())
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

fn clamp_selection(ui: &mut UiState, snapshot: &SidebarSnapshot) {
    let len = visible_row_count(snapshot, ui.make_up_filter);
    if len == 0 {
        ui.selected_index = 0;
    } else if ui.selected_index >= len {
        ui.selected_index = len - 1;
    }
}

/// Reconcile the highlight after folding a new snapshot. Selection is *derived*
/// state: the baseline is the own view's active working pane, re-queried from
/// the mux every fold — same-tab by construction — so the highlight always
/// reconverges on where the user actually is; it cannot desynchronize, only lag
/// a frame. One transient local layer rides above it: the arrow-key [`Browse`]
/// pick. A jump moves no local state — its highlight arrives here, when the
/// baseline catches up. Keyed on pane identity, never position.
///
/// `derived` is the snapshot's active-pane derivation, pre-filtered at the call
/// site to a non-sidebar row: `Some(pane)` iff `!own_is_active` and the view's
/// active pane is a row in this snapshot; `None` otherwise.
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
/// 5. **Dashboard tab.** A manual tab pick ends when the selection-derived
///    provider kind genuinely changes from the value captured at pick time —
///    the dashboard's twin of the browse end-condition.
pub(super) fn reconcile_selection(
    ui: &mut UiState,
    snapshot: &SidebarSnapshot,
    derived: Option<PaneId>,
) {
    // 0. The make-up filter ends when its bucket empties. The check reads the
    //    full-fleet `status_counts` sum — exactly the figure the make-up line
    //    displays — so the filter clears in the same fold its bucket reads 0,
    //    and a click-then-fold race self-heals here.
    if let Some(filter) = ui.make_up_filter
        && filter_total(snapshot, filter) == 0
    {
        ui.make_up_filter = None;
    }

    // 1. Hold-last baseline: a Some derivation advances it, a None holds it.
    if let Some(pane) = derived {
        ui.baseline_pane = Some(pane);
    }

    // 2. Browse: hold the roamed pick while the baseline hasn't genuinely
    //    moved; on a baseline change the take stands — the browse ends and the
    //    highlight follows the new baseline.
    let mut pinned = false;
    if let Some(browse) = ui.browse.take()
        && ui.baseline_pane == browse.baseline_at_start
    {
        ui.selected_pane = Some(browse.pane.clone());
        ui.browse = Some(browse);
        pinned = true;
    }

    // 3. Steady state: the highlight is the derived baseline.
    if !pinned && let Some(pane) = ui.baseline_pane.clone() {
        ui.selected_pane = Some(pane);
    }

    // 4. Drop state whose pane left the room — so a pick whose pane closed
    //    stops shadowing the baseline — then re-anchor by identity. The
    //    baseline check is deliberately unfiltered: the mux's active pane is
    //    real regardless of the cosmetic make-up filter, so a hidden baseline
    //    holds and re-seats the highlight the moment the filter clears. The
    //    browse pick *is* filtered — it roams the visible rows, so a pick the
    //    filter hides has nothing to render and drops.
    if let Some(pane) = ui.baseline_pane.clone()
        && row_index_of_pane(snapshot, None, &pane).is_none()
    {
        ui.baseline_pane = None;
    }
    if let Some(browse) = &ui.browse
        && row_index_of_pane(snapshot, ui.make_up_filter, &browse.pane).is_none()
    {
        ui.browse = None;
    }
    anchor_selection(ui, snapshot);

    // 5. A wheel pin holds the viewport only while the selection it began over
    //    stands; a genuine selection change — a jump landing, an external focus
    //    move — ends the peek and the viewport snaps back to the selected card.
    if let Some(manual) = &ui.manual_scroll
        && ui.selected_pane != manual.selection_at_start
    {
        ui.manual_scroll = None;
    }

    // 6. The manual dashboard-tab pick ends like a browse: a selection-derived
    //    provider kind that *genuinely* changed from the value captured at pick
    //    time hands the tab back to the derived default. A `None` derivation —
    //    a process row, an empty room — holds the pick, so jumping through a
    //    shell pane never loses it; a pick whose panel left the dashboard is
    //    dropped too.
    if let Some(tab) = &ui.dashboard_tab {
        let derived = selected_agent_kind(snapshot, ui);
        let derived_moved = derived.is_some() && derived != tab.derived_at_start;
        let panel_gone = !snapshot
            .providers
            .iter()
            .any(|panel| panel.kind == tab.kind);
        if derived_moved || panel_gone {
            ui.dashboard_tab = None;
        }
    }
}

/// Re-derive `selected_index` from the identity-keyed `selected_pane`. When the
/// selected pane has left the room — or the make-up filter hides its row — drop
/// the dangling identity and clamp the index; the held baseline or the next
/// pick re-seats it.
fn anchor_selection(ui: &mut UiState, snapshot: &SidebarSnapshot) {
    if let Some(pane) = ui.selected_pane.clone() {
        if let Some(index) = row_index_of_pane(snapshot, ui.make_up_filter, &pane) {
            ui.selected_index = index;
            return;
        }
        ui.selected_pane = None;
    }
    clamp_selection(ui, snapshot);
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
    visible_rows(snapshot, filter).position(|row| {
        row.pane
            .as_ref()
            .is_some_and(|pane| pane.pane_id == *pane_id)
    })
}

/// The hit-test reader, and deliberately nothing more: `UiState::line_map` —
/// built in lockstep with the body by `render::compose_lines` — is the **only**
/// row-geometry source. Never re-derive a row's screen position here (counting
/// header/gap lines, card heights, or clip offsets); any parallel math would
/// drift from the renderer the first time a section changes shape.
fn row_index_at_screen_position(ui: &UiState, row: u16) -> Option<usize> {
    // Borderless: the body fills the frame from row 0 (no border to skip) and a
    // row's lane spine occupies column 0, so a click anywhere on a line — spine
    // included — maps straight onto the hit-test entry built alongside it.
    ui.line_map.get(usize::from(row)).copied().flatten()
}

fn visible_row_count(snapshot: &SidebarSnapshot, filter: Option<BodyFilter>) -> usize {
    visible_rows(snapshot, filter).count()
}

/// Every rendered row in body order — the snapshot's groups flattened through
/// the one shared [`row_passes_filter`] predicate, so these ordinals are
/// exactly the `line_map` ordinals the renderer builds.
fn visible_rows(
    snapshot: &SidebarSnapshot,
    filter: Option<BodyFilter>,
) -> impl Iterator<Item = &crate::SidebarRow> {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
        .filter(move |row| row_passes_filter(row, filter))
}

/// The inbox triage list, stepped one row `forward` or backward from
/// `selected`. The list is unread needs-a-look rows (oldest episode first) then
/// read actionable rows (oldest first); `forward` wraps to the next, backward to
/// the previous, and a selection outside the list enters at the first row
/// forward or the last row backward.
fn step_attention_index(
    snapshot: &SidebarSnapshot,
    filter: Option<BodyFilter>,
    selected: usize,
    forward: bool,
) -> Option<usize> {
    let rows = visible_rows(snapshot, filter).collect::<Vec<_>>();
    let mut unread: Vec<_> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.unread && row.status().is_some_and(AgentStatus::needs_a_look))
        .collect();
    unread.sort_by_key(|(_, row)| row.last_activity);
    let mut actionable: Vec<_> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| !row.unread && row.status().is_some_and(AgentStatus::is_actionable))
        .collect();
    actionable.sort_by_key(|(_, row)| row.last_activity);
    let candidates: Vec<usize> = unread
        .into_iter()
        .chain(actionable)
        .map(|(index, _)| index)
        .collect();
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

fn filter_total(snapshot: &SidebarSnapshot, filter: BodyFilter) -> usize {
    match filter {
        BodyFilter::Status(status) => status_total(&snapshot.worktree_groups, status),
        BodyFilter::Unread => unread_total(&snapshot.worktree_groups),
    }
}

#[cfg(test)]
mod tests;
