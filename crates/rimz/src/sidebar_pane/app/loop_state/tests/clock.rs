use super::*;
use std::sync::atomic::Ordering;

#[test]
fn tab_read_dwell_ignores_wall_steps() {
    let clock = LoopClock::play("2026-06-09T12:00:00Z".parse().unwrap());
    let own = PaneId::from_parts(MuxName::Zellij, "terminal_99");
    let mut rig = Rig::with_clock(Some(own.clone()), BodyLens::default(), None, clock.clone());
    rig.view().current = agent_snapshot(&rig.ws);
    rig.view().current.own_view = Some(SidebarOwnView {
        sibling_count: 1,
        working_pane_ids: vec![],
        own_view_is_daemon: false,
    });
    rig.view().current.viewed_panes = vec![own];
    rig.view().ui.viewing_own_tab = Some(false);
    let marks = ReadMarks::load(&rig.runtime);
    let mut clear = ReadClear::default();
    let wall = clock.timestamp();
    rig.view()
        .sweep_tab_read_receipts(None, &marks, wall, &mut clear);
    assert_eq!(
        rig.view().tab_read_dwell_until,
        Some(clock.now() + TAB_READ_DWELL)
    );
    rig.view()
        .sweep_tab_read_receipts(None, &marks, wall + Duration::from_secs(3_600), &mut clear);
    assert!(
        rig.view().tab_read_dwell_until.is_some(),
        "a forward wall step is not a read"
    );
    clock.advance(TAB_READ_DWELL);
    rig.view()
        .sweep_tab_read_receipts(None, &marks, wall - Duration::from_secs(3_600), &mut clear);
    assert!(
        rig.view().tab_read_dwell_until.is_none(),
        "a backward wall step cannot extend the dwell"
    );
}

#[test]
fn body_filter_observation_is_not_backdated_to_the_fold() {
    let mut rig = Rig::new();
    let filter = BodyLens {
        query: Some("no such row".to_owned()),
        ..Default::default()
    };
    crate::sidebar::body_filter::write(&rig.runtime, &filter).unwrap();
    let previous = rig.state.current.clone();
    let stale = Instant::now() - Duration::from_secs(1);
    let before_load = Instant::now();
    rig.state
        .reconcile_selection_and_order(&previous, None, None, false, true, stale);
    assert!(rig.view().filter_observed_at.unwrap() >= before_load);
    rig.shared_fold(
        agent_snapshot(&rig.ws),
        Arc::new(super::super::super::fetch::FoldInputs {
            sampled_at: before_load,
            ..Default::default()
        }),
    );
    assert_eq!(
        rig.view().ui.make_up_filter,
        filter,
        "an older worker cut cannot undo the filter just loaded"
    );
    {
        let mut guard = rig.view();
        let state = &mut *guard;
        assert_eq!(state.ui.visible_roster(&state.current).len(), 0);
    }
}

#[test]
fn play_clock_drives_the_loop_without_focus_or_notification_effects() {
    let epoch: Timestamp = "2026-06-09T12:00:00Z".parse().unwrap();
    let clock = LoopClock::play(epoch);
    let mut rig = Rig::with_clock(None, BodyLens::default(), None, clock.clone());
    let output = FrameOutput::default();
    rig.terminal = Terminal::with_options(
        PaneBackend::headless(output.clone()),
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 80, 24)),
        },
    )
    .unwrap();
    let mut snapshot = agent_snapshot(&rig.ws);
    snapshot.now = epoch;
    let row = &mut Arc::make_mut(&mut Arc::make_mut(&mut snapshot.worktree_groups)[0].rows)[0];
    row.unread = true;
    let crate::store::snapshot::RowCard::Agent(card) = &mut row.card else {
        panic!("agent fixture");
    };
    Arc::make_mut(card).status = crate::agents::AgentStatus::Waiting;
    let target = row.pane.as_ref().unwrap().pane_id.clone();
    snapshot.own_view = Some(SidebarOwnView {
        sibling_count: 1,
        working_pane_ids: vec![target.clone()],
        own_view_is_daemon: false,
    });
    rig.fold(snapshot, SnapshotSource::Produced);
    rig.state.config.notification_prefs.enabled = true;
    rig.state.config.notification_prefs.remind_secs = 1;
    rig.state.config.notification_prefs.command = Some("exit 99".into());
    clock.advance(Duration::from_secs(1));
    assert_eq!(rig.state.clock.timestamp(), epoch + Duration::from_secs(1));
    rig.state.handle_notification(
        &mut rig.terminal,
        SidebarEvent::Notify {
            title: "waiting".into(),
            body: "waiting".into(),
            panes: vec![target.clone()],
            recheck_unread: false,
            notification_kind: Some("waiting".into()),
        },
    );
    assert!(
        output.0.lock().unwrap().is_empty(),
        "no terminal notifications"
    );

    rig.state.paint_now(&mut rig.terminal, clock.now()).unwrap();
    assert_eq!(
        rig.view().ui.frame_ms,
        1_000,
        "paint uses scenario elapsed time"
    );
    let row = rig.state.ui.interactions.line_for_row(0).unwrap() as u16;
    let (tx, rx) = std::sync::mpsc::sync_channel(4);
    let mut inbox = WakeupInbox::new(rx);
    tx.send(Wakeup::MouseClick { column: 2, row }).unwrap();
    let (painted, deadline) = rig
        .state
        .advance(&mut inbox, &mut rig.fetch, &mut rig.terminal)
        .unwrap();
    assert!(painted);
    assert!(deadline > clock.now());
    assert!(
        rig.state.last_heartbeat.is_none(),
        "play publishes no heartbeat"
    );
    assert_eq!(rig.view().ui.selected_pane, Some(target));
    assert!(
        rig.state.clicked_row.is_some(),
        "click reached the focus effect"
    );
    assert_eq!(
        rig.view().ui.freeze.as_ref().unwrap().armed_ms,
        clock.now_ms() as i64
    );
    assert_eq!(clock.focus_calls.load(Ordering::Relaxed), 0);
    rig.state
        .on_resize(&mut rig.fetch, &mut rig.terminal, Some(80))
        .unwrap();
    assert!(
        rig.state.caps_refresh_deadline.is_none(),
        "play never arms an external caps probe"
    );

    let wall = LoopClock::wall();
    wall.focus(|| {});
    assert_eq!(wall.focus_calls.load(Ordering::Relaxed), 1);
}
