use super::*;

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

    // The structural chrome around the block stays inert, and the worktree
    // header jumps into its first row: the borderless title line at screen
    // row 0 is not clickable, the worktree header routes to its first row,
    // the process row beneath it follows, and the section gap just above the
    // header is inert.
    let header = ui.line_map.iter().position(|m| *m == Some(0)).unwrap();
    let row_below_header = header + 1;
    let process_row = ui.line_map.iter().position(|m| *m == Some(1)).unwrap();
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
        row_index_at_screen_position(&ui, screen_row_for(row_below_header)),
        Some(0),
        "the agent card line below the header still routes to row 0"
    );
    assert_eq!(
        row_index_at_screen_position(&ui, screen_row_for(process_row)),
        Some(1)
    );
    assert_eq!(
        row_index_at_screen_position(&ui, screen_row_for(header - 1)),
        None,
        "the section gap is not a row"
    );
}
#[test]
fn focus_keys_fire_without_mutating_selection() {
    // Every jump — a mouse click, a digit ordinal, Enter, or the Space triage
    // key — fires the focus command and mutates no selection state.
    let ws = workspace();
    let target = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
        ],
    );

    // A mouse click on terminal_2's row jumps without moving the selection.
    let mut ui = UiState {
        selected_index: 0,
        help_visible: false,
        animation_phase: 0,
        line_map: line_map_for(&snapshot, 0),
        ..Default::default()
    };
    let row1 = ui.line_map.iter().position(|m| *m == Some(1)).unwrap();
    let outcome = handle_mouse_click(1, screen_row_for(row1), &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::focus(target.clone()));
    assert!(!outcome.redraw, "a jump changes nothing to repaint");
    assert_eq!(ui.selected_index, 0, "the click moves no selection");
    assert_eq!(ui.selected_pane, None);
    assert_eq!(ui.browse, None);

    // A digit ordinal jumps to the second pane without moving the selection.
    let mut ui = UiState::default();
    let outcome = handle_key(KeyAction::Digit(2), &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::focus(target.clone()));
    assert_eq!(ui.selected_index, 0, "the digit moves no selection");
    assert_eq!(ui.selected_pane, None);

    // An out-of-range ordinal resolves no pane and does nothing.
    let outcome = handle_key(KeyAction::Digit(9), &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::default());

    // Enter focuses the selected pane and reads, never writes, the selection.
    let mut ui = UiState {
        selected_index: 1,
        selected_pane: Some(target.clone()),
        help_visible: false,
        animation_phase: 0,
        line_map: Vec::new(),
        ..Default::default()
    };
    let outcome = handle_key(KeyAction::Enter, &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::focus(target.clone()));
    assert_eq!(ui.selected_index, 1);
    assert_eq!(
        ui.selected_pane,
        Some(target.clone()),
        "Enter reads, never writes"
    );

    // With nothing selected there is no target and nothing happens.
    ui.selected_pane = None;
    let outcome = handle_key(KeyAction::Enter, &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::default());

    // The inbox keys triage to the next attention row, firing focus without
    // selecting. With one attention row, `n` (forward) and `N` (reverse) both
    // land on it.
    let mut snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", true),
            pane("terminal_2", "tab_0", false),
        ],
    );
    snapshot.worktree_groups[0].rows[1].name = "claude".to_owned();
    snapshot.worktree_groups[0].rows[1].card = crate::RowCard::Agent(Box::new(crate::AgentCard {
        status: Some(crate::agents::AgentStatus::Waiting),
        phase: crate::agents::TurnPhase::Idle,
        ..crate::AgentCard::default()
    }));
    let mut ui = UiState::default();
    let outcome = handle_key(KeyAction::InboxNext, &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::focus(target.clone()));
    assert_eq!(ui.selected_index, 0, "the triage key moves no selection");
    assert_eq!(ui.selected_pane, None);

    let outcome = handle_key(KeyAction::InboxPrev, &mut ui, &snapshot);
    assert_eq!(
        outcome,
        InputOutcome::focus(target),
        "N walks the inbox in reverse"
    );
    assert_eq!(
        ui.selected_index, 0,
        "the reverse triage key moves no selection"
    );
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
fn banner_click_scrolls_to_top_and_pins_without_focusing() {
    let ws = workspace();
    let target = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let snapshot = snapshot_with_panes(&ws, vec![pane("terminal_1", "tab_0", false)]);
    let mut ui = UiState {
        scroll_offset: 42,
        banner_line: Some(2),
        selected_pane: Some(target),
        ..Default::default()
    };

    let outcome = handle_mouse_click(0, 2, &mut ui, &snapshot);

    assert_eq!(outcome, InputOutcome::redraw());
    assert_eq!(ui.scroll_offset, 0);
    assert!(
        ui.manual_scroll.is_some(),
        "banner click pins the scroll-to-top position"
    );
    assert_eq!(outcome.focus, None);
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
fn help_key_opens_without_touching_the_viewport() {
    let ws = workspace();
    let snapshot = snapshot_with_panes(&ws, vec![pane("terminal_1", "tab_0", false)]);
    let mut ui = UiState {
        scroll_offset: 6,
        manual_scroll: Some(ManualScroll {
            selection_at_start: None,
        }),
        ..Default::default()
    };

    handle_key(KeyAction::Help, &mut ui, &snapshot);

    assert!(ui.help_visible);
    assert_eq!(ui.scroll_offset, 6);
    assert!(
        ui.manual_scroll.is_some(),
        "opening help no longer resets card scroll state"
    );
}
#[test]
fn top_and_bottom_keys_browse_to_the_ends() {
    let ws = workspace();
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
            pane("terminal_3", "tab_0", false),
        ],
    );
    let mut ui = UiState::default();

    // G jumps to the last visible row as a browse pick — selection only.
    let outcome = handle_key(KeyAction::Bottom, &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::redraw());
    assert_eq!(ui.selected_index, 2);
    assert_eq!(outcome.focus, None, "the end jump never focuses");
    assert!(ui.browse.is_some(), "G begins a browse pick");

    // G again at the bottom is a no-op.
    let outcome = handle_key(KeyAction::Bottom, &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::default());

    // g jumps back to the first row.
    let outcome = handle_key(KeyAction::Top, &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::redraw());
    assert_eq!(ui.selected_index, 0);
}
#[test]
fn mark_keys_name_the_selected_agent_row_without_focus() {
    // `m`/`M` name the selected agent row's id for the loop to mark read /
    // unread. They never focus, and they don't redraw here — the loop clears the
    // row and repaints once after the durable write, so the frame never flashes
    // the pre-clear state.
    let ws = workspace();
    let snapshot = filterable_snapshot(&ws);
    // Index 0 is the running `claude` agent row (the inbox participant).
    let mut ui = UiState {
        selected_index: 0,
        ..Default::default()
    };
    let row_id = snapshot.worktree_groups[0].rows[0].id.clone();

    let outcome = handle_key(KeyAction::MarkRead, &mut ui, &snapshot);
    assert_eq!(outcome.mark_read, Some(row_id.clone()));
    assert_eq!(outcome.focus, None);
    assert!(!outcome.redraw, "the loop owns the repaint after the write");
    assert_eq!(ui.selected_index, 0, "marking read moves no selection");

    let outcome = handle_key(KeyAction::MarkUnread, &mut ui, &snapshot);
    assert_eq!(outcome.mark_unread, Some(row_id));
    assert_eq!(outcome.focus, None);
}

