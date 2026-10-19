use super::*;
use std::sync::Arc;

fn delegation_fleet(parents: &[usize]) -> SidebarSnapshot {
    let now = fixed_now();
    let mut agents = (0..12)
        .map(|i| {
            let mut card = agent(
                &format!("codex-{i}"),
                "codex",
                AgentStatus::Running,
                Some("/repo/alpha"),
                Some("alpha"),
                Some(&format!("task-{i}")),
            );
            card.last_activity = now - Duration::from_secs(8);
            card
        })
        .collect::<Vec<_>>();
    for parent in parents {
        for i in 0..4_u64 {
            let mut child = agent(
                &format!("child-{parent}-{i}"),
                "codex",
                AgentStatus::Running,
                None,
                None,
                Some("Explore"),
            );
            child.parent_agent_id = Some(format!("codex-{parent}").into());
            child.subagent_description = Some(format!("survey area {parent}-{i}"));
            child.subagent_started_at = Some(now - Duration::from_secs(240 - i * 30));
            child.last_activity = now;
            agents.push(child);
        }
    }
    snapshot_with(agents)
}

fn select_card(snapshot: &SidebarSnapshot, ui: &mut UiState, id: &str) {
    ui.freeze = Some(LayoutFreeze {
        frozen: FrozenOrder::default(),
        armed_ms: fixed_now().as_millisecond(),
    });
    ui.selected_index = ui.visible_roster(snapshot).ordinal_of_id(id);
    ui.own_input_reveal = true;
    assert!(ui.selected_index.is_some(), "selection exists");
}

#[test]
fn shared_placement_preserves_the_sources_expanded_selection() {
    let snapshot = delegation_fleet(&[0]);
    let mut source = selected_ui();
    let before = paint_card_row(&snapshot, &mut source, "task-1", 45);
    let mut target = UiState::default();
    select_card(&snapshot, &mut target, "codex-1");
    target.focus_placement = Some(
        serde_json::from_value(serde_json::json!({
            "row_id": "codex-1",
            "viewport_row": before as isize - source.scroll_zone_top as isize,
            "held": ["codex-0", "codex-1", "departed"],
        }))
        .unwrap(),
    );

    assert_eq!(paint_card_row(&snapshot, &mut target, "task-1", 45), before);
    assert_eq!(target.held_expansions, ["codex-0".into()].into());
}

#[test]
fn shared_placement_replaces_the_destinations_previous_selection() {
    let mut snapshot = delegation_fleet(&[2]);
    Arc::make_mut(&mut Arc::make_mut(&mut snapshot.worktree_groups)[0].rows).truncate(4);
    let mut source = selected_ui();
    let before = paint_card_row(&snapshot, &mut source, "task-3", 45);
    let mut target = UiState::default();
    select_card(&snapshot, &mut target, "codex-2");
    paint_card_row(&snapshot, &mut target, "task-3", 45);
    select_card(&snapshot, &mut target, "codex-3");
    target.focus_placement = Some(
        serde_json::from_value(serde_json::json!({
            "row_id": "codex-3",
            "viewport_row": before as isize - source.scroll_zone_top as isize,
            "held": ["codex-0"],
        }))
        .unwrap(),
    );

    assert_eq!(paint_card_row(&snapshot, &mut target, "task-3", 45), before);
    assert!(!target.held_expansions.contains("codex-2"));
}

#[test]
fn shared_placement_with_no_opening_set_overrides_local_holds() {
    let snapshot = delegation_fleet(&[0]);
    let mut target = selected_ui();
    paint_card_row(&snapshot, &mut target, "task-1", 45);
    select_card(&snapshot, &mut target, "codex-1");
    let held_row = paint_card_row(&snapshot, &mut target, "task-1", 45);
    assert!(!target.held_expansions.is_empty());
    target.focus_placement = Some(
        serde_json::from_value(serde_json::json!({
            "row_id": "codex-1",
            "viewport_row": held_row as isize - target.scroll_zone_top as isize,
            "held": [],
        }))
        .unwrap(),
    );

    paint_card_row(&snapshot, &mut target, "task-1", 45);
    assert!(target.held_expansions.is_empty());
}

