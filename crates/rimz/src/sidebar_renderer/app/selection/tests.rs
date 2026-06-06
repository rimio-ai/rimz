use super::*;
use crate::sidebar_renderer::app::fixtures::{pane, snapshot, snapshot_with_panes, workspace};
use crate::sidebar_renderer::render;
use crate::{MuxName, WorkspaceId};
use jiff::Timestamp;

/// A browse pick of `pane`, begun while the derived baseline was `baseline`.
fn browse(pane: &PaneId, baseline: Option<&PaneId>) -> Browse {
    Browse {
        pane: pane.clone(),
        baseline_at_start: baseline.cloned(),
    }
}

/// A group whose first row is a multi-line agent card (model, effort, and
/// context% set so it carries identity + description + gauge, and selecting
/// it reveals its deeper budget-bar and stats lines), followed by a
/// single-line process row, with a non-zero hidden count so a `+K more` line
/// renders. The fixture for the whole-block clickability regression guard.
fn clickable_block_snapshot(ws: &WorkspaceId) -> SidebarSnapshot {
    let mut snapshot = snapshot(ws);
    let agent = crate::SidebarRow {
        row_kind: crate::SidebarRowKind::Agent,
        id: "agent-1".to_owned(),
        name: "claude".to_owned(),
        status: Some(crate::feed::AgentStatus::Running),
        phase: crate::agents::TurnPhase::Idle,
        pane: Some(pane("terminal_9", "tab_0", false)),
        request_id: None,
        surface: None,
        task: Some("inspect auth".to_owned()),
        prompt: None,
        model: Some("Opus".to_owned()),
        effort: Some("high".to_owned()),
        context_pct: Some(38),
        context_window: None,
        total_tokens: Some(12_400),
        cache_read_input_tokens: None,
        fresh_input_tokens: None,
        output_tokens: None,
        todo_done: Some(3),
        todo_total: Some(5),
        context: None,
        context_severity: None,
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        last_activity: Timestamp::now(),
        registered_at: None,
        resolver: None,
        options: Vec::new(),
        sub_agents: Vec::new(),
        process_active: false,
        command_detail: None,
        compacting: false,
        turn_error_label: None,
        rss_kb: None,
        cpu_pct: None,
        io_bps: None,
    };
    let process = crate::SidebarRow {
        row_kind: crate::SidebarRowKind::Process,
        id: "terminal_10".to_owned(),
        name: "zsh".to_owned(),
        status: None,
        phase: crate::agents::TurnPhase::Idle,
        pane: Some(pane("terminal_10", "tab_0", false)),
        request_id: None,
        surface: None,
        task: None,
        prompt: None,
        model: None,
        effort: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        cache_read_input_tokens: None,
        fresh_input_tokens: None,
        output_tokens: None,
        todo_done: None,
        todo_total: None,
        context: None,
        context_severity: None,
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        last_activity: Timestamp::now(),
        registered_at: None,
        resolver: None,
        options: Vec::new(),
        sub_agents: Vec::new(),
        process_active: false,
        command_detail: None,
        compacting: false,
        turn_error_label: None,
        rss_kb: None,
        cpu_pct: None,
        io_bps: None,
    };
    snapshot.worktree_groups = vec![crate::SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        kind: crate::SidebarWorktreeKind::Worktree,
        status_counts: vec![crate::SidebarStatusCount {
            status: crate::feed::AgentStatus::Running,
            count: 1,
        }],
        rows: vec![agent, process],
        hidden_count: 2,
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        clean: None,
    }];
    snapshot
}

#[test]
fn cold_start_derives_from_first_active_pane() {
    // No baseline and no local layer: the first frame's active-pane
    // derivation seeds both the baseline and the highlight.
    let ws = workspace();
    let active = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", true),
        ],
    );
    let mut ui = UiState::default();

    reconcile_selection(&mut ui, &snapshot, Some(active.clone()));

    assert_eq!(ui.selected_index, 1);
    assert_eq!(ui.selected_pane, Some(active.clone()));
    assert_eq!(ui.baseline_pane, Some(active));
}

