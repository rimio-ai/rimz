use super::*;
use std::time::Duration;

fn assert_search_selection(ui: &UiState, snapshot: &SidebarSnapshot) {
    assert_eq!(
        ui.visible_roster(snapshot)
            .pane_at_ordinal(ui.selected_index),
        ui.selected_pane,
        "Enter must focus the highlighted match"
    );
}

fn search_press(
    ui: &mut UiState,
    snapshot: &SidebarSnapshot,
    code: ratatui::crossterm::event::KeyCode,
    mods: ratatui::crossterm::event::KeyModifiers,
) -> InputOutcome {
    super::super::super::loop_state::handle_wakeup(
        super::super::super::input::Wakeup::Press { code, mods },
        ui,
        snapshot,
        &super::super::super::NavKeymap::from_config(&crate::config::SidebarKeys {
            wider: "ctrl+b".to_owned(),
            ..Default::default()
        }),
    )
}

fn search_key(
    ui: &mut UiState,
    snapshot: &SidebarSnapshot,
    code: ratatui::crossterm::event::KeyCode,
) -> InputOutcome {
    search_press(
        ui,
        snapshot,
        code,
        ratatui::crossterm::event::KeyModifiers::NONE,
    )
}

#[test]
fn search_typing_swallows_commands_and_rebound_chords() {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    let snapshot = clickable_block_snapshot(&workspace());
    let mut ui = UiState::default();
    search_key(&mut ui, &snapshot, KeyCode::Char('/'));
    assert_eq!(ui.search_draft.as_deref(), Some(""));
    for ch in "njqr?A1é".chars() {
        let outcome = search_key(&mut ui, &snapshot, KeyCode::Char(ch));
        assert!(outcome.effects.is_empty(), "typing {ch} must not act");
    }
    search_press(
        &mut ui,
        &snapshot,
        KeyCode::Char('b'),
        KeyModifiers::CONTROL,
    );
    assert_eq!(ui.search_draft.as_deref(), Some("njqr?A1é"));
    assert!(!ui.help_visible);
    assert_eq!(
        ui.make_up_filter,
        BodyLens::default(),
        "draft is not committed"
    );
    search_key(&mut ui, &snapshot, KeyCode::Backspace);
    assert_eq!(ui.search_draft.as_deref(), Some("njqr?A1"));
}

#[test]
fn search_edits_select_first_match_and_commit_focuses_it() {
    use ratatui::crossterm::event::KeyCode;
    let snapshot = clickable_block_snapshot(&workspace());
    let mut ui = UiState {
        selected_index: 1,
        selected_pane: Some(PaneId::from_parts(MuxName::Zellij, "terminal_10")),
        manual_scroll: Some(ManualScroll {
            selection_at_start: None,
        }),
        ..Default::default()
    };
    search_key(&mut ui, &snapshot, KeyCode::Char('/'));
    for ch in "cla".chars() {
        search_key(&mut ui, &snapshot, KeyCode::Char(ch));
    }
    let top = PaneId::from_parts(MuxName::Zellij, "terminal_9");
    assert_eq!(ui.selected_pane, Some(top.clone()));
    assert_eq!(ui.selected_index, 0);
    assert_eq!(ui.manual_scroll, None);
    assert_eq!(ui.browse, None);
    assert_eq!(ui.visible_roster(&snapshot).len(), 1);
    reconcile_selection(
        &mut ui,
        &snapshot,
        Some(PaneId::from_parts(MuxName::Zellij, "terminal_10")),
    );
    assert_search_selection(&ui, &snapshot);
    assert_eq!(
        ui.selected_pane,
        Some(top.clone()),
        "fold must preserve the draft selection"
    );
    let outcome = search_key(&mut ui, &snapshot, KeyCode::Enter);
    assert_eq!(ui.search_draft, None);
    assert_eq!(
        outcome.effects,
        vec![
            InputEffect::SyncFilter(BodyLens {
                query: Some("cla".to_owned()),
                ..Default::default()
            }),
            InputEffect::Focus(top)
        ]
    );
}