fn assert_cache_notice_anchor(
    density: crate::config::CardDensityMode,
    offset: usize,
    expired_cache: bool,
    count: usize,
    target: usize,
) {
    let mut snapshot = delegation_fleet(&[]);
    Arc::make_mut(&mut Arc::make_mut(&mut snapshot.worktree_groups)[0].rows).truncate(count);
    snapshot.theme.display.card_density = density;
    let row = snapshot.rows_mut().find(|row| row.id == "codex-0").unwrap();
    row.last_activity = fixed_now() - Duration::from_secs(1801);
    row.as_agent_mut().unwrap().cache =
        expired_cache.then_some(crate::store::snapshot::CacheClock {
            ceiling_secs: 1800,
            last_request_at: Some(fixed_now() - Duration::from_secs(1801)),
            warm_until: Some(fixed_now() - Duration::from_secs(1)),
            held_since: None,
            rewarm_cost_usd: None,
        });
    let mut ui = UiState {
        scroll_offset: offset,
        ..Default::default()
    };
    select_card(&snapshot, &mut ui, "codex-0");
    let task = format!("task-{target}");
    let before = paint_card_row(&snapshot, &mut ui, &task, 45);
    let screen = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, 54, 45);
    assert_eq!(screen.contains("cache expired"), expired_cache, "{screen}");

    select_card(&snapshot, &mut ui, &format!("codex-{target}"));
    let after = paint_card_row(&snapshot, &mut ui, &task, 45);
    assert_eq!(after, before, "cache notice above must not move the click");
    let screen = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, 54, 45);
    let keeps_stats = true;
    assert_eq!(
        screen.contains("cache expired"),
        expired_cache && keeps_stats,
        "cache notice follows the old card's stats: {screen}"
    );
    assert!(line_containing(&screen, &task).starts_with('▌'));
    if expired_cache && keeps_stats {
        assert!(!line_containing(&screen, "cache expired").starts_with('▌'));
    }
}

#[test]
fn cache_notice_anchor_auto_top() {
    assert_cache_notice_anchor(crate::config::CardDensityMode::Auto, 0, true, 12, 2);
}

#[test]
fn cache_notice_anchor_auto_with_room() {
    assert_cache_notice_anchor(crate::config::CardDensityMode::Auto, 2, true, 12, 2);
}

#[test]
fn cache_notice_anchor_expanded_top() {
    assert_cache_notice_anchor(crate::config::CardDensityMode::Expanded, 0, true, 12, 2);
}

#[test]
fn cache_notice_anchor_expanded_with_room() {
    assert_cache_notice_anchor(crate::config::CardDensityMode::Expanded, 2, true, 12, 2);
}

#[test]
fn cache_notice_anchor_auto_top_without_cache() {
    assert_cache_notice_anchor(crate::config::CardDensityMode::Auto, 0, false, 12, 2);
}

#[test]
fn cache_notice_anchor_expanded_top_without_cache() {
    assert_cache_notice_anchor(crate::config::CardDensityMode::Expanded, 0, false, 12, 2);
}

#[test]
fn cache_notice_anchor_two_cards() {
    assert_cache_notice_anchor(crate::config::CardDensityMode::Auto, 0, true, 2, 1);
}

#[test]
fn cache_notice_anchor_two_cards_without_cache() {
    assert_cache_notice_anchor(crate::config::CardDensityMode::Auto, 0, false, 2, 1);
}

fn assert_collapse_hold(offset: usize, parent: usize, target: usize) {
    let snapshot = delegation_fleet(&[parent]);
    let mut ui = UiState {
        scroll_offset: offset,
        ..Default::default()
    };
    select_card(&snapshot, &mut ui, &format!("codex-{parent}"));
    let task = format!("task-{target}");
    let before = paint_card_row(&snapshot, &mut ui, &task, 45);
    select_card(&snapshot, &mut ui, &format!("codex-{target}"));
    let after = paint_card_row(&snapshot, &mut ui, &task, 45);
    assert_eq!(
        after, before,
        "previous selection's collapse must not move the click"
    );
    assert!(!ui.anchor_clamped);
    let screen = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, 54, 45);
    assert!(
        screen.contains("survey area"),
        "held entries remain painted: {screen}"
    );
    assert!(!line_containing(&screen, "survey area").starts_with('▌'));
    assert!(line_containing(&screen, &task).starts_with('▌'));
}

#[test]
fn collapse_above_at_zero_holds_the_previous_cards_stats() {
    let snapshot = delegation_fleet(&[]);
    let mut ui = selected_ui();
    let before = paint_card_row(&snapshot, &mut ui, "task-2", 45);
    select_card(&snapshot, &mut ui, "codex-2");
    assert_eq!(paint_card_row(&snapshot, &mut ui, "task-2", 45), before);
    let screen = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, 54, 45);
    assert_eq!(
        screen.matches('▤').count(),
        2,
        "only live and held selections keep stats: {screen}"
    );
    assert!(ui.held_expansions.contains("codex-0"));
    assert!(
        screen.lines().any(|line| line.starts_with("▎  ▤")),
        "the held card keeps its stats: {screen}"
    );
}

#[test]
fn collapse_above_at_zero_keeps_clicked_card_and_unselected_entries() {
    assert_collapse_hold(0, 0, 2);
}

