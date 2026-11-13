use super::*;
use crate::sidebar_pane::render::PresentationToggles;
use crate::sidebar_pane::render::ViewportIntent;
use crate::sidebar_pane::render::ViewportRequest;
use std::sync::Arc;

#[test]
fn moving_a_view_keeps_banner_geometry_on_the_pane() {
    fn assert_send(_: &impl Send) {}

    let snapshot = overflowing_fleet_with_unread_lead();
    let mut source = ViewState {
        selected_index: Some(4),
        scroll_offset: 8,
        viewport: ViewportRequest {
            offset_authoritative: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pane_source = UiState::default();
    let _ = paint_card_row(&snapshot, &mut source, &mut pane_source, "task-3", 35);
    assert!(pane_source.banner_shown);
    assert_send(&source);
    let _destination = source;
    let pane_destination = UiState::default();
    assert!(
        !pane_destination.banner_shown,
        "the destination has not painted a banner; its geometry cannot come from the source"
    );
}

#[test]
fn a_portable_view_keeps_scroll_zone_relative_anchors_across_geometries() {
    let snapshot = overflowing_fleet();
    let mut source = ViewState {
        scroll_offset: 12,
        viewport: ViewportRequest {
            offset_authoritative: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pane_source = UiState::default();
    let mut terminal = Terminal::new(TestBackend::new(38, 22)).unwrap();
    draw_to_terminal(
        &mut terminal,
        &snapshot,
        None,
        &mut source,
        &mut pane_source,
    )
    .unwrap();
    assert!(!source.anchored_rows.is_empty());
    for (id, relative) in &source.anchored_rows {
        let painted = pane_source
            .painted_rows
            .iter()
            .find(|(row, _)| row == id)
            .unwrap()
            .1;
        assert_eq!(
            *relative,
            painted - pane_source.scroll_zone_top as isize,
            "shared anchors are relative to the scroll zone, not the pane"
        );
    }
    let anchor = source.anchored_rows[0].clone();
    let mut destination = source;
    let mut pane_destination = UiState::default();
    let mut terminal = Terminal::new(TestBackend::new(54, 22)).unwrap();
    draw_to_terminal(
        &mut terminal,
        &snapshot,
        None,
        &mut destination,
        &mut pane_destination,
    )
    .unwrap();
    assert_eq!(
        destination
            .anchored_rows
            .iter()
            .find(|(id, _)| id == &anchor.0),
        Some(&anchor),
        "the destination's cockpit geometry must not move the carried anchor"
    );
}

#[test]
fn focus_marker_overlays_the_edge_without_moving_cards_or_anchors() {
    let snapshot = overflowing_fleet();
    let theme = Theme::fixed(false);
    let mut ui = ViewState {
        selected_index: Some(5),
        ..Default::default()
    };
    let pane_ui = UiState::default();
    let plain = compose_lines(&snapshot, None, &ui, &pane_ui, &theme, 38, 20);
    ui.focus_marker_ready = true;
    let marked = compose_lines(&snapshot, None, &ui, &pane_ui, &theme, 38, 20);
    assert!(
        marked
            .interactions
            .line_for_target(&HitTarget::FocusMarker)
            .is_some(),
        "named edge marker"
    );
    let (column, row) = marked
        .interactions
        .line_for_target(&HitTarget::FocusMarker)
        .unwrap();
    assert_eq!(usize::from(row), 20 - marked.bottom_height - 1);
    assert_eq!(
        marked.interactions.target_at(column + 10, row),
        Some(HitTarget::FocusMarker)
    );
    assert_eq!(marked.scroll_offset, plain.scroll_offset);
    assert_eq!(marked.painted_rows, plain.painted_rows);
    assert_eq!(marked.anchored_rows, plain.anchored_rows);
    assert_eq!(marked.top_height, plain.top_height);
    assert_eq!(marked.lines.len(), plain.lines.len());
    let screen = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, &pane_ui, 38, 20);
    let name = ui.visible_roster(&snapshot).row(5).unwrap().display_name();
    assert!(screen.contains(&format!("↓ {name}")));
    assert_snapshot("offscreen_focus_marker", screen);
}

#[test]
fn unread_banner_suppresses_only_the_up_marker() {
    let mut snapshot = overflowing_fleet();
    let lead = snapshot.rows_mut().next().unwrap();
    lead.as_agent_mut().unwrap().status = AgentStatus::Waiting;
    lead.unread = true;
    let theme = Theme::fixed(false);
    for (selected_index, scroll_offset, marker) in [(0, 99, false), (5, 8, true)] {
        let ui = ViewState {
            selected_index: Some(selected_index),
            scroll_offset,
            focus_marker_ready: true,
            ..Default::default()
        };
        let pane_ui = UiState::default();
        let frame = compose_lines(&snapshot, None, &ui, &pane_ui, &theme, 38, 20);
        assert!(
            frame
                .interactions
                .line_for_target(&HitTarget::UnreadBanner)
                .is_some()
        );
        assert_eq!(
            frame
                .interactions
                .line_for_target(&HitTarget::FocusMarker)
                .is_some(),
            marker
        );
    }
    snapshot.rows_mut().next().unwrap().unread = false;
    let ui = ViewState {
        selected_index: Some(0),
        scroll_offset: 99,
        focus_marker_ready: true,
        ..Default::default()
    };
    let pane_ui = UiState::default();
    let screen = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, &pane_ui, 38, 20);
    let name = ui.visible_roster(&snapshot).row(0).unwrap().display_name();
    assert!(
        screen.contains(&format!("↑ {name}")),
        "the up marker returns once the banner is gone"
    );
}

#[test]
fn focus_marker_trims_wide_names_without_wrapping_the_frame() {
    let mut snapshot = overflowing_fleet();
    snapshot
        .rows_mut()
        .nth(5)
        .unwrap()
        .as_agent_mut()
        .unwrap()
        .handle = Some("多".repeat(100));
    let theme = Theme::fixed(false);
    let ui = ViewState {
        selected_index: Some(5),
        focus_marker_ready: true,
        ..Default::default()
    };
    let pane_ui = UiState::default();
    let frame = compose_lines(&snapshot, None, &ui, &pane_ui, &theme, 16, 20);
    assert!(
        frame
            .interactions
            .line_for_target(&HitTarget::FocusMarker)
            .is_some(),
        "narrow marker"
    );
    let (_, row) = frame
        .interactions
        .line_for_target(&HitTarget::FocusMarker)
        .unwrap();
    assert!(frame.lines[usize::from(row)].width() <= 16);
    let unmarked = ViewState {
        focus_marker_ready: false,
        ..ui.clone()
    };
    let pane_unmarked = pane_ui.clone();
    assert_eq!(
        snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, &pane_ui, 16, 20)
            .lines()
            .count(),
        snapshot_to_screen_with_alert_and_ui(&snapshot, None, &unmarked, &pane_unmarked, 16, 20)
            .lines()
            .count(),
        "the marker never wraps into an extra line"
    );
}

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

fn select_card(snapshot: &SidebarSnapshot, ui: &mut ViewState, id: &str) {
    ui.freeze = Some(LayoutFreeze {
        frozen: FrozenOrder::default(),
        armed_ms: fixed_now().as_millisecond(),
    });
    ui.selected_index = ui.visible_roster(snapshot).ordinal_of_id(id);
    ui.viewport.own_input_reveal = true;
    assert!(ui.selected_index.is_some(), "selection exists");
}

#[test]
fn shared_placement_preserves_the_sources_expanded_selection() {
    let snapshot = delegation_fleet(&[0]);
    let mut source = selected_ui();
    let mut pane_source = UiState::default();
    let before = paint_card_row(&snapshot, &mut source, &mut pane_source, "task-1", 45);
    let mut target = ViewState::default();
    let mut pane_target = UiState::default();
    select_card(&snapshot, &mut target, "codex-1");
    target.viewport.focus_placement = Some(
        serde_json::from_value(serde_json::json!({
            "row_id": "codex-1",
            "viewport_row": before as isize - pane_source.scroll_zone_top as isize,
            "held": ["codex-0", "codex-1", "departed"],
        }))
        .unwrap(),
    );

    assert_eq!(
        paint_card_row(&snapshot, &mut target, &mut pane_target, "task-1", 45),
        before
    );
    assert_eq!(target.held_expansions, ["codex-0".into()].into());
}

#[test]
fn shared_placement_replaces_the_destinations_previous_selection() {
    let mut snapshot = delegation_fleet(&[2]);
    Arc::make_mut(&mut Arc::make_mut(&mut snapshot.worktree_groups)[0].rows).truncate(4);
    let mut source = selected_ui();
    let mut pane_source = UiState::default();
    let before = paint_card_row(&snapshot, &mut source, &mut pane_source, "task-3", 45);
    let mut target = ViewState::default();
    let mut pane_target = UiState::default();
    select_card(&snapshot, &mut target, "codex-2");
    paint_card_row(&snapshot, &mut target, &mut pane_target, "task-3", 45);
    select_card(&snapshot, &mut target, "codex-3");
    target.viewport.focus_placement = Some(
        serde_json::from_value(serde_json::json!({
            "row_id": "codex-3",
            "viewport_row": before as isize - pane_source.scroll_zone_top as isize,
            "held": ["codex-0"],
        }))
        .unwrap(),
    );

    assert_eq!(
        paint_card_row(&snapshot, &mut target, &mut pane_target, "task-3", 45),
        before
    );
    assert!(!target.held_expansions.contains("codex-2"));
}

#[test]
fn shared_placement_with_no_opening_set_overrides_local_holds() {
    let snapshot = delegation_fleet(&[0]);
    let mut target = selected_ui();
    let mut pane_target = UiState::default();
    paint_card_row(&snapshot, &mut target, &mut pane_target, "task-1", 45);
    select_card(&snapshot, &mut target, "codex-1");
    let held_row = paint_card_row(&snapshot, &mut target, &mut pane_target, "task-1", 45);
    assert!(!target.held_expansions.is_empty());
    target.viewport.focus_placement = Some(
        serde_json::from_value(serde_json::json!({
            "row_id": "codex-1",
            "viewport_row": held_row as isize - pane_target.scroll_zone_top as isize,
            "held": [],
        }))
        .unwrap(),
    );

    paint_card_row(&snapshot, &mut target, &mut pane_target, "task-1", 45);
    assert!(target.held_expansions.is_empty());
}

fn placement(
    row_id: &str,
    viewport_row: isize,
    held: &[&str],
) -> crate::mux::focus_anchor::FocusPlacement {
    serde_json::from_value(serde_json::json!({
        "row_id": row_id,
        "viewport_row": viewport_row,
        "held": held,
    }))
    .unwrap()
}

/// An authoritative offset suppresses a pending jump's scroll target and this sidebar's own
/// reveal, whichever was requested first, while the jump's held set still opens its cards,
/// both in the composed frame and in the entry stops input reads before that paint.
#[test]
fn authoritative_offset_keeps_a_pending_jumps_held_set_but_not_its_target() {
    let mut snapshot = delegation_fleet(&[0]);
    let codex = Arc::make_mut(&mut Arc::make_mut(&mut snapshot.worktree_groups)[0].rows)
        .iter_mut()
        .find(|row| row.id == "codex-0")
        .unwrap();
    codex.as_agent_mut().unwrap().sub_agents[0].pane =
        Some(PaneId::from_parts(MuxName::Zellij, "terminal_child"));
    let parent = ViewState::default()
        .visible_roster(&snapshot)
        .ordinal_of_id("codex-0");
    let parent = parent.unwrap();
    let theme = Theme::fixed(false);
    let pane_ui = UiState::default();
    let arm = |authority_first: bool| {
        let mut view = ViewState::default();
        select_card(&snapshot, &mut view, "codex-11");
        let jump = placement("codex-11", 0, &["codex-0"]);
        if authority_first {
            view.request(ViewportIntent::Hold);
            view.request(ViewportIntent::Jump(jump));
        } else {
            view.request(ViewportIntent::Jump(jump));
            view.request(ViewportIntent::Hold);
        }
        view
    };
    let mut unrequested = ViewState::default();
    select_card(&snapshot, &mut unrequested, "codex-11");
    unrequested.viewport.own_input_reveal = false;
    assert!(unrequested.entry_stops(&snapshot, parent).is_empty());
    let mut directed = unrequested.clone();
    directed.viewport.focus_placement = Some(placement("codex-11", 0, &["codex-0"]));
    let directed = compose_lines(&snapshot, None, &directed, &pane_ui, &theme, 54, 45);
    assert!(
        directed.scroll_offset > 0,
        "without authority the jump lands"
    );
    for authority_first in [true, false] {
        let view = arm(authority_first);
        assert!(view.viewport.own_input_reveal);
        assert!(!view.entry_stops(&snapshot, parent).is_empty());
        let composed = compose_lines(&snapshot, None, &view, &pane_ui, &theme, 54, 45);
        assert_eq!(
            composed.scroll_offset, 0,
            "authority first: {authority_first}"
        );
        assert_eq!(composed.held_expansions, ["codex-0".into()].into());
        assert!(
            composed
                .lines
                .iter()
                .any(|line| line.to_string().contains("survey area 0-"))
        );
    }
}

/// A placement for another row still snaps an in-flight glide, but its own target is not used:
/// the anchor places the window as it would with no placement.
#[test]
fn a_mismatched_placement_snaps_without_a_shared_target() {
    let snapshot = overflowing_fleet();
    let mut view = ViewState {
        selected_index: Some(0),
        ..Default::default()
    };
    let mut pane_ui = UiState::default();
    let mut terminal = Terminal::new(TestBackend::new(54, 20)).unwrap();
    draw_to_terminal(&mut terminal, &snapshot, None, &mut view, &mut pane_ui).unwrap();
    view.scroll_offset += 3;
    view.viewport.offset_authoritative = true;
    view.scroll_ease.arm();
    draw_to_terminal(&mut terminal, &snapshot, None, &mut view, &mut pane_ui).unwrap();
    assert!(view.scroll_ease.in_flight());
    let theme = Theme::fixed(false);
    let control = compose_lines(&snapshot, None, &view, &pane_ui, &theme, 54, 20);
    assert!(control.scroll_ease.in_flight());
    let other = snapshot.rows().nth(4).unwrap().id.clone();
    view.viewport.focus_placement = Some(placement(&other, 0, &[]));
    let composed = compose_lines(&snapshot, None, &view, &pane_ui, &theme, 54, 20);
    assert!(!composed.scroll_ease.in_flight());
    assert_eq!(composed.scroll_offset, control.scroll_offset);
    assert_eq!(composed.correction, control.correction);
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
    let card = row.as_agent_mut().unwrap();
    card.usage.context_pct = Some(1);
    card.cache = expired_cache.then_some(crate::store::snapshot::CacheClock {
        ceiling_secs: 1800,
        last_request_at: Some(fixed_now() - Duration::from_secs(1801)),
        warm_until: Some(fixed_now() - Duration::from_secs(1)),
        rewarm_cost_usd: None,
        recorded: false,
    });
    let mut ui = ViewState {
        scroll_offset: offset,
        ..Default::default()
    };
    let mut pane_ui = UiState::default();
    select_card(&snapshot, &mut ui, "codex-0");
    let task = format!("task-{target}");
    let before = paint_card_row(&snapshot, &mut ui, &mut pane_ui, &task, 45);
    let screen = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, &pane_ui, 54, 45);
    assert_eq!(screen.contains("cache expired"), expired_cache, "{screen}");

    select_card(&snapshot, &mut ui, &format!("codex-{target}"));
    let after = paint_card_row(&snapshot, &mut ui, &mut pane_ui, &task, 45);
    assert_eq!(after, before, "cache notice above must not move the click");
    let screen = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, &pane_ui, 54, 45);
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
    let mut ui = ViewState {
        scroll_offset: offset,
        ..Default::default()
    };
    let mut pane_ui = UiState::default();
    select_card(&snapshot, &mut ui, &format!("codex-{parent}"));
    let task = format!("task-{target}");
    let before = paint_card_row(&snapshot, &mut ui, &mut pane_ui, &task, 45);
    select_card(&snapshot, &mut ui, &format!("codex-{target}"));
    let after = paint_card_row(&snapshot, &mut ui, &mut pane_ui, &task, 45);
    assert_eq!(
        after, before,
        "previous selection's collapse must not move the click"
    );
    assert!(!pane_ui.anchor_clamped);
    let screen = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, &pane_ui, 54, 45);
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
    let mut pane_ui = UiState::default();
    let before = paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 45);
    select_card(&snapshot, &mut ui, "codex-2");
    assert_eq!(
        paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 45),
        before
    );
    let screen = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, &pane_ui, 54, 45);
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
    let mut ui = ViewState {
        scroll_offset: 10,
        ..Default::default()
    };
    let mut pane_ui = UiState::default();
    select_card(&snapshot, &mut ui, "codex-0");
    let before = paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 45);
    select_card(&snapshot, &mut ui, "codex-2");
    assert_eq!(
        paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 45),
        before
    );
    assert!(!ui.held_expansions.is_empty());
    assert!(
        snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, &pane_ui, 54, 45)
            .contains("survey area")
    );
    ui.freeze = None;
    paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 45);
    assert!(ui.held_expansions.is_empty());
    assert!(
        !snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, &pane_ui, 54, 45)
            .contains("survey area")
    );
}

