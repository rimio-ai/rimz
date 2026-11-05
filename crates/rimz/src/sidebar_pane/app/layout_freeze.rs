//! View-owned timed freeze following input or a focus hold's release.
//!
//! The view module owns focused cards' held rank keys. This module reapplies
//! the last painted order for the settling window. Arrivals enter before their
//! first frozen successor in their group's live order; new groups follow live
//! groups. Content keeps updating.

use std::collections::{HashMap, HashSet};

use crate::ids::PaneId;
use crate::sidebar_pane::render::{FrozenOrder, FrozenRow, LayoutFreeze, ViewState};
use crate::store::snapshot::{SidebarSnapshot, SidebarWorktreeGroup, SidebarWorktreeKind};

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

pub(super) fn defer_inactive_admission(
    current: &mut SidebarSnapshot,
    previous: &SidebarSnapshot,
    selected: Option<&PaneId>,
    focused: &[PaneId],
    held: bool,
) -> bool {
    let viewed: HashSet<_> = selected
        .into_iter()
        .chain(focused)
        .filter_map(|pane| current.row_owning_pane(pane))
        .map(|row| row.id.clone())
        .collect();
    let defer = |group: &SidebarWorktreeGroup| {
        group.inactive.is_some()
            && !group.rows.is_empty()
            && previous
                .worktree_groups
                .iter()
                .any(|old| old.key == group.key && old.inactive.is_none())
            && (held || group.rows.iter().any(|row| viewed.contains(&row.id)))
    };
    if !current.worktree_groups.iter().any(defer) {
        return false;
    }
    for group in std::sync::Arc::make_mut(&mut current.worktree_groups)
        .iter_mut()
        .filter(|group| defer(group))
    {
        group.inactive = None;
    }
    true
}

/// Fallback for an external or recovered focus anchor without a live source view.
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

pub(super) fn reorder_to_frozen(current: &mut SidebarSnapshot, frozen: &FrozenOrder) {
    let group_pos: HashMap<&str, usize> = frozen
        .groups
        .iter()
        .enumerate()
        .map(|(index, key)| (key.as_str(), index))
        .collect();
    let group_key = |group: &SidebarWorktreeGroup| {
        (
            group.kind == SidebarWorktreeKind::External,
            group.inactive.is_some(),
            group_pos
                .get(group.key.as_str())
                .copied()
                .unwrap_or(usize::MAX),
        )
    };
    if !current.worktree_groups.iter().is_sorted_by_key(group_key) {
        std::sync::Arc::make_mut(&mut current.worktree_groups).sort_by_key(group_key);
    }
    current.sort_rows_in_order(frozen.rows.iter().map(|row| row.id.as_str()));
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
        for (index, row) in group.rows.iter().enumerate() {
            if frozen.rows.iter().any(|frozen| frozen.id == row.id) {
                continue;
            }
            let position = group.rows[index + 1..]
                .iter()
                .find_map(|next| frozen.rows.iter().position(|frozen| frozen.id == next.id))
                .unwrap_or(frozen.rows.len());
            frozen.rows.insert(
                position,
                FrozenRow {
                    id: row.id.clone(),
                    pane: row.pane.as_ref().map(|pane| pane.pane_id.to_string()),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests;