#[test]
fn collapse_above_with_partial_room_keeps_clicked_card() {
    assert_collapse_hold(3, 0, 2);
}

#[test]
fn collapse_below_at_bottom_keeps_clicked_card() {
    assert_collapse_hold(1000, 11, 9);
}

#[test]
fn collapse_with_room_keeps_entries_until_freeze_expires() {
    let snapshot = delegation_fleet(&[0]);
    let mut ui = UiState {
        scroll_offset: 10,
        ..Default::default()
    };
    select_card(&snapshot, &mut ui, "codex-0");
    let before = paint_card_row(&snapshot, &mut ui, "task-2", 45);
    select_card(&snapshot, &mut ui, "codex-2");
    assert_eq!(paint_card_row(&snapshot, &mut ui, "task-2", 45), before);
    assert!(!ui.held_expansions.is_empty());
    assert!(
        snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, 54, 45).contains("survey area")
    );
    ui.freeze = None;
    paint_card_row(&snapshot, &mut ui, "task-2", 45);
    assert!(ui.held_expansions.is_empty());
    assert!(
        !snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, 54, 45).contains("survey area")
    );
}

#[test]
fn authoritative_wheel_paint_keeps_a_frozen_expansion() {
    let snapshot = delegation_fleet(&[0]);
    let mut ui = selected_ui();
    let before = paint_card_row(&snapshot, &mut ui, "task-2", 45);
    select_card(&snapshot, &mut ui, "codex-2");
    assert_eq!(paint_card_row(&snapshot, &mut ui, "task-2", 45), before);
    ui.scroll_offset += 3;
    ui.offset_authoritative = true;
    ui.scroll_ease.arm();
    paint_card_row(&snapshot, &mut ui, "task-2", 45);
    assert!(!ui.held_expansions.is_empty());
    assert!(
        snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, 54, 45).contains("survey area")
    );
    ui.freeze = None;
    paint_card_row(&snapshot, &mut ui, "task-2", 45);
    assert!(ui.held_expansions.is_empty());
    assert!(
        !snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, 54, 45).contains("survey area")
    );
}

#[test]
fn growth_above_keeps_expansions_until_freeze_expires() {
    let snapshot = delegation_fleet(&[0]);
    let mut ui = selected_ui();
    ui.delegation_overrides.insert("codex-1".into(), true);
    let before = paint_card_row(&snapshot, &mut ui, "task-2", 45);
    select_card(&snapshot, &mut ui, "codex-2");
    assert_eq!(paint_card_row(&snapshot, &mut ui, "task-2", 45), before);
    assert_eq!(ui.held_expansions, ["codex-0".into()].into());
    assert!(
        snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, 54, 45)
            .contains("survey area 0-")
    );

    // The already-open card above gains children, making room to collapse.
    let grown = delegation_fleet(&[0, 1]);
    assert!(!ui.offset_authoritative);
    assert_eq!(paint_card_row(&grown, &mut ui, "task-2", 45), before);
    assert!(!ui.anchor_clamped);
    assert!(!ui.held_expansions.is_empty());
    let screen = snapshot_to_screen_with_alert_and_ui(&grown, None, &ui, 54, 45);
    assert!(screen.contains("survey area 0-"));
    assert!(screen.contains("survey area 1-"));
    ui.freeze = None;
    paint_card_row(&grown, &mut ui, "task-2", 45);
    assert!(ui.held_expansions.is_empty());
    assert!(
        !snapshot_to_screen_with_alert_and_ui(&grown, None, &ui, 54, 45).contains("survey area 0-")
    );
}

#[test]
fn collapse_holds_accumulate_across_clicks_at_the_top() {
    let snapshot = delegation_fleet(&[0, 1]);
    let mut ui = selected_ui();
    paint_card_row(&snapshot, &mut ui, "task-1", 45);
    select_card(&snapshot, &mut ui, "codex-1");
    let before = paint_card_row(&snapshot, &mut ui, "task-2", 45);
    select_card(&snapshot, &mut ui, "codex-2");
    assert_eq!(paint_card_row(&snapshot, &mut ui, "task-2", 45), before);
    assert_eq!(ui.held_expansions.len(), 2);
}

#[test]
fn compact_collapse_at_zero_keeps_the_clicked_card_shape() {
    let mut snapshot = overflowing_fleet();
    snapshot.theme.display.card_density = crate::config::CardDensityMode::Compact;
    let mut ui = selected_ui();
    let before = paint_card_row(&snapshot, &mut ui, "task-2", 30);
    select_card(&snapshot, &mut ui, &snapshot.rows().nth(2).unwrap().id);
    assert_eq!(paint_card_row(&snapshot, &mut ui, "task-2", 30), before);
    assert!(!ui.anchor_clamped);
}

