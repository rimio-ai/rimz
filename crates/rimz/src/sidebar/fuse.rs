//! Pure fusion of pulled sidebar truth with realtime events.
//!
//! Pulled truth remains authoritative. Events newer than the pane frame overlay
//! latency-sensitive topology, command, and focus changes until the next pull
//! supersedes them.

use std::collections::HashSet;

use crate::SidebarSnapshot;
use crate::sidebar::events::EventStore;
use crate::sidebar::events::SidebarEvent;

pub fn fuse(pulled: &SidebarSnapshot, events: &EventStore, now_ms: u64) -> SidebarSnapshot {
    let baseline = pulled
        .panes_observed_at_ms
        .or(pulled.panes_produced_at_ms)
        .unwrap_or(0);
    let carried_panes = pulled
        .truth_degraded
        .as_ref()
        .map(|notice| notice.pane_ids.iter().cloned().collect::<HashSet<_>>())
        .unwrap_or_default();
    let active = events
        .active(now_ms)
        .filter(|event| event.sent_at_ms > baseline || closes_carried_pane(event, &carried_panes))
        .collect::<Vec<_>>();
    if active.is_empty() {
        return pulled.clone();
    }

    let mut fused = pulled.clone();
    let mut deleted = HashSet::new();
    for event in &active {
        if let SidebarEvent::PaneClosed { pane_id } = &event.event {
            deleted.insert(pane_id.clone());
            fused.remove_pane_rows(pane_id);
            remove_carried_notice(&mut fused, pane_id);
        }
    }

    for event in &active {
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

    fused
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
            is_focused: false,
            is_floating: false,
            command: Some(command.to_owned()),
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

    fn row_ids(snapshot: &SidebarSnapshot) -> Vec<String> {
        snapshot
            .worktree_groups
            .iter()
            .flat_map(|group| group.rows.iter().map(|row| row.id.clone()))
            .collect()
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

        let fused = fuse(&snapshot, &store, 11);
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
            row_ids(&fuse(&snapshot, &store, 20)),
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

        let fused = fuse(&snapshot, &store, 21);
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

        let fused = fuse(&snapshot, &store, 20);
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

        let fused = fuse(&snapshot, &store, 11);
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

        let fused = fuse(&snapshot, &store, 11);
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

        let fused = fuse(&snapshot, &store, 12);
        assert_eq!(fused.focused_pane, Some(active.clone()));
        assert!(fused.viewed_panes.contains(&active));
    }

    #[test]
    fn multi_pane_focus_event_mirrors_rows_without_setting_register() {
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

        let fused = fuse(&snapshot, &store, 11);
        assert_eq!(fused.focused_pane, Some(first));
        let focused_rows = fused
            .worktree_groups
            .iter()
            .flat_map(|group| &group.rows)
            .filter_map(|row| row.pane.as_ref())
            .filter(|pane| pane.is_focused)
            .map(|pane| pane.pane_id.clone())
            .collect::<Vec<_>>();
        assert!(focused_rows.contains(&second));
        assert!(focused_rows.contains(&foreign));
    }
}