#[test]
fn search_cancel_with_no_committed_query_reanchors_without_a_sync() {
    use ratatui::crossterm::event::KeyCode;
    let snapshot = clickable_block_snapshot(&workspace());
    let mut ui = UiState::default();
    let baseline = PaneId::from_parts(MuxName::Zellij, "terminal_9");
    reconcile_selection(&mut ui, &snapshot, Some(baseline.clone()));
    search_key(&mut ui, &snapshot, KeyCode::Char('/'));
    for ch in "zsh".chars() {
        search_key(&mut ui, &snapshot, KeyCode::Char(ch));
    }
    let matched = PaneId::from_parts(MuxName::Zellij, "terminal_10");
    assert_eq!(ui.selected_pane, Some(matched.clone()));
    assert_eq!(ui.selected_index, 0);

    let outcome = search_key(&mut ui, &snapshot, KeyCode::Esc);
    assert!(outcome.redraw);
    assert_eq!(
        outcome.effects,
        vec![],
        "an unchanged lens is not republished"
    );
    assert_eq!(ui.selected_pane, Some(baseline));
    assert_eq!(
        ui.selected_index, 0,
        "cancel follows the baseline like own-pane unfocus"
    );
}

#[test]
fn search_fold_removing_selected_row_reseats_enter_target() {
    use crate::agents::AgentStatus;
    use ratatui::crossterm::event::KeyCode;
    let mut snapshot = clickable_block_snapshot(&workspace());
    snapshot.worktree_groups[0].rows[1].card = snapshot.worktree_groups[0].rows[0].card.clone();
    snapshot.worktree_groups[0].status_counts[0].count = 2;
    let mut ui = UiState::default();
    search_key(&mut ui, &snapshot, KeyCode::Char('w'));
    search_key(&mut ui, &snapshot, KeyCode::Char('/'));
    for ch in "main".chars() {
        search_key(&mut ui, &snapshot, KeyCode::Char(ch));
    }
    assert_search_selection(&ui, &snapshot);

    snapshot.worktree_groups[0].rows[0]
        .as_agent_mut()
        .unwrap()
        .status = AgentStatus::Success;
    snapshot.worktree_groups[0].status_counts[0].count = 1;
    reconcile_selection(&mut ui, &snapshot, None);
    assert_search_selection(&ui, &snapshot);
    let target = PaneId::from_parts(MuxName::Zellij, "terminal_10");
    assert_eq!(ui.selected_pane, Some(target.clone()));
    let outcome = search_key(&mut ui, &snapshot, KeyCode::Enter);
    assert_eq!(outcome.effects.last(), Some(&InputEffect::Focus(target)));
}

#[test]
fn search_peer_lens_hiding_selected_row_reseats_enter_target() {
    use ratatui::crossterm::event::KeyCode;
    let mut snapshot = clickable_block_snapshot(&workspace());
    snapshot.worktree_groups[0].rows[1].unread = true;
    let mut ui = UiState::default();
    search_key(&mut ui, &snapshot, KeyCode::Char('/'));
    for ch in "main".chars() {
        search_key(&mut ui, &snapshot, KeyCode::Char(ch));
    }
    set_make_up_filter(&mut ui, &snapshot, BodyLens::from(BodyFilter::Unread));
    assert_search_selection(&ui, &snapshot);
    reconcile_selection(&mut ui, &snapshot, None);
    assert_search_selection(&ui, &snapshot);
    let target = PaneId::from_parts(MuxName::Zellij, "terminal_10");
    assert_eq!(ui.selected_pane, Some(target.clone()));
    let outcome = search_key(&mut ui, &snapshot, KeyCode::Enter);
    assert_eq!(outcome.effects.last(), Some(&InputEffect::Focus(target)));
}

#[test]
fn search_zero_match_then_fold_reseats_enter_target() {
    use ratatui::crossterm::event::KeyCode;
    let mut snapshot = clickable_block_snapshot(&workspace());
    let mut ui = UiState::default();
    search_key(&mut ui, &snapshot, KeyCode::Char('/'));
    for ch in "incoming".chars() {
        search_key(&mut ui, &snapshot, KeyCode::Char(ch));
    }
    reconcile_selection(&mut ui, &snapshot, None);
    assert_search_selection(&ui, &snapshot);
    assert_eq!(ui.selected_pane, None);
    snapshot.worktree_groups[0].rows[1].name = "incoming".to_owned();
    reconcile_selection(&mut ui, &snapshot, None);
    assert_search_selection(&ui, &snapshot);
    let target = PaneId::from_parts(MuxName::Zellij, "terminal_10");
    assert_eq!(ui.selected_pane, Some(target.clone()));
    let outcome = search_key(&mut ui, &snapshot, KeyCode::Enter);
    assert_eq!(outcome.effects.last(), Some(&InputEffect::Focus(target)));
}

