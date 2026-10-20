use super::*;
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
    rig.state
        .ui
        .expanded_groups
        .insert(snapshot.worktree_groups[0].key.clone());
    rig.state.current = snapshot;
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
    assert!(rig.state.ui.drawn_scroll_max.unwrap() > 12);
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

fn viewport_pair(mux: MuxName) -> (Rig, Rig, FrameOutput) {
    let (mut source, _) = scroll_rig();
    let (mut destination, output) = scroll_rig();
    destination.state.viewport = source.state.viewport.clone();
    for (rig, own, working) in [(&mut source, 99, 0), (&mut destination, 98, 19)] {
        rig.state.config.mux = mux;
        let id = |index| {
            PaneId::from_parts(
                mux,
                match mux {
                    MuxName::Tmux => format!("%{index}"),
                    MuxName::Zellij => format!("terminal_{index}"),
                },
            )
        };
        rig.state.config.own_pane = Some(id(own));
        let mut snapshot = rig.state.current.clone();
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
    source.state.dirty = true;
    paint_at(&mut source, 0);
    assert!(source.state.watched());
    assert!(!destination.state.watched());
    (source, destination, output)
}

fn publish_scrolled(source: &mut Rig, offset: usize) {
    source.state.ui.scroll_offset = offset;
    source.state.ui.offset_authoritative = true;
    source.state.ui.scroll_ease.cancel();
    source.state.ui.landed_open = true;
    source.state.dirty = true;
    paint_at(source, 0);
}

#[test]
fn unfrozen_publication_clears_any_hold_and_restores_hidden_deferral() {
    for prior in ["none", "own", "copied"] {
        let (mut source, mut destination, _) = viewport_pair(MuxName::Zellij);
        destination.state.ui.freeze = None;
        if prior == "own" {
            layout_freeze::arm_freeze(&mut destination.state.ui, 1);
        } else if prior == "copied" {
            layout_freeze::arm_freeze(
                &mut source.state.ui,
                source.state.clock.timestamp().as_millisecond(),
            );
            publish_scrolled(&mut source, 12);
            destination
                .state
                .adopt_viewport(&mut destination.fetch, &mut destination.terminal);
            assert!(destination.state.ui.freeze.is_some());
        }
        source.state.ui.freeze = None;
        Arc::make_mut(&mut Arc::make_mut(&mut source.state.current.worktree_groups)[0].rows)
            .swap(1, 2);
        publish_scrolled(&mut source, 3);
        let published = source.state.ui.last_order.clone();
        let rows = Arc::make_mut(
            &mut Arc::make_mut(&mut destination.state.current.worktree_groups)[0].rows,
        );
        let mut arrival = rows.last().unwrap().clone();
        arrival.id = "arrival".into();
        arrival.pane = Some(pane("terminal_20", "tab_0", false));
        rows.push(arrival);
        destination
            .state
            .adopt_viewport(&mut destination.fetch, &mut destination.terminal);
        assert!(destination.state.ui.freeze.is_none(), "prior hold: {prior}");
        let mut expected = published.rows;
        expected.push(render::FrozenRow {
            id: "arrival".into(),
            pane: Some("zellij:terminal_20".into()),
        });
        assert_eq!(destination.state.ui.last_order.rows, expected);
        assert_eq!(
            layout_freeze::capture_order(&destination.state.current, &destination.state.ui).rows,
            expected
        );
        assert!(destination.state.admits_clock_only(false));
        let before = destination.state.projections;
        let mut content = destination.state.current.clone();
        content.display_name = "deferred after adoption".into();
        destination.deliver(destination.lazy_update(content, SnapshotSource::Produced));
        assert_eq!(
            destination.state.projections, before,
            "a settled hidden adoption must not project within the tick"
        );
        assert_ne!(
            destination.state.current.display_name,
            "deferred after adoption"
        );
    }
}

#[test]
fn frozen_publication_holds_nonproducer_order_through_tab_arrival() {
    for (mux, resized) in [
        (MuxName::Zellij, false),
        (MuxName::Tmux, false),
        (MuxName::Zellij, true),
        (MuxName::Tmux, true),
    ] {
        let (mut source, mut destination, _) = viewport_pair(mux);
        let clock = LoopClock::play(Timestamp::now());
        destination.state.clock = clock.clone();
        if resized {
            destination.state.ui.freeze = None;
            destination
                .state
                .on_resize(&mut destination.fetch, &mut destination.terminal, None)
                .unwrap();
            assert!(destination.state.optimistic_watch_until.is_some());
        }
        Arc::make_mut(&mut Arc::make_mut(&mut source.state.current.worktree_groups)[0].rows)
            .swap(7, 8);
        source.state.ui.last_order =
            layout_freeze::capture_order(&source.state.current, &source.state.ui);
        layout_freeze::arm_freeze(
            &mut source.state.ui,
            source.state.clock.timestamp().as_millisecond(),
        );
        publish_scrolled(&mut source, 12);
        let published = source.state.ui.last_order.clone();
        destination
            .state
            .adopt_viewport(&mut destination.fetch, &mut destination.terminal);
        assert!(
            destination.state.ui.freeze.is_some(),
            "{mux:?} resized={resized}: a hidden pane must adopt the frozen publication"
        );
        let hold = destination.state.ui.freeze.as_ref().unwrap();
        assert_eq!(hold.armed_ms, clock.timestamp().as_millisecond());
        assert_eq!(hold.frozen.rows, published.rows);
        assert!(!destination.state.admits_clock_only(false));
        destination.state.next_frame = clock.now();
        destination.paint(true);
        assert!(!destination.state.viewport_pending);
        let painted = destination.state.ui.painted_rows.clone();
        let painted_ids: Vec<_> = painted.iter().map(|(id, _)| id.as_str()).collect();
        assert!(
            painted_ids
                .windows(2)
                .any(|rows| rows == ["agent-8", "agent-7"])
        );
        let target = destination
            .state
            .current
            .own_view
            .as_ref()
            .unwrap()
            .working_pane_ids[0]
            .clone();
        destination.event(SidebarEvent::FocusChanged {
            focused: vec![target],
            unfocused: vec![],
        });
        assert!(destination.state.watched());
        destination.state.next_frame = clock.now();
        destination.paint(true);
        assert_eq!(destination.state.ui.last_order.rows, published.rows);
        assert_eq!(destination.state.ui.painted_rows, painted);
    }
}

#[test]
fn zellij_own_scroll_before_observed_reveal_refuses_viewport_adoption() {
    assert_own_scroll_before_observed_reveal(MuxName::Zellij);
}

#[test]
fn tmux_own_scroll_before_observed_reveal_refuses_viewport_adoption() {
    assert_own_scroll_before_observed_reveal(MuxName::Tmux);
}

fn assert_own_scroll_before_observed_reveal(mux: MuxName) {
    let (mut source, mut destination, _) = viewport_pair(mux);
    let clock = LoopClock::play(Timestamp::now());
    destination.state.clock = clock.clone();
    publish_scrolled(&mut source, 12);
    destination
        .state
        .on_resize(&mut destination.fetch, &mut destination.terminal, None)
        .unwrap();
    destination
        .state
        .on_input(
            Wakeup::Scroll { down: true },
            &mut destination.terminal,
            &mut destination.fetch,
        )
        .unwrap();
    assert!(destination.state.watched());
    assert!(
        !destination
            .state
            .watches_snapshot(&destination.state.current)
    );
    let offset = destination.state.ui.scroll_offset;
    assert!(offset > 12);
    let hold = destination.state.ui.freeze.clone().unwrap();

    source.state.ui.freeze = None;
    publish_scrolled(&mut source, 3);
    destination
        .state
        .adopt_viewport(&mut destination.fetch, &mut destination.terminal);
    assert_eq!(
        destination.state.ui.scroll_offset, offset,
        "{mux:?}: a stale hidden snapshot must not overwrite the user's scroll"
    );
    let retained = destination.state.ui.freeze.as_ref().unwrap();
    assert_eq!(retained.armed_ms, hold.armed_ms);
    assert_eq!(retained.frozen.rows, hold.frozen.rows);

    // A new observed fold supersedes the input evidence, even if still hidden.
    destination.fold(destination.state.current.clone(), SnapshotSource::Produced);
    destination
        .state
        .adopt_viewport(&mut destination.fetch, &mut destination.terminal);
    assert_eq!(destination.state.ui.scroll_offset, 3);
    assert!(destination.state.ui.freeze.is_none());
}

#[test]
fn hidden_resize_does_not_replace_the_watched_viewport() {
    for mux in [MuxName::Zellij, MuxName::Tmux] {
        let (source, mut destination, _) = viewport_pair(mux);
        let sequence = source
            .state
            .viewport
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .sequence;
        destination.state.ui.freeze = None;
        destination
            .state
            .on_resize(&mut destination.fetch, &mut destination.terminal, None)
            .unwrap();
        let latest = source.state.viewport.lock().unwrap();
        let latest = latest.as_ref().unwrap();
        assert_eq!(latest.source, source.state.config.instance_id);
        assert_eq!(latest.sequence, sequence);
    }
}

#[test]
fn freeze_start_and_expiry_each_publish_once_with_unchanged_view() {
    let (mut source, _, _) = viewport_pair(MuxName::Zellij);
    let clock = LoopClock::play(Timestamp::now());
    source.state.clock = clock.clone();
    source.state.ui.freeze = None;
    source.state.tab_read_dwell_until = None;
    source.state.dirty = true;
    source.state.next_frame = clock.now();
    source.paint(true);
    let initial = source
        .state
        .viewport
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .clone();
    source
        .state
        .on_input(
            Wakeup::Scroll { down: false },
            &mut source.terminal,
            &mut source.fetch,
        )
        .unwrap();
    assert!(source.state.ui.freeze.is_some());
    let armed = source
        .state
        .viewport
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .clone();
    assert_eq!(armed.view.rows, initial.view.rows);
    assert_eq!(armed.view.order, initial.view.order);
    assert_eq!(
        armed.sequence,
        initial.sequence + 1,
        "arming alone republishes"
    );
    clock.advance(crate::sidebar::timing::LAYOUT_FREEZE);
    let mut snapshot = source.state.current.clone();
    snapshot.now = clock.timestamp();
    source.fold(snapshot, SnapshotSource::Produced);
    assert!(source.state.ui.freeze.is_none());
    source.state.next_frame = clock.now();
    source.paint(true);
    let expired = source
        .state
        .viewport
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .clone();
    assert_eq!(expired.view.rows, initial.view.rows);
    assert_eq!(expired.view.order, initial.view.order);
    assert_eq!(
        expired.sequence,
        armed.sequence + 1,
        "expiry alone republishes"
    );
    source.state.dirty = true;
    source.state.next_frame = clock.now();
    source.paint(true);
    assert_eq!(
        source
            .state
            .viewport
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .sequence,
        expired.sequence,
        "identical paints deduplicate"
    );
}

fn offscreen_focus_marker(mux: MuxName) -> (Rig, Instant) {
    let (mut rig, _) = scroll_rig();
    rig.state.config.mux = mux;
    rig.state.config.own_pane = None;
    let mut focused = rig.state.current.clone();
    focused.focused_pane = focused.worktree_groups[0].rows[19]
        .pane
        .as_ref()
        .map(|pane| pane.pane_id.clone());
    let before = Instant::now();
    rig.fold(focused, SnapshotSource::Produced);
    let after = Instant::now();
    assert!(
        matches!(rig.state.focus_marker, Some(FocusMarker::Dwelling(_))),
        "the consuming focus fold arms a dwell"
    );
    let Some(FocusMarker::Dwelling(deadline)) = rig.state.focus_marker else {
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
            .ui
            .interactions
            .line_for_target(&render::HitTarget::FocusMarker)
            .is_none()
    );
    let rows = rig.state.ui.painted_rows.clone();
    let anchors = rig.state.ui.anchored_rows.clone();
    let offset = rig.state.ui.scroll_offset;
    rig.state
        .draw_frame(&mut rig.terminal, deadline, false)
        .unwrap();
    assert!(
        rig.state
            .ui
            .interactions
            .line_for_target(&render::HitTarget::FocusMarker)
            .is_some()
    );
    assert_eq!(rig.state.ui.painted_rows, rows);
    assert_eq!(rig.state.ui.anchored_rows, anchors);
    assert_eq!(rig.state.ui.scroll_offset, offset);
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
    let mut focused = rig.state.current.clone();
    focused.focused_pane = focused.worktree_groups[0].rows[18]
        .pane
        .as_ref()
        .map(|pane| pane.pane_id.clone());
    rig.fold(focused, SnapshotSource::Produced);
    let Some(FocusMarker::Dwelling(deadline)) = rig.state.focus_marker else {
        panic!("rearmed dwell")
    };
    assert!(deadline > previous);
    rig.state
        .draw_frame(&mut rig.terminal, previous, false)
        .unwrap();
    assert!(
        rig.state
            .ui
            .interactions
            .line_for_target(&render::HitTarget::FocusMarker)
            .is_none()
    );
}

#[test]
fn marker_deadline_wakes_and_paints_an_otherwise_idle_sidebar() {
    let (mut rig, _) = scroll_rig();
    rig.state.ui.selected_index = Some(19);
    rig.state.ui.selected_pane = rig.state.current.worktree_groups[0].rows[19]
        .pane
        .as_ref()
        .map(|pane| pane.pane_id.clone());
    rig.state.ui.freeze = None;
    rig.state.dirty = false;
    rig.state.focus_marker = Some(FocusMarker::Dwelling(
        Instant::now() + Duration::from_millis(50),
    ));
    let (active, timeout) = rig.state.frame_timing();
    assert!(!active);
    assert!(
        timeout <= Duration::from_millis(50),
        "the dwell caps the idle timeout"
    );
    rig.state.focus_marker = Some(FocusMarker::Dwelling(Instant::now()));
    rig.state.next_frame = Instant::now() + Duration::from_secs(2);
    let (active, _) = rig.state.frame_timing();
    assert!(active, "the elapsed dwell itself makes the frame due");
    rig.paint(active);
    assert!(
        rig.state
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
        .ui
        .interactions
        .line_for_target(&render::HitTarget::FocusMarker)
        .unwrap();
    let selected = rig.state.ui.selected_pane.clone();
    let applied = rig
        .state
        .apply_input([Wakeup::MouseClick { column, row }], &mut rig.terminal)
        .unwrap();
    assert!(applied.effects.is_empty());
    assert_eq!(rig.state.ui.selected_pane, selected);
    assert!(rig.state.ui.scroll_offset > 0);
    assert!(
        rig.state
            .ui
            .painted_rows
            .iter()
            .any(|(id, _)| id == "agent-19")
    );
    assert!(
        rig.state
            .ui
            .interactions
            .line_for_target(&render::HitTarget::FocusMarker)
            .is_none()
    );
    assert!(rig.state.focus_marker.is_none());
}

#[test]
fn hidden_paints_hide_the_marker_and_own_selection_ends_it() {
    let (mut rig, deadline) = offscreen_focus_marker(MuxName::Zellij);
    rig.state
        .draw_frame(&mut rig.terminal, deadline, false)
        .unwrap();
    assert!(
        rig.state
            .ui
            .interactions
            .line_for_target(&render::HitTarget::FocusMarker)
            .is_some()
    );
    rig.state.config.own_pane = Some(PaneId::from_parts(MuxName::Zellij, "terminal_99"));
    rig.state.current.own_view = Some(own_view());
    rig.state.current.viewed_panes.clear();
    assert!(!rig.state.watched());
    rig.state
        .draw_frame(&mut rig.terminal, deadline, false)
        .unwrap();
    assert!(
        rig.state
            .ui
            .interactions
            .line_for_target(&render::HitTarget::FocusMarker)
            .is_none()
    );
    rig.state
        .apply_input([Wakeup::Key(KeyAction::Top)], &mut rig.terminal)
        .unwrap();
    assert!(rig.state.focus_marker.is_none());
}

fn assert_native_arrival_keeps_shared_frame(mux: MuxName) {
    let (mut source, mut destination, output) = viewport_pair(mux);
    publish_scrolled(&mut source, 12);
    destination.state.click_guard = Some(ClickGuard {
        until: Instant::now() + Duration::from_secs(1),
        interactions: destination.state.ui.interactions.clone(),
        roster: destination.state.ui.painted_roster.clone(),
    });
    destination.maintenance();
    destination.state.next_frame = Instant::now();
    destination.paint(false);
    assert_eq!(
        paints(&output),
        1,
        "the hidden destination paints the publication"
    );
    assert_eq!(
        destination.state.ui.painted_rows,
        source.state.ui.anchored_rows
    );
    assert!(destination.state.ui.landed_open);
    assert!(destination.state.click_guard.is_none());
    let before = destination.state.ui.painted_rows.clone();
    let target = destination.state.current.worktree_groups[0].rows[19]
        .pane
        .as_ref()
        .unwrap()
        .pane_id
        .clone();
    let previous = destination.state.current.focused_pane.clone().unwrap();
    destination.event(SidebarEvent::FocusChanged {
        focused: vec![target.clone()],
        unfocused: if mux == MuxName::Tmux {
            vec![]
        } else {
            vec![previous]
        },
    });
    assert!(destination.state.watched());
    paint_at(&mut destination, 0);
    assert_eq!(destination.state.ui.selected_pane, Some(target));
    assert_eq!(
        destination.state.ui.painted_rows, before,
        "arrival moves only the highlight"
    );
    assert!(
        !destination
            .state
            .ui
            .painted_rows
            .iter()
            .any(|(id, _)| id == "agent-19")
    );
    assert!(destination.state.click_guard.is_none());
    let (y, ordinal) = (0..24)
        .find_map(
            |y| match destination.state.ui.interactions.target_at(2, y) {
                Some(render::HitTarget::Row(ordinal)) => Some((y, ordinal)),
                _ => None,
            },
        )
        .unwrap();
    let clicked = destination
        .state
        .ui
        .visible_roster(&destination.state.current)
        .row(ordinal)
        .unwrap()
        .pane
        .as_ref()
        .unwrap()
        .pane_id
        .clone();
    let applied = destination
        .state
        .apply_input(
            [Wakeup::MouseClick { column: 2, row: y }],
            &mut destination.terminal,
        )
        .unwrap();
    assert_eq!(applied.effects, vec![InputEffect::Focus(clicked)]);
}

#[test]
fn tmux_native_arrival_keeps_the_departing_viewport() {
    assert_native_arrival_keeps_shared_frame(MuxName::Tmux);
}

#[test]
fn tmux_switch_away_from_the_last_pulled_view_allows_hidden_adoption() {
    let (mut source, mut destination, _) = viewport_pair(MuxName::Tmux);
    let source_pane = source.state.current.focused_pane.clone().unwrap();
    let destination_pane = destination
        .state
        .current
        .own_view
        .as_ref()
        .unwrap()
        .working_pane_ids[0]
        .clone();
    for rig in [&mut source, &mut destination] {
        let mut pulled = rig.state.current.clone();
        pulled.focused_pane = Some(destination_pane.clone());
        pulled.viewed_panes = vec![destination_pane.clone()];
        pulled.client_views = vec![crate::pane::ClientPaneView {
            client_id: crate::ids::MuxClientId::Tmux("client-1".into()),
            pane_id: destination_pane.clone(),
        }];
        rig.fold(pulled, SnapshotSource::Produced);
        paint_at(rig, 0);
        rig.event(SidebarEvent::FocusChanged {
            focused: vec![source_pane.clone()],
            unfocused: vec![],
        });
    }
    publish_scrolled(&mut source, 12);
    destination.maintenance();
    destination.state.next_frame = Instant::now();
    destination.paint(false);
    assert_eq!(
        destination.state.ui.painted_rows, source.state.ui.anchored_rows,
        "a stale pulled client view must not block hidden adoption"
    );
    assert!(!destination.state.watched());
    for rig in [&mut source, &mut destination] {
        rig.event(SidebarEvent::FocusChanged {
            focused: vec![destination_pane.clone()],
            unfocused: vec![],
        });
    }
    let before = destination.state.ui.painted_rows.clone();
    paint_at(&mut destination, 0);
    assert_eq!(destination.state.ui.painted_rows, before);
    for target in [source_pane, destination_pane].iter().cycle().take(8) {
        for rig in [&mut source, &mut destination] {
            rig.event(SidebarEvent::FocusChanged {
                focused: vec![target.clone()],
                unfocused: vec![],
            });
            rig.maintenance();
            paint_at(rig, 0);
            assert_eq!(
                rig.state.ui.painted_rows, before,
                "rapid switch without a pull"
            );
        }
    }
}

#[test]
fn tmux_focus_overlay_keeps_other_clients_watched() {
    let (mut source, mut destination, _) = viewport_pair(MuxName::Tmux);
    let target = source.state.current.focused_pane.clone().unwrap();
    let other = destination
        .state
        .current
        .own_view
        .as_ref()
        .unwrap()
        .working_pane_ids[0]
        .clone();
    let mut pulled = destination.state.current.clone();
    pulled.viewed_panes.push(other.clone());
    pulled.client_views = [target.clone(), other]
        .into_iter()
        .enumerate()
        .map(|(i, pane_id)| crate::pane::ClientPaneView {
            client_id: crate::ids::MuxClientId::Tmux(format!("client-{i}")),
            pane_id,
        })
        .collect();
    destination.fold(pulled, SnapshotSource::Produced);
    paint_at(&mut destination, 0);
    let before = destination.state.ui.painted_rows.clone();
    destination.event(SidebarEvent::FocusChanged {
        focused: vec![target],
        unfocused: vec![],
    });
    publish_scrolled(&mut source, 12);
    destination.maintenance();
    destination.state.next_frame = Instant::now();
    destination.paint(false);
    assert!(destination.state.watched());
    assert_eq!(destination.state.ui.painted_rows, before);
}

#[test]
fn tmux_switch_from_sidebar_cancels_optimistic_watching() {
    let (source, mut destination, _) = viewport_pair(MuxName::Tmux);
    let own = destination.state.config.own_pane.clone().unwrap();
    destination.state.current.client_views = vec![crate::pane::ClientPaneView {
        client_id: crate::ids::MuxClientId::Tmux("client-1".into()),
        pane_id: own.clone(),
    }];
    destination.event(SidebarEvent::FocusChanged {
        focused: vec![own],
        unfocused: vec![],
    });
    assert!(destination.state.optimistic_watch_until.is_some());
    destination.event(SidebarEvent::FocusChanged {
        focused: source
            .state
            .current
            .focused_pane
            .clone()
            .into_iter()
            .collect(),
        unfocused: vec![],
    });
    assert!(
        destination.state.optimistic_watch_until.is_none(),
        "a single client's switch supersedes optimistic resize/focus watching"
    );
    assert!(!destination.state.watched());
}

#[test]
fn held_source_handles_cross_tab_click_before_dispatch() {
    held_source_click(true);
}

#[test]
fn held_source_handles_same_tab_click_before_dispatch() {
    held_source_click(false);
}

fn held_source_click(cross_tab: bool) {
    let (mut source, _, _) = viewport_pair(MuxName::Zellij);
    source.state.clock.skip_focus = true;
    let peers = source.focus_plane.focus_peers(
        source.state.config.instance_id.clone(),
        crate::diag::DiagSink::disabled(),
    );
    source.state.focus_peers = Some(peers.clone());
    let old_target = pane("terminal_19", "tab_1", false).pane_id;
    let old = peers.arm(old_target.clone(), Instant::now());
    assert!(source.state.jump_hold().is_some());
    let index = usize::from(cross_tab);
    let (row_id, row) = source.state.ui.painted_rows[index].clone();
    let target = source
        .state
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
    source
        .result_tx
        .send(FetchUpdate::Snapshot {
            snapshot: Box::new(source.state.current.clone()),
            phase: FetchPhase::Final,
            source: SnapshotSource::Produced,
        })
        .unwrap();
    let projections = source.state.projections;
    source
        .state
        .advance(&mut inbox, &mut source.fetch, &mut source.terminal)
        .unwrap();
    assert_eq!(
        source.state.pending_input_focus,
        Some(target),
        "a held source must handle the new click before dispatch"
    );
    assert_eq!(
        source
            .state
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
        source.state.projections, projections,
        "room work stays held"
    );
}

#[test]
fn same_tab_focus_supersedes_pending_cross_tab_push() {
    for keyboard in [false, true] {
        let (mut source, destination, _) = viewport_pair(MuxName::Zellij);
        let diag = crate::diag::DiagSink::under(
            source._dir.path().to_owned(),
            source.ws.clone(),
            &source.state.config.session_name,
            Some(source.state.config.instance_id.clone()),
        );
        let peers = source
            .focus_plane
            .focus_peers(source.state.config.instance_id.clone(), diag);
        source.state.focus_peers = Some(peers.clone());
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        let _subscription = source.focus_plane.subscribe(&destination.state.config, tx);
        let destination_peers = source.focus_plane.focus_peers(
            destination.state.config.instance_id.clone(),
            crate::diag::DiagSink::disabled(),
        );
        let (_, row) = source.state.ui.painted_rows[1].clone();
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
        let first = source.state.input_jump.take().unwrap();
        let (_, _, mut anchor) = hidden_destination();
        anchor.pane_id = source.state.pending_input_focus.clone().unwrap();
        let (_, _, _, ack, _) = first
            .push(&anchor, first.clicked().unwrap(), Instant::now)
            .unwrap();
        rx.try_iter().for_each(drop);
        assert!(source.state.jump_hold().is_some());
        let (_, row) = source.state.ui.painted_rows[0].clone();
        let input = if keyboard {
            source.state.ui.selected_index = Some(0);
            source.state.ui.selected_pane = Some(pane("terminal_0", "tab_0", false).pane_id);
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
        assert_eq!(source.state.pending_input_focus, Some(target));
        assert!(
            source.state.input_jump.is_none(),
            "same-tab focus arms no push"
        );
        assert!(
            !first.is_current(),
            "same-tab focus must revoke the older lease"
        );
        assert!(
            peers.active(Instant::now()).is_none(),
            "same-tab focus arms no hold"
        );
        assert!(destination_peers.take_push().is_none());
        let report = ack
            .try_iter()
            .find(|report| report.instance_id == destination.state.config.instance_id)
            .unwrap();
        assert_eq!(report.verdict, Some(PushVerdict::Superseded));
        assert!(
            matches!(rx.try_recv(), Ok(Wakeup::Tick)),
            "release wakes held peers"
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
            source.state.current.worktree_groups[0].rows[1]
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
fn jump_hold_retains_ordinary_work_until_release_or_virtual_deadline() {
    for release in [false, true] {
        let (mut source, mut rig, output) = viewport_pair(MuxName::Zellij);
        let clock = LoopClock::play(Timestamp::now());
        rig.state.clock = clock.clone();
        rig.state.current.pane_session_name = Some("rimz-test".into());
        let plane = super::super::super::plane::DataPlane::detached();
        let peers = plane.focus_peers(
            source.state.config.instance_id.clone(),
            crate::diag::DiagSink::disabled(),
        );
        rig.state.focus_peers = Some(plane.focus_peers(
            rig.state.config.instance_id.clone(),
            crate::diag::DiagSink::disabled(),
        ));
        let target = pane("terminal_0", "tab_0", false).pane_id;
        let lease = peers.arm(target.clone(), clock.now());
        publish_scrolled(&mut source, 12);
        let (_, _, mut anchor) = hidden_destination();
        anchor.pane_id = target.clone();
        anchor.placement = None;
        anchor.order = None;
        crate::mux::focus_anchor::store(&rig.runtime, &anchor).unwrap();
        let event = SidebarEventEnvelope::new(
            rig.ws.clone(),
            Some("rimz-test".into()),
            crate::utils::time::unix_now_ms(),
            SidebarEvent::FocusIntent {
                pane_id: target,
                nonce: anchor.nonce,
            },
        );
        let mut inbox = queued([Wakeup::Event(event), Wakeup::Tick, Wakeup::Snapshot]);
        rig.result_tx
            .send(FetchUpdate::Snapshot {
                snapshot: Box::new(rig.state.current.clone()),
                phase: FetchPhase::Final,
                source: SnapshotSource::Produced,
            })
            .unwrap();
        let before = (
            rig.state.content_applications,
            rig.state.projections,
            rig.state.viewport_seen,
            rig.state.last_folded_focus_intent,
        );
        let paints_before = rig.state.paints;
        rig.state.adopt_viewport(&mut rig.fetch, &mut rig.terminal);
        for _ in 0..3 {
            rig.state
                .advance(&mut inbox, &mut rig.fetch, &mut rig.terminal)
                .unwrap();
        }
        assert_eq!(
            (
                rig.state.content_applications,
                rig.state.projections,
                rig.state.viewport_seen,
                rig.state.last_folded_focus_intent
            ),
            before,
            "held work must not reach its consumer"
        );
        assert_eq!(paints(&output), 0);
        if release {
            lease.release();
        } else {
            clock.advance(Duration::from_millis(500));
        }
        rig.state
            .advance(&mut inbox, &mut rig.fetch, &mut rig.terminal)
            .unwrap();
        assert_eq!(
            rig.state.last_folded_focus_intent,
            Some(anchor.nonce),
            "the retained event replays first"
        );
        for _ in 0..3 {
            rig.state
                .advance(&mut inbox, &mut rig.fetch, &mut rig.terminal)
                .unwrap();
        }
        assert!(rig.state.viewport_seen > before.2);
        assert!(
            rig.state.paints - paints_before <= 1,
            "catch-up coalesces to one hidden paint"
        );
    }
}

#[test]
fn source_click_arms_before_publication_and_release_wakes_each_peer_once() {
    let (mut source, destination, _) = viewport_pair(MuxName::Zellij);
    let plane = super::super::super::plane::DataPlane::detached();
    let (source_tx, source_rx) = std::sync::mpsc::sync_channel(64);
    let (dest_tx, dest_rx) = std::sync::mpsc::sync_channel(64);
    let _source_subscription = plane.subscribe(&source.state.config, source_tx);
    let _destination_subscription = plane.subscribe(&destination.state.config, dest_tx);
    let peers = plane.focus_peers(
        source.state.config.instance_id.clone(),
        crate::diag::DiagSink::disabled(),
    );
    source.state.focus_peers = Some(peers.clone());
    source.state.viewport_peers = Some(plane.subscribers.clone());
    let (_, row) = source.state.ui.painted_rows[1].clone();
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
        peers.active(source.state.clock.now()).is_some(),
        "lease must precede click publication"
    );
    assert!(
        dest_rx.try_recv().is_err(),
        "click publication must not wake peers"
    );
    drop(source.state.input_jump.take());
    assert_eq!(source_rx.try_recv().unwrap(), Wakeup::Tick);
    assert_eq!(dest_rx.try_recv().unwrap(), Wakeup::Tick);
    assert!(source_rx.try_recv().is_err());
    assert!(dest_rx.try_recv().is_err());
}

#[test]
fn queued_publication_tick_does_not_paint_ahead_of_a_focus_push() {
    let (mut source, mut destination, output) = viewport_pair(MuxName::Tmux);
    publish_scrolled(&mut source, 12);
    let target = PaneId::from_parts(MuxName::Tmux, "%8");
    let mut pulled = destination.state.current.clone();
    pulled.own_view.as_mut().unwrap().working_pane_ids = vec![target.clone()];
    pulled.pane_session_name = Some("rimz-test".into());
    destination.fold(pulled, SnapshotSource::Produced);
    destination.state.titled = true;
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
    let _subscription = plane.subscribe(&destination.state.config, tx);
    destination.state.focus_peers = Some(plane.focus_peers(
        destination.state.config.instance_id.clone(),
        crate::diag::DiagSink::disabled(),
    ));
    let clicked = Instant::now();
    let lease = plane
        .focus_peers(
            source.state.config.instance_id.clone(),
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
    destination.state.next_frame = Instant::now();
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
    assert!(destination.state.ui.painted_rows.contains(&(
        "agent-8".into(),
        destination.state.ui.scroll_zone_top as isize + 2
    )));
}

#[test]
fn existing_attachment_holds_selection_until_a_new_window_has_a_row() {
    let (mut source, mut destination, _) = viewport_pair(MuxName::Tmux);
    let previous = destination
        .state
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
    publish_scrolled(&mut source, 12);
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
            &destination.state.current,
            destination.state.config.own_pane.as_ref()
        ),
        None
    );
    assert_eq!(destination.state.ui.selected_pane, Some(previous));
    assert_eq!(destination.state.ui.selected_index, Some(19));
    let new_pane = PaneId::from_parts(MuxName::Tmux, "%20");
    let mut admitted = destination.state.current.clone();
    let rows = Arc::make_mut(&mut Arc::make_mut(&mut admitted.worktree_groups)[0].rows);
    let mut row = rows[19].clone();
    row.id = "agent-20".into();
    row.pane.as_mut().unwrap().pane_id = new_pane.clone();
    rows.push(row);
    destination.fold(admitted, SnapshotSource::Produced);
    paint_at(&mut destination, 0);
    assert_eq!(destination.state.ui.selected_pane, Some(new_pane));
}

#[test]
fn zellij_native_arrival_keeps_the_departing_viewport() {
    assert_native_arrival_keeps_shared_frame(MuxName::Zellij);
}

#[test]
fn shared_viewport_skips_watched_peers_and_its_source() {
    let (mut source, mut destination, output) = viewport_pair(MuxName::Zellij);
    publish_scrolled(&mut source, 12);
    destination.maintenance();
    destination.state.next_frame = Instant::now();
    destination.paint(false);
    assert_eq!(paints(&output), 1);
    let held = destination.state.ui.painted_rows.clone();
    destination
        .state
        .current
        .viewed_panes
        .push(destination.state.config.own_pane.clone().unwrap());
    assert!(destination.state.watched());
    publish_scrolled(&mut source, 20);
    destination.maintenance();
    destination.state.next_frame = Instant::now();
    destination.paint(false);
    assert_eq!(
        destination.state.ui.painted_rows, held,
        "another watched client keeps its own view"
    );
    source.state.current.viewed_panes.clear();
    source.state.ui.scroll_offset = 3;
    source.maintenance();
    assert_eq!(
        source.state.ui.scroll_offset, 3,
        "never adopt one's own publication"
    );
}

#[test]
fn shared_viewport_publishes_glide_target_once_and_coalesces_hidden_paints() {
    let (mut source, mut destination, output) = viewport_pair(MuxName::Zellij);
    source
        .state
        .on_input(
            Wakeup::Scroll { down: true },
            &mut source.terminal,
            &mut source.fetch,
        )
        .unwrap();
    assert!(source.state.ui.scroll_ease.in_flight());
    destination.maintenance();
    destination.state.next_frame = Instant::now();
    destination.paint(false);
    assert_eq!(paints(&output), 1);
    assert_eq!(
        destination.state.ui.painted_rows,
        source.state.ui.anchored_rows
    );
    let sequence = source
        .state
        .viewport
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .sequence;
    paint_at(&mut source, 16);
    assert_eq!(
        source
            .state
            .viewport
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .sequence,
        sequence,
        "animation alone never republishes"
    );
    destination.state.next_frame = Instant::now() + Duration::from_secs(1);
    for offset in [12, 20] {
        publish_scrolled(&mut source, offset);
        destination.maintenance();
        destination.state.next_frame = Instant::now() + Duration::from_millis(100);
        destination.paint(false);
        assert_eq!(paints(&output), 1, "hidden paints obey the frame boundary");
    }
    destination.state.next_frame = Instant::now();
    destination.paint(false);
    assert_eq!(paints(&output), 2);
    assert_eq!(
        destination.state.ui.painted_rows,
        source.state.ui.anchored_rows
    );
}

#[test]
fn newborn_viewport_adopts_before_its_first_paint_and_flushes_hidden_content() {
    let (mut source, mut destination, output) = viewport_pair(MuxName::Zellij);
    destination.state.ui.freeze = None;
    let mut update = destination.state.current.clone();
    update.display_name = "deferred content".into();
    let update = destination.lazy_update(update, SnapshotSource::Produced);
    destination.deliver(update);
    assert_ne!(destination.state.current.display_name, "deferred content");
    publish_scrolled(&mut source, 12);
    destination.maintenance();
    destination.state.next_frame = Instant::now();
    destination.paint(false);
    assert_eq!(paints(&output), 1);
    assert_eq!(destination.state.current.display_name, "deferred content");
    destination.state.ui = UiState {
        selected_pane: destination.state.current.focused_pane.clone(),
        selected_index: Some(0),
        ..UiState::default()
    };
    destination
        .state
        .current
        .viewed_panes
        .push(destination.state.config.own_pane.clone().unwrap());
    destination.state.viewport_seen = 0;
    destination
        .state
        .on_input(
            Wakeup::Resize,
            &mut destination.terminal,
            &mut destination.fetch,
        )
        .unwrap();
    assert_eq!(
        destination.state.ui.painted_rows, source.state.ui.anchored_rows,
        "newborn uses its own measured cockpit top"
    );
}

fn move_cards_without_input(rig: &mut Rig, first: usize, second: usize, now: Instant) {
    rig.state.ui.freeze = None;
    Arc::make_mut(&mut Arc::make_mut(&mut rig.state.current.worktree_groups)[0].rows)
        .swap(first, second);
    rig.state.dirty = true;
    rig.state.paint_now(&mut rig.terminal, now).unwrap();
}

#[test]
fn arrival_freezes_the_painted_order_not_a_hidden_fold() {
    let (mut rig, _) = scroll_rig();
    let painted = layout_freeze::capture_order(&rig.state.current, &rig.state.ui);
    let mut hidden = rig.state.current.clone();
    hidden.own_view = Some(own_view());
    hidden.viewed_panes.clear();
    hidden.focused_pane = None;
    rig.fold(hidden, SnapshotSource::Produced);
    rig.state.ui.freeze = None;
    let mut reranked = rig.state.current.clone();
    Arc::make_mut(&mut Arc::make_mut(&mut reranked.worktree_groups)[0].rows)[1].attention_score =
        1000;
    Arc::make_mut(&mut Arc::make_mut(&mut reranked.worktree_groups)[0].rows)[1]
        .as_agent_mut()
        .unwrap()
        .status = crate::agents::AgentStatus::Waiting;
    rig.fold(reranked, SnapshotSource::Produced);
    rig.state.drain_snapshot(&mut rig.fetch, false);
    assert_eq!(rig.state.current.worktree_groups[0].rows[0].id, "agent-1");
    let mut arrival = rig.state.current.clone();
    arrival.viewed_panes = own_view().working_pane_ids;
    rig.fold(arrival, SnapshotSource::Produced);
    rig.state.next_frame = Instant::now();
    rig.paint(true);
    assert_eq!(rig.state.ui.last_order.rows, painted.rows);
    assert!(rig.state.ui.freeze.is_some());
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
        let row = rig.state.ui.interactions.line_for_row(1).unwrap() as u16;
        let now = Instant::now();
        move_cards_without_input(&mut rig, 1, 2, now);
        if input_paint {
            rig.state
                .apply_input_at(
                    [Wakeup::Scroll { down: false }],
                    &mut rig.terminal,
                    now + Duration::from_millis(50),
                )
                .unwrap();
        }
        if second_move {
            move_cards_without_input(&mut rig, 1, 3, now + Duration::from_millis(200));
        }
        let outcome = rig
            .state
            .apply_input_at(
                [Wakeup::MouseClick { column: 1, row }],
                &mut rig.terminal,
                now + Duration::from_millis(click_ms),
            )
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
    let row = rig.state.ui.interactions.line_for_row(1).unwrap() as u16;
    let now = Instant::now();
    Arc::make_mut(&mut Arc::make_mut(&mut rig.state.current.worktree_groups)[0].rows).remove(1);
    rig.state.dirty = true;
    rig.state.paint_now(&mut rig.terminal, now).unwrap();
    layout_freeze::arm_freeze(&mut rig.state.ui, 1);
    let selection = rig.state.ui.selected_pane.clone();
    let before = paints(&output);
    let outcome = rig
        .state
        .apply_input_at(
            [Wakeup::MouseClick { column: 1, row }],
            &mut rig.terminal,
            now + Duration::from_millis(399),
        )
        .unwrap();
    assert_eq!(
        outcome,
        InputOutcome::default(),
        "do not click the replacement card"
    );
    assert_eq!(rig.state.ui.selected_pane, selection);
    assert_eq!(paints(&output), before);
    assert_eq!(rig.state.ui.freeze.as_ref().unwrap().armed_ms, 1);
}

#[test]
fn resize_delivery_is_not_a_user_input_paint_for_the_click_guard() {
    let (mut rig, _) = scroll_rig();
    let row = rig.state.ui.interactions.line_for_row(1).unwrap() as u16;
    let now = Instant::now();
    move_cards_without_input(&mut rig, 1, 2, now);
    rig.state
        .apply_input_at(
            [Wakeup::Resize],
            &mut rig.terminal,
            now + Duration::from_millis(100),
        )
        .unwrap();
    let outcome = rig
        .state
        .apply_input_at(
            [Wakeup::MouseClick { column: 1, row }],
            &mut rig.terminal,
            now + Duration::from_millis(399),
        )
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
    let row = rig.state.ui.interactions.line_for_row(1).unwrap() as u16;
    Arc::make_mut(&mut Arc::make_mut(&mut rig.state.current.worktree_groups)[0].rows).swap(1, 2);
    let outcome = rig
        .state
        .apply_input_at(
            [Wakeup::MouseClick { column: 1, row }],
            &mut rig.terminal,
            Instant::now(),
        )
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
        .ui
        .scroll_ease
        .visible(rig.state.ui.scroll_offset, rig.state.ui.frame_ms)
}

fn paint_at(rig: &mut Rig, elapsed_ms: u64) {
    rig.state.anim_start = Instant::now() - Duration::from_millis(elapsed_ms);
    rig.state.next_frame = Instant::now();
    rig.paint(true);
}

fn hidden_destination() -> (Rig, FrameOutput, crate::mux::focus_anchor::FocusAnchor) {
    use crate::mux::focus_anchor::{FocusAnchor, FocusIntentState, FocusPlacement};
    let (mut rig, output) = scroll_rig();
    let target = rig.state.current.worktree_groups[0].rows[4].clone();
    let pane_id = target.pane.as_ref().unwrap().pane_id.clone();
    let mut pulled = rig.state.current.clone();
    pulled.own_view = Some(SidebarOwnView {
        sibling_count: 1,
        working_pane_ids: vec![pane_id.clone()],
        own_view_is_daemon: false,
    });
    pulled.focused_pane = Some(pane("terminal_0", "tab_0", false).pane_id);
    pulled.viewed_panes.clear();
    pulled.pane_session_name = Some("rimz-test".to_owned());
    rig.state.optimistic_watch_until = None;
    rig.state.ui.scroll_offset = 30;
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
        order: Some(rig.state.ui.last_order.clone()),
    };
    (rig, output, anchor)
}

fn assert_external_selection_stays_offscreen(rowless_anchor: bool) {
    let (mut rig, _) = scroll_rig();
    rig.state.config.own_pane = None;
    let target = rig.state.current.worktree_groups[0].rows[19]
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
    let mut focused = rig.state.current.clone();
    focused.focused_pane = Some(target.clone());
    rig.fold(focused, SnapshotSource::Produced);
    paint_at(&mut rig, 0);
    assert_eq!(rig.state.ui.selected_pane, Some(target));
    assert_eq!(rig.state.ui.scroll_offset, 0);
    assert!(
        !rig.state
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
    assert!(rig.state.ui.scroll_offset > 0);
    assert!(
        rig.state
            .ui
            .painted_rows
            .iter()
            .any(|(id, _)| id == "agent-19")
    );
}

#[test]
fn own_inbox_jump_reveals_when_its_offscreen_target_lands() {
    let (mut rig, _) = scroll_rig();
    rig.state.config.own_pane = None;
    Arc::make_mut(&mut Arc::make_mut(&mut rig.state.current.worktree_groups)[0].rows)[19]
        .as_agent_mut()
        .unwrap()
        .status = crate::agents::AgentStatus::Waiting;
    let target = rig.state.current.worktree_groups[0].rows[19]
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
    assert_eq!(rig.state.ui.selected_pane, None);
    let mut focused = rig.state.current.clone();
    focused.focused_pane = Some(target.clone());
    rig.fold(focused, SnapshotSource::Produced);
    paint_at(&mut rig, 0);
    assert_eq!(rig.state.ui.selected_pane, Some(target));
    assert!(rig.state.ui.scroll_offset > 0);
    assert!(
        rig.state
            .ui
            .painted_rows
            .iter()
            .any(|(id, _)| id == "agent-19")
    );
}

#[test]
fn shared_placement_captures_the_painted_opening_set() {
    let (mut rig, _) = scroll_rig();
    let target = rig.state.current.worktree_groups[0].rows[1]
        .pane
        .as_ref()
        .unwrap()
        .pane_id
        .clone();
    rig.state.ui.selected_index = Some(0);
    rig.state.ui.selected_pane = rig.state.current.worktree_groups[0].rows[0]
        .pane
        .as_ref()
        .map(|pane| pane.pane_id.clone());
    rig.state.dirty = true;
    paint_at(&mut rig, 0);
    let row = rig.state.ui.interactions.line_for_row(1).unwrap() as u16;
    rig.state
        .apply_input([Wakeup::MouseClick { column: 1, row }], &mut rig.terminal)
        .unwrap();
    assert_eq!(rig.state.ui.drawn_selection.as_deref(), Some("agent-1"));
    assert!(
        rig.state.ui.held_expansions.contains("agent-0"),
        "the click paint already retains A under its freeze"
    );
    let placement = rig.state.focus_placement(&target).expect("visible target");
    let mut expected = rig.state.ui.held_expansions.clone();
    expected.insert("agent-1".into());
    assert_eq!(placement.held, expected);
}

fn assert_shared_placement_from_history(previous: usize) {
    let (mut source, _) = scroll_rig();
    let first = source.state.current.worktree_groups[0].rows[0]
        .pane
        .as_ref()
        .unwrap()
        .pane_id
        .clone();
    source.state.ui.selected_index = Some(0);
    source.state.ui.selected_pane = Some(first.clone());
    source.state.dirty = true;
    paint_at(&mut source, 0);
    let target = source.state.current.worktree_groups[0].rows[1].clone();
    let target_pane = target.pane.as_ref().unwrap().pane_id.clone();
    let row = source.state.ui.interactions.line_for_row(1).unwrap() as u16;
    source
        .state
        .apply_input(
            [Wakeup::MouseClick { column: 1, row }],
            &mut source.terminal,
        )
        .unwrap();
    assert_eq!(source.state.ui.drawn_selection.as_deref(), Some("agent-1"));
    let placement = source
        .state
        .focus_placement(&target_pane)
        .expect("source painted target");
    let source_y = source
        .state
        .ui
        .painted_rows
        .iter()
        .find(|(id, _)| id == &target.id)
        .unwrap()
        .1;

    let (mut destination, _, mut anchor) = hidden_destination();
    destination.state.current = source.state.current.clone();
    destination.state.ui.selected_index = Some(previous);
    destination.state.ui.selected_pane = Some(
        destination.state.current.worktree_groups[0].rows[previous]
            .pane
            .as_ref()
            .unwrap()
            .pane_id
            .clone(),
    );
    destination.state.dirty = true;
    destination
        .state
        .paint_now(&mut destination.terminal, Instant::now())
        .unwrap();
    destination.state.current.own_view = Some(SidebarOwnView {
        sibling_count: 1,
        working_pane_ids: vec![target_pane.clone()],
        own_view_is_daemon: false,
    });
    destination.state.current.focused_pane = Some(first);
    destination.state.current.viewed_panes.clear();
    destination.state.optimistic_watch_until = None;
    anchor.pane_id = target_pane.clone();
    anchor.placement = Some(placement);
    anchor.order = Some(source.state.ui.last_order.clone());
    let (push, ack) = focus_push(&mut destination, anchor);
    destination
        .state
        .on_focus_push(push, &mut destination.fetch, &mut destination.terminal)
        .unwrap();

    assert_eq!(destination.state.ui.selected_pane, Some(target_pane));
    assert!(destination.state.ui.focus_placement.is_none());
    assert_eq!(
        destination
            .state
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
        .ui
        .painted_rows
        .iter()
        .filter(|(_, y)| *y <= source_y)
    {
        assert_eq!(
            destination
                .state
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
    assert_eq!(rig.state.ui.selected_pane, Some(target.clone()));
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
            .ui
            .painted_rows
            .iter()
            .find(|(id, _)| id == &placement.row_id)
            .unwrap()
            .1
    };
    assert_eq!(
        y(&rig),
        rig.state.ui.scroll_zone_top as isize + placement.viewport_row
    );
    assert_eq!(rig.state.ui.last_focus_anchor_ms, anchor.issued_at_ms);
    let old_y = y(&rig);
    let mut switched = rig.state.current.clone();
    switched.focused_pane = Some(target.clone());
    switched.viewed_panes = vec![target];
    rig.fold(switched, SnapshotSource::Produced);
    assert!(rig.state.ui.focus_placement.is_none());
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
        rig.state.paint_hold.release();
        rig.state.should_exit = exit;
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
        rig.state.should_exit = exit;
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
    let mut switched = rig.state.current.clone();
    switched.focused_pane = Some(anchor.pane_id.clone());
    switched.viewed_panes = vec![anchor.pane_id.clone()];
    rig.fold(switched, SnapshotSource::Produced);
    assert_eq!(rig.state.ui.selected_pane, Some(anchor.pane_id));
    assert!(!rig.state.ui.own_input_reveal);
    assert!(rig.state.ui.focus_placement.is_none());
    assert_eq!(rig.state.ui.last_focus_anchor_ms, 0);
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
        rig.state.prev_width = Some(40);
        rig.state.self_close.seen_sibling = true;
        let (mut push, ack) = focus_push(&mut rig, anchor);
        let pane = if own_pane {
            rig.state.config.own_pane.clone().unwrap()
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
                rig.state.paint_hold.is_engaged(),
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
            rig.state.paint_hold.is_engaged(),
            holds,
            "resize_first={resize_first}, rows={rows}, cols={cols:?}, own_pane={own_pane}: only the exact fitted own-pane size bypasses the hold"
        );
        if holds && !painted_before_resize {
            assert_eq!(ack.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
        }
        if holds {
            rig.state.paint_hold.release();
        }
        rig.state.dirty = true;
        rig.state.next_frame = Instant::now();
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
        rig.state.clock = LoopClock::play(jiff::Timestamp::now());
        anchor.placement.as_mut().unwrap().viewport_row = 1_000;
        if cached {
            rig.state
                .fold_focus_intent(anchor.clone(), rig.state.clock.now());
            rig.state
                .paint_now(&mut rig.terminal, rig.state.clock.now())
                .unwrap();
            rig.state.clock.advance(Duration::from_millis(17));
        }
        let (push, ack) = focus_push(&mut rig, anchor.clone());
        if !cached {
            rig.state.clock.advance(Duration::from_millis(23));
        }
        rig.state
            .on_focus_push(push, &mut rig.fetch, &mut rig.terminal)
            .unwrap();

        assert_eq!(rig.state.ui.selected_pane, Some(anchor.pane_id));
        assert!(rig.state.ui.focus_placement.is_none());
        assert!(rig.state.ui.anchor_clamped);
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
        rig.state.config.own_pane.clone().unwrap(),
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
    rig.state.dirty = true;
    rig.state.next_frame = Instant::now();
    rig.paint(true);
    assert_push_report(&ack, &rig, PushVerdict::Painted, false);
}

#[test]
fn parked_focus_ack_waits_for_resize_but_other_panes_rows_do_not_gate_it() {
    for own_rows in [true, false] {
        let (mut rig, _, anchor) = hidden_destination();
        let (mut push, ack) = focus_push(&mut rig, anchor);
        let pane = if own_rows {
            rig.state.config.own_pane.clone().unwrap()
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
    rig.state.pending_focus_push.as_mut().unwrap().until = Instant::now();
    rig.paint(true);
    let second = assert_push_report(&second_ack, &rig, PushVerdict::Expired, true);
    assert_eq!(second.blocked_by, Some(PushBlock::PaintHold));
    assert_eq!(paints(&output), 0);

    for nonowner_push in [false, true] {
        let (mut rig, output, anchor) = hidden_destination();
        rig.state
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
            rig.state.paint_hold.release();
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
        .paint_hold
        .engage(Instant::now(), crate::utils::time::unix_now_ms());
    let row_id = anchor.placement.as_ref().unwrap().row_id.clone();
    let (push, ack) = focus_push(&mut rig, anchor);
    rig.state
        .on_focus_push(push, &mut rig.fetch, &mut rig.terminal)
        .unwrap();
    rig.state.ui.drawn_selection = None;
    rig.state.observe_paint(Instant::now());
    rig.state.ui.drawn_selection = Some(row_id);
    assert!(
        rig.state.ui.focus_placement.is_some(),
        "placement has not been consumed by a paint"
    );
    rig.state.observe_paint(Instant::now());
    assert_eq!(ack.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
    rig.state.should_exit = true;
    rig.paint(true);
    let report = assert_push_report(&ack, &rig, PushVerdict::Exiting, true);
    assert_eq!(report.unmatched_paints, 2);
    assert_eq!(report.misplaced_paints, 1);
}

#[test]
fn focus_click_is_watched_at_the_painted_row_until_the_next_paint() {
    let (mut rig, _) = scroll_rig();
    let (row_id, from_y) = rig.state.ui.painted_rows[1].clone();
    let target = rig
        .state
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
    rig.state.observe_tx = tx;
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
    rig.state.ui.offset_authoritative = true;
    rig.state.ui.scroll_offset += 1;
    rig.state.dirty = true;
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
            let lead = &mut Arc::make_mut(
                &mut Arc::make_mut(&mut rig.state.current.worktree_groups)[0].rows,
            )[0];
            lead.unread = true;
            lead.as_agent_mut().unwrap().status = crate::agents::AgentStatus::Waiting;
        }
        let target = rig.state.current.worktree_groups[0].rows[4].clone();
        rig.state.ui.selected_index = Some(4);
        rig.state.ui.selected_pane = Some(target.pane.as_ref().unwrap().pane_id.clone());
        rig.state.ui.scroll_offset = 30;
        let stamp = crate::utils::time::unix_now_ms();
        crate::mux::focus_anchor::store(
            &rig.runtime,
            &FocusAnchor {
                nonce: crate::ids::FocusNonce::new(),
                session_name: "rimz-test".to_owned(),
                pane_id: rig.state.ui.selected_pane.clone().unwrap(),
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
        rig.state.apply_focus_anchor();
        assert!(rig.state.ui.focus_placement.is_some());
        rig.state.dirty = true;
        paint_at(&mut rig, 0);
        let theme = rig.state.ui.theme(&rig.state.current.theme);
        let frame = render::compose_lines(&rig.state.current, None, &rig.state.ui, &theme, 80, 24);
        let (_, first_line) = rig
            .state
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
        let offscreen = rig.state.current.worktree_groups[0].rows.last().unwrap();
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
        let rows = &rig.state.current.worktree_groups[0].rows;
        let placed_id = rows[placed].id.clone();
        let target = rows[selected].clone();
        rig.state.ui.selected_index = Some(selected);
        rig.state.ui.own_input_reveal = true;
        rig.state.ui.selected_pane = Some(target.pane.as_ref().unwrap().pane_id.clone());
        rig.state.ui.scroll_offset = 0;
        rig.state.ui.focus_placement = Some(FocusPlacement {
            row_id: placed_id,
            viewport_row,
            held: std::collections::BTreeSet::new(),
        });
        rig.state.dirty = true;
        paint_at(&mut rig, 0);
        assert!(
            rig.state
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
    let started = rig.state.ui.frame_ms;
    paint_at(&mut rig, started + 40);
    assert!(rig.state.ui.scroll_ease.in_flight());
    let (tx, rx) = std::sync::mpsc::sync_channel(8);
    rig.state.observe_tx = tx;
    let (_, y) = rig.state.ui.painted_rows.last().unwrap().clone();
    rig.state
        .apply_input(
            [Wakeup::MouseClick {
                column: 2,
                row: y as u16,
            }],
            &mut rig.terminal,
        )
        .unwrap();
    assert!(rig.state.clicked_row.is_some());
    for elapsed in [60, 80, 100, 140] {
        paint_at(&mut rig, started + elapsed);
    }
    assert!(!rig.state.ui.scroll_ease.in_flight());
    assert!(rx.try_recv().is_err(), "wheel glide is not a displacement");
    assert!(
        rig.state.clicked_row.is_some(),
        "the watch survives the glide"
    );
}

#[test]
fn wheel_run_eases_then_releases_the_animation_grid() {
    let (mut rig, output) = scroll_rig();
    let mut inbox = queued((0..3).map(|_| Wakeup::Scroll { down: true }));
    dispatch(&mut rig, &mut inbox);
    assert_eq!(rig.state.ui.scroll_offset, 9);
    assert_eq!(visible(&rig), 0);
    assert_eq!(paints(&output), 1);
    assert!(rig.frame_active());
    let started = rig.state.ui.frame_ms;
    paint_at(&mut rig, started + 100);
    assert_eq!(visible(&rig), 9);
    assert!(!rig.state.ui.scroll_ease.in_flight());
    assert_eq!(
        render::animation_interval(
            &rig.state.current,
            &rig.state.ui,
            rig.state.ui.animation_phase,
            false
        ),
        Some(animation_frame(&rig.state.current))
    );
    paint_at(&mut rig, started + 2100);
    assert!(!rig.frame_active());
}

#[test]
fn click_after_wheel_ease_hits_the_landed_frame() {
    let (mut rig, _) = scroll_rig();
    let now = Instant::now();
    let mut inbox = queued((0..3).map(|_| Wakeup::Scroll { down: true }));
    dispatch(&mut rig, &mut inbox);
    let started = rig.state.ui.frame_ms;
    for elapsed in [16, 48, 100] {
        paint_at(&mut rig, started + elapsed);
    }
    assert!(!rig.state.ui.scroll_ease.in_flight());
    let index = rig.state.ui.interactions.visible_row_span().unwrap().0 + 1;
    let row = rig.state.ui.interactions.line_for_row(index).unwrap() as u16;
    let id = &rig.state.ui.painted_roster[index];
    let expected = rig
        .state
        .current
        .rows()
        .find(|row| &row.id == id)
        .unwrap()
        .pane
        .as_ref()
        .unwrap()
        .pane_id
        .clone();
    let outcome = rig
        .state
        .apply_input_at(
            [Wakeup::MouseClick { column: 1, row }],
            &mut rig.terminal,
            now + Duration::from_millis(200),
        )
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
        starts.push(rig.state.ui.frame_ms);
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
        assert!(rig.state.ui.scroll_ease.in_flight());
        rig.input(KeyAction::Down);
        assert!(!rig.state.ui.scroll_ease.in_flight());
        assert_eq!(visible(&rig), rig.state.ui.scroll_offset);
        let (first, last) = rig.state.ui.interactions.visible_row_span().unwrap();
        assert!((first..=last).contains(&rig.state.ui.selected_index.unwrap()));
    }
}

#[test]
fn wheel_input_pulls_the_next_grid_frame_in() {
    let (mut rig, _) = scroll_rig();
    rig.state.next_frame = Instant::now() + Duration::from_secs(1);
    assert!(!rig.frame_active(), "the run starts from an idle loop");
    let mut inbox = queued((0..3).map(|_| Wakeup::Scroll { down: true }));
    rig.step(&mut inbox);
    assert!(
        rig.state
            .next_frame
            .saturating_duration_since(Instant::now())
            <= render::SCROLL_EASE_FRAME
    );
}

#[test]
fn queued_wheel_ticks_paint_once_at_the_final_offset() {
    for count in [1, 4, 30] {
        let (mut rig, output) = scroll_rig();
        let selected = rig.state.ui.selected_pane.clone();
        let max = rig.state.ui.drawn_scroll_max.unwrap();
        let mut inbox = queued((0..count).map(|_| Wakeup::Scroll { down: true }));
        dispatch(&mut rig, &mut inbox);
        assert_eq!(rig.state.ui.scroll_offset, (count * 3).min(max));
        assert_eq!(paints(&output), 1);
        assert_eq!(rig.state.ui.selected_pane, selected);
        assert!(
            !rig.state.ui.offset_authoritative,
            "paint consumed the wheel intent"
        );
        assert!(rig.state.ui.freeze.is_some());
        assert!(!rig.state.dirty);
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
                rig.state.ui.scroll_offset = rig.state.ui.drawn_scroll_max.unwrap();
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
            batched.state.ui.scroll_offset,
            single.state.ui.scroll_offset
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
    assert_eq!(rig.state.ui.scroll_offset, 6);
    assert_eq!(paints(&output), 1);
    // ScreenTop resolves the row from the frame the preceding run painted.
    let expected = rig.state.ui.interactions.visible_row_span().unwrap().0;
    dispatch(&mut rig, &mut inbox);
    assert_eq!(rig.state.ui.selected_index, Some(expected));
    assert_eq!(paints(&output), 2);
    let offset = rig.state.ui.scroll_offset;
    dispatch(&mut rig, &mut inbox);
    assert_eq!(rig.state.ui.scroll_offset, offset + 3);
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
    assert!(!rig.state.ui.help_visible);
    assert_eq!(rig.state.ui.scroll_offset, 6);
    assert_eq!(paints(&output), 1);
}

#[test]
fn hidden_run_stays_dirty_until_watched() {
    let (mut rig, output) = scroll_rig();
    let cards = rig.state.current.worktree_groups.clone();
    rig.hide();
    rig.state.current.worktree_groups = cards;
    let mut inbox = queued((0..3).map(|_| Wakeup::Scroll { down: true }));
    dispatch(&mut rig, &mut inbox);
    assert_eq!(rig.state.ui.scroll_offset, 9);
    assert!(rig.state.dirty);
    assert!(output.0.lock().unwrap().is_empty());
    assert_eq!(paints(&output), 0);
    rig.watch();
    rig.state.next_frame = Instant::now();
    rig.paint(true);
    assert_eq!(rig.state.ui.scroll_offset, 9);
    assert_eq!(paints(&output), 1);
    assert!(!rig.state.dirty);
}
