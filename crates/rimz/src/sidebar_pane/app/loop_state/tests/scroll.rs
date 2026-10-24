use super::*;
use crate::ids::FocusNonce;
use crate::sidebar_pane::app::inbox::WakeupInbox;

fn scroll_rig() -> (Rig, FrameOutput) {
    let mut rig = Rig::with_own_pane(pane("terminal_99", "tab_0", false).pane_id);
    let mut snapshot = agent_snapshot(&rig.ws);
    let first = snapshot.worktree_groups[0].rows[0].clone();
    let rows = Arc::make_mut(&mut Arc::make_mut(&mut snapshot.worktree_groups)[0].rows);
    *rows = (0..20)
        .map(|index| {
            let mut row = first.clone();
            row.id = format!("agent-{index}");
            row.name = format!("scroll-card-{index}");
            row.pane = Some(pane(&format!("terminal_{index}"), "tab_0", false));
            row
        })
        .collect();
    rig.view()
        .ui()
        .expanded_groups
        .insert(snapshot.worktree_groups[0].key.clone());
    rig.view().ctx().current = snapshot;
    let output = FrameOutput::default();
    rig.terminal = Terminal::with_options(
        PaneBackend::headless(output.clone()),
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 80, 24)),
        },
    )
    .unwrap();
    rig.state
        .on_input(Wakeup::Resize, &mut rig.terminal, &mut rig.fetch)
        .unwrap();
    assert!(rig.state.pane.ui.drawn_scroll_max.unwrap() > 12);
    output.0.lock().unwrap().clear();
    output.1.store(0, std::sync::atomic::Ordering::SeqCst);
    (rig, output)
}

fn queued(wakeups: impl IntoIterator<Item = Wakeup>) -> WakeupInbox {
    let (tx, rx) = std::sync::mpsc::sync_channel(64);
    for wakeup in wakeups {
        tx.send(wakeup).unwrap();
    }
    WakeupInbox::new(rx)
}

fn play_clock(rig: &mut Rig) -> LoopClock {
    let clock = LoopClock::play(Timestamp::now());
    rig.state.pane.clock = clock.clone();
    clock
}

#[test]
fn input_uses_the_clock_after_preparation() {
    let (mut rig, _) = scroll_rig();
    let clock = play_clock(&mut rig);
    let input = std::iter::once_with(|| {
        clock.advance(Duration::from_secs(2));
        Wakeup::Scroll { down: true }
    });
    rig.state.apply_input(input, &mut rig.terminal).unwrap();
    assert_eq!(
        rig.state
            .inspect_view(|ui, _| ui.freeze.as_ref().unwrap().armed_ms),
        clock.timestamp().as_millisecond()
    );
}

struct AdvancingOutput(LoopClock);

impl std::io::Write for AdvancingOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.advance(Duration::from_secs(2));
        Ok(())
    }
}

#[test]
fn input_frame_deadline_uses_the_clock_after_output() {
    let (mut rig, _) = scroll_rig();
    let clock = play_clock(&mut rig);
    rig.terminal
        .backend_mut()
        .play_output(AdvancingOutput(clock.clone()));
    rig.state
        .apply_input([Wakeup::Scroll { down: true }], &mut rig.terminal)
        .unwrap();
    assert!(
        rig.state.pane.next_frame > clock.now(),
        "the next frame must not already be due after output"
    );
}

#[test]
fn input_advances_the_planes_view_before_the_client_moves_tabs() {
    use crate::ids::MuxClientId;
    use crate::pane::ClientPaneView;

    let plane = super::super::super::plane::DataPlane::detached();
    let (mut source, _) = scroll_rig();
    let own = source.state.pane.config.own_pane.clone().unwrap();
    plane.views.lock().unwrap().reconcile(
        &[ClientPaneView {
            client_id: MuxClientId::Zellij(1),
            pane_id: own.clone(),
        }],
        true,
        source.state.pane.clock.now(),
        Duration::from_secs(60),
    );
    let group = source.state.views.mirror().worktree_groups[0].key.clone();
    source.share_views(plane.views.clone());
    plane
        .views
        .lock()
        .unwrap()
        .paint(&own, source.state.pane.clock.now())
        .expanded_groups
        .insert(group);
    source
        .state
        .on_input(
            Wakeup::Scroll { down: true },
            &mut source.terminal,
            &mut source.fetch,
        )
        .unwrap();
    assert_eq!(
        plane
            .views
            .lock()
            .unwrap()
            .paint(&own, source.state.pane.clock.now())
            .scroll_offset,
        3,
        "input must advance the plane-owned presentation, not a pane copy"
    );
}

fn pane_pair(mux: MuxName) -> (Rig, Rig, FrameOutput) {
    let (mut source, _) = scroll_rig();
    let (mut destination, output) = scroll_rig();
    for (rig, own, working) in [(&mut source, 99, 0), (&mut destination, 98, 19)] {
        rig.state.pane.config.mux = mux;
        let id = |index| {
            PaneId::from_parts(
                mux,
                match mux {
                    MuxName::Tmux => format!("%{index}"),
                    MuxName::Zellij => format!("terminal_{index}"),
                },
            )
        };
        rig.state.pane.config.own_pane = Some(id(own));
        let mut snapshot = rig.state.views.mirror().clone();
        for (index, row) in Arc::make_mut(&mut Arc::make_mut(&mut snapshot.worktree_groups)[0].rows)
            .iter_mut()
            .enumerate()
        {
            row.pane.as_mut().unwrap().pane_id = id(index);
        }
        snapshot.own_view = Some(SidebarOwnView {
            sibling_count: 1,
            working_pane_ids: vec![id(working)],
            own_view_is_daemon: false,
        });
        snapshot.focused_pane = Some(id(0));
        snapshot.viewed_panes = vec![id(0)];
        rig.fold(snapshot, SnapshotSource::Produced);
        rig.state.drain_snapshot(&mut rig.fetch, false);
    }
    source.state.pane.dirty = true;
    paint_at(&mut source, 0);
    assert!(source.state.watched());
    assert!(!destination.state.watched());
    (source, destination, output)
}

fn paint_scrolled(source: &mut Rig, offset: usize) {
    source.view().ui().scroll_offset = offset;
    source.view().ui().offset_authoritative = true;
    source.view().ui().scroll_ease.cancel();
    source.view().ui().landed_open = true;
    source.state.pane.dirty = true;
    paint_at(source, 0);
}

pub(super) struct SharedPanes {
    pub(super) plane: super::super::super::plane::DataPlane,
    pub(super) panes: [Rig; 2],
    pub(super) snapshot: SidebarSnapshot,
    pub(super) clock: LoopClock,
}

impl SharedPanes {
    pub(super) fn new(mux: MuxName) -> Self {
        let mut pair = Self::unpainted(mux);
        pair.sample(&[(1, 0)]);
        pair.fold(1);
        pair.panes[0].state.with_view_for_test(Route::READ, |_, v| {
            let group = v.context.current.worktree_groups[0].key.clone();
            v.state.expanded_groups.insert(group);
        });
        pair.paint(0);
        pair
    }

    fn unpainted(mux: MuxName) -> Self {
        let (a, b, _) = pane_pair(mux);
        let snapshot = a.state.views.mirror().clone();
        let clock = LoopClock::play(snapshot.now);
        let plane = super::super::super::plane::DataPlane::detached();
        let mut pair = Self {
            plane,
            panes: [a, b],
            snapshot,
            clock,
        };
        for rig in &mut pair.panes {
            rig.state.pane.clock = pair.clock.clone();
            rig.share_views(pair.plane.views.clone());
        }
        pair
    }

    pub(super) fn sample(&mut self, clients: &[(u32, usize)]) {
        let mux = self.panes[0].state.pane.config.mux;
        let mut panes = Vec::new();
        for (index, rig) in self.panes.iter().enumerate() {
            let own = rig.state.pane.config.own_pane.as_ref().unwrap();
            let mut sidebar = pane(own.raw(), &format!("tab_{index}"), false);
            sidebar.pane_id = own.clone();
            sidebar.command = Some("rimz-sidebar".into());
            panes.push(sidebar);
        }
        for (index, row) in self.snapshot.rows().enumerate() {
            let mut working = row.pane.clone().unwrap();
            working.view_id = Some(format!("tab_{}", usize::from(index >= 10)));
            panes.push(working);
        }
        let mut frame =
            crate::sidebar::frame::assemble_frame(panes, self.clock.now_ms(), "rimz-test");
        frame.client_views = clients
            .iter()
            .map(|(client, destination)| crate::pane::ClientPaneView {
                client_id: match mux {
                    MuxName::Tmux => crate::ids::MuxClientId::Tmux(format!("client-{client}")),
                    MuxName::Zellij => crate::ids::MuxClientId::Zellij(*client),
                },
                pane_id: self.panes[*destination]
                    .state
                    .pane
                    .config
                    .own_pane
                    .clone()
                    .unwrap(),
            })
            .collect();
        frame.client_view_fresh = true;
        self.snapshot.viewed_panes = frame
            .client_views
            .iter()
            .map(|client| client.pane_id.clone())
            .collect();
        let fold = WorkspaceFold {
            workspace: crate::sidebar::enrich::WorkspaceSnapshot(self.snapshot.clone()),
            frame: Some(Arc::new(frame)),
        };
        let mut views = self.plane.views.lock().unwrap();
        views.publish(FetchUpdate::Fold {
            fold: Arc::new(fold),
            phase: FetchPhase::Final,
            source: SnapshotSource::Produced,
        });
        for rig in &self.panes {
            rig.completion.store(true, Ordering::Relaxed);
        }
    }

    pub(super) fn paint(&mut self, index: usize) {
        let rig = &mut self.panes[index];
        rig.state.pane.dirty = true;
        rig.state
            .paint_now(&mut rig.terminal, self.clock.now())
            .unwrap();
    }

    fn fold(&mut self, index: usize) {
        let rig = &mut self.panes[index];
        rig.state.on_snapshot(&mut rig.fetch);
    }

    fn input(&mut self, index: usize) {
        let rig = &mut self.panes[index];
        rig.state
            .on_input(
                Wakeup::Scroll { down: true },
                &mut rig.terminal,
                &mut rig.fetch,
            )
            .unwrap();
    }

    pub(super) fn view(&self, index: usize) -> render::ViewState {
        self.panes[index].state.inspect_view(|ui, _| ui.clone())
    }
}

#[test]
fn first_observed_reveal_reads_siblings_after_dwell_on_both_backends() {
    for mux in [MuxName::Tmux, MuxName::Zellij] {
        let mut pair = SharedPanes::unpainted(mux);
        for row in Arc::make_mut(&mut Arc::make_mut(&mut pair.snapshot.worktree_groups)[0].rows) {
            row.unread = true;
        }
        pair.sample(&[]);
        pair.fold(1);
        assert!(!pair.panes[1].state.watched());
        pair.sample(&[(1, 1)]);
        pair.paint(1);

        assert!(
            pair.panes[1]
                .state
                .inspect_view(|_, view| view.tab_read_dwell_until.is_some())
        );
        let arrived = pair.view(1);
        assert!(arrived.freeze.is_some());
        assert_eq!(arrived.viewing_own_tab, Some(true));
        let held = arrived.freeze.unwrap().frozen;
        assert_eq!(held.groups, arrived.last_order.groups);
        assert_eq!(held.rows, arrived.last_order.rows);
        assert!(
            pair.panes[1]
                .state
                .views
                .mirror()
                .rows()
                .find(|row| row.id == "agent-10")
                .unwrap()
                .unread
        );

        pair.clock
            .advance(TAB_READ_DWELL + Duration::from_millis(1));
        pair.sample(&[(1, 1)]);
        pair.fold(1);
        let current = &pair.panes[1].state.views.mirror();
        let own = current.own_view.as_ref().unwrap();
        assert!(
            current
                .rows()
                .filter(|row| row.pane_ids().any(|id| own.working_pane_ids.contains(id)))
                .all(|row| !row.unread)
        );
        assert!(
            ReadMarks::load(&pair.panes[1].runtime)
                .cleared_at_ms("agent-10")
                .is_some()
        );
    }
}

