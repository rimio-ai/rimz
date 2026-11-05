use super::*;
use crate::ids::{MuxName, PaneId, WorkspaceId};
use crate::pane::PaneRef;

fn ws() -> WorkspaceId {
    WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-fuse"))
}

fn pane(raw: &str, command: &str) -> PaneRef {
    PaneRef {
        pane_id: PaneId::from_parts(MuxName::Zellij, raw),
        session_name: "rimz-test".to_owned(),
        view_id: Some("tab_1".to_owned()),
        view_kind: Some(crate::ids::ViewKind::Tab),
        view_name: None,
        title: None,
        is_floating: false,
        command: Some(command.to_owned()),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: Some("/repo/main".to_owned()),
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        hosted_agent_lineage: Vec::new(),
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }
}

fn pane_in_view(raw: &str, command: &str, view: &str) -> PaneRef {
    let mut pane = pane(raw, command);
    pane.view_id = Some(view.to_owned());
    pane
}

/// An own view whose working set is `working`, mirroring what
/// `SidebarOwnView::from_frame` derives from the live pane frame.
fn own_view(working: &[&PaneId]) -> crate::store::snapshot::SidebarOwnView {
    crate::store::snapshot::SidebarOwnView {
        sibling_count: working.len(),
        working_pane_ids: working.iter().map(|&pane_id| pane_id.clone()).collect(),
        own_view_is_daemon: false,
    }
}

fn pulled(panes: Vec<PaneRef>, produced_at_ms: u64) -> SidebarSnapshot {
    let mut snapshot = SidebarSnapshot::build_with_agents(ws(), Vec::new(), jiff::Timestamp::now())
        .with_project_root(Some(std::path::PathBuf::from("/repo/main")))
        .with_live_panes(panes, None);
    snapshot.panes_produced_at_ms = Some(produced_at_ms);
    snapshot
}

fn append(store: &mut EventStore, sent_at_ms: u64, event: SidebarEvent) {
    store.append(event, sent_at_ms, sent_at_ms);
}

fn anchor(pane_id: PaneId, stamp_ms: u64) -> FocusAnchor {
    FocusAnchor {
        nonce: crate::ids::FocusNonce::new(),
        session_name: "rimz-test".to_owned(),
        pane_id,
        origin: crate::mux::focus_anchor::FocusOrigin::User,
        repair_generation: None,
        issued_at_ms: stamp_ms,
        applied_at_ms: Some(stamp_ms),
        state: crate::mux::focus_anchor::FocusIntentState::Applied,
        pre_action: Vec::new(),
        placement: None,
        order: None,
    }
}

fn intent(pane_id: PaneId, stamp_ms: u64) -> FocusPresentation {
    FocusPresentation::Target(Box::new(anchor(pane_id, stamp_ms)))
}

fn row_ids(snapshot: &SidebarSnapshot) -> Vec<String> {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter().map(|row| row.id.clone()))
        .collect()
}

#[test]
fn owned_pull_retains_a_baseline_only_while_fusion_is_active() {
    let snapshot = pulled(vec![pane("terminal_1", "zsh")], 10);
    let empty = EventStore::default();
    let (moved, baseline, focus_event_sent_at_ms) = fuse_owned(snapshot, empty, None, 11);
    assert!(baseline.is_none());
    assert_eq!(focus_event_sent_at_ms, None);
    assert_eq!(row_ids(&moved).len(), 1);

    let mut events = EventStore::default();
    append(
        &mut events,
        12,
        SidebarEvent::CommandChanged {
            pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_1"),
            command: "claude".to_owned(),
        },
    );
    let (fused, baseline, focus_event_sent_at_ms) = fuse_owned(moved, events, None, 12);
    assert_eq!(
        fused.worktree_groups[0].rows[0]
            .pane
            .as_ref()
            .and_then(|pane| pane.command.as_deref()),
        Some("claude")
    );
    assert!(baseline.is_some());
    assert_eq!(focus_event_sent_at_ms, None);
}

#[test]
fn pane_closed_newer_than_pull_removes_row_and_clears_focus() {
    let focused = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let mut snapshot = pulled(vec![pane("terminal_1", "zsh")], 10);
    snapshot.focused_pane = Some(focused.clone());
    let mut store = EventStore::default();
    append(
        &mut store,
        11,
        SidebarEvent::PaneClosed { pane_id: focused },
    );

    let (fused, _, _) = fuse_owned(snapshot, store, None, 11);
    assert!(row_ids(&fused).is_empty());
    assert_eq!(fused.focused_pane, None);
}

