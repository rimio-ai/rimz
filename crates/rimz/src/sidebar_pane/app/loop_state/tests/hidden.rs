use super::*;

impl Rig {
    fn lazy_fold(&mut self, snapshot: SidebarSnapshot, source: SnapshotSource) {
        self.deliver(self.lazy_update(snapshot, source));
    }

    pub(super) fn lazy_update(
        &self,
        snapshot: SidebarSnapshot,
        source: SnapshotSource,
    ) -> FetchUpdate {
        let own = self.state.config.own_pane.clone().unwrap();
        let view = snapshot.own_view.as_ref().unwrap();
        let mut panes = vec![pane(own.raw(), "tab_0", false)];
        panes.extend(
            view.working_pane_ids
                .iter()
                .map(|id| pane(id.raw(), "tab_0", false)),
        );
        let frame = crate::sidebar::frame::assemble_frame(panes, 1, "rimz-test");
        FetchUpdate::Fold {
            fold: Arc::new(WorkspaceFold {
                workspace: crate::sidebar::enrich::WorkspaceSnapshot(snapshot),
                frame: Some(Arc::new(frame)),
            }),
            own_pane: Some(own),
            refresh_override: self.state.config.refresh_ms_override,
            phase: FetchPhase::Final,
            source,
        }
    }
}

#[test]
fn equal_hidden_tick_restarts_the_projection_bound() {
    let mut rig = hidden_rig();
    let before = rig.state.projections;
    rig.state.last_content_fold -= Duration::from_secs(60);
    rig.lazy_fold(rig.state.current.clone(), SnapshotSource::Produced);
    assert_eq!(rig.state.projections, before + 1);
    assert_eq!(
        rig.state.content_applications, 1,
        "the tick applies clocks only"
    );
    for _ in 0..3 {
        rig.lazy_fold(rig.state.current.clone(), SnapshotSource::Produced);
    }
    assert_eq!(
        rig.state.projections,
        before + 1,
        "equal ticks restart the deferral bound"
    );
}

#[test]
fn hidden_projection_waits_for_tick_and_completes_deferred_final() {
    let mut rig = hidden_rig();
    assert_eq!(
        rig.state.projections, 1,
        "the initial fold projects on take"
    );
    rig.fetch.request(FetchRequest::default(), false);
    assert!(rig.next_request().is_some());
    for name in ["first", "latest"] {
        defer_name(&mut rig, name);
    }
    assert_eq!(rig.state.projections, 1, "deferred folds never project");
    rig.fetch.request(FetchRequest::default(), false);
    assert!(rig.next_request().is_some(), "deferred final completes");
    rig.state.last_content_fold -= Duration::from_secs(60);
    rig.state.on_snapshot(&mut rig.fetch);
    assert_eq!(
        rig.state.projections, 2,
        "the tick projects only the latest"
    );
    assert_eq!(rig.state.current.display_name, "latest");
}

fn hidden_rig() -> Rig {
    let mut rig = Rig::with_own_pane(pane("terminal_1", "tab_0", false).pane_id);
    let mut snapshot = agent_snapshot(&rig.ws);
    snapshot.own_view = Some(own_view());
    snapshot.viewed_panes.clear();
    snapshot.focused_pane = None;
    rig.lazy_fold(snapshot, SnapshotSource::Produced);
    assert!(!rig.state.watched());
    assert!(rig.state.admits_clock_only(false));
    rig
}

fn defer_name(rig: &mut Rig, name: &str) {
    let mut snapshot = rig.state.current.clone();
    snapshot.display_name = name.into();
    let before = rig.state.content_applications;
    rig.lazy_fold(snapshot, SnapshotSource::Produced);
    assert_eq!(
        rig.state.content_applications, before,
        "hidden content stays in the mailbox"
    );
}

