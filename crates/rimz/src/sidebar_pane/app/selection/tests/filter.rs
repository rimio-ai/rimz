use super::*;
use std::time::Duration;

#[test]
fn worktree_keys_respect_the_make_up_filter() {
    use crate::agents::AgentStatus;
    let ws = workspace();
    let snapshot = filterable_snapshot(&ws);
    let failed = PaneId::from_parts(MuxName::Zellij, "terminal_3");
    let mut ui = UiState {
        selected_index: 0,
        selected_pane: Some(failed),
        make_up_filter: Some(BodyFilter::Status(AgentStatus::Failed)),
        ..Default::default()
    };

    let outcome = handle_key(KeyAction::WorktreeDown, &mut ui, &snapshot);

    assert_eq!(outcome, InputOutcome::default());
    assert_eq!(
        ui.selected_index, 0,
        "only one group remains under the failed filter"
    );
    assert_eq!(
        ui.make_up_filter,
        Some(BodyFilter::Status(AgentStatus::Failed))
    );
}
#[test]
fn make_up_click_picks_switches_and_clears_the_filter() {
    use crate::agents::AgentStatus;
    let ws = workspace();
    let snapshot = filterable_snapshot(&ws);
    let mut ui = UiState {
        interactions: render::FrameInteractions::from_parts(
            vec![None; 6],
            vec![
                render::HitRegion::line(
                    5,
                    5..8,
                    HitTarget::BodyFilter(BodyFilter::Status(AgentStatus::Failed)),
                ),
                render::HitRegion::line(
                    5,
                    28..31,
                    HitTarget::BodyFilter(BodyFilter::Status(AgentStatus::Running)),
                ),
            ],
        ),
        ..Default::default()
    };

    // A bucket click filters in place — a repaint, never a jump.
    let outcome = handle_mouse_click(6, 5, &mut ui, &snapshot);
    assert_eq!(
        outcome,
        InputOutcome {
            redraw: true,
            effect: Some(InputEffect::SyncFilter(Some(BodyFilter::Status(
                AgentStatus::Failed,
            )))),
        }
    );
    assert_eq!(
        ui.make_up_filter,
        Some(BodyFilter::Status(AgentStatus::Failed))
    );

    // A click on another bucket switches the pick in place.
    handle_mouse_click(28, 5, &mut ui, &snapshot);
    assert_eq!(
        ui.make_up_filter,
        Some(BodyFilter::Status(AgentStatus::Running))
    );

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
    use crate::agents::AgentStatus;
    let ws = workspace();
    let snapshot = filterable_snapshot(&ws);
    let mut ui = UiState::default();

    let outcome = handle_key(
        KeyAction::Filter(Some(BodyFilter::Status(AgentStatus::Failed))),
        &mut ui,
        &snapshot,
    );
    assert_eq!(
        outcome,
        InputOutcome::sync_filter(Some(BodyFilter::Status(AgentStatus::Failed)))
    );
    assert_eq!(
        ui.make_up_filter,
        Some(BodyFilter::Status(AgentStatus::Failed))
    );

    let outcome = handle_key(
        KeyAction::Filter(Some(BodyFilter::Status(AgentStatus::Failed))),
        &mut ui,
        &snapshot,
    );
    assert_eq!(outcome, InputOutcome::sync_filter(None));
    assert_eq!(ui.make_up_filter, None, "the active key toggles to all");

    let outcome = handle_key(
        KeyAction::Filter(Some(BodyFilter::Status(AgentStatus::Waiting))),
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
        KeyAction::Filter(Some(BodyFilter::Status(AgentStatus::Running))),
        &mut ui,
        &snapshot,
    );
    assert_eq!(
        ui.make_up_filter,
        Some(BodyFilter::Status(AgentStatus::Running))
    );

    let outcome = handle_key(KeyAction::Filter(None), &mut ui, &snapshot);
    assert_eq!(outcome.effect, Some(InputEffect::SyncFilter(None)));
    assert_eq!(ui.make_up_filter, None);

    let outcome = handle_key(KeyAction::Filter(None), &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::default());
}
#[test]
fn make_up_hits_land_on_the_painted_buckets_through_the_real_frame() {
    use crate::agents::AgentStatus;
    let ws = workspace();
    let snapshot = filterable_snapshot(&ws);
    let mut ui = UiState::default();

    // The absolute translation — the cockpit's line base plus the chrome
    // gutter — is what the synthetic-hit test above takes on faith; the real
    // composed frame proves each hit's footprint covers exactly the bucket it
    // filters by, zero buckets emitting none.
    let theme = ui.theme(&snapshot.theme);
    let composed = render::compose_lines(&snapshot, None, &ui, theme.as_ref(), 54, 64);
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
    let footprints: Vec<(BodyFilter, String)> = composed
        .interactions
        .regions()
        .iter()
        .filter_map(|hit| {
            let HitTarget::BodyFilter(filter) = &hit.target else {
                return None;
            };
            let text = text_cell_range(&texts[hit.rows.start], hit.columns.start, hit.columns.end);
            Some((*filter, text))
        })
        .collect();
    assert_eq!(
        footprints,
        vec![
            (BodyFilter::Status(AgentStatus::Failed), "! 1".to_owned()),
            (BodyFilter::Status(AgentStatus::Running), "⢿ 1".to_owned()),
        ],
        "one hit per non-zero bucket, each covering its painted text"
    );

    // The same composed hits drive the click path — the draw's write-back,
    // then a click inside the failed bucket's footprint picks it.
    ui.interactions = composed.interactions;
    let target = HitTarget::BodyFilter(BodyFilter::Status(AgentStatus::Failed));
    let (column, row) = ui
        .interactions
        .line_for_target(&target)
        .expect("failed hit");
    let outcome = handle_mouse_click(column, row, &mut ui, &snapshot);
    assert_eq!(
        outcome.effect,
        Some(InputEffect::SyncFilter(Some(BodyFilter::Status(
            AgentStatus::Failed,
        ))))
    );
    assert_eq!(
        ui.make_up_filter,
        Some(BodyFilter::Status(AgentStatus::Failed))
    );
}