#[test]
fn pull_newer_than_event_supersedes_it() {
    let snapshot = pulled(vec![pane("terminal_1", "zsh")], 20);
    let mut store = EventStore::default();
    append(
        &mut store,
        19,
        SidebarEvent::PaneClosed {
            pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_1"),
        },
    );

    assert_eq!(
        row_ids(&fuse_owned(snapshot, store, None, 20).0),
        vec!["zellij:terminal_1"]
    );
}

#[test]
fn observed_at_not_publish_time_supersedes_focus_events() {
    let first = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let active = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let mut snapshot = pulled(
        vec![pane("terminal_1", "zsh"), pane("terminal_2", "zsh")],
        20,
    );
    snapshot.panes_observed_at_ms = Some(10);
    snapshot.focused_pane = Some(first.clone());
    let mut store = EventStore::default();
    append(
        &mut store,
        15,
        SidebarEvent::FocusChanged {
            focused: vec![active.clone()],
            unfocused: vec![first],
        },
    );

    let (fused, _, _) = fuse_owned(snapshot, store, None, 21);
    assert_eq!(fused.focused_pane, Some(active));
}

#[test]
fn carried_pane_close_older_than_pull_deletes_the_card() {
    let carried = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let mut snapshot = pulled(vec![pane("terminal_1", "zsh")], 20);
    snapshot.truth_degraded = Some(crate::store::snapshot::TruthNotice {
        carried: 1,
        since_ms: 10,
        pane_ids: vec![carried.clone()],
    });
    let mut store = EventStore::default();
    append(
        &mut store,
        19,
        SidebarEvent::PaneClosed {
            pane_id: carried.clone(),
        },
    );

    let (fused, _, _) = fuse_owned(snapshot, store, None, 20);
    assert!(row_ids(&fused).is_empty());
    assert_eq!(fused.truth_degraded, None);
}

#[test]
fn command_events_keep_pane_slots_and_preserve_process_sinks() {
    for agent_before in [false, true] {
        let mut pane = pane("terminal_1", "zsh");
        pane.first_seen_at_ms = Some(1_000);
        let mut snapshot = pulled(vec![pane], 10);
        let row = &mut std::sync::Arc::make_mut(
            &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
        )[0];
        row.attention_at_ms = Some(9_000);
        row.inactive = true;
        row.archived = true;
        if agent_before {
            row.card = crate::store::snapshot::RowCard::Agent(std::sync::Arc::new(
                crate::store::snapshot::AgentCard::default(),
            ));
        }
        let mut events = EventStore::default();
        append(
            &mut events,
            11,
            SidebarEvent::CommandChanged {
                pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_1"),
                command: "cargo build".to_owned(),
            },
        );
        let (fused, _, _) = fuse_owned(snapshot, events, None, 11);
        let row = fused.rows().next().unwrap();
        assert_eq!(row.id, "zellij:terminal_1");
        assert_eq!(row.name, "cargo");
        assert!(row.process_is_busy());
        assert_eq!(
            (row.attention_at_ms, row.inactive, row.archived),
            (Some(9_000), true, true),
            "agent exit={agent_before}"
        );
    }

    let actual = [false, true].map(|cooled| {
        let pane_id = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let mut snapshot = pulled(
            vec![pane("terminal_1", "zsh"), pane("terminal_2", "zsh")],
            10,
        );
        snapshot.wired_kinds = vec!["claude".to_owned()];
        for row in snapshot.rows_mut() {
            row.attention_at_ms = Some(if row.id == pane_id.as_str() {
                2_000
            } else {
                1_000
            });
            row.inactive = cooled;
            row.archived = cooled;
        }
        snapshot.sort_groups_for_presentation();
        let position = |snapshot: &SidebarSnapshot| {
            snapshot.rows().position(|row| {
                row.pane
                    .as_ref()
                    .is_some_and(|pane| pane.pane_id == pane_id)
            })
        };
        let before = position(&snapshot);
        let mut events = EventStore::default();
        append(
            &mut events,
            11,
            SidebarEvent::CommandChanged {
                pane_id: pane_id.clone(),
                command: "claude".to_owned(),
            },
        );
        let (mut fused, _, _) = fuse_owned(snapshot, events, None, 11);
        fused.sort_groups_for_presentation();
        let row = fused.row_owning_pane(&pane_id).unwrap();
        (
            before,
            position(&fused),
            row.is_agent(),
            row.attention_at_ms,
            row.inactive,
            row.archived,
        )
    });
    assert_eq!(
        actual,
        [(Some(0), Some(0), true, Some(2_000), false, false); 2]
    );
}