#[test]
fn reattach_after_expiry_reads_siblings_after_dwell_on_both_backends() {
    for mux in [MuxName::Tmux, MuxName::Zellij] {
        let mut pair = SharedPanes::new(mux);
        pair.input(0);
        assert!(pair.view(0).scroll_offset > 0);
        let selected = pair
            .snapshot
            .rows()
            .next()
            .unwrap()
            .pane_ids()
            .next()
            .unwrap()
            .clone();
        pair.panes[0].view().ui().baseline_pane = Some(selected.clone());
        pair.panes[0].view().ui().selected_pane = Some(selected);
        pair.panes[0].view().ui().selected_index = Some(0);
        pair.sample(&[]);
        pair.plane.views.lock().unwrap().refresh(pair.clock.now());
        pair.clock.advance(
            Duration::from_millis(pair.snapshot.sidebar.afk_after_ms()) + Duration::from_millis(1),
        );
        for row in Arc::make_mut(&mut Arc::make_mut(&mut pair.snapshot.worktree_groups)[0].rows) {
            row.unread = true;
        }
        pair.sample(&[(1, 0)]);
        pair.panes[0].state.with_view_for_test(Route::READ, |_, v| {
            assert_eq!(v.state.scroll_offset, 0);
            assert_eq!(v.state.selected_index, None);
            assert_eq!(v.state.baseline_pane, None);
            assert!(v.state.freeze.is_none());
        });
        pair.paint(0);
        assert!(
            pair.panes[0]
                .state
                .inspect_view(|_, view| view.tab_read_dwell_until.is_some())
        );
        assert!(pair.view(0).freeze.is_some());
        assert!(
            pair.panes[0]
                .state
                .views
                .mirror()
                .rows()
                .find(|row| row.id == "agent-1")
                .unwrap()
                .unread
        );

        pair.clock
            .advance(TAB_READ_DWELL + Duration::from_millis(1));
        pair.sample(&[(1, 0)]);
        pair.fold(0);
        let current = &pair.panes[0].state.views.mirror();
        let own = current.own_view.as_ref().unwrap();
        assert!(
            current
                .rows()
                .filter(|row| row.pane_ids().any(|id| own.working_pane_ids.contains(id)))
                .all(|row| !row.unread)
        );
        assert!(
            ReadMarks::load(&pair.panes[0].runtime)
                .cleared_at_ms("agent-1")
                .is_some()
        );
    }
}

#[test]
fn born_visible_arms_no_arrival_effects_on_both_backends() {
    for mux in [MuxName::Tmux, MuxName::Zellij] {
        let pair = SharedPanes::new(mux);
        assert_eq!(
            pair.panes[0]
                .state
                .inspect_view(|_, view| view.tab_read_dwell_until),
            None
        );
        assert!(pair.view(0).freeze.is_none());
        assert!(!pair.view(0).arrival_pending);
    }
}

#[test]
fn frozen_view_survives_round_trip_on_both_backends() {
    for mux in [MuxName::Tmux, MuxName::Zellij] {
        let mut pair = SharedPanes::new(mux);
        pair.input(0);
        let original = pair.view(0);
        for destination in [1, 0] {
            pair.clock.advance(Duration::from_millis(50));
            pair.sample(&[(1, destination)]);
            pair.paint(destination);
            let arrived = pair.view(destination);
            assert_eq!(
                arrived.freeze.as_ref().map(|hold| hold.deadline_ms(false)),
                original.freeze.as_ref().map(|hold| hold.deadline_ms(false)),
                "arrival never restarts the hold"
            );
            assert_eq!(arrived.last_order, original.last_order);
            assert_eq!(arrived.scroll_offset, original.scroll_offset);
            assert!(!pair.panes[1 - destination].state.observed_watched());
        }
    }
}

#[test]
fn arrival_holds_last_painted_order_and_catches_up_content() {
    let mut pair = SharedPanes::new(MuxName::Zellij);
    let order = pair.view(0).last_order;
    pair.snapshot.display_name = "newest content".into();
    Arc::make_mut(&mut Arc::make_mut(&mut pair.snapshot.worktree_groups)[0].rows).swap(4, 5);
    pair.sample(&[(1, 1)]);
    pair.paint(1);
    assert_eq!(pair.view(1).last_order, order);
    assert_eq!(
        pair.panes[1].state.views.mirror().display_name,
        "newest content"
    );
}

#[test]
fn an_expired_hold_keeps_the_last_painted_order_for_the_first_return_frame() {
    let mut pair = SharedPanes::new(MuxName::Zellij);
    pair.input(0);
    let before = pair.view(0);
    pair.sample(&[]);
    pair.plane.views.lock().unwrap().refresh(pair.clock.now());
    pair.clock.advance(Duration::from_secs(31));
    Arc::make_mut(&mut Arc::make_mut(&mut pair.snapshot.worktree_groups)[0].rows).swap(4, 5);
    pair.sample(&[(1, 1)]);
    pair.paint(1);
    assert_eq!(
        pair.view(1).last_order,
        before.last_order,
        "return first paints the frame the client actually saw"
    );
    assert_eq!(
        pair.view(1)
            .freeze
            .as_ref()
            .map(|hold| hold.deadline_ms(false)),
        before.freeze.as_ref().map(|hold| hold.deadline_ms(false)),
        "the expired epoch is not restarted"
    );
}

#[test]
fn newborn_paints_the_clients_view_and_latest_content_in_its_first_frame() {
    let mut pair = SharedPanes::new(MuxName::Zellij);
    pair.input(0);
    let target = pair.view(0).scroll_offset;
    let before = pair.panes[1].state.pane.paints;
    let reductions = pair.panes[1].state.pane.content_applications;
    pair.panes[1].state.pane.ui = UiState::default();
    *pair.panes[1].state.views.scratch_for_test().0 = ViewState::default();
    for name in ["hidden one", "hidden two", "latest at arrival"] {
        pair.snapshot.display_name = name.into();
        pair.sample(&[(1, 0)]);
        pair.panes[1].maintenance();
        assert_eq!(pair.panes[1].state.pane.content_applications, reductions);
    }
    pair.sample(&[(1, 1)]);
    pair.paint(1);
    assert_eq!(pair.panes[1].state.pane.paints, before + 1);
    assert_eq!(
        pair.panes[1].state.views.mirror().display_name,
        "latest at arrival"
    );
    assert_eq!(pair.view(1).scroll_offset, target);
}

#[test]
fn hidden_resize_changes_no_client_view() {
    for mux in [MuxName::Tmux, MuxName::Zellij] {
        let mut pair = SharedPanes::new(mux);
        pair.input(0);
        let before = pair.view(0);
        let reductions = pair.panes[1].state.pane.content_applications;
        let rig = &mut pair.panes[1];
        rig.state
            .on_resize(&mut rig.fetch, &mut rig.terminal, Some(80))
            .unwrap();
        assert_eq!(pair.view(0).scroll_offset, before.scroll_offset);
        assert_eq!(
            pair.view(0)
                .freeze
                .as_ref()
                .map(|hold| hold.deadline_ms(false)),
            before.freeze.as_ref().map(|hold| hold.deadline_ms(false))
        );
        assert_eq!(pair.panes[1].state.pane.content_applications, reductions);
    }
}

#[test]
fn input_before_resolved_reveal_survives_the_next_sample_on_both_backends() {
    for mux in [MuxName::Tmux, MuxName::Zellij] {
        let mut pair = SharedPanes::new(mux);
        pair.input(0);
        let source = pair.view(0).scroll_offset;
        pair.input(1);
        let destination = pair.view(1).scroll_offset;
        assert!(destination > source);
        assert_eq!(
            pair.view(0).scroll_offset,
            source,
            "evidence cannot take a resolved painting view"
        );
        pair.sample(&[(1, 0)]);
        pair.paint(1);
        assert_eq!(
            pair.view(1).scroll_offset,
            destination,
            "older membership cannot undo input"
        );
        pair.sample(&[(1, 1)]);
        pair.paint(1);
        assert_eq!(
            pair.view(1).scroll_offset,
            destination,
            "the newly resolved viewer joins the newer view"
        );
    }
}

#[test]
fn two_zellij_views_keep_independent_input_until_they_join() {
    let mut pair = SharedPanes::new(MuxName::Zellij);
    pair.sample(&[(1, 0), (2, 1)]);
    pair.paint(1);
    pair.input(0);
    let before = pair.view(0).scroll_offset;
    pair.input(1);
    pair.input(1);
    assert_eq!(pair.view(0).scroll_offset, before);
    let destination = pair.view(1).scroll_offset;
    pair.sample(&[(1, 1), (2, 1)]);
    pair.paint(1);
    assert_eq!(pair.view(1).scroll_offset, destination);
}

#[test]
fn input_before_a_coviewer_split_keeps_the_copied_selection() {
    let mut pair = SharedPanes::new(MuxName::Zellij);
    pair.sample(&[(1, 0), (2, 0)]);
    pair.panes[0].input(KeyAction::Bottom);
    let selected = pair.view(0).selected_pane.clone();
    assert!(selected.is_some());
    let focused = pair
        .snapshot
        .rows()
        .nth(1)
        .unwrap()
        .pane
        .as_ref()
        .map(|pane| pane.pane_id.clone());
    pair.snapshot.focused_pane = focused;
    pair.sample(&[(1, 0), (2, 0)]);
    pair.panes[1].input(KeyAction::Other);
    assert_eq!(
        pair.view(1).selected_pane,
        selected,
        "an unresolved destination must not derive focus from unrelated clients"
    );
    pair.sample(&[(1, 0), (2, 1)]);
    pair.paint(1);
    assert_eq!(pair.view(1).selected_pane, selected);
}

#[test]
fn resolved_departure_cancels_old_input_optimism_before_the_client_returns() {
    let mut pair = SharedPanes::new(MuxName::Zellij);
    pair.input(1);
    pair.sample(&[(1, 0), (2, 1)]);
    pair.paint(1);
    pair.input(1);
    pair.panes[0].input(KeyAction::Bottom);
    let joined = pair.view(0).selected_pane;
    assert!(joined.is_some());
    pair.clock.advance(Duration::from_millis(50));
    pair.sample(&[(1, 0), (2, 0)]);
    pair.paint(0);
    pair.paint(1);
    assert!(
        !pair.panes[1].state.watched(),
        "resolved departure must supersede the departed pane's input optimism"
    );
    pair.clock.advance(Duration::from_millis(50));
    pair.sample(&[(1, 0), (2, 1)]);
    pair.paint(1);
    assert_eq!(pair.view(1).selected_pane, joined);
}

#[test]
fn an_unresolved_pane_releases_its_view_when_viewing_evidence_expires() {
    let mut pair = SharedPanes::new(MuxName::Zellij);
    pair.input(1);
    let pane = pair.panes[1].state.pane.config.own_pane.clone().unwrap();
    assert!(pair.plane.views.lock().unwrap().peek(&pane).is_some());
    pair.clock.advance(Duration::from_secs(3));
    pair.panes[1].state.frame_timing();
    assert!(
        pair.plane.views.lock().unwrap().peek(&pane).is_none(),
        "a hidden pane cannot keep holding an anonymous view"
    );
}

#[test]
fn tmux_clients_share_a_view_through_focus_overlay_and_detach() {
    let mut pair = SharedPanes::new(MuxName::Tmux);
    pair.sample(&[(1, 0), (2, 0)]);
    pair.input(0);
    let before = pair.view(0).scroll_offset;
    let rig = &mut pair.panes[0];
    rig.event(SidebarEvent::FocusChanged {
        focused: vec![rig.state.pane.config.own_pane.clone().unwrap()],
        unfocused: vec![],
    });
    pair.sample(&[(2, 0)]);
    pair.paint(0);
    assert_eq!(pair.view(0).scroll_offset, before);
}

#[test]
fn a_view_change_wakes_no_unrelated_pane() {
    let mut pair = SharedPanes::new(MuxName::Zellij);
    let (tx, rx) = std::sync::mpsc::sync_channel(64);
    let _subscription = pair.plane.subscribe(&pair.panes[1].state.pane.config, tx);
    pair.input(0);
    assert!(rx.try_recv().is_err());
}