#[test]
fn search_enter_focuses_navigated_match() {
    use ratatui::crossterm::event::KeyCode;
    let snapshot = clickable_block_snapshot(&workspace());
    let mut ui = UiState::default();
    search_key(&mut ui, &snapshot, KeyCode::Char('/'));
    for ch in "main".chars() {
        search_key(&mut ui, &snapshot, KeyCode::Char(ch));
    }
    search_key(&mut ui, &snapshot, KeyCode::Down);
    assert_search_selection(&ui, &snapshot);
    let outcome = search_key(&mut ui, &snapshot, KeyCode::Enter);
    assert_eq!(
        outcome.effects.last(),
        Some(&InputEffect::Focus(PaneId::from_parts(
            MuxName::Zellij,
            "terminal_10"
        )))
    );
}

#[test]
fn search_draft_composes_with_an_active_status_pick() {
    use ratatui::crossterm::event::KeyCode;
    let snapshot = filterable_snapshot(&workspace());
    let mut ui = UiState::default();
    search_key(&mut ui, &snapshot, KeyCode::Char('w'));
    search_key(&mut ui, &snapshot, KeyCode::Char('/'));
    for ch in "main".chars() {
        search_key(&mut ui, &snapshot, KeyCode::Char(ch));
    }
    assert_eq!(ui.visible_roster(&snapshot).len(), 1);
    assert_search_selection(&ui, &snapshot);
    assert_eq!(ui.make_up_filter.query, None, "draft stays local");
    assert_eq!(
        ui.selected_pane,
        Some(PaneId::from_parts(MuxName::Zellij, "terminal_1"))
    );
}

#[test]
fn pick_click_toggle_preserves_committed_query() {
    use crate::agents::AgentStatus;
    let snapshot = filterable_snapshot(&workspace());
    let mut ui = UiState {
        make_up_filter: BodyLens {
            query: Some("main".to_owned()),
            ..Default::default()
        },
        interactions: render::FrameInteractions::from_parts(
            vec![None],
            vec![render::HitRegion::line(
                0,
                0..3,
                HitTarget::BodyFilter(BodyFilter::Status(AgentStatus::Running)),
            )],
        ),
        ..Default::default()
    };
    for filter in [Some(BodyFilter::Status(AgentStatus::Running)), None] {
        let outcome = handle_mouse_click(1, 0, &mut ui, &snapshot);
        let lens = BodyLens {
            filter,
            query: Some("main".to_owned()),
        };
        assert_eq!(ui.make_up_filter, lens);
        assert_eq!(outcome.effects, vec![InputEffect::SyncFilter(lens)]);
    }
}

#[test]
fn search_cancel_and_empty_enter_clear_only_query() {
    use ratatui::crossterm::event::KeyCode;
    let snapshot = clickable_block_snapshot(&workspace());
    for exit in [KeyCode::Esc, KeyCode::Backspace, KeyCode::Enter] {
        let mut ui = UiState {
            make_up_filter: BodyLens {
                filter: Some(BodyFilter::Status(crate::agents::AgentStatus::Running)),
                query: Some("cla".to_owned()),
            },
            ..Default::default()
        };
        search_key(&mut ui, &snapshot, KeyCode::Char('/'));
        assert_eq!(ui.search_draft.as_deref(), Some("cla"));
        if exit != KeyCode::Esc {
            for _ in 0..3 {
                search_key(&mut ui, &snapshot, KeyCode::Backspace);
            }
        }
        let outcome = search_key(&mut ui, &snapshot, exit);
        assert_eq!(ui.search_draft, None);
        assert_eq!(ui.make_up_filter.query, None);
        assert_eq!(
            outcome.effects,
            vec![InputEffect::SyncFilter(BodyLens::from(BodyFilter::Status(
                crate::agents::AgentStatus::Running
            )))]
        );
    }
}

