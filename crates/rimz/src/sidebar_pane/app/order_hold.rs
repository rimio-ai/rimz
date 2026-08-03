//! Renderer-local row/group order hold.
//!
//! Ranking remains producer truth. This module reapplies the last painted order
//! over a fresh, already-ranked snapshot for a short interaction window and
//! splices items born during the hold into their producer-ranked position, so
//! read state and attention signals update without moving the cards under the
//! user's eyes.

use std::collections::{HashMap, HashSet};

use crate::agents::AgentStatus;
use crate::ids::PaneId;
use crate::sidebar::timing::REORDER_HOLD;
use crate::sidebar_pane::render::{FrozenOrder, FrozenRow, OrderHold, UiState};
use crate::sidebar_pane::view::VisibleRoster;
use crate::store::snapshot::SidebarSnapshot;

/// The focused row leaving the attention class or entering `Running` is the
/// user acting on that agent in its own pane -- answering its ask or submitting
/// a prompt -- the moment their eyes return to the sidebar. It arms the same
/// hold a sidebar interaction does.
pub(super) fn focused_interaction(
    prev: &SidebarSnapshot,
    current: &SidebarSnapshot,
    selected: Option<&PaneId>,
) -> bool {
    let Some(selected) = selected else {
        return false;
    };
    let prev_status = super::state::row_of_pane(prev, selected).and_then(|row| row.status());
    let next_status = super::state::row_of_pane(current, selected).and_then(|row| row.status());
    let answered = prev_status.is_some_and(AgentStatus::is_attention)
        && !next_status.is_some_and(AgentStatus::is_attention);
    let prompted =
        next_status == Some(AgentStatus::Running) && prev_status != Some(AgentStatus::Running);
    answered || prompted
}

pub(super) fn arm_order_hold(ui: &mut UiState, now_ms: i64) {
    ui.order_hold = Some(OrderHold {
        frozen: ui.last_order.clone(),
        expires_ms: now_ms + REORDER_HOLD.as_millis() as i64,
    });
}

pub(super) fn apply_order_hold(
    ui: &mut UiState,
    current: &mut SidebarSnapshot,
    interacted: bool,
    now_ms: i64,
) {
    if interacted {
        arm_order_hold(ui, now_ms);
    } else if ui
        .order_hold
        .as_ref()
        .is_some_and(|hold| now_ms >= hold.expires_ms)
    {
        ui.order_hold = None;
    }
    if let Some(hold) = ui.order_hold.as_mut() {
        migrate_frozen_order(current, &mut hold.frozen);
        admit_new_items(current, &mut hold.frozen);
        reorder_to_frozen(current, &hold.frozen);
    }
    if ui.order_hold.is_some() {
        super::selection::anchor_selection(ui, current);
    }
}

pub(super) fn capture_order(current: &SidebarSnapshot, ui: &UiState) -> FrozenOrder {
    let roster = VisibleRoster::new(
        current,
        ui.make_up_filter,
        &ui.expanded_groups,
        ui.held_visible(),
    );
    let visible: HashSet<String> = roster
        .rows()
        .iter()
        .copied()
        .map(|row| row.id.clone())
        .collect();
    FrozenOrder {
        groups: current
            .worktree_groups
            .iter()
            .map(|group| group.key.clone())
            .collect(),
        rows: current
            .worktree_groups
            .iter()
            .flat_map(|group| {
                group.rows.iter().map(|row| FrozenRow {
                    id: row.id.clone(),
                    pane: row.pane.as_ref().map(|pane| pane.pane_id.to_string()),
                })
            })
            .collect(),
        visible,
    }
}

pub(super) fn adopt_shared_hold(
    ui: &mut UiState,
    current: &mut SidebarSnapshot,
    mut order: FrozenOrder,
    stamp_ms: i64,
) {
    migrate_frozen_order(current, &mut order);
    admit_new_items(current, &mut order);
    reorder_to_frozen(current, &order);
    ui.order_hold = Some(OrderHold {
        frozen: order,
        expires_ms: stamp_ms + REORDER_HOLD.as_millis() as i64,
    });
    super::selection::anchor_selection(ui, current);
    ui.last_order = capture_order(current, ui);
}

fn reorder_to_frozen(current: &mut SidebarSnapshot, frozen: &FrozenOrder) {
    let group_pos: HashMap<&str, usize> = frozen
        .groups
        .iter()
        .enumerate()
        .map(|(index, key)| (key.as_str(), index))
        .collect();
    let row_pos: HashMap<&str, usize> = frozen
        .rows
        .iter()
        .enumerate()
        .map(|(index, row)| (row.id.as_str(), index))
        .collect();

    current.worktree_groups.sort_by_key(|group| {
        group_pos
            .get(group.key.as_str())
            .copied()
            .unwrap_or(usize::MAX)
    });
    for group in &mut current.worktree_groups {
        group
            .rows
            .sort_by_key(|row| row_pos.get(row.id.as_str()).copied().unwrap_or(usize::MAX));
    }
}

fn migrate_frozen_order(current: &SidebarSnapshot, frozen: &mut FrozenOrder) {
    let mut frozen_ids: HashSet<String> = frozen.rows.iter().map(|row| row.id.clone()).collect();
    let pane_pos: HashMap<String, usize> = frozen
        .rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| row.pane.clone().map(|pane| (pane, index)))
        .collect();
    let mut migrated = HashSet::new();

    for current_row in current
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
    {
        if frozen_ids.contains(&current_row.id) {
            continue;
        }
        let Some(pane) = current_row
            .pane
            .as_ref()
            .map(|pane| pane.pane_id.to_string())
        else {
            continue;
        };
        let Some(index) = pane_pos.get(&pane).copied() else {
            continue;
        };
        if !migrated.insert(index) {
            continue;
        }

        let old_id = std::mem::replace(&mut frozen.rows[index].id, current_row.id.clone());
        frozen_ids.remove(&old_id);
        frozen_ids.insert(current_row.id.clone());
        if frozen.visible.remove(&old_id) {
            frozen.visible.insert(current_row.id.clone());
        }
    }
}

fn admit_new_items(current: &SidebarSnapshot, frozen: &mut FrozenOrder) {
    // Held and producer order can disagree mid-hold; never walk the cursor
    // backward past a known producer predecessor.
    let mut cursor = 0;
    for group in &current.worktree_groups {
        if let Some(position) = frozen.groups.iter().position(|key| key == &group.key) {
            cursor = cursor.max(position + 1);
        } else {
            frozen.groups.insert(cursor, group.key.clone());
            cursor += 1;
        }
    }

    cursor = 0;
    for row in current.worktree_groups.iter().flat_map(|group| &group.rows) {
        if let Some(position) = frozen.rows.iter().position(|frozen| frozen.id == row.id) {
            cursor = cursor.max(position + 1);
        } else {
            frozen.rows.insert(
                cursor,
                FrozenRow {
                    id: row.id.clone(),
                    pane: row.pane.as_ref().map(|pane| pane.pane_id.to_string()),
                },
            );
            cursor += 1;
        }
    }
}

#[cfg(test)]
mod tests;