#[test]
fn glide_arrival_snaps_to_target_in_the_receiving_geometry() {
    let mut pair = SharedPanes::new(MuxName::Zellij);
    pair.input(0);
    let target = pair.view(0).scroll_offset;
    assert!(pair.view(0).scroll_ease.in_flight());
    pair.panes[1].terminal = Terminal::with_options(
        PaneBackend::headless(FrameOutput::default()),
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 80, 30)),
        },
    )
    .unwrap();
    pair.sample(&[(1, 1)]);
    pair.paint(1);
    assert!(!pair.view(1).scroll_ease.in_flight());
    assert_eq!(pair.view(1).scroll_offset, target);
    assert_eq!(pair.panes[1].state.pane.painted_height, 30);
}

fn offscreen_focus_marker(mux: MuxName) -> (Rig, Instant) {
    let (mut rig, _) = scroll_rig();
    rig.state.pane.config.mux = mux;
    let mut focused = rig.state.views.mirror().clone();
    focused.focused_pane = focused.worktree_groups[0].rows[19]
        .pane
        .as_ref()
        .map(|pane| pane.pane_id.clone());
    let before = Instant::now();
    rig.fold(focused, SnapshotSource::Produced);
    let after = Instant::now();
    assert!(
        matches!(
            rig.view().ctx().focus_marker,
            Some(FocusMarker::Dwelling(_))
        ),
        "the consuming focus fold arms a dwell"
    );
    let Some(FocusMarker::Dwelling(deadline)) = rig.view().ctx().focus_marker else {
        unreachable!()
    };
    let dwell = Duration::from_millis(if mux == MuxName::Tmux { 500 } else { 300 });
    assert!(deadline >= before + dwell && deadline <= after + dwell);
    (rig, deadline)
}

fn assert_focus_marker_boundary(mux: MuxName) {
    let (mut rig, deadline) = offscreen_focus_marker(mux);
    rig.state
        .draw_frame(
            &mut rig.terminal,
            deadline - Duration::from_millis(1),
            false,
        )
        .unwrap();
    assert!(
        rig.state
            .pane
            .ui
            .interactions
            .line_for_target(&render::HitTarget::FocusMarker)
            .is_none()
    );
    let rows = rig.state.pane.ui.painted_rows.clone();
    let anchors = rig.view().ui().anchored_rows.clone();
    let offset = rig.view().ui().scroll_offset;
    rig.state
        .draw_frame(&mut rig.terminal, deadline, false)
        .unwrap();
    assert!(
        rig.state
            .pane
            .ui
            .interactions
            .line_for_target(&render::HitTarget::FocusMarker)
            .is_some()
    );
    assert_eq!(rig.state.pane.ui.painted_rows, rows);
    assert_eq!(rig.view().ui().anchored_rows, anchors);
    assert_eq!(rig.view().ui().scroll_offset, offset);
}

#[test]
fn tmux_focus_marker_appears_exactly_at_500ms() {
    assert_focus_marker_boundary(MuxName::Tmux);
}

#[test]
fn zellij_focus_marker_appears_exactly_at_300ms() {
    assert_focus_marker_boundary(MuxName::Zellij);
}

#[test]
fn a_second_focus_move_rearms_the_marker_dwell() {
    let (mut rig, previous) = offscreen_focus_marker(MuxName::Zellij);
    let mut focused = rig.state.views.mirror().clone();
    focused.focused_pane = focused.worktree_groups[0].rows[18]
        .pane
        .as_ref()
        .map(|pane| pane.pane_id.clone());
    rig.fold(focused, SnapshotSource::Produced);
    let Some(FocusMarker::Dwelling(deadline)) = rig.view().ctx().focus_marker else {
        panic!("rearmed dwell")
    };
    assert!(deadline > previous);
    rig.state
        .draw_frame(&mut rig.terminal, previous, false)
        .unwrap();
    assert!(
        rig.state
            .pane
            .ui
            .interactions
            .line_for_target(&render::HitTarget::FocusMarker)
            .is_none()
    );
}

#[test]
fn marker_deadline_wakes_and_paints_an_otherwise_idle_sidebar() {
    let (mut rig, _) = scroll_rig();
    rig.view().ui().selected_index = Some(19);
    {
        let pane = &mut rig.state.pane;
        let mut guard = rig
            .state
            .views
            .rig(pane.config.own_pane.as_ref(), pane.clock.now());
        let (ui, context) = guard.pair();
        ui.selected_pane = context.current.worktree_groups[0].rows[19]
            .pane
            .as_ref()
            .map(|pane| pane.pane_id.clone());
    }
    rig.view().ui().freeze = None;
    rig.state.pane.dirty = false;
    rig.view().ctx().focus_marker = Some(FocusMarker::Dwelling(
        Instant::now() + Duration::from_millis(50),
    ));
    let (active, timeout) = rig.state.frame_timing();
    assert!(!active);
    assert!(
        timeout <= Duration::from_millis(50),
        "the dwell caps the idle timeout"
    );
    rig.view().ctx().focus_marker = Some(FocusMarker::Dwelling(Instant::now()));
    rig.state.pane.next_frame = Instant::now() + Duration::from_secs(2);
    let (active, _) = rig.state.frame_timing();
    assert!(active, "the elapsed dwell itself makes the frame due");
    rig.paint(active);
    assert!(
        rig.state
            .pane
            .ui
            .interactions
            .line_for_target(&render::HitTarget::FocusMarker)
            .is_some()
    );
}

#[test]
fn marker_click_reveals_without_changing_selection_and_then_disappears() {
    let (mut rig, deadline) = offscreen_focus_marker(MuxName::Zellij);
    rig.state
        .draw_frame(&mut rig.terminal, deadline, false)
        .unwrap();
    let (column, row) = rig
        .state
        .pane
        .ui
        .interactions
        .line_for_target(&render::HitTarget::FocusMarker)
        .unwrap();
    let selected = rig.view().ui().selected_pane.clone();
    let applied = rig
        .state
        .apply_input([Wakeup::MouseClick { column, row }], &mut rig.terminal)
        .unwrap();
    assert!(applied.effects.is_empty());
    assert_eq!(rig.view().ui().selected_pane, selected);
    assert!(rig.view().ui().scroll_offset > 0);
    assert!(
        rig.state
            .pane
            .ui
            .painted_rows
            .iter()
            .any(|(id, _)| id == "agent-19")
    );
    assert!(
        rig.state
            .pane
            .ui
            .interactions
            .line_for_target(&render::HitTarget::FocusMarker)
            .is_none()
    );
    assert!(rig.view().ctx().focus_marker.is_none());
}

#[test]
fn hidden_paints_hide_the_marker_and_own_selection_ends_it() {
    let (mut rig, deadline) = offscreen_focus_marker(MuxName::Zellij);
    rig.state
        .draw_frame(&mut rig.terminal, deadline, false)
        .unwrap();
    assert!(
        rig.state
            .pane
            .ui
            .interactions
            .line_for_target(&render::HitTarget::FocusMarker)
            .is_some()
    );
    rig.view().ctx().current.own_view = Some(own_view());
    rig.view().ctx().current.viewed_panes.clear();
    rig.set_plane_viewed(false);
    assert!(!rig.state.watched());
    rig.state
        .draw_frame(&mut rig.terminal, deadline, false)
        .unwrap();
    assert!(
        rig.state
            .pane
            .ui
            .interactions
            .line_for_target(&render::HitTarget::FocusMarker)
            .is_none()
    );
    rig.state
        .apply_input([Wakeup::Key(KeyAction::Top)], &mut rig.terminal)
        .unwrap();
    assert!(rig.view().ctx().focus_marker.is_none());
}

#[test]
fn tmux_switch_from_sidebar_cancels_optimistic_watching() {
    let (mut source, mut destination, _) = pane_pair(MuxName::Tmux);
    let own = destination.state.pane.config.own_pane.clone().unwrap();
    // The plane samples one client, on the sidebar.
    let mut sampled = destination.state.views.mirror().clone();
    sampled.client_views = vec![crate::pane::ClientPaneView {
        client_id: crate::ids::MuxClientId::Tmux("client-1".into()),
        pane_id: own.clone(),
    }];
    destination.fold(sampled, SnapshotSource::Produced);
    destination.event(SidebarEvent::FocusChanged {
        focused: vec![own],
        unfocused: vec![],
    });
    assert!(destination.state.pane.optimistic_watch_until.is_some());
    destination.event(SidebarEvent::FocusChanged {
        focused: source
            .view()
            .ctx()
            .current
            .focused_pane
            .clone()
            .into_iter()
            .collect(),
        unfocused: vec![],
    });
    assert!(
        destination.state.pane.optimistic_watch_until.is_none(),
        "a single client's switch supersedes optimistic resize/focus watching"
    );
    assert!(!destination.state.watched());
}

#[test]
fn source_handles_cross_tab_click_without_holding_content() {
    for mux in [MuxName::Tmux, MuxName::Zellij] {
        source_click(mux, true);
    }
}

#[test]
fn source_handles_same_tab_click_without_holding_content() {
    source_click(MuxName::Zellij, false);
}