#[test]
fn one_watched_of_fifty_seven_folds_each_cycle_while_hidden_panes_coalesce() {
    let mut rigs: Vec<_> = (0..57)
        .map(|index| {
            let own = pane(&format!("sidebar_{index}"), &format!("tab_{index}"), false).pane_id;
            let mut rig = Rig::with_own_pane(own.clone());
            let mut snapshot = agent_snapshot(&rig.ws);
            snapshot.own_view = Some(crate::store::snapshot::SidebarOwnView {
                sibling_count: 1,
                working_pane_ids: vec![
                    pane(&format!("working_{index}"), &format!("tab_{index}"), false).pane_id,
                ],
                own_view_is_daemon: false,
            });
            snapshot.viewed_panes = vec![pane("sidebar_0", "tab_0", false).pane_id];
            snapshot.focused_pane = None;
            rig.lazy_fold(snapshot, SnapshotSource::Produced);
            // A viewed tab's read dwell is a deliberate blocker, not hidden work.
            assert_eq!(rig.state.watched(), index == 0);
            rig
        })
        .collect();
    for cycle in 1..=3 {
        for (index, rig) in rigs.iter_mut().enumerate() {
            let mut snapshot = rig.state.current.clone();
            snapshot.display_name = format!("cycle-{cycle}");
            rig.lazy_fold(snapshot, SnapshotSource::Produced);
            assert_eq!(
                rig.state.content_applications,
                if index == 0 { 1 + cycle } else { 1 }
            );
        }
    }
    for rig in rigs.iter_mut().skip(1) {
        rig.state.last_content_fold -= Duration::from_secs(60);
        rig.state.on_snapshot(&mut rig.fetch);
        assert_eq!(rig.state.content_applications, 2);
        assert_eq!(rig.state.current.display_name, "cycle-3");
    }
}

#[test]
fn deferred_final_completes_requests_and_refreshes_backstops() {
    let mut rig = hidden_rig();
    rig.fetch.request(FetchRequest::default(), false);
    assert!(rig.next_request().is_some());
    let before = Instant::now();
    defer_name(&mut rig, "deferred");
    assert!(rig.state.fetched_at >= before);
    assert!(rig.state.last_self_close_check >= before);
    rig.fetch.request(FetchRequest::default(), false);
    assert!(
        rig.next_request().is_some(),
        "deferred final releases single-flight"
    );
    rig.state.last_content_fold -= Duration::from_secs(60);
    assert_eq!(rig.state.frame_timing().1, FRAME_MIN_TIMEOUT);
    rig.state.on_snapshot(&mut rig.fetch);
    assert_eq!(rig.state.current.display_name, "deferred");
}

#[test]
fn reveal_and_changed_own_view_never_defer() {
    for reveal in [true, false] {
        let mut rig = hidden_rig();
        let mut snapshot = rig.state.current.clone();
        snapshot.display_name = "immediate".into();
        if reveal {
            snapshot.viewed_panes = snapshot.own_view.as_ref().unwrap().working_pane_ids.clone();
        } else {
            snapshot.own_view = Some(empty_own_view());
        }
        rig.lazy_fold(snapshot, SnapshotSource::Produced);
        assert_eq!(rig.state.current.display_name, "immediate");
        assert_eq!(
            rig.state.should_exit, !reveal,
            "an observed sibling disappearing closes immediately"
        );
    }
}

#[test]
fn unsettled_hidden_panes_and_failed_deliveries_never_defer() {
    for blocker in 0..3 {
        let mut rig = hidden_rig();
        match blocker {
            0 => rig.state.health.failure_streak = 1,
            1 => rig.state.gate.reject_streak = 1,
            _ => {
                rig.state.self_close.seen_sibling = false;
                rig.state.self_close.should_close(Some(0), Instant::now());
            }
        }
        let mut snapshot = rig.state.current.clone();
        snapshot.display_name = "unsettled".into();
        rig.lazy_fold(snapshot, SnapshotSource::Produced);
        assert_eq!(rig.state.current.display_name, "unsettled");
    }
    let mut rig = hidden_rig();
    defer_name(&mut rig, "before failure");
    rig.deliver(FetchUpdate::Failed {
        error: "offline".into(),
    });
    assert_eq!(rig.state.current.display_name, "before failure");
    assert_eq!(rig.state.health.failure_streak, 1);
}

