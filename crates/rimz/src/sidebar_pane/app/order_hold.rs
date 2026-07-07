//! Renderer-local row/group order hold.
//!
//! Ranking remains producer truth. This module only reapplies the last painted
//! order over a fresh, already-ranked snapshot for a short interaction window,
//! so read state and attention signals update without moving the cards under
//! the user's eyes.

use std::collections::{HashMap, HashSet};

use crate::SidebarSnapshot;
use crate::sidebar::timing::REORDER_HOLD;
use crate::sidebar_pane::render::{FrozenOrder, OrderHold, UiState, group_visible_rows};

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
    if let Some(hold) = ui.order_hold.as_ref() {
        reorder_to_frozen(current, &hold.frozen);
        super::selection::anchor_selection(ui, current);
    }
}

pub(super) fn capture_order(current: &SidebarSnapshot, ui: &UiState) -> FrozenOrder {
    let held = ui.held_visible();
    let visible: HashSet<String> = current
        .worktree_groups
        .iter()
        .flat_map(|group| {
            group_visible_rows(
                group,
                ui.make_up_filter,
                ui.expanded_groups.contains(&group.key),
                held,
            )
        })
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
            .flat_map(|group| group.rows.iter().map(|row| row.id.clone()))
            .collect(),
        visible,
    }
}