#[test]
fn authoritative_wheel_paint_keeps_a_frozen_expansion() {
    let snapshot = delegation_fleet(&[0]);
    let mut ui = selected_ui();
    let mut pane_ui = UiState::default();
    let before = paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 45);
    select_card(&snapshot, &mut ui, "codex-2");
    assert_eq!(
        paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 45),
        before
    );
    ui.scroll_offset += 3;
    ui.viewport.offset_authoritative = true;
    ui.scroll_ease.arm();
    paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 45);
    assert!(!ui.held_expansions.is_empty());
    assert!(
        snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, &pane_ui, 54, 45)
            .contains("survey area")
    );
    ui.freeze = None;
    paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 45);
    assert!(ui.held_expansions.is_empty());
    assert!(
        !snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, &pane_ui, 54, 45)
            .contains("survey area")
    );
}

#[test]
fn growth_above_keeps_expansions_until_freeze_expires() {
    let snapshot = delegation_fleet(&[0]);
    let mut ui = selected_ui();
    let mut pane_ui = UiState::default();
    ui.toggles
        .delegation_overrides
        .extend([DelegationPin::current(&snapshot, "codex-1", true)]);
    let before = paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 45);
    select_card(&snapshot, &mut ui, "codex-2");
    assert_eq!(
        paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 45),
        before
    );
    assert_eq!(ui.held_expansions, ["codex-0".into()].into());
    assert!(
        snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, &pane_ui, 54, 45)
            .contains("survey area 0-")
    );

    // The already-open card above gains children, making room to collapse.
    let grown = delegation_fleet(&[0, 1]);
    assert!(!ui.viewport.offset_authoritative);
    assert_eq!(
        paint_card_row(&grown, &mut ui, &mut pane_ui, "task-2", 45),
        before
    );
    assert!(!pane_ui.anchor_clamped);
    assert!(!ui.held_expansions.is_empty());
    let screen = snapshot_to_screen_with_alert_and_ui(&grown, None, &ui, &pane_ui, 54, 45);
    assert!(screen.contains("survey area 0-"));
    assert!(screen.contains("survey area 1-"));
    ui.freeze = None;
    paint_card_row(&grown, &mut ui, &mut pane_ui, "task-2", 45);
    assert!(ui.held_expansions.is_empty());
    assert!(
        !snapshot_to_screen_with_alert_and_ui(&grown, None, &ui, &pane_ui, 54, 45)
            .contains("survey area 0-")
    );
}