#[test]
fn unread_count_click_toggles_the_unread_lens() {
    let ws = workspace();
    let mut snapshot = filterable_snapshot(&ws);
    snapshot.worktree_groups[0].rows[0].unread = true;
    snapshot.worktree_groups[1].rows[0].unread = true;
    let mut ui = UiState::default();
    let theme = ui.theme(&snapshot.theme);
    let composed = render::compose_lines(&snapshot, None, &ui, theme.as_ref(), 54, 64);
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
    let unread_hit = composed
        .interactions
        .regions()
        .iter()
        .find(|hit| hit.target == HitTarget::BodyFilter(BodyFilter::Unread))
        .expect("unread count emits a hit when unread rows exist");
    assert_eq!(
        text_cell_range(
            &texts[unread_hit.rows.start],
            unread_hit.columns.start,
            unread_hit.columns.end
        ),
        "(2)",
        "unread hit covers only the count, not its leading space"
    );
    let unread_column = unread_hit.columns.start;
    let unread_row = u16::try_from(unread_hit.rows.start).unwrap();

    ui.interactions = composed.interactions;
    let outcome = handle_mouse_click(unread_column, unread_row, &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::sync_filter(Some(BodyFilter::Unread)));
    assert_eq!(ui.make_up_filter, Some(BodyFilter::Unread));
    let theme = ui.theme(&snapshot.theme);
    let picked = render::compose_lines(&snapshot, None, &ui, theme.as_ref(), 54, 64);
    let picked_count = picked.lines[usize::from(unread_row)]
        .spans
        .iter()
        .find(|span| span.content.as_ref() == "(2)")
        .expect("picked unread count is its own chip span");
    assert!(
        picked_count.style.bg.is_some()
            || picked_count
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::REVERSED),
        "picked unread count reads as a chip"
    );
    assert!(
        picked_count
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD),
        "picked unread count keeps the count weight"
    );

    let outcome = handle_mouse_click(unread_column, unread_row, &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::sync_filter(None));
    assert_eq!(ui.make_up_filter, None);

    for row in snapshot
        .worktree_groups
        .iter_mut()
        .flat_map(|group| group.rows.iter_mut())
    {
        row.unread = false;
    }
    let mut ui = UiState::default();
    let theme = ui.theme(&snapshot.theme);
    let composed = render::compose_lines(&snapshot, None, &ui, theme.as_ref(), 54, 64);
    assert!(
        composed
            .interactions
            .regions()
            .iter()
            .all(|hit| hit.target != HitTarget::BodyFilter(BodyFilter::Unread)),
        "zero unread rows leave the cockpit count inert"
    );
}