#[test]
fn input_and_non_nudge_events_flush_hidden_content() {
    for input in [
        Wakeup::Key(KeyAction::Dismiss),
        Wakeup::Scroll { down: true },
        Wakeup::MouseClick { column: 0, row: 0 },
    ] {
        let mut rig = hidden_rig();
        defer_name(&mut rig, "before input");
        rig.state
            .on_input(input, &mut rig.terminal, &mut rig.fetch)
            .unwrap();
        assert_eq!(rig.state.current.display_name, "before input");
    }
    let mut rig = hidden_rig();
    defer_name(&mut rig, "before resize");
    rig.state
        .on_resize(&mut rig.fetch, &mut rig.terminal, None)
        .unwrap();
    assert_eq!(rig.state.current.display_name, "before resize");
    assert!(!rig.state.dirty, "the resize paint shows the flushed fold");
    let mut rig = hidden_rig();
    defer_name(&mut rig, "before filter");
    rig.event(SidebarEvent::BodyFilterChanged);
    assert_eq!(rig.state.current.display_name, "before filter");
}

fn unread_snapshot(rig: &Rig, unread: bool) -> SidebarSnapshot {
    let mut snapshot = rig.state.current.clone();
    let row = &mut Arc::make_mut(&mut Arc::make_mut(&mut snapshot.worktree_groups)[0].rows)[0];
    row.unread = unread;
    if let crate::store::snapshot::RowCard::Agent(card) = &mut row.card {
        Arc::make_mut(card).status = crate::agents::AgentStatus::Waiting;
    }
    snapshot
}

fn capture_bells(rig: &mut Rig) -> FrameOutput {
    rig.state.config.notification_prefs.desktop = crate::config::DesktopNotificationMode::Off;
    let output = FrameOutput::default();
    rig.terminal = Terminal::with_options(
        PaneBackend::headless(output.clone()),
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 54, 24)),
        },
    )
    .unwrap();
    output
}

#[test]
fn notification_rings_against_the_deferred_unread_fold() {
    for drain in [false, true] {
        let mut rig = hidden_rig();
        let output = capture_bells(&mut rig);
        let snapshot = unread_snapshot(&rig, true);
        let panes = snapshot.own_view.as_ref().unwrap().working_pane_ids.clone();
        rig.result_tx
            .send(rig.lazy_update(snapshot, SnapshotSource::Produced))
            .unwrap();
        if drain {
            rig.state.on_snapshot(&mut rig.fetch);
        }
        assert!(
            !rig.state.current.rows().next().unwrap().unread,
            "unread fold deferred"
        );
        rig.event(SidebarEvent::Notify {
            title: "waiting".into(),
            body: "waiting".into(),
            panes,
            recheck_unread: true,
            notification_kind: Some("waiting".into()),
        });
        assert_eq!(*output.0.lock().unwrap(), b"\x07");
    }
}

#[test]
fn due_reminder_flushes_a_deferred_unread_clear_before_ringing() {
    let mut rig = hidden_rig();
    rig.state.last_content_fold -= Duration::from_secs(60);
    rig.lazy_fold(unread_snapshot(&rig, true), SnapshotSource::Produced);
    assert!(rig.state.current.rows().next().unwrap().unread);
    let output = capture_bells(&mut rig);
    rig.state
        .remind
        .note_ring(crate::utils::time::unix_now_ms() - 61_000);
    rig.lazy_fold(unread_snapshot(&rig, false), SnapshotSource::Produced);
    assert!(
        rig.state.current.rows().next().unwrap().unread,
        "clear deferred"
    );
    rig.state.maybe_remind(&mut rig.fetch, &mut rig.terminal);
    assert!(
        output.0.lock().unwrap().is_empty(),
        "fresh read row must not ring"
    );
    assert!(!rig.state.current.rows().next().unwrap().unread);
}