#[test]
fn search_navigation_click_scroll_and_zero_match_commit() {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    let snapshot = clickable_block_snapshot(&workspace());
    let mut ui = UiState::default();
    search_key(&mut ui, &snapshot, KeyCode::Char('/'));
    for code in [KeyCode::Down, KeyCode::Char('n')] {
        search_press(
            &mut ui,
            &snapshot,
            code,
            if code == KeyCode::Down {
                KeyModifiers::NONE
            } else {
                KeyModifiers::CONTROL
            },
        );
        assert_eq!(ui.selected_index, 1);
        search_press(
            &mut ui,
            &snapshot,
            KeyCode::Char('p'),
            KeyModifiers::CONTROL,
        );
        assert_eq!(ui.selected_index, 0);
    }
    search_key(&mut ui, &snapshot, KeyCode::Down);
    search_key(&mut ui, &snapshot, KeyCode::Up);
    assert_eq!(ui.selected_index, 0);
    assert_eq!(
        search_key(&mut ui, &snapshot, KeyCode::Left),
        InputOutcome::default()
    );
    search_key(&mut ui, &snapshot, KeyCode::Char('c'));
    super::super::super::loop_state::handle_wakeup(
        super::super::super::input::Wakeup::Scroll { down: true },
        &mut ui,
        &snapshot,
        &super::super::super::NavKeymap::from_config(&crate::config::SidebarKeys {
            wider: "ctrl+b".to_owned(),
            ..Default::default()
        }),
    );
    assert_eq!(ui.search_draft.as_deref(), Some("c"));
    ui.interactions = render::FrameInteractions::from_parts(vec![Some(0)], Vec::new());
    let outcome = super::super::super::loop_state::handle_wakeup(
        super::super::super::input::Wakeup::MouseClick { column: 1, row: 0 },
        &mut ui,
        &snapshot,
        &super::super::super::NavKeymap::from_config(&crate::config::SidebarKeys {
            wider: "ctrl+b".to_owned(),
            ..Default::default()
        }),
    );
    assert_eq!(ui.search_draft, None);
    assert_eq!(
        outcome.effects,
        vec![
            InputEffect::SyncFilter(BodyLens {
                query: Some("c".to_owned()),
                ..Default::default()
            }),
            InputEffect::Focus(PaneId::from_parts(MuxName::Zellij, "terminal_9"))
        ]
    );
    search_key(&mut ui, &snapshot, KeyCode::Char('/'));
    search_key(&mut ui, &snapshot, KeyCode::Char('!'));
    let outcome = search_key(&mut ui, &snapshot, KeyCode::Enter);
    assert_eq!(ui.make_up_filter.query.as_deref(), Some("c!"));
    assert_eq!(
        outcome.effects.len(),
        1,
        "zero matches commits without focus"
    );
}

#[test]
fn search_composes_with_status_and_n_keeps_needs_you_meaning() {
    use ratatui::crossterm::event::KeyCode;
    let mut snapshot = clickable_block_snapshot(&workspace());
    let mut waiting = snapshot.worktree_groups[0].rows[0].clone();
    waiting.id = "waiting".to_owned();
    waiting.pane = Some(pane("terminal_11", "tab_0", false));
    waiting.as_agent_mut().unwrap().status = crate::agents::AgentStatus::Waiting;
    let mut outside = waiting.clone();
    outside.id = "outside".to_owned();
    outside.name = "other".to_owned();
    outside.pane = Some(pane("terminal_12", "tab_0", false));
    snapshot.worktree_groups[0].rows.extend([waiting, outside]);
    let mut ui = UiState::default();
    search_key(&mut ui, &snapshot, KeyCode::Char('/'));
    for ch in "cla".chars() {
        search_key(&mut ui, &snapshot, KeyCode::Char(ch));
    }
    search_key(&mut ui, &snapshot, KeyCode::Enter);
    assert_eq!(ui.make_up_filter.query.as_deref(), Some("cla"));
    assert_eq!(
        search_key(&mut ui, &snapshot, KeyCode::Char('n')),
        InputOutcome::focus(PaneId::from_parts(MuxName::Zellij, "terminal_11"))
    );
    search_key(&mut ui, &snapshot, KeyCode::Char('w'));
    assert_eq!(ui.visible_roster(&snapshot).len(), 1);
    assert_eq!(ui.make_up_filter.query.as_deref(), Some("cla"));
    search_key(&mut ui, &snapshot, KeyCode::Char('A'));
    assert_eq!(ui.make_up_filter, BodyLens::default());
}

#[test]
fn show_all_clears_both_pick_and_query() {
    let snapshot = filterable_snapshot(&workspace());
    let mut ui = UiState {
        make_up_filter: BodyLens {
            filter: Some(BodyFilter::Unread),
            query: Some("auth".to_owned()),
        },
        ..Default::default()
    };
    let outcome = handle_key(KeyAction::Filter(None), &mut ui, &snapshot);
    assert_eq!(ui.make_up_filter, BodyLens::default());
    assert_eq!(
        outcome.effects,
        vec![InputEffect::SyncFilter(BodyLens::default())]
    );
}