#[test]
fn pr_count_click_toggles_the_open_pr_lens() {
    let ws = workspace();
    let mut snapshot = filterable_snapshot(&ws);
    snapshot.worktree_groups[1].pr_state = Some(crate::store::snapshot::WorktreePrState::Open);
    snapshot.worktree_groups[1].pr_number = Some(91);
    let mut ui = UiState::default();
    let theme = ui.theme(&snapshot.theme);
    let composed = render::compose_lines(&snapshot, None, &ui, theme.as_ref(), 54, 64);
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
    let pr_hit = composed
        .interactions
        .regions()
        .iter()
        .find(|hit| hit.target == HitTarget::BodyFilter(BodyFilter::OpenPr))
        .expect("open PR count emits a hit when an open PR lane exists");
    assert_eq!(
        text_cell_range(
            &texts[pr_hit.rows.start],
            pr_hit.columns.start,
            pr_hit.columns.end
        ),
        "⑃ 1",
        "open PR hit covers only the glyph and count, not its leading space"
    );
    let pr_column = pr_hit.columns.start;
    let pr_row = u16::try_from(pr_hit.rows.start).unwrap();

    ui.interactions = composed.interactions;
    let outcome = handle_mouse_click(pr_column, pr_row, &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::sync_filter(Some(BodyFilter::OpenPr)));
    assert_eq!(ui.make_up_filter, Some(BodyFilter::OpenPr));

    let theme = ui.theme(&snapshot.theme);
    let picked = render::compose_lines(&snapshot, None, &ui, theme.as_ref(), 54, 64);
    let glyph_index = picked.lines[usize::from(pr_row)]
        .spans
        .iter()
        .position(|span| span.content.as_ref() == "⑃")
        .expect("picked open PR glyph is its own span");
    for span in &picked.lines[usize::from(pr_row)].spans[glyph_index..=glyph_index + 1] {
        assert!(
            span.style.bg.is_some()
                || span
                    .style
                    .add_modifier
                    .contains(ratatui::style::Modifier::REVERSED),
            "picked open PR glyph and count read as one chip"
        );
        assert!(
            span.style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD),
            "picked open PR chip keeps the count weight"
        );
    }

    let outcome = handle_mouse_click(pr_column, pr_row, &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::sync_filter(None));
    assert_eq!(ui.make_up_filter, None);

    snapshot.worktree_groups[1].pr_state = None;
    snapshot.worktree_groups[1].pr_number = None;
    let mut ui = UiState::default();
    let theme = ui.theme(&snapshot.theme);
    let composed = render::compose_lines(&snapshot, None, &ui, theme.as_ref(), 54, 64);
    assert!(
        composed
            .interactions
            .regions()
            .iter()
            .all(|hit| hit.target != HitTarget::BodyFilter(BodyFilter::OpenPr)),
        "zero open PR lanes leave the cockpit count inert"
    );
}

fn text_cell_range(text: &str, start: u16, end: u16) -> String {
    let start = byte_index_at_cell(text, usize::from(start));
    let end = byte_index_at_cell(text, usize::from(end));
    text[start..end].to_owned()
}

fn byte_index_at_cell(text: &str, target: usize) -> usize {
    let mut cells = 0;
    for (index, ch) in text.char_indices() {
        let width = ratatui::text::Span::raw(ch.to_string()).width();
        if cells >= target && width > 0 {
            return index;
        }
        cells += width;
    }
    text.len()
}

