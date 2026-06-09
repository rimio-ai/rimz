use super::*;

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
#[test]
fn jumping_to_a_card_ends_the_make_up_filter() {
    use crate::feed::AgentStatus;
    let ws = workspace();
    let snapshot = filterable_snapshot(&ws);
    let failed = PaneId::from_parts(MuxName::Zellij, "terminal_3");
    let running = PaneId::from_parts(MuxName::Zellij, "terminal_1");

    // A digit jump resolves its target in the filtered body, then ends the
    // filter — a status lens is one tab's transient view, never a mode that
    // outlives the jump leaving the tab. The body reshapes, so it repaints too.
    let mut ui = UiState {
        make_up_filter: Some(AgentStatus::Failed),
        ..Default::default()
    };
    let outcome = handle_key(KeyAction::Digit(1), &mut ui, &snapshot);
    assert_eq!(outcome.focus, Some(failed.clone()));
    assert!(outcome.redraw, "clearing the filter reshapes the body");
    assert_eq!(ui.make_up_filter, None);

    // Enter on the highlighted row clears it the same way, re-anchoring the
    // surviving highlight at its show-all ordinal.
    let mut ui = UiState {
        make_up_filter: Some(AgentStatus::Failed),
        selected_pane: Some(failed.clone()),
        ..Default::default()
    };
    let outcome = handle_key(KeyAction::Enter, &mut ui, &snapshot);
    assert_eq!(outcome.focus, Some(failed.clone()));
    assert_eq!(ui.make_up_filter, None);
    assert_eq!(
        ui.selected_index, 2,
        "the held highlight re-anchors under show-all"
    );

    // With no filter live a jump stays pure: nothing to clear, nothing to
    // repaint.
    let mut ui = UiState::default();
    let outcome = handle_key(KeyAction::Digit(1), &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::focus(running));
}