#[test]
fn cold_start_with_no_derivation_holds_none() {
    // No baseline, no local layer, a None derivation: nothing to follow, so
    // the highlight stays unseated (index clamped to row 0) until a frame
    // derives an active row — never a fleet-row guess that may sit in
    // another tab.
    let ws = workspace();
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
fn baseline_change_moves_the_highlight() {
    // No local layer: the highlight follows the derived baseline, so a
    // genuine external move (the user focused terminal_3) lands on the very
    // next fold.
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
}

#[test]
fn none_derivation_holds_last_baseline() {
    // The sidebar itself is the view's active pane (the user focused it to
    // type), or the active pane is not a row: the derivation is None, the
    // baseline holds, and the highlight stays put.
    let ws = workspace();
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
fn highlight_moves_only_when_the_baseline_catches_up() {
    // The "accepts latency" contract behind the one-packet jump: a jump
    // action fires the focus command and mutates nothing, so a fold still
    // deriving the old pane keeps the old highlight, and the jumped pane
    // lights up only once the mux reports it focused.
    let ws = workspace();
    let from = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let jumped = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", true),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState {
        selected_index: 0,
        selected_pane: Some(from.clone()),
        baseline_pane: Some(from.clone()),
        line_map: line_map_for(&snapshot, 0),
        ..Default::default()
    };

    // Click terminal_2's row: the outcome carries the target, the UI holds.
    let row1 = ui.line_map.iter().position(|m| *m == Some(1)).unwrap();
    let outcome = handle_mouse_click(1, screen_row_for(row1), &mut ui, &snapshot);
    assert_eq!(outcome.focus, Some(jumped.clone()));
    assert_eq!(ui.selected_pane, Some(from.clone()));

    // A fold still deriving the pre-jump pane keeps the old highlight.
    reconcile_selection(&mut ui, &snapshot, Some(from.clone()));
    assert_eq!(ui.selected_pane, Some(from));

    // The fold that derives the jumped pane moves it.
    reconcile_selection(&mut ui, &snapshot, Some(jumped.clone()));
    assert_eq!(ui.selected_pane, Some(jumped.clone()));
    assert_eq!(ui.baseline_pane, Some(jumped));
}

#[test]
fn browse_roams_other_tabs_rows() {
    // The browse pick may walk every visible row — another tab's included
    // (the cross-tab peek that expands a remote card) — while the derived
    // baseline stays untouched underneath.
    let ws = workspace();
    let here = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let remote = PaneId::from_parts(MuxName::Zellij, "terminal_9");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", true),
            pane("terminal_9", "tab_7", false),
        ],
    );
    let mut ui = UiState {
        selected_index: 0,
        selected_pane: Some(here.clone()),
        baseline_pane: Some(here.clone()),
        ..Default::default()
    };

    select_row(&mut ui, &snapshot, 1);
    begin_or_continue_browse(&mut ui);
    // While browsing the user has the sidebar focused, so frames derive None.
    reconcile_selection(&mut ui, &snapshot, None);

    assert_eq!(ui.selected_pane, Some(remote), "the pick roams cross-tab");
    assert_eq!(ui.baseline_pane, Some(here), "the baseline never moves");
}

#[test]
fn browse_holds_across_inert_frames() {
    // Browsing with the baseline unchanged: None derivations hold the
    // baseline, the anchor still matches, the pick holds.
    let ws = workspace();
    let picked = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let baseline = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", true),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState {
        selected_index: 1,
        selected_pane: Some(picked.clone()),
        baseline_pane: Some(baseline.clone()),
        browse: Some(browse(&picked, Some(&baseline))),
        ..Default::default()
    };

    reconcile_selection(&mut ui, &snapshot, None);
    reconcile_selection(&mut ui, &snapshot, None);

    assert_eq!(ui.selected_pane, Some(picked));
    assert!(ui.browse.is_some(), "still browsing");
}

