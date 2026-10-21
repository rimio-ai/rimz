//! View-owned layout freeze.
//!
//! Ranking remains producer truth. This module reapplies the last painted order
//! over a fresh, already-ranked snapshot for a short interaction window and
//! appends arrivals within their groups, with new groups after live groups, so
//! read state and attention signals update without moving the cards under the
//! user's eyes.

use std::collections::{HashMap, HashSet};

use crate::agents::AgentStatus;
use crate::ids::PaneId;
use crate::sidebar_pane::render::{FrozenOrder, FrozenRow, LayoutFreeze, ViewState};
use crate::store::snapshot::{
    SidebarRow, SidebarSnapshot, SidebarWorktreeGroup, SidebarWorktreeKind,
};

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

pub(super) fn arm_freeze(ui: &mut ViewState, now_ms: i64) {
    ui.freeze = Some(LayoutFreeze {
        frozen: ui.last_order.clone(),
        armed_ms: now_ms,
    });
}

pub(super) fn apply_freeze(
    ui: &mut ViewState,
    current: &mut SidebarSnapshot,
    interacted: bool,
    now_ms: i64,
    parked: bool,
) {
    if interacted {
        arm_freeze(ui, now_ms);
    } else if !ui.arrival_pending
        && ui
            .freeze
            .as_ref()
            .is_some_and(|hold| now_ms >= hold.deadline_ms(parked))
    {
        ui.freeze = None;
    }
    if let Some(hold) = ui.freeze.as_mut() {
        migrate_frozen_order(current, &mut hold.frozen);
        admit_new_items(current, &mut hold.frozen);
        reorder_to_frozen(current, &hold.frozen);
    }
    if ui.freeze.is_some() {
        super::selection::anchor_selection(ui, current);
    }
}

pub(super) fn capture_order(current: &SidebarSnapshot, ui: &ViewState) -> FrozenOrder {
    let roster = ui.visible_roster(current);
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

pub(super) fn adopt_shared_order(
    ui: &mut ViewState,
    current: &mut SidebarSnapshot,
    mut order: FrozenOrder,
    armed_ms: Option<i64>,
) {
    migrate_frozen_order(current, &mut order);
    admit_new_items(current, &mut order);
    reorder_to_frozen(current, &order);
    ui.freeze = armed_ms.map(|armed_ms| LayoutFreeze {
        frozen: order,
        armed_ms,
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

    let group_key = |group: &SidebarWorktreeGroup| {
        (
            group.kind == SidebarWorktreeKind::External,
            group.finished,
            group_pos
                .get(group.key.as_str())
                .copied()
                .unwrap_or(usize::MAX),
        )
    };
    if !current.worktree_groups.iter().is_sorted_by_key(group_key) {
        std::sync::Arc::make_mut(&mut current.worktree_groups).sort_by_key(group_key);
    }
    let row_key = |row: &SidebarRow| {
        (
            row.is_process(),
            row_pos.get(row.id.as_str()).copied().unwrap_or(usize::MAX),
        )
    };
    for index in 0..current.worktree_groups.len() {
        if current.worktree_groups[index]
            .rows
            .iter()
            .is_sorted_by_key(row_key)
        {
            continue;
        }
        let group = &mut std::sync::Arc::make_mut(&mut current.worktree_groups)[index];
        std::sync::Arc::make_mut(&mut group.rows).sort_by_key(row_key);
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
    for group in current.worktree_groups.iter() {
        if !frozen.groups.contains(&group.key) {
            frozen.groups.push(group.key.clone());
        }
    }

    for row in current
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
    {
        if !frozen.rows.iter().any(|frozen| frozen.id == row.id) {
            frozen.rows.push(FrozenRow {
                id: row.id.clone(),
                pane: row.pane.as_ref().map(|pane| pane.pane_id.to_string()),
            });
        }
    }
}

#[cfg(test)]
mod tests;
