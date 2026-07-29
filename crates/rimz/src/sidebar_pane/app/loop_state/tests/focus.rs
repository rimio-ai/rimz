//! Focus anchors resolving into selection and scroll, the order holds that
//! keep the frame still while the user acts, and the read-mark receipts.

use super::*;

fn zellij(raw: &str) -> PaneId {
    PaneId::from_parts(crate::MuxName::Zellij, raw)
}

fn applied_focus_anchor(
    pane_id: PaneId,
    offset: usize,
    stamp_ms: u64,
    order: Option<crate::sidebar_pane::render::FrozenOrder>,
) -> crate::sidebar::focus_anchor::FocusAnchor {
    crate::sidebar::focus_anchor::FocusAnchor {
        nonce: crate::sidebar::focus_anchor::FocusNonce::new(),
        session_name: "rimz-test".to_owned(),
        pane_id,
        origin: crate::sidebar::focus_anchor::FocusOrigin::User,
        repair_generation: None,
        issued_at_ms: stamp_ms,
        applied_at_ms: Some(stamp_ms),
        state: crate::sidebar::focus_anchor::FocusIntentState::Applied,
        pre_action: Vec::new(),
        offset,
        order,
    }
}

fn requested_focus_anchor(
    pane_id: PaneId,
    offset: usize,
    stamp_ms: u64,
    order: Option<crate::sidebar_pane::render::FrozenOrder>,
) -> crate::sidebar::focus_anchor::FocusAnchor {
    let mut anchor = applied_focus_anchor(pane_id, offset, stamp_ms, order);
    anchor.applied_at_ms = None;
    anchor.state = crate::sidebar::focus_anchor::FocusIntentState::Requested;
    anchor
}

#[test]
fn fresh_focus_anchor_seeds_scroll_on_matching_fold() {
    let mut rig = Rig::new();
    let target = zellij("terminal_2");
    let stamp_ms = crate::sidebar::timing::unix_now_ms();
    crate::sidebar::focus_anchor::store(
        &rig.runtime,
        &applied_focus_anchor(target.clone(), 7, stamp_ms, None),
    )
    .expect("store anchor");
    rig.state.ui.scroll_offset = 2;
    rig.state.ui.manual_scroll = Some(crate::sidebar_pane::render::ManualScroll {
        selection_at_start: Some(zellij("terminal_1")),
    });

    let snapshot = snapshot_with_focused_pane(&rig.ws, target.clone());
    rig.fold(snapshot, PaneFrame::Fresh, SnapshotSource::Produced);

    assert_eq!(rig.state.ui.selected_pane, Some(target));
    assert_eq!(rig.state.ui.scroll_offset, 7);
    assert_eq!(rig.state.ui.manual_scroll, None);
    assert!(
        !rig.state.ui.focus_group_reveal,
        "a sidebar jump's fresh anchor suppresses external-focus group reveal"
    );
    assert_eq!(rig.state.ui.last_focus_anchor_ms, stamp_ms);
}

#[test]
fn fresh_requested_focus_anchor_installs_shared_hold_once() {
    let mut rig = Rig::new();
    let first = zellij("terminal_1");
    let target = zellij("terminal_2");
    let stamp_ms = crate::sidebar::timing::unix_now_ms();
    let mut anchor = requested_focus_anchor(
        target.clone(),
        7,
        stamp_ms,
        Some(crate::sidebar_pane::render::FrozenOrder {
            groups: vec!["/repo/main".to_owned()],
            rows: vec![
                crate::sidebar_pane::render::FrozenRow {
                    id: target.to_string(),
                    pane: Some(target.to_string()),
                },
                crate::sidebar_pane::render::FrozenRow {
                    id: first.to_string(),
                    pane: Some(first.to_string()),
                },
            ],
            visible: HashSet::from([target.to_string()]),
        }),
    );
    crate::sidebar::focus_anchor::store(&rig.runtime, &anchor).expect("store anchor");

    let snapshot = snapshot_with_focused_pane(&rig.ws, target.clone());
    rig.fold(snapshot, PaneFrame::Fresh, SnapshotSource::Produced);

    assert_eq!(rig.state.ui.selected_pane, Some(target.clone()));
    assert_eq!(rig.state.ui.scroll_offset, 7);
    assert_eq!(
        rig.state.current.worktree_groups[0]
            .rows
            .iter()
            .map(|row| row.pane.as_ref().expect("pane").pane_id.clone())
            .collect::<Vec<_>>(),
        vec![target.clone(), first],
        "fold snapshot adopts anchor row order before paint"
    );
    let hold = rig.state.ui.order_hold.as_ref().expect("shared hold");
    assert_eq!(hold.frozen.visible, HashSet::from([target.to_string()]));
    assert_eq!(
        hold.expires_ms,
        stamp_ms as i64 + crate::sidebar::timing::REORDER_HOLD.as_millis() as i64
    );

    rig.state.ui.scroll_offset = 4;
    rig.state.ui.manual_scroll = Some(crate::sidebar_pane::render::ManualScroll {
        selection_at_start: Some(target.clone()),
    });
    anchor.state = crate::sidebar::focus_anchor::FocusIntentState::Applied;
    anchor.applied_at_ms = Some(crate::sidebar::timing::unix_now_ms());
    crate::sidebar::focus_anchor::store(&rig.runtime, &anchor).expect("apply anchor");
    let snapshot = snapshot_with_focused_pane(&rig.ws, target);
    rig.fold(snapshot, PaneFrame::Fresh, SnapshotSource::Produced);

    assert_eq!(rig.state.ui.scroll_offset, 4);
    assert!(rig.state.ui.manual_scroll.is_some());
    assert_eq!(rig.state.ui.last_focus_anchor_ms, stamp_ms);
}

