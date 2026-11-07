use super::*;
use crate::sidebar_pane::app::card_commands::reduce;
use crate::sidebar_pane::commands::{CardDirection, CardReply, CardRequest, CardTarget};

fn walk(direction: CardDirection, worktree: bool) -> CardRequest {
    CardRequest::Walk {
        direction,
        worktree,
    }
}

fn jump(view: &mut ViewState, snapshot: &SidebarSnapshot, request: CardRequest, raw: &str) {
    let target = PaneId::from_parts(MuxName::Zellij, raw);
    let (outcome, reply) = reduce(&request, view, snapshot);
    assert_eq!(reply, CardReply::Done);
    assert_eq!(outcome.effects, vec![InputEffect::Focus(target.clone())]);
    assert_eq!(view.selected_pane, Some(target));
}

#[test]
fn card_walk_wraps_skips_paneless_and_children_and_moves_at_the_press() {
    let mut snapshot = filterable_snapshot(&workspace());
    let now = snapshot.now;
    let rows = std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    );
    rows[0].as_agent_mut().unwrap().sub_agents = vec![
        serde_json::from_value(serde_json::json!({
            "id": "child", "kind": "claude", "name": "child", "status": "waiting",
            "last_activity": now, "pane": "zellij:terminal_99", "prior_turn": true,
        }))
        .unwrap(),
    ];
    let mut paneless = rows[0].clone();
    paneless.id = "paneless".into();
    paneless.pane = None;
    rows.insert(1, paneless);
    let mut view = ViewState::default();
    jump(
        &mut view,
        &snapshot,
        walk(CardDirection::Prev, false),
        "terminal_3",
    );
    assert_eq!(view.selected_index, Some(3));
    jump(
        &mut view,
        &snapshot,
        walk(CardDirection::Next, false),
        "terminal_1",
    );
    assert_eq!(view.selected_index, Some(0));
    for (raw, index) in [("terminal_2", 2), ("terminal_3", 3), ("terminal_1", 0)] {
        jump(&mut view, &snapshot, walk(CardDirection::Next, false), raw);
        assert_eq!(view.selected_index, Some(index));
        assert_eq!(view.baseline_pane, None, "no focus fold between requests");
    }
}

#[test]
fn card_walk_skips_folded_rows_and_worktree_walk_wraps_nonempty_groups() {
    let snapshot = snapshot_with_panes(
        &workspace(),
        (1..=8)
            .map(|n| pane(&format!("terminal_{n}"), "tab_0", false))
            .collect(),
    );
    let mut view = ViewState {
        selected_index: Some(5),
        selected_pane: Some(PaneId::from_parts(MuxName::Zellij, "terminal_6")),
        ..Default::default()
    };
    jump(
        &mut view,
        &snapshot,
        walk(CardDirection::Next, false),
        "terminal_1",
    );
    let snapshot = filterable_snapshot(&workspace());
    let mut view = ViewState::default();
    jump(
        &mut view,
        &snapshot,
        walk(CardDirection::Next, true),
        "terminal_1",
    );
    jump(
        &mut view,
        &snapshot,
        walk(CardDirection::Next, true),
        "terminal_3",
    );
    jump(
        &mut view,
        &snapshot,
        walk(CardDirection::Next, true),
        "terminal_1",
    );
    jump(
        &mut view,
        &snapshot,
        walk(CardDirection::Prev, true),
        "terminal_3",
    );
    view.make_up_filter.query = Some("main".into());
    view.selected_pane = None;
    view.selected_index = None;
    jump(
        &mut view,
        &snapshot,
        walk(CardDirection::Prev, true),
        "terminal_1",
    );
    jump(
        &mut view,
        &snapshot,
        walk(CardDirection::Next, true),
        "terminal_1",
    );
}

#[test]
fn card_walk_uses_committed_query_and_needs_you_and_cancels_only_the_draft() {
    let snapshot = filterable_snapshot(&workspace());
    let mut view = ViewState {
        help_visible: true,
        search_draft: Some("no matches".into()),
        ..Default::default()
    };
    view.make_up_filter.query = Some("feature".into());
    view.make_up_filter.filter = Some(BodyFilter::NeedsYou);
    jump(
        &mut view,
        &snapshot,
        walk(CardDirection::Next, false),
        "terminal_3",
    );
    assert!(!view.help_visible);
    assert!(view.search_draft.is_none());
    assert_eq!(view.make_up_filter.query.as_deref(), Some("feature"));
    assert_eq!(view.make_up_filter.filter, Some(BodyFilter::NeedsYou));
    jump(
        &mut view,
        &snapshot,
        walk(CardDirection::Next, false),
        "terminal_3",
    );
}