#[test]
fn make_up_filter_auto_clears_when_its_bucket_empties() {
    use crate::agents::AgentStatus;
    let ws = workspace();
    let snapshot = filterable_snapshot(&ws);

    // The waiting bucket reads 0 in this room, so a stale waiting filter ends
    // on the fold — the body's twin of a tab pick whose panel left.
    let mut ui = UiState {
        make_up_filter: Some(BodyFilter::Status(AgentStatus::Waiting)),
        ..Default::default()
    };
    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(ui.make_up_filter, None);

    // A filter whose bucket still counts holds through the fold.
    ui.make_up_filter = Some(BodyFilter::Status(AgentStatus::Failed));
    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(
        ui.make_up_filter,
        Some(BodyFilter::Status(AgentStatus::Failed))
    );

    ui.make_up_filter = Some(BodyFilter::OpenPr);
    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(
        ui.make_up_filter, None,
        "a stale open PR lens clears after its last PR resolves"
    );
}
#[test]
fn make_up_filter_narrows_ordinals_in_lockstep_with_frame_targets() {
    use crate::agents::AgentStatus;
    let ws = workspace();
    let snapshot = filterable_snapshot(&ws);
    let filter = Some(BodyFilter::Status(AgentStatus::Failed));

    // The selection walk and the rendered line map share one predicate, so
    // their ordinals can never drift: the filtered universe is exactly the
    // contiguous 0..count the body's hit-test entries carry.
    assert_eq!(roster_len(&snapshot, None, &Default::default()), 3);
    assert_eq!(roster_len(&snapshot, filter, &Default::default()), 1);
    let failed = PaneId::from_parts(MuxName::Zellij, "terminal_3");
    let running = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    assert_eq!(row_index_of_pane(&snapshot, filter, &failed), Some(0));
    assert_eq!(row_index_of_pane(&snapshot, filter, &running), None);
    assert_eq!(row_index_of_pane(&snapshot, None, &failed), Some(2));

    let mut ui = UiState {
        make_up_filter: filter,
        ..Default::default()
    };
    let theme = ui.theme(&snapshot.theme);
    let interactions =
        render::compose_lines(&snapshot, None, &ui, theme.as_ref(), 54, 64).interactions;
    let mut ordinals = (0..interactions.line_count())
        .filter_map(|line| interactions.row_at_line(line))
        .collect::<Vec<_>>();
    ordinals.dedup();
    assert_eq!(
        ordinals,
        (0..roster_len(&snapshot, filter, &Default::default())).collect::<Vec<_>>(),
        "the line map carries exactly the filtered walk's ordinals"
    );
}
#[test]
fn next_attention_jump_respects_the_filter() {
    use crate::agents::AgentStatus;
    let ws = workspace();
    let snapshot = filterable_snapshot(&ws);

    // Unfiltered, `␣` finds the failed row at its body ordinal.
    assert_eq!(next_attention_index(&snapshot, None, 0), Some(2));
    // Filtered to a calm status, the universe holds nothing actionable.
    assert_eq!(
        next_attention_index(&snapshot, Some(BodyFilter::Status(AgentStatus::Running)), 0),
        None
    );
    // Filtered to the attention status, the jump cycles the filtered rows.
    assert_eq!(
        next_attention_index(&snapshot, Some(BodyFilter::Status(AgentStatus::Failed)), 0),
        Some(0)
    );
}