#[test]
fn focus_anchor_stamp_applies_once() {
    let mut rig = Rig::new();
    let target = zellij("terminal_2");
    let stamp_ms = crate::sidebar::timing::unix_now_ms();
    crate::sidebar::focus_anchor::store(
        &rig.runtime,
        &applied_focus_anchor(target.clone(), 7, stamp_ms, None),
    )
    .expect("store anchor");

    let snapshot = snapshot_with_focused_pane(&rig.ws, target.clone());
    rig.fold(snapshot, PaneFrame::Fresh, SnapshotSource::Produced);
    assert_eq!(rig.state.ui.scroll_offset, 7);

    rig.state.ui.scroll_offset = 4;
    rig.state.ui.manual_scroll = Some(crate::sidebar_pane::render::ManualScroll {
        selection_at_start: Some(target.clone()),
    });
    let snapshot = snapshot_with_focused_pane(&rig.ws, target);
    rig.fold(snapshot, PaneFrame::Fresh, SnapshotSource::Produced);

    assert_eq!(rig.state.ui.scroll_offset, 4);
    assert!(rig.state.ui.manual_scroll.is_some());
}

#[test]
fn stale_focus_anchor_fences_unchanged_observation_to_unknown() {
    let mut rig = Rig::new();
    let target = zellij("terminal_2");
    let ttl_ms = crate::sidebar::timing::FOCUS_ANCHOR_FRESH.as_millis() as u64;
    let stale_stamp = crate::sidebar::timing::unix_now_ms().saturating_sub(ttl_ms + 1);
    crate::sidebar::focus_anchor::store(
        &rig.runtime,
        &applied_focus_anchor(target.clone(), 7, stale_stamp, None),
    )
    .expect("store anchor");
    rig.state.ui.scroll_offset = 3;

    let snapshot = snapshot_with_focused_pane(&rig.ws, target);
    rig.fold(snapshot, PaneFrame::Fresh, SnapshotSource::Produced);

    assert_eq!(rig.state.ui.selected_pane, None);
    assert_eq!(rig.state.ui.scroll_offset, 3);
    assert_eq!(rig.state.ui.last_focus_anchor_ms, 0);
}

#[test]
fn superseding_client_observation_leaves_scroll_untouched() {
    let mut rig = Rig::new();
    let selected = zellij("terminal_2");
    let stamp_ms = crate::sidebar::timing::unix_now_ms();
    let mut anchor = applied_focus_anchor(zellij("terminal_1"), 7, stamp_ms, None);
    anchor.pre_action = vec![crate::mux::ClientPaneView {
        client_id: crate::mux::MuxClientId::Zellij(7),
        pane_id: anchor.pane_id.clone(),
    }];
    crate::sidebar::focus_anchor::store(&rig.runtime, &anchor).expect("store anchor");
    rig.state.ui.scroll_offset = 3;

    let mut snapshot = snapshot_with_focused_pane(&rig.ws, selected.clone());
    snapshot.presence = Some(crate::SidebarPresence::Active);
    snapshot.client_views = vec![crate::mux::ClientPaneView {
        client_id: crate::mux::MuxClientId::Zellij(7),
        pane_id: selected.clone(),
    }];
    rig.fold(snapshot, PaneFrame::Fresh, SnapshotSource::Produced);

    assert_eq!(rig.state.ui.selected_pane, Some(selected));
    assert_eq!(rig.state.ui.scroll_offset, 3);
    assert_eq!(rig.state.ui.last_focus_anchor_ms, 0);
}

