//! Pure fusion of pulled sidebar truth, realtime events, and pending jump intent.
//!
//! Pulled truth remains authoritative for observations. Events newer than the
//! pane frame overlay latency-sensitive topology, command, and focus changes
//! until the next pull supersedes them. A pending user focus intent outranks
//! both observation channels until the mux confirms it or its anchor expires.

use std::collections::HashSet;

use crate::SidebarSnapshot;
use crate::sidebar::events::EventStore;
use crate::sidebar::events::SidebarEvent;
use crate::sidebar::focus_anchor::{FocusAnchor, FocusObservation, FocusPresentation};

pub fn fuse(
    pulled: &SidebarSnapshot,
    events: &EventStore,
    intent: Option<&FocusPresentation>,
    now_ms: u64,
) -> SidebarSnapshot {
    let active = active_events(pulled, events, now_ms);
    if active.is_empty() && intent.is_none() {
        return pulled.clone();
    }
    apply_fusion(pulled.clone(), &active, intent)
}

/// Move an unmodified pull straight into presentation when no realtime state
/// overlays it. The returned baseline exists only while an overlay or focus
/// fence can outlive this pull; the renderer uses it for later synthetic folds.
pub fn fuse_owned(
    pulled: SidebarSnapshot,
    events: &EventStore,
    intent: Option<&FocusPresentation>,
    now_ms: u64,
) -> (SidebarSnapshot, Option<SidebarSnapshot>) {
    let active = active_events(&pulled, events, now_ms);
    if active.is_empty() && intent.is_none() {
        return (pulled, None);
    }
    let fused = apply_fusion(pulled.clone(), &active, intent);
    (fused, Some(pulled))
}

fn active_events<'a>(
    pulled: &SidebarSnapshot,
    events: &'a EventStore,
    now_ms: u64,
) -> Vec<&'a crate::sidebar::events::StoredEvent> {
    let baseline = pulled
        .panes_observed_at_ms
        .or(pulled.panes_produced_at_ms)
        .unwrap_or(0);
    let carried_panes = pulled
        .truth_degraded
        .as_ref()
        .map(|notice| notice.pane_ids.iter().cloned().collect::<HashSet<_>>())
        .unwrap_or_default();
    events
        .active(now_ms)
        .filter(|event| event.sent_at_ms > baseline || closes_carried_pane(event, &carried_panes))
        .collect()
}

fn apply_fusion(
    mut fused: SidebarSnapshot,
    active: &[&crate::sidebar::events::StoredEvent],
    intent: Option<&FocusPresentation>,
) -> SidebarSnapshot {
    let mut deleted = HashSet::new();
    for event in active {
        if let SidebarEvent::PaneClosed { pane_id } = &event.event {
            deleted.insert(pane_id.clone());
            fused.remove_pane_rows(pane_id);
            remove_carried_notice(&mut fused, pane_id);
        }
    }

    for event in active {
        if let SidebarEvent::CommandChanged { pane_id, command } = &event.event
            && !deleted.contains(pane_id)
        {
            fused.overlay_pane_command(pane_id, command);
        }
    }

    if let Some(focus) = active
        .iter()
        .filter_map(|event| match &event.event {
            SidebarEvent::FocusChanged { focused, unfocused } => {
                Some((event.sent_at_ms, focused, unfocused))
            }
            _ => None,
        })
        .max_by_key(|(sent_at_ms, _, _)| *sent_at_ms)
    {
        fused.overlay_focus(focus.1, focus.2);
    }

    match intent {
        Some(FocusPresentation::Target(intent)) if snapshot_has_pane(&fused, &intent.pane_id) => {
            fused.overlay_focus(std::slice::from_ref(&intent.pane_id), &[]);
        }
        Some(FocusPresentation::Fence) => fused.focused_pane = None,
        _ => {}
    }

    fused
}

pub fn focus_intent_confirmed(
    pulled: &SidebarSnapshot,
    events: &EventStore,
    intent: &FocusAnchor,
    now_ms: u64,
) -> bool {
    focus_intent_confirmed_from(
        &FocusObservation::from_snapshot(pulled),
        events,
        intent,
        now_ms,
    )
}

pub(crate) fn focus_intent_confirmed_from(
    pulled: &FocusObservation,
    events: &EventStore,
    intent: &FocusAnchor,
    now_ms: u64,
) -> bool {
    if intent.state != crate::sidebar::focus_anchor::FocusIntentState::Applied {
        return false;
    }
    let pulled_confirms = pulled
        .panes_observed_at_ms
        .is_some_and(|observed_at_ms| observed_at_ms >= intent.issued_at_ms)
        && !pulled.client_views.is_empty()
        && pulled
            .client_views
            .iter()
            .all(|view| view.pane_id == intent.pane_id);
    let event_confirms = events.active(now_ms).any(|event| {
        event.sent_at_ms >= intent.issued_at_ms
            && matches!(
                &event.event,
                SidebarEvent::FocusChanged { focused, .. }
                    if matches!(focused.as_slice(), [pane] if pane == &intent.pane_id)
            )
    });

    pulled_confirms || event_confirms
}

fn snapshot_has_pane(snapshot: &SidebarSnapshot, pane_id: &crate::ids::PaneId) -> bool {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .any(|row| {
            row.pane
                .as_ref()
                .is_some_and(|pane| &pane.pane_id == pane_id)
        })
}