fn source_click(mux: MuxName, cross_tab: bool) {
    let (mut source, _, _) = pane_pair(mux);
    let output = FrameOutput::default();
    source.terminal.backend_mut().play_output(output.clone());
    source.state.pane.clock.skip_focus = true;
    let peers = source.focus_plane.focus_peers(
        source.state.pane.config.instance_id.clone(),
        crate::diag::DiagSink::disabled(),
    );
    source.state.pane.focus_peers = Some(peers.clone());
    let old_target = pane("terminal_19", "tab_1", false).pane_id;
    let old = peers.arm(old_target.clone(), Instant::now());
    assert!(old.is_current());
    let index = usize::from(cross_tab);
    let (row_id, row) = source.state.pane.ui.painted_rows[index].clone();
    let target = source
        .view()
        .ctx()
        .current
        .rows()
        .find(|row| row.id == row_id)
        .unwrap()
        .pane
        .as_ref()
        .unwrap()
        .pane_id
        .clone();
    let (input_tx, input_rx) = std::sync::mpsc::sync_channel(64);
    let mut inbox = queued([Wakeup::Snapshot]).with_input(input_rx);
    input_tx
        .send(Wakeup::MouseClick {
            column: 2,
            row: row as u16,
        })
        .unwrap();
    source.send(FetchUpdate::Snapshot {
        snapshot: Box::new(source.state.views.mirror().clone()),
        phase: FetchPhase::Final,
        source: SnapshotSource::Produced,
    });
    let applications = source.state.pane.content_applications;
    let paints = source.state.pane.paints;
    source
        .state
        .advance(&mut inbox, &mut source.fetch, &mut source.terminal)
        .unwrap();
    assert!(
        source.state.pane.paints > paints,
        "the source selection paints synchronously: {mux:?}"
    );
    assert!(
        !output.0.lock().unwrap().is_empty(),
        "source paint reaches its output: {mux:?}"
    );
    assert_eq!(
        source.view().ctx().pending_input_focus,
        Some(target),
        "the source must handle the new click before dispatch"
    );
    assert_eq!(
        source
            .state
            .pane
            .clock
            .focus_calls
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    let (_, _, mut anchor) = hidden_destination();
    anchor.pane_id = old_target;
    assert!(
        old.dispatch(anchor, old.clicked().unwrap(), || panic!(
            "older jump must not dispatch"
        ))
        .is_none()
    );
    assert_eq!(
        source.state.pane.content_applications,
        applications + 1,
        "input flushes content even during an active jump"
    );
}

#[test]
fn same_tab_focus_supersedes_pending_cross_tab_push() {
    for keyboard in [false, true] {
        let (mut source, destination, _) = pane_pair(MuxName::Zellij);
        let diag = crate::diag::DiagSink::under(
            source._dir.path().to_owned(),
            source.ws.clone(),
            &source.state.pane.config.session_name,
            Some(source.state.pane.config.instance_id.clone()),
        );
        let peers = source
            .focus_plane
            .focus_peers(source.state.pane.config.instance_id.clone(), diag);
        source.state.pane.focus_peers = Some(peers.clone());
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        let _subscription = source
            .focus_plane
            .subscribe(&destination.state.pane.config, tx);
        let destination_peers = source.focus_plane.focus_peers(
            destination.state.pane.config.instance_id.clone(),
            crate::diag::DiagSink::disabled(),
        );
        let (_, row) = source.state.pane.ui.painted_rows[1].clone();
        source
            .state
            .apply_input(
                [Wakeup::MouseClick {
                    column: 2,
                    row: row as u16,
                }],
                &mut source.terminal,
            )
            .unwrap();
        let first = source.state.pane.input_jump.take().unwrap();
        let (_, _, mut anchor) = hidden_destination();
        anchor.pane_id = source.view().ctx().pending_input_focus.clone().unwrap();
        let (_, _, _, ack, _) = first
            .push(&anchor, first.clicked().unwrap(), Instant::now)
            .unwrap();
        rx.try_iter().for_each(drop);
        assert!(first.is_current());
        let (_, row) = source.state.pane.ui.painted_rows[0].clone();
        let input = if keyboard {
            source.view().ui().selected_index = Some(0);
            source.view().ui().selected_pane = Some(pane("terminal_0", "tab_0", false).pane_id);
            Wakeup::Key(KeyAction::Enter)
        } else {
            Wakeup::MouseClick {
                column: 2,
                row: row as u16,
            }
        };
        let applied = source
            .state
            .apply_input([input], &mut source.terminal)
            .unwrap();
        let target = pane("terminal_0", "tab_0", false).pane_id;
        assert!(
            applied
                .effects
                .contains(&InputEffect::Focus(target.clone()))
        );
        assert_eq!(source.view().ctx().pending_input_focus, Some(target));
        assert!(
            source.state.pane.input_jump.is_none(),
            "same-tab focus arms no push"
        );
        assert!(
            !first.is_current(),
            "same-tab focus must revoke the older lease"
        );
        assert!(destination_peers.take_push().is_none());
        let report = ack
            .try_iter()
            .find(|report| report.instance_id == destination.state.pane.config.instance_id)
            .unwrap();
        assert_eq!(report.verdict, Some(PushVerdict::Superseded));
        assert!(
            matches!(rx.try_recv(), Ok(Wakeup::Tick)),
            "release wakes peers"
        );
        assert!(first.request(|| panic!("obsolete request")).is_none());
        assert!(
            first
                .dispatch(anchor, first.clicked().unwrap(), || panic!(
                    "obsolete dispatch"
                ))
                .is_none()
        );
        let log = std::fs::read_to_string(crate::diag::focus_trace::log_path(source._dir.path()))
            .unwrap();
        let records: Vec<serde_json::Value> = log
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let stopped = records
            .iter()
            .find(|row| row["event"]["stage"] == "input_superseded")
            .unwrap();
        assert_eq!(stopped["event"]["sequence"], 1);
        assert_eq!(
            stopped["event"]["target"],
            source.view().ctx().current.worktree_groups[0].rows[1]
                .pane
                .as_ref()
                .unwrap()
                .pane_id
                .to_string()
        );
        let _next = peers.arm(pane("terminal_19", "tab_1", false).pane_id, Instant::now());
        let log = std::fs::read_to_string(crate::diag::focus_trace::log_path(source._dir.path()))
            .unwrap();
        let last: serde_json::Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
        assert_eq!(
            last["event"]["sequence"], 3,
            "same-tab focus advances click order"
        );
    }
}

#[test]
fn jump_target_paints_while_hidden_non_owner_handles_room_work() {
    let (mut target, _, _) = pane_pair(MuxName::Zellij);
    target.view().ctx().current.pane_session_name = Some("rimz-test".into());
    let peers = target
        .focus_plane
        .focus_peers(SidebarInstanceId::new(), crate::diag::DiagSink::disabled());
    let start = std::sync::Barrier::new(2);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let start = &start;
        let work = scope.spawn(move || {
            let (_, mut hidden, output) = pane_pair(MuxName::Zellij);
            hidden.state.pane.focus_peers = Some(peers);
            hidden.view().ctx().filter_observed_at = None;
            hidden.state.pane.titled = true;
            let event = SidebarEventEnvelope::new(
                hidden.ws.clone(),
                Some("rimz-test".into()),
                crate::utils::time::unix_now_ms(),
                SidebarEvent::BodyFilterChanged,
            );
            let mut inbox = queued([Wakeup::Event(event), Wakeup::Snapshot]);
            hidden.send(FetchUpdate::Snapshot {
                snapshot: Box::new(hidden.state.views.mirror().clone()),
                phase: FetchPhase::Final,
                source: SnapshotSource::Produced,
            });
            let before = (
                hidden.state.pane.projections,
                hidden.state.pane.content_applications,
            );
            ready_tx.send(()).unwrap();
            start.wait();
            assert!(
                hidden
                    .state
                    .known_non_owner(&pane("terminal_0", "tab_0", false).pane_id)
            );
            assert!(
                !hidden
                    .state
                    .pane
                    .focus_peers
                    .as_ref()
                    .unwrap()
                    .accepts(None, hidden.state.pane.clock.now())
            );
            hidden
                .state
                .advance(&mut inbox, &mut hidden.fetch, &mut hidden.terminal)
                .unwrap();
            assert!(
                hidden.view().ctx().filter_observed_at.is_some(),
                "an active jump must not hold ordinary room work"
            );
            assert_eq!(
                (
                    hidden.state.pane.projections,
                    hidden.state.pane.content_applications
                ),
                before
            );
            assert_eq!(paints(&output), 0, "a hidden non-owner still never paints");
        });
        ready_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let (_, _, mut anchor) = hidden_destination();
        anchor.pane_id = pane("terminal_0", "tab_0", false).pane_id;
        anchor.placement = Some(crate::mux::focus_anchor::FocusPlacement {
            row_id: "agent-0".into(),
            viewport_row: 0,
            held: Default::default(),
        });
        anchor.order = None;
        let (push, ack) = focus_push(&mut target, anchor);
        start.wait();
        target
            .state
            .on_focus_push(push, &mut target.fetch, &mut target.terminal)
            .unwrap();
        work.join().unwrap();
        let report = assert_push_report(&ack, &target, PushVerdict::Painted, true);
        assert!(
            report.finished_ms.unwrap() < 500,
            "target paint must finish inside PAINT_WAIT"
        );
    });
}
#[test]
fn source_click_arms_before_paint_and_release_wakes_each_peer_once() {
    let (mut source, destination, _) = pane_pair(MuxName::Zellij);
    let plane = super::super::super::plane::DataPlane::detached();
    let (source_tx, source_rx) = std::sync::mpsc::sync_channel(64);
    let (dest_tx, dest_rx) = std::sync::mpsc::sync_channel(64);
    let _source_subscription = plane.subscribe(&source.state.pane.config, source_tx);
    let _destination_subscription = plane.subscribe(&destination.state.pane.config, dest_tx);
    let peers = plane.focus_peers(
        source.state.pane.config.instance_id.clone(),
        crate::diag::DiagSink::disabled(),
    );
    source.state.pane.focus_peers = Some(peers.clone());
    let (_, row) = source.state.pane.ui.painted_rows[1].clone();
    source
        .state
        .apply_input(
            [Wakeup::MouseClick {
                column: 2,
                row: row as u16,
            }],
            &mut source.terminal,
        )
        .unwrap();
    assert!(
        source.state.pane.input_jump.as_ref().unwrap().is_current(),
        "lease must precede click paint"
    );
    assert!(
        dest_rx.try_recv().is_err(),
        "click paint must not wake peers"
    );
    drop(source.state.pane.input_jump.take());
    assert_eq!(source_rx.try_recv().unwrap(), Wakeup::Tick);
    assert_eq!(dest_rx.try_recv().unwrap(), Wakeup::Tick);
    assert!(source_rx.try_recv().is_err());
    assert!(dest_rx.try_recv().is_err());
}

#[test]
fn queued_snapshot_tick_does_not_paint_ahead_of_a_focus_push() {
    let (mut source, mut destination, output) = pane_pair(MuxName::Tmux);
    paint_scrolled(&mut source, 12);
    let target = PaneId::from_parts(MuxName::Tmux, "%8");
    let mut pulled = destination.state.views.mirror().clone();
    pulled.own_view.as_mut().unwrap().working_pane_ids = vec![target.clone()];
    pulled.pane_session_name = Some("rimz-test".into());
    destination.fold(pulled, SnapshotSource::Produced);
    destination.state.pane.titled = true;
    let anchor = crate::mux::focus_anchor::FocusAnchor {
        nonce: crate::ids::FocusNonce::new(),
        session_name: "rimz-test".into(),
        pane_id: target,
        origin: crate::mux::focus_anchor::FocusOrigin::User,
        repair_generation: None,
        issued_at_ms: crate::utils::time::unix_now_ms(),
        applied_at_ms: None,
        state: crate::mux::focus_anchor::FocusIntentState::Requested,
        pre_action: Vec::new(),
        placement: Some(crate::mux::focus_anchor::FocusPlacement {
            row_id: "agent-8".into(),
            viewport_row: 2,
            held: Default::default(),
        }),
        order: None,
    };
    crate::mux::focus_anchor::store(&destination.runtime, &anchor).unwrap();
    let event = crate::wakeup::events::SidebarEventEnvelope::new(
        destination.ws.clone(),
        Some("rimz-test".into()),
        crate::utils::time::unix_now_ms(),
        SidebarEvent::BodyFilterChanged,
    );
    let plane = super::super::super::plane::DataPlane::detached();
    let (tx, rx) = std::sync::mpsc::sync_channel(3);
    for wakeup in [Wakeup::Snapshot, Wakeup::Event(event), Wakeup::Tick] {
        tx.send(wakeup).unwrap();
    }
    let _subscription = plane.subscribe(&destination.state.pane.config, tx);
    destination.state.pane.focus_peers = Some(plane.focus_peers(
        destination.state.pane.config.instance_id.clone(),
        crate::diag::DiagSink::disabled(),
    ));
    let clicked = Instant::now();
    let lease = plane
        .focus_peers(
            source.state.pane.config.instance_id.clone(),
            crate::diag::DiagSink::disabled(),
        )
        .arm(anchor.pane_id.clone(), clicked);
    let (_, _, _, ack, deliveries) = lease.push(&anchor, clicked, Instant::now).unwrap();
    assert_eq!(
        deliveries[0].delivery,
        PushDelivery::Sent,
        "a full ordinary inbox cannot lose the priority push"
    );
    let mut inbox = WakeupInbox::new(rx);
    destination.state.pane.next_frame = Instant::now();
    destination
        .state
        .step(
            &mut inbox,
            &mut destination.fetch,
            &mut destination.terminal,
        )
        .unwrap();
    assert_push_report(&ack, &destination, PushVerdict::Painted, true);
    assert_eq!(paints(&output), 1, "only the pushed placement paints");
    {
        let pane = &mut destination.state.pane;
        assert!(
            pane.ui
                .painted_rows
                .contains(&("agent-8".into(), pane.ui.scroll_zone_top as isize + 2))
        );
    }
}

#[test]
fn existing_attachment_holds_selection_until_a_new_window_has_a_row() {
    let (mut source, mut destination, _) = pane_pair(MuxName::Tmux);
    let previous = destination
        .view()
        .ctx()
        .current
        .own_view
        .as_ref()
        .unwrap()
        .working_pane_ids[0]
        .clone();
    source.event(SidebarEvent::FocusChanged {
        focused: vec![previous.clone()],
        unfocused: vec![],
    });
    paint_scrolled(&mut source, 12);
    destination.maintenance();
    destination.event(SidebarEvent::FocusChanged {
        focused: vec![previous.clone()],
        unfocused: vec![],
    });
    paint_at(&mut destination, 0);
    destination.event(SidebarEvent::FocusChanged {
        focused: vec![PaneId::from_parts(MuxName::Tmux, "%20")],
        unfocused: vec![],
    });
    paint_at(&mut destination, 0);
    assert_eq!(
        session_focus_baseline(
            destination.state.views.mirror(),
            destination.state.pane.config.own_pane.as_ref()
        ),
        None
    );
    assert_eq!(destination.view().ui().selected_pane, Some(previous));
    assert_eq!(destination.view().ui().selected_index, Some(19));
    let new_pane = PaneId::from_parts(MuxName::Tmux, "%20");
    let mut admitted = destination.state.views.mirror().clone();
    let rows = Arc::make_mut(&mut Arc::make_mut(&mut admitted.worktree_groups)[0].rows);
    let mut row = rows[19].clone();
    row.id = "agent-20".into();
    row.pane.as_mut().unwrap().pane_id = new_pane.clone();
    rows.push(row);
    admitted.own_view.as_mut().unwrap().working_pane_ids = vec![new_pane.clone()];
    destination.fold(admitted, SnapshotSource::Produced);
    paint_at(&mut destination, 0);
    assert_eq!(destination.view().ui().selected_pane, Some(new_pane));
}