#[test]
fn empty_pick_auto_clear_preserves_the_query() {
    let snapshot = filterable_snapshot(&workspace());
    let mut ui = UiState {
        make_up_filter: BodyLens {
            filter: Some(BodyFilter::Status(crate::agents::AgentStatus::Waiting)),
            query: Some("auth".to_owned()),
        },
        ..Default::default()
    };
    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(ui.make_up_filter.filter, None);
    assert_eq!(ui.make_up_filter.query.as_deref(), Some("auth"));
}

#[test]
fn zero_match_query_stays_set_with_an_empty_roster() {
    let snapshot = filterable_snapshot(&workspace());
    let mut ui = UiState {
        make_up_filter: BodyLens {
            query: Some("no such row".to_owned()),
            ..Default::default()
        },
        ..Default::default()
    };
    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(ui.visible_roster(&snapshot).len(), 0);
    assert_eq!(ui.make_up_filter.query.as_deref(), Some("no such row"));
}

#[test]
fn worktree_keys_respect_the_make_up_filter() {
    use crate::agents::AgentStatus;
    let ws = workspace();
    let snapshot = filterable_snapshot(&ws);
    let failed = PaneId::from_parts(MuxName::Zellij, "terminal_3");
    let mut ui = UiState {
        selected_index: 0,
        selected_pane: Some(failed),
        make_up_filter: BodyLens::from(BodyFilter::Status(AgentStatus::Failed)),
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
        BodyLens::from(BodyFilter::Status(AgentStatus::Failed))
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
            effects: vec![InputEffect::SyncFilter(BodyLens::from(BodyFilter::Status(
                AgentStatus::Failed,
            )))],
        }
    );
    assert_eq!(
        ui.make_up_filter,
        BodyLens::from(BodyFilter::Status(AgentStatus::Failed))
    );

    // A click on another bucket switches the pick in place.
    handle_mouse_click(28, 5, &mut ui, &snapshot);
    assert_eq!(
        ui.make_up_filter,
        BodyLens::from(BodyFilter::Status(AgentStatus::Running))
    );

    // A second click on the active bucket clears back to show-all.
    handle_mouse_click(28, 5, &mut ui, &snapshot);
    assert_eq!(ui.make_up_filter, BodyLens::default());

    // The hit range is half-open: the cell past the bucket falls through to
    // the row hit-test (and lands nowhere on this chrome line).
    let outcome = handle_mouse_click(8, 5, &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::default());
    assert_eq!(ui.make_up_filter, BodyLens::default());
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
        InputOutcome::sync_filter(BodyLens::from(BodyFilter::Status(AgentStatus::Failed)))
    );
    assert_eq!(
        ui.make_up_filter,
        BodyLens::from(BodyFilter::Status(AgentStatus::Failed))
    );

    let outcome = handle_key(
        KeyAction::Filter(Some(BodyFilter::Status(AgentStatus::Failed))),
        &mut ui,
        &snapshot,
    );
    assert_eq!(outcome, InputOutcome::sync_filter(BodyLens::default()));
    assert_eq!(
        ui.make_up_filter,
        BodyLens::default(),
        "the active key toggles to all"
    );

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
    assert_eq!(ui.make_up_filter, BodyLens::default());

    handle_key(
        KeyAction::Filter(Some(BodyFilter::Status(AgentStatus::Running))),
        &mut ui,
        &snapshot,
    );
    assert_eq!(
        ui.make_up_filter,
        BodyLens::from(BodyFilter::Status(AgentStatus::Running))
    );

    let outcome = handle_key(KeyAction::Filter(None), &mut ui, &snapshot);
    assert_eq!(
        outcome.effects,
        vec![InputEffect::SyncFilter(BodyLens::default())]
    );
    assert_eq!(ui.make_up_filter, BodyLens::default());

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
        outcome.effects,
        vec![InputEffect::SyncFilter(BodyLens::from(BodyFilter::Status(
            AgentStatus::Failed,
        )))]
    );
    assert_eq!(
        ui.make_up_filter,
        BodyLens::from(BodyFilter::Status(AgentStatus::Failed))
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
    assert_eq!(
        outcome,
        InputOutcome::sync_filter(BodyLens::from(BodyFilter::Unread))
    );
    assert_eq!(ui.make_up_filter, BodyLens::from(BodyFilter::Unread));
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
    assert_eq!(outcome, InputOutcome::sync_filter(BodyLens::default()));
    assert_eq!(ui.make_up_filter, BodyLens::default());

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
    assert_eq!(
        outcome,
        InputOutcome::sync_filter(BodyLens::from(BodyFilter::OpenPr))
    );
    assert_eq!(ui.make_up_filter, BodyLens::from(BodyFilter::OpenPr));

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
    assert_eq!(outcome, InputOutcome::sync_filter(BodyLens::default()));
    assert_eq!(ui.make_up_filter, BodyLens::default());

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
        make_up_filter: BodyLens::from(BodyFilter::Status(AgentStatus::Waiting)),
        ..Default::default()
    };
    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(ui.make_up_filter, BodyLens::default());

    // A filter whose bucket still counts holds through the fold.
    ui.make_up_filter = BodyLens::from(BodyFilter::Status(AgentStatus::Failed));
    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(
        ui.make_up_filter,
        BodyLens::from(BodyFilter::Status(AgentStatus::Failed))
    );

    ui.make_up_filter = BodyLens::from(BodyFilter::OpenPr);
    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(
        ui.make_up_filter,
        BodyLens::default(),
        "a stale open PR lens clears after its last PR resolves"
    );
}
#[test]
fn make_up_filter_narrows_ordinals_in_lockstep_with_frame_targets() {
    use crate::agents::AgentStatus;
    let ws = workspace();
    let snapshot = filterable_snapshot(&ws);
    let filter = BodyLens::from(BodyFilter::Status(AgentStatus::Failed));

    // The selection walk and the rendered line map share one predicate, so
    // their ordinals can never drift: the filtered universe is exactly the
    // contiguous 0..count the body's hit-test entries carry.
    assert_eq!(
        roster_len(&snapshot, &BodyLens::default(), &Default::default()),
        3
    );
    assert_eq!(roster_len(&snapshot, &filter, &Default::default()), 1);
    let failed = PaneId::from_parts(MuxName::Zellij, "terminal_3");
    let running = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    assert_eq!(row_index_of_pane(&snapshot, &filter, &failed), Some(0));
    assert_eq!(row_index_of_pane(&snapshot, &filter, &running), None);
    assert_eq!(
        row_index_of_pane(&snapshot, &BodyLens::default(), &failed),
        Some(2)
    );

    let mut ui = UiState {
        make_up_filter: filter.clone(),
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
        (0..roster_len(&snapshot, &filter, &Default::default())).collect::<Vec<_>>(),
        "the line map carries exactly the filtered walk's ordinals"
    );
}
#[test]
fn next_attention_jump_respects_the_filter() {
    use crate::agents::AgentStatus;
    let ws = workspace();
    let snapshot = filterable_snapshot(&ws);

    // Unfiltered, `␣` finds the failed row at its body ordinal.
    assert_eq!(
        next_attention_index(&snapshot, &BodyLens::default(), 0),
        Some(2)
    );
    // Filtered to a calm status, the universe holds nothing actionable.
    assert_eq!(
        next_attention_index(
            &snapshot,
            &BodyLens::from(BodyFilter::Status(AgentStatus::Running)),
            0
        ),
        None
    );
    // Filtered to the attention status, the jump cycles the filtered rows.
    assert_eq!(
        next_attention_index(
            &snapshot,
            &BodyLens::from(BodyFilter::Status(AgentStatus::Failed)),
            0
        ),
        Some(0)
    );
}