#[test]
fn collapse_holds_accumulate_across_clicks_at_the_top() {
    let snapshot = delegation_fleet(&[0, 1]);
    let mut ui = selected_ui();
    let mut pane_ui = UiState::default();
    paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-1", 45);
    select_card(&snapshot, &mut ui, "codex-1");
    let before = paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 45);
    select_card(&snapshot, &mut ui, "codex-2");
    assert_eq!(
        paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 45),
        before
    );
    assert_eq!(ui.held_expansions.len(), 2);
}

#[test]
fn compact_collapse_at_zero_keeps_the_clicked_card_shape() {
    let mut snapshot = overflowing_fleet();
    snapshot.theme.display.card_density = crate::config::CardDensityMode::Compact;
    let mut ui = selected_ui();
    let mut pane_ui = UiState::default();
    let before = paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 30);
    select_card(&snapshot, &mut ui, &snapshot.rows().nth(2).unwrap().id);
    assert_eq!(
        paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 30),
        before
    );
    assert!(!pane_ui.anchor_clamped);
}

#[test]
fn offscreen_selection_keeps_the_previous_card_and_reveals() {
    let snapshot = delegation_fleet(&[0]);
    let mut ui = selected_ui();
    let mut pane_ui = UiState::default();
    paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 45);
    select_card(&snapshot, &mut ui, "codex-11");
    paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-11", 45);
    assert!(!ui.held_expansions.is_empty());
    let theme = Theme::fixed(false);
    let composed = compose_lines(&snapshot, None, &ui, &pane_ui, &theme, 54, 200);
    assert!(
        composed
            .lines
            .iter()
            .any(|line| line.to_string().contains("survey area"))
    );
    ui.freeze = None;
    paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-11", 45);
    assert!(ui.held_expansions.is_empty());
    let composed = compose_lines(&snapshot, None, &ui, &pane_ui, &theme, 54, 200);
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
    let mut pane_ui = UiState::default();
    let before = paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 45);
    select_card(&snapshot, &mut ui, "codex-2");
    ui.held_expansions = ["codex-2".into(), "departed".into()].into();
    assert_eq!(
        paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 45),
        before
    );
    assert!(!pane_ui.anchor_clamped);
    assert_eq!(ui.held_expansions, ["codex-0".into()].into());
}

fn paint_card_row(
    snapshot: &SidebarSnapshot,
    ui: &mut ViewState,
    pane_ui: &mut UiState,
    task: &str,
    height: u16,
) -> usize {
    let mut terminal = Terminal::new(TestBackend::new(54, height)).unwrap();
    draw_to_terminal(&mut terminal, snapshot, None, ui, pane_ui).unwrap();
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
    let mut ui = ViewState {
        selected_index: Some(0),
        scroll_offset: 2,
        ..Default::default()
    };
    let mut pane_ui = UiState::default();
    let before = paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 30);
    ui.selected_index = Some(2);
    let after = paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 30);
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
    let mut ui = ViewState {
        selected_index: Some(0),
        scroll_offset: 2,
        ..Default::default()
    };
    let mut pane_ui = UiState::default();
    let before = paint_card_row(&snapshot, &mut ui, &mut pane_ui, "card-2", 30);
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
    assert_eq!(
        paint_card_row(&snapshot, &mut ui, &mut pane_ui, "card-2", 30),
        before
    );

    snapshot.rows_mut().next().unwrap().last_activity = fixed_now() - Duration::from_secs(86400);
    let before = paint_card_row(&snapshot, &mut ui, &mut pane_ui, "card-2", 30);
    snapshot.now += Duration::from_secs(1);
    assert_eq!(
        paint_card_row(&snapshot, &mut ui, &mut pane_ui, "card-2", 30),
        before,
        "shrinking above preserves the selection"
    );
}

#[test]
fn growth_above_an_unchanged_selection_keeps_its_row() {
    let mut snapshot = overflowing_fleet();
    snapshot.theme.display.card_density = crate::config::CardDensityMode::Compact;
    let mut ui = ViewState {
        selected_index: Some(2),
        scroll_offset: 2,
        ..Default::default()
    };
    let mut pane_ui = UiState::default();
    let before = paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 30);
    snapshot.theme.display.card_density = crate::config::CardDensityMode::Expanded;
    let after = paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 30);
    assert_eq!(
        after, before,
        "live height changes above do not move the selection"
    );
    assert!(
        !ui.scrollbar
            .visible(ui.frame_ms, snapshot.theme.display.resolved_refresh_ms())
    );
}

#[test]
fn shrinking_above_at_the_top_records_the_unavoidable_clamp() {
    let mut snapshot = overflowing_fleet();
    snapshot.theme.display.card_density = crate::config::CardDensityMode::Expanded;
    let mut ui = ViewState {
        selected_index: Some(2),
        ..Default::default()
    };
    let mut pane_ui = UiState::default();
    let before = paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 35);
    snapshot.theme.display.card_density = crate::config::CardDensityMode::Compact;
    let frame = compose_lines(&snapshot, None, &ui, &pane_ui, &Theme::fixed(false), 54, 35);
    assert!(
        frame.anchor_clamped,
        "shrinking above needs a negative offset"
    );
    assert!(paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 35) < before);
}