fn move_cards_without_input(rig: &mut Rig, first: usize, second: usize, now: Instant) {
    rig.view().ui().freeze = None;
    Arc::make_mut(&mut Arc::make_mut(&mut rig.view().ctx().current.worktree_groups)[0].rows)
        .swap(first, second);
    rig.state.pane.dirty = true;
    rig.state.paint_now(&mut rig.terminal, now).unwrap();
}

#[test]
fn arrival_freezes_the_painted_order_not_a_hidden_fold() {
    let (mut rig, _) = scroll_rig();
    let painted = {
        let mut guard = rig.view();
        let (ui, context) = guard.pair();
        layout_freeze::capture_order(&context.current, ui)
    };
    let mut hidden = rig.state.views.mirror().clone();
    hidden.own_view = Some(own_view());
    hidden.viewed_panes.clear();
    hidden.focused_pane = None;
    rig.fold(hidden.clone(), SnapshotSource::Produced);
    rig.view().ui().freeze = None;
    // Still hidden: the pane-local copy lags a fold its pane did not paint.
    let mut reranked = hidden;
    Arc::make_mut(&mut Arc::make_mut(&mut reranked.worktree_groups)[0].rows)[1].attention_score =
        1000;
    Arc::make_mut(&mut Arc::make_mut(&mut reranked.worktree_groups)[0].rows)[1]
        .as_agent_mut()
        .unwrap()
        .status = crate::agents::AgentStatus::Waiting;
    rig.fold(reranked.clone(), SnapshotSource::Produced);
    rig.state.drain_snapshot(&mut rig.fetch, false);
    assert_eq!(
        rig.view().ctx().current.worktree_groups[0].rows[0].id,
        "agent-1"
    );
    let mut arrival = reranked;
    arrival.viewed_panes = own_view().working_pane_ids;
    rig.fold(arrival, SnapshotSource::Produced);
    rig.state.pane.next_frame = Instant::now();
    rig.paint(true);
    assert_eq!(rig.view().ui().last_order.rows, painted.rows);
    assert!(rig.view().ui().freeze.is_some());
}

#[test]
fn automatic_moves_guard_the_first_frame_until_400ms_and_input_paints_clear_it() {
    for (click_ms, input_paint, second_move, target) in [
        (399, false, false, 1),
        (400, false, false, 2),
        (100, true, false, 2),
        (399, false, true, 1),
        (400, false, true, 3),
    ] {
        let (mut rig, _) = scroll_rig();
        let clock = play_clock(&mut rig);
        let row = rig.state.pane.ui.interactions.line_for_row(1).unwrap() as u16;
        let now = clock.now();
        move_cards_without_input(&mut rig, 1, 2, now);
        if input_paint {
            clock.advance(Duration::from_millis(50));
            rig.state
                .apply_input([Wakeup::Scroll { down: false }], &mut rig.terminal)
                .unwrap();
        }
        if second_move {
            move_cards_without_input(&mut rig, 1, 3, now + Duration::from_millis(200));
        }
        clock.advance(now + Duration::from_millis(click_ms) - clock.now());
        let outcome = rig
            .state
            .apply_input([Wakeup::MouseClick { column: 1, row }], &mut rig.terminal)
            .unwrap();
        assert_eq!(
            outcome.effects,
            vec![InputEffect::Focus(
                pane(&format!("terminal_{target}"), "tab_0", false).pane_id
            )],
            "click={click_ms}, input={input_paint}, second move={second_move}",
        );
    }
}

#[test]
fn a_guarded_card_that_left_is_swallowed_without_selection_paint_or_rearm() {
    let (mut rig, output) = scroll_rig();
    let clock = play_clock(&mut rig);
    let row = rig.state.pane.ui.interactions.line_for_row(1).unwrap() as u16;
    Arc::make_mut(&mut Arc::make_mut(&mut rig.view().ctx().current.worktree_groups)[0].rows)
        .remove(1);
    rig.state.pane.dirty = true;
    rig.state.paint_now(&mut rig.terminal, clock.now()).unwrap();
    layout_freeze::arm_freeze(rig.view().ui(), 1);
    let selection = rig.view().ui().selected_pane.clone();
    let before = paints(&output);
    clock.advance(Duration::from_millis(399));
    let outcome = rig
        .state
        .apply_input([Wakeup::MouseClick { column: 1, row }], &mut rig.terminal)
        .unwrap();
    assert_eq!(
        outcome,
        InputOutcome::default(),
        "do not click the replacement card"
    );
    assert_eq!(rig.view().ui().selected_pane, selection);
    assert_eq!(paints(&output), before);
    assert_eq!(rig.view().ui().freeze.as_ref().unwrap().armed_ms, 1);
}

#[test]
fn resize_delivery_is_not_a_user_input_paint_for_the_click_guard() {
    let (mut rig, _) = scroll_rig();
    let clock = play_clock(&mut rig);
    let row = rig.state.pane.ui.interactions.line_for_row(1).unwrap() as u16;
    move_cards_without_input(&mut rig, 1, 2, clock.now());
    clock.advance(Duration::from_millis(100));
    rig.state
        .apply_input([Wakeup::Resize], &mut rig.terminal)
        .unwrap();
    clock.advance(Duration::from_millis(299));
    let outcome = rig
        .state
        .apply_input([Wakeup::MouseClick { column: 1, row }], &mut rig.terminal)
        .unwrap();
    assert_eq!(
        outcome.effects,
        vec![InputEffect::Focus(
            pane("terminal_1", "tab_0", false).pane_id
        )]
    );
}

#[test]
fn row_click_resolves_painted_identity_through_an_unpainted_fold() {
    let (mut rig, _) = scroll_rig();
    let row = rig.state.pane.ui.interactions.line_for_row(1).unwrap() as u16;
    Arc::make_mut(&mut Arc::make_mut(&mut rig.view().ctx().current.worktree_groups)[0].rows)
        .swap(1, 2);
    let outcome = rig
        .state
        .apply_input([Wakeup::MouseClick { column: 1, row }], &mut rig.terminal)
        .unwrap();
    assert_eq!(
        outcome.effects,
        vec![InputEffect::Focus(
            pane("terminal_1", "tab_0", false).pane_id
        )]
    );
}

fn dispatch(rig: &mut Rig, inbox: &mut WakeupInbox) {
    inbox
        .dispatch_next(
            &mut rig.state,
            &mut rig.fetch,
            &mut rig.terminal,
            Duration::ZERO,
        )
        .unwrap();
}

fn paints(output: &FrameOutput) -> usize {
    // Cell-only frames have no sync bracket. The backend flushes its output
    // once at frame completion, even when the ratatui diff is empty.
    output.1.load(std::sync::atomic::Ordering::SeqCst)
}

fn visible(rig: &Rig) -> usize {
    rig.state
        .inspect_view(|ui, _| ui.scroll_ease.visible(ui.scroll_offset, ui.frame_ms))
}

fn paint_at(rig: &mut Rig, elapsed_ms: u64) {
    rig.view().ctx().anim_start = Instant::now() - Duration::from_millis(elapsed_ms);
    rig.state.pane.next_frame = Instant::now();
    rig.paint(true);
}

fn hidden_destination() -> (Rig, FrameOutput, crate::mux::focus_anchor::FocusAnchor) {
    use crate::mux::focus_anchor::{FocusAnchor, FocusIntentState, FocusPlacement};
    let (mut rig, output) = scroll_rig();
    let target = rig.view().ctx().current.worktree_groups[0].rows[4].clone();
    let pane_id = target.pane.as_ref().unwrap().pane_id.clone();
    let mut pulled = rig.state.views.mirror().clone();
    pulled.own_view = Some(SidebarOwnView {
        sibling_count: 1,
        working_pane_ids: vec![pane_id.clone()],
        own_view_is_daemon: false,
    });
    pulled.focused_pane = Some(pane("terminal_0", "tab_0", false).pane_id);
    pulled.viewed_panes.clear();
    pulled.pane_session_name = Some("rimz-test".to_owned());
    rig.state.pane.optimistic_watch_until = None;
    rig.view().ui().scroll_offset = 30;
    rig.fold(pulled, SnapshotSource::Produced);
    assert!(!rig.state.watched());
    let anchor = FocusAnchor {
        nonce: crate::ids::FocusNonce::new(),
        session_name: "rimz-test".to_owned(),
        pane_id,
        origin: FocusOrigin::User,
        repair_generation: None,
        issued_at_ms: crate::utils::time::unix_now_ms(),
        applied_at_ms: None,
        state: FocusIntentState::Requested,
        pre_action: Vec::new(),
        placement: Some(FocusPlacement {
            row_id: target.id,
            viewport_row: 2,
            held: std::collections::BTreeSet::new(),
        }),
        order: Some(rig.view().ui().last_order.clone()),
    };
    (rig, output, anchor)
}

fn assert_external_selection_stays_offscreen(rowless_anchor: bool) {
    let (mut rig, _) = scroll_rig();
    let target = rig.view().ctx().current.worktree_groups[0].rows[19]
        .pane
        .as_ref()
        .unwrap()
        .pane_id
        .clone();
    if rowless_anchor {
        let (_, _, mut anchor) = hidden_destination();
        anchor.pane_id = target.clone();
        anchor.placement = None;
        anchor.order = None;
        crate::mux::focus_anchor::store(&rig.runtime, &anchor).unwrap();
    }
    let mut focused = rig.state.views.mirror().clone();
    focused.focused_pane = Some(target.clone());
    rig.fold(focused, SnapshotSource::Produced);
    paint_at(&mut rig, 0);
    assert_eq!(rig.view().ui().selected_pane, Some(target));
    assert_eq!(rig.view().ui().scroll_offset, 0);
    assert!(
        !rig.state
            .pane
            .ui
            .painted_rows
            .iter()
            .any(|(id, _)| id == "agent-19")
    );
}

#[test]
fn native_focus_never_reveals_an_offscreen_selection() {
    assert_external_selection_stays_offscreen(false);
}

#[test]
fn rowless_focus_anchor_never_reveals_an_offscreen_selection() {
    assert_external_selection_stays_offscreen(true);
}

#[test]
fn own_key_browse_reveals_an_offscreen_selection() {
    let (mut rig, _) = scroll_rig();
    rig.input(KeyAction::Bottom);
    assert!(rig.view().ui().scroll_offset > 0);
    assert!(
        rig.state
            .pane
            .ui
            .painted_rows
            .iter()
            .any(|(id, _)| id == "agent-19")
    );
}

#[test]
fn own_inbox_jump_reveals_when_its_offscreen_target_lands() {
    let (mut rig, _) = scroll_rig();
    Arc::make_mut(&mut Arc::make_mut(&mut rig.view().ctx().current.worktree_groups)[0].rows)[19]
        .as_agent_mut()
        .unwrap()
        .status = crate::agents::AgentStatus::Waiting;
    let target = rig.view().ctx().current.worktree_groups[0].rows[19]
        .pane
        .as_ref()
        .unwrap()
        .pane_id
        .clone();
    let outcome = rig
        .state
        .apply_input([Wakeup::Key(KeyAction::InboxNext)], &mut rig.terminal)
        .unwrap();
    assert_eq!(outcome.effects, vec![InputEffect::Focus(target.clone())]);
    assert_eq!(rig.view().ui().selected_pane, None);
    let mut focused = rig.state.views.mirror().clone();
    focused.focused_pane = Some(target.clone());
    rig.fold(focused, SnapshotSource::Produced);
    paint_at(&mut rig, 0);
    assert_eq!(rig.view().ui().selected_pane, Some(target));
    assert!(rig.view().ui().scroll_offset > 0);
    assert!(
        rig.state
            .pane
            .ui
            .painted_rows
            .iter()
            .any(|(id, _)| id == "agent-19")
    );
}