pub(super) fn adopt_shared_hold(
    ui: &mut UiState,
    current: &mut SidebarSnapshot,
    order: FrozenOrder,
    stamp_ms: i64,
) {
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
        .map(|(index, id)| (id.as_str(), index))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidebar_pane::app::fixtures::{pane, snapshot, workspace};
    use crate::{
        ProcessCard, RowCard, SidebarRow, SidebarStatusCount, SidebarWorktreeGroup,
        SidebarWorktreeKind,
    };

    fn row(id: &str, raw_pane: &str) -> SidebarRow {
        SidebarRow {
            id: id.to_owned(),
            name: id.to_owned(),
            pane: Some(pane(raw_pane, "tab_0", false)),
            worktree_path: Some("/repo/main".to_owned()),
            worktree_branch: Some("main".to_owned()),
            channel: None,
            unread: false,
            inactive: false,
            archived: false,
            attention_score: 0,
            last_activity: jiff::Timestamp::from_second(1).expect("fixed timestamp"),
            card: RowCard::Process(ProcessCard::default()),
        }
    }

    fn group(key: &str, rows: Vec<SidebarRow>) -> SidebarWorktreeGroup {
        SidebarWorktreeGroup {
            key: key.to_owned(),
            label: key.to_owned(),
            kind: SidebarWorktreeKind::Worktree,
            status_counts: Vec::<SidebarStatusCount>::new(),
            rows,
            diff_added: None,
            diff_removed: None,
            commits_ahead: None,
            commits_behind: None,
            trunk: None,
            worktree_backed: false,
            clean: None,
            landed: None,
            trunk_sync: None,
            pr_state: None,
        }
    }

    fn snapshot_with_groups(groups: Vec<SidebarWorktreeGroup>) -> SidebarSnapshot {
        let ws = workspace();
        let mut current = snapshot(&ws);
        current.worktree_groups = groups;
        current
    }

    fn group_keys(current: &SidebarSnapshot) -> Vec<&str> {
        current
            .worktree_groups
            .iter()
            .map(|group| group.key.as_str())
            .collect()
    }

    fn row_ids<'a>(current: &'a SidebarSnapshot, group_key: &str) -> Vec<&'a str> {
        current
            .worktree_groups
            .iter()
            .find(|group| group.key == group_key)
            .expect("group exists")
            .rows
            .iter()
            .map(|row| row.id.as_str())
            .collect()
    }

    #[test]
    fn reorder_to_frozen_reproduces_order_and_appends_unknowns_stably() {
        let mut current = snapshot_with_groups(vec![
            group("new", vec![row("x", "terminal_9")]),
            group(
                "b",
                vec![
                    row("b2", "terminal_4"),
                    row("b1", "terminal_3"),
                    row("b-new", "terminal_5"),
                ],
            ),
            group("a", vec![row("a2", "terminal_2"), row("a1", "terminal_1")]),
        ]);
        let frozen = FrozenOrder {
            groups: vec!["a".to_owned(), "b".to_owned()],
            rows: vec![
                "a1".to_owned(),
                "a2".to_owned(),
                "b1".to_owned(),
                "b2".to_owned(),
            ],
            visible: HashSet::new(),
        };

        reorder_to_frozen(&mut current, &frozen);

        assert_eq!(group_keys(&current), vec!["a", "b", "new"]);
        assert_eq!(row_ids(&current, "a"), vec!["a1", "a2"]);
        assert_eq!(row_ids(&current, "b"), vec!["b1", "b2", "b-new"]);
        assert_eq!(row_ids(&current, "new"), vec!["x"]);
    }

    #[test]
    fn capture_order_collects_group_keys_flat_row_ids_and_visible_ids() {
        let current = snapshot_with_groups(vec![group(
            "a",
            (0..8)
                .map(|index| row(&format!("a{index}"), &format!("terminal_{index}")))
                .collect(),
        )]);
        let ui = UiState::default();

        let order = capture_order(&current, &ui);

        assert_eq!(order.groups, vec!["a"]);
        assert_eq!(order.rows, ["a0", "a1", "a2", "a3", "a4", "a5", "a6", "a7"]);
        assert_eq!(
            order.visible,
            HashSet::from([
                "a0".to_owned(),
                "a1".to_owned(),
                "a2".to_owned(),
                "a3".to_owned(),
                "a4".to_owned(),
                "a5".to_owned(),
            ])
        );
    }

    #[test]
    fn apply_order_hold_arms_holds_and_expires() {
        let mut ui = UiState {
            last_order: FrozenOrder {
                groups: vec!["a".to_owned()],
                rows: vec!["a2".to_owned(), "a1".to_owned()],
                visible: HashSet::from(["a2".to_owned(), "a1".to_owned()]),
            },
            ..UiState::default()
        };
        let mut current = snapshot_with_groups(vec![group(
            "a",
            vec![row("a1", "terminal_1"), row("a2", "terminal_2")],
        )]);
        let now_ms = 1_000;

        apply_order_hold(&mut ui, &mut current, true, now_ms);

        let expires_ms = now_ms + REORDER_HOLD.as_millis() as i64;
        assert_eq!(
            ui.order_hold.as_ref().map(|hold| hold.expires_ms),
            Some(expires_ms)
        );
        assert_eq!(row_ids(&current, "a"), vec!["a2", "a1"]);

        current.worktree_groups[0].rows.reverse();
        apply_order_hold(&mut ui, &mut current, false, expires_ms - 1);
        assert_eq!(row_ids(&current, "a"), vec!["a2", "a1"]);

        current.worktree_groups[0].rows.reverse();
        apply_order_hold(&mut ui, &mut current, false, expires_ms);
        assert!(ui.order_hold.is_none());
        assert_eq!(row_ids(&current, "a"), vec!["a1", "a2"]);
    }

    #[test]
    fn adopt_shared_hold_installs_reorders_and_recaptures() {
        let mut ui = UiState::default();
        let mut current = snapshot_with_groups(vec![group(
            "a",
            vec![row("a1", "terminal_1"), row("a2", "terminal_2")],
        )]);
        let order = FrozenOrder {
            groups: vec!["a".to_owned()],
            rows: vec!["a2".to_owned(), "a1".to_owned()],
            visible: HashSet::from(["a2".to_owned()]),
        };
        let stamp_ms = 2_000;

        adopt_shared_hold(&mut ui, &mut current, order.clone(), stamp_ms);

        assert_eq!(row_ids(&current, "a"), vec!["a2", "a1"]);
        assert_eq!(
            ui.order_hold.as_ref().map(|hold| hold.expires_ms),
            Some(stamp_ms + REORDER_HOLD.as_millis() as i64)
        );
        assert_eq!(
            ui.order_hold.as_ref().map(|hold| &hold.frozen),
            Some(&order)
        );
        assert_eq!(ui.last_order.rows, vec!["a2", "a1"]);
        assert_eq!(
            ui.last_order.visible,
            HashSet::from(["a2".to_owned(), "a1".to_owned()])
        );
    }
}