#[test]
fn unread_filter_narrows_to_unread_rows() {
    let ws = workspace();
    let mut snapshot = filterable_snapshot(&ws);
    snapshot.worktree_groups[0].rows[0].unread = true;
    snapshot.worktree_groups[1].rows[0].unread = true;
    let filter = Some(BodyFilter::Unread);

    assert_eq!(roster_len(&snapshot, None, &Default::default()), 3);
    assert_eq!(roster_len(&snapshot, filter, &Default::default()), 2);

    let mut ui = UiState::default();
    let outcome = handle_key(
        KeyAction::Filter(Some(BodyFilter::Unread)),
        &mut ui,
        &snapshot,
    );
    assert_eq!(outcome, InputOutcome::sync_filter(filter));
    assert_eq!(ui.make_up_filter, filter);

    let failed = PaneId::from_parts(MuxName::Zellij, "terminal_3");
    let running = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    assert_eq!(row_index_of_pane(&snapshot, filter, &running), Some(0));
    assert_eq!(row_index_of_pane(&snapshot, filter, &failed), Some(1));
    assert_eq!(
        next_attention_index(&snapshot, filter, 0),
        Some(1),
        "only the unread failed row is an unread needs-a-look target"
    );

    let outcome = handle_key(
        KeyAction::Filter(Some(BodyFilter::Unread)),
        &mut ui,
        &snapshot,
    );
    assert_eq!(outcome, InputOutcome::sync_filter(None));
    assert_eq!(ui.make_up_filter, None);

    for row in snapshot
        .worktree_groups
        .iter_mut()
        .flat_map(|group| group.rows.iter_mut())
    {
        row.unread = false;
    }
    let outcome = handle_key(
        KeyAction::Filter(Some(BodyFilter::Unread)),
        &mut ui,
        &snapshot,
    );
    assert_eq!(outcome, InputOutcome::default());
    assert_eq!(ui.make_up_filter, None);
}

#[test]
fn next_attention_jump_targets_unread_before_read_attention() {
    use crate::agents::AgentStatus;
    let ws = workspace();
    let mut snapshot = filterable_snapshot(&ws);
    let read_failed = &mut snapshot.worktree_groups[1].rows[0];
    read_failed.last_activity = snapshot.now - Duration::from_secs(3_600);

    let unread_running = &mut snapshot.worktree_groups[0].rows[0];
    unread_running.unread = true;
    unread_running.last_activity = snapshot.now - Duration::from_secs(7_200);

    let unread_success = &mut snapshot.worktree_groups[0].rows[1];
    unread_success.name = "claude".to_owned();
    unread_success.card =
        crate::store::snapshot::RowCard::Agent(Box::new(crate::store::snapshot::AgentCard {
            status: AgentStatus::Success,
            phase: crate::agents::TurnPhase::Idle,
            ..crate::store::snapshot::AgentCard::default()
        }));
    unread_success.unread = true;
    unread_success.last_activity = snapshot.now - Duration::from_secs(900);

    assert_eq!(
        next_attention_index(&snapshot, None, 0),
        Some(1),
        "unread calm/running rows are filtered out, unread needs-a-look rows lead"
    );
    assert_eq!(
        next_attention_index(&snapshot, None, 1),
        Some(2),
        "read actionable rows follow after unread episodes"
    );
    assert_eq!(
        next_attention_index(&snapshot, None, 2),
        Some(1),
        "the triage list cycles by attention priority"
    );
}

