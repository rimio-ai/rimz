use super::*;

#[test]
fn row_index_maps_process_row_screen_positions() {
    let ws = workspace();
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let ui = UiState {
        line_map: line_map_for(&snapshot, 0),
        ..UiState::default()
    };

    // The worktree header is the first line that routes to row 0 — clicking
    // the pod name jumps into its first row — and the first process row
    // follows directly beneath it. Both route to row 0.
    let header = ui.line_map.iter().position(|m| *m == Some(0)).unwrap();
    let row0 = header + 1;
    let row1 = ui.line_map.iter().position(|m| *m == Some(1)).unwrap();
    assert_eq!(
        ui.line_map[row0],
        Some(0),
        "the first process row follows its worktree header"
    );

    // The borderless title line at screen row 0 is inert chrome.
    assert_eq!(
        row_index_at_screen_position(&ui, 0),
        None,
        "the title line is not clickable content"
    );
    assert_eq!(
        row_index_at_screen_position(&ui, screen_row_for(header)),
        Some(0),
        "the worktree header jumps into its first row"
    );
    assert_eq!(
        row_index_at_screen_position(&ui, screen_row_for(row0)),
        Some(0)
    );
    assert_eq!(
        row_index_at_screen_position(&ui, screen_row_for(row1)),
        Some(1)
    );
    // The line just above the worktree header is the section gap — inert.
    assert_eq!(
        row_index_at_screen_position(&ui, screen_row_for(header - 1)),
        None,
        "the section gap is not a row"
    );
}
#[test]
fn every_line_of_an_agent_block_routes_to_that_agent() {
    // The user-visible contract: the whole multi-line agent card is one
    // click target, the worktree header that jumps into it routes there too,
    // the gaps and `+K more` are inert, and a process row's single line
    // routes to its own index.
    let ws = workspace();
    let snapshot = clickable_block_snapshot(&ws);
    // Select the agent so its deeper stats lines appear too.
    let map = line_map_for(&snapshot, 0);

    // Index 0 is the agent (a multi-line card) plus the worktree header that
    // jumps into it; index 1 is the process row.
    let agent_lines = map.iter().filter(|m| **m == Some(0)).count();
    assert!(
        agent_lines >= 4,
        "the worktree header plus the selected agent card (identity + \
             description + gauge + stats) route to row 0, not {agent_lines} lines",
    );
    let process_lines = map.iter().filter(|m| **m == Some(1)).count();
    assert_eq!(process_lines, 1, "a process row is a single line");

    // No content line of the agent block is missed: every map slot routes
    // through the hit-test to exactly the row it was tagged with.
    let ui = UiState {
        line_map: map.clone(),
        ..UiState::default()
    };
    for (i, entry) in map.iter().enumerate() {
        let got = row_index_at_screen_position(&ui, screen_row_for(i));
        assert_eq!(got, *entry, "screen row {i} mismatched its map slot");
    }

    // The cockpit header, gaps, and the `+K more` hidden-count line are inert.
    assert!(
        map.contains(&None),
        "cockpit header / gaps / +K more stay inert"
    );
}
#[test]
fn mouse_click_fires_focus_without_moving_selection() {
    let ws = workspace();
    let target = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState {
        selected_index: 0,
        help_visible: false,
        animation_phase: 0,
        line_map: line_map_for(&snapshot, 0),
        ..Default::default()
    };
    let row1 = ui.line_map.iter().position(|m| *m == Some(1)).unwrap();

    let outcome = handle_mouse_click(1, screen_row_for(row1), &mut ui, &snapshot);

    assert_eq!(outcome, InputOutcome::focus(target));
    assert!(!outcome.redraw, "a jump changes nothing to repaint");
    assert_eq!(ui.selected_index, 0, "the click moves no selection");
    assert_eq!(ui.selected_pane, None);
    assert_eq!(ui.browse, None);
}
#[test]
fn digit_fires_focus_at_the_ordinal_row_without_selecting() {
    let ws = workspace();
    let target = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState::default();

    let outcome = handle_key(KeyAction::Digit(2), &mut ui, &snapshot);

    assert_eq!(outcome, InputOutcome::focus(target));
    assert_eq!(ui.selected_index, 0, "the digit moves no selection");
    assert_eq!(ui.selected_pane, None);

    // An out-of-range ordinal resolves no pane and does nothing.
    let outcome = handle_key(KeyAction::Digit(9), &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::default());
}
#[test]
fn space_fires_focus_at_the_next_attention_row_without_selecting() {
    let ws = workspace();
    let target = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let mut snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", true),
            pane("terminal_2", "tab_0", false),
        ],
    );
    snapshot.worktree_groups[0].rows[1].name = "claude".to_owned();
    snapshot.worktree_groups[0].rows[1].card = crate::RowCard::Agent(Box::new(crate::AgentCard {
        status: Some(crate::feed::AgentStatus::Waiting),
        phase: crate::agents::TurnPhase::Idle,
        ..crate::AgentCard::default()
    }));
    let mut ui = UiState::default();

    let outcome = handle_key(KeyAction::Space, &mut ui, &snapshot);

    assert_eq!(outcome, InputOutcome::focus(target));
    assert_eq!(ui.selected_index, 0, "the triage key moves no selection");
    assert_eq!(ui.selected_pane, None);
}
#[test]
fn arrow_key_reports_immediate_ui_change() {
    let ws = workspace();
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState {
        selected_index: 0,
        help_visible: false,
        animation_phase: 0,
        line_map: Vec::new(),
        ..Default::default()
    };

    let outcome = handle_key(KeyAction::Down, &mut ui, &snapshot);

    assert_eq!(outcome, InputOutcome::redraw());
    assert_eq!(ui.selected_index, 1);
    assert!(ui.browse.is_some(), "an arrow begins a browse pick");
}
#[test]
fn worktree_keys_browse_to_neighboring_worktree_heads() {
    let ws = workspace();
    let snapshot = filterable_snapshot(&ws);
    let feature = PaneId::from_parts(MuxName::Zellij, "terminal_3");
    let main = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let mut ui = UiState {
        selected_index: 0,
        selected_pane: Some(main.clone()),
        baseline_pane: Some(main.clone()),
        ..Default::default()
    };

    let outcome = handle_key(KeyAction::WorktreeDown, &mut ui, &snapshot);

    assert_eq!(outcome, InputOutcome::redraw());
    assert_eq!(ui.selected_index, 2);
    assert_eq!(ui.selected_pane, Some(feature));
    assert!(ui.browse.is_some(), "a worktree jump is a browse pick");

    let outcome = handle_key(KeyAction::WorktreeDown, &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::default(), "no wrap at the end");
    assert_eq!(ui.selected_index, 2);

    let outcome = handle_key(KeyAction::WorktreeUp, &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::redraw());
    assert_eq!(ui.selected_index, 0);
    assert_eq!(ui.selected_pane, Some(main));
}
#[test]
fn dismiss_key_requests_alert_dismissal() {
    let ws = workspace();
    let snapshot = snapshot_with_panes(&ws, vec![pane("terminal_1", "tab_0", false)]);
    let mut ui = UiState::default();

    let outcome = handle_key(KeyAction::Dismiss, &mut ui, &snapshot);

    assert_eq!(outcome, InputOutcome::dismiss());
    assert!(outcome.dismiss);
    assert!(outcome.redraw);
    // Dismiss never moves the selection.
    assert_eq!(ui.selected_index, 0);
}
#[test]
fn enter_fires_focus_at_the_selected_pane_without_mutating_ui() {
    let ws = workspace();
    let selected = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState {
        selected_index: 1,
        selected_pane: Some(selected.clone()),
        help_visible: false,
        animation_phase: 0,
        line_map: Vec::new(),
        ..Default::default()
    };

    let outcome = handle_key(KeyAction::Enter, &mut ui, &snapshot);

    assert_eq!(outcome, InputOutcome::focus(selected.clone()));
    assert_eq!(ui.selected_index, 1);
    assert_eq!(
        ui.selected_pane,
        Some(selected),
        "Enter reads, never writes"
    );

    // With nothing selected there is no target and nothing happens.
    ui.selected_pane = None;
    let outcome = handle_key(KeyAction::Enter, &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::default());
}
#[test]
fn wheel_scroll_pins_the_viewport_and_steps_the_offset() {
    // The wheel moves the window, never the selection: each tick steps the
    // offset and the first tick pins a ManualScroll anchored on the selection
    // it began over.
    let mut ui = UiState::default();

    let outcome = handle_scroll(true, &mut ui);

    assert_eq!(outcome, InputOutcome::redraw());
    assert_eq!(ui.scroll_offset, SCROLL_STEP);
    assert!(ui.manual_scroll.is_some());
    assert_eq!(ui.selected_index, 0, "the wheel never moves the selection");

    handle_scroll(true, &mut ui);
    assert_eq!(ui.scroll_offset, 2 * SCROLL_STEP);

    // Scrolling back above the top clamps at zero rather than wrapping.
    handle_scroll(false, &mut ui);
    handle_scroll(false, &mut ui);
    handle_scroll(false, &mut ui);
    assert_eq!(ui.scroll_offset, 0);
    assert!(
        ui.manual_scroll.is_some(),
        "the pin outlives the round trip"
    );
}
#[test]
fn selection_change_snaps_a_wheel_pin_back() {
    // The pin holds across folds that keep the selection, and ends the moment
    // the selection genuinely changes — the viewport snaps back to following
    // the selected card.
    let ws = workspace();
    let first = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let second = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState::default();
    reconcile_selection(&mut ui, &snapshot, Some(first.clone()));
    handle_scroll(true, &mut ui);

    // An unchanged selection keeps the peek.
    reconcile_selection(&mut ui, &snapshot, Some(first));
    assert!(ui.manual_scroll.is_some());

    // A genuine selection move ends it.
    reconcile_selection(&mut ui, &snapshot, Some(second));
    assert_eq!(ui.manual_scroll, None);
}
#[test]
fn arrow_browse_ends_a_wheel_pin() {
    // ↑/↓ are explicit picks: the wheel peek ends and the viewport resumes
    // following the selection.
    let ws = workspace();
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState::default();
    handle_scroll(true, &mut ui);
    assert!(ui.manual_scroll.is_some());

    handle_key(KeyAction::Down, &mut ui, &snapshot);

    assert_eq!(ui.manual_scroll, None);
}
#[test]
fn help_toggle_jumps_the_viewport_to_the_overlay() {
    // The overlay lives at the scroll zone's tail: opening help jumps the
    // viewport to the end (the draw clamps the sentinel to the zone's last
    // window) and the open overlay itself owns the viewport — no wheel pin
    // needed, the wheel may still roam. Closing drops any roaming peek so the
    // view snaps back to the selection.
    let ws = workspace();
    let snapshot = snapshot_with_panes(&ws, vec![pane("terminal_1", "tab_0", false)]);
    let mut ui = UiState::default();

    handle_key(KeyAction::Help, &mut ui, &snapshot);
    assert!(ui.help_visible);
    assert_eq!(ui.scroll_offset, usize::MAX);
    assert_eq!(ui.manual_scroll, None, "the overlay needs no wheel pin");

    handle_scroll(false, &mut ui);
    assert!(ui.manual_scroll.is_some(), "the wheel roams while reading");

    handle_key(KeyAction::Help, &mut ui, &snapshot);
    assert!(!ui.help_visible);
    assert_eq!(ui.manual_scroll, None);
}
#[test]
fn help_overlay_holds_the_viewport_through_selection_churn() {
    // While the overlay is open it owns the viewport: a fold that genuinely
    // moves the selection — an external focus move landing — never pulls the
    // view away mid-read. Closing the overlay resumes auto-follow and the
    // selected card scrolls back into view.
    let ws = workspace();
    let first = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let second = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let snapshot = snapshot_with_panes(
        &ws,
        (1..=6)
            .map(|n| pane(&format!("terminal_{n}"), "tab_0", false))
            .collect(),
    );
    let mut ui = UiState::default();
    reconcile_selection(&mut ui, &snapshot, Some(first));

    // Opening help lands the viewport on the overlay at the zone's tail.
    handle_key(KeyAction::Help, &mut ui, &snapshot);
    let offset = render::compose_lines(&snapshot, None, &ui, 38, 14).scroll_offset;
    assert!(offset > 0, "the overlay overflows the short frame");
    ui.scroll_offset = offset; // the draw's write-back

    // A genuine selection move beneath the open overlay holds the window.
    reconcile_selection(&mut ui, &snapshot, Some(second));
    let held = render::compose_lines(&snapshot, None, &ui, 38, 14).scroll_offset;
    assert_eq!(held, offset, "the open overlay owns the viewport");
    ui.scroll_offset = held;

    // Closing resumes auto-follow: the selected card scrolls back into view.
    handle_key(KeyAction::Help, &mut ui, &snapshot);
    let map = render::compose_lines(&snapshot, None, &ui, 38, 14).line_map;
    assert!(
        map.contains(&Some(ui.selected_index)),
        "the selection is back on screen"
    );
}