#[test]
fn pane_opened_event_without_frame_does_not_create_a_card() {
    let snapshot = pulled(Vec::new(), 10);
    let mut store = EventStore::default();
    append(
        &mut store,
        11,
        SidebarEvent::PaneOpened {
            pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_7"),
            command: Some("zsh".to_owned()),
        },
    );

    let (fused, _, _) = fuse_owned(snapshot, store, None, 11);
    assert!(row_ids(&fused).is_empty());
}

#[test]
fn latest_single_focus_event_sets_register_and_viewed_pane() {
    let first = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let active = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let mut snapshot = pulled(
        vec![pane("terminal_1", "zsh"), pane("terminal_2", "zsh")],
        10,
    );
    snapshot.own_view = Some(own_view(&[&first, &active]));
    let mut store = EventStore::default();
    append(
        &mut store,
        11,
        SidebarEvent::FocusChanged {
            focused: vec![first.clone()],
            unfocused: Vec::new(),
        },
    );
    append(
        &mut store,
        12,
        SidebarEvent::FocusChanged {
            focused: vec![active.clone()],
            unfocused: vec![first],
        },
    );

    let (fused, _, focus_event_sent_at_ms) = fuse_owned(snapshot, store, None, 12);
    assert_eq!(fused.focused_pane, Some(active.clone()));
    assert!(fused.viewed_panes.contains(&active));
    assert_eq!(focus_event_sent_at_ms, Some(12));
}

#[test]
fn ambiguous_focus_event_does_not_change_session_register() {
    let first = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let second = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let foreign = PaneId::from_parts(MuxName::Zellij, "terminal_9");
    let mut snapshot = pulled(
        vec![
            pane("terminal_1", "zsh"),
            pane("terminal_2", "zsh"),
            pane_in_view("terminal_9", "zsh", "tab_2"),
        ],
        10,
    );
    snapshot.focused_pane = Some(first.clone());
    let mut store = EventStore::default();
    append(
        &mut store,
        11,
        SidebarEvent::FocusChanged {
            focused: vec![second.clone(), foreign.clone()],
            unfocused: Vec::new(),
        },
    );

    let (fused, _, _) = fuse_owned(snapshot, store, None, 11);
    assert_eq!(fused.focused_pane, Some(first));
    assert!(!fused.viewed_panes.contains(&second));
    assert!(!fused.viewed_panes.contains(&foreign));
}

#[test]
fn pending_intent_overrides_newer_pulled_focus() {
    let target = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let observed = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let mut snapshot = pulled(
        vec![pane("terminal_1", "zsh"), pane("terminal_2", "zsh")],
        12,
    );
    snapshot.panes_observed_at_ms = Some(12);
    snapshot.focused_pane = Some(observed.clone());
    snapshot.viewed_panes = vec![observed.clone()];

    let (fused, _, _) = fuse_owned(
        snapshot,
        EventStore::default(),
        Some(&intent(target.clone(), 11)),
        12,
    );

    assert_eq!(fused.focused_pane, Some(target.clone()));
    assert_eq!(fused.viewed_panes, vec![observed, target]);
}

#[test]
fn pending_intent_overrides_newer_focus_event() {
    let target = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let observed = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let snapshot = pulled(
        vec![pane("terminal_1", "zsh"), pane("terminal_2", "zsh")],
        10,
    );
    let mut store = EventStore::default();
    append(
        &mut store,
        12,
        SidebarEvent::FocusChanged {
            focused: vec![observed],
            unfocused: Vec::new(),
        },
    );

    let (fused, _, _) = fuse_owned(snapshot, store, Some(&intent(target.clone(), 11)), 12);

    assert_eq!(fused.focused_pane, Some(target));
}