#[test]
fn next_attention_jump_orders_unread_episodes_by_age() {
    use crate::agents::AgentStatus;
    let ws = workspace();
    let mut snapshot = filterable_snapshot(&ws);
    snapshot.worktree_groups[0].rows[0].unread = true;
    snapshot.worktree_groups[0].rows[0].last_activity = snapshot.now - Duration::from_secs(7_200);

    let success = &mut snapshot.worktree_groups[0].rows[1];
    success.name = "claude".to_owned();
    success.card =
        crate::store::snapshot::RowCard::Agent(Box::new(crate::store::snapshot::AgentCard {
            status: AgentStatus::Success,
            phase: crate::agents::TurnPhase::Idle,
            ..crate::store::snapshot::AgentCard::default()
        }));
    success.unread = true;
    success.last_activity = snapshot.now - Duration::from_secs(3_600);

    snapshot.worktree_groups[1].rows.push(filter_row(
        true,
        "agent-3",
        "pi",
        Some(AgentStatus::Paused),
        "terminal_4",
        "/repo/feature",
    ));
    snapshot.worktree_groups[1].rows[1].unread = true;
    snapshot.worktree_groups[1].rows[1].last_activity = snapshot.now - Duration::from_secs(1_800);
    snapshot.worktree_groups[1].rows[0].unread = true;
    snapshot.worktree_groups[1].rows[0].last_activity = snapshot.now - Duration::from_secs(600);

    assert_eq!(
        next_attention_index(&snapshot, None, 0),
        Some(1),
        "oldest unread episode leads when the selection is outside the triage list"
    );
    assert_eq!(
        next_attention_index(&snapshot, None, 1),
        Some(3),
        "paused unread episodes stay in the unread pass"
    );
    assert_eq!(
        next_attention_index(&snapshot, None, 3),
        Some(2),
        "newer unread episodes follow"
    );
}
#[test]
fn step_attention_index_reverses_the_inbox_walk() {
    use crate::agents::AgentStatus;
    let ws = workspace();
    let mut snapshot = filterable_snapshot(&ws);
    let read_failed = &mut snapshot.worktree_groups[1].rows[0];
    read_failed.last_activity = snapshot.now - Duration::from_secs(3_600);

    let unread_success = &mut snapshot.worktree_groups[0].rows[1];
    unread_success.name = "claude".to_owned();
    unread_success.card =
        crate::store::snapshot::RowCard::Agent(Box::new(crate::store::snapshot::AgentCard {
            status: AgentStatus::Success,
            phase: crate::agents::TurnPhase::Idle,
            ..crate::store::snapshot::AgentCard::default()
        }));
    unread_success.unread = true;
    unread_success.last_activity = snapshot.now - Duration::from_secs(900);

    // The forward triage order is [unread success @1, read failed @2]; reverse
    // inverts every step and enters at the last row from outside the list.
    assert_eq!(
        step_attention_index(&snapshot, None, &Default::default(), 1, false),
        Some(2),
        "reverse from the first candidate wraps to the last"
    );
    assert_eq!(
        step_attention_index(&snapshot, None, &Default::default(), 2, false),
        Some(1),
        "reverse steps to the previous candidate"
    );
    assert_eq!(
        step_attention_index(&snapshot, None, &Default::default(), 0, false),
        Some(2),
        "a selection outside the list enters at the last row going backward"
    );
}
#[test]
fn filtered_out_selection_drops_and_reseats_from_the_held_baseline() {
    use crate::agents::AgentStatus;
    let ws = workspace();
    let snapshot = filterable_snapshot(&ws);
    let running = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let mut ui = UiState::default();
    reconcile_selection(&mut ui, &snapshot, Some(running.clone()));
    assert_eq!(ui.selected_pane, Some(running.clone()));

    // Filtering to `failed` leaves the running highlight no row: the visible
    // pick drops to a clamped index, but the baseline — room membership, not
    // body membership — holds through every fold.
    toggle_make_up_filter(&mut ui, &snapshot, BodyFilter::Status(AgentStatus::Failed));
    assert_eq!(
        ui.make_up_filter,
        Some(BodyFilter::Status(AgentStatus::Failed))
    );
    assert_eq!(ui.selected_pane, None);
    assert_eq!(ui.selected_index, 0);
    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(ui.baseline_pane, Some(running.clone()));
    assert_eq!(ui.selected_pane, None, "the hidden highlight stays dropped");

    // Clearing the filter re-seats the highlight on the held baseline.
    toggle_make_up_filter(&mut ui, &snapshot, BodyFilter::Status(AgentStatus::Failed));
    assert_eq!(ui.make_up_filter, None);
    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(ui.selected_pane, Some(running));
    assert_eq!(ui.selected_index, 0);
}
#[test]
fn focus_jumps_keep_the_make_up_filter() {
    use crate::agents::AgentStatus;
    let ws = workspace();
    let snapshot = filterable_snapshot(&ws);
    let failed = PaneId::from_parts(MuxName::Zellij, "terminal_3");

    // A digit resolves its target in the filtered body and focuses it without
    // changing renderer-local state.
    let mut ui = UiState {
        make_up_filter: Some(BodyFilter::Status(AgentStatus::Failed)),
        ..Default::default()
    };
    let outcome = handle_key(KeyAction::Digit(1), &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::focus(failed.clone()));
    assert_eq!(
        ui.make_up_filter,
        Some(BodyFilter::Status(AgentStatus::Failed))
    );

    // Enter focuses the highlighted filtered row with the same pure effect.
    let mut ui = UiState {
        make_up_filter: Some(BodyFilter::Status(AgentStatus::Failed)),
        selected_pane: Some(failed.clone()),
        ..Default::default()
    };
    let outcome = handle_key(KeyAction::Enter, &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::focus(failed));
    assert_eq!(
        ui.make_up_filter,
        Some(BodyFilter::Status(AgentStatus::Failed))
    );
    assert_eq!(ui.selected_index, 0, "the filtered ordinal stays anchored");
}