fn closes_carried_pane(
    event: &crate::sidebar::events::StoredEvent,
    carried_panes: &HashSet<crate::ids::PaneId>,
) -> bool {
    match &event.event {
        SidebarEvent::PaneClosed { pane_id } => carried_panes.contains(pane_id),
        _ => false,
    }
}

fn remove_carried_notice(snapshot: &mut SidebarSnapshot, pane_id: &crate::ids::PaneId) {
    let Some(notice) = &mut snapshot.truth_degraded else {
        return;
    };
    let before = notice.pane_ids.len();
    notice.pane_ids.retain(|carried| carried != pane_id);
    if before == notice.pane_ids.len() {
        return;
    }
    notice.carried = notice
        .carried
        .saturating_sub(before - notice.pane_ids.len());
    if notice.carried == 0 {
        snapshot.truth_degraded = None;
    }
}

#[cfg(test)]
mod tests {
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
    fn own_view(working: &[&PaneId]) -> crate::SidebarOwnView {
        crate::SidebarOwnView {
            sibling_count: working.len(),
            working_pane_ids: working.iter().map(|&pane_id| pane_id.clone()).collect(),
            own_view_is_daemon: false,
        }
    }

    fn pulled(panes: Vec<PaneRef>, produced_at_ms: u64) -> SidebarSnapshot {
        let mut snapshot =
            SidebarSnapshot::build_with_agents(ws(), Vec::new(), jiff::Timestamp::now())
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
            nonce: crate::sidebar::focus_anchor::FocusNonce::new(),
            session_name: "rimz-test".to_owned(),
            pane_id,
            origin: crate::sidebar::focus_anchor::FocusOrigin::User,
            repair_generation: None,
            issued_at_ms: stamp_ms,
            applied_at_ms: Some(stamp_ms),
            state: crate::sidebar::focus_anchor::FocusIntentState::Applied,
            pre_action: Vec::new(),
            offset: 0,
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
        let (moved, baseline) = fuse_owned(snapshot, &empty, None, 11);
        assert!(baseline.is_none());
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
        let (fused, baseline) = fuse_owned(moved, &events, None, 12);
        assert_eq!(
            fused.worktree_groups[0].rows[0]
                .pane
                .as_ref()
                .and_then(|pane| pane.command.as_deref()),
            Some("claude")
        );
        assert!(baseline.is_some());
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

        let fused = fuse(&snapshot, &store, None, 11);
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
            row_ids(&fuse(&snapshot, &store, None, 20)),
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

        let fused = fuse(&snapshot, &store, None, 21);
        assert_eq!(fused.focused_pane, Some(active));
    }

    #[test]
    fn carried_pane_close_older_than_pull_deletes_the_card() {
        let carried = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let mut snapshot = pulled(vec![pane("terminal_1", "zsh")], 20);
        snapshot.truth_degraded = Some(crate::TruthNotice {
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

        let fused = fuse(&snapshot, &store, None, 20);
        assert!(row_ids(&fused).is_empty());
        assert_eq!(fused.truth_degraded, None);
    }

    #[test]
    fn command_changed_overlays_process_row_identity() {
        let snapshot = pulled(vec![pane("terminal_1", "zsh")], 10);
        let mut store = EventStore::default();
        append(
            &mut store,
            11,
            SidebarEvent::CommandChanged {
                pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_1"),
                command: "cargo build".to_owned(),
            },
        );

        let fused = fuse(&snapshot, &store, None, 11);
        let row = &fused.worktree_groups[0].rows[0];
        assert_eq!(row.id, "zellij:terminal_1");
        assert_eq!(row.name, "cargo");
        assert!(row.process_is_busy());
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

        let fused = fuse(&snapshot, &store, None, 11);
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

        let fused = fuse(&snapshot, &store, None, 12);
        assert_eq!(fused.focused_pane, Some(active.clone()));
        assert!(fused.viewed_panes.contains(&active));
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

        let fused = fuse(&snapshot, &store, None, 11);
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
        snapshot.focused_pane = Some(observed);

        let fused = fuse(
            &snapshot,
            &EventStore::default(),
            Some(&intent(target.clone(), 11)),
            12,
        );

        assert_eq!(fused.focused_pane, Some(target));
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

        let fused = fuse(&snapshot, &store, Some(&intent(target.clone(), 11)), 12);

        assert_eq!(fused.focused_pane, Some(target));
    }

    #[test]
    fn pending_intent_without_a_row_does_not_override() {
        let observed = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let missing = PaneId::from_parts(MuxName::Zellij, "terminal_9");
        let mut snapshot = pulled(vec![pane("terminal_1", "zsh")], 10);
        snapshot.focused_pane = Some(observed.clone());

        let fused = fuse(
            &snapshot,
            &EventStore::default(),
            Some(&intent(missing, 11)),
            11,
        );

        assert_eq!(fused.focused_pane, Some(observed));
    }

    #[test]
    fn observed_pulled_focus_confirms_intent() {
        let target = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let mut snapshot = pulled(vec![pane("terminal_1", "zsh")], 12);
        snapshot.panes_observed_at_ms = Some(12);
        snapshot.focused_pane = Some(target.clone());
        snapshot.client_views = vec![crate::mux::ClientPaneView {
            client_id: crate::mux::MuxClientId::Zellij(1),
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
}