#[test]
fn banner_appearance_and_disappearance_keep_the_card_row() {
    let mut snapshot = overflowing_fleet_with_unread_lead();
    let mut ui = ViewState {
        selected_index: Some(4),
        scroll_offset: 8,
        viewport: ViewportRequest {
            offset_authoritative: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pane_ui = UiState::default();
    let before = paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-3", 35);
    let lead = &mut std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0];
    lead.unread = false;
    let after = paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-3", 35);
    assert_eq!(
        after, before,
        "removing the banner preserves absolute pane placement"
    );
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .unread = true;
    assert_eq!(
        paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-3", 35),
        before
    );
}

#[test]
fn selecting_a_partially_visible_card_does_not_reveal_it() {
    let snapshot = overflowing_fleet();
    let mut ui = ViewState::default();
    let mut pane_ui = UiState::default();
    let _ = paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-0", 20);
    let offset = ui.scroll_offset;
    ui.selected_index = pane_ui
        .interactions
        .visible_row_span()
        .map(|(_, last)| last);
    let mut terminal = Terminal::new(TestBackend::new(54, 20)).unwrap();
    draw_to_terminal(&mut terminal, &snapshot, None, &mut ui, &mut pane_ui).unwrap();
    assert_eq!(
        ui.scroll_offset, offset,
        "even one visible card line prevents reveal"
    );
}

#[test]
fn wheel_peek_anchors_the_top_visible_card_not_the_offscreen_selection() {
    let mut snapshot = overflowing_fleet();
    snapshot.theme.display.card_density = crate::config::CardDensityMode::Compact;
    let mut ui = ViewState {
        selected_index: Some(0),
        scroll_offset: 12,
        viewport: ViewportRequest {
            offset_authoritative: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pane_ui = UiState::default();
    let before = paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-3", 20);
    snapshot.theme.display.card_density = crate::config::CardDensityMode::Expanded;
    assert_eq!(
        paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-3", 20),
        before
    );
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
    let mut ui = ViewState {
        scroll_offset: 12,
        ..Default::default()
    };
    let mut pane_ui = UiState::default();
    let before = paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-4", 26);
    let survivor = snapshot.worktree_groups[1].rows[1].id.clone();
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[1].rows)
        .remove(0);
    let mut terminal = Terminal::new(TestBackend::new(54, 26)).unwrap();
    draw_to_terminal(&mut terminal, &snapshot, None, &mut ui, &mut pane_ui).unwrap();
    assert_eq!(
        pane_ui
            .painted_rows
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
    let mut ui = ViewState {
        selected_index: Some(2),
        scroll_offset: 7,
        ..Default::default()
    };
    let mut pane_ui = UiState::default();
    let before = paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 24);
    let rows = std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    );
    let selected = rows.remove(2);
    rows.insert(0, selected);
    ui.selected_index = Some(0);
    assert_eq!(
        paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 24),
        before
    );
}