#[test]
fn external_focus_change_arms_group_reveal_once() {
    let mut rig = Rig::new();
    let target = zellij("terminal_2");

    let snapshot = snapshot_with_focused_pane(&rig.ws, target.clone());
    rig.fold(snapshot, PaneFrame::Fresh, SnapshotSource::Produced);

    assert_eq!(rig.state.ui.selected_pane, Some(target.clone()));
    assert!(
        rig.state.ui.focus_group_reveal,
        "the first focused pane learned on attach arms a one-shot group reveal"
    );

    rig.state.ui.focus_group_reveal = false;
    let snapshot = snapshot_with_focused_pane(&rig.ws, target);
    rig.fold(snapshot, PaneFrame::Fresh, SnapshotSource::Produced);

    assert!(
        !rig.state.ui.focus_group_reveal,
        "unchanged focused pane refolds leave the consumed reveal off"
    );
}

#[test]
fn input_browse_arms_order_hold_before_next_fold() {
    let mut rig = Rig::new();
    let panes = vec![
        pane("terminal_1", "tab_0", false),
        pane("terminal_2", "tab_0", false),
    ];
    let first = panes[0].pane_id.clone();
    let second = panes[1].pane_id.clone();
    rig.state.current = snapshot_with_panes(&rig.ws, panes);
    rig.state.ui.selected_pane = Some(first);
    rig.state.ui.selected_index = 0;
    rig.state.ui.last_order =
        super::super::order_hold::capture_order(&rig.state.current, &rig.state.ui);

    rig.input(KeyAction::Down);

    assert_eq!(rig.state.ui.selected_pane, Some(second));
    assert!(
        rig.state.ui.order_hold.is_some(),
        "arrow-key browse arms the order hold immediately, without waiting for a fold"
    );
}

#[test]
fn answering_focused_agent_holds_the_pre_answer_order() {
    let mut rig = Rig::new();
    let mut before = agent_snapshot(&rig.ws);
    let selected = before.worktree_groups[0].rows[0]
        .pane
        .as_ref()
        .expect("agent pane")
        .pane_id
        .clone();
    let mut other = before.worktree_groups[0].rows[0].clone();
    other.id = "agent-2".to_owned();
    other.pane = Some(pane("terminal_1", "tab_0", false));
    other.attention_score = 200;
    before.worktree_groups[0].rows[0].attention_score = 600;
    set_agent_status(&mut before, crate::agents::AgentStatus::Waiting);
    other.as_agent_mut().expect("agent row").status = crate::agents::AgentStatus::Running;
    before.worktree_groups[0].rows.push(other);
    before.focused_pane = Some(selected.clone());
    // Keep this fold independent of the existing focus-read hold trigger.
    before.viewed_panes.clear();
    before.sort_groups_for_presentation();
    rig.state.current = before.clone();
    rig.state.ui.selected_pane = Some(selected.clone());
    rig.state.ui.baseline_pane = Some(selected);
    rig.state.ui.last_order =
        super::super::order_hold::capture_order(&rig.state.current, &rig.state.ui);

    let mut after = before;
    after.panes_produced_at_ms = Some(2);
    after.worktree_groups[0].rows[0].attention_score = 200;
    set_agent_status(&mut after, crate::agents::AgentStatus::Running);
    let mut ranked = after.clone();
    ranked.sort_groups_for_presentation();
    assert_eq!(
        ranked.worktree_groups[0].rows[0].id, "agent-2",
        "the live rank moves the answered row down"
    );

    rig.fold(after, PaneFrame::Fresh, SnapshotSource::Produced);

    assert!(rig.state.ui.order_hold.is_some());
    assert_eq!(
        rig.state.current.worktree_groups[0]
            .rows
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        vec!["agent-1", "agent-2"]
    );
}

#[test]
fn mark_all_read_clears_every_unread_row_and_writes_receipts() {
    let mut rig = Rig::new();
    let mut snapshot = agent_snapshot(&rig.ws);
    let mut second = snapshot.worktree_groups[0].rows[0].clone();
    snapshot.worktree_groups[0].rows[0].id = "agent-1".to_owned();
    snapshot.worktree_groups[0].rows[0].unread = true;
    second.id = "agent-2".to_owned();
    second.unread = true;
    snapshot.worktree_groups[0].rows.push(second);
    rig.state.current = snapshot;
    rig.state.ui.unread_guard = Some("agent-1".to_owned());
    rig.state.ui.last_order =
        super::super::order_hold::capture_order(&rig.state.current, &rig.state.ui);

    rig.input(KeyAction::MarkAllRead);

    assert!(
        rig.state
            .current
            .worktree_groups
            .iter()
            .flat_map(|group| group.rows.iter())
            .all(|row| !row.unread),
        "all unread bits clear locally"
    );
    assert_eq!(rig.state.ui.unread_guard, None);
    let marks = rig.state.read_marks.load_merged();
    assert!(marks.cleared_at_ms("agent-1").is_some());
    assert!(marks.cleared_at_ms("agent-2").is_some());
    assert!(rig.state.dirty);
    assert!(
        rig.next_request().is_some(),
        "mark-all schedules a convergence refetch"
    );
}