#[test]
fn shared_placement_captures_the_painted_opening_set() {
    let (mut rig, _) = scroll_rig();
    let target = rig.view().ctx().current.worktree_groups[0].rows[1]
        .pane
        .as_ref()
        .unwrap()
        .pane_id
        .clone();
    rig.view().ui().selected_index = Some(0);
    {
        let pane = &mut rig.state.pane;
        let mut guard = rig
            .state
            .views
            .rig(pane.config.own_pane.as_ref(), pane.clock.now());
        let (ui, context) = guard.pair();
        ui.selected_pane = context.current.worktree_groups[0].rows[0]
            .pane
            .as_ref()
            .map(|pane| pane.pane_id.clone());
    }
    rig.state.pane.dirty = true;
    paint_at(&mut rig, 0);
    let row = rig.state.pane.ui.interactions.line_for_row(1).unwrap() as u16;
    rig.state
        .apply_input([Wakeup::MouseClick { column: 1, row }], &mut rig.terminal)
        .unwrap();
    assert_eq!(rig.view().ui().drawn_selection.as_deref(), Some("agent-1"));
    assert!(
        rig.view().ui().held_expansions.contains("agent-0"),
        "the click paint already retains A under its freeze"
    );
    let placement = rig.state.focus_placement(&target).expect("visible target");
    let mut expected = rig.view().ui().held_expansions.clone();
    expected.insert("agent-1".into());
    assert_eq!(placement.held, expected);
}

fn assert_shared_placement_from_history(previous: usize) {
    let (mut source, _) = scroll_rig();
    let first = source.view().ctx().current.worktree_groups[0].rows[0]
        .pane
        .as_ref()
        .unwrap()
        .pane_id
        .clone();
    source.view().ui().selected_index = Some(0);
    source.view().ui().selected_pane = Some(first.clone());
    source.state.pane.dirty = true;
    paint_at(&mut source, 0);
    let target = source.view().ctx().current.worktree_groups[0].rows[1].clone();
    let target_pane = target.pane.as_ref().unwrap().pane_id.clone();
    let row = source.state.pane.ui.interactions.line_for_row(1).unwrap() as u16;
    source
        .state
        .apply_input(
            [Wakeup::MouseClick { column: 1, row }],
            &mut source.terminal,
        )
        .unwrap();
    assert_eq!(
        source.view().ui().drawn_selection.as_deref(),
        Some("agent-1")
    );
    let placement = source
        .state
        .focus_placement(&target_pane)
        .expect("source painted target");
    let source_y = source
        .state
        .pane
        .ui
        .painted_rows
        .iter()
        .find(|(id, _)| id == &target.id)
        .unwrap()
        .1;

    let (mut destination, _, mut anchor) = hidden_destination();
    destination.view().ctx().current = source.state.views.mirror().clone();
    destination.view().ui().selected_index = Some(previous);
    {
        let mut guard = destination.view();
        let (ui, context) = guard.pair();
        ui.selected_pane = Some(
            context.current.worktree_groups[0].rows[previous]
                .pane
                .as_ref()
                .unwrap()
                .pane_id
                .clone(),
        );
    }
    destination.state.pane.dirty = true;
    destination
        .state
        .paint_now(&mut destination.terminal, Instant::now())
        .unwrap();
    // The target's pane now shares the hidden destination's tab; the jump folds that sample.
    let mut moved = destination.state.views.mirror().clone();
    moved.own_view = Some(SidebarOwnView {
        sibling_count: 1,
        working_pane_ids: vec![target_pane.clone()],
        own_view_is_daemon: false,
    });
    moved.focused_pane = Some(first);
    moved.viewed_panes.clear();
    moved.pane_session_name = Some("rimz-test".to_owned());
    destination.fold(moved, SnapshotSource::Produced);
    destination.state.pane.optimistic_watch_until = None;
    anchor.pane_id = target_pane.clone();
    anchor.placement = Some(placement);
    anchor.order = Some(source.view().ui().last_order.clone());
    let (push, ack) = focus_push(&mut destination, anchor);
    destination
        .state
        .on_focus_push(push, &mut destination.fetch, &mut destination.terminal)
        .unwrap();

    assert_eq!(destination.view().ui().selected_pane, Some(target_pane));
    assert!(destination.view().ui().focus_placement.is_none());
    assert_eq!(
        destination
            .state
            .pane
            .ui
            .painted_rows
            .iter()
            .find(|(id, _)| id == &target.id)
            .unwrap()
            .1,
        source_y
    );
    assert_push_report(&ack, &destination, PushVerdict::Painted, true);
    for (id, y) in source
        .state
        .pane
        .ui
        .painted_rows
        .iter()
        .filter(|(_, y)| *y <= source_y)
    {
        assert_eq!(
            destination
                .state
                .pane
                .ui
                .painted_rows
                .iter()
                .find(|(painted, _)| painted == id)
                .map(|(_, row)| row),
            Some(y),
            "landing preserves the post-click rows above and including B"
        );
    }
}

#[test]
fn shared_placement_lands_after_the_destination_previously_selected_the_target() {
    assert_shared_placement_from_history(1);
}

#[test]
fn shared_placement_lands_after_the_destination_previously_selected_the_source() {
    assert_shared_placement_from_history(0);
}

#[test]
fn pushed_hidden_destination_paints_placement_without_disk_and_keeps_it_after_switch() {
    let (mut rig, output, anchor) = hidden_destination();
    let target = anchor.pane_id.clone();
    let placement = anchor.placement.clone().unwrap();
    let (push, ack) = focus_push(&mut rig, anchor.clone());
    assert!(!rig.runtime.focus_anchor_path().exists());
    rig.state
        .on_focus_push(push, &mut rig.fetch, &mut rig.terminal)
        .unwrap();
    assert_eq!(rig.view().ui().selected_pane, Some(target.clone()));
    assert!(
        rig.state.watched(),
        "the overlay must open the existing paint gate"
    );
    assert_eq!(
        paints(&output),
        1,
        "the push paints inside on_focus_push, before maintenance"
    );
    assert_push_report(&ack, &rig, PushVerdict::Painted, true);
    rig.paint(true);
    assert_eq!(
        paints(&output),
        1,
        "the later frame phase must not paint twice"
    );
    let y = |rig: &Rig| {
        rig.state
            .pane
            .ui
            .painted_rows
            .iter()
            .find(|(id, _)| id == &placement.row_id)
            .unwrap()
            .1
    };
    assert_eq!(
        y(&rig),
        rig.state.pane.ui.scroll_zone_top as isize + placement.viewport_row
    );
    assert_eq!(rig.view().ui().last_focus_anchor_ms, anchor.issued_at_ms);
    let old_y = y(&rig);
    let mut switched = rig.state.views.mirror().clone();
    switched.focused_pane = Some(target.clone());
    switched.viewed_panes = vec![target];
    rig.fold(switched, SnapshotSource::Produced);
    assert!(rig.view().ui().focus_placement.is_none());
    paint_at(&mut rig, 100);
    assert_eq!(
        y(&rig),
        old_y,
        "the post-switch fold must not reveal the card again"
    );
}

#[test]
fn destination_ack_waits_through_a_paint_hold_and_releases_on_exit() {
    for exit in [false, true] {
        let (mut rig, output, anchor) = hidden_destination();
        let (push, ack) = focus_push(&mut rig, anchor);
        rig.state
            .pane
            .paint_hold
            .engage(Instant::now(), crate::utils::time::unix_now_ms());
        rig.state
            .on_focus_push(push, &mut rig.fetch, &mut rig.terminal)
            .unwrap();
        rig.paint(true);
        assert_eq!(paints(&output), 0);
        assert_eq!(
            ack.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty),
            "a blocked destination keeps its acknowledgement"
        );
        rig.state.pane.paint_hold.release();
        rig.state.pane.should_exit = exit;
        rig.paint(true);
        if exit {
            assert_eq!(paints(&output), 0);
            assert_push_report(&ack, &rig, PushVerdict::Exiting, true);
        } else {
            assert_eq!(paints(&output), 1);
            let report = assert_push_report(&ack, &rig, PushVerdict::Painted, true);
            assert_eq!(report.blocked_by, Some(PushBlock::PaintHold));
        }
    }
}

#[test]
fn a_late_duplicate_push_uses_the_painted_frame_unless_the_destination_is_exiting() {
    for exit in [false, true] {
        let (mut rig, output, anchor) = hidden_destination();
        let (push, ack) = focus_push(&mut rig, anchor.clone());
        rig.state
            .on_focus_push(push, &mut rig.fetch, &mut rig.terminal)
            .unwrap();
        rig.paint(true);
        assert_push_report(&ack, &rig, PushVerdict::Painted, true);
        rig.state.pane.should_exit = exit;
        let (push, ack) = focus_push(&mut rig, anchor);
        rig.state
            .on_focus_push(push, &mut rig.fetch, &mut rig.terminal)
            .unwrap();
        if exit {
            assert_push_report(&ack, &rig, PushVerdict::Exiting, false);
        } else {
            assert_push_report(&ack, &rig, PushVerdict::Painted, false);
        }
        assert_eq!(paints(&output), 1, "a duplicate push must not paint again");
    }
}

#[test]
fn cleared_anchor_without_push_keeps_the_viewport_on_the_first_target_fold() {
    let (mut rig, _, anchor) = hidden_destination();
    crate::mux::focus_anchor::store(&rig.runtime, &anchor).unwrap();
    assert!(crate::mux::focus_anchor::clear_matching(
        &rig.runtime,
        anchor.nonce
    ));
    let mut switched = rig.state.views.mirror().clone();
    switched.focused_pane = Some(anchor.pane_id.clone());
    switched.viewed_panes = vec![anchor.pane_id.clone()];
    rig.fold(switched, SnapshotSource::Produced);
    assert_eq!(rig.view().ui().selected_pane, Some(anchor.pane_id));
    assert!(!rig.view().ui().own_input_reveal);
    assert!(rig.view().ui().focus_placement.is_none());
    assert_eq!(rig.view().ui().last_focus_anchor_ms, 0);
}

