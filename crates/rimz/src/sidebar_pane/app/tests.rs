use super::*;
use crate::sidebar_pane::app::fixtures::{pane, snapshot, snapshot_with_panes, workspace};
use jiff::Timestamp;

fn focus_fixture() -> (SidebarSnapshot, PaneId, PaneId, PaneId) {
    let ws = workspace();
    let sidebar = PaneId::from_parts(MuxName::Zellij, "terminal_10");
    let first_work = PaneId::from_parts(MuxName::Zellij, "terminal_11");
    let second_work = PaneId::from_parts(MuxName::Zellij, "terminal_12");
    let mut snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_11", "tab_1", false),
            pane("terminal_12", "tab_1", false),
        ],
    );
    snapshot.own_view = Some(crate::SidebarOwnView {
        sibling_count: 3,
        own_is_active: true,
        active_pane_id: None,
        working_pane_ids: vec![first_work.clone(), second_work.clone()],
        focus_contested: false,
        own_view_is_daemon: false,
    });
    (snapshot, sidebar, first_work, second_work)
}

#[test]
fn tick_for_clamps_zero_and_honours_explicit_seconds() {
    assert_eq!(tick_for(5), Duration::from_secs(5));
    assert_eq!(tick_for(0), Duration::from_secs(1));
}

#[test]
fn frame_grid_advances_one_frame_or_snaps_past_missed_frames() {
    let base = Instant::now();
    let frame = crate::sidebar::timing::animation_frame(crate::sidebar::timing::DEFAULT_REFRESH_MS);
    assert_eq!(next_frame_after(base, base, frame), base + frame);
    let now = base + frame * 5;
    assert_eq!(next_frame_after(base, now, frame), now + frame);
}

#[test]
fn frame_interval_uses_breath_for_pulse_and_fast_for_work() {
    let ws = workspace();
    let mut slow = snapshot(&ws);
    slow.worktree_groups = vec![crate::SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        kind: crate::SidebarWorktreeKind::Worktree,
        status_counts: Vec::new(),
        rows: vec![crate::SidebarRow {
            id: "claude-1".to_owned(),
            name: "claude".to_owned(),
            pane: None,
            worktree_path: Some("/repo/main".to_owned()),
            worktree_branch: Some("main".to_owned()),
            unread: false,
            inactive: false,
            last_activity: Timestamp::now(),
            card: crate::RowCard::Agent(Box::new(crate::AgentCard {
                status: Some(crate::feed::AgentStatus::Waiting),
                phase: crate::agents::TurnPhase::Idle,
                task: Some("allow cargo fmt".to_owned()),
                ..crate::AgentCard::default()
            })),
        }],
        hidden_count: 0,
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        clean: None,
    }];

    assert_eq!(
        frame_interval(&slow, &UiState::default()),
        crate::sidebar::timing::BREATH_ANIMATION_FRAME
    );

    slow.worktree_groups[0].rows[0]
        .as_agent_mut()
        .unwrap()
        .status = Some(crate::feed::AgentStatus::Running);
    assert_eq!(
        frame_interval(&slow, &UiState::default()),
        crate::sidebar::timing::animation_frame(crate::sidebar::timing::DEFAULT_REFRESH_MS)
    );
}

#[test]
fn heartbeat_write_due_on_first_or_aged_write_only() {
    assert!(heartbeat_write_due(None));
    assert!(!heartbeat_write_due(Some(Instant::now())));
    assert!(heartbeat_write_due(Some(
        Instant::now() - HEARTBEAT_WRITE_INTERVAL
    )));
}

#[test]
fn suppressed_produce_panic_hook_chains_without_renderer_diagnostic() {
    let _hook_guard = PANIC_HOOK_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let sink =
        crate::diag::DiagSink::under(dir.path().to_path_buf(), workspace(), "rimz-test", None);
    let prior_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let prior_called_hook = prior_called.clone();
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |_| {
        prior_called_hook.store(true, std::sync::atomic::Ordering::SeqCst);
    }));
    install_panic_diagnostic_hook(Some(sink.clone()));

    let result = with_produce_panic_diagnostic_suppressed(|| {
        std::panic::catch_unwind(|| panic!("caught produce panic"))
    });
    let _installed = std::panic::take_hook();
    std::panic::set_hook(original);

    assert!(result.is_err());
    assert!(
        prior_called.load(std::sync::atomic::Ordering::SeqCst),
        "the suppressed diagnostic branch still chains the previously installed panic hook"
    );
    assert!(
        !sink.log_path().exists(),
        "caught producer panics are converted to fetch failures, not renderer-panic diagnostics"
    );
}

