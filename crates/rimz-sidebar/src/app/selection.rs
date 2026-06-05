//! The selection model: an identity-keyed highlight over a derived baseline,
//! the transient arrow-key browse layer above it, the key/mouse handlers that
//! act on it, and the hit-test reader over the render-built line map.

use rimz::SidebarSnapshot;
use rimz::ids::PaneId;

use crate::render::{Browse, UiState};

use super::input::KeyAction;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct InputOutcome {
    pub(super) redraw: bool,
    /// The pane to fire the one-way focus command at — `Some` only on a jump
    /// action. The handler resolves the target and returns it without moving
    /// the highlight: selection stays derived state, so there is nothing to
    /// repaint until the baseline catches up.
    pub(super) focus: Option<PaneId>,
    pub(super) dismiss: bool,
}

impl InputOutcome {
    pub(super) fn redraw() -> Self {
        Self {
            redraw: true,
            focus: None,
            dismiss: false,
        }
    }

    pub(super) fn focus(pane: PaneId) -> Self {
        Self {
            redraw: false,
            focus: Some(pane),
            dismiss: false,
        }
    }

    pub(super) fn dismiss() -> Self {
        Self {
            redraw: true,
            focus: None,
            dismiss: true,
        }
    }
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
            let len = visible_row_count(snapshot);
            if ui.selected_index + 1 < len {
                select_row(ui, snapshot, ui.selected_index + 1);
                begin_or_continue_browse(ui);
                return InputOutcome::redraw();
            }
            InputOutcome::default()
        }
        KeyAction::Enter => {
            // Jump on the current row: fire the focus command at the selected
            // pane without touching selection — the highlight follows once the
            // derived baseline catches up, identical to a click.
            match ui.selected_pane.clone() {
                Some(pane) => InputOutcome::focus(pane),
                None => InputOutcome::default(),
            }
        }
        KeyAction::Space => {
            if let Some(index) = next_attention_index(snapshot, ui.selected_index)
                && let Some(pane) = pane_at_row(snapshot, index)
            {
                return InputOutcome::focus(pane);
            }
            InputOutcome::default()
        }
        KeyAction::Help => {
            ui.help_visible = !ui.help_visible;
            InputOutcome::redraw()
        }
        KeyAction::Dismiss => InputOutcome::dismiss(),
        KeyAction::Digit(digit) => {
            let index = usize::from(digit.saturating_sub(1));
            if let Some(pane) = pane_at_row(snapshot, index) {
                return InputOutcome::focus(pane);
            }
            InputOutcome::default()
        }
    }
}

pub(super) fn handle_mouse_click(
    _column: u16,
    row: u16,
    ui: &mut UiState,
    snapshot: &SidebarSnapshot,
) -> InputOutcome {
    if let Some(index) = row_index_at_screen_position(ui, row)
        && let Some(pane) = pane_at_row(snapshot, index)
    {
        return InputOutcome::focus(pane);
    }
    InputOutcome::default()
}

/// Point the highlight at a visible row by index — the identity-keyed selection
/// (`selected_pane`) plus its derived render index. A pure positioner for the
/// arrow-key browse; the jump actions resolve their target through
/// [`pane_at_row`] instead and never move the highlight.
fn select_row(ui: &mut UiState, snapshot: &SidebarSnapshot, index: usize) {
    ui.selected_index = index;
    ui.selected_pane = pane_at_row(snapshot, index);
}

/// The pane backing visible row `index`, or `None` for a pane-less row or an
/// out-of-range index.
fn pane_at_row(snapshot: &SidebarSnapshot, index: usize) -> Option<PaneId> {
    visible_rows(snapshot)
        .nth(index)
        .and_then(|row| row.pane.as_ref())
        .map(|pane| pane.pane_id.clone())
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
    let len = visible_row_count(snapshot);
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
/// 1. **Hold-last baseline.** A `Some` derivation advances `baseline_pane`; a
///    `None` holds it, so a momentary "no active row" gap (the sidebar itself
///    focused) never blanks or moves the highlight.
/// 2. **Browse.** A live browse pins its pick while the baseline still equals
///    the value captured at browse start; a genuine baseline change ends it.
/// 3. **Follow the baseline** — the steady state.
/// 4. **Reanchor.** State whose pane left the room is dropped, and
///    `anchor_selection` re-derives `selected_index` by identity.
pub(super) fn reconcile_selection(
    ui: &mut UiState,
    snapshot: &SidebarSnapshot,
    derived: Option<PaneId>,
) {
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
    //    stops shadowing the baseline — then re-anchor by identity.
    if let Some(pane) = ui.baseline_pane.clone()
        && row_index_of_pane(snapshot, &pane).is_none()
    {
        ui.baseline_pane = None;
    }
    if let Some(browse) = &ui.browse
        && row_index_of_pane(snapshot, &browse.pane).is_none()
    {
        ui.browse = None;
    }
    anchor_selection(ui, snapshot);
}

/// Re-derive `selected_index` from the identity-keyed `selected_pane`. When the
/// selected pane has left the room its row is gone, so drop the dangling
/// identity and clamp the index — the next mirror report or pick re-seats it.
fn anchor_selection(ui: &mut UiState, snapshot: &SidebarSnapshot) {
    if let Some(pane) = ui.selected_pane.clone() {
        if let Some(index) = row_index_of_pane(snapshot, &pane) {
            ui.selected_index = index;
            return;
        }
        ui.selected_pane = None;
    }
    clamp_selection(ui, snapshot);
}

/// The visible-row index backing `pane_id`, in `visible_rows` order.
pub(super) fn row_index_of_pane(snapshot: &SidebarSnapshot, pane_id: &PaneId) -> Option<usize> {
    visible_rows(snapshot).position(|row| {
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

fn visible_row_count(snapshot: &SidebarSnapshot) -> usize {
    snapshot
        .worktree_groups
        .iter()
        .map(|group| group.rows.len())
        .sum()
}

fn visible_rows(snapshot: &SidebarSnapshot) -> impl Iterator<Item = &rimz::SidebarRow> {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
}

fn next_attention_index(snapshot: &SidebarSnapshot, selected: usize) -> Option<usize> {
    let rows = visible_rows(snapshot).collect::<Vec<_>>();
    if rows.is_empty() {
        return None;
    }
    let start = selected.saturating_add(1);
    (0..rows.len()).find_map(|offset| {
        let index = (start + offset) % rows.len();
        rows[index]
            .status
            .is_some_and(rimz::feed::AgentStatus::is_actionable)
            .then_some(index)
    })
}

#[cfg(test)]
mod tests;