fn fitted_grow_hold(resize_first: bool) {
    for (rows, cols, own_pane, holds) in [
        (30, None, true, true),
        (30, Some(79), true, true),
        (31, Some(80), true, true),
        (30, Some(80), false, true),
        (30, Some(80), true, false),
    ] {
        let (mut rig, _, anchor) = hidden_destination();
        let mut pty = crate::sidebar_pane::app::backend::tests::Pty::open(40, 24);
        rig.terminal = Terminal::new(PaneBackend::for_fd(pty.slave()).unwrap()).unwrap();
        rig.state.pane.prev_width = Some(40);
        rig.state.pane.self_close.seen_sibling = true;
        let (mut push, ack) = focus_push(&mut rig, anchor);
        let pane = if own_pane {
            rig.state.pane.config.own_pane.clone().unwrap()
        } else {
            pane("terminal_100", "tab_0", false).pane_id
        };
        push.expected_panes = Some(Arc::new(std::collections::HashMap::from([(
            pane,
            crate::mux::FittedPane { rows, cols },
        )])));
        if resize_first {
            pty.resize(80, 30);
            rig.state
                .on_resize(&mut rig.fetch, &mut rig.terminal, Some(80))
                .unwrap();
            assert!(
                rig.state.pane.paint_hold.is_engaged(),
                "without a push the stale legit bound holds the grow"
            );
        }
        rig.state
            .on_focus_push(push, &mut rig.fetch, &mut rig.terminal)
            .unwrap();
        let painted_before_resize = !resize_first && !own_pane;
        if painted_before_resize {
            assert_push_report(&ack, &rig, PushVerdict::Painted, true);
        }
        if !resize_first {
            pty.resize(80, 30);
            rig.state
                .on_resize(&mut rig.fetch, &mut rig.terminal, Some(80))
                .unwrap();
        }
        assert_eq!(
            rig.state.pane.paint_hold.is_engaged(),
            holds,
            "resize_first={resize_first}, rows={rows}, cols={cols:?}, own_pane={own_pane}: only the exact fitted own-pane size bypasses the hold"
        );
        if holds && !painted_before_resize {
            assert_eq!(ack.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
        }
        if holds {
            rig.state.pane.paint_hold.release();
        }
        rig.state.pane.dirty = true;
        rig.state.pane.next_frame = Instant::now();
        pty.drain();
        rig.paint(true);
        if rows == 30 && !painted_before_resize {
            assert_push_report(&ack, &rig, PushVerdict::Painted, true);
        }
    }
}

#[test]
fn fitted_push_before_resize_skips_only_the_exact_size_hold() {
    fitted_grow_hold(false);
}

#[test]
fn fitted_push_after_resize_releases_only_the_exact_size_hold() {
    fitted_grow_hold(true);
}

#[test]
fn displaced_placement_reports_the_landing_in_both_ack_paths() {
    for cached in [false, true] {
        let (mut rig, _, mut anchor) = hidden_destination();
        rig.state.pane.clock = LoopClock::play(jiff::Timestamp::now());
        anchor.placement.as_mut().unwrap().viewport_row = 1_000;
        if cached {
            // The intent revealed this tab, so its fold and paint land in the routed view.
            rig.state.pane.optimistic_watch_until =
                Some(rig.state.pane.clock.now() + FOCUS_RESUME_WATCH_WINDOW);
            rig.state.fold_focus_intent(anchor.clone());
            rig.state
                .paint_now(&mut rig.terminal, rig.state.pane.clock.now())
                .unwrap();
            rig.state.pane.clock.advance(Duration::from_millis(17));
        }
        let (push, ack) = focus_push(&mut rig, anchor.clone());
        if !cached {
            rig.state.pane.clock.advance(Duration::from_millis(23));
        }
        rig.state
            .on_focus_push(push, &mut rig.fetch, &mut rig.terminal)
            .unwrap();

        assert_eq!(rig.view().ui().selected_pane, Some(anchor.pane_id));
        assert!(rig.view().ui().focus_placement.is_none());
        assert!(rig.state.pane.ui.anchor_clamped);
        let report = assert_push_report(&ack, &rig, PushVerdict::Displaced, !cached);
        assert_eq!(
            serde_json::to_value(&report).unwrap()["painted_ms"],
            if cached { -17 } else { 23 }
        );
        assert_eq!(report.misplaced_paints, 0);
        assert_eq!(report.unmatched_paints, 0);
    }
}

#[test]
fn cached_focus_ack_requires_a_frame_at_the_fitted_rows_not_the_current_viewport() {
    let (mut rig, _, anchor) = hidden_destination();
    let (push, ack) = focus_push(&mut rig, anchor.clone());
    rig.state
        .on_focus_push(push, &mut rig.fetch, &mut rig.terminal)
        .unwrap();
    rig.paint(true);
    assert_push_report(&ack, &rig, PushVerdict::Painted, true);
    rig.terminal = Terminal::with_options(
        PaneBackend::headless(FrameOutput::default()),
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 80, 30)),
        },
    )
    .unwrap();
    let (mut push, ack) = focus_push(&mut rig, anchor);
    push.expected_panes = Some(Arc::new(std::collections::HashMap::from([(
        rig.state.pane.config.own_pane.clone().unwrap(),
        crate::mux::FittedPane {
            rows: 30,
            cols: None,
        },
    )])));
    rig.state
        .on_focus_push(push, &mut rig.fetch, &mut rig.terminal)
        .unwrap();
    assert_eq!(
        ack.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty),
        "the cached frame was painted at 24 rows, not 30"
    );
    rig.state.pane.dirty = true;
    rig.state.pane.next_frame = Instant::now();
    rig.paint(true);
    assert_push_report(&ack, &rig, PushVerdict::Painted, false);
}

#[test]
fn parked_focus_ack_waits_for_resize_but_other_panes_rows_do_not_gate_it() {
    for own_rows in [true, false] {
        let (mut rig, _, anchor) = hidden_destination();
        let (mut push, ack) = focus_push(&mut rig, anchor);
        let pane = if own_rows {
            rig.state.pane.config.own_pane.clone().unwrap()
        } else {
            pane("terminal_100", "tab_0", false).pane_id
        };
        push.expected_panes = Some(Arc::new(std::collections::HashMap::from([(
            pane,
            crate::mux::FittedPane {
                rows: 30,
                cols: None,
            },
        )])));
        rig.state
            .on_focus_push(push, &mut rig.fetch, &mut rig.terminal)
            .unwrap();
        rig.paint(true);
        if own_rows {
            assert_eq!(
                ack.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Empty),
                "an old-height paint must not acknowledge a parked push"
            );
            rig.terminal = Terminal::with_options(
                PaneBackend::headless(FrameOutput::default()),
                ratatui::TerminalOptions {
                    viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 80, 30)),
                },
            )
            .unwrap();
            rig.state
                .on_input(Wakeup::Resize, &mut rig.terminal, &mut rig.fetch)
                .unwrap();
        }
        assert_push_report(&ack, &rig, PushVerdict::Painted, true);
    }
}

#[test]
fn parked_push_reports_first_block_expiry_and_supersession() {
    let (mut rig, output, anchor) = hidden_destination();
    rig.state
        .pane
        .paint_hold
        .engage(Instant::now(), crate::utils::time::unix_now_ms());
    let (first, first_ack) = focus_push(&mut rig, anchor.clone());
    rig.state
        .on_focus_push(first, &mut rig.fetch, &mut rig.terminal)
        .unwrap();
    rig.paint(true);
    let mut newer = anchor;
    newer.nonce = crate::ids::FocusNonce::new();
    newer.issued_at_ms += 1;
    let (second, second_ack) = focus_push(&mut rig, newer);
    rig.state
        .on_focus_push(second, &mut rig.fetch, &mut rig.terminal)
        .unwrap();
    let first = assert_push_report(&first_ack, &rig, PushVerdict::Superseded, true);
    assert_eq!(first.blocked_by, Some(PushBlock::PaintHold));
    rig.paint(true);
    rig.state.pane.pending_focus_push.as_mut().unwrap().until = Instant::now();
    rig.paint(true);
    let second = assert_push_report(&second_ack, &rig, PushVerdict::Expired, true);
    assert_eq!(second.blocked_by, Some(PushBlock::PaintHold));
    assert_eq!(paints(&output), 0);

    for nonowner_push in [false, true] {
        let (mut rig, output, anchor) = hidden_destination();
        rig.state
            .pane
            .paint_hold
            .engage(Instant::now(), crate::utils::time::unix_now_ms());
        let (push, ack) = focus_push(&mut rig, anchor.clone());
        rig.state
            .on_focus_push(push, &mut rig.fetch, &mut rig.terminal)
            .unwrap();
        let mut newer = anchor;
        newer.nonce = FocusNonce::new();
        newer.pane_id = pane("terminal_0", "tab_0", false).pane_id;
        let (push, newer_ack) = focus_push(&mut rig, newer);
        if nonowner_push {
            rig.state
                .on_focus_push(push, &mut rig.fetch, &mut rig.terminal)
                .unwrap();
            assert_push_report(&newer_ack, &rig, PushVerdict::NotDestination, false);
        } else {
            drop(push);
            rig.state.pane.paint_hold.release();
            rig.paint(true);
        }
        assert_push_report(&ack, &rig, PushVerdict::Superseded, true);
        assert_eq!(
            paints(&output),
            0,
            "obsolete parked placement must never paint"
        );
    }
}