#[test]
fn pending_intent_without_a_row_does_not_override() {
    let observed = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let missing = PaneId::from_parts(MuxName::Zellij, "terminal_9");
    let mut snapshot = pulled(vec![pane("terminal_1", "zsh")], 10);
    snapshot.focused_pane = Some(observed.clone());

    let (fused, _, _) = fuse_owned(
        snapshot,
        EventStore::default(),
        Some(&intent(missing, 11)),
        11,
    );

    assert_eq!(fused.focused_pane, Some(observed));
}

#[test]
fn pending_child_intent_overrides_parent_focus() {
    let target = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let mut snapshot = pulled(vec![pane("terminal_1", "zsh")], 10);
    snapshot.focused_pane = Some(PaneId::from_parts(MuxName::Zellij, "terminal_1"));
    let row = &mut std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0];
    row.card = crate::store::snapshot::RowCard::Agent(std::sync::Arc::default());
    row.as_agent_mut().unwrap().sub_agents.push(
        serde_json::from_value(serde_json::json!({
            "id": "child", "kind": "claude", "name": "explorer", "status": "running",
            "last_activity": snapshot.now, "pane": target,
        }))
        .unwrap(),
    );
    let (fused, _, _) = fuse_owned(
        snapshot,
        EventStore::default(),
        Some(&intent(target.clone(), 11)),
        11,
    );
    assert_eq!(fused.focused_pane, Some(target));
}

#[test]
fn observed_pulled_focus_confirms_intent() {
    let target = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let mut snapshot = pulled(vec![pane("terminal_1", "zsh")], 12);
    snapshot.panes_observed_at_ms = Some(12);
    snapshot.focused_pane = Some(target.clone());
    snapshot.client_views = vec![crate::pane::ClientPaneView {
        client_id: crate::ids::MuxClientId::Zellij(1),
        pane_id: target.clone(),
    }];

    assert!(focus_intent_confirmed(
        &snapshot,
        &EventStore::default(),
        &anchor(target, 11),
        12,
    ));
}

#[test]
fn held_session_register_does_not_confirm_intent_without_a_client_observation() {
    let target = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let mut snapshot = pulled(vec![pane("terminal_1", "zsh")], 12);
    snapshot.panes_observed_at_ms = Some(12);
    snapshot.focused_pane = Some(target.clone());

    assert!(!focus_intent_confirmed(
        &snapshot,
        &EventStore::default(),
        &anchor(target, 11),
        12,
    ));
}

#[test]
fn observed_single_focus_event_confirms_intent() {
    let target = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let snapshot = pulled(vec![pane("terminal_1", "zsh")], 10);
    let mut store = EventStore::default();
    append(
        &mut store,
        12,
        SidebarEvent::FocusChanged {
            focused: vec![target.clone()],
            unfocused: Vec::new(),
        },
    );

    assert!(focus_intent_confirmed(
        &snapshot,
        &store,
        &anchor(target, 11),
        12,
    ));
}

#[test]
fn different_or_older_focus_evidence_does_not_confirm_intent() {
    let target = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let other = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let mut snapshot = pulled(
        vec![pane("terminal_1", "zsh"), pane("terminal_2", "zsh")],
        10,
    );
    snapshot.panes_observed_at_ms = Some(12);
    snapshot.focused_pane = Some(other);
    let mut store = EventStore::default();
    append(
        &mut store,
        10,
        SidebarEvent::FocusChanged {
            focused: vec![target.clone()],
            unfocused: Vec::new(),
        },
    );

    assert!(!focus_intent_confirmed(
        &snapshot,
        &store,
        &anchor(target, 11),
        12,
    ));
}

#[test]
fn multi_pane_focus_dump_does_not_confirm_intent() {
    let target = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let other = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let snapshot = pulled(
        vec![pane("terminal_1", "zsh"), pane("terminal_2", "zsh")],
        10,
    );
    let mut store = EventStore::default();
    append(
        &mut store,
        12,
        SidebarEvent::FocusChanged {
            focused: vec![target.clone(), other],
            unfocused: Vec::new(),
        },
    );

    assert!(!focus_intent_confirmed(
        &snapshot,
        &store,
        &anchor(target, 11),
        12,
    ));
}