#[test]
fn card_slot_ignores_lenses_folds_and_freeze_and_empty_slot_changes_nothing() {
    let mut snapshot = filterable_snapshot(&workspace());
    let mut view = ViewState::default();
    view.make_up_filter.filter = Some(BodyFilter::NeedsYou);
    crate::sidebar_pane::app::layout_freeze::arm_freeze(&mut view, 0);
    std::sync::Arc::make_mut(&mut snapshot.worktree_groups).reverse();
    jump(
        &mut view,
        &snapshot,
        CardRequest::Slot { n: 1 },
        "terminal_1",
    );
    assert_eq!(view.selected_index, None, "the hidden slot still focuses");
    assert_eq!(view.make_up_filter.filter, Some(BodyFilter::NeedsYou));
    let before = (
        view.selected_index,
        view.selected_pane.clone(),
        view.browse.clone(),
    );
    let (outcome, reply) = reduce(&CardRequest::Slot { n: 9 }, &mut view, &snapshot);
    assert!(
        matches!(reply, CardReply::Refused { ref reason } if reason.contains("3 cards")),
        "{reply:?}"
    );
    assert_eq!(outcome, InputOutcome::default());
    assert_eq!(
        (
            view.selected_index,
            view.selected_pane.clone(),
            view.browse.clone()
        ),
        before
    );
}

#[test]
fn card_last_tracks_only_distinct_card_baselines_and_refuses_a_departed_row() {
    let mut snapshot = filterable_snapshot(&workspace());
    let first = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let second = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let mut view = ViewState::default();
    reconcile_selection(&mut view, &snapshot, Some(first.clone()));
    reconcile_selection(&mut view, &snapshot, Some(second.clone()));
    assert_eq!(view.previous_card, Some(first.clone()));
    reconcile_selection(&mut view, &snapshot, None);
    reconcile_selection(&mut view, &snapshot, Some(second.clone()));
    assert_eq!(view.previous_card, Some(first.clone()));
    jump(&mut view, &snapshot, CardRequest::Last, "terminal_1");
    reconcile_selection(&mut view, &snapshot, Some(first.clone()));
    jump(&mut view, &snapshot, CardRequest::Last, "terminal_2");
    reconcile_selection(&mut view, &snapshot, Some(second));
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows)
        .retain(|row| row.pane.as_ref().is_none_or(|pane| pane.pane_id != first));
    let (outcome, reply) = reduce(&CardRequest::Last, &mut view, &snapshot);
    assert!(matches!(reply, CardReply::Refused { .. }));
    assert_eq!(outcome, InputOutcome::default());
}

#[test]
fn card_needs_you_set_toggle_and_empty_refusal_preserve_the_lens() {
    let snapshot = filterable_snapshot(&workspace());
    let mut view = ViewState::default();
    view.make_up_filter.query = Some("feature".into());
    let (outcome, reply) = reduce(
        &CardRequest::FilterNeedsYou { toggle: false },
        &mut view,
        &snapshot,
    );
    assert_eq!(reply, CardReply::Done);
    assert_eq!(
        outcome.effects,
        vec![InputEffect::SyncFilter(view.make_up_filter.clone())]
    );
    assert_eq!(view.make_up_filter.filter, Some(BodyFilter::NeedsYou));
    assert_eq!(view.make_up_filter.query.as_deref(), Some("feature"));
    let (outcome, reply) = reduce(
        &CardRequest::FilterNeedsYou { toggle: false },
        &mut view,
        &snapshot,
    );
    assert!(outcome.effects.is_empty());
    assert_eq!(
        reply,
        CardReply::Refused {
            reason: "sidebar filter needs-you: already showing needs-you".to_owned(),
        }
    );
    assert_eq!(
        reduce(
            &CardRequest::FilterNeedsYou { toggle: true },
            &mut view,
            &snapshot
        )
        .1,
        CardReply::Done
    );
    assert_eq!(view.make_up_filter.filter, None);
    let empty = snapshot_with_panes(&workspace(), vec![pane("terminal_1", "tab_0", false)]);
    let before = view.make_up_filter.clone();
    let (outcome, reply) = reduce(
        &CardRequest::FilterNeedsYou { toggle: true },
        &mut view,
        &empty,
    );
    assert!(matches!(reply, CardReply::Refused { .. }));
    assert_eq!(view.make_up_filter, before);
    assert!(
        outcome.effects.is_empty(),
        "an empty toggle writes no shared filter"
    );
}

#[test]
fn card_close_target_is_the_highlight_session_key_or_process_pane() {
    let snapshot = filterable_snapshot(&workspace());
    let mut view = ViewState {
        selected_index: Some(0),
        selected_pane: Some(PaneId::from_parts(MuxName::Zellij, "terminal_1")),
        ..Default::default()
    };
    let (outcome, reply) = reduce(&CardRequest::CloseTarget, &mut view, &snapshot);
    assert_eq!(
        serde_json::to_value(&reply).unwrap(),
        serde_json::json!({"reply": "close_target", "agent": {
            "kind": "claude", "agent_id": "agent-1", "pane": "zellij:terminal_1"
        }})
    );
    assert!(outcome.effects.is_empty());
    view.selected_index = Some(1);
    view.selected_pane = Some(PaneId::from_parts(MuxName::Zellij, "terminal_2"));
    assert_eq!(
        reduce(&CardRequest::CloseTarget, &mut view, &snapshot).1,
        CardReply::CloseTarget {
            target: CardTarget::Pane(view.selected_pane.clone().unwrap())
        }
    );
}