#[test]
fn unread_filter_narrows_to_unread_rows() {
    let ws = workspace();
    let mut snapshot = filterable_snapshot(&ws);
    snapshot.worktree_groups[0].rows[0].unread = true;
    snapshot.worktree_groups[1].rows[0].unread = true;
    let filter = BodyLens::from(BodyFilter::Unread);

    assert_eq!(
        roster_len(&snapshot, &BodyLens::default(), &Default::default()),
        3
    );
    assert_eq!(roster_len(&snapshot, &filter, &Default::default()), 2);

    let mut ui = UiState::default();
    let outcome = handle_key(
        KeyAction::Filter(Some(BodyFilter::Unread)),
        &mut ui,
        &snapshot,
    );
    assert_eq!(outcome, InputOutcome::sync_filter(filter.clone()));
    assert_eq!(ui.make_up_filter, filter);

    let failed = PaneId::from_parts(MuxName::Zellij, "terminal_3");
    let running = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    assert_eq!(row_index_of_pane(&snapshot, &filter, &running), Some(0));
    assert_eq!(row_index_of_pane(&snapshot, &filter, &failed), Some(1));
    assert_eq!(
        next_attention_index(&snapshot, &filter, 0),
        Some(1),
        "only the unread failed row is an unread needs-a-look target"
    );

    let outcome = handle_key(
        KeyAction::Filter(Some(BodyFilter::Unread)),
        &mut ui,
        &snapshot,
    );
    assert_eq!(outcome, InputOutcome::sync_filter(BodyLens::default()));
    assert_eq!(ui.make_up_filter, BodyLens::default());

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
    assert_eq!(ui.make_up_filter, BodyLens::default());
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
        next_attention_index(&snapshot, &BodyLens::default(), 0),
        Some(1),
        "unread calm/running rows are filtered out, unread needs-a-look rows lead"
    );
    assert_eq!(
        next_attention_index(&snapshot, &BodyLens::default(), 1),
        Some(2),
        "read actionable rows follow after unread episodes"
    );
    assert_eq!(
        next_attention_index(&snapshot, &BodyLens::default(), 2),
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
        next_attention_index(&snapshot, &BodyLens::default(), 0),
        Some(1),
        "oldest unread episode leads when the selection is outside the triage list"
    );
    assert_eq!(
        next_attention_index(&snapshot, &BodyLens::default(), 1),
        Some(3),
        "paused unread episodes stay in the unread pass"
    );
    assert_eq!(
        next_attention_index(&snapshot, &BodyLens::default(), 3),
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
        step_attention_index(
            &snapshot,
            &BodyLens::default(),
            &Default::default(),
            1,
            false
        ),
        Some(2),
        "reverse from the first candidate wraps to the last"
    );
    assert_eq!(
        step_attention_index(
            &snapshot,
            &BodyLens::default(),
            &Default::default(),
            2,
            false
        ),
        Some(1),
        "reverse steps to the previous candidate"
    );
    assert_eq!(
        step_attention_index(
            &snapshot,
            &BodyLens::default(),
            &Default::default(),
            0,
            false
        ),
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
        BodyLens::from(BodyFilter::Status(AgentStatus::Failed))
    );
    assert_eq!(ui.selected_pane, None);
    assert_eq!(ui.selected_index, 0);
    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(ui.baseline_pane, Some(running.clone()));
    assert_eq!(ui.selected_pane, None, "the hidden highlight stays dropped");

    // Clearing the filter re-seats the highlight on the held baseline.
    toggle_make_up_filter(&mut ui, &snapshot, BodyFilter::Status(AgentStatus::Failed));
    assert_eq!(ui.make_up_filter, BodyLens::default());
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
        make_up_filter: BodyLens::from(BodyFilter::Status(AgentStatus::Failed)),
        ..Default::default()
    };
    let outcome = handle_key(KeyAction::Digit(1), &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::focus(failed.clone()));
    assert_eq!(
        ui.make_up_filter,
        BodyLens::from(BodyFilter::Status(AgentStatus::Failed))
    );

    // Enter focuses the highlighted filtered row with the same pure effect.
    let mut ui = UiState {
        make_up_filter: BodyLens::from(BodyFilter::Status(AgentStatus::Failed)),
        selected_pane: Some(failed.clone()),
        ..Default::default()
    };
    let outcome = handle_key(KeyAction::Enter, &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::focus(failed));
    assert_eq!(
        ui.make_up_filter,
        BodyLens::from(BodyFilter::Status(AgentStatus::Failed))
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

    let filter = BodyLens::from(BodyFilter::Unread);
    let mut ui = UiState {
        selected_index: 0,
        selected_pane: Some(PaneId::from_parts(MuxName::Zellij, "terminal_1")),
        make_up_filter: filter.clone(),
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
        InputOutcome::sync_filter(BodyLens::from(BodyFilter::Unread))
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
    assert_eq!(ui.make_up_filter, BodyLens::from(BodyFilter::Unread));

    let second = handle_mouse_click(0, second_row, &mut ui, &snapshot);
    assert_eq!(
        second,
        InputOutcome::focus(PaneId::from_parts(MuxName::Zellij, "terminal_3"))
    );
    assert_eq!(ui.make_up_filter, BodyLens::from(BodyFilter::Unread));
}
