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
    let contested_panes = pulled
        .focus_contested_panes
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    let active = events
        .active(now_ms)
        .filter(|event| {
            event.sent_at_ms > baseline
                || closes_carried_pane(event, &carried_panes)
                || focus_touches_contested_pane(event, &contested_panes)
        })
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

fn focus_touches_contested_pane(
    event: &crate::sidebar::events::StoredEvent,
    contested_panes: &HashSet<crate::ids::PaneId>,
) -> bool {
    match &event.event {
        SidebarEvent::FocusChanged { focused, unfocused } => focused
            .iter()
            .chain(unfocused)
            .any(|pane_id| contested_panes.contains(pane_id)),
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
    use std::time::{Duration, Instant};

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
    fn own_view(working: &[&PaneId], active: Option<PaneId>) -> crate::SidebarOwnView {
        crate::SidebarOwnView {
            sibling_count: working.len(),
            own_is_active: false,
            active_pane_id: active,
            active_pane_is_viewed: false,
            working_pane_ids: working.iter().map(|&pane_id| pane_id.clone()).collect(),
            focus_contested: false,
            own_view_is_daemon: false,
        }
    }

    fn contested_own_view(working: &[&PaneId], active: Option<PaneId>) -> crate::SidebarOwnView {
        crate::SidebarOwnView {
            focus_contested: true,
            ..own_view(working, active)
        }
    }

    fn pulled(panes: Vec<PaneRef>, produced_at_ms: u64) -> SidebarSnapshot {
        let mut snapshot = SidebarSnapshot::build_with_agents(
            ws(),
            Vec::new(),
            Vec::new(),
            jiff::Timestamp::now(),
        )
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

    fn fuse_n(
        snapshot: &SidebarSnapshot,
        store: &EventStore,
        now_ms: u64,
        rounds: u32,
    ) -> Duration {
        let start = Instant::now();
        for _ in 0..rounds {
            let _ = fuse(snapshot, store, now_ms);
        }
        start.elapsed()
    }

    fn fleet_panes(count: usize) -> Vec<PaneRef> {
        (0..count)
            .map(|idx| {
                let mut pane = pane(&format!("terminal_{idx}"), "zsh");
                pane.cwd = Some(format!("/repo/wt{}", idx / 25));
                pane
            })
            .collect()
    }

    fn fleet_events(count: usize, baseline_ms: u64) -> EventStore {
        let mut store = EventStore::default();
        for idx in 0..count {
            let pane_id = PaneId::from_parts(MuxName::Zellij, format!("terminal_{idx}"));
            let event = if idx % 2 == 0 {
                SidebarEvent::CommandChanged {
                    pane_id,
                    command: "cargo build".to_owned(),
                }
            } else {
                SidebarEvent::PaneClosed { pane_id }
            };
            append(&mut store, baseline_ms + 1 + idx as u64, event);
        }
        store
    }

    #[test]
    fn pane_closed_newer_than_pull_deletes_the_card() {
        let snapshot = pulled(vec![pane("terminal_1", "zsh")], 10);
        let mut store = EventStore::default();
        append(
            &mut store,
            11,
            SidebarEvent::PaneClosed {
                pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_1"),
            },
        );

        assert!(row_ids(&fuse(&snapshot, &store, 11)).is_empty());
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
        snapshot.own_view = Some(own_view(&[&first, &active], Some(first.clone())));
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
        assert_eq!(
            fused.own_view.and_then(|view| view.active_pane_id),
            Some(active)
        );
    }

    #[test]
    fn contested_focus_pane_event_survives_newer_ambiguous_frame() {
        let first = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let active = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let mut snapshot = pulled(
            vec![pane("terminal_1", "zsh"), pane("terminal_2", "zsh")],
            20,
        );
        snapshot.own_view = Some(contested_own_view(&[&first, &active], Some(first.clone())));
        snapshot.focus_contested_panes = vec![first.clone(), active.clone()];
        let mut store = EventStore::default();
        append(
            &mut store,
            19,
            SidebarEvent::FocusChanged {
                focused: vec![active.clone()],
                unfocused: vec![first],
            },
        );

        let fused = fuse(&snapshot, &store, 21);
        assert_eq!(
            fused
                .own_view
                .as_ref()
                .and_then(|view| view.active_pane_id.clone()),
            Some(active.clone())
        );
        assert!(
            !fused
                .own_view
                .as_ref()
                .is_some_and(|view| view.focus_contested),
            "the focus event resolves the own-view contest for the fused frame",
        );
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
        assert!(
            row_ids(&fused).is_empty(),
            "a pane-open event is a wakeup hint; the producer frame admits the card"
        );
    }

    #[test]
    fn latest_focus_event_sets_the_active_baseline() {
        let first = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let active = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let mut snapshot = pulled(
            vec![pane("terminal_1", "zsh"), pane("terminal_2", "zsh")],
            10,
        );
        snapshot.own_view = Some(own_view(&[&first, &active], None));
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
        let view = fused.own_view.expect("own view kept");
        assert_eq!(view.active_pane_id, Some(active));
        assert!(
            view.active_pane_is_viewed,
            "a FocusChanged event that names an own working pane is a viewing signal"
        );
    }

    #[test]
    fn a_foreign_view_focus_mark_never_retargets_the_own_view_baseline() {
        let sibling = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let own_active = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let foreign = PaneId::from_parts(MuxName::Zellij, "terminal_9");
        let mut snapshot = pulled(
            vec![
                pane("terminal_1", "zsh"),
                pane("terminal_2", "zsh"),
                pane_in_view("terminal_9", "zsh", "tab_2"),
            ],
            10,
        );
        snapshot.own_view = Some(own_view(&[&sibling, &own_active], Some(own_active.clone())));
        let mut store = EventStore::default();
        append(
            &mut store,
            11,
            SidebarEvent::FocusChanged {
                focused: vec![foreign],
                unfocused: Vec::new(),
            },
        );

        let fused = fuse(&snapshot, &store, 11);
        let foreign_row = fused
            .worktree_groups
            .iter()
            .flat_map(|group| &group.rows)
            .find(|row| row.id == "zellij:terminal_9")
            .expect("foreign pane renders");
        assert!(
            foreign_row.pane.as_ref().expect("pane ref").is_focused,
            "per-view marks mirror onto every row"
        );
        let view = fused.own_view.expect("own view kept");
        assert_eq!(
            view.active_pane_id,
            Some(own_active),
            "another view's focus move is never this view's baseline"
        );
    }

    #[test]
    fn a_session_wide_focus_patch_takes_the_own_view_pane_for_the_baseline() {
        let sibling = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let own_active = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let foreign = PaneId::from_parts(MuxName::Zellij, "terminal_9");
        let mut snapshot = pulled(
            vec![
                pane("terminal_1", "zsh"),
                pane("terminal_2", "zsh"),
                pane_in_view("terminal_9", "zsh", "tab_2"),
            ],
            10,
        );
        snapshot.own_view = Some(own_view(&[&sibling, &own_active], Some(sibling.clone())));
        let mut store = EventStore::default();
        // The plugin's declarative patch lists every view's marks; the foreign
        // pane deliberately sorts last so a `focused.last()` regression fails.
        append(
            &mut store,
            11,
            SidebarEvent::FocusChanged {
                focused: vec![own_active.clone(), foreign],
                unfocused: vec![sibling],
            },
        );

        let fused = fuse(&snapshot, &store, 11);
        assert_eq!(
            fused.own_view.and_then(|view| view.active_pane_id),
            Some(own_active)
        );
    }

    #[test]
    fn level_dump_focus_prefers_transition_over_current_active() {
        let sibling = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let own_active = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let mut snapshot = pulled(
            vec![pane("terminal_1", "zsh"), pane("terminal_2", "zsh")],
            10,
        );
        snapshot.own_view = Some(own_view(&[&sibling, &own_active], Some(sibling.clone())));
        let mut store = EventStore::default();
        append(
            &mut store,
            11,
            SidebarEvent::FocusChanged {
                focused: vec![sibling, own_active.clone()],
                unfocused: Vec::new(),
            },
        );

        let fused = fuse(&snapshot, &store, 11);
        assert_eq!(
            fused.own_view.and_then(|view| view.active_pane_id),
            Some(own_active)
        );
    }

    #[test]
    fn unfocusing_the_own_baseline_clears_it_to_the_hold_last_derivation() {
        let sibling = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let own_active = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let foreign = PaneId::from_parts(MuxName::Zellij, "terminal_9");
        let mut snapshot = pulled(
            vec![
                pane("terminal_1", "zsh"),
                pane("terminal_2", "zsh"),
                pane_in_view("terminal_9", "zsh", "tab_2"),
            ],
            10,
        );
        snapshot.own_view = Some(own_view(&[&sibling, &own_active], Some(own_active.clone())));
        let mut store = EventStore::default();
        append(
            &mut store,
            11,
            SidebarEvent::FocusChanged {
                focused: vec![foreign],
                unfocused: vec![own_active],
            },
        );

        let fused = fuse(&snapshot, &store, 11);
        let view = fused.own_view.expect("own view kept");
        assert_eq!(
            view.active_pane_id, None,
            "the renderer holds its last selection on a None derivation"
        );
        assert!(!view.active_pane_is_viewed);
    }

    #[test]
    fn fuse_stays_inside_the_frame_bucket_at_fleet_scale() {
        let snapshot = pulled(fleet_panes(500), 1_000);
        let store = fleet_events(crate::sidebar::events::MAX_EVENTS, 1_000);
        let now_ms = 2_000;

        fuse_n(&snapshot, &store, now_ms, 5); // warm
        let elapsed = fuse_n(&snapshot, &store, now_ms, 20) / 20;

        assert!(
            elapsed < Duration::from_millis(50),
            "one fleet-scale fuse took {elapsed:?}; the default 100ms frame grid \
             leaves no headroom for this"
        );
    }
}