#[test]
fn mark_keys_ignore_process_rows() {
    // A process row has no status and can never be unread, so `m`/`M` on one are
    // no-ops — never naming the row. This guards the downstream `opened_unread`
    // invariant: were the id passed through, the unread write would panic on a
    // statusless row. (Regression: R1-01.)
    let ws = workspace();
    let snapshot = filterable_snapshot(&ws);
    // Index 1 is the `zsh` process row in the `main` group.
    assert!(
        snapshot.worktree_groups[0].rows[1].status().is_none(),
        "fixture index 1 is the statusless process row",
    );
    let mut ui = UiState {
        selected_index: 1,
        ..Default::default()
    };

    let outcome = handle_key(KeyAction::MarkRead, &mut ui, &snapshot);
    assert_eq!(outcome.mark_read, None, "no read mark on a process row");
    assert_eq!(outcome, InputOutcome::default());

    let outcome = handle_key(KeyAction::MarkUnread, &mut ui, &snapshot);
    assert_eq!(outcome.mark_unread, None, "no unread mark on a process row");
    assert_eq!(outcome, InputOutcome::default());
}
#[test]
fn the_unread_snap_overrides_selection_follow_to_the_top() {
    use crate::agents::AgentStatus;
    // A tall room: a top-ranked unread waiting lead, then nine calm rows.
    let ws = workspace();
    let mut snapshot = snapshot(&ws);
    let mut lead = filter_row(
        true,
        "agent-lead",
        "claude",
        Some(AgentStatus::Waiting),
        "terminal_1",
        "/repo/main",
    );
    lead.unread = true;
    let mut rows = vec![lead];
    for n in 2..=10 {
        rows.push(filter_row(
            true,
            &format!("agent-{n}"),
            "claude",
            Some(AgentStatus::Running),
            &format!("terminal_{n}"),
            "/repo/main",
        ));
    }
    snapshot.worktree_groups = vec![crate::SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        kind: crate::SidebarWorktreeKind::Worktree,
        status_counts: Vec::new(),
        rows,
        hidden_count: 0,
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        clean: None,
        landed: None,
        trunk_sync: None,
        pr_state: None,
    }];

    // Selecting the last row scrolls the short viewport down, off the lead.
    let mut ui = UiState {
        selected_index: 9,
        ..Default::default()
    };
    let theme = ui.theme(&snapshot.theme);
    let following =
        render::compose_lines(&snapshot, None, &ui, theme.as_ref(), 40, 15).scroll_offset;
    assert!(
        following > 0,
        "following the bottom selection scrolls down off the top-ranked unread",
    );

    // Arming the snap returns the viewport to the top to reveal the lead, even
    // though the selection still sits at the bottom — unread outranks selection.
    ui.unread_focus = Some("agent-lead".to_owned());
    let snapped = render::compose_lines(&snapshot, None, &ui, theme.as_ref(), 40, 15).scroll_offset;
    assert_eq!(
        snapped, 0,
        "the unread snap outranks selection-follow and reaches the top",
    );
}