#[test]
fn offscreen_selection_keeps_the_previous_card_and_reveals() {
    let snapshot = delegation_fleet(&[0]);
    let mut ui = selected_ui();
    paint_card_row(&snapshot, &mut ui, "task-2", 45);
    select_card(&snapshot, &mut ui, "codex-11");
    paint_card_row(&snapshot, &mut ui, "task-11", 45);
    assert!(!ui.held_expansions.is_empty());
    let theme = Theme::fixed(false);
    let composed = compose_lines(&snapshot, None, &ui, &theme, 54, 200);
    assert!(
        composed
            .lines
            .iter()
            .any(|line| line.to_string().contains("survey area"))
    );
    ui.freeze = None;
    paint_card_row(&snapshot, &mut ui, "task-11", 45);
    assert!(ui.held_expansions.is_empty());
    let composed = compose_lines(&snapshot, None, &ui, &theme, 54, 200);
    assert!(
        !composed
            .lines
            .iter()
            .any(|line| line.to_string().contains("survey area"))
    );
}

#[test]
fn collapse_hold_drops_departed_and_live_selection_ids() {
    let snapshot = delegation_fleet(&[0]);
    let mut ui = selected_ui();
    let before = paint_card_row(&snapshot, &mut ui, "task-2", 45);
    select_card(&snapshot, &mut ui, "codex-2");
    ui.held_expansions = ["codex-2".into(), "departed".into()].into();
    assert_eq!(paint_card_row(&snapshot, &mut ui, "task-2", 45), before);
    assert!(!ui.anchor_clamped);
    assert_eq!(ui.held_expansions, ["codex-0".into()].into());
}

fn paint_card_row(snapshot: &SidebarSnapshot, ui: &mut UiState, task: &str, height: u16) -> usize {
    let mut terminal = Terminal::new(TestBackend::new(54, height)).unwrap();
    draw_to_terminal(&mut terminal, snapshot, None, ui).unwrap();
    (0..height)
        .find(|&y| {
            let text: String = (0..54)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect();
            text.contains(task)
        })
        .expect("card is painted") as usize
        - 1
}

#[test]
fn accordion_selection_keeps_the_clicked_card_row_with_room_above() {
    let mut snapshot = overflowing_fleet();
    snapshot.theme.display.card_density = crate::config::CardDensityMode::Compact;
    let mut ui = UiState {
        selected_index: Some(0),
        scroll_offset: 2,
        ..Default::default()
    };
    let before = paint_card_row(&snapshot, &mut ui, "task-2", 30);
    ui.selected_index = Some(2);
    let after = paint_card_row(&snapshot, &mut ui, "task-2", 30);
    assert_eq!(after, before, "collapse above is absorbed by the offset");
}

#[test]
fn stale_card_selection_and_threshold_crossing_keep_the_selected_row() {
    let mut snapshot = overflowing_fleet();
    for (index, row) in snapshot.rows_mut().enumerate() {
        row.as_agent_mut().unwrap().status = AgentStatus::Success;
        row.as_agent_mut().unwrap().handle = Some(format!("card-{index}"));
        row.last_activity = fixed_now() - Duration::from_secs(86401);
        row.unread = false;
    }
    let mut ui = UiState {
        selected_index: Some(0),
        scroll_offset: 2,
        ..Default::default()
    };
    let before = paint_card_row(&snapshot, &mut ui, "card-2", 30);
    let old = snapshot.rows().nth(2).unwrap();
    let stale_line = group_lines(&snapshot, &Theme::fixed(false), 0)
        .into_iter()
        .find(|line| line.to_string().contains(old.display_name()))
        .unwrap();
    assert!(
        stale_line.to_string().contains("task-2"),
        "the unselected card has shrunk"
    );
    select_card(&snapshot, &mut ui, &old.id);
    assert_eq!(paint_card_row(&snapshot, &mut ui, "card-2", 30), before);

    snapshot.rows_mut().next().unwrap().last_activity = fixed_now() - Duration::from_secs(86400);
    let before = paint_card_row(&snapshot, &mut ui, "card-2", 30);
    snapshot.now += Duration::from_secs(1);
    assert_eq!(
        paint_card_row(&snapshot, &mut ui, "card-2", 30),
        before,
        "shrinking above preserves the selection"
    );
}

#[test]
fn growth_above_an_unchanged_selection_keeps_its_row() {
    let mut snapshot = overflowing_fleet();
    snapshot.theme.display.card_density = crate::config::CardDensityMode::Compact;
    let mut ui = UiState {
        selected_index: Some(2),
        scroll_offset: 2,
        ..Default::default()
    };
    let before = paint_card_row(&snapshot, &mut ui, "task-2", 30);
    snapshot.theme.display.card_density = crate::config::CardDensityMode::Expanded;
    let after = paint_card_row(&snapshot, &mut ui, "task-2", 30);
    assert_eq!(
        after, before,
        "live height changes above do not move the selection"
    );
    assert!(!ui.scrollbar.visible(ui.animation_phase));
}

