use super::*;
use crate::sidebar::produce::test_support::pane;
use crate::sidebar::timing::SNAPSHOT_CACHE_TTL;
use crate::store::atomic;

mod cache;
mod fields;

fn frame(panes: Vec<crate::pane::PaneRef>) -> crate::sidebar::frame::PaneFrame {
    crate::sidebar::frame::assemble_frame(panes, 1, "s")
}

#[test]
fn resume_stamping_dispatches_for_kiro_panes() {
    let mut pane = pane("terminal_1", Some("kiro-cli chat --v3"), Some("/repo/main"));
    pane.pane_pid = Some(42);
    let mut frame = frame(vec![pane]);
    stamp_pane_resumed_session_ids(&mut frame, &|pid| {
        (pid == 42)
            .then(|| crate::ids::AgentSessionId::from("sess_11111111-1111-4111-8111-111111111111"))
    });
    assert_eq!(
        first(&frame).current.resumed_session_id.as_deref(),
        Some("sess_11111111-1111-4111-8111-111111111111")
    );
}

fn first(frame: &crate::sidebar::frame::PaneFrame) -> &crate::sidebar::frame::PaneState {
    &frame.tabs[0].panes[0]
}

fn first_mut(
    frame: &mut crate::sidebar::frame::PaneFrame,
) -> &mut crate::sidebar::frame::PaneState {
    &mut frame.tabs[0].panes[0]
}

fn live_row_ids(frame: &crate::sidebar::frame::PaneFrame) -> Vec<String> {
    let workspace = crate::ids::WorkspaceId::from_project_root(std::path::Path::new("/repo"));
    let snapshot = crate::SidebarSnapshot::build(workspace, Vec::new(), jiff::Timestamp::now())
        .with_live_panes(frame.to_pane_refs(), None);
    let mut ids = snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter().map(|row| row.id.clone()))
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

fn write_snapshot_cache(path: &Path, session: &str, produced_at_ms: u64, carried: bool) {
    let mut cache = crate::sidebar::frame::assemble_frame(Vec::new(), produced_at_ms, session);
    if carried {
        cache.carried_panes = vec![crate::sidebar::frame::CarriedPane {
            pane_id: crate::ids::PaneId::from_parts(crate::ids::MuxName::Zellij, "terminal_9"),
            pid: Some(909),
            start_ticks: Some(90),
            carried_since_ms: produced_at_ms,
        }];
    }
    atomic::write_temp_then_rename(path, &cache).expect("write snapshot cache");
}

fn presence_sample(
    human_clients: usize,
    last_input_ms: Option<u64>,
    sampled_at_ms: u64,
) -> PresenceSample {
    PresenceSample {
        human_clients,
        last_input_ms,
        sampled_at_ms,
    }
}

fn frame_with_presence(presence: Option<PresenceSample>) -> crate::sidebar::frame::PaneFrame {
    let mut frame = frame(Vec::new());
    frame.presence = presence;
    frame
}

fn focused_pane_in_tab(id: &str, tab: &str, tab_name: &str) -> crate::pane::PaneRef {
    crate::pane::PaneRef {
        is_focused: true,
        view_id: Some(tab.to_owned()),
        view_name: Some(tab_name.to_owned()),
        ..pane(id, Some("zsh"), Some("/repo/main"))
    }
}

#[test]
fn multi_focus_topology_detects_multiple_focused_tiled_panes_per_tab() {
    let mut floating = focused_pane_in_tab("terminal_3", "tab_7", "work");
    floating.is_floating = true;
    let anomalies = multi_focus_topology_anomalies(&[
        focused_pane_in_tab("terminal_1", "tab_7", "work"),
        focused_pane_in_tab("terminal_2", "tab_7", "work"),
        floating,
        focused_pane_in_tab("terminal_4", "tab_8", "other"),
    ]);

    assert_eq!(
        anomalies,
        vec![AnomalyKind::MultiFocusTopology {
            tab_name: Some("work".to_owned()),
            tab_position: Some(7),
            pane_ids: vec![
                "zellij:terminal_1".to_owned(),
                "zellij:terminal_2".to_owned(),
            ],
        }],
    );
}

#[test]
fn presence_sample_due_requires_idle_capable_attached_stale_sample() {
    let stale = unix_now_ms()
        .saturating_sub(crate::sidebar::timing::PRESENCE_SAMPLE_TTL.as_millis() as u64 + 1);
    let fresh = unix_now_ms()
        .saturating_add(crate::sidebar::timing::PRESENCE_SAMPLE_TTL.as_millis() as u64);

    assert!(presence_sample_due(&frame_with_presence(Some(
        presence_sample(1, Some(stale - 1), stale),
    ))));
    assert!(!presence_sample_due(&frame_with_presence(None)));
    assert!(!presence_sample_due(&frame_with_presence(Some(
        presence_sample(0, Some(stale - 1), stale),
    ))));
    assert!(!presence_sample_due(&frame_with_presence(Some(
        presence_sample(1, None, stale),
    ))));
    assert!(!presence_sample_due(&frame_with_presence(Some(
        presence_sample(1, Some(fresh - 1), fresh),
    ))));
}

#[test]
fn presence_meaningfully_changed_ignores_sample_timestamp() {
    let prior = presence_sample(1, Some(1_000), 1_000);
    let restamped = presence_sample(1, Some(1_000), 2_000);

    assert!(presence_meaningfully_changed(None, &restamped));
    assert!(!presence_meaningfully_changed(Some(&prior), &restamped));
    assert!(presence_meaningfully_changed(
        Some(&prior),
        &presence_sample(1, Some(1_500), 2_000),
    ));
    assert!(presence_meaningfully_changed(
        Some(&prior),
        &presence_sample(2, Some(1_000), 2_000),
    ));
}
