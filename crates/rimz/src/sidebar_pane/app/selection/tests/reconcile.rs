use super::*;

#[test]
fn cold_start_derives_from_focused_pane_or_holds_unseated() {
    let ws = workspace();
    let active = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let active_snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", true),
        ],
    );
    let mut ui = UiState::default();

    reconcile_selection(&mut ui, &active_snapshot, Some(active.clone()));

    assert_eq!(ui.selected_index, 1);
    assert_eq!(ui.selected_pane, Some(active.clone()));
    assert_eq!(ui.baseline_pane, Some(active));

    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState::default();

    reconcile_selection(&mut ui, &snapshot, None);

    assert_eq!(ui.selected_pane, None);
    assert_eq!(ui.selected_index, 0);
}
#[test]
fn baseline_changes_move_highlight_while_none_derivations_hold_it() {
    let ws = workspace();
    let was = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let now_active = PaneId::from_parts(MuxName::Zellij, "terminal_3");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
            pane("terminal_3", "tab_0", true),
        ],
    );
    let mut ui = UiState {
        selected_index: 0,
        selected_pane: Some(was.clone()),
        baseline_pane: Some(was),
        ..Default::default()
    };

    reconcile_selection(&mut ui, &snapshot, Some(now_active.clone()));

    assert_eq!(ui.selected_index, 2);
    assert_eq!(ui.selected_pane, Some(now_active.clone()));
    assert_eq!(ui.baseline_pane, Some(now_active));

    let held = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState {
        selected_index: 0,
        selected_pane: Some(held.clone()),
        baseline_pane: Some(held.clone()),
        ..Default::default()
    };

    reconcile_selection(&mut ui, &snapshot, None);

    assert_eq!(ui.selected_pane, Some(held.clone()));
    assert_eq!(ui.baseline_pane, Some(held));
}
#[test]
fn selection_reanchors_to_its_pane_after_a_reorder() {
    // terminal_2 moved from row 1 to row 0 between folds with no baseline
    // change; the highlight follows its pane, not the old index.
    let ws = workspace();
    let active = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_2", "tab_0", true),
            pane("terminal_1", "tab_0", false),
        ],
    );
    let mut ui = UiState {
        selected_index: 1,
        selected_pane: Some(active.clone()),
        baseline_pane: Some(active.clone()),
        ..Default::default()
    };

    reconcile_selection(&mut ui, &snapshot, Some(active.clone()));

    assert_eq!(ui.selected_index, 0, "re-anchored to the pane's new row");
    assert_eq!(ui.selected_pane, Some(active));
}
#[test]
fn selection_drops_when_its_pane_leaves_the_room() {
    // The baseline's pane is gone from the snapshot: drop the dangling
    // identity and clamp, so the next derivation can re-seat it.
    let ws = workspace();
    let gone = PaneId::from_parts(MuxName::Zellij, "terminal_9");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState {
        selected_index: 1,
        selected_pane: Some(gone.clone()),
        baseline_pane: Some(gone),
        ..Default::default()
    };

    reconcile_selection(&mut ui, &snapshot, None);

    assert_eq!(ui.selected_pane, None, "dangling identity dropped");
    assert_eq!(ui.baseline_pane, None, "absent baseline cleared");
    assert!(ui.selected_index < 2, "clamped to a valid row");
}

/// `filterable_snapshot` with the failed codex row (`agent-2`) marked unread, so
/// it is the actionable unread lead the snap focuses.
fn unread_lead_snapshot(ws: &WorkspaceId) -> SidebarSnapshot {
    let mut snapshot = filterable_snapshot(ws);
    snapshot.worktree_groups[1].rows[0].unread = true;
    snapshot
}

#[test]
fn fresh_actionable_unread_arms_and_holds_the_snap() {
    let ws = workspace();
    let snapshot = unread_lead_snapshot(&ws);
    let mut ui = UiState::default();

    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(
        ui.unread_focus.as_deref(),
        Some("agent-2"),
        "a fresh actionable unread arms the snap on the lead row",
    );
    assert_eq!(ui.last_lead_unread.as_deref(), Some("agent-2"));

    // It holds across a fold that keeps the lead — selection-follow never reclaims
    // the viewport while the unread stands.
    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(ui.unread_focus.as_deref(), Some("agent-2"));
}

#[test]
fn manual_scroll_pin_suppresses_the_snap_but_records_the_lead() {
    let ws = workspace();
    let snapshot = unread_lead_snapshot(&ws);
    let mut ui = UiState::default();
    handle_scroll(true, &mut ui); // pin the viewport before the unread arrives

    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(ui.unread_focus, None, "a manual scroll pin is respected");
    assert_eq!(
        ui.last_lead_unread.as_deref(),
        Some("agent-2"),
        "the lead is still recorded, so releasing the pin never retro-snaps it",
    );
}

#[test]
fn a_dismissed_lead_is_not_re_snapped_while_it_lingers() {
    let ws = workspace();
    let snapshot = unread_lead_snapshot(&ws);
    let mut ui = UiState::default();
    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(ui.unread_focus.as_deref(), Some("agent-2"));

    // A keystroke is engagement: it dismisses the snap.
    handle_key(KeyAction::Down, &mut ui, &snapshot);
    assert_eq!(ui.unread_focus, None);

    // The same still-unanswered lead does not re-arm on the next fold.
    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(ui.unread_focus, None, "a dismissed lead stays dismissed");
}

#[test]
fn answering_the_lead_clears_the_snap() {
    let ws = workspace();
    let mut snapshot = unread_lead_snapshot(&ws);
    let mut ui = UiState::default();
    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(ui.unread_focus.as_deref(), Some("agent-2"));

    // The row is read/answered: it is no longer the actionable lead.
    snapshot.worktree_groups[1].rows[0].unread = false;
    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(ui.unread_focus, None, "an answered lead ends the snap");
    assert_eq!(ui.last_lead_unread, None);
}