#[test]
fn shrinking_above_at_the_top_records_the_unavoidable_clamp() {
    let mut snapshot = overflowing_fleet();
    snapshot.theme.display.card_density = crate::config::CardDensityMode::Expanded;
    let mut ui = UiState {
        selected_index: Some(2),
        ..Default::default()
    };
    let before = paint_card_row(&snapshot, &mut ui, "task-2", 35);
    snapshot.theme.display.card_density = crate::config::CardDensityMode::Compact;
    let frame = compose_lines(&snapshot, None, &ui, &Theme::fixed(false), 54, 35);
    assert!(
        frame.anchor_clamped,
        "shrinking above needs a negative offset"
    );
    assert!(paint_card_row(&snapshot, &mut ui, "task-2", 35) < before);
}

#[test]
fn banner_appearance_and_disappearance_keep_the_card_row() {
    let mut snapshot = overflowing_fleet_with_unread_lead();
    let mut ui = UiState {
        selected_index: Some(4),
        scroll_offset: 8,
        offset_authoritative: true,
        ..Default::default()
    };
    let before = paint_card_row(&snapshot, &mut ui, "task-3", 35);
    let lead = &mut std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0];
    lead.unread = false;
    let after = paint_card_row(&snapshot, &mut ui, "task-3", 35);
    assert_eq!(
        after, before,
        "removing the banner preserves absolute pane placement"
    );
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .unread = true;
    assert_eq!(paint_card_row(&snapshot, &mut ui, "task-3", 35), before);
}

#[test]
fn selecting_a_partially_visible_card_does_not_reveal_it() {
    let snapshot = overflowing_fleet();
    let mut ui = UiState::default();
    let _ = paint_card_row(&snapshot, &mut ui, "task-0", 20);
    let offset = ui.scroll_offset;
    ui.selected_index = ui.interactions.visible_row_span().map(|(_, last)| last);
    let mut terminal = Terminal::new(TestBackend::new(54, 20)).unwrap();
    draw_to_terminal(&mut terminal, &snapshot, None, &mut ui).unwrap();
    assert_eq!(
        ui.scroll_offset, offset,
        "even one visible card line prevents reveal"
    );
}

#[test]
fn wheel_peek_anchors_the_top_visible_card_not_the_offscreen_selection() {
    let mut snapshot = overflowing_fleet();
    snapshot.theme.display.card_density = crate::config::CardDensityMode::Compact;
    let mut ui = UiState {
        selected_index: Some(0),
        scroll_offset: 12,
        offset_authoritative: true,
        ..Default::default()
    };
    let before = paint_card_row(&snapshot, &mut ui, "task-3", 20);
    snapshot.theme.display.card_density = crate::config::CardDensityMode::Expanded;
    assert_eq!(paint_card_row(&snapshot, &mut ui, "task-3", 20), before);
}

#[test]
fn a_departed_anchor_falls_through_to_the_next_placed_card() {
    let mut snapshot = overflowing_fleet();
    snapshot.theme.display.card_density = crate::config::CardDensityMode::Compact;
    let rows = std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[1].rows,
    );
    for index in 0..3 {
        let mut row = rows.last().unwrap().clone();
        row.id = format!("tail-{index}");
        rows.push(row);
    }
    let mut ui = UiState {
        scroll_offset: 12,
        ..Default::default()
    };
    let before = paint_card_row(&snapshot, &mut ui, "task-4", 26);
    let survivor = snapshot.worktree_groups[1].rows[1].id.clone();
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[1].rows)
        .remove(0);
    let mut terminal = Terminal::new(TestBackend::new(54, 26)).unwrap();
    draw_to_terminal(&mut terminal, &snapshot, None, &mut ui).unwrap();
    assert_eq!(
        ui.painted_rows
            .iter()
            .find(|(id, _)| id == &survivor)
            .unwrap()
            .1,
        before as isize
    );
}

#[test]
fn resort_anchors_the_cards_own_line_not_its_new_group_header() {
    let mut snapshot = overflowing_fleet();
    let mut ui = UiState {
        selected_index: Some(2),
        scroll_offset: 7,
        ..Default::default()
    };
    let before = paint_card_row(&snapshot, &mut ui, "task-2", 24);
    let rows = std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    );
    let selected = rows.remove(2);
    rows.insert(0, selected);
    ui.selected_index = Some(0);
    assert_eq!(paint_card_row(&snapshot, &mut ui, "task-2", 24), before);
}