#[test]
fn correction_during_a_wheel_flight_snaps_without_losing_the_remaining_glide() {
    let mut snapshot = overflowing_fleet();
    snapshot.theme.display.card_density = crate::config::CardDensityMode::Compact;
    let mut ui = ViewState {
        selected_index: Some(2),
        scroll_offset: 6,
        frame_ms: ms(40),
        viewport: ViewportRequest {
            offset_authoritative: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pane_ui = UiState::default();
    ui.scroll_ease.observe(2, ms(0));
    ui.scroll_ease.arm();
    ui.scroll_ease.observe(6, ms(0));
    let before = paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 28);
    let remaining = ui.scroll_offset - ui.scroll_ease.visible(ui.scroll_offset, ui.frame_ms);
    snapshot.theme.display.card_density = crate::config::CardDensityMode::Expanded;
    assert_eq!(
        paint_card_row(&snapshot, &mut ui, &mut pane_ui, "task-2", 28),
        before
    );
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
    let mut ui = ViewState {
        scroll_offset: 9,
        frame_ms: ms(40),
        viewport: ViewportRequest {
            offset_authoritative: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let pane_ui = UiState::default();
    ui.scrollbar.observe(0, ms(0));
    ui.scroll_ease.arm();
    ui.scroll_ease.observe(9, ms(0));
    let visible = ui.scroll_ease.visible(9, ui.frame_ms);
    assert!((1..9).contains(&visible));
    let expected = compose_lines(
        &snapshot,
        None,
        &ViewState {
            scroll_offset: visible,
            scroll_ease: Default::default(),
            ..ui.clone()
        },
        &pane_ui.clone(),
        &theme,
        38,
        20,
    );
    let actual = compose_lines(&snapshot, None, &ui, &pane_ui, &theme, 38, 20);
    assert_eq!(actual.scroll_offset, 9);
    assert_eq!(actual.lines, expected.lines);
    assert_eq!(actual.interactions, expected.interactions);
}

#[test]
fn wheel_banner_height_uses_the_target_throughout_the_glide() {
    let snapshot = overflowing_fleet_with_unread_lead();
    let theme = Theme::fixed(false);
    let mut ui = ViewState {
        scroll_offset: 99,
        viewport: ViewportRequest {
            offset_authoritative: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let pane_ui = UiState::default();
    let target_frame = compose_lines(&snapshot, None, &ui, &pane_ui, &theme, 38, 20);
    ui.scroll_offset = target_frame.scroll_offset;
    ui.scroll_ease.arm();
    ui.scroll_ease.observe(ui.scroll_offset, ms(0));
    let first = compose_lines(&snapshot, None, &ui, &pane_ui, &theme, 38, 20);
    assert_eq!(first.interactions.visible_row_span().unwrap().0, 0);
    for elapsed in [0, 16, 48, 100] {
        ui.frame_ms = ms(elapsed);
        let frame = compose_lines(&snapshot, None, &ui, &pane_ui, &theme, 38, 20);
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
    let mut ui = ViewState {
        scroll_offset: 3,
        ..Default::default()
    };
    let mut pane_ui = UiState::default();
    ui.scroll_ease.arm();
    ui.scroll_ease.observe(3, ms(0));
    let mut terminal = Terminal::new(TestBackend::new(38, 20)).unwrap();
    draw_to_terminal(&mut terminal, &snapshot, None, &mut ui, &mut pane_ui).unwrap();
    assert_eq!(ui.scroll_offset, 3);
    assert!(!ui.scroll_ease.in_flight());
}

#[test]
fn no_selection_holds_the_clamped_scroll_after_own_input() {
    let snapshot = overflowing_fleet();
    let mut ui = ViewState {
        scroll_offset: 4,
        viewport: ViewportRequest {
            own_input_reveal: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let pane_ui = UiState::default();
    let theme = Theme::fixed(false);
    let composed = compose_lines(&snapshot, None, &ui, &pane_ui, &theme, 38, 20);
    assert_eq!(composed.scroll_offset, 4);
    ui.scroll_offset = usize::MAX;
    let clamped = compose_lines(&snapshot, None, &ui, &pane_ui, &theme, 38, 20);
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
fn entry_reveal_nudges_both_lines_inside_a_card_taller_than_the_viewport() {
    let parent = agent(
        "parent",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        None,
        Some("delegate the scan"),
    );
    let mut snapshot = snapshot_with(vec![parent]);
    let panes: Vec<PaneId> = (0..20)
        .map(|index| format!("zellij:terminal_child_{index}").parse().unwrap())
        .collect();
    let card = Arc::make_mut(&mut Arc::make_mut(&mut snapshot.worktree_groups)[0].rows)[0]
        .as_agent_mut()
        .unwrap();
    card.sub_agents = panes
        .iter()
        .enumerate()
        .map(|(index, pane)| {
            serde_json::from_value(serde_json::json!({
                "id": format!("child-{index}"), "kind": "claude", "name": "worker",
                "status": "running", "last_activity": snapshot.now, "pane": pane, "model": "Haiku",
            }))
            .unwrap()
        })
        .collect();
    let target = HitTarget::SubAgentPane(panes.last().unwrap().clone());
    let view = ViewState {
        selected_index: Some(0),
        entry_cursor: Some(EntryCursor::SubAgent(panes.last().unwrap().clone())),
        viewport: ViewportRequest {
            own_input_reveal: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let theme = Theme::fixed(false);
    let all = compose_lines(&snapshot, None, &view, &UiState::default(), &theme, 54, 200);
    let last_entry =
        usize::from(all.interactions.line_for_target(&target).unwrap().1) - all.top_height;
    let frame = compose_lines(&snapshot, None, &view, &UiState::default(), &theme, 54, 18);
    let viewport = 18 - frame.top_height - frame.bottom_height;
    assert_eq!(
        frame.scroll_offset,
        (last_entry + 2).saturating_sub(viewport)
    );
    let (column, line) = frame.interactions.line_for_target(&target).unwrap();
    assert_eq!(frame.interactions.target_at(column, line + 1), Some(target));
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
        &ViewState {
            selected_index: Some(0),
            scrollbar: scrolled_fade(0, 0),
            ..Default::default()
        },
        &UiState::default(),
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
        &ViewState {
            selected_index: Some(0),
            toggles: PresentationToggles {
                help_visible: true,
                ..Default::default()
            },
            ..Default::default()
        },
        &UiState::default(),
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
            .contains("R reload"),
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
            &ViewState {
                selected_index: Some(0),
                toggles: PresentationToggles {
                    help_visible: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            &UiState::default(),
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
    let ui = ViewState {
        selected_index: Some(5),
        viewport: ViewportRequest {
            own_input_reveal: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let pane_ui = UiState::default();
    let rendered =
        snapshot_to_screen_with_alert_and_ui(&overflowing_fleet(), None, &ui, &pane_ui, 38, 29);
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
    let expected = snapshot_to_screen_with_alert_and_ui(&no_bar, None, &ui, &pane_ui, 38, 29);
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

    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &selected_ui(),
        &UiState::default(),
        54,
        24,
    );
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
        &ViewState {
            selected_index: Some(0),
            scroll_offset: 6,
            viewport: ViewportRequest {
                offset_authoritative: true,
                ..Default::default()
            },
            ..Default::default()
        },
        &UiState::default(),
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
    let settled_ui = ViewState {
        selected_index: Some(0),
        scrollbar: scrolled_fade(0, 0),
        animation_phase: 11,
        frame_ms: ms(1100),
        ..Default::default()
    };
    let pane_settled_ui = UiState::default();
    let settled = snapshot_to_screen_with_alert_and_ui(
        &overflowing_fleet(),
        None,
        &settled_ui,
        &pane_settled_ui,
        38,
        21,
    );
    let mut no_bar = overflowing_fleet();
    no_bar.theme.display.scrollbar = ScrollbarMode::Never;
    let expected =
        snapshot_to_screen_with_alert_and_ui(&no_bar, None, &settled_ui, &pane_settled_ui, 38, 21);
    assert_eq!(
        settled, expected,
        "the settle window has passed — no scrollbar overlay"
    );

    let mut snapshot = overflowing_fleet();
    snapshot.theme.display.scrollbar = ScrollbarMode::Always;
    let always = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &selected_ui(),
        &UiState::default(),
        38,
        21,
    );
    assert!(
        line_containing(&always, "⑂ alpha").ends_with('▐'),
        "always mode pins the bar with no activity:\n{always}"
    );

    let mut snapshot = overflowing_fleet();
    snapshot.theme.display.scrollbar = ScrollbarMode::Never;
    let never = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &ViewState {
            selected_index: Some(0),
            scroll_offset: 9,
            scrollbar: scrolled_fade(9, 0),
            viewport: ViewportRequest {
                offset_authoritative: true,
                ..Default::default()
            },
            ..Default::default()
        },
        &UiState::default(),
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

/// Insert a never-prompted idle card `id` at `at` in group `group`, as a launch's first fold
/// delivers it.
fn bear(snapshot: &mut SidebarSnapshot, group: usize, at: usize, id: &str) {
    let groups = std::sync::Arc::make_mut(&mut snapshot.worktree_groups);
    let rows = std::sync::Arc::make_mut(&mut groups[group].rows);
    let mut row = rows[0].clone();
    row.id = id.to_owned();
    row.unread = false;
    let card = row.as_agent_mut().unwrap();
    card.status = AgentStatus::Idle;
    card.phase = crate::agents::TurnPhase::Idle;
    card.handle = Some(id.to_owned());
    card.task = Some(format!("{id}-task"));
    rows.insert(at, row);
}

/// The card's first prompt: it leaves `Fresh` and draws its working height.
fn engage(snapshot: &mut SidebarSnapshot, id: &str) {
    let row = snapshot.rows_mut().find(|row| row.id == id).unwrap();
    let card = row.as_agent_mut().unwrap();
    card.status = AgentStatus::Running;
    card.phase = crate::agents::TurnPhase::Acting;
    card.prompt = Some("go".to_owned());
}

fn select_id(snapshot: &SidebarSnapshot, view: &mut ViewState, id: &str) {
    view.selected_index = view.visible_roster(snapshot).ordinal_of_id(id);
    assert!(view.selected_index.is_some(), "{id} is listed");
}

/// The focus reached the newborn `id`, as reconciliation hands it to the next draw.
fn focus_newborn(snapshot: &SidebarSnapshot, view: &mut ViewState, id: &str) {
    view.newborns.insert(
        id.to_owned(),
        crate::sidebar_pane::render::Newborn {
            born_ms: 0,
            revealed: true,
            laid_out: false,
        },
    );
    select_id(snapshot, view, id);
    view.request(ViewportIntent::RevealBirth(id.to_owned()));
}

fn draw_lines(
    snapshot: &SidebarSnapshot,
    view: &mut ViewState,
    pane_ui: &mut UiState,
    height: u16,
) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(54, height)).unwrap();
    draw_to_terminal(&mut terminal, snapshot, None, view, pane_ui).unwrap();
    (0..height)
        .map(|y| {
            (0..54)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect()
        })
        .collect()
}

/// Draw once more after the glide's duration, so the frame shows the target.
fn settle(
    snapshot: &SidebarSnapshot,
    view: &mut ViewState,
    pane_ui: &mut UiState,
    height: u16,
) -> Vec<String> {
    view.frame_ms = FrameTime::from_millis(
        u64::try_from(
            view.frame_ms
                .elapsed_since(FrameTime::default())
                .as_millis(),
        )
        .unwrap()
            + 200,
    );
    draw_lines(snapshot, view, pane_ui, height)
}

fn row_with(lines: &[String], needle: &str) -> Option<usize> {
    lines.iter().position(|line| line.contains(needle))
}

/// The pane rows of the last draw's scroll zone.
fn zone(
    snapshot: &SidebarSnapshot,
    view: &ViewState,
    pane_ui: &UiState,
    height: u16,
) -> std::ops::Range<usize> {
    let frame = compose_lines(
        snapshot,
        None,
        view,
        pane_ui,
        &Theme::fixed(false),
        54,
        height,
    );
    pane_ui.scroll_zone_top..usize::from(height) - frame.bottom_height
}

fn lines_of(map: &[Option<usize>], ordinal: usize) -> usize {
    map.iter().filter(|entry| **entry == Some(ordinal)).count()
}

/// Every line of `id`, its group header when it leads the group, is in the last frame.
fn shown_whole(snapshot: &SidebarSnapshot, view: &ViewState, pane_ui: &UiState, id: &str) -> bool {
    let ordinal = view.visible_roster(snapshot).ordinal_of_id(id).unwrap();
    let tall = compose_lines(snapshot, None, view, pane_ui, &Theme::fixed(false), 54, 200);
    lines_of(pane_ui.interactions.row_map(), ordinal)
        == lines_of(tall.interactions.row_map(), ordinal)
}

fn bar_awake(snapshot: &SidebarSnapshot, view: &ViewState) -> bool {
    view.scrollbar
        .visible(view.frame_ms, snapshot.theme.display.resolved_refresh_ms())
}

/// The shortest pane whose scroll zone has `viewport` rows.
fn height_for_viewport(snapshot: &SidebarSnapshot, view: &ViewState, viewport: usize) -> u16 {
    (8..80)
        .find(|&height| {
            let frame = compose_lines(
                snapshot,
                None,
                view,
                &UiState::default(),
                &Theme::fixed(false),
                54,
                height,
            );
            usize::from(height) - frame.top_height - frame.bottom_height == viewport
        })
        .expect("a pane with that viewport")
}

fn scrolled_to(
    snapshot: &SidebarSnapshot,
    view: &mut ViewState,
    pane_ui: &mut UiState,
    height: u16,
    offset: usize,
) -> Vec<String> {
    view.scroll_offset = offset;
    view.request(ViewportIntent::Wheel);
    draw_lines(snapshot, view, pane_ui, height);
    view.scroll_ease.cancel();
    draw_lines(snapshot, view, pane_ui, height)
}

#[test]
fn a_focused_newborn_above_the_anchor_is_drawn_whole_beneath_its_header() {
    for scrolled in [false, true] {
        let mut snapshot = overflowing_fleet();
        let mut view = ViewState::default();
        let mut pane_ui = UiState::default();
        let (group, header, anchor) = if scrolled {
            (1, "beta", "codex-4")
        } else {
            (0, "alpha", "codex-2")
        };
        select_id(&snapshot, &mut view, anchor);
        let before = scrolled_to(
            &snapshot,
            &mut view,
            &mut pane_ui,
            22,
            if scrolled { 12 } else { 0 },
        );
        assert_eq!(row_with(&before, header).is_none(), scrolled);
        bear(&mut snapshot, group, 0, "babe");
        focus_newborn(&snapshot, &mut view, "babe");
        draw_lines(&snapshot, &mut view, &mut pane_ui, 22);
        let lines = settle(&snapshot, &mut view, &mut pane_ui, 22);
        let header_row = row_with(&lines, header).expect("header drawn");
        assert_eq!(row_with(&lines, "babe"), Some(header_row + 1));
        assert!(shown_whole(&snapshot, &view, &pane_ui, "babe"));
        assert_eq!(view.birth_hold.as_deref(), Some("babe"));
        if scrolled {
            assert_eq!(header_row, pane_ui.scroll_zone_top, "least move up");
            assert!(bar_awake(&snapshot, &view), "the reveal moved the view");
        } else {
            assert_eq!(view.scroll_offset, 0, "nothing needed to move");
            assert!(!bar_awake(&snapshot, &view));
        }
    }
}

#[test]
fn a_focused_newborn_below_the_view_reserves_its_working_height() {
    for start in [0, 2] {
        let mut snapshot = overflowing_fleet();
        let mut view = ViewState::default();
        let mut pane_ui = UiState::default();
        select_id(&snapshot, &mut view, "codex-0");
        let before = scrolled_to(&snapshot, &mut view, &mut pane_ui, 22, start);
        assert!(row_with(&before, "beta").is_none(), "beta starts below");
        bear(&mut snapshot, 1, 0, "babe");
        focus_newborn(&snapshot, &mut view, "babe");
        draw_lines(&snapshot, &mut view, &mut pane_ui, 22);
        let lines = settle(&snapshot, &mut view, &mut pane_ui, 22);
        let identity = row_with(&lines, "babe").expect("revealed");
        assert_eq!(row_with(&lines, "beta"), Some(identity - 1));
        let zone = zone(&snapshot, &view, &pane_ui, 22);
        assert_eq!(
            identity + 3,
            zone.end - 1,
            "room for four lines, by the least move"
        );
        let offset = view.scroll_offset;
        engage(&mut snapshot, "babe");
        let engaged = draw_lines(&snapshot, &mut view, &mut pane_ui, 22);
        assert_eq!(view.scroll_offset, offset, "the first prompt moves nothing");
        assert_eq!(row_with(&engaged, "babe"), Some(identity));
        assert!(shown_whole(&snapshot, &view, &pane_ui, "babe"));
        assert!(view.birth_hold.is_none(), "the hold ends with Fresh");
    }
}

#[test]
fn a_focused_newborn_last_in_the_roster_is_whole_once_it_engages() {
    let mut snapshot = overflowing_fleet();
    let mut view = ViewState::default();
    let mut pane_ui = UiState::default();
    select_id(&snapshot, &mut view, "codex-0");
    draw_lines(&snapshot, &mut view, &mut pane_ui, 22);
    bear(&mut snapshot, 1, 3, "babe");
    focus_newborn(&snapshot, &mut view, "babe");
    draw_lines(&snapshot, &mut view, &mut pane_ui, 22);
    settle(&snapshot, &mut view, &mut pane_ui, 22);
    assert!(shown_whole(&snapshot, &view, &pane_ui, "babe"));
    assert_eq!(view.birth_hold.as_deref(), Some("babe"));
    engage(&mut snapshot, "babe");
    draw_lines(&snapshot, &mut view, &mut pane_ui, 22);
    assert!(
        shown_whole(&snapshot, &view, &pane_ui, "babe"),
        "whole on the paint it engages"
    );
    settle(&snapshot, &mut view, &mut pane_ui, 22);
    assert!(shown_whole(&snapshot, &view, &pane_ui, "babe"));
    assert!(view.birth_hold.is_none());
}

#[test]
fn the_birth_hold_keeps_the_header_and_then_yields_to_the_card() {
    let mut snapshot = overflowing_fleet();
    let mut view = ViewState::default();
    let mut pane_ui = UiState::default();
    select_id(&snapshot, &mut view, "codex-0");
    draw_lines(&snapshot, &mut view, &mut pane_ui, 22);
    bear(&mut snapshot, 1, 0, "babe");
    focus_newborn(&snapshot, &mut view, "babe");
    draw_lines(&snapshot, &mut view, &mut pane_ui, 22);
    let revealed = settle(&snapshot, &mut view, &mut pane_ui, 22);
    assert!(view.scroll_offset > 0);
    let header = row_with(&revealed, "beta").unwrap();
    let identity = row_with(&revealed, "babe").unwrap();

    bear(&mut snapshot, 1, 0, "kid");
    view.newborns.insert(
        "kid".into(),
        crate::sidebar_pane::render::Newborn {
            born_ms: 0,
            revealed: false,
            laid_out: false,
        },
    );
    select_id(&snapshot, &mut view, "babe");
    let held = draw_lines(&snapshot, &mut view, &mut pane_ui, 22);
    assert_eq!(row_with(&held, "beta"), Some(header), "the header holds");
    assert!(
        row_with(&held, "babe").unwrap() > identity,
        "the newborn moves down"
    );

    engage(&mut snapshot, "babe");
    let engaged = draw_lines(&snapshot, &mut view, &mut pane_ui, 22);
    assert!(view.birth_hold.is_none());
    let card = row_with(&engaged, "babe").unwrap();
    bear(&mut snapshot, 1, 0, "kid-2");
    select_id(&snapshot, &mut view, "babe");
    let after = draw_lines(&snapshot, &mut view, &mut pane_ui, 22);
    assert_eq!(
        row_with(&after, "babe"),
        Some(card),
        "the card anchors again"
    );
}

#[test]
fn a_reveal_taller_than_the_view_lands_by_header_card_or_first_line() {
    // (newborn's place in beta, viewport rows, what lands on the top row or fits)
    for (at, viewport) in [(0, 4), (1, 5), (1, 3)] {
        let mut snapshot = overflowing_fleet();
        let mut view = ViewState::default();
        let mut pane_ui = UiState::default();
        select_id(&snapshot, &mut view, "codex-0");
        let height = height_for_viewport(&snapshot, &view, viewport);
        draw_lines(&snapshot, &mut view, &mut pane_ui, height);
        bear(&mut snapshot, 1, at, "babe");
        focus_newborn(&snapshot, &mut view, "babe");
        draw_lines(&snapshot, &mut view, &mut pane_ui, height);
        let lines = settle(&snapshot, &mut view, &mut pane_ui, height);
        let zone = zone(&snapshot, &view, &pane_ui, height);
        assert_eq!(zone.len(), viewport);
        let identity = row_with(&lines, "babe").expect("revealed");
        match (at, viewport) {
            (0, _) => assert_eq!(
                row_with(&lines, "beta"),
                Some(zone.start),
                "the header lands on the top row"
            ),
            (_, 5) => assert_eq!(identity + 3, zone.end - 1, "the card alone, whole"),
            _ => assert_eq!(identity, zone.start, "the card's first line on top"),
        }
    }
    // A team leader: its group's pipeline line sits between the header and the card. A second
    // group makes the worktree header draw.
    let mut snapshot = super::worktree::pipeline_snapshot();
    let groups = std::sync::Arc::make_mut(&mut snapshot.worktree_groups);
    let mut solo = groups[0].clone();
    solo.key = "solo".to_owned();
    solo.label = "solo".to_owned();
    solo.pipeline = None;
    let mut row = solo.rows[0].clone();
    row.id = "solo".to_owned();
    let card = row.as_agent_mut().unwrap();
    card.team = None;
    card.handle = Some("solo".to_owned());
    solo.rows = std::sync::Arc::new(vec![row]);
    groups.push(solo);
    let mut view = ViewState::default();
    let mut pane_ui = UiState::default();
    select_id(&snapshot, &mut view, "coder");
    let height = height_for_viewport(&snapshot, &view, 5);
    draw_lines(&snapshot, &mut view, &mut pane_ui, height);
    bear(&mut snapshot, 0, 0, "babe");
    focus_newborn(&snapshot, &mut view, "babe");
    draw_lines(&snapshot, &mut view, &mut pane_ui, height);
    let lines = settle(&snapshot, &mut view, &mut pane_ui, height);
    let zone = zone(&snapshot, &view, &pane_ui, height);
    let identity = row_with(&lines, "babe").expect("revealed");
    assert!(
        lines[zone.start].contains("⑂ pipeline"),
        "the header lands on the top row: {:?}",
        &lines[zone.clone()]
    );
    assert!(lines[zone.start + 1].contains("Implement"));
    assert_eq!(
        identity,
        zone.start + 2,
        "the card follows the pipeline line"
    );
}

#[test]
fn a_short_reveal_glides_a_long_one_lands_and_a_wheel_tick_wins() {
    for (viewport, glides) in [(12, true), (6, false)] {
        let mut snapshot = overflowing_fleet();
        let mut view = ViewState::default();
        let mut pane_ui = UiState::default();
        select_id(&snapshot, &mut view, "codex-0");
        let height = height_for_viewport(&snapshot, &view, viewport);
        draw_lines(&snapshot, &mut view, &mut pane_ui, height);
        let start = view.scroll_offset;
        bear(&mut snapshot, 1, 0, "babe");
        focus_newborn(&snapshot, &mut view, "babe");
        view.frame_ms = ms(1000);
        draw_lines(&snapshot, &mut view, &mut pane_ui, height);
        let target = view.scroll_offset;
        assert_eq!(target - start <= viewport, glides, "{viewport}");
        assert_eq!(view.scroll_ease.in_flight(), glides, "{viewport}");
        if glides {
            assert!(target - start >= 2);
            let mid = view.scroll_ease.visible(target, ms(1050));
            assert!(start < mid && mid < target, "{start} < {mid} < {target}");
        }
    }
    let mut snapshot = overflowing_fleet();
    let mut view = ViewState::default();
    let mut pane_ui = UiState::default();
    select_id(&snapshot, &mut view, "codex-0");
    draw_lines(&snapshot, &mut view, &mut pane_ui, 22);
    bear(&mut snapshot, 1, 0, "babe");
    focus_newborn(&snapshot, &mut view, "babe");
    view.scroll_offset = 1;
    view.request(ViewportIntent::Wheel);
    let lines = draw_lines(&snapshot, &mut view, &mut pane_ui, 22);
    assert_eq!(view.scroll_offset, 1, "the wheel wins");
    assert!(row_with(&lines, "babe").is_none());
    assert!(view.birth_hold.is_none());
}

#[test]
fn a_reveal_measured_with_the_banner_puts_the_header_under_it() {
    let mut snapshot = overflowing_fleet();
    {
        let rows = std::sync::Arc::make_mut(
            &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[1].rows,
        );
        let lead = rows.last_mut().unwrap();
        lead.unread = true;
        lead.as_agent_mut().unwrap().status = AgentStatus::Waiting;
    }
    let mut view = ViewState::default();
    let mut pane_ui = UiState::default();
    select_id(&snapshot, &mut view, "codex-5");
    scrolled_to(&snapshot, &mut view, &mut pane_ui, 22, usize::MAX);
    assert!(!pane_ui.banner_shown, "the lead starts in view");
    bear(&mut snapshot, 0, 0, "babe");
    focus_newborn(&snapshot, &mut view, "babe");
    draw_lines(&snapshot, &mut view, &mut pane_ui, 22);
    let lines = settle(&snapshot, &mut view, &mut pane_ui, 22);
    assert!(pane_ui.banner_shown, "the reveal raised the banner");
    assert_eq!(row_with(&lines, "alpha"), Some(pane_ui.scroll_zone_top));
    assert_eq!(row_with(&lines, "babe"), Some(pane_ui.scroll_zone_top + 1));
}

#[test]
fn an_unfocused_newborn_above_keeps_a_top_resting_view_at_the_top() {
    for start in [0, 3] {
        let mut snapshot = overflowing_fleet();
        let mut view = ViewState::default();
        let mut pane_ui = UiState::default();
        select_id(&snapshot, &mut view, "codex-2");
        let before = scrolled_to(&snapshot, &mut view, &mut pane_ui, 30, start);
        let task_0 = row_with(&before, "task-0");
        let header = row_with(&before, "alpha");
        let task_2 = row_with(&before, "task-2").unwrap();
        bear(&mut snapshot, 0, 0, "babe");
        view.newborns.insert(
            "babe".into(),
            crate::sidebar_pane::render::Newborn {
                born_ms: 0,
                revealed: false,
                laid_out: false,
            },
        );
        select_id(&snapshot, &mut view, "codex-2");
        let after = draw_lines(&snapshot, &mut view, &mut pane_ui, 30);
        if start == 0 {
            let born = lines_of(
                compose_lines(
                    &snapshot,
                    None,
                    &view,
                    &pane_ui,
                    &Theme::fixed(false),
                    54,
                    200,
                )
                .interactions
                .row_map(),
                0,
            ) - 1;
            assert_eq!(view.scroll_offset, 0, "the view stays at the top");
            assert_eq!(row_with(&after, "alpha"), header);
            assert_eq!(row_with(&after, "task-0"), Some(task_0.unwrap() + born));
            assert_eq!(row_with(&after, "task-2"), Some(task_2 + born));
            assert!(!pane_ui.anchor_clamped);
        } else {
            assert_eq!(row_with(&after, "task-2"), Some(task_2), "the anchor holds");
        }
    }
}

#[test]
fn a_newborn_that_loses_the_selection_or_leaves_before_its_paint_moves_nothing() {
    for leaves in [false, true] {
        let mut snapshot = overflowing_fleet();
        let mut view = ViewState::default();
        let mut pane_ui = UiState::default();
        select_id(&snapshot, &mut view, "codex-0");
        let before = draw_lines(&snapshot, &mut view, &mut pane_ui, 22);
        bear(&mut snapshot, 1, 0, "babe");
        focus_newborn(&snapshot, &mut view, "babe");
        if leaves {
            snapshot = overflowing_fleet();
        }
        select_id(&snapshot, &mut view, "codex-0");
        let after = settle(&snapshot, &mut view, &mut pane_ui, 22);
        let cards = pane_ui.scroll_zone_top..;
        assert_eq!(after[cards.clone()], before[cards], "leaves {leaves}");
        assert!(view.birth_hold.is_none(), "leaves {leaves}");
    }
}

/// A worktree group `key` holding only the newborn `id`, inserted at group `index`.
fn bear_group(snapshot: &mut SidebarSnapshot, index: usize, key: &str, id: &str) {
    let groups = std::sync::Arc::make_mut(&mut snapshot.worktree_groups);
    let mut group = groups[0].clone();
    group.key = key.to_owned();
    group.label = key.to_owned();
    group.rows = std::sync::Arc::new(vec![groups[0].rows[0].clone()]);
    groups.insert(index, group);
    bear(snapshot, index, 0, id);
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[index].rows,
    )
    .truncate(1);
}

#[test]
fn a_new_group_born_above_a_top_resting_view_keeps_it_at_the_top() {
    // (where the new group lands, the anchor, a line above the new group, lines below it)
    for (index, anchor, above, below) in [
        (0, "codex-2", None, ["task-0", "task-2"]),
        (1, "codex-3", Some("task-0"), ["beta", "task-3"]),
    ] {
        let mut snapshot = overflowing_fleet();
        let mut view = ViewState::default();
        let mut pane_ui = UiState::default();
        select_id(&snapshot, &mut view, anchor);
        let before = scrolled_to(&snapshot, &mut view, &mut pane_ui, 30, 0);
        bear_group(&mut snapshot, index, "gamma", "babe");
        view.newborns.insert(
            "babe".into(),
            crate::sidebar_pane::render::Newborn {
                born_ms: 0,
                revealed: false,
                laid_out: false,
            },
        );
        select_id(&snapshot, &mut view, anchor);
        let after = draw_lines(&snapshot, &mut view, &mut pane_ui, 30);
        let ordinal = view
            .visible_roster(&snapshot)
            .ordinal_of_id("babe")
            .unwrap();
        let tall = compose_lines(
            &snapshot,
            None,
            &view,
            &pane_ui,
            &Theme::fixed(false),
            54,
            200,
        );
        let group_and_gap = lines_of(tall.interactions.row_map(), ordinal) + 1;
        assert_eq!(view.scroll_offset, 0, "the view stays at the top: {index}");
        assert!(!pane_ui.anchor_clamped, "{index}");
        if let Some(above) = above {
            assert_eq!(row_with(&after, above), row_with(&before, above), "{index}");
        }
        for needle in below {
            let moved = row_with(&before, needle).unwrap() + group_and_gap;
            assert_eq!(row_with(&after, needle), Some(moved), "{needle}: {index}");
        }
    }
}

/// Remember `id` as new to the view, unfocused, as reconciliation records it.
fn record_newborn(view: &mut ViewState, id: &str) {
    view.newborns.insert(
        id.to_owned(),
        crate::sidebar_pane::render::Newborn {
            born_ms: 0,
            revealed: false,
            laid_out: false,
        },
    );
}

/// The offset after one draw that changes `snapshot` by `change`, with `born` recorded new.
fn offset_after(
    snapshot: &SidebarSnapshot,
    anchor: &str,
    height: u16,
    born: Option<&str>,
    change: impl Fn(&mut SidebarSnapshot),
) -> (usize, Vec<String>, Vec<String>) {
    let mut snapshot = snapshot.clone();
    let mut view = ViewState::default();
    let mut pane_ui = UiState::default();
    select_id(&snapshot, &mut view, anchor);
    let before = scrolled_to(&snapshot, &mut view, &mut pane_ui, height, 0);
    change(&mut snapshot);
    if let Some(id) = born {
        record_newborn(&mut view, id);
    }
    select_id(&snapshot, &mut view, anchor);
    let after = draw_lines(&snapshot, &mut view, &mut pane_ui, height);
    (view.scroll_offset, before, after)
}

#[test]
fn a_rowless_group_above_growth_and_a_newborn_below_move_the_view_as_the_base() {
    let mut snapshot = overflowing_fleet();
    snapshot.theme.display.card_density = crate::config::CardDensityMode::Compact;
    let lane = crate::sidebar::scenario::Scenario::parse("workflow", "preset workflow")
        .unwrap()
        .at(Duration::ZERO)
        .worktree_groups[0]
        .clone();
    assert!(lane.rows.is_empty() && lane.workflow.is_some());
    let label = lane.label.clone();
    Arc::make_mut(&mut snapshot.worktree_groups).insert(0, lane);
    let change = |snapshot: &mut SidebarSnapshot| {
        snapshot.theme.display.card_density = crate::config::CardDensityMode::Expanded;
        bear(snapshot, 2, 3, "babe");
    };
    let (base, before, _) = offset_after(&snapshot, "codex-1", 22, None, change);
    let (offset, _, after) = offset_after(&snapshot, "codex-1", 22, Some("babe"), change);
    assert!(row_with(&before, &label) < row_with(&before, "task-1"));
    assert!(row_with(&after, "babe").is_none(), "born below the view");
    assert!(base > 0, "growth above moves the view");
    assert_eq!(offset, base, "the rowless group is no birth");
}

#[test]
fn a_newborn_drawn_below_and_then_resorted_above_moves_the_view_as_a_resort() {
    let snapshot = overflowing_fleet();
    for born in [None, Some("babe")] {
        let mut snapshot = snapshot.clone();
        let mut view = ViewState::default();
        let mut pane_ui = UiState::default();
        select_id(&snapshot, &mut view, "codex-3");
        scrolled_to(&snapshot, &mut view, &mut pane_ui, 30, 0);
        bear(&mut snapshot, 1, 3, "babe");
        if let Some(id) = born {
            record_newborn(&mut view, id);
        }
        select_id(&snapshot, &mut view, "codex-3");
        let first = draw_lines(&snapshot, &mut view, &mut pane_ui, 30);
        assert!(
            row_with(&first, "babe").is_none(),
            "first drawn below the view"
        );
        let task_3 = row_with(&first, "task-3").expect("the anchor is in view");
        let rows = Arc::make_mut(&mut Arc::make_mut(&mut snapshot.worktree_groups)[1].rows);
        let babe = rows.pop().unwrap();
        rows.insert(0, babe);
        select_id(&snapshot, &mut view, "codex-3");
        let resorted = draw_lines(&snapshot, &mut view, &mut pane_ui, 30);
        assert_eq!(
            row_with(&resorted, "task-3"),
            Some(task_3),
            "the anchor keeps its row: {born:?}"
        );
        assert!(view.scroll_offset > 0, "{born:?}");
    }
}

#[test]
fn a_team_born_into_a_group_above_a_top_resting_view_counts_its_pipeline_line() {
    let mut snapshot = overflowing_fleet();
    let mut view = ViewState::default();
    let mut pane_ui = UiState::default();
    select_id(&snapshot, &mut view, "codex-2");
    let before = scrolled_to(&snapshot, &mut view, &mut pane_ui, 30, 0);
    let mut pipeline = super::worktree::pipeline_snapshot().worktree_groups[0]
        .pipeline
        .clone();
    pipeline.as_mut().unwrap().owner = None;
    for id in ["mate", "lead"] {
        bear(&mut snapshot, 0, 0, id);
        record_newborn(&mut view, id);
    }
    let groups = Arc::make_mut(&mut snapshot.worktree_groups);
    groups[0].team = Some("forge".to_owned());
    groups[0].pipeline = pipeline;
    for row in Arc::make_mut(&mut groups[0].rows).iter_mut().take(2) {
        row.as_agent_mut().unwrap().team = Some("forge".to_owned());
    }
    select_id(&snapshot, &mut view, "codex-2");
    let after = draw_lines(&snapshot, &mut view, &mut pane_ui, 30);
    assert!(
        after.iter().any(|line| line.contains("Implement")),
        "{after:#?}"
    );
    let tall = compose_lines(
        &snapshot,
        None,
        &view,
        &pane_ui,
        &Theme::fixed(false),
        54,
        200,
    );
    let roster = view.visible_roster(&snapshot);
    let born: usize = ["lead", "mate"]
        .map(|id| {
            lines_of(
                tall.interactions.row_map(),
                roster.ordinal_of_id(id).unwrap(),
            )
        })
        .iter()
        .sum();
    // The header line now maps the lead but was drawn before; the strip line is new.
    let moved = born;
    assert_eq!(view.scroll_offset, 0, "the view stays at the top");
    for needle in ["task-0", "task-2"] {
        let expected = row_with(&before, needle).unwrap() + moved;
        assert_eq!(row_with(&after, needle), Some(expected), "{needle}");
    }
}

#[test]
fn a_panes_first_draw_lands_a_birth_reveal_in_one_frame() {
    let mut snapshot = overflowing_fleet();
    let mut view = ViewState::default();
    select_id(&snapshot, &mut view, "codex-0");
    draw_lines(&snapshot, &mut view, &mut UiState::default(), 22);
    bear(&mut snapshot, 1, 0, "babe");
    focus_newborn(&snapshot, &mut view, "babe");
    let mut fresh_pane = UiState::default();
    draw_lines(&snapshot, &mut view, &mut fresh_pane, 22);
    assert!(view.scroll_offset > 0, "the reveal moved the view");
    assert!(!view.scroll_ease.in_flight(), "no glide on a first draw");
    assert!(shown_whole(&snapshot, &view, &fresh_pane, "babe"));
}

#[test]
fn the_birth_hold_at_the_top_keeps_the_header_and_then_yields_to_the_card() {
    let mut snapshot = overflowing_fleet();
    let mut view = ViewState::default();
    let mut pane_ui = UiState::default();
    select_id(&snapshot, &mut view, "codex-2");
    scrolled_to(&snapshot, &mut view, &mut pane_ui, 30, 0);
    bear(&mut snapshot, 0, 0, "babe");
    focus_newborn(&snapshot, &mut view, "babe");
    draw_lines(&snapshot, &mut view, &mut pane_ui, 30);
    let revealed = settle(&snapshot, &mut view, &mut pane_ui, 30);
    assert_eq!(view.scroll_offset, 0);
    let header = row_with(&revealed, "alpha").unwrap();
    let identity = row_with(&revealed, "babe").unwrap();

    bear(&mut snapshot, 0, 0, "kid");
    record_newborn(&mut view, "kid");
    select_id(&snapshot, &mut view, "babe");
    let held = draw_lines(&snapshot, &mut view, &mut pane_ui, 30);
    assert_eq!(view.scroll_offset, 0);
    assert_eq!(row_with(&held, "alpha"), Some(header), "the header holds");
    let kid = row_with(&held, "kid").unwrap();
    assert_eq!(kid, identity, "the teammate takes the newborn's row");
    assert!(
        row_with(&held, "babe").unwrap() > identity,
        "the newborn moves down"
    );
    assert_eq!(view.birth_hold.as_deref(), Some("babe"));

    engage(&mut snapshot, "babe");
    let engaged = draw_lines(&snapshot, &mut view, &mut pane_ui, 30);
    assert!(view.birth_hold.is_none());
    let card = row_with(&engaged, "babe").unwrap();
    engage(&mut snapshot, "kid");
    select_id(&snapshot, &mut view, "babe");
    let grown = draw_lines(&snapshot, &mut view, &mut pane_ui, 30);
    assert_eq!(
        row_with(&grown, "babe"),
        Some(card),
        "the card anchors again"
    );
    assert!(view.scroll_offset > 0, "growth above moves the view");
}