#[test]
fn inbox_jumps_keep_the_make_up_filter_in_both_directions() {
    use crate::agents::AgentStatus;
    let ws = workspace();
    let mut snapshot = filterable_snapshot(&ws);

    snapshot.worktree_groups[0].rows[0].unread = true;
    let success = &mut snapshot.worktree_groups[0].rows[1];
    success.name = "claude".to_owned();
    success.card =
        crate::store::snapshot::RowCard::Agent(Box::new(crate::store::snapshot::AgentCard {
            status: AgentStatus::Success,
            phase: crate::agents::TurnPhase::Idle,
            ..crate::store::snapshot::AgentCard::default()
        }));
    success.unread = true;
    success.last_activity = snapshot.now - Duration::from_secs(3_600);
    snapshot.worktree_groups[1].rows[0].unread = true;
    snapshot.worktree_groups[1].rows[0].last_activity = snapshot.now - Duration::from_secs(1_800);

    let filter = Some(BodyFilter::Unread);
    let mut ui = UiState {
        selected_index: 0,
        selected_pane: Some(PaneId::from_parts(MuxName::Zellij, "terminal_1")),
        make_up_filter: filter,
        ..Default::default()
    };

    let forward = handle_key(KeyAction::InboxNext, &mut ui, &snapshot);
    assert_eq!(
        forward,
        InputOutcome::focus(PaneId::from_parts(MuxName::Zellij, "terminal_2"))
    );
    assert_eq!(ui.make_up_filter, filter);

    let backward = handle_key(KeyAction::InboxPrev, &mut ui, &snapshot);
    assert_eq!(
        backward,
        InputOutcome::focus(PaneId::from_parts(MuxName::Zellij, "terminal_3"))
    );
    assert_eq!(ui.make_up_filter, filter);
}

#[test]
fn make_up_filter_survives_repeated_row_clicks_and_keeps_frame_ordinals() {
    let ws = workspace();
    let mut snapshot = filterable_snapshot(&ws);
    snapshot.worktree_groups[0].rows[0].unread = true;
    snapshot.worktree_groups[1].rows[0].unread = true;

    let mut ui = UiState::default();
    assert_eq!(
        handle_key(
            KeyAction::Filter(Some(BodyFilter::Unread)),
            &mut ui,
            &snapshot
        ),
        InputOutcome::sync_filter(Some(BodyFilter::Unread))
    );
    let theme = ui.theme(&snapshot.theme);
    let composed = render::compose_lines(&snapshot, None, &ui, theme.as_ref(), 54, 64);
    let first_row = u16::try_from(
        composed
            .interactions
            .line_for_row(0)
            .expect("first unread row is painted"),
    )
    .unwrap();
    let second_row = u16::try_from(
        composed
            .interactions
            .line_for_row(1)
            .expect("second unread row is painted"),
    )
    .unwrap();
    ui.interactions = composed.interactions;

    let first = handle_mouse_click(0, first_row, &mut ui, &snapshot);
    assert_eq!(
        first,
        InputOutcome::focus(PaneId::from_parts(MuxName::Zellij, "terminal_1"))
    );
    assert_eq!(ui.make_up_filter, Some(BodyFilter::Unread));

    let second = handle_mouse_click(0, second_row, &mut ui, &snapshot);
    assert_eq!(
        second,
        InputOutcome::focus(PaneId::from_parts(MuxName::Zellij, "terminal_3"))
    );
    assert_eq!(ui.make_up_filter, Some(BodyFilter::Unread));
}