#[test]
fn correction_during_a_wheel_flight_snaps_without_losing_the_remaining_glide() {
    let mut snapshot = overflowing_fleet();
    snapshot.theme.display.card_density = crate::config::CardDensityMode::Compact;
    let mut ui = UiState {
        selected_index: Some(2),
        scroll_offset: 6,
        frame_ms: 40,
        offset_authoritative: true,
        ..Default::default()
    };
    ui.scroll_ease.observe(2, 0);
    ui.scroll_ease.arm();
    ui.scroll_ease.observe(6, 0);
    let before = paint_card_row(&snapshot, &mut ui, "task-2", 28);
    let remaining = ui.scroll_offset - ui.scroll_ease.visible(ui.scroll_offset, ui.frame_ms);
    snapshot.theme.display.card_density = crate::config::CardDensityMode::Expanded;
    assert_eq!(paint_card_row(&snapshot, &mut ui, "task-2", 28), before);
    assert!(ui.scroll_ease.in_flight());
    assert_eq!(
        ui.scroll_offset - ui.scroll_ease.visible(ui.scroll_offset, ui.frame_ms),
        remaining
    );
}

#[test]
fn wheel_ease_positions_cards_scrollbar_and_hits_by_the_visible_row() {
    let snapshot = overflowing_fleet();
    let theme = Theme::fixed(false);
    let mut ui = UiState {
        scroll_offset: 9,
        frame_ms: 40,
        offset_authoritative: true,
        ..Default::default()
    };
    ui.scrollbar.observe(0, 0);
    ui.scroll_ease.arm();
    ui.scroll_ease.observe(9, 0);
    let visible = ui.scroll_ease.visible(9, ui.frame_ms);
    assert!((1..9).contains(&visible));
    let expected = compose_lines(
        &snapshot,
        None,
        &UiState {
            scroll_offset: visible,
            scroll_ease: Default::default(),
            ..ui.clone()
        },
        &theme,
        38,
        20,
    );
    let actual = compose_lines(&snapshot, None, &ui, &theme, 38, 20);
    assert_eq!(actual.scroll_offset, 9);
    assert_eq!(actual.lines, expected.lines);
    assert_eq!(actual.interactions, expected.interactions);
}

#[test]
fn wheel_banner_height_uses_the_target_throughout_the_glide() {
    let snapshot = overflowing_fleet_with_unread_lead();
    let theme = Theme::fixed(false);
    let mut ui = UiState {
        scroll_offset: 99,
        offset_authoritative: true,
        ..Default::default()
    };
    let target_frame = compose_lines(&snapshot, None, &ui, &theme, 38, 20);
    ui.scroll_offset = target_frame.scroll_offset;
    ui.scroll_ease.arm();
    ui.scroll_ease.observe(ui.scroll_offset, 0);
    let first = compose_lines(&snapshot, None, &ui, &theme, 38, 20);
    assert_eq!(first.interactions.visible_row_span().unwrap().0, 0);
    for elapsed in [0, 16, 48, 100] {
        ui.frame_ms = elapsed;
        let frame = compose_lines(&snapshot, None, &ui, &theme, 38, 20);
        assert_eq!(frame.top_height, target_frame.top_height);
        assert!(
            frame
                .interactions
                .line_for_target(&HitTarget::UnreadBanner)
                .is_some()
        );
    }
}

#[test]
fn selection_follow_cancels_a_flight_even_with_the_same_target() {
    let snapshot = overflowing_fleet();
    let mut ui = UiState {
        scroll_offset: 3,
        ..Default::default()
    };
    ui.scroll_ease.arm();
    ui.scroll_ease.observe(3, 0);
    let mut terminal = Terminal::new(TestBackend::new(38, 20)).unwrap();
    draw_to_terminal(&mut terminal, &snapshot, None, &mut ui).unwrap();
    assert_eq!(ui.scroll_offset, 3);
    assert!(!ui.scroll_ease.in_flight());
}

#[test]
fn no_selection_holds_the_clamped_scroll_after_own_input() {
    let snapshot = overflowing_fleet();
    let mut ui = UiState {
        scroll_offset: 4,
        own_input_reveal: true,
        ..Default::default()
    };
    let theme = Theme::fixed(false);
    let composed = compose_lines(&snapshot, None, &ui, &theme, 38, 20);
    assert_eq!(composed.scroll_offset, 4);
    ui.scroll_offset = usize::MAX;
    let clamped = compose_lines(&snapshot, None, &ui, &theme, 38, 20);
    assert!(clamped.scroll_offset < usize::MAX);
}