#[test]
fn browse_clears_on_baseline_change() {
    // A genuine baseline change — the user focused another working pane —
    // ends the browse, and the highlight follows the new baseline.
    let ws = workspace();
    let picked = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let anchor = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let moved = PaneId::from_parts(MuxName::Zellij, "terminal_3");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
            pane("terminal_3", "tab_0", true),
        ],
    );
    let mut ui = UiState {
        selected_index: 1,
        selected_pane: Some(picked.clone()),
        baseline_pane: Some(anchor.clone()),
        browse: Some(browse(&picked, Some(&anchor))),
        ..Default::default()
    };

    reconcile_selection(&mut ui, &snapshot, Some(moved.clone()));

    assert_eq!(ui.browse, None, "a real move ends the browse");
    assert_eq!(ui.selected_pane, Some(moved));
}

#[test]
fn browse_survives_a_jump_and_ends_on_baseline_change() {
    // A jump mutates nothing, the browse included: an Enter mid-browse
    // leaves the pick in place, so the highlight holds still until the
    // derived baseline catches up underneath it — no flicker back to the
    // old pane. The browse then ends on the genuine baseline change.
    let ws = workspace();
    let anchor = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let picked = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", true),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState {
        baseline_pane: Some(anchor.clone()),
        ..Default::default()
    };

    select_row(&mut ui, &snapshot, 1);
    begin_or_continue_browse(&mut ui);
    let outcome = handle_key(KeyAction::Enter, &mut ui, &snapshot);
    assert_eq!(outcome.focus, Some(picked.clone()));
    assert!(ui.browse.is_some(), "the jump leaves the browse in place");

    // An inert fold (baseline unchanged) keeps the pick pinned.
    reconcile_selection(&mut ui, &snapshot, Some(anchor));
    assert!(ui.browse.is_some());
    assert_eq!(ui.selected_pane, Some(picked.clone()));

    // The fold that derives the jumped pane ends the browse seamlessly —
    // the baseline takes over on the same pane.
    reconcile_selection(&mut ui, &snapshot, Some(picked.clone()));
    assert_eq!(ui.browse, None, "a real baseline change ends the browse");
    assert_eq!(ui.selected_pane, Some(picked));
}

#[test]
fn continued_browse_keeps_the_first_anchor() {
    // The second arrow continues the browse: the pick moves, but the anchor
    // (baseline_at_start) stays the one captured when browsing began, so a
    // baseline change mid-browse still ends it.
    let ws = workspace();
    let anchor = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", true),
            pane("terminal_2", "tab_0", false),
            pane("terminal_3", "tab_0", false),
        ],
    );
    let mut ui = UiState {
        baseline_pane: Some(anchor.clone()),
        ..Default::default()
    };

    select_row(&mut ui, &snapshot, 1);
    begin_or_continue_browse(&mut ui);
    // The baseline advances mid-browse (rule 1 of an intervening fold)...
    ui.baseline_pane = Some(PaneId::from_parts(MuxName::Zellij, "terminal_3"));
    select_row(&mut ui, &snapshot, 2);
    begin_or_continue_browse(&mut ui);

    assert_eq!(
        ui.browse.as_ref().map(|b| b.baseline_at_start.clone()),
        Some(Some(anchor)),
        "the anchor is the browse-start baseline, not the latest one"
    );
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

#[test]
fn browse_drops_when_its_pane_leaves_the_room() {
    // A browse picks terminal_9, which then closes. The pick must not keep
    // shadowing the baseline — it is dropped, so the highlight reconverges
    // on the next fold.
    let ws = workspace();
    let gone = PaneId::from_parts(MuxName::Zellij, "terminal_9");
    let real = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", true),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState {
        selected_index: 1,
        selected_pane: Some(gone.clone()),
        baseline_pane: Some(real.clone()),
        browse: Some(browse(&gone, Some(&real))),
        ..Default::default()
    };

    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(ui.browse, None, "the dead pick is dropped");

    // The next fold reconverges on the live baseline.
    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(ui.selected_pane, Some(real));
}

/// Lay out `snapshot` at a generous size through the real render path,
/// returning the freshly-composed hit-test map — the same map the live draw
/// stores on `UiState`. Width/height are wide and tall enough that nothing
/// the tests probe is clipped.
fn line_map_for(snapshot: &SidebarSnapshot, selected: usize) -> Vec<Option<usize>> {
    let ui = UiState {
        selected_index: selected,
        help_visible: false,
        animation_phase: 0,
        line_map: Vec::new(),
        ..Default::default()
    };
    render::compose_lines(snapshot, None, &ui, 54, 64).line_map
}