#[test]
fn parked_push_counts_unmatched_and_misplaced_frames_at_the_paint_consumer() {
    let (mut rig, _, anchor) = hidden_destination();
    rig.state
        .pane
        .paint_hold
        .engage(Instant::now(), crate::utils::time::unix_now_ms());
    let row_id = anchor.placement.as_ref().unwrap().row_id.clone();
    let (push, ack) = focus_push(&mut rig, anchor);
    rig.state
        .on_focus_push(push, &mut rig.fetch, &mut rig.terminal)
        .unwrap();
    rig.view().ui().drawn_selection = None;
    rig.observe_paint(Instant::now());
    rig.view().ui().drawn_selection = Some(row_id);
    assert!(
        rig.view().ui().focus_placement.is_some(),
        "placement has not been consumed by a paint"
    );
    rig.observe_paint(Instant::now());
    assert_eq!(ack.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
    rig.state.pane.should_exit = true;
    rig.paint(true);
    let report = assert_push_report(&ack, &rig, PushVerdict::Exiting, true);
    assert_eq!(report.unmatched_paints, 2);
    assert_eq!(report.misplaced_paints, 1);
}

#[test]
fn focus_click_is_watched_at_the_painted_row_until_the_next_paint() {
    let (mut rig, _) = scroll_rig();
    let (row_id, from_y) = rig.state.pane.ui.painted_rows[1].clone();
    let target = rig
        .view()
        .ctx()
        .current
        .rows()
        .find(|row| row.id == row_id)
        .unwrap()
        .pane
        .as_ref()
        .unwrap()
        .pane_id
        .clone();
    let (tx, rx) = std::sync::mpsc::sync_channel(8);
    rig.state.pane.observe_tx = tx;
    let effect = rig
        .state
        .apply_input(
            [Wakeup::MouseClick {
                column: 2,
                row: from_y as u16,
            }],
            &mut rig.terminal,
        )
        .unwrap();
    assert!(effect.effects.contains(&InputEffect::Focus(target)));
    // Deliberately bypass anchoring to exercise the detector at an actual paint.
    rig.view().ui().offset_authoritative = true;
    rig.view().ui().scroll_offset += 1;
    rig.state.pane.dirty = true;
    paint_at(&mut rig, 0);
    let message = rx.try_recv();
    assert!(
        message.is_ok(),
        "the paint consumer must detect the clicked card moving: {message:?}"
    );
    let ObserveMsg::OwnAnomaly { draft, .. } = message.unwrap() else {
        panic!("expected own anomaly")
    };
    let wire = serde_json::to_value(draft.kind).unwrap();
    assert_eq!(wire["row_id"], row_id);
    assert_eq!(wire["from_y"], from_y);
    assert_eq!(wire["to_y"], from_y - 1);
}

#[test]
fn shared_placement_uses_the_destination_zone_instead_of_its_offset() {
    use crate::mux::focus_anchor::{FocusAnchor, FocusIntentState, FocusPlacement};
    for (viewport_row, banner) in [(-1, false), (2, false), (2, true)] {
        let (mut rig, _) = scroll_rig();
        if banner {
            let mut guard = rig.view();
            let (_, context) = guard.pair();
            let lead = &mut Arc::make_mut(
                &mut Arc::make_mut(&mut context.current.worktree_groups)[0].rows,
            )[0];
            lead.unread = true;
            lead.as_agent_mut().unwrap().status = crate::agents::AgentStatus::Waiting;
        }
        let target = rig.view().ctx().current.worktree_groups[0].rows[4].clone();
        // The anchored pane shares this sidebar's viewed tab, so the anchor is this view's.
        let target_pane = target.pane.as_ref().unwrap().pane_id.clone();
        rig.view().ctx().current.own_view = Some(SidebarOwnView {
            sibling_count: 1,
            working_pane_ids: vec![target_pane.clone()],
            own_view_is_daemon: false,
        });
        rig.view().ctx().current.viewed_panes = vec![target_pane];
        rig.view().ui().selected_index = Some(4);
        rig.view().ui().selected_pane = Some(target.pane.as_ref().unwrap().pane_id.clone());
        rig.view().ui().scroll_offset = 30;
        let stamp = crate::utils::time::unix_now_ms();
        let selected = rig.view().ui().selected_pane.clone().unwrap();
        crate::mux::focus_anchor::store(
            &rig.runtime,
            &FocusAnchor {
                nonce: crate::ids::FocusNonce::new(),
                session_name: "rimz-test".to_owned(),
                pane_id: selected,
                origin: FocusOrigin::User,
                repair_generation: None,
                issued_at_ms: stamp,
                applied_at_ms: Some(stamp),
                state: FocusIntentState::Applied,
                pre_action: Vec::new(),
                placement: Some(FocusPlacement {
                    row_id: target.id.clone(),
                    viewport_row,
                    held: std::collections::BTreeSet::new(),
                }),
                order: None,
            },
        )
        .unwrap();
        rig.state.fold_transition(PaneState::apply_focus_anchor);
        assert!(rig.view().ui().focus_placement.is_some());
        rig.state.pane.dirty = true;
        paint_at(&mut rig, 0);
        let theme = {
            let pane = &mut rig.state.pane;
            let mut guard = rig
                .state
                .views
                .rig(pane.config.own_pane.as_ref(), pane.clock.now());
            let (_, context) = guard.pair();
            pane.ui.theme(&context.current.theme)
        };
        let frame = {
            let pane = &mut rig.state.pane;
            let mut guard = rig
                .state
                .views
                .rig(pane.config.own_pane.as_ref(), pane.clock.now());
            let (ui, context) = guard.pair();
            render::compose_lines(&context.current, None, ui, &pane.ui, &theme, 80, 24)
        };
        let (_, first_line) = rig
            .state
            .pane
            .ui
            .painted_rows
            .iter()
            .find(|(id, _)| id == &target.id)
            .unwrap();
        assert_eq!(*first_line, frame.top_height as isize + viewport_row);
        assert_eq!(
            rig.state
                .focus_placement(&target.pane.as_ref().unwrap().pane_id),
            Some(FocusPlacement {
                row_id: target.id.clone(),
                viewport_row,
                held: [target.id.clone()].into(),
            })
        );
        let offscreen = rig.view().ctx().current.worktree_groups[0]
            .rows
            .last()
            .unwrap()
            .clone();
        assert!(
            rig.state
                .focus_placement(&offscreen.pane.as_ref().unwrap().pane_id)
                .is_none()
        );
        assert_eq!(
            frame
                .interactions
                .line_for_target(&render::HitTarget::UnreadBanner)
                .is_some(),
            banner
        );
    }
}

#[test]
fn shared_placement_never_leaves_the_selected_card_off_screen() {
    use crate::mux::focus_anchor::FocusPlacement;
    // A placement below a shorter destination zone, and a placement for a row the
    // selection has since left through own input: either way the card must be visible.
    for (placed, selected, viewport_row) in [(19, 19, 30), (4, 18, 2)] {
        let (mut rig, _) = scroll_rig();
        let rows = rig.view().ctx().current.worktree_groups[0].rows.clone();
        let placed_id = rows[placed].id.clone();
        let target = rows[selected].clone();
        rig.view().ui().selected_index = Some(selected);
        rig.view().ui().own_input_reveal = true;
        rig.view().ui().selected_pane = Some(target.pane.as_ref().unwrap().pane_id.clone());
        rig.view().ui().scroll_offset = 0;
        rig.view().ui().focus_placement = Some(FocusPlacement {
            row_id: placed_id,
            viewport_row,
            held: std::collections::BTreeSet::new(),
        });
        rig.state.pane.dirty = true;
        paint_at(&mut rig, 0);
        assert!(
            rig.state
                .pane
                .ui
                .painted_rows
                .iter()
                .any(|(id, _)| id == &target.id),
            "selected card {selected} must be painted after placing row {placed} at {viewport_row}"
        );
    }
}

#[test]
fn focus_click_mid_glide_does_not_report_the_glide_as_displacement() {
    let (mut rig, _) = scroll_rig();
    let mut inbox = queued((0..3).map(|_| Wakeup::Scroll { down: true }));
    dispatch(&mut rig, &mut inbox);
    let started = rig.view().ui().frame_ms;
    paint_at(&mut rig, started + 40);
    assert!(rig.view().ui().scroll_ease.in_flight());
    let (tx, rx) = std::sync::mpsc::sync_channel(8);
    rig.state.pane.observe_tx = tx;
    let (_, y) = rig.state.pane.ui.painted_rows.last().unwrap().clone();
    rig.state
        .apply_input(
            [Wakeup::MouseClick {
                column: 2,
                row: y as u16,
            }],
            &mut rig.terminal,
        )
        .unwrap();
    assert!(rig.state.pane.clicked_row.is_some());
    for elapsed in [60, 80, 100, 140] {
        paint_at(&mut rig, started + elapsed);
    }
    assert!(!rig.view().ui().scroll_ease.in_flight());
    assert!(rx.try_recv().is_err(), "wheel glide is not a displacement");
    assert!(
        rig.state.pane.clicked_row.is_some(),
        "the watch survives the glide"
    );
}

#[test]
fn wheel_run_eases_then_releases_the_animation_grid() {
    let (mut rig, output) = scroll_rig();
    let mut inbox = queued((0..3).map(|_| Wakeup::Scroll { down: true }));
    dispatch(&mut rig, &mut inbox);
    assert_eq!(rig.view().ui().scroll_offset, 9);
    assert_eq!(visible(&rig), 0);
    assert_eq!(paints(&output), 1);
    assert!(rig.frame_active());
    let started = rig.view().ui().frame_ms;
    paint_at(&mut rig, started + 100);
    assert_eq!(visible(&rig), 9);
    assert!(!rig.view().ui().scroll_ease.in_flight());
    {
        let pane = &mut rig.state.pane;
        let mut guard = rig
            .state
            .views
            .rig(pane.config.own_pane.as_ref(), pane.clock.now());
        let (ui, context) = guard.pair();
        assert_eq!(
            render::animation_interval(&context.current, ui, &pane.ui, ui.animation_phase, false),
            Some(animation_frame(&context.current))
        );
    }
    paint_at(&mut rig, started + 2100);
    assert!(!rig.frame_active());
}

#[test]
fn click_after_wheel_ease_hits_the_landed_frame() {
    let (mut rig, _) = scroll_rig();
    let mut inbox = queued((0..3).map(|_| Wakeup::Scroll { down: true }));
    dispatch(&mut rig, &mut inbox);
    let started = rig.view().ui().frame_ms;
    for elapsed in [16, 48, 100] {
        paint_at(&mut rig, started + elapsed);
    }
    assert!(!rig.view().ui().scroll_ease.in_flight());
    let index = rig.state.pane.ui.interactions.visible_row_span().unwrap().0 + 1;
    let row = rig.state.pane.ui.interactions.line_for_row(index).unwrap() as u16;
    let id = rig.state.pane.ui.painted_roster[index].clone();
    let expected = rig
        .view()
        .ctx()
        .current
        .rows()
        .find(|row| row.id == id)
        .unwrap()
        .pane
        .as_ref()
        .unwrap()
        .pane_id
        .clone();
    let outcome = rig
        .state
        .apply_input([Wakeup::MouseClick { column: 1, row }], &mut rig.terminal)
        .unwrap();
    assert_eq!(
        outcome.effects,
        vec![InputEffect::Focus(expected)],
        "the glide continues the wheel input, not an automatic layout move"
    );
}

#[test]
fn late_wheel_frame_matches_regular_playback() {
    let (mut regular, _) = scroll_rig();
    let (mut late, _) = scroll_rig();
    let mut starts = Vec::new();
    for rig in [&mut regular, &mut late] {
        let mut inbox = queued((0..3).map(|_| Wakeup::Scroll { down: true }));
        dispatch(rig, &mut inbox);
        starts.push(rig.view().ui().frame_ms);
    }
    for elapsed in [16, 32, 48, 64] {
        paint_at(&mut regular, starts[0] + elapsed);
    }
    paint_at(&mut late, starts[1] + 64);
    assert!((1..9).contains(&visible(&late)));
    assert_eq!(visible(&regular), visible(&late));
}

#[test]
fn selection_key_snaps_an_in_flight_wheel_run() {
    for count in [1, 3] {
        let (mut rig, _) = scroll_rig();
        rig.input(KeyAction::Down);
        let mut inbox = queued((0..count).map(|_| Wakeup::Scroll { down: true }));
        dispatch(&mut rig, &mut inbox);
        assert!(rig.view().ui().scroll_ease.in_flight());
        rig.input(KeyAction::Down);
        assert!(!rig.view().ui().scroll_ease.in_flight());
        assert_eq!(visible(&rig), rig.view().ui().scroll_offset);
        let (first, last) = rig.state.pane.ui.interactions.visible_row_span().unwrap();
        assert!((first..=last).contains(&rig.view().ui().selected_index.unwrap()));
    }
}

#[test]
fn wheel_input_pulls_the_next_grid_frame_in() {
    let (mut rig, _) = scroll_rig();
    rig.state.pane.next_frame = Instant::now() + Duration::from_secs(1);
    assert!(!rig.frame_active(), "the run starts from an idle loop");
    let mut inbox = queued((0..3).map(|_| Wakeup::Scroll { down: true }));
    rig.step(&mut inbox);
    assert!(
        rig.state
            .pane
            .next_frame
            .saturating_duration_since(Instant::now())
            <= render::SCROLL_EASE_FRAME
    );
}

#[test]
fn queued_wheel_ticks_paint_once_at_the_final_offset() {
    for count in [1, 4, 30] {
        let (mut rig, output) = scroll_rig();
        let selected = rig.view().ui().selected_pane.clone();
        let max = rig.state.pane.ui.drawn_scroll_max.unwrap();
        let mut inbox = queued((0..count).map(|_| Wakeup::Scroll { down: true }));
        dispatch(&mut rig, &mut inbox);
        assert_eq!(rig.view().ui().scroll_offset, (count * 3).min(max));
        assert_eq!(paints(&output), 1);
        assert_eq!(rig.view().ui().selected_pane, selected);
        assert!(
            !rig.view().ui().offset_authoritative,
            "paint consumed the wheel intent"
        );
        assert!(rig.view().ui().freeze.is_some());
        assert!(!rig.state.pane.dirty);
    }
}

#[test]
fn wheel_run_matches_per_tick_paints_at_both_edges() {
    for (bottom, directions) in [
        (false, vec![false, false, true, true]),
        (true, vec![true, true, true, true, false, false]),
    ] {
        let (mut batched, _) = scroll_rig();
        let (mut single, _) = scroll_rig();
        for rig in [&mut batched, &mut single] {
            if bottom {
                {
                    let pane = &mut rig.state.pane;
                    let mut guard = rig
                        .state
                        .views
                        .rig(pane.config.own_pane.as_ref(), pane.clock.now());
                    let (ui, _) = guard.pair();
                    ui.scroll_offset = pane.ui.drawn_scroll_max.unwrap();
                }
            }
        }
        let mut inbox = queued(directions.iter().map(|&down| Wakeup::Scroll { down }));
        dispatch(&mut batched, &mut inbox);
        for down in directions {
            single
                .state
                .on_input(
                    Wakeup::Scroll { down },
                    &mut single.terminal,
                    &mut single.fetch,
                )
                .unwrap();
        }
        assert_eq!(
            batched.view().ui().scroll_offset,
            single.view().ui().scroll_offset
        );
    }
}

#[test]
fn key_between_runs_acts_on_the_first_painted_frame() {
    let (mut rig, output) = scroll_rig();
    let mut inbox = queued([
        Wakeup::Scroll { down: true },
        Wakeup::Scroll { down: true },
        Wakeup::Key(KeyAction::ScreenTop),
        Wakeup::Scroll { down: true },
    ]);
    dispatch(&mut rig, &mut inbox);
    assert_eq!(rig.view().ui().scroll_offset, 6);
    assert_eq!(paints(&output), 1);
    // ScreenTop resolves the row from the frame the preceding run painted.
    let expected = rig.state.pane.ui.interactions.visible_row_span().unwrap().0;
    dispatch(&mut rig, &mut inbox);
    assert_eq!(rig.view().ui().selected_index, Some(expected));
    assert_eq!(paints(&output), 2);
    let offset = rig.view().ui().scroll_offset;
    dispatch(&mut rig, &mut inbox);
    assert_eq!(rig.view().ui().scroll_offset, offset + 3);
    assert_eq!(paints(&output), 3);
}

#[test]
fn help_dismissal_consumes_only_the_first_tick_of_a_run() {
    let (mut rig, output) = scroll_rig();
    rig.input(KeyAction::Help);
    output.0.lock().unwrap().clear();
    output.1.store(0, std::sync::atomic::Ordering::SeqCst);
    let mut inbox = queued((0..3).map(|_| Wakeup::Scroll { down: true }));
    dispatch(&mut rig, &mut inbox);
    assert!(!rig.view().ui().help_visible);
    assert_eq!(rig.view().ui().scroll_offset, 6);
    assert_eq!(paints(&output), 1);
}

#[test]
fn hidden_redraw_stays_dirty_until_watched() {
    let (mut rig, output) = scroll_rig();
    let cards = rig.view().ctx().current.worktree_groups.clone();
    rig.hide();
    rig.view().ctx().current.worktree_groups = cards;
    // A wheel run is viewing evidence and paints; a relayout alone is not.
    rig.state
        .on_input(Wakeup::Resize, &mut rig.terminal, &mut rig.fetch)
        .unwrap();
    assert!(rig.state.pane.dirty);
    assert!(output.0.lock().unwrap().is_empty());
    assert_eq!(paints(&output), 0);
    rig.watch();
    rig.state.pane.next_frame = Instant::now();
    rig.paint(true);
    assert_eq!(paints(&output), 1);
    assert!(!rig.state.pane.dirty);
}