#[test]
fn auto_scroll_nudges_the_selection_minimally_into_view() {
    // A hand-built scroll-zone map: a leading gap, row 0 on lines 1-2, row 1
    // on lines 3-4, row 2 an expanded card on lines 5-8.
    let map = vec![
        None,
        Some(0),
        Some(0),
        Some(1),
        Some(1),
        Some(2),
        Some(2),
        Some(2),
        Some(2),
    ];
    // Fully visible: the window doesn't move.
    assert_eq!(auto_scroll_to_selection(&map, 1, 0, 5), 0);
    // Above the window: scroll up to the card's first line.
    assert_eq!(auto_scroll_to_selection(&map, 0, 4, 5), 1);
    // Below the window: scroll down just enough for its last line.
    assert_eq!(auto_scroll_to_selection(&map, 2, 0, 5), 4);
    // Taller than the viewport: pin the card's first line to the top.
    assert_eq!(auto_scroll_to_selection(&map, 2, 0, 3), 5);
    // Absent from the zone: hold the clamped offset.
    assert_eq!(auto_scroll_to_selection(&map, 9, 2, 5), 2);
    // Degenerate zero-height viewport: hold.
    assert_eq!(auto_scroll_to_selection(&map, 1, 2, 0), 2);
}

#[test]
fn scroll_thumb_reads_top_and_bottom_true() {
    // 10 zone lines through a 5-row viewport: the thumb spans half the track.
    assert_eq!(scroll_thumb(0, 10, 5), (0, 2));
    // At the bottom (offset == max) the thumb pins to the track's last rows.
    assert_eq!(scroll_thumb(5, 10, 5), (3, 2));
    // Midway it sits between, flush at neither end.
    assert_eq!(scroll_thumb(2, 10, 5), (1, 2));
    // A huge zone never shrinks the thumb below one row.
    assert_eq!(scroll_thumb(0, 1_000, 4), (0, 1));
}
#[test]
fn render_scroll_overflow_shows_bar() {
    // More cards than the frame holds, mid-scroll: the cockpit stays pinned at
    // the top, the footer at the bottom, and the cards scroll between them
    // behind a right-rail scrollbar — the thumb at the top of the track, since
    // the selection (row 0) holds the window at the zone's start. The
    // freshly-stamped fade is what shows the bar: `auto` mode paints it only
    // while the viewport moves.
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &overflowing_fleet(),
        None,
        &UiState {
            selected_index: Some(0),
            scrollbar: scrolled_fade(0, 0),
            ..Default::default()
        },
        38,
        27,
    );
    let lines: Vec<&str> = rendered.lines().collect();
    assert!(
        lines[0].contains("⌘ query-engine"),
        "cockpit pinned:\n{rendered}"
    );
    assert!(lines[5].contains("⢿ 6"), "make-up pinned:\n{rendered}");
    assert!(
        lines.last().unwrap().contains("? for help"),
        "footer pinned:\n{rendered}"
    );
    assert!(
        line_containing(&rendered, "⑂ alpha").ends_with('▐'),
        "the thumb replaces the selected header rail:\n{rendered}"
    );
    assert!(rendered.contains('▕'), "the track renders:\n{rendered}");
    assert_snapshot("scroll_overflow_shows_bar", rendered);
}
#[test]
fn help_overlay_floats_over_cards_with_scrollbar() {
    let mut snapshot = overflowing_fleet();
    snapshot.theme.display.scrollbar = ScrollbarMode::Always;
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: Some(0),
            help_visible: true,
            ..Default::default()
        },
        54,
        28,
    );

    let lines = rendered.lines().collect::<Vec<_>>();
    assert!(
        lines[0].contains("⌘ query-engine"),
        "cockpit stays pinned while help is open:\n{rendered}"
    );
    assert!(
        lines.last().unwrap().contains("? for help"),
        "footer stays pinned while help is open:\n{rendered}"
    );
    assert!(
        rendered.contains("╭ help"),
        "the floating help box renders:\n{rendered}"
    );
    assert!(
        rendered
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .contains("r reload"),
        "help chrome survives narrow framing:\n{rendered}"
    );
    assert!(
        rendered.contains("task-0"),
        "the floating help box leaves uncovered card body visible:\n{rendered}"
    );
    assert_snapshot(
        "help_overlay_floating_box",
        snapshot_to_screen_with_alert_and_ui(
            &snapshot,
            None,
            &UiState {
                selected_index: Some(0),
                help_visible: true,
                ..Default::default()
            },
            80,
            44,
        ),
    );
}
#[test]
fn render_scroll_offset_follows_selection_to_bottom() {
    // Selecting the last row auto-scrolls its card fully into view. The first
    // draw establishes the fade's baseline rather than reading as a move, so
    // this frame doubles as the settled witness: no scroll activity, no bar.
    let ui = UiState {
        selected_index: Some(5),
        own_input_reveal: true,
        ..Default::default()
    };
    let rendered = snapshot_to_screen_with_alert_and_ui(&overflowing_fleet(), None, &ui, 38, 29);
    assert!(
        rendered.contains("task-5"),
        "the selected last card is in view:\n{rendered}"
    );
    assert!(
        !rendered.contains("task-0"),
        "the zone's head scrolled off:\n{rendered}"
    );
    let mut no_bar = overflowing_fleet();
    no_bar.theme.display.scrollbar = ScrollbarMode::Never;
    let expected = snapshot_to_screen_with_alert_and_ui(&no_bar, None, &ui, 38, 29);
    assert_eq!(
        rendered, expected,
        "a settled viewport carries no scrollbar overlay"
    );
    assert_snapshot("scroll_offset_follows_selection_to_bottom", rendered);
}