#[test]
fn notification_targeting_matches_mux_reachability_rules() {
    let (targeted_snapshot, _sidebar, first_work, _second_work) = focus_fixture();
    assert!(notification_targets_own_view(
        &targeted_snapshot,
        std::slice::from_ref(&first_work)
    ));
    assert!(!notification_targets_own_view(&targeted_snapshot, &[]));

    let foreign = PaneId::from_parts(MuxName::Zellij, "terminal_99");
    assert!(!notification_targets_own_view(
        &targeted_snapshot,
        &[foreign]
    ));

    let no_own_view = snapshot(&workspace());
    assert!(!notification_targets_own_view(
        &no_own_view,
        std::slice::from_ref(&first_work)
    ));

    assert!(desktop_notification_targets_renderer(
        MuxName::Tmux,
        &targeted_snapshot,
        &[]
    ));

    let foreign = PaneId::from_parts(MuxName::Tmux, "%99");
    assert!(desktop_notification_targets_renderer(
        MuxName::Tmux,
        &targeted_snapshot,
        &[foreign]
    ));

    let no_own_view = snapshot(&workspace());
    assert!(!desktop_notification_targets_renderer(
        MuxName::Tmux,
        &no_own_view,
        std::slice::from_ref(&first_work)
    ));

    assert!(desktop_notification_targets_renderer(
        MuxName::Zellij,
        &targeted_snapshot,
        std::slice::from_ref(&first_work)
    ));

    let foreign = PaneId::from_parts(MuxName::Zellij, "terminal_99");
    assert!(!desktop_notification_targets_renderer(
        MuxName::Zellij,
        &targeted_snapshot,
        &[foreign]
    ));
    assert!(!desktop_notification_targets_renderer(
        MuxName::Zellij,
        &targeted_snapshot,
        &[]
    ));
}

#[test]
fn focus_stranded_targets_recent_own_pane_events_only() {
    let (snapshot, sidebar, _first_work, second_work) = focus_fixture();
    let ui = UiState {
        baseline_pane: Some(second_work.clone()),
        ..UiState::default()
    };

    assert_eq!(
        focus_stranded_target(&snapshot, &ui, &sidebar, Some(&sidebar), 1_000, 1_050),
        Some(second_work.clone()),
    );

    let foreign = PaneId::from_parts(MuxName::Zellij, "terminal_99");
    let ui = UiState {
        baseline_pane: Some(second_work.clone()),
        ..UiState::default()
    };
    assert_eq!(
        focus_stranded_target(&snapshot, &ui, &sidebar, Some(&foreign), 1_000, 1_050),
        None,
    );

    let now = 1_000 + duration_millis(FOCUS_STRANDED_EVENT_TTL) + 1;
    assert_eq!(
        focus_stranded_target(&snapshot, &ui, &sidebar, Some(&sidebar), 1_000, now),
        None,
    );
}

#[test]
fn focus_stranded_falls_back_to_working_sibling_when_baseline_is_missing() {
    let (snapshot, sidebar, first_work, _second_work) = focus_fixture();
    let ui = UiState {
        baseline_pane: Some(PaneId::from_parts(MuxName::Zellij, "terminal_99")),
        ..UiState::default()
    };

    assert_eq!(
        focus_stranded_target(&snapshot, &ui, &sidebar, Some(&sidebar), 1_000, 1_050),
        Some(first_work),
    );

    let (mut snapshot, sidebar, _first_work, _second_work) = focus_fixture();
    if let Some(view) = &mut snapshot.own_view {
        view.working_pane_ids.clear();
    }

    assert_eq!(
        focus_stranded_target(
            &snapshot,
            &UiState::default(),
            &sidebar,
            Some(&sidebar),
            1_000,
            1_050,
        ),
        None,
    );
}