/// The screen row a content-line index maps to: borderless, the body fills
/// the frame from row 0, so map index `i` is screen row `i`.
fn screen_row_for(map_index: usize) -> u16 {
    u16::try_from(map_index).unwrap()
}

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
    snapshot.worktree_groups[0].rows[1].status = Some(crate::feed::AgentStatus::Waiting);
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
fn worktree_keys_respect_the_make_up_filter() {
    use crate::feed::AgentStatus;
    let ws = workspace();
    let snapshot = filterable_snapshot(&ws);
    let failed = PaneId::from_parts(MuxName::Zellij, "terminal_3");
    let mut ui = UiState {
        selected_index: 0,
        selected_pane: Some(failed),
        make_up_filter: Some(AgentStatus::Failed),
        ..Default::default()
    };

    let outcome = handle_key(KeyAction::WorktreeDown, &mut ui, &snapshot);

    assert_eq!(outcome, InputOutcome::default());
    assert_eq!(
        ui.selected_index, 0,
        "only one group remains under the failed filter"
    );
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

// ── The dashboard tab model ──────────────────────────────────────────────────

/// A minimal provider panel — only `kind` matters to the tab model.
fn provider(kind: &str) -> crate::SidebarProviderPanel {
    crate::SidebarProviderPanel {
        kind: kind.to_owned(),
        product_name: kind.to_owned(),
        art: Vec::new(),
        color: 7,
        version: None,
        plan: None,
        metered: false,
        remote_control: false,
        spending: None,
        windows: Vec::new(),
    }
}

/// The clickable-block room (a claude agent row, then a process row) with a
/// three-account dashboard — the tab-model fixture.
fn tabbed_snapshot(ws: &WorkspaceId) -> SidebarSnapshot {
    let mut snapshot = clickable_block_snapshot(ws);
    snapshot.providers = vec![provider("claude"), provider("codex"), provider("pi")];
    snapshot
}

#[test]
fn tab_keys_cycle_the_dashboard_and_wrap() {
    let ws = workspace();
    let snapshot = tabbed_snapshot(&ws);
    let mut ui = UiState::default();
    // Selected row 0 is the claude agent, so the derived tab starts there.
    assert_eq!(
        render::active_provider_kind(&snapshot, &ui).as_deref(),
        Some("claude")
    );

    let outcome = handle_key(KeyAction::TabNext, &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::redraw());
    assert_eq!(
        render::active_provider_kind(&snapshot, &ui).as_deref(),
        Some("codex")
    );
    // The first pick captures the derived kind it began from.
    assert_eq!(
        ui.dashboard_tab
            .as_ref()
            .unwrap()
            .derived_at_start
            .as_deref(),
        Some("claude")
    );

    // A later pick only moves the tab — the anchor holds.
    handle_key(KeyAction::TabNext, &mut ui, &snapshot);
    assert_eq!(
        render::active_provider_kind(&snapshot, &ui).as_deref(),
        Some("pi")
    );
    handle_key(KeyAction::TabNext, &mut ui, &snapshot);
    assert_eq!(
        render::active_provider_kind(&snapshot, &ui).as_deref(),
        Some("claude"),
        "→ wraps past the last tab"
    );
    handle_key(KeyAction::TabPrev, &mut ui, &snapshot);
    assert_eq!(
        render::active_provider_kind(&snapshot, &ui).as_deref(),
        Some("pi"),
        "← wraps back from the first tab"
    );
    assert_eq!(
        ui.dashboard_tab
            .as_ref()
            .unwrap()
            .derived_at_start
            .as_deref(),
        Some("claude"),
        "the browse anchor survives every pick"
    );
}

#[test]
fn tab_keys_noop_without_a_second_panel() {
    let ws = workspace();
    let mut snapshot = clickable_block_snapshot(&ws);
    snapshot.providers = vec![provider("claude")];
    let mut ui = UiState::default();

    let outcome = handle_key(KeyAction::TabNext, &mut ui, &snapshot);

    assert_eq!(outcome, InputOutcome::default());
    assert!(ui.dashboard_tab.is_none(), "one account: nothing to cycle");
}

#[test]
fn tab_pick_holds_until_the_derived_kind_genuinely_changes() {
    let ws = workspace();
    let snapshot = tabbed_snapshot(&ws);
    let agent_pane = PaneId::from_parts(MuxName::Zellij, "terminal_9");
    let process_pane = PaneId::from_parts(MuxName::Zellij, "terminal_10");
    let mut ui = UiState::default();
    reconcile_selection(&mut ui, &snapshot, Some(agent_pane.clone()));
    handle_key(KeyAction::TabNext, &mut ui, &snapshot);
    assert_eq!(
        render::active_provider_kind(&snapshot, &ui).as_deref(),
        Some("codex")
    );

    // Re-deriving the same claude row keeps the pick.
    reconcile_selection(&mut ui, &snapshot, Some(agent_pane.clone()));
    assert!(ui.dashboard_tab.is_some(), "same derived kind: pick holds");

    // A process-row selection derives no kind — the pick survives the hop.
    reconcile_selection(&mut ui, &snapshot, Some(process_pane));
    assert!(
        ui.dashboard_tab.is_some(),
        "a None derivation never ends the pick"
    );

    // The selected agent row turning into another provider's ends it: the
    // derived kind genuinely changed, so the derived default takes over.
    let mut moved = tabbed_snapshot(&ws);
    moved.worktree_groups[0].rows[0].name = "pi".to_owned();
    reconcile_selection(&mut ui, &moved, Some(agent_pane));
    assert!(
        ui.dashboard_tab.is_none(),
        "a genuine derived-kind change hands the tab back"
    );
    assert_eq!(
        render::active_provider_kind(&moved, &ui).as_deref(),
        Some("pi")
    );
}

#[test]
fn tab_pick_drops_when_its_panel_leaves_the_dashboard() {
    let ws = workspace();
    let snapshot = tabbed_snapshot(&ws);
    let agent_pane = PaneId::from_parts(MuxName::Zellij, "terminal_9");
    let mut ui = UiState::default();
    reconcile_selection(&mut ui, &snapshot, Some(agent_pane.clone()));
    handle_key(KeyAction::TabNext, &mut ui, &snapshot);

    let mut shrunk = tabbed_snapshot(&ws);
    shrunk.providers = vec![provider("claude"), provider("pi")];
    reconcile_selection(&mut ui, &shrunk, Some(agent_pane));

    assert!(
        ui.dashboard_tab.is_none(),
        "a pick whose panel left the dashboard is dropped"
    );
}

#[test]
fn clicking_a_tab_label_picks_that_tab_in_place() {
    let ws = workspace();
    let snapshot = tabbed_snapshot(&ws);
    // The rail's geometry after the gutter translation: the active
    // `─ Claude ─` chip footprint edge to edge, then the inactive
    // `─ Codex ─` footprint past the 2-cell `──` gap.
    let mut ui = UiState {
        tab_hits: vec![
            crate::sidebar_renderer::render::ProviderTabHit {
                line: 30,
                col_start: 3,
                col_end: 13,
                kind: "claude".to_owned(),
            },
            crate::sidebar_renderer::render::ProviderTabHit {
                line: 30,
                col_start: 15,
                col_end: 24,
                kind: "codex".to_owned(),
            },
        ],
        ..Default::default()
    };

    let outcome = handle_mouse_click(17, 30, &mut ui, &snapshot);

    // A tab click repaints in place — never a jump.
    assert_eq!(outcome, InputOutcome::redraw());
    assert!(outcome.focus.is_none());
    assert_eq!(
        render::active_provider_kind(&snapshot, &ui).as_deref(),
        Some("codex")
    );

    // The hit range is half-open: the cell past the tab falls through to the
    // row hit-test (and lands nowhere on this chrome line).
    let outcome = handle_mouse_click(24, 30, &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::default());
}

/// One agent or process row bound to `pane_name` in `worktree` — the make-up
/// filter tests' row builder.
fn filter_row(
    kind: crate::SidebarRowKind,
    id: &str,
    name: &str,
    status: Option<crate::feed::AgentStatus>,
    pane_name: &str,
    worktree: &str,
) -> crate::SidebarRow {
    crate::SidebarRow {
        row_kind: kind,
        id: id.to_owned(),
        name: name.to_owned(),
        status,
        phase: crate::agents::TurnPhase::Idle,
        pane: Some(pane(pane_name, "tab_0", false)),
        request_id: None,
        surface: None,
        task: None,
        prompt: None,
        model: None,
        effort: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        cache_read_input_tokens: None,
        fresh_input_tokens: None,
        output_tokens: None,
        todo_done: None,
        todo_total: None,
        context: None,
        context_severity: None,
        worktree_path: Some(worktree.to_owned()),
        worktree_branch: None,
        last_activity: Timestamp::now(),
        registered_at: None,
        resolver: None,
        options: Vec::new(),
        sub_agents: Vec::new(),
        process_active: false,
        command_detail: None,
        compacting: false,
        turn_error_label: None,
        rss_kb: None,
        cpu_pct: None,
        io_bps: None,
    }
}

/// Two worktrees and a process tail — the make-up filter fixture: a running
/// agent (`terminal_1`) and a `zsh` process row (`terminal_2`) in `main`, a
/// failed agent (`terminal_3`) in `feature`. Failed and running each read 1
/// in the cockpit; waiting reads 0.
fn filterable_snapshot(ws: &WorkspaceId) -> SidebarSnapshot {
    use crate::feed::AgentStatus;
    let mut snapshot = snapshot(ws);
    snapshot.worktree_groups = vec![
        crate::SidebarWorktreeGroup {
            key: "/repo/main".to_owned(),
            label: "main".to_owned(),
            kind: crate::SidebarWorktreeKind::Worktree,
            status_counts: vec![crate::SidebarStatusCount {
                status: AgentStatus::Running,
                count: 1,
            }],
            rows: vec![
                filter_row(
                    crate::SidebarRowKind::Agent,
                    "agent-1",
                    "claude",
                    Some(AgentStatus::Running),
                    "terminal_1",
                    "/repo/main",
                ),
                filter_row(
                    crate::SidebarRowKind::Process,
                    "terminal_2",
                    "zsh",
                    None,
                    "terminal_2",
                    "/repo/main",
                ),
            ],
            hidden_count: 0,
            diff_added: None,
            diff_removed: None,
            commits_ahead: None,
            commits_behind: None,
            trunk: None,
            clean: None,
        },
        crate::SidebarWorktreeGroup {
            key: "/repo/feature".to_owned(),
            label: "feature".to_owned(),
            kind: crate::SidebarWorktreeKind::Worktree,
            status_counts: vec![crate::SidebarStatusCount {
                status: AgentStatus::Failed,
                count: 1,
            }],
            rows: vec![filter_row(
                crate::SidebarRowKind::Agent,
                "agent-2",
                "codex",
                Some(AgentStatus::Failed),
                "terminal_3",
                "/repo/feature",
            )],
            hidden_count: 0,
            diff_added: None,
            diff_removed: None,
            commits_ahead: None,
            commits_behind: None,
            trunk: None,
            clean: None,
        },
    ];
    snapshot
}

#[test]
fn make_up_click_picks_switches_and_clears_the_filter() {
    use crate::feed::AgentStatus;
    let ws = workspace();
    let snapshot = filterable_snapshot(&ws);
    let mut ui = UiState {
        make_up_hits: vec![
            render::MakeUpHit {
                line: 5,
                col_start: 5,
                col_end: 8,
                status: AgentStatus::Failed,
            },
            render::MakeUpHit {
                line: 5,
                col_start: 28,
                col_end: 31,
                status: AgentStatus::Running,
            },
        ],
        ..Default::default()
    };

    // A bucket click filters in place — a repaint, never a jump.
    let outcome = handle_mouse_click(6, 5, &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::redraw());
    assert!(outcome.focus.is_none());
    assert_eq!(ui.make_up_filter, Some(AgentStatus::Failed));

    // A click on another bucket switches the pick in place.
    handle_mouse_click(28, 5, &mut ui, &snapshot);
    assert_eq!(ui.make_up_filter, Some(AgentStatus::Running));

    // A second click on the active bucket clears back to show-all.
    handle_mouse_click(28, 5, &mut ui, &snapshot);
    assert_eq!(ui.make_up_filter, None);

    // The hit range is half-open: the cell past the bucket falls through to
    // the row hit-test (and lands nowhere on this chrome line).
    let outcome = handle_mouse_click(8, 5, &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::default());
    assert_eq!(ui.make_up_filter, None);
}

#[test]
fn make_up_filter_keys_pick_toggle_clear_and_ignore_empty_buckets() {
    use crate::feed::AgentStatus;
    let ws = workspace();
    let snapshot = filterable_snapshot(&ws);
    let mut ui = UiState::default();

    let outcome = handle_key(
        KeyAction::Filter(FilterAction::Status(AgentStatus::Failed)),
        &mut ui,
        &snapshot,
    );
    assert_eq!(outcome, InputOutcome::redraw());
    assert!(outcome.focus.is_none());
    assert_eq!(ui.make_up_filter, Some(AgentStatus::Failed));

    let outcome = handle_key(
        KeyAction::Filter(FilterAction::Status(AgentStatus::Failed)),
        &mut ui,
        &snapshot,
    );
    assert_eq!(outcome, InputOutcome::redraw());
    assert_eq!(ui.make_up_filter, None, "the active key toggles to all");

    let outcome = handle_key(
        KeyAction::Filter(FilterAction::Status(AgentStatus::Waiting)),
        &mut ui,
        &snapshot,
    );
    assert_eq!(
        outcome,
        InputOutcome::default(),
        "zero-count buckets are inert from keys too"
    );
    assert_eq!(ui.make_up_filter, None);

    handle_key(
        KeyAction::Filter(FilterAction::Status(AgentStatus::Running)),
        &mut ui,
        &snapshot,
    );
    assert_eq!(ui.make_up_filter, Some(AgentStatus::Running));

    let outcome = handle_key(KeyAction::Filter(FilterAction::All), &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::redraw());
    assert_eq!(ui.make_up_filter, None);

    let outcome = handle_key(KeyAction::Filter(FilterAction::All), &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::default());
}

#[test]
fn make_up_hits_land_on_the_painted_buckets_through_the_real_frame() {
    use crate::feed::AgentStatus;
    let ws = workspace();
    let snapshot = filterable_snapshot(&ws);
    let mut ui = UiState::default();

    // The absolute translation — the cockpit's line base plus the chrome
    // gutter — is what the synthetic-hit test above takes on faith; the real
    // composed frame proves each hit's footprint covers exactly the bucket it
    // filters by, zero buckets emitting none.
    let composed = render::compose_lines(&snapshot, None, &ui, 54, 64);
    let texts: Vec<String> = composed
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect();
    let footprints: Vec<(AgentStatus, String)> = composed
        .make_up_hits
        .iter()
        .map(|hit| {
            let text: String = texts[hit.line]
                .chars()
                .skip(usize::from(hit.col_start))
                .take(usize::from(hit.col_end - hit.col_start))
                .collect();
            (hit.status, text)
        })
        .collect();
    assert_eq!(
        footprints,
        vec![
            (AgentStatus::Failed, "! 1".to_owned()),
            (AgentStatus::Running, "⢿ 1".to_owned()),
        ],
        "one hit per non-zero bucket, each covering its painted text"
    );

    // The same composed hits drive the click path — the draw's write-back,
    // then a click inside the failed bucket's footprint picks it.
    ui.make_up_hits = composed.make_up_hits;
    let (column, row) = (ui.make_up_hits[0].col_start, ui.make_up_hits[0].line);
    handle_mouse_click(column, u16::try_from(row).unwrap(), &mut ui, &snapshot);
    assert_eq!(ui.make_up_filter, Some(AgentStatus::Failed));
}

#[test]
fn make_up_filter_auto_clears_when_its_bucket_empties() {
    use crate::feed::AgentStatus;
    let ws = workspace();
    let snapshot = filterable_snapshot(&ws);

    // The waiting bucket reads 0 in this room, so a stale waiting filter ends
    // on the fold — the body's twin of a tab pick whose panel left.
    let mut ui = UiState {
        make_up_filter: Some(AgentStatus::Waiting),
        ..Default::default()
    };
    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(ui.make_up_filter, None);

    // A filter whose bucket still counts holds through the fold.
    ui.make_up_filter = Some(AgentStatus::Failed);
    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(ui.make_up_filter, Some(AgentStatus::Failed));
}

#[test]
fn make_up_filter_narrows_ordinals_in_lockstep_with_the_line_map() {
    use crate::feed::AgentStatus;
    let ws = workspace();
    let snapshot = filterable_snapshot(&ws);
    let filter = Some(AgentStatus::Failed);

    // The selection walk and the rendered line map share one predicate, so
    // their ordinals can never drift: the filtered universe is exactly the
    // contiguous 0..count the body's hit-test entries carry.
    assert_eq!(visible_row_count(&snapshot, None), 3);
    assert_eq!(visible_row_count(&snapshot, filter), 1);
    let failed = PaneId::from_parts(MuxName::Zellij, "terminal_3");
    let running = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    assert_eq!(row_index_of_pane(&snapshot, filter, &failed), Some(0));
    assert_eq!(row_index_of_pane(&snapshot, filter, &running), None);
    assert_eq!(row_index_of_pane(&snapshot, None, &failed), Some(2));

    let ui = UiState {
        make_up_filter: filter,
        ..Default::default()
    };
    let map = render::compose_lines(&snapshot, None, &ui, 54, 64).line_map;
    let mut ordinals: Vec<usize> = map.iter().flatten().copied().collect();
    ordinals.dedup();
    assert_eq!(
        ordinals,
        (0..visible_row_count(&snapshot, filter)).collect::<Vec<_>>(),
        "the line map carries exactly the filtered walk's ordinals"
    );
}

#[test]
fn next_attention_jump_respects_the_filter() {
    use crate::feed::AgentStatus;
    let ws = workspace();
    let snapshot = filterable_snapshot(&ws);

    // Unfiltered, `␣` finds the failed row at its body ordinal.
    assert_eq!(next_attention_index(&snapshot, None, 0), Some(2));
    // Filtered to a calm status, the universe holds nothing actionable.
    assert_eq!(
        next_attention_index(&snapshot, Some(AgentStatus::Running), 0),
        None
    );
    // Filtered to the attention status, the jump cycles the filtered rows.
    assert_eq!(
        next_attention_index(&snapshot, Some(AgentStatus::Failed), 0),
        Some(0)
    );
}

#[test]
fn filtered_out_selection_drops_and_reseats_from_the_held_baseline() {
    use crate::feed::AgentStatus;
    let ws = workspace();
    let snapshot = filterable_snapshot(&ws);
    let running = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let mut ui = UiState::default();
    reconcile_selection(&mut ui, &snapshot, Some(running.clone()));
    assert_eq!(ui.selected_pane, Some(running.clone()));

    // Filtering to `failed` leaves the running highlight no row: the visible
    // pick drops to a clamped index, but the baseline — room membership, not
    // body membership — holds through every fold.
    toggle_make_up_filter(&mut ui, &snapshot, AgentStatus::Failed);
    assert_eq!(ui.make_up_filter, Some(AgentStatus::Failed));
    assert_eq!(ui.selected_pane, None);
    assert_eq!(ui.selected_index, 0);
    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(ui.baseline_pane, Some(running.clone()));
    assert_eq!(ui.selected_pane, None, "the hidden highlight stays dropped");

    // Clearing the filter re-seats the highlight on the held baseline.
    toggle_make_up_filter(&mut ui, &snapshot, AgentStatus::Failed);
    assert_eq!(ui.make_up_filter, None);
    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(ui.selected_pane, Some(running));
    assert_eq!(ui.selected_index, 0);
}