#[test]
fn render_scroll_pins_tall_expanded_card_top() {
    // A selected card whose expanded subagent list outgrows the viewport pins
    // its first line — the group header — to the top of the scroll zone.
    let now = fixed_now();
    let mut parent = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    parent.last_activity = now - Duration::from_secs(8);
    let mut agents = vec![parent];
    for i in 0..4_u64 {
        let mut child = agent(
            &format!("child-{i}"),
            "claude",
            AgentStatus::Running,
            None,
            None,
            Some("Explore"),
        );
        child.parent_agent_id = Some("claude-1".into());
        child.subagent_description = Some(format!("survey area {i}"));
        child.subagent_started_at = Some(now - Duration::from_secs(240 - i * 30));
        child.last_activity = now;
        child.usage.fresh_input_tokens = Some(1_000 * (i + 1));
        agents.push(child);
    }
    let snapshot = snapshot_with(agents);

    let rendered = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &selected_ui(), 54, 24);
    // The viewport opens below the pinned cockpit separator, so the card
    // block's first line holds one row lower while the subagent list fills down.
    let lines: Vec<&str> = rendered.lines().collect();
    assert!(
        lines[7].contains("⑂ main"),
        "the tall card's first line pins below the cockpit separator:\n{rendered}"
    );
    assert!(
        rendered.contains("⧉ subagents (4)"),
        "the expanded list is what overflows:\n{rendered}"
    );
    assert_snapshot("scroll_pins_tall_expanded_card_top", rendered);
}
#[test]
fn render_scroll_manual_offset_holds() {
    // A wheel frame takes the offset even with the selection above the fold.
    // A settled window has no scrollbar activity.
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &overflowing_fleet(),
        None,
        &UiState {
            selected_index: Some(0),
            scroll_offset: 6,
            offset_authoritative: true,
            ..Default::default()
        },
        38,
        28,
    );
    assert!(
        !rendered.contains("task-0"),
        "the selected card stays beyond the fold during the peek:\n{rendered}"
    );
    assert_snapshot("scroll_manual_offset_holds", rendered);
}
#[test]
fn scrollbar_modes_control_visibility_without_moving_the_window() {
    let settled_ui = UiState {
        selected_index: Some(0),
        scrollbar: scrolled_fade(0, 0),
        animation_phase: 11,
        ..Default::default()
    };
    let settled =
        snapshot_to_screen_with_alert_and_ui(&overflowing_fleet(), None, &settled_ui, 38, 21);
    let mut no_bar = overflowing_fleet();
    no_bar.theme.display.scrollbar = ScrollbarMode::Never;
    let expected = snapshot_to_screen_with_alert_and_ui(&no_bar, None, &settled_ui, 38, 21);
    assert_eq!(
        settled, expected,
        "the settle window has passed — no scrollbar overlay"
    );

    let mut snapshot = overflowing_fleet();
    snapshot.theme.display.scrollbar = ScrollbarMode::Always;
    let always = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &selected_ui(), 38, 21);
    assert!(
        line_containing(&always, "⑂ alpha").ends_with('▐'),
        "always mode pins the bar with no activity:\n{always}"
    );

    let mut snapshot = overflowing_fleet();
    snapshot.theme.display.scrollbar = ScrollbarMode::Never;
    let never = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: Some(0),
            scroll_offset: 9,
            offset_authoritative: true,
            scrollbar: scrolled_fade(9, 0),
            ..Default::default()
        },
        38,
        18,
    );
    assert!(
        !ends_with_rail(line_containing(&never, "⑂ beta")),
        "never mode leaves a non-selected rail blank even mid-scroll:\n{never}"
    );
    assert!(
        never.contains("task-2") && !never.contains("task-0"),
        "the pinned window still renders its cards:\n{never}"
    );
}

fn line_containing<'a>(rendered: &'a str, needle: &str) -> &'a str {
    rendered
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("missing {needle:?} in:\n{rendered}"))
}

fn ends_with_rail(line: &str) -> bool {
    matches!(line.chars().last(), Some('▐' | '▕'))
}
